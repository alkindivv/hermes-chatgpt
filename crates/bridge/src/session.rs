//! Outer Codex owns tool execution. This module only correlates Responses rounds.
use super::*;

pub(super) struct PendingCall {
    pub(super) item: Value,
    pub(super) delivered: bool,
    pub(super) order: u64,
    pub(super) reply: oneshot::Sender<Result<Value, String>>,
}
pub(super) struct History {
    root_id: String,
    owner: TurnOwner,
    items: Vec<Value>,
    at: Instant,
    awaiting_tools: bool,
    actual_final: Option<(Value, Vec<Value>, String)>,
}
pub(super) enum RoundCommand {
    Tools(Vec<Value>),
    ManualComplete(String),
}

impl BridgeHandle {
    /// GPUI handoff to the outer Codex Responses/continuation path. No local execution.
    pub async fn forward_native_tool(&self, call: NativeToolCall) -> Result<(), String> {
        self.handoff_tool(call).await
    }

    /// Return an MCP invocation to the outer Codex Responses stream. Never execute
    /// it in GPUI. Its oneshot resolves when Codex sends the correlated tool output.
    pub async fn handoff_tool(&self, call: NativeToolCall) -> Result<(), String> {
        self.handoff_tools(vec![call]).await
    }
    /// Emit one parallel batch. Continuation must return every call in this batch.
    pub async fn handoff_tools(&self, calls: Vec<NativeToolCall>) -> Result<(), String> {
        if calls.is_empty() {
            return Err("empty tool batch".into());
        }
        let root = calls[0].request_id.clone();
        let mut turns = self.inner.turns.lock().unwrap();
        let turn = turns
            .get_mut(&root)
            .ok_or("tool owner is no longer active")?;
        validate_turn(turn, None)?;
        if turn.compaction_request.is_some() {
            turn.compaction_interceptions += calls.len();
            for call in calls {
                let _ = call.result.send(Ok(compaction::interruption(
                    self.inner.execution_mode(),
                    false,
                )));
            }
            return Ok(());
        }
        if turn.task.compaction != CompactionMode::None {
            return Err("work tools are forbidden during compaction".into());
        }
        let visible = mcp_policy::visible(&turn.tools, self.inner.execution_mode());
        let mut ids = std::collections::HashSet::new();
        let mut items = vec![];
        for call in &calls {
            if call.request_id != root
                || call.owner != turn.owner
                || !ids.insert(&call.call_id)
                || turn.pending.contains_key(&call.call_id)
            {
                return Err("tool batch owner or call identity mismatch".into());
            }
            if call.cancellation.borrow().is_some() || call.result.is_closed() {
                return Err("MCP tool call cancelled before handoff".into());
            }
            let tool = visible
                .iter()
                .find(|t| t.wire_name == call.wire_name)
                .ok_or("tool is not in this turn registry")?;
            mcp_policy::validate(tool, call.arguments.as_ref().unwrap_or(&json!({})))?;
            let mut item = json!({"id":format!("{}_{}",if tool.freeform {"ctc"} else {"fc"},Uuid::new_v4().simple()),"call_id":call.call_id,"status":"completed","name":tool.name});
            if tool.freeform {
                item["type"] = json!("custom_tool_call");
                item["input"] = json!(call.input.as_deref().ok_or("freeform input missing")?);
            } else if tool.definition["type"] == "tool_search" {
                item["type"] = json!("tool_search_call");
                item["execution"] = json!("client");
                item["arguments"] = call.arguments.clone().unwrap_or(json!({}));
            } else {
                item["type"] = json!("function_call");
                item["arguments"] = json!(call.arguments.clone().unwrap_or(json!({})).to_string());
                if let Some(namespace) = &tool.namespace {
                    item["namespace"] = json!(namespace);
                }
            }
            items.push(item);
        }
        // Reserve before committing pending entries: queue saturation cannot strand a reply.
        let permit = turn
            .commands
            .try_reserve()
            .map_err(|_| "tool handoff channel is full")?;
        for (call, item) in calls.into_iter().zip(&items) {
            turn.pending.insert(
                call.call_id,
                PendingCall {
                    item: item.clone(),
                    delivered: false,
                    order: turn.next_call_order,
                    reply: call.result,
                },
            );
            turn.next_call_order += 1;
        }
        permit.send(RoundCommand::Tools(items));
        Ok(())
    }
}

