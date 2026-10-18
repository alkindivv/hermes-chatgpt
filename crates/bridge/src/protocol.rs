use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot, watch};
pub(crate) const SUMMARY_PREFIX: &str = "Another language model started to solve this problem and produced a summary of its thinking process. You also have access to the state of the tools that were used by that language model. Use this to build on the work that has already been done and avoid duplicating work. Here is the summary produced by the other language model, use the information in this summary to assist with your own analysis:";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionMode {
    #[default]
    Automatic,
    Manual,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnOwner {
    pub thread_id: String,
    pub turn_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InputImage {
    pub r#ref: String,
    pub image_url: String,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NativeTool {
    pub wire_name: String,
    pub name: String,
    pub namespace: Option<String>,
    pub freeform: bool,
    pub definition: Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CompactionMode {
    #[default]
    None,
    NativeText,
    RemoteV2,
}

#[derive(Debug, Clone)]
pub struct CompiledTask {
    pub compaction: CompactionMode,
    /// Complete original request, including unrecognized fields.
    pub raw: Value,
    pub model: String,
    pub effort: Option<String>,
    pub context: Value,
    pub prompt: String,
    pub images: Vec<InputImage>,
    pub tools: Vec<NativeTool>,
}

/// The application must consume Turn requests and route ToolCall requests to its
/// real native harness. Dropping a reply produces an explicit failure.
#[derive(Debug)]
pub enum BrowserRequest {
    Turn(BrowserTurn),
    ToolCall(NativeToolCall),
    Compaction(CompactionRequest),
}

/// One-purpose checkpoint. Never run this through the ordinary task dispatcher.
#[derive(Debug)]
pub struct CompactionRequest {
    pub request_id: String,
    pub source_request_id: Option<String>,
    pub owner: TurnOwner,
    pub model: String,
    pub effort: Option<String>,
    pub task: CompiledTask,
    pub prompt: String,
    pub control_token: String,
    pub handoff_id: String,
    pub mode: ExecutionMode,
    /// Manual live-source compaction completes the already submitted response.
    pub reuse_manual_response: bool,
    /// Wait for true, then wait for physical source settlement and call
    /// confirm_compaction_source_settled BEFORE submitting the checkpoint message.
    pub source_finished: watch::Receiver<bool>,
    /// True means the actual MCP checkpoint receipt was accepted. Stop/settle the
    /// owned checkpoint response; no DOM answer or Copy action is required.
    pub accepted: watch::Receiver<bool>,
    pub cancellation: watch::Receiver<Option<String>>,
    /// Send only after physical browser cleanup (also on errors/cancellation).
    pub completion: oneshot::Sender<Result<(), String>>,
}

#[derive(Debug)]
pub struct BrowserTurn {
    pub request_id: String,
    pub owner: TurnOwner,
    pub turn_token: String,
    pub task: CompiledTask,
    pub context: Value,
    pub model: String,
    pub effort: Option<String>,
    pub mode: ExecutionMode,
    pub events: mpsc::Sender<BrowserEvent>,
    /// None while active; Some(reason) on cancellation, expiry or retirement.
    pub cancellation: watch::Receiver<Option<String>>,
    /// Manual mode must wait until true before accepting connector work.
    pub confirmed: watch::Receiver<bool>,
}

#[derive(Debug)]
pub struct NativeToolCall {
    pub request_id: String,
    pub owner: TurnOwner,
    pub call_id: String,
    pub wire_name: String,
    pub arguments: Option<Value>,
    pub input: Option<String>,
    pub cancellation: watch::Receiver<Option<String>>,
    /// Exact MCP CallToolResult, including content, structuredContent and _meta.
    pub result: oneshot::Sender<Result<Value, String>>,
}

#[derive(Debug, Clone)]
pub enum BrowserEvent {
    TextDelta(String),
    /// Complete actual output item (function_call/custom_tool_call/reasoning/etc.).
    OutputItem(Value),
    Completed {
        output: Vec<Value>,
        usage: Option<Value>,
    },
    Incomplete {
        reason: String,
        usage: Option<Value>,
    },
    Failed {
        code: String,
        message: String,
    },
    Cancelled,
}

pub fn turn_metadata(raw: &Value) -> Value {
    let value = &raw["client_metadata"]["x-codex-turn-metadata"];
    value
        .as_str()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_else(|| value.clone())
}

pub fn compile_request(raw: Value) -> Result<CompiledTask, String> {
    let model = raw["model"]
        .as_str()
        .filter(|s| !s.is_empty())
        .ok_or("model must be a nonempty string")?
        .to_owned();
    let metadata = turn_metadata(&raw);
    let mut compaction = CompactionMode::None;
    if metadata["request_kind"] == "compaction" {
        if metadata["compaction"]["implementation"] != "responses"
            || metadata["compaction"]["strategy"] != "memento"
        {
            return Err(
                "Unsupported native text compaction protocol; expected responses/memento".into(),
            );
        }
        compaction = CompactionMode::NativeText;
    }
    let mut messages = match raw.get("input") {
        None | Some(Value::Null) => vec![],
        Some(Value::String(s)) => vec![json!({"role":"user", "content":s})],
        Some(Value::Array(items)) => items.clone(),
        _ => return Err("input must be a string or array".into()),
    };
    if messages.iter().any(|m| m["type"] == "compaction_trigger") {
        compaction = CompactionMode::RemoteV2;
    }
    messages.retain(|m| {
        m["type"] != "compaction_trigger"
            && !(m["type"] == "context_compaction" && !m["encrypted_content"].is_string())
    });
    for message in &mut messages {
        if matches!(
            message["type"].as_str(),
            Some("compaction" | "compaction_summary" | "context_compaction")
        ) {
            use base64::Engine;
            let summary = message["encrypted_content"]
                .as_str()
                .and_then(|s| s.strip_prefix("ocx1:"))
                .and_then(|s| base64::engine::general_purpose::STANDARD.decode(s).ok())
                .and_then(|v| String::from_utf8(v).ok());
            *message = json!({"role":"user","content":summary.map(|s|format!("Another language model started to solve this problem and produced a summary of its thinking process. You also have access to the state of the tools that were used by that language model. Use this to build on the work that has already been done and avoid duplicating work. Here is the summary produced by the other language model, use the information in this summary to assist with your own analysis:\n\n{s}")).unwrap_or_else(||"[earlier conversation was compacted; the summary is stored in a format this model cannot read]".into())});
        }
    }
    if messages.iter().any(|m| {
        m["type"] == "agent_message"
            && m["content"]
                .as_array()
                .is_some_and(|blocks| blocks.iter().any(|b| b["type"] == "encrypted_content"))
    }) {
        return Err(
            "opaque encrypted agent_message cannot be submitted to the browser adapter".into(),
        );
    }
    let mut images = vec![];
    for message in &mut messages {
        extract_images(message, &mut images)?;
    }
    let mut tools = vec![];
    let mut entries = raw
        .get("tools")
        .map(|v| v.as_array().cloned().ok_or("tools must be an array"))
        .transpose()?
        .unwrap_or_default();
    for message in &messages {
        if matches!(
            message["type"].as_str(),
            Some("additional_tools" | "tool_search_output")
        ) {
            if let Some(extra) = message["tools"].as_array() {
                for tool in extra {
                    if !entries.contains(tool) {
                        entries.push(tool.clone());
                    }
                }
            }
        }
    }
    {
        for entry in &entries {
            if entry["type"] == "namespace" {
                let namespace = entry["name"].as_str().ok_or("namespace name is missing")?;
                for inner in entry["tools"]
                    .as_array()
                    .ok_or("namespace tools must be an array")?
                {
                    add_tool(
                        inner,
                        (namespace != "functions").then_some(namespace),
                        &mut tools,
                    )?;
                }
            } else {
                add_tool(entry, None, &mut tools)?;
            }
        }
    }
    if images.len() > 10 {
        let dropped = images.len() - 10;
        images.drain(..dropped);
        for (index, image) in images.iter_mut().enumerate() {
            image.r#ref = format!("codex-input-image-{}", index + 1);
        }
        for message in &mut messages {
            renumber_images(message, dropped);
        }
    }
    let (system, messages) = message_envelopes(&raw, &messages)?;
    let context = json!({"version":3,"system":system,"messages":messages});
    if compaction != CompactionMode::None {
        tools.clear();
    }
    let mut task = CompiledTask {
        compaction,
        effort: raw["reasoning"]["effort"].as_str().map(str::to_owned),
        raw,
        model,
        context,
        prompt: String::new(),
        images,
        tools,
    };
    task.prompt = crate::prompt::compile_prompt(&task, None, ExecutionMode::Automatic);
    Ok(task)
}

fn message_envelopes(raw: &Value, items: &[Value]) -> Result<(Vec<String>, Vec<Value>), String> {
    let mut system = raw["instructions"]
        .as_str()
        .filter(|s| !s.is_empty())
        .map(|s| vec![s.to_owned()])
        .unwrap_or_default();
    let mut messages = vec![];
    for item in items {
        let kind = item["type"].as_str().unwrap_or("message");
        if kind == "additional_tools" {
            continue;
        }
        let mut message = item.clone();
        if item["role"] == "system" {
            system.push(content_text(&item["content"]));
            continue;
        }
        match kind {
            "function_call" | "custom_tool_call" => {
                let arguments = if kind == "custom_tool_call" {
                    json!({"input":item["input"]})
                } else {
                    serde_json::from_str(item["arguments"].as_str().unwrap_or("{}"))
                        .map_err(|_| "invalid JSON tool arguments")?
                };
                let mut part = json!({"type":"tool_call","id":item["call_id"],"name":item["name"],"arguments":arguments});
                if item.get("namespace").is_some() {
                    part["namespace"] = item["namespace"].clone();
                }
                message = json!({"role":"assistant","content":[part]});
            }
            "function_call_output" | "custom_tool_call_output" => {
                let call = items.iter().rev().find(|i| {
                    i["call_id"] == item["call_id"]
                        && matches!(
                            i["type"].as_str(),
                            Some("function_call" | "custom_tool_call")
                        )
                });
                message["role"] = json!("tool_result");
                message["tool_call_id"] = item["call_id"].clone();
                message["tool_name"] = call.map(|c| c["name"].clone()).unwrap_or(json!("unknown"));
                if let Some(namespace) = call.and_then(|c| c.get("namespace")) {
                    message["tool_namespace"] = namespace.clone();
                }
                message["is_error"] = json!(false);
                message["content"] = input_content(&item["output"]);
            }
            "agent_message" => {
                message["role"] = json!("agent_message");
                message["content"] = input_content(&item["content"]);
            }
            "message" => {
                if item["role"] == "assistant" {
                    let content = if let Some(text) = item["content"].as_str() {
                        vec![json!({"type":"text","text":text})]
                    } else {
                        item["content"]
                            .as_array()
                            .map(|a| {
                                a.iter()
                                    .map(|p| {
                                        if matches!(
                                            p["type"].as_str(),
                                            Some("output_text" | "text")
                                        ) {
                                            json!({"type":"text","text":p["text"]})
                                        } else {
                                            p.clone()
                                        }
                                    })
                                    .collect()
                            })
                            .unwrap_or_default()
                    };
                    message["content"] = json!(content);
                } else {
                    message["content"] = input_content(&item["content"]);
                }
            }
            _ => {}
        }
        messages.push(message);
    }
    Ok((system, messages))
}
fn content_text(value: &Value) -> String {
    if let Some(text) = value.as_str() {
        return text.into();
    }
    value
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|p| p["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}
fn input_content(value: &Value) -> Value {
    let Some(blocks) = value.as_array() else {
        return value.clone();
    };
    if !blocks.iter().any(|p| p["type"] == "image_attachment") {
        return json!(content_text(value));
    }
    json!(
        blocks
            .iter()
            .map(|p| {
                if matches!(
                    p["type"].as_str(),
                    Some("input_text" | "output_text" | "text")
                ) {
                    json!({"type":"text","text":p["text"]})
                } else {
                    p.clone()
                }
            })
            .collect::<Vec<_>>()
    )
}

fn extract_images(value: &mut Value, images: &mut Vec<InputImage>) -> Result<(), String> {
    if value["type"] == "input_image" {
        if let Some(url) = value["image_url"].as_str() {
            let reference = format!("codex-input-image-{}", images.len() + 1);
            images.push(InputImage {
                r#ref: reference.clone(),
                image_url: url.into(),
                detail: value["detail"].as_str().map(str::to_owned),
            });
            let object = value.as_object_mut().unwrap();
            object.remove("image_url");
            object.insert("type".into(), json!("image_attachment"));
            object.insert("attachment_ref".into(), json!(reference));
        } else if let Some(file) = value["file_id"].as_str() {
            *value = json!({"type":"text","text":format!("[image: {file}]")});
        } else {
            return Err("input_image requires image_url or file_id".into());
        }
        return Ok(());
    }
    match value {
        Value::Object(map) => {
            for child in map.values_mut() {
                extract_images(child, images)?;
            }
        }
        Value::Array(items) => {
            items.retain(|item| !one_pixel_image(item));
            for child in items {
                extract_images(child, images)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn add_tool(
    entry: &Value,
    namespace: Option<&str>,
    tools: &mut Vec<NativeTool>,
) -> Result<(), String> {
    let kind = entry["type"].as_str().ok_or("tool type is missing")?;
    if matches!(
        kind,
        "web_search" | "web_search_preview" | "image_generation"
    ) {
        return Ok(());
    }
    if kind == "custom" && namespace.is_some() {
        return Ok(());
    }
    let name = entry["name"]
        .as_str()
        .or((kind == "tool_search").then_some("tool_search"))
        .ok_or("unsupported unnamed tool")?;
    let wire_name = namespace
        .map(|ns| format!("{ns}__{name}"))
        .unwrap_or_else(|| name.into());
    if tools.iter().any(|t| t.wire_name == wire_name) {
        return Err(format!("duplicate tool wire name: {wire_name}"));
    }
    tools.push(NativeTool {
        wire_name,
        name: name.into(),
        namespace: namespace.map(str::to_owned),
        freeform: kind == "custom",
        definition: entry.clone(),
    });
    Ok(())
}

pub(crate) fn one_pixel_image(item: &Value) -> bool {
    use base64::Engine;
    let Some(data) = item["image_url"]
        .as_str()
        .filter(|_| item["type"] == "input_image")
        .and_then(|s| s.strip_prefix("data:image/png;base64,"))
    else {
        return false;
    };
    let Ok(png) = base64::engine::general_purpose::STANDARD.decode(data) else {
        return false;
    };
    png.len() >= 24
        && png[..8] == [137, 80, 78, 71, 13, 10, 26, 10]
        && png[16..20] == [0, 0, 0, 1]
        && png[20..24] == [0, 0, 0, 1]
}
fn renumber_images(value: &mut Value, dropped: usize) {
    if value["type"] == "image_attachment" {
        if let Some(index) = value["attachment_ref"]
            .as_str()
            .and_then(|s| s.strip_prefix("codex-input-image-"))
            .and_then(|s| s.parse::<usize>().ok())
        {
            if index <= dropped {
                *value = json!({"type":"text","text":"[older image not attached: ChatGPT accepts at most 10 per message]"});
            } else {
                value["attachment_ref"] = json!(format!("codex-input-image-{}", index - dropped));
            }
        }
        return;
    }
    match value {
        Value::Object(map) => {
            for child in map.values_mut() {
                renumber_images(child, dropped)
            }
        }
        Value::Array(items) => {
            for child in items {
                renumber_images(child, dropped)
            }
        }
        _ => {}
    }
}
