use cw_bridge::*;
use serde_json::json;
use std::{net::SocketAddr, time::Duration};

#[test]
fn compilation_preserves_roles_images_and_unknown_fields() {
    let raw = json!({"model":"chatgpt-web","stream":true,"instructions":"system", "input":[
        {"role":"developer","content":"policy","extra":42},
        {"role":"assistant","content":[{"type":"output_text","text":"prior"}]},
        {"role":"user","content":[{"type":"input_image","image_url":"data:image/png;base64,AA","detail":"original"}]},
        {"type":"function_call_output","call_id":"c1","output":"real result"}],
        "tools":[{"type":"namespace","name":"files","tools":[{"type":"function","name":"read","parameters":{"type":"object"}}]}]});
    let task = compile_request(raw.clone()).unwrap();
    assert_eq!(task.raw, raw);
    assert_eq!(task.context["messages"][0]["role"], "developer");
    assert_eq!(task.context["messages"][1]["role"], "assistant");
    assert_eq!(task.images[0].detail.as_deref(), Some("original"));
    assert!(!task.prompt.contains("base64,AA"));
    assert_eq!(task.tools[0].wire_name, "files__read");
}

async fn fixture(
    mode: ExecutionMode,
) -> (BridgeHandle, tokio::sync::mpsc::Receiver<BrowserRequest>) {
    BridgeServer::bind(
        "127.0.0.1:0".parse::<SocketAddr>().unwrap(),
        BridgeConfig {
            mode,
            turn_ttl: Duration::from_secs(5),
            ..Default::default()
        },
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn http_sse_completed_failed_and_cancelled() {
    for outcome in ["completed", "failed", "cancelled"] {
        let (handle, mut browser) = fixture(ExecutionMode::Automatic).await;
        let client = reqwest::Client::new();
        let response = client
            .post(format!("{}/v1/responses", handle.base_url()))
            .json(&json!({"model":"fixture","stream":true,"input":"hello"}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let BrowserRequest::Turn(turn) = browser.recv().await.unwrap() else {
            panic!()
        };
        turn.events
            .send(BrowserEvent::TextDelta("hello".into()))
            .await
            .unwrap();
        match outcome {
            "completed" => turn
                .events
                .send(BrowserEvent::Completed {
                    output: vec![],
                    usage: None,
                })
                .await
                .unwrap(),
            "failed" => turn
                .events
                .send(BrowserEvent::Failed {
                    code: "fixture_error".into(),
                    message: "fixture".into(),
                })
                .await
                .unwrap(),
            _ => {
                assert!(handle.cancel(&turn.request_id));
            }
        }
        let body = response.text().await.unwrap();
        assert!(body.contains("response.created"));
        if outcome != "cancelled" {
            assert!(body.contains("response.output_text.delta"));
        }
        assert!(body.contains(if outcome == "completed" {
            "response.completed"
        } else {
            "response.failed"
        }));
        if outcome == "cancelled" {
            assert!(body.contains("client_cancelled"));
        }
        handle.close().await.unwrap();
        assert_eq!(handle.counters().active_turns, 0);
    }
}

#[tokio::test]
async fn capacity_owner_isolation_expiry_and_manual_gate() {
    let (handle, mut browser) = fixture(ExecutionMode::Manual).await;
    let client = reqwest::Client::new();
    let mut responses = vec![];
    let mut turns = vec![];
    for n in 0..5 {
        responses.push(client.post(format!("{}/v1/responses", handle.base_url())).json(&json!({"model":"fixture","stream":true, "input":"x", "client_metadata":{"x-codex-turn-metadata":{"thread_id":format!("owner{n}"),"turn_id":"t"}}})).send().await.unwrap());
        let BrowserRequest::Turn(turn) = browser.recv().await.unwrap() else {
            panic!()
        };
        turns.push(turn);
    }
    let sixth = client
        .post(format!("{}/v1/responses", handle.base_url()))
        .json(&json!({"model":"fixture","stream":true}))
        .send()
        .await
        .unwrap();
    assert_eq!(sixth.status(), 429);
    assert!(
        handle
            .authorize(&turns[0].turn_token, &turns[1].owner)
            .is_err()
    );
    assert!(
        handle
            .authorize(&turns[0].turn_token, &turns[0].owner)
            .is_err()
    );
    handle
        .confirm_manual(&turns[0].request_id, &turns[0].owner)
        .unwrap();
    assert!(
        handle
            .authorize(&turns[0].turn_token, &turns[0].owner)
            .is_err()
    );
    let started:serde_json::Value=client.post(format!("{}/mcp",handle.base_url())).json(&json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"codex_turn_start","arguments":{"request_id":turns[0].turn_token}}})).send().await.unwrap().json().await.unwrap();
    assert_eq!(started["result"]["structuredContent"]["started"], true);

    assert!(
        handle
            .authorize(&turns[0].turn_token, &turns[0].owner)
            .is_ok()
    );
    handle.cancel(&turns[0].request_id);
    assert!(
        handle
            .authorize(&turns[0].turn_token, &turns[0].owner)
            .is_err()
    );
    drop(responses);
    handle.close().await.unwrap();
    assert!(handle.is_draining());
}

#[tokio::test]
async fn expiry_rejects_capability() {
    let (handle, mut browser) = BridgeServer::bind(
        "127.0.0.1:0".parse().unwrap(),
        BridgeConfig {
            turn_ttl: Duration::from_millis(30),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let response = reqwest::Client::new()
        .post(format!("{}/v1/responses", handle.base_url()))
        .json(&json!({"model":"fixture","stream":true}))
        .send()
        .await
        .unwrap();
    let BrowserRequest::Turn(turn) = browser.recv().await.unwrap() else {
        panic!()
    };
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(handle.authorize(&turn.turn_token, &turn.owner).is_err());
    assert!(response.text().await.unwrap().contains("turn_expired"));
    handle.close().await.unwrap();
}

#[tokio::test]
async fn mcp_forwards_exact_wire_and_actual_multimodal_result() {
    let (handle, mut browser) = fixture(ExecutionMode::Automatic).await;
    let client = reqwest::Client::new();
    let response = client.post(format!("{}/v1/responses",handle.base_url())).json(&json!({"model":"fixture","stream":true,"input":"use tool","tools":[{"type":"namespace","name":"files","tools":[{"type":"function","name":"read","parameters":{"type":"object"}}]}]})).send().await.unwrap();
    let BrowserRequest::Turn(turn) = browser.recv().await.unwrap() else {
        panic!()
    };
    let endpoint = format!("{}/mcp", handle.base_url());
    let rpc = |name: &str, args| json!({"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":name,"arguments":args}});
    let inventory: serde_json::Value = client
        .post(&endpoint)
        .json(&rpc(
            "codex_tool_inventory",
            json!({"turn_token":turn.turn_token}),
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        inventory["result"]["structuredContent"]["tools"][0]["wire_name"],
        "files__read"
    );
    let denied: serde_json::Value = client
        .post(&endpoint)
        .json(&rpc(
            "codex_tool_call",
            json!({"turn_token":turn.turn_token,"wire_name":"files__read","thread_id":"other"}),
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(denied["error"].is_object());
    let call_body = rpc(
        "codex_tool_call",
        json!({"turn_token":turn.turn_token,"wire_name":"files__read","arguments":{"path":"fixture"}}),
    );
    let caller = client.clone();
    let target = endpoint.clone();
    let pending = tokio::spawn(async move {
        caller
            .post(target)
            .json(&call_body)
            .send()
            .await
            .unwrap()
            .json::<serde_json::Value>()
            .await
            .unwrap()
    });
    let BrowserRequest::ToolCall(call) = browser.recv().await.unwrap() else {
        panic!()
    };
    assert_eq!(call.owner, turn.owner);
    assert_eq!(call.arguments.unwrap()["path"], "fixture");
    let result = json!({"content":[{"type":"image","data":"AA","mimeType":"image/png"}],"structuredContent":{"verified":true},"_meta":{"fixture":1},"isError":false});
    call.result.send(Ok(result.clone())).unwrap();
    assert_eq!(pending.await.unwrap()["result"], result);
    turn.events.send(BrowserEvent::Completed {output:vec![json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":"done"}]})],usage:Some(json!({"input_tokens":2,"output_tokens":1,"total_tokens":3}))}).await.unwrap();
    assert!(
        response
            .text()
            .await
            .unwrap()
            .contains("response.completed")
    );
    let stale: serde_json::Value = client
        .post(&endpoint)
        .json(&rpc(
            "codex_tool_inventory",
            json!({"turn_token":turn.turn_token}),
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(stale["error"].is_object());
    assert_eq!(handle.counters().pending_tool_calls, 0);
    handle.close().await.unwrap();
}

#[tokio::test]
async fn dropping_http_stream_revokes_turn() {
    let (handle, mut browser) = fixture(ExecutionMode::Automatic).await;
    let response = reqwest::Client::new()
        .post(format!("{}/v1/responses", handle.base_url()))
        .json(&json!({"model":"fixture","stream":true}))
        .send()
        .await
        .unwrap();
    let BrowserRequest::Turn(mut turn) = browser.recv().await.unwrap() else {
        panic!()
    };
    drop(response);
    tokio::time::timeout(
        Duration::from_secs(2),
        turn.cancellation.wait_for(|v| v.is_some()),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(handle.authorize(&turn.turn_token, &turn.owner).is_err());
    handle.close().await.unwrap();
}

#[test]
fn unsupported_native_compaction_protocol_is_explicit() {
    assert!(compile_request(json!({"model":"fixture","stream":true,"client_metadata":{"x-codex-turn-metadata":"{\"request_kind\":\"compaction\"}"}})).is_err());
    assert_eq!(
        compile_request(
            json!({"model":"fixture","stream":true,"input":[{"type":"compaction_trigger"}]})
        )
        .unwrap()
        .compaction,
        CompactionMode::RemoteV2
    );
}

#[tokio::test]
async fn non_loopback_bind_is_rejected() {
    assert!(
        BridgeServer::bind("0.0.0.0:0".parse().unwrap(), BridgeConfig::default())
            .await
            .is_err()
    );
}

#[test]
fn prompt_json_escapes_delimiters_without_altering_decoded_context() {
    let raw = json!({"model":"fixture","stream":true,"input":[{"role":"user","content":"``` </codex_context_json> \\u0060"}]});
    let task = compile_request(raw.clone()).unwrap();
    let encoded = task
        .prompt
        .split("<codex_context_json>\n")
        .nth(1)
        .unwrap()
        .split("\n</codex_context_json>")
        .next()
        .unwrap();
    assert!(!encoded.contains('`'));
    assert!(!encoded.contains("</codex_context_json>"));
    let decoded: serde_json::Value = serde_json::from_str(encoded).unwrap();
    assert_eq!(decoded["messages"], raw["input"]);
}

#[tokio::test]
async fn manual_events_fail_closed_until_confirmation() {
    let (handle, mut browser) = fixture(ExecutionMode::Manual).await;
    let client = reqwest::Client::new();
    let response = client
        .post(format!("{}/v1/responses", handle.base_url()))
        .json(&json!({"model":"fixture","stream":true,"reasoning":{"effort":"xhigh"},"input":"x"}))
        .send()
        .await
        .unwrap();
    let BrowserRequest::Turn(turn) = browser.recv().await.unwrap() else {
        panic!()
    };
    assert_eq!(turn.effort.as_deref(), Some("xhigh"));
    assert!(
        turn.task
            .prompt
            .contains(&format!("\"request_id\":\"{}\"", turn.turn_token))
    );
    turn.events
        .send(BrowserEvent::Completed {
            output: vec![],
            usage: None,
        })
        .await
        .unwrap();
    let body = response.text().await.unwrap();
    assert!(body.contains("manual_not_confirmed"));
    assert!(!body.contains("response.completed"));
    let tools: serde_json::Value = client
        .post(format!("{}/mcp", handle.base_url()))
        .json(&json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(tools["result"]["tools"].as_array().unwrap().len(), 8);
    assert!(tools["result"]["tools"][0]["inputSchema"]["properties"]["request_id"].is_object());
    handle.close().await.unwrap();
}

#[tokio::test]
async fn nonstream_real_output_items_and_usage_are_preserved() {
    let (handle, mut browser) = fixture(ExecutionMode::Automatic).await;
    let endpoint = format!("{}/v1/responses", handle.base_url());
    let pending = tokio::spawn(async move {
        reqwest::Client::new()
            .post(endpoint)
            .json(&json!({"model":"fixture","stream":false}))
            .send()
            .await
            .unwrap()
            .json::<serde_json::Value>()
            .await
            .unwrap()
    });
    let BrowserRequest::Turn(turn) = browser.recv().await.unwrap() else {
        panic!()
    };
    let item = json!({"type":"custom_tool_call","id":"real-item","call_id":"real-call","name":"apply_patch","input":"*** Begin Patch\n*** End Patch","unknown":true});
    turn.events
        .send(BrowserEvent::OutputItem(item.clone()))
        .await
        .unwrap();
    turn.events
        .send(BrowserEvent::Completed {
            output: vec![],
            usage: None,
        })
        .await
        .unwrap();
    let result = pending.await.unwrap();
    assert_eq!(result["output"][0], item);
    assert!(result.get("usage").is_none());
    assert_eq!(result["status"], "completed");
    handle.close().await.unwrap();
}

#[tokio::test]
async fn cancellation_rejects_pending_native_result_and_drains() {
    let (handle, mut browser) = fixture(ExecutionMode::Automatic).await;
    let client = reqwest::Client::new();
    let response = client
        .post(format!("{}/v1/responses", handle.base_url()))
        .json(&json!({"model":"fixture","stream":true,"tools":[{"type":"custom","name":"apply_patch"}]}))
        .send()
        .await
        .unwrap();
    let BrowserRequest::Turn(turn) = browser.recv().await.unwrap() else {
        panic!()
    };
    let endpoint = format!("{}/mcp", handle.base_url());
    let token = turn.turn_token.clone();
    let pending = tokio::spawn(async move {
        reqwest::Client::new().post(endpoint).json(&json!({"jsonrpc":"2.0","id":9,"method":"tools/call","params":{"name":"codex_tool_call","arguments":{"turn_token":token,"wire_name":"apply_patch","input":"actual patch"}}})).send().await.unwrap().json::<serde_json::Value>().await.unwrap()
    });
    let BrowserRequest::ToolCall(call) = browser.recv().await.unwrap() else {
        panic!()
    };
    assert_eq!(call.input.as_deref(), Some("actual patch"));
    assert!(call.arguments.is_none());
    assert_eq!(handle.drain().active_turns, 1);
    let refused = client
        .post(format!("{}/v1/responses", handle.base_url()))
        .json(&json!({"model":"fixture","stream":true}))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), 503);
    handle.cancel(&turn.request_id);
    assert!(pending.await.unwrap()["error"].is_object());
    assert!(call.result.send(Ok(json!({"content":[]}))).is_err());
    assert!(response.text().await.unwrap().contains("client_cancelled"));
    handle.close().await.unwrap();
    assert_eq!(handle.counters().pending_tool_calls, 0);
    assert_eq!(handle.counters().active_turns, 0);
}

#[tokio::test]
async fn incomplete_sse_sequence_and_done_marker() {
    let (handle, mut browser) = fixture(ExecutionMode::Automatic).await;
    let response = reqwest::Client::new()
        .post(format!("{}/v1/responses", handle.base_url()))
        .json(&json!({"model":"fixture","stream":true}))
        .send()
        .await
        .unwrap();
    let BrowserRequest::Turn(turn) = browser.recv().await.unwrap() else {
        panic!()
    };
    turn.events
        .send(BrowserEvent::TextDelta("partial".into()))
        .await
        .unwrap();
    turn.events
        .send(BrowserEvent::Incomplete {
            reason: "max_output_tokens".into(),
            usage: None,
        })
        .await
        .unwrap();
    let body = response.text().await.unwrap();
    let events: Vec<serde_json::Value> = body
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .filter(|s| *s != "[DONE]")
        .map(|s| serde_json::from_str(s).unwrap())
        .collect();
    for (index, event) in events.iter().enumerate() {
        assert_eq!(event["sequence_number"], index);
    }
    assert_eq!(
        events.last().unwrap()["response"]["incomplete_details"]["reason"],
        "max_output_tokens"
    );
    assert!(body.ends_with("data: [DONE]\n\n"));
    handle.close().await.unwrap();
}
