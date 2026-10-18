use crate::*;
#[path = "mcp_ledger.rs"]
mod mcp_ledger;
#[path = "mcp_policy.rs"]
mod mcp_policy;
pub use mcp_ledger::McpObservation;
#[path = "compaction.rs"]
mod compaction;
pub use compaction::build_compact_v1_output;
#[path = "session.rs"]
mod session;
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, State},
    http::StatusCode,
    response::{
        IntoResponse, Response, Sse,
        sse::{Event, KeepAlive},
    },
    routing::{get, post},
};
use serde::Serialize;
use serde_json::{Value, json};
use session::{History, PendingCall, RoundCommand};
#[path = "catalog.rs"]
mod catalog;
pub use catalog::{CatalogStatus, ModelCatalogOptions, RoutePreferences};
use std::{
    collections::HashMap,
    convert::Infallible,
    net::SocketAddr,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::{
    net::TcpListener,
    sync::{mpsc, oneshot, watch},
    task::JoinHandle,
};
use tokio_stream::{StreamExt, wrappers::ReceiverStream};
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct BridgeConfig {
    pub mode: ExecutionMode,
    pub turn_ttl: Duration,
    pub channel_capacity: usize,
}
impl Default for BridgeConfig {
    fn default() -> Self {
        Self {
            mode: ExecutionMode::Automatic,
            turn_ttl: Duration::from_secs(1200),
            channel_capacity: 64,
        }
    }
}
#[derive(Debug, Clone, Default, Serialize)]
pub struct BridgeCounters {
    pub active_turns: usize,
    pub active_native_requests: usize,
    pub active_compactions: usize,
    pub pending_tool_calls: usize,
    pub completed_turns: usize,
    pub failed_turns: usize,
    pub cancelled_turns: usize,
}
struct Turn {
    owner: TurnOwner,
    token: String,
    deadline: Instant,
    cancel: watch::Sender<Option<String>>,
    confirmed: watch::Sender<bool>,
    tools: Vec<NativeTool>,
    task: CompiledTask,
    event_rx: Arc<tokio::sync::Mutex<mpsc::Receiver<BrowserEvent>>>,
    commands: mpsc::Sender<RoundCommand>,
    command_rx: Arc<tokio::sync::Mutex<mpsc::Receiver<RoundCommand>>>,
    pending: HashMap<String, PendingCall>,
    observer_active: bool,
    next_call_order: u64,
    connector_started: bool,
    final_answer: Option<String>,
    inflight_tools: usize,
    compaction_request: Option<String>,
    work_revoked: bool,
    compaction_interceptions: usize,
}
struct Inner {
    mcp_ledger: Mutex<mcp_ledger::Ledger>,
    config: BridgeConfig,
    mode: AtomicUsize,
    history: Mutex<HashMap<String, History>>,
    catalog: Mutex<catalog::CatalogState>,
    compactions: Mutex<compaction::Registry>,
    turns: Mutex<HashMap<String, Turn>>,
    browser: mpsc::Sender<BrowserRequest>,
    draining: AtomicBool,
    session_changing: AtomicBool,
    pending_tools: AtomicUsize,
    native_requests: AtomicUsize,
    completed: AtomicUsize,
    failed: AtomicUsize,
    cancelled: AtomicUsize,
    shutdown: watch::Sender<bool>,
}
#[derive(Clone)]
pub struct BridgeHandle {
    pub local_addr: SocketAddr,
    inner: Arc<Inner>,
    server: Arc<tokio::sync::Mutex<Option<JoinHandle<std::io::Result<()>>>>>,
}
pub struct BridgeServer;
/// Keeps new requests out while the caller changes its owned browser session.
pub struct SessionChangeGuard { inner: Arc<Inner> }
impl Drop for SessionChangeGuard {
    fn drop(&mut self) { self.inner.session_changing.store(false, Ordering::SeqCst); }
}
impl BridgeServer {
    pub async fn bind(
        addr: SocketAddr,
        config: BridgeConfig,
    ) -> std::io::Result<(BridgeHandle, mpsc::Receiver<BrowserRequest>)> {
        if !addr.ip().is_loopback() || config.turn_ttl.is_zero() || config.channel_capacity == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "loopback address, positive TTL and channel capacity required",
            ));
        }
        let listener = TcpListener::bind(addr).await?;
        let local_addr = listener.local_addr()?;
        let (browser, receiver) = mpsc::channel(config.channel_capacity);
        let (shutdown, mut stop) = watch::channel(false);
        let inner = Arc::new(Inner {
            mcp_ledger: Mutex::new(mcp_ledger::Ledger::default()),
            mode: AtomicUsize::new(if config.mode == ExecutionMode::Manual {
                1
            } else {
                0
            }),
            config,
            history: Mutex::new(HashMap::new()),
            catalog: Mutex::new(catalog::CatalogState::default()),
            compactions: Mutex::new(compaction::Registry::default()),
            turns: Mutex::new(HashMap::new()),
            browser,
            draining: AtomicBool::new(false),
            session_changing: AtomicBool::new(false),
            pending_tools: AtomicUsize::new(0),
            native_requests: AtomicUsize::new(0),
            completed: AtomicUsize::new(0),
            failed: AtomicUsize::new(0),
            cancelled: AtomicUsize::new(0),
            shutdown,
        });
        let app = Router::new()
            .route("/health", get(health))
            .route("/healthz", get(health))
            .route(
                "/v1/responses",
                post(responses).get(|| async {
                    (
                        StatusCode::UPGRADE_REQUIRED,
                        "Responses WebSocket transport is not enabled on this local route",
                    )
                }),
            )
            .route("/responses", post(responses))
            .route("/v1/models", get(catalog::models))
            .route("/models", get(catalog::models))
            .route("/v1/responses/compact", post(catalog::compact))
            .route("/responses/compact", post(catalog::compact))
            .route("/mcp", post(mcp))
            .layer(DefaultBodyLimit::max(64 * 1024 * 1024))
            .with_state(inner.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    let _ = stop.wait_for(|v| *v).await;
                })
                .await
        });
        Ok((
            BridgeHandle {
                local_addr,
                inner,
                server: Arc::new(tokio::sync::Mutex::new(Some(task))),
            },
            receiver,
        ))
    }
    pub async fn start(
        addr: SocketAddr,
        config: BridgeConfig,
    ) -> std::io::Result<(BridgeHandle, mpsc::Receiver<BrowserRequest>)> {
        Self::bind(addr, config).await
    }
}
impl BridgeHandle {
    pub fn begin_session_change(&self) -> Result<SessionChangeGuard, String> {
        let turns = self.inner.turns.lock().unwrap();
        if self.inner.admission_paused() || !turns.is_empty()
            || self.inner.pending_tools.load(Ordering::SeqCst) != 0
            || self.inner.native_requests.load(Ordering::SeqCst) != 0
            || self.inner.compactions.lock().unwrap().active_count() != 0
        {
            return Err("finish active owners and requests before changing the browser session".into());
        }
        self.inner.session_changing.store(true, Ordering::SeqCst);
        Ok(SessionChangeGuard { inner: self.inner.clone() })
    }
    pub fn set_mode(&self, mode: ExecutionMode) -> Result<(), String> {
        let turns = self.inner.turns.lock().unwrap();
        if !turns.is_empty()
            || self.inner.pending_tools.load(Ordering::SeqCst) != 0
            || self.inner.native_requests.load(Ordering::SeqCst) != 0
            || self.inner.compactions.lock().unwrap().active_count() != 0
        {
            return Err("execution mode cannot change while turns or tools are active".into());
        }
        let changed = self.inner.execution_mode() != mode;
        self.inner.mode.store(
            if mode == ExecutionMode::Manual { 1 } else { 0 },
            Ordering::SeqCst,
        );
        if changed {
            self.inner.catalog.lock().unwrap().invalidate();
        }
        Ok(())
    }
    pub fn execution_mode(&self) -> ExecutionMode {
        self.inner.execution_mode()
    }
    pub fn base_url(&self) -> String {
        format!("http://{}", self.local_addr)
    }
    pub fn is_draining(&self) -> bool {
        self.inner.draining.load(Ordering::SeqCst)
    }
    pub fn counters(&self) -> BridgeCounters {
        BridgeCounters {
            active_turns: self.inner.turns.lock().unwrap().len(),
            active_native_requests: self.inner.native_requests.load(Ordering::SeqCst),
            active_compactions: self.inner.compactions.lock().unwrap().active_count(),
            pending_tool_calls: self.inner.pending_tools.load(Ordering::SeqCst),
            completed_turns: self.inner.completed.load(Ordering::SeqCst),
            failed_turns: self.inner.failed.load(Ordering::SeqCst),
            cancelled_turns: self.inner.cancelled.load(Ordering::SeqCst),
        }
    }
    pub fn authorize(&self, token: &str, owner: &TurnOwner) -> Result<(), String> {
        let turns = self.inner.turns.lock().unwrap();
        let turn = turns
            .values()
            .find(|t| t.token == token)
            .ok_or("turn token is invalid or expired")?;
        validate_turn(turn, Some(owner))
    }
    pub fn confirm_manual(&self, request_id: &str, owner: &TurnOwner) -> Result<(), String> {
        let turns = self.inner.turns.lock().unwrap();
        let turn = turns.get(request_id).ok_or("turn is not active")?;
        if &turn.owner != owner || Instant::now() >= turn.deadline || turn.cancel.borrow().is_some()
        {
            return Err("turn owner mismatch or expired turn".into());
        }
        turn.confirmed.send_replace(true);
        Ok(())
    }
    pub fn cancel(&self, request_id: &str) -> bool {
        if compaction::cancel(&self.inner, request_id) {
            return true;
        }
        if let Some(turn) = self.inner.turns.lock().unwrap().get(request_id) {
            turn.cancel.send_replace(Some("client_cancelled".into()));
            true
        } else {
            false
        }
    }
    /// Stop admission while allowing existing turns to settle.
    pub fn drain(&self) -> BridgeCounters {
        self.inner.draining.store(true, Ordering::SeqCst);
        self.counters()
    }
    /// Revoke all capabilities, notify consumers, and join the owned listener.
    pub async fn close(&self) -> std::io::Result<()> {
        self.drain();
        for (_, turn) in self.inner.turns.lock().unwrap().drain() {
            if !turn.observer_active {
                self.inner.cancelled.fetch_add(1, Ordering::SeqCst);
            }
            turn.cancel.send_replace(Some("bridge_closed".into()));
        }
        self.inner.shutdown.send_replace(true);
        compaction::wait_closed(&self.inner).await;
        if let Some(task) = self.server.lock().await.take() {
            task.await.map_err(std::io::Error::other)??;
        }
        Ok(())
    }
}
impl Inner {
    fn admission_paused(&self) -> bool {
        self.draining.load(Ordering::SeqCst) || self.session_changing.load(Ordering::SeqCst)
    }
    fn execution_mode(&self) -> ExecutionMode {
        if self.mode.load(Ordering::SeqCst) == 1 {
            ExecutionMode::Manual
        } else {
            ExecutionMode::Automatic
        }
    }
}
fn validate_turn(turn: &Turn, owner: Option<&TurnOwner>) -> Result<(), String> {
    if turn.work_revoked || Instant::now() >= turn.deadline || turn.cancel.borrow().is_some() {
        return Err("turn token is invalid or expired".into());
    }
    if owner.is_some_and(|owner| owner != &turn.owner) {
        return Err("turn owner mismatch".into());
    }
    if turn.final_answer.is_some() {
        return Err("Zero Risk turn is already terminal".into());
    }
    if !turn.connector_started {
        return Err("Zero Risk turn requires codex_turn_start".into());
    }
    if !*turn.confirmed.borrow() {
        return Err("manual turn has not been confirmed sent".into());
    }
    Ok(())
}
fn self_contained_compactions(inner: &Inner) -> usize {
    inner.compactions.lock().unwrap().fresh_active_count()
}
async fn health(State(inner): State<Arc<Inner>>) -> Json<Value> {
    Json(
        json!({"ok":true,"draining":inner.draining.load(Ordering::SeqCst),"active_turns":inner.turns.lock().unwrap().len()}),
    )
}
fn error(status: StatusCode, code: &str, message: impl Into<String>) -> Response {
    (
        status,
        Json(
            json!({"error":{"type":"invalid_request_error","code":code,"message":message.into()}}),
        ),
    )
        .into_response()
}
async fn responses(
    State(inner): State<Arc<Inner>>,
    headers: axum::http::HeaderMap,
    uri: axum::extract::OriginalUri,
    Json(mut raw): Json<Value>,
) -> Response {
    if let Some(response) = catalog::maybe_native(&inner, &headers, &uri.0, &raw, "responses").await
    {
        return response;
    }
    if let Err(e) = catalog::validate_route(&inner, &mut raw) {
        return error(StatusCode::BAD_REQUEST, "invalid_model", e);
    }
    if let Some(response) = compaction::resume_checkpoint(&inner, &raw).await {
        return response;
    }
    let streaming = raw["stream"] == true;
    match session::resume(&inner, &mut raw).await {
        Ok(Some(receiver)) => return response_body(receiver, streaming).await,
        Ok(None) => {}
        Err(e) => return error(StatusCode::BAD_REQUEST, "invalid_continuation", e),
    }
    let mut task = match compile_request(raw) {
        Ok(task) => task,
        Err(e) => return error(StatusCode::BAD_REQUEST, "unsupported_request", e),
    };
    let request_id = format!("resp_{}", Uuid::new_v4().simple());
    let metadata = turn_metadata(&task.raw);
    let owner = TurnOwner {
        thread_id: metadata["thread_id"].as_str().unwrap_or(&request_id).into(),
        turn_id: metadata["turn_id"].as_str().unwrap_or(&request_id).into(),
    };
    // Two independent random UUIDs supply 244 random bits. Tokens never derive from task IDs.
    let token = format!(
        "{}_{}{}",
        if inner.execution_mode() == ExecutionMode::Manual {
            "request"
        } else {
            "turn"
        },
        Uuid::new_v4().simple(),
        Uuid::new_v4().simple()
    );
    task.prompt = crate::prompt::compile_prompt(&task, Some(&token), inner.execution_mode());
    let (cancel, cancellation) = watch::channel(None);
    let (confirmed, confirmation) =
        watch::channel(inner.execution_mode() == ExecutionMode::Automatic);
    let deadline = Instant::now() + inner.config.turn_ttl;
    let (events, event_rx) = mpsc::channel(inner.config.channel_capacity);
    let event_rx = Arc::new(tokio::sync::Mutex::new(event_rx));
    let (commands, command_rx) = mpsc::channel(inner.config.channel_capacity);
    let command_rx = Arc::new(tokio::sync::Mutex::new(command_rx));
    {
        let mut turns = inner.turns.lock().unwrap();
        if inner.admission_paused() {
            return error(
                StatusCode::SERVICE_UNAVAILABLE,
                "draining",
                "bridge admission is paused",
            );
        }
        if turns.len() + self_contained_compactions(&inner) >= 5 {
            return error(
                StatusCode::TOO_MANY_REQUESTS,
                "browser_capacity",
                "at most five active browser owners",
            );
        }
        if turns.values().any(|t| t.owner == owner) {
            return error(
                StatusCode::CONFLICT,
                "owner_busy",
                "owner already has an active turn",
            );
        }
        turns.insert(
            request_id.clone(),
            Turn {
                owner: owner.clone(),
                token: token.clone(),
                deadline,
                cancel,
                confirmed,
                tools: task.tools.clone(),
                task: task.clone(),
                event_rx: event_rx.clone(),
                commands,
                command_rx: command_rx.clone(),
                pending: HashMap::new(),
                observer_active: true,
                next_call_order: 0,
                connector_started: inner.execution_mode() == ExecutionMode::Automatic,
                final_answer: None,
                inflight_tools: 0,
                compaction_request: None,
                work_revoked: false,
                compaction_interceptions: 0,
            },
        );
    }
    let streaming = task.raw["stream"] == true;
    let request = BrowserTurn {
        request_id: request_id.clone(),
        owner,
        turn_token: token.clone(),
        context: task.context.clone(),
        model: task.model.clone(),
        effort: task.effort.clone(),
        task: task.clone(),
        mode: inner.execution_mode(),
        events,
        cancellation: cancellation.clone(),
        confirmed: confirmation.clone(),
    };
    if inner
        .browser
        .try_send(BrowserRequest::Turn(request))
        .is_err()
    {
        inner.turns.lock().unwrap().remove(&request_id);
        return error(
            StatusCode::SERVICE_UNAVAILABLE,
            "browser_unavailable",
            "browser request consumer unavailable or full",
        );
    }
    let (wire_tx, wire_rx) = mpsc::channel(inner.config.channel_capacity);
    tokio::spawn(run_round(
        inner.clone(),
        request_id.clone(),
        request_id.clone(),
        task.model,
        event_rx,
        command_rx,
        cancellation.clone(),
        confirmation,
        deadline,
        wire_tx,
    ));
    session::arm_expiry(
        inner.clone(),
        request_id.clone(),
        cancellation.clone(),
        deadline,
    );
    response_body(wire_rx, streaming).await
}
async fn response_body(mut wire_rx: mpsc::Receiver<Value>, streaming: bool) -> Response {
    if !streaming {
        let mut terminal = None;
        while let Some(value) = wire_rx.recv().await {
            if value["response"]["status"]
                .as_str()
                .is_some_and(|s| s != "in_progress")
            {
                terminal = Some(value["response"].clone());
            }
        }
        return Json(terminal.unwrap_or(json!({"error":{"code":"stream_closed"}}))).into_response();
    }
    let stream = ReceiverStream::new(wire_rx).map(|value| {
        Ok::<_, Infallible>(
            Event::default()
                .event(value["type"].as_str().unwrap_or("error"))
                .data(value.to_string()),
        )
    });
    Sse::new(stream.chain(tokio_stream::once(Ok::<_, Infallible>(
        Event::default().data("[DONE]"),
    ))))
    .keep_alive(KeepAlive::default())
    .into_response()
}