pub(super) fn arm_expiry(
    inner: Arc<Inner>,
    id: String,
    mut cancellation: watch::Receiver<Option<String>>,
    deadline: Instant,
) {
    tokio::spawn(async move {
        tokio::select! { _ = tokio::time::sleep_until(deadline.into()) => {}, _ = cancellation.changed() => {} }
        // Active observers own their terminal SSE. Suspended rounds have no observer
        // but must still release the browser slot and pending MCP requests.
        let mut turns = inner.turns.lock().unwrap();
        if turns.get(&id).is_some_and(|t| !t.observer_active) {
            if let Some(turn) = turns.remove(&id) {
                let reason = cancellation
                    .borrow()
                    .clone()
                    .unwrap_or_else(|| "turn_expired".into());
                if reason == "turn_expired" {
                    inner.failed.fetch_add(1, Ordering::SeqCst);
                } else {
                    inner.cancelled.fetch_add(1, Ordering::SeqCst);
                }
                turn.cancel.send_replace(Some(reason));
            }
        }
    });
}

pub(super) fn remember(inner: &Inner, root: &str, response: &Value) {
    let turns = inner.turns.lock().unwrap();
    let Some(turn) = turns.get(root) else {
        return;
    };
    let mut items = input_items(&turn.task.raw);
    items.extend(response["output"].as_array().cloned().unwrap_or_default());
    let mut history = inner.history.lock().unwrap();
    history.retain(|_, h| h.at.elapsed() < Duration::from_secs(3600));
    while history.len() >= 1000
        || history
            .values()
            .map(|h| h.items.iter().map(|i| i.to_string().len()).sum::<usize>())
            .sum::<usize>()
            > 64 * 1024 * 1024
    {
        if let Some(oldest) = history
            .iter()
            .min_by_key(|(_, h)| h.at)
            .map(|(id, _)| id.clone())
        {
            history.remove(&oldest);
        } else {
            break;
        }
    }
    history.insert(
        response["id"].as_str().unwrap().into(),
        History {
            root_id: root.into(),
            owner: turn.owner.clone(),
            items,
            at: Instant::now(),
            awaiting_tools: response["end_turn"] == false,
            actual_final: None,
        },
    );
}
fn input_items(raw: &Value) -> Vec<Value> {
    match &raw["input"] {
        Value::Array(items) => items.clone(),
        Value::String(s) => vec![json!({"role":"user","content":s})],
        _ => vec![],
    }
}
fn matches_owner(metadata: &Value, owner: &TurnOwner) -> bool {
    metadata
        .get("thread_id")
        .is_none_or(|v| v.as_str() == Some(&owner.thread_id))
        && metadata
            .get("turn_id")
            .is_none_or(|v| v.as_str() == Some(&owner.turn_id))
}

