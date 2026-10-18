use super::*;
use std::net::{SocketAddr, TcpStream};
use tungstenite::{Message, WebSocket, protocol::WebSocketConfig};
pub(super) struct Cdp {
    socket: WebSocket<TcpStream>,
    next: u64,
    deadline: Instant,
}
impl Cdp {
    pub fn open(port: u16, path: &str, deadline: Instant) -> Result<Self> {
        let id = path
            .strip_prefix("/devtools/browser/")
            .ok_or_else(|| anyhow::anyhow!("invalid owned CDP browser path"))?;
        uuid::Uuid::parse_str(id).map_err(|_| anyhow::anyhow!("invalid CDP browser identity"))?;
        let timeout = deadline
            .saturating_duration_since(Instant::now())
            .min(Duration::from_secs(5));
        ensure!(!timeout.is_zero(), "sign-in deadline expired");
        let address = SocketAddr::from(([127, 0, 0, 1], port));
        let stream = TcpStream::connect_timeout(&address, timeout)
            .map_err(|_| anyhow::anyhow!("owned CDP endpoint unavailable"))?;
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(timeout))?;
        let config = WebSocketConfig::default()
            .max_message_size(Some(16 * 1024 * 1024))
            .max_frame_size(Some(16 * 1024 * 1024));
        let (socket, _) = tungstenite::client::client_with_config(
            format!("ws://127.0.0.1:{port}{path}"),
            stream,
            Some(config),
        )
        .map_err(|_| anyhow::anyhow!("owned CDP handshake failed"))?;
        Ok(Self {
            socket,
            next: 0,
            deadline,
        })
    }
    fn send(&mut self, method: &str, params: Value, session: Option<&str>) -> Result<u64> {
        ensure!(Instant::now() < self.deadline, "sign-in deadline expired");
        self.next += 1;
        let mut request = json!({"id":self.next,"method":method,"params":params});
        if let Some(s) = session {
            request["sessionId"] = s.into();
        }
        self.socket
            .send(Message::Text(request.to_string().into()))
            .map_err(|_| anyhow::anyhow!("owned CDP send failed"))?;
        Ok(self.next)
    }
    pub fn call(&mut self, method: &str, params: Value, session: Option<&str>) -> Result<Value> {
        let id = self.send(method, params, session)?;
        for _ in 0..10000 {
            let timeout = self
                .deadline
                .saturating_duration_since(Instant::now())
                .min(Duration::from_secs(5));
            ensure!(!timeout.is_zero(), "sign-in deadline expired");
            self.socket.get_mut().set_read_timeout(Some(timeout))?;
            let message = self
                .socket
                .read()
                .map_err(|_| anyhow::anyhow!("owned CDP response unavailable"))?;
            if let Message::Ping(data) = message {
                self.socket
                    .send(Message::Pong(data))
                    .map_err(|_| anyhow::anyhow!("CDP ping failed"))?;
                continue;
            }
            let Message::Text(text) = message else {
                continue;
            };
            let response: Value =
                serde_json::from_str(&text).map_err(|_| anyhow::anyhow!("invalid CDP response"))?;
            if response["method"] == "Fetch.requestPaused" {
                let request = response["params"]["requestId"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("invalid offline interception event"))?;
                self.send("Fetch.fulfillRequest",json!({"requestId":request,"responseCode":200,"responseHeaders":[{"name":"Content-Type","value":"text/html"}],"body":""}),response["sessionId"].as_str())?;
                continue;
            }
            if response["id"] == id {
                ensure!(
                    response.get("error").is_none(),
                    "owned CDP command rejected"
                );
                return Ok(response["result"].clone());
            }
        }
        bail!("CDP event budget exceeded")
    }
    pub fn capture(&mut self) -> Result<Value> {
        let targets = self.call("Target.getTargets", json!({}), None)?;
        let targets = targets["targetInfos"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("invalid CDP targets"))?;
        let pages: Vec<_> = targets.iter().filter(|t| t["type"] == "page").collect();
        ensure!(!pages.is_empty(), "offline capture has no page");
        for page in &pages {
            ensure!(
                page["url"] == "about:blank",
                "offline capture restored a nonblank page"
            );
        }
        let target = pages[0]["targetId"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("invalid offline target"))?;
        let attached = self.call(
            "Target.attachToTarget",
            json!({"targetId":target,"flatten":true}),
            None,
        )?;
        let session = attached["sessionId"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("offline attach failed"))?;
        for (method, params) in [
            ("Network.enable", json!({})),
            (
                "Network.emulateNetworkConditions",
                json!({"offline":true,"latency":0,"downloadThroughput":0,"uploadThroughput":0}),
            ),
            ("Network.setBypassServiceWorker", json!({"bypass":true})),
            ("ServiceWorker.stopAllWorkers", json!({})),
            ("ServiceWorker.disable", json!({})),
            (
                "Fetch.enable",
                json!({"patterns":[{"urlPattern":"*","requestStage":"Request"}]}),
            ),
            ("Page.enable", json!({})),
        ] {
            self.call(method, params, Some(session))?;
        }
        self.call(
            "Page.navigate",
            json!({"url":TEMPORARY_CHAT_URL}),
            Some(session),
        )?;
        let frame = self.call("Page.getFrameTree", json!({}), Some(session))?;
        let url = reqwest::Url::parse(frame["frameTree"]["frame"]["url"].as_str().unwrap_or(""))
            .map_err(|_| anyhow::anyhow!("invalid offline origin"))?;
        ensure!(
            url.origin().ascii_serialization() == "https://chatgpt.com"
                && url.username().is_empty()
                && url.password().is_none(),
            "offline capture reached unexpected origin"
        );
        let local = self.call(
            "DOMStorage.getDOMStorageItems",
            json!({"storageId":{"securityOrigin":"https://chatgpt.com","isLocalStorage":true}}),
            Some(session),
        )?;
        let rows = local["entries"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("invalid offline local storage"))?;
        ensure!(rows.len() <= 4096, "local-storage collection limit");
        let mut entries = Vec::new();
        for row in rows {
            ensure!(
                row.as_array().is_some_and(|v| v.len() == 2)
                    && row[0].is_string()
                    && row[1].is_string(),
                "invalid offline local-storage entry"
            );
            entries.push(json!({"name":row[0],"value":row[1]}));
        }
        let result = self.call("Storage.getCookies", json!({}), None)?;
        let mut cookies = result["cookies"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("invalid offline cookie response"))?
            .clone();
        // Chromium omits unspecified SameSite; Playwright's storage-state policy defaults to Lax.
        for cookie in &mut cookies {
            if cookie.get("sameSite").is_none() {
                cookie["sameSite"] = "Lax".into();
            }
        }
        Ok(
            json!({"cookies":cookies,"origins":[{"origin":"https://chatgpt.com","localStorage":entries}]}),
        )
    }
    pub fn close_browser(&mut self) {
        let _ = self.send("Browser.close", json!({}), None);
    }
}