struct TurnGuard {
    inner: Arc<Inner>,
    id: String,
    suspend: bool,
}
impl Drop for TurnGuard {
    fn drop(&mut self) {
        if self.suspend {
            return;
        }
        if let Some(turn) = self.inner.turns.lock().unwrap().remove(&self.id) {
            turn.cancel.send_replace(Some("turn_retired".into()));
        }
    }
}
struct Wire {
    tx: mpsc::Sender<Value>,
    sequence: u64,
    id: String,
    model: String,
    output: Vec<Value>,
    text: String,
    message_id: String,
    text_open: bool,
    send_failed: bool,
}
impl Wire {
    async fn emit(&mut self, kind: &str, mut data: Value) {
        data["type"] = json!(kind);
        data["sequence_number"] = json!(self.sequence);
        self.sequence += 1;
        if !matches!(
            tokio::time::timeout(Duration::from_secs(1), self.tx.send(data)).await,
            Ok(Ok(()))
        ) {
            self.send_failed = true;
        }
    }
    fn snapshot(&self, status: &str) -> Value {
        json!({"id":self.id,"object":"response","created_at":chrono::Utc::now().timestamp(),"status":status,"model":self.model,"output":self.output,"end_turn":true})
    }
    async fn text_delta(&mut self, delta: String) {
        let index = self.output.len();
        if !self.text_open {
            self.text_open = true;
            self.emit("response.output_item.added",json!({"output_index":index,"item":{"id":self.message_id,"type":"message","role":"assistant","status":"in_progress","content":[]}})).await;
            self.emit("response.content_part.added",json!({"item_id":self.message_id,"output_index":index,"content_index":0,"part":{"type":"output_text","text":"","annotations":[]}})).await;
        }
        self.text.push_str(&delta);
        self.emit(
            "response.output_text.delta",
            json!({"item_id":self.message_id,"output_index":index,"content_index":0,"delta":delta}),
        )
        .await;
    }
    async fn close_text(&mut self) {
        if !self.text_open {
            return;
        }
        let index = self.output.len();
        let part = json!({"type":"output_text","text":self.text,"annotations":[]});
        let item = json!({"id":self.message_id,"type":"message","role":"assistant","status":"completed","content":[part.clone()]});
        self.emit("response.output_text.done",json!({"item_id":self.message_id,"output_index":index,"content_index":0,"text":self.text})).await;
        self.emit(
            "response.content_part.done",
            json!({"item_id":self.message_id,"output_index":index,"content_index":0,"part":part}),
        )
        .await;
        self.emit(
            "response.output_item.done",
            json!({"output_index":index,"item":item}),
        )
        .await;
        self.output.push(item);
        self.text.clear();
        self.text_open = false;
        self.message_id = format!("msg_{}", Uuid::new_v4().simple());
    }
    async fn item(&mut self, item: Value) {
        self.close_text().await;
        let index = self.output.len();
        self.emit(
            "response.output_item.added",
            json!({"output_index":index,"item":item}),
        )
        .await;
        let kind = item["type"].as_str().unwrap_or("");
        if kind == "function_call" {
            self.emit(
                "response.function_call_arguments.done",
                json!({"item_id":item["id"],"output_index":index,"arguments":item["arguments"]}),
            )
            .await;
        }
        if kind == "custom_tool_call" {
            self.emit(
                "response.custom_tool_call_input.done",
                json!({"item_id":item["id"],"output_index":index,"input":item["input"]}),
            )
            .await;
        }
        self.emit(
            "response.output_item.done",
            json!({"output_index":index,"item":item}),
        )
        .await;
        self.output.push(item);
    }
    async fn terminal(&mut self, status: &str, usage: Option<Value>, failure: Option<Value>) {
        self.close_text().await;
        let mut response = self.snapshot(status);
        if let Some(usage) = usage {
            response["usage"] = usage;
        }
        if let Some(failure) = failure {
            if status == "incomplete" {
                response["incomplete_details"] = failure;
            } else {
                response["error"] = failure.clone();
                response["last_error"] = failure;
            }
        }
        self.emit(&format!("response.{status}"), json!({"response":response}))
            .await;
    }
}
#[allow(clippy::too_many_arguments)]
async fn run_round(
    inner: Arc<Inner>,
    id: String,
    response_id: String,
    model: String,
    event_rx: Arc<tokio::sync::Mutex<mpsc::Receiver<BrowserEvent>>>,
    command_rx: Arc<tokio::sync::Mutex<mpsc::Receiver<RoundCommand>>>,
    mut cancel: watch::Receiver<Option<String>>,
    confirmed: watch::Receiver<bool>,
    deadline: Instant,
    tx: mpsc::Sender<Value>,
) {
    let mut guard = TurnGuard {
        inner: inner.clone(),
        id: id.clone(),
        suspend: false,
    };
    let mut events = event_rx.lock().await;
    let mut commands = command_rx.lock().await;
    let mut wire = Wire {
        tx,
        sequence: 0,
        id: response_id,
        model,
        output: vec![],
        text: String::new(),
        message_id: format!("msg_{}", Uuid::new_v4().simple()),
        text_open: false,
        send_failed: false,
    };
    wire.emit(
        "response.created",
        json!({"response":wire.snapshot("in_progress")}),
    )
    .await;
    wire.emit(
        "response.in_progress",
        json!({"response":wire.snapshot("in_progress")}),
    )
    .await;
    loop {
        if wire.send_failed {
            inner.cancelled.fetch_add(1, Ordering::SeqCst);
            break;
        }
        let event = tokio::select! {
            biased;
            _ = wire.tx.closed() => { inner.cancelled.fetch_add(1,Ordering::SeqCst); break; }
            _ = tokio::time::sleep_until(deadline.into()) => BrowserEvent::Failed {code:"turn_expired".into(),message:"turn capability expired".into()},
            changed = cancel.changed() => {
                let reason = cancel.borrow().clone().unwrap_or_else(|| "client_cancelled".into());
                if changed.is_err() { break; }
                BrowserEvent::Failed { code: reason.clone(), message: reason }
            }
            command = commands.recv() => {
                if let Some(RoundCommand::ManualComplete(answer)) = &command {
                    wire.text_delta(answer.clone()).await;
                    wire.terminal("completed",None,None).await;
                    session::remember(&inner,&id,&wire.snapshot("completed"));
                    inner.completed.fetch_add(1,Ordering::SeqCst);break;
                }
                if let Some(RoundCommand::Tools(items)) = command {
                    if let Some(turn)=inner.turns.lock().unwrap().get_mut(&id) {
                        for item in &items {if let Some(pending)=item["call_id"].as_str().and_then(|key|turn.pending.get_mut(key)){pending.delivered=true;}}
                    }
                    for item in items { wire.item(item).await; }
                    wire.close_text().await;
                    let mut snapshot = wire.snapshot("completed");
                    snapshot["end_turn"] = json!(false);
                    session::remember(&inner, &id, &snapshot);
                    if let Some(turn) = inner.turns.lock().unwrap().get_mut(&id) { turn.observer_active = false; }
                    guard.suspend = true;
                    wire.emit("response.completed", json!({"response":snapshot})).await;
                    break;
                }
                continue;
            }
            event = events.recv() => event.unwrap_or(BrowserEvent::Failed { code:"browser_disconnected".into(),message:"browser event channel closed before terminal event".into() })
        };
        let event = if matches!(event, BrowserEvent::Completed { .. })
            && inner
                .turns
                .lock()
                .unwrap()
                .get(&id)
                .is_some_and(|t| t.inflight_tools > 0)
        {
            BrowserEvent::Failed {
                code: "pending_native_tools".into(),
                message: "browser cannot finish before all native tool calls settle".into(),
            }
        } else {
            event
        };
        let event = if !*confirmed.borrow()
            && !matches!(event, BrowserEvent::Failed { .. } | BrowserEvent::Cancelled)
        {
            BrowserEvent::Failed {
                code: "manual_not_confirmed".into(),
                message: "manual turn must be confirmed sent before browser events".into(),
            }
        } else {
            event
        };
        match event {
            BrowserEvent::TextDelta(text) => {
                let compaction = inner
                    .turns
                    .lock()
                    .unwrap()
                    .get(&id)
                    .map(|t| t.task.compaction)
                    .unwrap_or_default();
                if compaction == CompactionMode::RemoteV2 {
                    wire.text.push_str(&text);
                } else {
                    wire.text_delta(text).await;
                }
            }
            BrowserEvent::OutputItem(item) => wire.item(item).await,
            BrowserEvent::Completed { output, usage } => {
                let compaction = inner
                    .turns
                    .lock()
                    .unwrap()
                    .get(&id)
                    .map(|t| t.task.compaction)
                    .unwrap_or_default();
                let supplied_text = output
                    .iter()
                    .flat_map(|item| item["content"].as_array().into_iter().flatten())
                    .filter_map(|part| part["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("");
                if compaction != CompactionMode::None
                    && wire.text.trim().is_empty()
                    && supplied_text.trim().is_empty()
                {
                    wire.terminal("failed",None,Some(json!({"code":"empty_compaction_summary","message":"compaction completed without a real summary"}))).await;
                    inner.failed.fetch_add(1, Ordering::SeqCst);
                    break;
                }
                if compaction == CompactionMode::RemoteV2 {
                    use base64::Engine;
                    let summary = if wire.text.is_empty() {
                        supplied_text
                    } else {
                        wire.text.clone()
                    };
                    wire.text_open = false;
                    wire.output.clear();
                    wire.item(json!({"type":"compaction","id":format!("cmp_{}",Uuid::new_v4().simple()),"encrypted_content":format!("ocx1:{}",base64::engine::general_purpose::STANDARD.encode(summary))})).await;
                } else {
                    for item in output {
                        wire.item(item).await;
                    }
                }
                wire.terminal("completed", usage, None).await;
                session::remember(&inner, &id, &wire.snapshot("completed"));
                inner.completed.fetch_add(1, Ordering::SeqCst);
                break;
            }
            BrowserEvent::Incomplete { reason, usage } => {
                wire.terminal("incomplete", usage, Some(json!({"reason":reason})))
                    .await;
                inner.failed.fetch_add(1, Ordering::SeqCst);
                break;
            }
            BrowserEvent::Failed { code, message } => {
                wire.terminal(
                    "failed",
                    None,
                    Some(json!({"code":code,"message":message,"type":"server_error"})),
                )
                .await;
                if code == "client_cancelled" || code == "bridge_closed" {
                    inner.cancelled.fetch_add(1, Ordering::SeqCst);
                } else {
                    inner.failed.fetch_add(1, Ordering::SeqCst);
                }
                break;
            }
            BrowserEvent::Cancelled => {
                wire.terminal("failed",None,Some(json!({"code":"client_cancelled","message":"browser turn cancelled","type":"invalid_request_error"}))).await;
                inner.cancelled.fetch_add(1, Ordering::SeqCst);
                break;
            }
        }
    }
}

fn rpc_error(id: Value, code: i64, message: impl Into<String>) -> Response {
    Json(json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message.into()}}))
        .into_response()
}
async fn mcp(State(inner): State<Arc<Inner>>, Json(body): Json<Value>) -> Response {
    let id = body.get("id").cloned().unwrap_or(Value::Null);
    if body["jsonrpc"] != "2.0" || !body["method"].is_string() {
        return rpc_error(id, -32600, "Invalid JSON-RPC request");
    }
    let method = body["method"].as_str().unwrap();
    if body.get("id").is_none() {
        return if method.starts_with("notifications/") {
            if method == "notifications/cancelled" {
                let params = &body["params"];
                if let Some(scope) = params["turn_token"]
                    .as_str()
                    .or_else(|| params["request_id"].as_str())
                {
                    inner
                        .mcp_ledger
                        .lock()
                        .unwrap()
                        .cancel(scope, &params["requestId"]);
                }
            }
            StatusCode::ACCEPTED.into_response()
        } else {
            rpc_error(id, -32600, "request id required")
        };
    }
    let mut activity =
        (method == "tools/call").then(|| mcp_ledger::Activity::begin(inner.clone(), &body));
    let result = match method {
        "initialize" => Ok(
            json!({"protocolVersion":"2024-11-05","capabilities":{"tools":{"listChanged":false}},"serverInfo":{"name":"codex-web-gpui-bridge","version":env!("CARGO_PKG_VERSION")}}),
        ),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({"tools":mcp_tools(inner.execution_mode())})),
        "tools/call" => {
            call_tool(
                inner,
                &body["params"],
                activity.as_ref().unwrap().cancellation.clone(),
            )
            .await
        }
        _ => return rpc_error(id, -32601, "Method not found"),
    };
    if let Some(activity) = &mut activity {
        activity.prepared(&result);
    }
    match result {
        Ok(result) => Json(json!({"jsonrpc":"2.0","id":id,"result":result})).into_response(),
        Err(message) => rpc_error(id, -32602, message),
    }
}
fn mcp_tools(mode: ExecutionMode) -> Vec<Value> {
    let key = if mode == ExecutionMode::Manual {
        "request_id"
    } else {
        "turn_token"
    };
    let mut reference = json!({"type":"object","properties":{},"required":[key]});
    reference["properties"][key] = json!({"type":"string","minLength":20,"maxLength":256});
    let mut inventory = reference.clone();
    inventory["properties"]["query"] = json!({"type":"string"});
    inventory["properties"]["offset"] = json!({"type":"integer","minimum":0});
    inventory["properties"]["limit"] = json!({"type":"integer","minimum":1,"maximum":50});
    inventory["properties"]["include_schema"] = json!({"type":"boolean"});
    let mut call = reference;
    call["properties"]["wire_name"] = json!({"type":"string"});
    call["properties"]["arguments"] = json!({"type":"object"});
    call["properties"]["input"] = json!({"type":"string"});
    call["required"] = json!([key, "wire_name"]);
    let mut tools = vec![
        json!({"name":"codex_tool_inventory","description":"List the exact tool registry supplied to this active Codex turn.","inputSchema":inventory}),
        json!({"name":"codex_tool_call","description":"Invoke an exact wire_name through the owning native harness. Approvals and execution belong to that harness.","inputSchema":call}),
    ];
    if mode == ExecutionMode::Manual {
        tools.push(json!({"name":"codex_turn_start","description":"Connect the request_id included in the pasted Codex Web GPT request so its Codex tools can be used.","inputSchema":{"type":"object","properties":{"request_id":{"type":"string","minLength":20,"maxLength":256}},"required":["request_id"]}}));
        tools.push(json!({"name":"codex_turn_complete","description":"Send the complete answer back to the connected Codex request after its work is finished. For compaction, send the requested compacted summary.","inputSchema":{"type":"object","properties":{"request_id":{"type":"string","minLength":20,"maxLength":256},"final_answer":{"type":"string","minLength":1,"maxLength":5000000}},"required":["request_id","final_answer"]}}));
    }
    tools.extend(mcp_policy::aliases(key, mode));
    tools
}
fn tool_result(value: Value) -> Value {
    json!({"content":[{"type":"text","text":value.to_string()}],"structuredContent":value})
}
struct PendingTool(Arc<Inner>, String);
impl Drop for PendingTool {
    fn drop(&mut self) {
        self.0.pending_tools.fetch_sub(1, Ordering::SeqCst);
        if let Some(turn) = self.0.turns.lock().unwrap().get_mut(&self.1) {
            turn.inflight_tools = turn.inflight_tools.saturating_sub(1);
        }
    }
}
async fn call_tool(
    inner: Arc<Inner>,
    params: &Value,
    mut rpc_cancel: watch::Receiver<Option<String>>,
) -> Result<Value, String> {
    let name = params["name"].as_str().ok_or("tool name required")?;
    if let Some(result) = compaction::control_call(&inner, name, &params["arguments"]) {
        return result;
    }
    if matches!(name, "codex_turn_start" | "codex_turn_complete") {
        return session::manual_control(&inner, name, &params["arguments"]);
    }
    if !matches!(
        name,
        "codex_tool_inventory"
            | "codex_tool_call"
            | "codex_exec"
            | "codex_write_stdin"
            | "codex_apply_patch"
            | "codex_view_image"
    ) {
        return Err("unsupported bridge tool".into());
    }
    let args = params["arguments"]
        .as_object()
        .ok_or("arguments must be an object")?;
    let key = if inner.execution_mode() == ExecutionMode::Manual {
        "request_id"
    } else {
        "turn_token"
    };
    let token = args
        .get(key)
        .and_then(Value::as_str)
        .ok_or("turn capability required")?;
    let (request_id, owner, tools, mut cancellation, deadline) = {
        let mut turns = inner.turns.lock().unwrap();
        let (id, turn) = turns
            .iter_mut()
            .find(|(_, t)| t.token == token)
            .ok_or("turn token is invalid or expired")?;
        validate_turn(turn, None)?;
        // Optional explicit ownership claims may narrow a capability, never widen it.
        if args
            .get("thread_id")
            .is_some_and(|v| v.as_str() != Some(turn.owner.thread_id.as_str()))
            || args
                .get("turn_id")
                .is_some_and(|v| v.as_str() != Some(turn.owner.turn_id.as_str()))
        {
            return Err("turn owner mismatch".into());
        }
        if turn.compaction_request.is_some() {
            if name == "codex_tool_inventory" {
                return Ok(tool_result(
                    json!({"tools":[],"total":0,"next_offset":null}),
                ));
            }
            turn.compaction_interceptions += 1;
            return Ok(compaction::interruption(inner.execution_mode(), false));
        }
        (
            id.clone(),
            turn.owner.clone(),
            mcp_policy::visible(&turn.tools, inner.execution_mode()),
            turn.cancel.subscribe(),
            turn.deadline,
        )
    };
    if name == "codex_tool_inventory" {
        let needle = args
            .get("query")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_lowercase();
        let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0) as usize;
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .unwrap_or(20)
            .clamp(1, 50) as usize;
        let include_schema = args.get("include_schema") != Some(&Value::Bool(false));
        let matching: Vec<_> = tools
            .iter()
            .filter(|t| {
                format!("{} {}", t.wire_name, t.definition)
                    .to_lowercase()
                    .contains(&needle)
            })
            .collect();
        let page: Vec<_> = matching.iter().skip(offset).take(limit).map(|tool| {
            let mut item = json!({"wire_name":tool.wire_name,"name":tool.name,"namespace":tool.namespace,"description":mcp_policy::description(tool),"kind":if tool.freeform {"freeform"} else {"function"}});
            if include_schema { item["parameters"] = mcp_policy::parameters(tool); }
            item
        }).collect();
        let next = (offset.saturating_add(page.len()) < matching.len())
            .then_some(offset.saturating_add(page.len()));
        return Ok(tool_result(
            json!({"tools":page,"total":matching.len(),"next_offset":next}),
        ));
    }
    let (wire_name, arguments, input) = if name == "codex_tool_call" {
        let wire = args
            .get("wire_name")
            .and_then(Value::as_str)
            .ok_or("wire_name required")?;
        let input = args
            .get("input")
            .map(|v| {
                v.as_str()
                    .map(str::to_owned)
                    .ok_or("input must be a string")
            })
            .transpose()?;
        (wire.to_owned(), args.get("arguments").cloned(), input)
    } else {
        mcp_policy::resolve(name, args, &tools)?
    };
    let tool = tools
        .iter()
        .find(|t| t.wire_name == wire_name)
        .ok_or("Codex tool is not available in this turn")?;
    mcp_policy::validate(tool, arguments.as_ref().unwrap_or(&json!({})))?;
    if arguments.as_ref().is_some_and(|v| !v.is_object()) {
        return Err("arguments must be an object".into());
    }
    if tool.freeform {
        if input.is_none()
            || arguments
                .as_ref()
                .is_some_and(|v| !v.as_object().unwrap().is_empty())
        {
            return Err("freeform tool requires input and does not accept arguments".into());
        }
    } else if input.is_some() {
        return Err("function tool does not accept freeform input".into());
    }
    let (result, receiver) = oneshot::channel();
    let (call_cancel, call_cancellation) = watch::channel(None);
    let call = NativeToolCall {
        request_id: request_id.clone(),
        owner,
        call_id: format!("call_{}", Uuid::new_v4().simple()),
        wire_name: wire_name.into(),
        arguments: if tool.freeform {
            None
        } else {
            Some(arguments.unwrap_or(json!({})))
        },
        input,
        cancellation: call_cancellation,
        result,
    };
    {
        let mut turns = inner.turns.lock().unwrap();
        let turn = turns
            .get_mut(&request_id)
            .ok_or("turn retired before tool dispatch")?;
        validate_turn(turn, None)?;
        turn.inflight_tools += 1;
        inner.pending_tools.fetch_add(1, Ordering::SeqCst);
    }
    let _pending = PendingTool(inner.clone(), request_id);
    inner
        .browser
        .try_send(BrowserRequest::ToolCall(call))
        .map_err(|_| "native harness channel unavailable or full")?;
    let result = tokio::select! {
        biased;
        _ = cancellation.changed() => { call_cancel.send_replace(Some("turn was cancelled or retired".into())); return Err("turn was cancelled or retired".into()); },
        _ = rpc_cancel.changed() => { call_cancel.send_replace(Some("MCP request cancelled".into())); return Err("MCP request cancelled".into()); },
        _ = tokio::time::sleep_until(deadline.into()) => { call_cancel.send_replace(Some("turn token expired".into())); return Err("turn token expired".into()); },
        result = receiver => result.map_err(|_| "native harness dropped tool result")??,
    };
    if !result["content"].is_array() {
        return Err(
            "native harness must return an actual MCP CallToolResult with content array".into(),
        );
    }
    Ok(result)
}