pub(super) async fn resume(
    inner: &Arc<Inner>,
    raw: &mut Value,
) -> Result<Option<mpsc::Receiver<Value>>, String> {
    let metadata = turn_metadata(raw);
    if metadata["request_kind"] == "compaction"
        || input_items(raw)
            .iter()
            .any(|i| i["type"] == "compaction_trigger")
    {
        return Ok(None);
    }
    if let Some(receiver) = replay_compaction_final(inner, raw)? {
        return Ok(Some(receiver));
    }
    let previous = raw["previous_response_id"].as_str().map(str::to_owned);
    let mut root = None;
    let mut awaiting_tools = false;
    if let Some(previous) = &previous {
        let history = inner.history.lock().unwrap();
        if let Some(stored) = history
            .get(previous)
            .filter(|h| h.at.elapsed() < Duration::from_secs(3600))
        {
            if !matches_owner(&metadata, &stored.owner) {
                return Err("previous response owner mismatch".into());
            }
            let mut items = stored.items.clone();
            items.extend(input_items(raw));
            raw["input"] = json!(items);
            awaiting_tools = stored.awaiting_tools;
            root = Some(stored.root_id.clone());
        }
        // Source expandPreviousResponseInput leaves an unknown ID untouched; full
        // context/call IDs can still recover the live turn without manufacturing history.
    }
    let items = input_items(raw);
    let results: Vec<_> = items
        .iter()
        .filter(|i| {
            matches!(
                i["type"].as_str(),
                Some("function_call_output" | "custom_tool_call_output" | "tool_search_output")
            )
        })
        .collect();
    let receiver = {
        let mut turns = inner.turns.lock().unwrap();
        if root.is_none() {
            root = turns
                .iter()
                .find(|(_, t)| {
                    matches_owner(&metadata, &t.owner)
                        && metadata["thread_id"].as_str() == Some(t.owner.thread_id.as_str())
                        && !t.pending.is_empty()
                })
                .map(|(id, _)| id.clone());
            if root.is_none() && !results.is_empty() {
                // With no explicit owner, random outstanding call IDs can correlate a
                // full-history continuation, but an asserted wrong owner never can.
                root = turns
                    .iter()
                    .find(|(_, t)| {
                        matches_owner(&metadata, &t.owner)
                            && results.iter().any(|r| {
                                r["call_id"]
                                    .as_str()
                                    .is_some_and(|id| t.pending.contains_key(id))
                            })
                    })
                    .map(|(id, _)| id.clone());
            }
        }
        let Some(root_id) = root else {
            if results.iter().any(|r| {
                turns.values().any(|t| {
                    r["call_id"]
                        .as_str()
                        .is_some_and(|id| t.pending.contains_key(id))
                })
            }) {
                return Err("tool result owner mismatch".into());
            }
            return Ok(None);
        };
        let Some(turn) = turns.get_mut(&root_id) else {
            if awaiting_tools {
                return Err("tool continuation belongs to a retired turn".into());
            }
            return Ok(None);
        };
        validate_turn(turn, None)?;
        if turn.compaction_request.is_some() {
            return Err("compaction owns this retained source".into());
        }
        if !matches_owner(&metadata, &turn.owner) {
            return Err("turn owner mismatch".into());
        }
        if raw["model"].as_str() != Some(turn.task.model.as_str()) {
            return Err("model changed during tool continuation".into());
        }
        if turn.observer_active {
            return Err("Responses observer is already active for this turn".into());
        }
        let mut matched = HashMap::new();
        for result in results {
            let call_id = result["call_id"]
                .as_str()
                .ok_or("tool output call_id required")?;
            if let Some(pending) = turn.pending.get(call_id).filter(|p| p.delivered) {
                let expected = match pending.item["type"].as_str() {
                    Some("custom_tool_call") => "custom_tool_call_output",
                    Some("tool_search_call") => "tool_search_output",
                    _ => "function_call_output",
                };
                if result["type"] != expected
                    || matched
                        .insert(call_id.to_owned(), broker_result(result)?)
                        .is_some()
                {
                    return Err("duplicate or mismatched tool output".into());
                }
            }
        }
        let outstanding = turn.pending.values().filter(|p| p.delivered).count();
        if !matched.is_empty() && matched.len() != outstanding {
            return Err(format!(
                "Codex returned {} of {} results for a parallel tool batch",
                matched.len(),
                outstanding
            ));
        }
        let replay = if matched.is_empty() {
            let mut calls = turn
                .pending
                .values()
                .filter(|p| p.delivered)
                .collect::<Vec<_>>();
            calls.sort_by_key(|p| p.order);
            Some(
                calls
                    .into_iter()
                    .map(|p| p.item.clone())
                    .collect::<Vec<_>>(),
            )
        } else {
            None
        };
        if replay.as_ref().is_some_and(Vec::is_empty) {
            return Err("no outstanding tool batch to resume".into());
        }
        let (wire_tx, wire_rx) = mpsc::channel(inner.config.channel_capacity);
        let id = format!("resp_{}", Uuid::new_v4().simple());
        turn.observer_active = true;
        turn.task.raw = raw.clone();
        // The new observer must be installed before unblocking MCP/browser work.
        if let Some(replay) = replay {
            turn.commands
                .try_send(RoundCommand::Tools(replay))
                .map_err(|_| "resume queue is full")?;
        }
        for (call_id, result) in matched {
            if let Some(pending) = turn.pending.remove(&call_id) {
                let _ = pending.reply.send(Ok(result));
            }
        }
        tokio::spawn(run_round(
            inner.clone(),
            root_id,
            id,
            turn.task.model.clone(),
            turn.event_rx.clone(),
            turn.command_rx.clone(),
            turn.cancel.subscribe(),
            turn.confirmed.subscribe(),
            turn.deadline,
            wire_tx,
        ));
        wire_rx
    };
    Ok(Some(receiver))
}

