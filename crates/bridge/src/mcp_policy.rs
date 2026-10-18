//! MCP surfaces wrap only advertised native tools; execution stays in the harness.
use super::*;
const BRIDGE: &[&str] = &[
    "codex_turn_start",
    "codex_exec",
    "codex_write_stdin",
    "codex_apply_patch",
    "codex_view_image",
    "codex_tool_inventory",
    "codex_tool_call",
    "codex_turn_complete",
];
const WAIT_RULE: &str = "ChatGPT Web transport rule: wait for exactly 30 seconds per call, matching the Codex default, then release the MCP channel so spawned Web agents can use their own tools. A wait timeout is not task completion; check agent progress and wait again if needed. Keep the native tool's declared arguments.";
pub(super) fn is_wait(name: &str) -> bool {
    matches!(
        name,
        "multi_agent_v1__wait_agent" | "multi_agent_v2__wait_agent" | "collaboration__wait_agent"
    )
}
pub(super) fn visible(tools: &[NativeTool], mode: ExecutionMode) -> Vec<NativeTool> {
    if mode != ExecutionMode::Manual {
        return tools.to_vec();
    }
    let namespaces: Vec<_> = tools
        .iter()
        .filter(|t| BRIDGE.contains(&t.name.as_str()))
        .filter_map(|t| t.namespace.as_ref())
        .collect();
    tools
        .iter()
        .filter(|t| {
            t.wire_name != "codex.control.compaction_handoff"
                && !BRIDGE.contains(&t.name.as_str())
                && !(t.namespace.is_none() && t.name == "exec")
                && !t
                    .namespace
                    .as_ref()
                    .is_some_and(|n| namespaces.contains(&n))
        })
        .cloned()
        .collect()
}
pub(super) fn description(tool: &NativeTool) -> Value {
    if is_wait(&tool.wire_name) {
        json!(format!(
            "{}\n\n{}",
            tool.definition["description"].as_str().unwrap_or(""),
            WAIT_RULE
        ))
    } else {
        tool.definition["description"].clone()
    }
}
pub(super) fn parameters(tool: &NativeTool) -> Value {
    let mut schema = tool
        .definition
        .get("parameters")
        .filter(|v| v.is_object())
        .cloned()
        .unwrap_or(json!({}));
    if !is_wait(&tool.wire_name) {
        return schema;
    }
    if !schema["properties"].is_object() {
        schema["properties"] = json!({});
    }
    let mut timeout = schema["properties"]["timeout_ms"]
        .as_object()
        .cloned()
        .unwrap_or_default();
    timeout.remove("default");
    for (k,v) in json!({"type":"number","const":30000,"minimum":30000,"maximum":30000,"description":"Required transport-safe polling interval. Use exactly 30000; a timed-out wait does not mean the agents have finished."}).as_object().unwrap() {timeout.insert(k.clone(),v.clone());}
    schema["properties"]["timeout_ms"] = Value::Object(timeout);
    let mut required = Vec::new();
    for value in schema["required"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|v| v.is_string())
        .chain(std::iter::once(&json!("timeout_ms")))
    {
        if !required.contains(value) {
            required.push(value.clone());
        }
    }
    schema["required"] = json!(required);
    schema
}
pub(super) fn validate(tool: &NativeTool, args: &Value) -> Result<(), String> {
    if is_wait(&tool.wire_name) && args["timeout_ms"].as_f64() != Some(30000.0) {
        return Err("ChatGPT Web wait_agent requires timeout_ms=30000 so the shared MCP channel remains available to spawned Web agents".into());
    }
    // The upstream JS gateway guard is not available in a browser-agnostic Rust bridge.
    if tool.namespace.is_none() && tool.name == "exec" {
        return Err("raw exec gateway is unsupported; advertise direct native tools".into());
    }
    Ok(())
}
pub(super) fn aliases(key: &str, mode: ExecutionMode) -> Vec<Value> {
    let specs = [
        (
            "codex_exec",
            "Invoke the command tool advertised by the current outer Codex harness. A long-running command returns its native session_id.",
            json!({"cmd":{"type":"string","minLength":1,"maxLength":100000},"workdir":{"type":"string","maxLength":16384},"yield_time_ms":{"type":"integer","minimum":250,"maximum":30000},"max_output_tokens":{"type":"integer","minimum":1,"maximum":1000000},"tty":{"type":"boolean"},"sandbox_permissions":{"type":"string","enum":["use_default","require_escalated"]},"justification":{"type":"string"},"prefix_rule":{"type":"array","items":{"type":"string"}}}),
            "cmd",
        ),
        (
            "codex_write_stdin",
            "Write characters to, or poll, a session_id returned by codex_exec.",
            json!({"session_id":{"type":"integer","minimum":0},"chars":{"type":"string","maxLength":1000000},"yield_time_ms":{"type":"integer","minimum":250,"maximum":300000},"max_output_tokens":{"type":"integer","minimum":1,"maximum":1000000}}),
            "session_id",
        ),
        (
            "codex_apply_patch",
            "Invoke the outer Codex apply_patch tool, producing a native file-change item in the Codex task.",
            json!({"patch":{"type":"string","minLength":1,"maxLength":5000000}}),
            "patch",
        ),
        (
            "codex_view_image",
            "Invoke the outer Codex view_image tool and return its multimodal result to this same ChatGPT response.",
            json!({"path":{"type":"string","minLength":1,"maxLength":16384},"detail":{"type":"string","enum":["high","original"]}}),
            "path",
        ),
    ];
    specs.into_iter().map(|(name,desc,mut props,required)|{props[key]=json!({"type":"string","minLength":20,"maxLength":256}); json!({"name":name,"description":format!("{}{}",if mode==ExecutionMode::Manual {"For a Zero Risk request connected by codex_turn_start. "}else{""},desc),"inputSchema":{"type":"object","properties":props,"required":[key,required]},"annotations":{"readOnlyHint":name=="codex_view_image","destructiveHint":name!="codex_view_image","idempotentHint":name=="codex_view_image","openWorldHint":matches!(name,"codex_exec"|"codex_write_stdin")}})}).collect()
}
fn validate_alias(name: &str, args: &serde_json::Map<String, Value>) -> Result<(), String> {
    let spec = aliases("turn_token", ExecutionMode::Automatic)
        .into_iter()
        .find(|s| s["name"] == name)
        .ok_or("unsupported bridge tool")?;
    let schema = &spec["inputSchema"];
    for key in schema["required"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .filter(|k| *k != "turn_token")
    {
        if !args.contains_key(key) {
            return Err(format!("{key} required"));
        }
    }
    for (key, value) in args {
        let rule = &schema["properties"][key];
        if rule.is_null() {
            continue;
        }
        let valid = match rule["type"].as_str() {
            Some("string") => value.as_str().is_some_and(|s| {
                let n = s.chars().count() as u64;
                n >= rule["minLength"].as_u64().unwrap_or(0)
                    && n <= rule["maxLength"].as_u64().unwrap_or(u64::MAX)
            }),
            Some("integer") => value.as_u64().is_some_and(|n| {
                n >= rule["minimum"].as_u64().unwrap_or(0)
                    && n <= rule["maximum"].as_u64().unwrap_or(u64::MAX)
            }),
            Some("boolean") => value.is_boolean(),
            Some("array") => value
                .as_array()
                .is_some_and(|a| a.iter().all(Value::is_string)),
            _ => true,
        };
        if !valid || rule["enum"].as_array().is_some_and(|a| !a.contains(value)) {
            return Err(format!("invalid {key}"));
        }
    }
    Ok(())
}
pub(super) fn resolve(
    name: &str,
    args: &serde_json::Map<String, Value>,
    tools: &[NativeTool],
) -> Result<(String, Option<Value>, Option<String>), String> {
    validate_alias(name, args)?;
    let candidates: &[&str] = match name {
        "codex_exec" => &["exec_command", "shell_command"],
        "codex_write_stdin" => &["write_stdin"],
        "codex_apply_patch" => &["apply_patch"],
        "codex_view_image" => &["view_image"],
        _ => return Err("unsupported bridge tool".into()),
    };
    let tool=candidates.iter().find_map(|name|tools.iter().find(|t|t.namespace.is_none()&&t.name==*name)).ok_or("this turn did not advertise the required direct native tool; nested exec fallback is unsupported")?;
    let mut out = serde_json::Map::new();
    let mut copy = |from: &str, to: &str| {
        if let Some(v) = args.get(from) {
            out.insert(to.into(), v.clone());
        }
    };
    match name {
        "codex_exec" => {
            let shell = tool.name == "shell_command";
            copy("cmd", if shell { "command" } else { "cmd" });
            if args
                .get("workdir")
                .and_then(Value::as_str)
                .is_some_and(|s| !s.is_empty())
            {
                copy("workdir", "workdir");
            }
            copy(
                "yield_time_ms",
                if shell { "timeout_ms" } else { "yield_time_ms" },
            );
            if !shell {
                copy("tty", "tty");
                copy("max_output_tokens", "max_output_tokens");
            }
            for key in ["sandbox_permissions", "justification", "prefix_rule"] {
                if args.contains_key(key) {
                    if tool.definition["parameters"]["properties"]
                        .get(key)
                        .is_none()
                    {
                        return Err(format!(
                            "The current native {} tool does not support {key}",
                            tool.name
                        ));
                    }
                    copy(key, key);
                }
            }
        }
        "codex_write_stdin" => {
            for key in ["session_id", "chars", "yield_time_ms", "max_output_tokens"] {
                copy(key, key);
            }
        }
        "codex_apply_patch" => {
            if tool.freeform {
                return Ok((
                    tool.wire_name.clone(),
                    None,
                    Some(args["patch"].as_str().unwrap().into()),
                ));
            }
            copy("patch", "input");
        }
        "codex_view_image" => {
            copy("path", "path");
            copy("detail", "detail");
        }
        _ => unreachable!(),
    }
    Ok((tool.wire_name.clone(), Some(Value::Object(out)), None))
}
#[cfg(test)]
mod tests {
    use super::*;
    fn tools(v: Value) -> Vec<NativeTool> {
        compile_request(json!({"model":"fixture","input":"x","tools":v}))
            .unwrap()
            .tools
    }
    #[test]
    fn wait_schema_and_enforcement() {
        let t=tools(json!([{"type":"namespace","name":"collaboration","tools":[{"type":"function","name":"wait_agent","parameters":{"properties":{"timeout_ms":{"default":99,"extra":true}},"required":["ids",7,"ids"]}}]}])).remove(0);
        let s = parameters(&t);
        assert!(s["properties"]["timeout_ms"].get("default").is_none());
        assert_eq!(s["required"], json!(["ids", "timeout_ms"]));
        assert!(validate(&t, &json!({"timeout_ms":30000})).is_ok());
        assert!(validate(&t, &json!({"timeout_ms":30001})).is_err());
    }
    #[test]
    fn safe_namespaces_and_alias_approval() {
        let ts = tools(
            json!([{"type":"namespace","name":"bridge","tools":[{"type":"function","name":"codex_exec"},{"type":"function","name":"other"}]},{"type":"function","name":"exec_command","parameters":{"properties":{"cmd":{}}}},{"type":"custom","name":"exec"}]),
        );
        let safe = visible(&ts, ExecutionMode::Manual);
        assert_eq!(safe.len(), 1);
        let a = json!({"cmd":"true","sandbox_permissions":"require_escalated"});
        assert!(
            resolve("codex_exec", a.as_object().unwrap(), &safe)
                .unwrap_err()
                .contains("does not support")
        );
        assert!(
            resolve(
                "codex_exec",
                json!({"cmd":"true"}).as_object().unwrap(),
                &safe
            )
            .is_ok()
        );
        assert!(
            resolve(
                "codex_view_image",
                json!({"path":"x"}).as_object().unwrap(),
                &safe
            )
            .is_err()
        );
    }
}

#[cfg(test)]
mod alias_tests {
    use super::*;
    #[test]
    fn alias_payloads_preserve_approval_and_source_shell_mapping() {
        let tools=compile_request(json!({"model":"fixture","input":"x","tools":[{"type":"function","name":"shell_command","parameters":{"properties":{"sandbox_permissions":{},"justification":{},"prefix_rule":{}}}},{"type":"function","name":"view_image"},{"type":"function","name":"write_stdin"}]})).unwrap().tools;
        let args = json!({"cmd":"fixture","workdir":"/fixture","yield_time_ms":250,"tty":true,"max_output_tokens":50,"sandbox_permissions":"require_escalated","justification":"approve","prefix_rule":["fixture"]});
        let (wire, payload, input) =
            resolve("codex_exec", args.as_object().unwrap(), &tools).unwrap();
        assert_eq!(wire, "shell_command");
        assert!(input.is_none());
        assert_eq!(
            payload.unwrap(),
            json!({"command":"fixture","workdir":"/fixture","timeout_ms":250,"sandbox_permissions":"require_escalated","justification":"approve","prefix_rule":["fixture"]})
        );
        let (_, payload, _) = resolve(
            "codex_view_image",
            json!({"path":"image","detail":"original"})
                .as_object()
                .unwrap(),
            &tools,
        )
        .unwrap();
        assert_eq!(
            payload.unwrap(),
            json!({"path":"image","detail":"original"})
        );
        assert!(
            resolve(
                "codex_write_stdin",
                json!({"session_id":-1}).as_object().unwrap(),
                &tools
            )
            .is_err()
        );
        assert!(
            resolve(
                "codex_exec",
                json!({"cmd":"x","yield_time_ms":249}).as_object().unwrap(),
                &tools
            )
            .is_err()
        );
    }
}
