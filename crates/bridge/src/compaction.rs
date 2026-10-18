//! Source compact-v1 replacement history and isolated retained checkpoint control.
use super::*;
use sha2::{Digest, Sha256};
use tokio::sync::Notify;
const CONTROL: &str = "codex.control.compaction_handoff";
const LATEST: &str = "CODEX_LATEST_USER_PROMPT_JSON";
#[derive(Default)]
pub(super) struct Registry {
    runs: HashMap<String, Arc<Operation>>,
}
pub(super) struct Operation {
    id: String,
    owner: TurnOwner,
    source: Option<String>,
    token: String,
    handoff: String,
    mode: ExecutionMode,
    latest: String,
    created: Instant,
    deadline: Instant,
    ready: AtomicBool,
    started: AtomicBool,
    active: AtomicBool,
    source_finished: watch::Sender<bool>,
    accepted: watch::Sender<bool>,
    cancel: watch::Sender<Option<String>>,
    summary: Mutex<Option<String>>,
    result: watch::Sender<Option<Result<String, String>>>,
    changed: Notify,
}
impl Registry {
    pub(super) fn clear_idle(&mut self) {
        self.runs.retain(|_, op| op.active.load(Ordering::SeqCst));
    }
    pub(super) fn active_count(&self) -> usize {
        self.runs
            .values()
            .filter(|op| op.active.load(Ordering::SeqCst))
            .count()
    }
    pub(super) fn fresh_active_count(&self) -> usize {
        self.runs
            .values()
            .filter(|op| op.source.is_none() && op.active.load(Ordering::SeqCst))
            .count()
    }
    fn find(&self, id: &str) -> Option<Arc<Operation>> {
        self.runs
            .values()
            .find(|op| op.id == id || op.token == id)
            .cloned()
    }
}
impl BridgeHandle {
    /// Called only AFTER the source helper physically settles. This is not a
    /// browser send receipt and never grants a work-tool environment.
    pub fn confirm_compaction_source_settled(
        &self,
        id: &str,
        owner: &TurnOwner,
    ) -> Result<(), String> {
        let op = self
            .inner
            .compactions
            .lock()
            .unwrap()
            .find(id)
            .ok_or("compaction request is not active")?;
        if &op.owner != owner
            || !op.active.load(Ordering::SeqCst)
            || op.cancel.borrow().is_some()
            || Instant::now() >= op.deadline
        {
            return Err("compaction owner mismatch, expired or cancelled request".into());
        }
        if !*op.source_finished.borrow() {
            return Err("source response has not semantically settled".into());
        }
        op.ready.store(true, Ordering::SeqCst);
        op.changed.notify_one();
        Ok(())
    }
}
pub(super) fn cancel(inner: &Inner, id: &str) -> bool {
    let op = inner
        .compactions
        .lock()
        .unwrap()
        .runs
        .values()
        .find(|op| {
            (op.id == id || op.source.as_deref() == Some(id)) && op.active.load(Ordering::SeqCst)
        })
        .cloned();
    if let Some(op) = op {
        op.cancel.send_replace(Some("client_cancelled".into()));
        op.changed.notify_one();
        true
    } else {
        false
    }
}
pub(super) async fn wait_closed(inner: &Inner) {
    let operations = inner
        .compactions
        .lock()
        .unwrap()
        .runs
        .values()
        .cloned()
        .collect::<Vec<_>>();
    for op in operations {
        let mut result = op.result.subscribe();
        while op.active.load(Ordering::SeqCst) {
            if result.changed().await.is_err() {
                break;
            }
        }
    }
}