pub(super) fn broker_result(item: &Value) -> Result<Value, String> {
    let output = &item["output"];
    let mut content = vec![];
    let mut text = String::new();
    if let Some(s) = output.as_str() {
        text = s.into();
        content.push(json!({"type":"text","text":s}));
    } else if let Some(blocks) = output.as_array() {
        for block in blocks {
            match block["type"].as_str() {
                Some("input_text" | "output_text" | "text") => {
                    let s = block["text"].as_str().ok_or("tool text must be a string")?;
                    text.push_str(s);
                    content.push(json!({"type":"text","text":s}));
                }
                Some("refusal") => {
                    let s = format!("[refusal: {}]", block["refusal"].as_str().unwrap_or(""));
                    text.push_str(&s);
                    content.push(json!({"type":"text","text":s}));
                }
                Some("input_image") => {
                    let url = block["image_url"]
                        .as_str()
                        .ok_or("tool image URL missing")?;
                    if let Some((mime, data)) = url
                        .strip_prefix("data:")
                        .and_then(|s| s.split_once(";base64,"))
                    {
                        content.push(json!({"type":"image","data":data,"mimeType":mime}));
                    } else {
                        content.push(json!({"type":"resource_link","uri":url,"name":"Codex tool image","mimeType":"image/*"}));
                    }
                }
                Some("encrypted_content") => {
                    content.push(json!({"type":"text","text":"[encrypted content omitted]"}))
                }
                _ => return Err("unsupported tool output block".into()),
            }
        }
    } else if !output.is_null() {
        return Err("tool output must be string or content array".into());
    }
    // Native harnesses can serialize the actual MCP CallToolResult as their
    // tool output. Preserve that envelope, including _meta and unknown fields;
    // do not turn it into an invented local result or lose multimodal payloads.
    if let Ok(envelope) = serde_json::from_str::<Value>(&text) {
        if envelope["content"].is_array() {
            return Ok(envelope);
        }
    }
    let mut result = json!({"content":content});
    for key in ["structuredContent", "_meta", "isError"] {
        if let Some(value) = item.get(key) {
            result[key] = value.clone();
        }
    }
    if let Ok(structured) = serde_json::from_str::<Value>(&text) {
        if (structured.is_object() || structured.is_array())
            && result.get("structuredContent").is_none()
        {
            result["structuredContent"] = structured;
        }
    }
    Ok(result)
}

