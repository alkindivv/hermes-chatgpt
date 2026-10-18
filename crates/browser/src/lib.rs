//! Native CDP adapter. The caller must supply its own CEF page WebSocket.
mod cdp;
mod checkpoint;
pub use checkpoint::{
    CheckpointCleanup, CheckpointOutcome, CheckpointRequest, CheckpointSettlement,
};
mod inspection;
pub mod markdown;
pub use markdown::{AtomicMarkdownBuffer, MarkdownError, render_assistant_markdown};
mod connector;
mod preparation;
mod selection;
pub use connector::ConnectorReceipt;
mod session;
pub use session::{SessionFailure, SessionReceipt};
pub mod smoke;
pub use smoke::{SMOKE_EXPECTED, SMOKE_PROMPT, smoke_turn_input, validate_smoke_response};
mod upload;
pub use cdp::{Cancellation, CdpClient};
pub use inspection::{
    AccountInspection, AvailableRoute, CapabilityInspection, CapabilityOptions, NativeZoomRequest,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{collections::HashSet, path::PathBuf, time::Duration};
use tokio::sync::{Mutex, mpsc::UnboundedSender};
pub type Result<T> = std::result::Result<T, Error>;
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("owned helper cleanup is unconfirmed; this engine cannot send another epoch")]
    CleanupUnconfirmed,
    #[error(transparent)]
    Markdown(#[from] MarkdownError),
    #[error("invalid owned endpoint: {0}")]
    Endpoint(String),
    #[error("CDP transport: {0}")]
    Transport(String),
    #[error("CDP protocol: {0}")]
    Protocol(String),
    #[error("page operation: {0}")]
    Dom(String),
    #[error("operation timed out; a send may already have been accepted")]
    Timeout,
    #[error("cancelled; a send may already have been accepted")]
    Cancelled,
    #[error("task/epoch has already attempted a send; do not resend")]
    DuplicateSend,
    #[error("ownership mismatch")]
    Ownership,
    #[error("unsupported exact model or effort: {0}")]
    Model(String),
    #[error("server authentication not proven")]
    Unauthenticated,
    #[error("DOM/session probing is unavailable in manual mode")]
    ManualDomForbidden,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Ownership {
    pub task_id: String,
    pub epoch: u64,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum InteractionMode {
    Automatic,
    Manual,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Attachment {
    pub path: PathBuf,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnInput {
    #[serde(default)]
    pub connector_name: Option<String>,
    #[serde(rename = "fullPrompt", alias = "full_prompt")]
    pub full_prompt: String,
    pub model: String,
    pub effort: String,
    pub attachments: Vec<Attachment>,
    #[serde(rename = "interactionMode", alias = "interaction_mode")]
    pub interaction_mode: InteractionMode,
    pub ownership: Ownership,
    #[serde(default)]
    pub turn_token: Option<String>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Progress {
    Accepted {
        ownership: Ownership,
        user_turn_id: String,
    },
    Assistant {
        ownership: Ownership,
        turn_id: String,
        text: String,
        html: String,
    },
    Commentary {
        ownership: Ownership,
        turn_id: String,
        text: String,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum TurnResult {
    Completed {
        ownership: Ownership,
        user_turn_id: String,
        assistant_turn_id: String,
        text: String,
        html: String,
    },
    /// Native UI must offer Copy and Sent. No DOM access occurred for this submission.
    ManualHandoff { input: TurnInput },
}
#[derive(Debug, Deserialize, Serialize)]
pub struct AuthInspection {
    pub readable: bool,
    pub composer: bool,
    pub server_authenticated: bool,
    #[serde(default)]
    pub signed_out_confirmed: bool,
}
#[derive(Debug, Deserialize)]
struct UserTurn {
    id: String,
    text: Option<String>,
    #[serde(default)]
    tokens: Vec<String>,
    #[serde(default)]
    pending: bool,
}
#[derive(Debug, Deserialize)]
struct Response {
    id: String,
    text: String,
    html: String,
    commentary: String,
    complete: bool,
}
#[derive(Debug, Deserialize)]
struct Snapshot {
    document_id: String,
    ids: Vec<String>,
    users: Vec<UserTurn>,
    responses: Vec<Response>,
    running: bool,
}
#[derive(Default)]
struct State {
    cleanup_unconfirmed: bool,
    owner: Option<Ownership>,
    attempts: HashSet<(String, u64)>,
}
pub struct BrowserEngine {
    client: CdpClient,
    state: Mutex<State>,
}
/// Baselines include virtualized containers and both logical group roles.
pub fn new_turn_identity<'a>(
    baseline: &[String],
    current: impl IntoIterator<Item = &'a str>,
) -> Result<Option<String>> {
    let added: Vec<_> = current
        .into_iter()
        .filter(|id| !baseline.iter().any(|old| old == id))
        .collect();
    if added.len() > 1 {
        return Err(Error::Dom("multiple new logical turns".into()));
    }
    Ok(added.first().map(|s| s.to_string()))
}
fn unique_literal_token(text: &str, token: &str) -> bool {
    !token.is_empty()
        && text
            .as_bytes()
            .windows(token.len())
            .filter(|candidate| *candidate == token.as_bytes())
            .count()
            == 1
}
fn accepted_matches(user: &UserTurn, input: &TurnInput) -> bool {
    if let Some(token) = &input.turn_token {
        user.tokens.len() == 1 && user.tokens.first() == Some(token)
    } else {
        let normalize = |text: &str| text.replace("\r\n", "\n").replace('\r', "\n");
        user.text
            .as_deref()
            .is_some_and(|text| normalize(text) == normalize(&input.full_prompt))
    }
}
impl BrowserEngine {
    /// The URL itself names the owned page target; browser endpoints/discovery are refused.
    pub async fn connect(ws_url: &str) -> Result<Self> {
        Ok(Self {
            client: CdpClient::connect(ws_url).await?,
            state: Mutex::new(State::default()),
        })
    }
    async fn page(&self, op: &str, arg: Value, cancel: &Cancellation) -> Result<Value> {
        let expr = format!(
            "(()=>{{{}\n{}\nreturn ({})({},{});}})()",
            include_str!("../assets/user-message.js"),
            include_str!("../assets/classifier.js"),
            include_str!("../assets/page.js"),
            json!(op),
            arg
        );
        self.client.evaluate(&expr, cancel).await
    }
    async fn snapshot(&self, token: Option<&str>, cancel: &Cancellation) -> Result<Snapshot> {
        serde_json::from_value(
            self.page("snapshot", json!({"turn_token":token}), cancel)
                .await?,
        )
        .map_err(|e| Error::Dom(e.to_string()))
    }
    pub async fn inspect_auth(&self) -> Result<AuthInspection> {
        self.inspect_auth_for_mode(InteractionMode::Automatic, Cancellation::default())
            .await
    }
    pub async fn inspect_auth_for_mode(
        &self,
        mode: InteractionMode,
        cancellation: Cancellation,
    ) -> Result<AuthInspection> {
        if mode == InteractionMode::Manual {
            return Err(Error::ManualDomForbidden);
        }
        let _state = tokio::select! {_=cancellation.cancelled()=>return Err(Error::Cancelled),state=self.state.lock()=>state};
        serde_json::from_value(self.page("auth", Value::Null, &cancellation).await?)
            .map_err(|e| Error::Dom(e.to_string()))
    }
    pub async fn navigate(&self, url: &str) -> Result<()> {
        self.navigate_with_cancellation(url, Cancellation::default())
            .await
    }
    /// CDP main-frame load readiness, without evaluating DOM (also safe in Manual).
    pub async fn navigate_with_cancellation(
        &self,
        url: &str,
        cancellation: Cancellation,
    ) -> Result<()> {
        let parsed = url::Url::parse(url).map_err(|e| Error::Endpoint(e.to_string()))?;
        if parsed.scheme() != "https"
            || parsed.host_str() != Some("chatgpt.com")
            || parsed.port_or_known_default() != Some(443)
            || !parsed.username().is_empty()
            || parsed.password().is_some()
        {
            return Err(Error::Endpoint("ChatGPT HTTPS navigation required".into()));
        }
        self.navigate_owned_document(url, cancellation).await
    }
    async fn navigate_owned_document(&self, url: &str, cancellation: Cancellation) -> Result<()> {
        let _state = tokio::select! {_=cancellation.cancelled()=>return Err(Error::Cancelled),state=self.state.lock()=>state};
        let mut events = self.client.events();
        for (method, params) in [
            ("Page.enable", json!({})),
            ("Page.setLifecycleEventsEnabled", json!({"enabled":true})),
        ] {
            self.client
                .request(method, params, Duration::from_secs(10), &cancellation)
                .await?;
        }
        let result = self
            .client
            .request(
                "Page.navigate",
                json!({"url":url}),
                Duration::from_secs(10),
                &cancellation,
            )
            .await?;
        if result.get("errorText").is_some() || result["isDownload"] == true {
            return Err(Error::Dom("owned target navigation failed".into()));
        }
        let frame = result["frameId"]
            .as_str()
            .filter(|frame| !frame.is_empty())
            .ok_or_else(|| Error::Dom("navigation omitted main-frame identity".into()))?;
        // Same-document navigation has no new loader; the existing document remains loaded.
        let Some(loader) = result["loaderId"].as_str() else {
            return Ok(());
        };
        let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
        loop {
            let event = tokio::select! {_=cancellation.cancelled()=>return Err(Error::Cancelled),event=tokio::time::timeout_at(deadline,events.recv())=>event.map_err(|_|Error::Timeout)?.map_err(|_|Error::Transport("navigation lifecycle observation lost".into()))?};
            if event["method"] == "Page.lifecycleEvent"
                && event["params"]["frameId"] == frame
                && event["params"]["loaderId"] == loader
                && event["params"]["name"] == "load"
            {
                return Ok(());
            }
        }
    }
    /// Only call after submit has returned (including cancellation). Never stops another owner.
    pub async fn stop(&self, owner: &Ownership) -> Result<()> {
        let state = self.state.lock().await;
        if state.owner.as_ref() != Some(owner) {
            return Err(Error::Ownership);
        }
        self.page("stop", Value::Null, &Cancellation::default())
            .await?;
        Ok(())
    }
    /// Explicit caller action; Page.close addresses only this connected target.
    pub async fn close(&self) -> Result<()> {
        let _state = self.state.lock().await;
        self.client
            .request(
                "Page.close",
                json!({}),
                Duration::from_secs(10),
                &Cancellation::default(),
            )
            .await?;
        Ok(())
    }
    pub async fn submit(
        &self,
        input: TurnInput,
        progress: UnboundedSender<Progress>,
        cancellation: Cancellation,
    ) -> Result<TurnResult> {
        if cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        if input.interaction_mode == InteractionMode::Manual {
            return Ok(TurnResult::ManualHandoff { input });
        }
        if let Some(token) = &input.turn_token
            && (!unique_literal_token(&input.full_prompt, token))
        {
            return Err(Error::Dom(
                "supplied literal token must occur exactly once in the prompt".into(),
            ));
        }
        let mut state = tokio::select! { biased; _=cancellation.cancelled()=>return Err(Error::Cancelled),s=self.state.lock()=>s };
        if state.cleanup_unconfirmed {
            return Err(Error::CleanupUnconfirmed);
        }
        let key = (input.ownership.task_id.clone(), input.ownership.epoch);
        if state.attempts.contains(&key) {
            return Err(Error::DuplicateSend);
        }
        if let Some(old) = &state.owner
            && (old.task_id != input.ownership.task_id || input.ownership.epoch < old.epoch)
        {
            return Err(Error::Ownership);
        }
        let prepared = self
            .prepare_submission(&input, &cancellation, &mut state)
            .await?;
        if cancellation.is_cancelled() {
            self.rollback_preparation(&input, &prepared.baseline, &mut state)
                .await?;
            return Err(Error::Cancelled);
        }
        let baseline = &prepared.baseline;
        state.owner = Some(input.ownership.clone());
        // Reserve before dispatch: a lost CDP reply is an uncertain send, never permission to retry.
        state.attempts.insert(key);
        // A dropped/failed future cannot silently return this page to the next epoch.
        state.cleanup_unconfirmed = true;
        let result = async {
        self.send_prepared(&input, &prepared, &cancellation).await?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(180);
        let mut accepted = None;
        let mut bound = None;
        let mut last = String::new();
        let mut commentary = String::new();
        let mut stable: Option<(String, tokio::time::Instant)> = None;
        let mut markdown = AtomicMarkdownBuffer::default();
        loop {
            tokio::select! { biased; _=cancellation.cancelled()=>return Err(Error::Cancelled),_=tokio::time::sleep(Duration::from_millis(200))=>{} }
            if accepted.is_none() && tokio::time::Instant::now() > deadline {
                return Err(Error::Timeout);
            }
            let snapshot = self
                .snapshot(input.turn_token.as_deref(), &cancellation)
                .await?;
            if snapshot.document_id != baseline.document_id {
                return Err(Error::Dom("document changed during owned turn".into()));
            }
            if accepted.is_none()
                && let Some(id) =
                    new_turn_identity(&baseline.ids, snapshot.users.iter().map(|u| u.id.as_str()))?
            {
                let user = snapshot.users.iter().find(|u| u.id == id).unwrap();
                if user.pending {
                    continue;
                }
                let matches = accepted_matches(user, &input);
                if !matches {
                    return Err(Error::Dom("accepted user prompt does not match".into()));
                }
                let _ = progress.send(Progress::Accepted {
                    ownership: input.ownership.clone(),
                    user_turn_id: id.clone(),
                });
                accepted = Some(id);
            }
            if accepted.is_none() {
                continue;
            }
            let candidate = new_turn_identity(
                &baseline.ids,
                snapshot.responses.iter().map(|r| r.id.as_str()),
            )?;
            if let Some(id) = candidate {
                if bound.as_ref().is_some_and(|b| b != &id) {
                    return Err(Error::Dom("assistant identity changed".into()));
                }
                bound = Some(id);
            }
            let Some(response) = snapshot
                .responses
                .iter()
                .find(|r| Some(&r.id) == bound.as_ref())
            else {
                stable = None;
                continue;
            };
            let signature = format!("{}\\0{}", response.text, response.html);
            if signature != last {
                markdown.observe(&response.html, &response.text)?;
                let _ = progress.send(Progress::Assistant {
                    ownership: input.ownership.clone(),
                    turn_id: response.id.clone(),
                    text: response.text.clone(),
                    html: response.html.clone(),
                });
                last = signature;
            }
            if commentary != response.commentary {
                commentary = response.commentary.clone();
                let _ = progress.send(Progress::Commentary {
                    ownership: input.ownership.clone(),
                    turn_id: response.id.clone(),
                    text: commentary.clone(),
                });
            }
            if response.complete && !snapshot.running && !response.text.is_empty() {
                let rendered = render_assistant_markdown(&response.html, &response.text)?;
                if stable
                    .as_ref()
                    .is_none_or(|(previous, _)| previous != &rendered)
                {
                    stable = Some((rendered, tokio::time::Instant::now()));
                }
                if stable
                    .as_ref()
                    .is_some_and(|(_, since)| since.elapsed() >= Duration::from_secs(2))
                {
                    return Ok(TurnResult::Completed {
                        ownership: input.ownership.clone(),
                        user_turn_id: accepted.unwrap(),
                        assistant_turn_id: response.id.clone(),
                        text: markdown.finish()?,
                        html: response.html.clone(),
                    });
                }
            } else {
                stable = None
            }
        }
        }.await;
        if result.is_err()
            && self
                .cleanup_after_send(baseline, &input, input.turn_token.as_deref())
                .await
                .is_err()
        {
            return Err(Error::CleanupUnconfirmed);
        }
        state.cleanup_unconfirmed = false;
        result
    }
    async fn key(&self, key: &str, cancel: &Cancellation) -> Result<()> {
        for kind in ["keyDown", "keyUp"] {
            self.client.request("Input.dispatchKeyEvent",json!({"type":kind,"key":key,"code":key,"windowsVirtualKeyCode":if key=="ArrowRight" {39}else{37}}),Duration::from_secs(5),cancel).await?;
        }
        Ok(())
    }
    async fn attach(&self, attachments: &[Attachment], cancel: &Cancellation) -> Result<()> {
        if attachments.is_empty() {
            return Ok(());
        }
        if attachments.len() > 10 {
            return Err(Error::Dom("at most ten attachments".into()));
        }
        let mut files = Vec::new();
        let mut names = Vec::new();
        let mut size = 0;
        for file in attachments {
            let path = tokio::fs::canonicalize(&file.path)
                .await
                .map_err(|e| Error::Dom(e.to_string()))?;
            let meta = tokio::fs::metadata(&path)
                .await
                .map_err(|e| Error::Dom(e.to_string()))?;
            if !meta.is_file() {
                return Err(Error::Dom("attachment is not a file".into()));
            }
            size += meta.len();
            names.push(path.file_name().unwrap().to_string_lossy().into_owned());
            files.push(path);
        }
        if size > 50 * 1024 * 1024 {
            return Err(Error::Dom("attachments exceed 50 MB".into()));
        }
        let mut uploads = upload::Uploads::start(&self.client, &names, cancel).await?;
        let root = self
            .client
            .request("DOM.getDocument", json!({}), Duration::from_secs(5), cancel)
            .await?;
        let nodes=self.client.request("DOM.querySelectorAll",json!({"nodeId":root["root"]["nodeId"],"selector":"input[data-testid=\"upload-photos-input\"], form[data-chatgpt-composer] input[type=\"file\"][multiple]:not([accept])"}),Duration::from_secs(5),cancel).await?;
        let ids = nodes["nodeIds"]
            .as_array()
            .ok_or_else(|| Error::Dom("file input missing".into()))?;
        if ids.len() != 1 {
            return Err(Error::Dom("ambiguous file input".into()));
        }
        self.client
            .request(
                "DOM.setFileInputFiles",
                json!({"nodeId":ids[0],"files":files}),
                Duration::from_secs(10),
                cancel,
            )
            .await?;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(120);
        loop {
            uploads.poll(&self.client, cancel).await?;
            if uploads.complete() && self.page("attachments", json!(names), cancel).await? == true {
                return Ok(());
            }
            if tokio::time::Instant::now() > deadline {
                return Err(Error::Dom("attachment acceptance not proven".into()));
            }
            tokio::select! {_=cancel.cancelled()=>return Err(Error::Cancelled),_=tokio::time::sleep(Duration::from_millis(200))=>{}}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::{SinkExt, StreamExt};
    use tokio::net::TcpListener;
    async fn server() -> (String, tokio::task::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(stream).await.unwrap();
            let mut held = None;
            let mut sent = false;
            while let Some(Ok(message)) = ws.next().await {
                let v: Value = serde_json::from_slice(&message.into_data()).unwrap();
                let method = v["method"].as_str().unwrap();
                if method == "hold" {
                    continue;
                }
                if method == "first" {
                    held = Some(v);
                    continue;
                }
                if method == "second" {
                    ws.send(
                        json!({"id":v["id"],"result":{"order":2}})
                            .to_string()
                            .into(),
                    )
                    .await
                    .unwrap();
                    let first = held.take().unwrap();
                    ws.send(
                        json!({"id":first["id"],"result":{"order":1}})
                            .to_string()
                            .into(),
                    )
                    .await
                    .unwrap();
                    continue;
                }
                let result = if method == "bad" {
                    json!({"id":v["id"],"error":{"code":-1,"message":"failure"}})
                } else if method == "Runtime.evaluate" {
                    let expr = v["params"]["expression"].as_str().unwrap();
                    let value = if expr.contains(")(\"ready\",") {
                        json!(true)
                    } else if expr.contains(")(\"account\",") {
                        json!({"user_id":"user","account_id":"account","plan_type":"pro","structure":"personal","needs_attention":false,"server_authenticated":true})
                    } else if expr.contains(")(\"capabilityState\",") {
                        json!({"composer_ready":true,"document_ready":true,"selector_count":1,"think_available":false})
                    } else if expr.contains(")(\"capabilityPicker\",") {
                        json!({"available":[true,true,true,true,true],"families":["6","5.6"]})
                    } else if expr.contains(")(\"auth\",") {
                        json!({"readable":true,"composer":true,"server_authenticated":true})
                    } else if expr.contains(")(\"model\",") {
                        json!({"min":0,"max":4,"value":2})
                    } else if expr.contains(")(\"connectorSnapshot\",") {
                        json!({"document_id":"test-document","text":"","selected":[],"rows":[],"has_attachments":false})
                    } else if expr.contains(")(\"snapshot\",") {
                        if sent {
                            json!({"document_id":"test-document","ids":["group:user:new","group:assistant:new"],"users":[{"id":"group:user:new","text":"hello"}],"responses":[{"id":"group:assistant:new","text":"answer","html":"<p><strong>answer</strong></p>","commentary":"thinking","complete":true}],"running":false})
                        } else {
                            json!({"document_id":"test-document","ids":["group:user:old","group:assistant:old"],"users":[],"responses":[],"running":false})
                        }
                    } else {
                        if expr.contains(")(\"send\",") {
                            sent = true
                        }
                        json!(true)
                    };
                    json!({"id":v["id"],"result":{"result":{"value":value}}})
                } else {
                    json!({"id":v["id"],"result":{}})
                };
                if ws.send(result.to_string().into()).await.is_err() {
                    break;
                }
            }
        });
        (format!("ws://127.0.0.1:{port}/devtools/page/owned"), handle)
    }
    #[tokio::test]
    async fn multiplex_timeout_cancel_protocol_and_drop_cleanup() {
        let (url, server) = server().await;
        let client = CdpClient::connect(&url).await.unwrap();
        let c = Cancellation::default();
        let (first, second) = tokio::join!(
            client.request("first", json!({}), Duration::from_secs(1), &c),
            client.request("second", json!({}), Duration::from_secs(1), &c)
        );
        assert_eq!(first.unwrap()["order"], 1);
        assert_eq!(second.unwrap()["order"], 2);
        assert!(matches!(
            client
                .request("hold", json!({}), Duration::from_millis(15), &c)
                .await,
            Err(Error::Timeout)
        ));
        assert!(matches!(
            client
                .request("bad", json!({}), Duration::from_secs(1), &c)
                .await,
            Err(Error::Protocol(_))
        ));
        let cancel = Cancellation::default();
        let trigger = cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(10)).await;
            trigger.cancel()
        });
        assert!(matches!(
            client
                .request("hold", json!({}), Duration::from_secs(5), &cancel)
                .await,
            Err(Error::Cancelled)
        ));
        // Cancelling/dropping one waiter must not poison the multiplexed connection.
        assert!(
            client
                .request("okay", json!({}), Duration::from_secs(1), &c)
                .await
                .is_ok()
        );
        server.abort();
    }
    #[test]
    fn logical_binding_ignores_old_virtualized_remounts_and_rejects_ambiguity() {
        let baseline = vec![
            "group:user:old".into(),
            "group:assistant:old".into(),
            "logical-id".into(),
        ];
        assert_eq!(
            new_turn_identity(&baseline, ["group:assistant:old", "logical-id"]).unwrap(),
            None
        );
        assert_eq!(
            new_turn_identity(&baseline, ["logical-id", "new"]).unwrap(),
            Some("new".into())
        );
        assert!(new_turn_identity(&baseline, ["new", "other"]).is_err());
    }
    fn input(mode: InteractionMode) -> TurnInput {
        TurnInput {
            connector_name: None,
            full_prompt: "hello".into(),
            turn_token: None,
            model: "chatgpt-web/gpt-6-sol".into(),
            effort: "high".into(),
            attachments: vec![],
            interaction_mode: mode,
            ownership: Ownership {
                task_id: "task".into(),
                epoch: 1,
            },
        }
    }
    #[tokio::test]
    async fn automatic_output_and_duplicate_accepted_send() {
        let (url, server) = server().await;
        let engine = BrowserEngine::connect(&url).await.unwrap();
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let result = engine
            .submit(
                input(InteractionMode::Automatic),
                tx.clone(),
                Cancellation::default(),
            )
            .await
            .unwrap();
        assert!(matches!(result,TurnResult::Completed{text,..}if text=="**answer**"));
        assert!(matches!(rx.recv().await, Some(Progress::Accepted { .. })));
        assert!(matches!(
            engine
                .submit(
                    input(InteractionMode::Automatic),
                    tx,
                    Cancellation::default()
                )
                .await,
            Err(Error::DuplicateSend)
        ));
        server.abort();
    }
    #[tokio::test]
    async fn manual_never_touches_dom() {
        // Any request would time out: server does not acknowledge Runtime.evaluate here.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!(
            "ws://127.0.0.1:{}/devtools/page/manual",
            listener.local_addr().unwrap().port()
        );
        let task = tokio::spawn(async move {
            let (s, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(s).await.unwrap();
            assert!(
                tokio::time::timeout(Duration::from_millis(150), ws.next())
                    .await
                    .is_err()
            );
        });
        let engine = BrowserEngine::connect(&url).await.unwrap();
        let (tx, _) = tokio::sync::mpsc::unbounded_channel();
        assert!(matches!(
            engine
                .submit(input(InteractionMode::Manual), tx, Cancellation::default())
                .await
                .unwrap(),
            TurnResult::ManualHandoff { .. }
        ));
        task.await.unwrap();
    }
    #[tokio::test]
    async fn refuses_discovery_and_nonloopback() {
        for url in [
            "ws://127.0.0.1:1234/devtools/browser/id",
            "ws://example.com:1234/devtools/page/id",
            "http://127.0.0.1:1234/devtools/page/id",
            "ws://localhost:1234/devtools/page/id",
        ] {
            assert!(matches!(
                CdpClient::connect(url).await,
                Err(Error::Endpoint(_))
            ))
        }
    }
    #[test]
    fn broker_token_binding_is_unique_and_engine_is_arc_friendly() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<BrowserEngine>();
        let mut input = input(InteractionMode::Automatic);
        let token = "turn_0123456789abcdef0123456789abcdef".to_string();
        input.turn_token = Some(token.clone());
        let mut user = UserTurn {
            id: "user".into(),
            text: None,
            tokens: vec![token.clone()],
            pending: false,
        };
        assert!(accepted_matches(&user, &input));
        user.tokens.push(token);
        assert!(!accepted_matches(&user, &input));
        user.tokens = vec!["turn_other".into()];
        assert!(!accepted_matches(&user, &input));
        input.turn_token = None;
        user.text = Some("hello".into());
        assert!(accepted_matches(&user, &input));
    }
    #[test]
    fn supplied_token_is_literal_unique_and_not_length_limited() {
        for token in [
            format!("turn_{}", "0123456789abcdef".repeat(4)),
            "marker:[random.*+?]".into(),
            "任意-marker-λ".into(),
        ] {
            assert!(unique_literal_token(
                &format!("prefix {token} suffix"),
                &token
            ));
            assert!(!unique_literal_token(&format!("{token} {token}"), &token));
            assert!(!unique_literal_token("missing", &token));
        }
        assert!(!unique_literal_token("aaaa", "aaa"));
        assert!(!unique_literal_token("anything", ""));
    }
}
