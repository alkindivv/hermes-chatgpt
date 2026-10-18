use crate::{Error, Result};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};
use tokio::sync::{broadcast, mpsc, oneshot, watch};

#[derive(Clone, Debug)]
pub struct Cancellation(watch::Sender<bool>);
impl Default for Cancellation {
    fn default() -> Self {
        Self(watch::channel(false).0)
    }
}
impl Cancellation {
    pub fn cancel(&self) {
        self.0.send_replace(true);
    }
    pub fn is_cancelled(&self) -> bool {
        *self.0.borrow()
    }
    pub async fn cancelled(&self) {
        let mut rx = self.0.subscribe();
        if *rx.borrow_and_update() {
            return;
        }
        let _ = rx.wait_for(|v| *v).await;
    }
}
type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value>>>>>;
#[derive(Clone)]
pub struct CdpClient {
    tx: mpsc::UnboundedSender<Value>,
    pending: Pending,
    next: Arc<AtomicU64>,
    events: broadcast::Sender<Value>,
}
struct PendingGuard {
    id: u64,
    pending: Pending,
}
impl Drop for PendingGuard {
    fn drop(&mut self) {
        self.pending.lock().unwrap().remove(&self.id);
    }
}
impl CdpClient {
    pub async fn connect(ws_url: &str) -> Result<Self> {
        let url = url::Url::parse(ws_url).map_err(|e| Error::Endpoint(e.to_string()))?;
        let host = url.host_str().unwrap_or("");
        let target = url.path().strip_prefix("/devtools/page/").unwrap_or("");
        if url.scheme() != "ws"
            || !matches!(host, "127.0.0.1" | "[::1]" | "::1")
            || url.port().is_none()
            || target.is_empty()
            || target.contains('/')
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(Error::Endpoint(
                "explicit owned loopback page WebSocket required".into(),
            ));
        }
        let (ws, _) = tokio::time::timeout(
            Duration::from_secs(10),
            tokio_tungstenite::connect_async(ws_url),
        )
        .await
        .map_err(|_| Error::Timeout)?
        .map_err(|e| Error::Transport(e.to_string()))?;
        let (mut sink, mut stream) = ws.split();
        let (tx, mut rx) = mpsc::unbounded_channel::<Value>();
        let pending: Pending = Default::default();
        let shared = pending.clone();
        let (events, _) = broadcast::channel(1024);
        let event_tx = events.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    outgoing=rx.recv()=>match outgoing { Some(v)=>if sink.send(v.to_string().into()).await.is_err(){break},None=>break },
                    incoming=stream.next()=>match incoming {
                        Some(Ok(message))=> { if let Ok(v)=serde_json::from_slice::<Value>(&message.into_data()) { if let Some(id)=v["id"].as_u64() { if let Some(reply)=shared.lock().unwrap().remove(&id) { let result=if v.get("error").is_some(){Err(Error::Protocol(v["error"].to_string()))}else{Ok(v["result"].clone())}; let _=reply.send(result); } } else { let _=event_tx.send(v); } } },
                        _=>break,
                    }
                }
            }
            for (_, reply) in shared.lock().unwrap().drain() {
                let _ = reply.send(Err(Error::Transport("CDP disconnected".into())));
            }
        });
        Ok(Self {
            tx,
            pending,
            next: Arc::new(AtomicU64::new(1)),
            events,
        })
    }
    pub fn events(&self) -> broadcast::Receiver<Value> {
        self.events.subscribe()
    }
    pub async fn request(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
        cancel: &Cancellation,
    ) -> Result<Value> {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        let _guard = PendingGuard {
            id,
            pending: self.pending.clone(),
        };
        self.tx
            .send(json!({"id":id,"method":method,"params":params}))
            .map_err(|_| Error::Transport("CDP disconnected".into()))?;
        tokio::select! { biased;
            _=cancel.cancelled()=>Err(Error::Cancelled),
            result=tokio::time::timeout(timeout,rx)=>match result { Ok(Ok(v))=>v,Ok(Err(_))=>Err(Error::Transport("CDP reply dropped".into())),Err(_)=>Err(Error::Timeout) }
        }
    }
    pub async fn evaluate(&self, expression: &str, cancel: &Cancellation) -> Result<Value> {
        let request = self
            .request(
                "Runtime.evaluate",
                json!({"expression":expression,"returnByValue":true,"awaitPromise":true}),
                Duration::from_secs(10),
                cancel,
            )
            .await;
        let result = match request {
            Ok(value) => value,
            Err(error) => {
                if matches!(error, Error::Cancelled | Error::Timeout) {
                    // Invalidate continuations of our versioned async page helper. Do not click Stop.
                    let id = self.next.fetch_add(1, Ordering::Relaxed);
                    let _=self.tx.send(json!({"id":id,"method":"Runtime.evaluate","params":{"expression":"globalThis.__CW_BROWSER_ABORT_GENERATION__=(globalThis.__CW_BROWSER_ABORT_GENERATION__||0)+1"}}));
                }
                return Err(error);
            }
        };
        if result.get("exceptionDetails").is_some() {
            return Err(Error::Dom(result["exceptionDetails"].to_string()));
        }
        Ok(result["result"]["value"].clone())
    }
}