pub(super) fn manual_control(inner: &Inner, name: &str, args: &Value) -> Result<Value, String> {
    if inner.execution_mode() != ExecutionMode::Manual {
        return Err("Zero Risk tools require Manual mode".into());
    }
    let token = args["request_id"].as_str().ok_or("request_id required")?;
    let mut turns = inner.turns.lock().unwrap();
    let turn = turns
        .values_mut()
        .find(|t| t.token == token)
        .ok_or("Zero Risk request_id is invalid, expired, or revoked")?;
    if Instant::now() >= turn.deadline || turn.cancel.borrow().is_some() {
        return Err("Zero Risk request_id is invalid, expired, or revoked".into());
    }
    if name == "codex_turn_start" {
        if turn.final_answer.is_some() {
            return Err("Zero Risk turn is already terminal".into());
        }
        let duplicate = turn.connector_started;
        turn.connector_started = true;
        return Ok(tool_result(json!({"started":true,"duplicate":duplicate})));
    }
    let answer = args["final_answer"]
        .as_str()
        .filter(|s| !s.trim().is_empty() && s.len() <= 5_000_000)
        .ok_or("Zero Risk final_answer must not be empty or oversized")?;
    if let Some(accepted) = &turn.final_answer {
        if accepted != answer {
            return Err("Zero Risk completion conflicts with accepted final_answer".into());
        }
        return Ok(tool_result(json!({"completed":true,"duplicate":true})));
    }
    validate_turn(turn, None)?;
    if turn.inflight_tools > 0 || !turn.pending.is_empty() {
        return Err("Zero Risk turn cannot complete with pending native tool invocations".into());
    }
    turn.commands
        .try_send(RoundCommand::ManualComplete(answer.into()))
        .map_err(|_| "manual completion queue full")?;
    turn.final_answer = Some(answer.into());
    Ok(tool_result(json!({"completed":true,"duplicate":false})))
}

pub(super) fn preserve_compaction_final(
    inner: &Inner,
    root: &str,
    raw: &Value,
    receipts: Vec<Value>,
    answer: &str,
) {
    let response = json!({"id":format!("resp_{}",Uuid::new_v4().simple()),"object":"response","created_at":chrono::Utc::now().timestamp(),"model":raw["model"],"status":"completed","end_turn":true,"output":[{"type":"message","id":format!("msg_{}",Uuid::new_v4().simple()),"role":"assistant","status":"completed","content":[{"type":"output_text","text":answer,"annotations":[]}]}]});
    let latest = crate::server::compaction::latest_prompt(raw, None).unwrap_or_default();
    for history in inner
        .history
        .lock()
        .unwrap()
        .values_mut()
        .filter(|h| h.root_id == root)
    {
        history.actual_final = Some((response.clone(), receipts.clone(), latest.clone()));
    }
}
fn replay_compaction_final(
    inner: &Inner,
    raw: &Value,
) -> Result<Option<mpsc::Receiver<Value>>, String> {
    let metadata = turn_metadata(raw);
    let history = inner.history.lock().unwrap();
    for (id, entry) in history.iter() {
        let Some((snapshot, receipts, latest)) = &entry.actual_final else {
            continue;
        };
        let linked = raw["previous_response_id"].as_str() == Some(id);
        if !linked
            && (metadata["thread_id"].as_str() != Some(entry.owner.thread_id.as_str())
                || metadata["turn_id"].as_str() != Some(entry.owner.turn_id.as_str()))
        {
            continue;
        }
        if !matches_owner(&metadata, &entry.owner) {
            return Err("retained final owner mismatch".into());
        }
        if raw["model"] != snapshot["model"] {
            continue;
        }
        if let Ok(request_latest) = crate::server::compaction::latest_prompt(raw, None) {
            if &request_latest != latest {
                continue;
            }
        }
        let mut matched = 0;
        let mut ids = std::collections::HashSet::new();
        for item in input_items(raw) {
            if let Some(receipt) = receipts.iter().find(|r| {
                r["call_id"] == item["call_id"]
                    && item["type"]
                        .as_str()
                        .is_some_and(|s| s.ends_with("_output"))
            }) {
                if receipt["type"] != item["type"]
                    || receipt["output"] != item["output"]
                    || !ids.insert(item["call_id"].to_string())
                {
                    return Err("retained final canonical tool result conflict".into());
                }
                matched += 1;
            }
        }
        if matched != receipts.len() || receipts.is_empty() {
            continue;
        }
        let (tx, rx) = mpsc::channel(2);
        let mut initial = snapshot.clone();
        initial["status"] = json!("in_progress");
        initial["output"] = json!([]);
        tx.try_send(json!({"type":"response.created","sequence_number":0,"response":initial}))
            .unwrap();
        tx.try_send(json!({"type":"response.completed","sequence_number":1,"response":snapshot}))
            .unwrap();
        return Ok(Some(rx));
    }
    Ok(None)
}