fn contents(item: &Value) -> Vec<Value> {
    match &item["content"] {
        Value::String(s) => vec![json!({"type":"input_text","text":s})],
        Value::Array(a) => a.iter().filter(|v| v.is_object()).cloned().collect(),
        _ => vec![],
    }
}
fn text_block(block: &Value) -> bool {
    matches!(block["type"].as_str(), Some("text" | "input_text")) && block["text"].is_string()
}
fn agents_refresh(item: &Value) -> bool {
    let meta = &item["internal_chat_message_metadata_passthrough"];
    item["type"] == "message"
        && item["role"] == "user"
        && item["id"].as_str().is_some_and(|s| !s.is_empty())
        && meta["turn_id"]
            .as_str()
            .is_some_and(|s| !s.trim().is_empty())
        && meta["content_item_kinds"]
            .as_array()
            .is_some_and(|a| !a.is_empty() && a.iter().all(|v| v == "agents_md.instructions"))
}
fn internal_fragment(text: &str) -> bool {
    let text = text.trim();
    if text.starts_with("<goal_context>") && text.ends_with("</goal_context>") {
        return true;
    }
    let Some(tail) = text.strip_prefix("<codex_internal_context source=\"") else {
        return false;
    };
    let Some((source, _)) = tail.split_once("\">") else {
        return false;
    };
    text.ends_with("</codex_internal_context>")
        && !source.is_empty()
        && source.as_bytes()[0].is_ascii_lowercase()
        && source
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}
fn user_messages(input: &Value) -> Vec<Value> {
    input
        .as_array()
        .into_iter()
        .flatten()
        .filter(|item| {
            if item["role"] != "user"
                || item.get("type").is_some_and(|t| t != "message")
                || agents_refresh(item)
            {
                return false;
            }
            let blocks = contents(item);
            !blocks
                .iter()
                .any(|b| text_block(b) && internal_fragment(b["text"].as_str().unwrap()))
                && !blocks
                    .iter()
                    .filter(|b| text_block(b))
                    .filter_map(|b| b["text"].as_str())
                    .collect::<String>()
                    .starts_with(&format!("{}\n", crate::protocol::SUMMARY_PREFIX))
        })
        .cloned()
        .collect()
}
fn image_block(block: &Value) -> bool {
    if block["type"] != "input_image" || !block["image_url"].is_string() {
        return false;
    }
    !crate::protocol::one_pixel_image(block)
}
/// Source compact-v1 retains 80,000 UTF-16 code units of human text and the newest
/// ten non-sentinel images, preserving original item/block metadata.
pub fn build_compact_v1_output(input: &Value, summary: &str) -> Vec<Value> {
    let users = user_messages(input);
    let mut selected = vec![];
    let mut remaining = 80_000usize;
    let mut images = 0;
    for item in users.iter().rev() {
        if remaining == 0 && images >= 10 {
            break;
        }
        let mut blocks = vec![];
        for mut block in contents(item).into_iter().rev() {
            if image_block(&block) {
                if images < 10 {
                    images += 1;
                    blocks.push(block);
                }
                continue;
            }
            if !text_block(&block) || remaining == 0 {
                continue;
            }
            let text = block["text"].as_str().unwrap();
            let encoded: Vec<_> = text.encode_utf16().collect();
            let take = encoded.len().min(remaining);
            let retained = String::from_utf16_lossy(&encoded[encoded.len() - take..]);
            remaining -= take;
            block["type"] = json!("input_text");
            block["text"] = json!(retained);
            blocks.push(block);
        }
        blocks.reverse();
        if !blocks.is_empty() {
            let mut message = item.clone();
            message["type"] = json!("message");
            message["role"] = json!("user");
            message["content"] = json!(blocks);
            selected.push(message);
        }
    }
    selected.reverse();
    selected.push(json!({"type":"message","role":"user","content":[{"type":"input_text","text":if summary.trim().is_empty(){"(no summary available)".into()}else{format!("{}\n{summary}",crate::protocol::SUMMARY_PREFIX)}}]}));
    selected
}
pub(super) fn latest_prompt(raw: &Value, fallback: Option<&Value>) -> Result<String, String> {
    for body in [Some(raw), fallback].into_iter().flatten() {
        if let Some(user) = user_messages(&body["input"]).iter().rev().find(|item| {
            let text = contents(item)
                .iter()
                .filter(|b| text_block(b))
                .filter_map(|b| b["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n");
            !text.trim_start().starts_with("<environment_context")
        }) {
            let text = contents(user)
                .iter()
                .filter(|b| text_block(b))
                .filter_map(|b| b["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n");
            if !text.is_empty() {
                return Ok(text);
            }
        }
        if let Some(text) = body["input"].as_str() {
            if !text.is_empty() {
                return Ok(text.into());
            }
        }
    }
    Err("compaction source has no canonical latest user prompt".into())
}
fn canonical_summary(op: &Operation, summary: &str) -> Result<String, String> {
    let summary = summary.trim();
    if summary.is_empty() {
        return Err("compaction handoff summary is empty".into());
    }
    let appendix = format!("{LATEST}\n{}", json!(op.latest));
    if let Some(offset) = summary.rfind(&format!("\n{LATEST}\n")) {
        if summary[offset + 1..].trim_end() != appendix {
            return Err("compaction handoff contains a conflicting latest-user marker".into());
        }
        return Ok(summary.into());
    }
    Ok(format!("{summary}\n\n{appendix}"))
}
fn checkpoint_prompt(op: &Operation, task: &CompiledTask) -> String {
    let instruction = format!(
        "Automatic Codex context compaction has started. Stop ordinary task work and do not call any more work tools.\n{}\nSubmit the summary to the pending Codex task through the attached Codex Native plugin using codex_tool_call with the binding below.\nThe reserved codex.control.compaction_handoff operation stores this summary for task continuation. It does not run commands, read or edit files, or invoke other tools, and it is not listed by tool inventory.\nThis one-shot control token is valid only for the reserved compaction operation; do not use it with codex_exec, codex_tool_inventory, or any outer Codex tool.\n<codex_compaction_control>\nturn_token {}\nwire_name {CONTROL}\nhandoff_id {}\n</codex_compaction_control>\nCall codex_tool_call exactly once with {}.\nAfter the control call returns submitted=true, call no more tools. The bridge will close this one-purpose Web response after accepting the checkpoint.\nIf the call is rejected or fails, stop and report its actual error. Do not retry through another tool or claim the summary was submitted without submitted=true.",
        compact_prompt(),
        op.token,
        op.handoff,
        json!({"turn_token":op.token,"wire_name":CONTROL,"arguments":{"handoff_id":op.handoff,"summary":"<complete checkpoint summary>"}})
    );
    if op.mode == ExecutionMode::Manual {
        format!(
            "{}\n<codex_zero_risk_request_json>\n{}\n</codex_zero_risk_request_json>\nCall codex_turn_start for this request_id, then submit only the complete checkpoint with codex_turn_complete. Do not call work tools.",
            task.prompt,
            json!({"request_id":op.token})
        )
    } else if op.source.is_some() {
        instruction
    } else {
        format!("{}\n{instruction}", task.prompt)
    }
}
fn compact_prompt() -> String {
    serde_json::from_str::<Value>(include_str!("fixtures/prompt-contract.json")).unwrap()["compact_prompt"].as_str().unwrap().into()
}
pub(super) fn interruption(mode: ExecutionMode, executed: bool) -> Value {
    let text = if mode == ExecutionMode::Manual {
        format!(
            "<CODEX_ACTIVE_COMPACTION_REQUEST>\n{}\n{}\n{}\nCall no more work tools. Return only the complete checkpoint summary to Codex with codex_turn_complete.\n</CODEX_ACTIVE_COMPACTION_REQUEST>",
            if executed {
                "Codex reached its context limit while this Web response was waiting for the tool result above."
            } else {
                "Codex reached its context limit before the requested tool could be sent for execution. The tool was not executed."
            },
            if executed {
                "Consume that canonical result, stop ordinary task work now, and do not call any more work tools."
            } else {
                "Stop ordinary task work now and do not call any more work tools."
            },
            compact_prompt()
        )
    } else {
        "<CODEX_ACTIVE_COMPACTION_REQUEST>\nCodex reached its context limit before this newly requested tool could be sent for execution. The tool was not executed.\nStop ordinary task work now, call no more tools, and end this Web response normally.\nDo not create or submit a checkpoint in this response. After it settles, the retained conversation will receive exactly one separate structured compaction handoff request.\n</CODEX_ACTIVE_COMPACTION_REQUEST>".into()
    };
    let mut result = json!({"content":[{"type":"text","text":text}]});
    if !executed {
        result["isError"] = json!(true);
    }
    result
}
fn find_source(inner: &Inner, raw: &Value) -> Option<String> {
    let metadata = turn_metadata(raw);
    let thread = metadata["thread_id"].as_str()?;
    let turn = metadata["turn_id"].as_str()?;
    inner
        .turns
        .lock()
        .unwrap()
        .iter()
        .find(|(_, t)| {
            t.owner.thread_id == thread
                && t.owner.turn_id == turn
                && t.task.compaction == CompactionMode::None
        })
        .map(|(id, _)| id.clone())
}
fn checkpoint_response(raw: &Value, id: &str, summary: &str) -> Value {
    let remote = raw["input"].as_array().is_some_and(|items| {
        items
            .iter()
            .any(|item| item["type"] == "compaction_trigger")
    });
    let output = if turn_metadata(raw)["request_kind"] == "compaction" && !remote {
        json!([{"type":"message","id":format!("msg_{}",Uuid::new_v4().simple()),"role":"assistant","status":"completed","content":[{"type":"output_text","text":summary,"annotations":[]}]}])
    } else {
        use base64::Engine;
        json!([{"type":"compaction","id":format!("cmp_{}",Uuid::new_v4().simple()),"encrypted_content":format!("ocx1:{}",base64::engine::general_purpose::STANDARD.encode(summary))}])
    };
    json!({"id":id,"object":"response","created_at":chrono::Utc::now().timestamp(),"model":raw["model"],"status":"completed","output":output,"end_turn":true})
}
pub(super) async fn resume_checkpoint(inner: &Arc<Inner>, raw: &Value) -> Option<Response> {
    let requested = turn_metadata(raw)["request_kind"] == "compaction"
        || raw["input"].as_array().is_some_and(|items| {
            items
                .iter()
                .any(|item| item["type"] == "compaction_trigger")
        });
    if !requested {
        return None;
    }
    let source = find_source(inner, raw);
    let key = format!("{:x}", Sha256::digest(serde_json::to_vec(raw).ok()?));
    let cached = inner
        .compactions
        .lock()
        .unwrap()
        .runs
        .get(&key)
        .is_some_and(|op| {
            op.active.load(Ordering::SeqCst) || op.created.elapsed() < Duration::from_secs(1800)
        });
    if source.is_none() && !cached {
        return None;
    }
    let op = match reserve(inner, raw.clone(), source) {
        Ok(op) => op,
        Err(e) => return Some(error(StatusCode::CONFLICT, "compaction_handoff_failed", e)),
    };
    let id = format!("resp_{}", Uuid::new_v4().simple());
    if raw["stream"] != true {
        return Some(match wait_result(op).await {
            Ok(summary) => Json(checkpoint_response(raw, &id, &summary)).into_response(),
            Err(e) => error(StatusCode::CONFLICT, "compaction_handoff_failed", e),
        });
    }
    let (tx, rx) = mpsc::channel(inner.config.channel_capacity);
    let raw = raw.clone();
    tokio::spawn(async move {
        let initial = json!({"id":id,"object":"response","created_at":chrono::Utc::now().timestamp(),"model":raw["model"],"status":"in_progress","output":[]});
        if tx
            .send(json!({"type":"response.created","sequence_number":0,"response":initial}))
            .await
            .is_err()
        {
            return;
        }
        let result = tokio::select! {_=tx.closed()=>return,result=wait_result(op)=>result};
        match result {
            Ok(summary) => {
                let response = checkpoint_response(&raw, &id, &summary);
                let _=tx.send(json!({"type":"response.output_item.done","sequence_number":1,"output_index":0,"item":response["output"][0]})).await;
                let _=tx.send(json!({"type":"response.completed","sequence_number":2,"response":response})).await;
            }
            Err(message) => {
                let mut failed = initial;
                failed["status"] = json!("failed");
                failed["error"] = json!({"code":"compaction_handoff_failed","message":message});
                let _ = tx
                    .send(json!({"type":"response.failed","sequence_number":1,"response":failed}))
                    .await;
            }
        }
    });
    Some(response_body(rx, true).await)
}

pub(super) async fn compact_v1(inner: Arc<Inner>, raw: Value) -> Response {
    let source = find_source(&inner, &raw);
    match begin(&inner, raw.clone(), source).await {
        Ok(summary) => {
            Json(json!({"output":build_compact_v1_output(&raw["input"],&summary)})).into_response()
        }
        Err(e) => error(StatusCode::CONFLICT, "compaction_handoff_failed", e),
    }
}
struct Source {
    raw: Value,
    receipts: Vec<Value>,
    events: Arc<tokio::sync::Mutex<mpsc::Receiver<BrowserEvent>>>,
    commands: Arc<tokio::sync::Mutex<mpsc::Receiver<session::RoundCommand>>>,
}
async fn begin(
    inner: &Arc<Inner>,
    raw: Value,
    source_id: Option<String>,
) -> Result<String, String> {
    wait_result(reserve(inner, raw, source_id)?).await
}
fn reserve(
    inner: &Arc<Inner>,
    raw: Value,
    source_id: Option<String>,
) -> Result<Arc<Operation>, String> {
    let key = format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(&raw).map_err(|e| e.to_string())?)
    );
    let existing = inner.compactions.lock().unwrap().runs.get(&key).cloned();
    let op = if let Some(existing) = existing {
        existing
    } else {
        let mut task_raw = raw.clone();
        let mut input = task_raw["input"].as_array().cloned().unwrap_or_default();
        input.push(json!({"type":"compaction_trigger"}));
        task_raw["input"] = json!(input);
        let mut task = compile_request(task_raw)?;
        task.tools.clear();
        let metadata = turn_metadata(&raw);
        let id = format!("checkpoint_{}", Uuid::new_v4().simple());
        let owner = TurnOwner {
            thread_id: metadata["thread_id"].as_str().unwrap_or(&id).into(),
            turn_id: metadata["turn_id"].as_str().unwrap_or(&id).into(),
        };
        let mut turns = inner.turns.lock().unwrap();
        let mode = inner.execution_mode();
        let mut registry = inner.compactions.lock().unwrap();
        // Recheck under the same admission fence so exact concurrent retries share one operation.
        if let Some(existing) = registry.runs.get(&key).cloned() {
            drop(registry);
            drop(turns);
            return Ok(existing);
        }
        if inner.admission_paused() {
            return Err("bridge admission is paused".into());
        }
        registry.runs.retain(|_, op| {
            op.active.load(Ordering::SeqCst) || op.created.elapsed() < Duration::from_secs(1800)
        });
        if registry
            .runs
            .values()
            .any(|op| op.owner == owner && op.active.load(Ordering::SeqCst))
        {
            return Err("another checkpoint owns this native turn".into());
        }
        if source_id.is_none() && turns.values().any(|turn| turn.owner == owner) {
            return Err(
                "native owner already has an active non-retainable browser operation".into(),
            );
        }
        if source_id.is_none() && turns.len() + registry.fresh_active_count() >= 5 {
            return Err("at most five browser owners including checkpoints".into());
        }
        let source = source_id
            .as_ref()
            .map(|id| turns.get(id).ok_or("source retired before compaction"))
            .transpose()?;
        let latest = latest_prompt(&raw, source.map(|t| &t.task.raw))?;
        let mut results = HashMap::new();
        if let Some(source) = source {
            validate_turn(source, None)?;
            if source.owner != owner || source.task.model != task.model {
                return Err("compaction owner/model does not match retained source".into());
            }
            if source.observer_active {
                return Err("source observer is active; retry at the native tool boundary".into());
            }
            for item in raw["input"].as_array().into_iter().flatten() {
                if let Some(id) = item["call_id"].as_str() {
                    if let Some(pending) = source.pending.get(id).filter(|p| p.delivered) {
                        let expected = if pending.item["type"] == "custom_tool_call" {
                            "custom_tool_call_output"
                        } else if pending.item["type"] == "tool_search_call" {
                            "tool_search_output"
                        } else {
                            "function_call_output"
                        };
                        // Historical call items may share IDs; only outputs can resolve invocations.
                        if item["type"] == expected {
                            if results
                                .insert(id.to_owned(), session::broker_result(item)?)
                                .is_some()
                            {
                                return Err("duplicate tool result in compaction input".into());
                            }
                        } else if item["type"]
                            .as_str()
                            .is_some_and(|s| s.ends_with("_output"))
                        {
                            return Err("mismatched compaction tool-output type".into());
                        }
                    }
                }
            }
            let outstanding = source.pending.values().filter(|p| p.delivered).count();
            if results.len() != outstanding {
                return Err(format!(
                    "Codex supplied {} of {outstanding} required tool results for compaction",
                    results.len()
                ));
            }
        }
        let permit = inner
            .browser
            .try_reserve()
            .map_err(|_| "browser checkpoint channel unavailable or full")?;
        let (source_finished, _) = watch::channel(source_id.is_none());
        let (accepted, _) = watch::channel(false);
        let (cancel, _) = watch::channel(None);
        let (result, _) = watch::channel(None);
        let op = Arc::new(Operation {
            id: id.clone(),
            owner: owner.clone(),
            source: source_id.clone(),
            token: format!(
                "{}_{}",
                if mode == ExecutionMode::Manual {
                    "request"
                } else {
                    "control"
                },
                Uuid::new_v4().simple()
            ),
            handoff: format!("handoff_{}", Uuid::new_v4().simple()),
            mode,
            latest,
            created: Instant::now(),
            deadline: Instant::now() + inner.config.turn_ttl.min(Duration::from_secs(300)),
            ready: AtomicBool::new(false),
            started: AtomicBool::new(mode == ExecutionMode::Automatic),
            active: AtomicBool::new(true),
            source_finished,
            accepted,
            cancel,
            summary: Mutex::new(None),
            result,
            changed: Notify::new(),
        });
        let source = if let Some(root) = &source_id {
            let source = turns.get_mut(root).unwrap();
            source.compaction_request = Some(id.clone());
            source.observer_active = true;
            let mut ordered = source
                .pending
                .iter()
                .filter(|(_, p)| p.delivered)
                .map(|(id, p)| (p.order, id.clone()))
                .collect::<Vec<_>>();
            ordered.sort_by_key(|p| p.0);
            for (index, (_, call_id)) in ordered.iter().enumerate() {
                let pending = source.pending.remove(call_id).unwrap();
                let mut result = results.remove(call_id).unwrap();
                if mode == ExecutionMode::Manual && index == ordered.len() - 1 {
                    result["content"]
                        .as_array_mut()
                        .ok_or("canonical tool result content is not an array")?
                        .push(interruption(mode, true)["content"][0].clone());
                }
                let _ = pending.reply.send(Ok(result));
            }
            source.compaction_interceptions += source.pending.len();
            for (_, pending) in source.pending.drain() {
                let _ = pending.reply.send(Ok(interruption(mode, false)));
            }
            let receipts = raw["input"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|item| {
                    item["call_id"]
                        .as_str()
                        .is_some_and(|id| ordered.iter().any(|(_, call_id)| call_id == id))
                        && item["type"]
                            .as_str()
                            .is_some_and(|s| s.ends_with("_output"))
                })
                .cloned()
                .collect();
            Some(Source {
                raw: raw.clone(),
                receipts,
                events: source.event_rx.clone(),
                commands: source.command_rx.clone(),
            })
        } else {
            None
        };
        let (completion, completed) = oneshot::channel();
        let prompt = checkpoint_prompt(&op, &task);
        permit.send(BrowserRequest::Compaction(CompactionRequest {
            request_id: id,
            source_request_id: source_id.clone(),
            owner,
            model: task.model.clone(),
            effort: task.effort.clone(),
            task,
            prompt,
            control_token: op.token.clone(),
            handoff_id: op.handoff.clone(),
            mode,
            reuse_manual_response: mode == ExecutionMode::Manual && source_id.is_some(),
            source_finished: op.source_finished.subscribe(),
            accepted: op.accepted.subscribe(),
            cancellation: op.cancel.subscribe(),
            completion,
        }));
        while registry.runs.len() >= 64 {
            if let Some(oldest) = registry
                .runs
                .iter()
                .filter(|(_, op)| !op.active.load(Ordering::SeqCst))
                .min_by_key(|(_, op)| op.created)
                .map(|(k, _)| k.clone())
            {
                registry.runs.remove(&oldest);
            } else {
                break;
            }
        }
        registry.runs.insert(key, op.clone());
        tokio::spawn(run(inner.clone(), op.clone(), source, completed));
        op
    };
    Ok(op)
}
async fn wait_result(op: Arc<Operation>) -> Result<String, String> {
    let mut result = op.result.subscribe();
    loop {
        if let Some(result) = result.borrow().clone() {
            return result;
        }
        result
            .changed()
            .await
            .map_err(|_| "checkpoint result channel closed")?;
    }
}
async fn source_settlement(source: Source, mode: ExecutionMode) -> Result<String, String> {
    let mut events = source.events.lock().await;
    let mut commands = source.commands.lock().await;
    let mut answer = String::new();
    loop {
        tokio::select! {
            event=events.recv()=>match event.ok_or("source browser disconnected during compaction")?{
                BrowserEvent::TextDelta(text)=>answer.push_str(&text),
                BrowserEvent::Completed{output,..}=>{if answer.is_empty(){answer=output.iter().flat_map(|i|i["content"].as_array().into_iter().flatten()).filter_map(|p|p["text"].as_str()).collect::<String>();}return Ok(answer);},
                BrowserEvent::Failed{message,..}=>return Err(message),BrowserEvent::Cancelled=>return Err("source browser cancelled".into()),BrowserEvent::Incomplete{reason,..}=>return Err(format!("source incomplete: {reason}")),BrowserEvent::OutputItem(_)=>{},
            },
            command=commands.recv()=>match command{
                Some(session::RoundCommand::ManualComplete(answer))if mode==ExecutionMode::Manual=>return Ok(answer),
                Some(session::RoundCommand::Tools(_))=>{},Some(_)=>return Err("unexpected source checkpoint command".into()),None=>return Err("source command channel closed".into()),
            }
        }
    }
}
async fn work(
    inner: &Inner,
    op: &Operation,
    source: Option<Source>,
    completion: &mut oneshot::Receiver<Result<(), String>>,
    completion_done: &mut bool,
    physical: &mut bool,
) -> Result<String, String> {
    if let Some(source) = source {
        let source_raw = source.raw.clone();
        let receipts = source.receipts.clone();
        let answer = source_settlement(source, op.mode).await?;
        if op.mode == ExecutionMode::Automatic {
            if let Some(root) = &op.source {
                let intercepted = inner
                    .turns
                    .lock()
                    .unwrap()
                    .get(root)
                    .is_none_or(|t| t.compaction_interceptions != 0);
                if !intercepted {
                    session::preserve_compaction_final(inner, root, &source_raw, receipts, &answer);
                }
            }
        }
        if let Some(root) = &op.source {
            if let Some(turn) = inner.turns.lock().unwrap().get_mut(root) {
                turn.work_revoked = true;
            }
        }
        op.source_finished.send_replace(true);
        if op.mode == ExecutionMode::Manual {
            let summary = canonical_summary(op, &answer)?;
            *op.summary.lock().unwrap() = Some(summary);
            op.accepted.send_replace(true);
        }
    }
    loop {
        if *op.accepted.borrow() {
            break;
        }
        tokio::select! {
            _=op.changed.notified()=>{},
            ended=&mut *completion=>{
                *completion_done=true;*physical=ended.is_ok();
                if *op.accepted.borrow() && matches!(ended,Ok(Ok(()))) {return op.summary.lock().unwrap().clone().ok_or("checkpoint receipt has no summary".into());}
                return Err(match ended{Ok(Err(error))=>error,_=>"browser settled without submitting the checkpoint".into()});
            }
        }
    }
    let summary = op
        .summary
        .lock()
        .unwrap()
        .clone()
        .ok_or("checkpoint receipt has no summary")?;
    let ended = completion.await;
    *completion_done = true;
    *physical = ended.is_ok();
    ended.map_err(|_| "browser dropped its physical-settlement acknowledgment")??;
    Ok(summary)
}
async fn run(
    inner: Arc<Inner>,
    op: Arc<Operation>,
    source: Option<Source>,
    mut completion: oneshot::Receiver<Result<(), String>>,
) {
    let mut stopped = inner.shutdown.subscribe();
    let mut cancelled = op.cancel.subscribe();
    let mut completion_done = false;
    let mut physical = false;
    let outcome = tokio::select! {biased;
        _=stopped.wait_for(|s|*s)=>Err("bridge closed".into()),
        reason=cancelled.wait_for(|s|s.is_some())=>Err(reason.ok().and_then(|reason|reason.clone()).unwrap_or_else(||"compaction cancelled".into())),
        _=tokio::time::sleep_until(op.deadline.into())=>Err("compaction handoff timed out".into()),
        outcome=work(&inner,&op,source,&mut completion,&mut completion_done,&mut physical)=>outcome,
    };
    if let Err(error) = &outcome {
        op.cancel.send_replace(Some(error.clone()));
        op.result.send_replace(Some(Err(error.clone())));
        // Failure is logical, not proof of physical release. Keep the owner and
        // admission fence until GPUI acknowledges cleanup (or this bridge closes).
        if !physical {
            if completion_done {
                let _ = stopped.wait_for(|s| *s).await;
            } else {
                tokio::select! {_=stopped.wait_for(|s|*s)=>{},_= &mut completion=>{}}
            }
        }
    }
    if let Some(root) = &op.source {
        let mut turns = inner.turns.lock().unwrap();
        if turns
            .get(root)
            .is_some_and(|t| t.compaction_request.as_deref() == Some(&op.id))
        {
            let turn = turns.remove(root).unwrap();
            turn.cancel.send_replace(Some(
                if outcome.is_ok() {
                    "compaction_checkpoint_accepted"
                } else {
                    "compaction_failed"
                }
                .into(),
            ));
        }
    }
    op.active.store(false, Ordering::SeqCst);
    op.cancel.send_replace(Some("compaction_retired".into()));
    if outcome.is_err() {
        inner
            .compactions
            .lock()
            .unwrap()
            .runs
            .retain(|_, candidate| !Arc::ptr_eq(candidate, &op));
    }
    op.result.send_replace(Some(outcome));
}
pub(super) fn control_call(
    inner: &Inner,
    name: &str,
    args: &Value,
) -> Option<Result<Value, String>> {
    let token = args["turn_token"]
        .as_str()
        .or_else(|| args["request_id"].as_str());
    let op = token.and_then(|token| inner.compactions.lock().unwrap().find(token));
    let reserved = name == "codex_tool_call" && args["wire_name"] == CONTROL;
    let Some(op) = op else {
        return reserved
            .then(|| Err("compaction control token is invalid, expired, or consumed".into()));
    };
    Some((|| {
        if !op.active.load(Ordering::SeqCst)
            || op.cancel.borrow().is_some()
            || Instant::now() >= op.deadline
        {
            return Err("compaction control token is invalid, expired, or consumed".into());
        }
        if args
            .get("thread_id")
            .is_some_and(|v| v != &json!(op.owner.thread_id))
            || args
                .get("turn_id")
                .is_some_and(|v| v != &json!(op.owner.turn_id))
        {
            return Err("compaction owner mismatch".into());
        }
        if op.mode == ExecutionMode::Manual && op.source.is_some() {
            return Err(
                "active Manual checkpoint must complete through its original request_id".into(),
            );
        }
        if !op.ready.load(Ordering::SeqCst) {
            return Err("physical source settlement has not been acknowledged".into());
        }
        if op.mode == ExecutionMode::Manual && name == "codex_turn_start" {
            return Ok(tool_result(
                json!({"started":true,"duplicate":op.started.swap(true,Ordering::SeqCst)}),
            ));
        }
        let summary = if op.mode == ExecutionMode::Manual && name == "codex_turn_complete" {
            if !op.started.load(Ordering::SeqCst) {
                return Err("Manual checkpoint requires codex_turn_start".into());
            }
            args["final_answer"]
                .as_str()
                .ok_or("final_answer required")?
        } else if op.mode == ExecutionMode::Automatic && reserved {
            if args.get("input").is_some() {
                return Err("Compaction control handoff does not accept freeform input".into());
            }
            if args["arguments"]["handoff_id"] != op.handoff {
                return Err("compaction handoff id does not match the pending transaction".into());
            }
            args["arguments"]["summary"]
                .as_str()
                .ok_or("compaction handoff requires summary")?
        } else {
            return Err("checkpoint capability cannot access work tools or inventory".into());
        };
        let summary = canonical_summary(&op, summary)?;
        let mut accepted = op.summary.lock().unwrap();
        if accepted.is_some() {
            return Err("compaction control token is already consumed".into());
        }
        *accepted = Some(summary);
        op.accepted.send_replace(true);
        op.changed.notify_one();
        Ok(tool_result(if op.mode == ExecutionMode::Manual {
            json!({"completed":true,"duplicate":false})
        } else {
            json!({"submitted":true})
        }))
    })())
}
