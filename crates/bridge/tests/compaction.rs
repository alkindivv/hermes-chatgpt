use cw_bridge::*;
use serde_json::{Value, json};
use std::time::Duration;
async fn start() -> (BridgeHandle, tokio::sync::mpsc::Receiver<BrowserRequest>) {
    BridgeServer::bind(
        "127.0.0.1:0".parse().unwrap(),
        BridgeConfig {
            turn_ttl: Duration::from_secs(3),
            ..Default::default()
        },
    )
    .await
    .unwrap()
}
fn meta() -> Value {
    json!({"x-codex-turn-metadata":{"thread_id":"thread","turn_id":"turn"}})
}
async fn submit(
    client: &reqwest::Client,
    bridge: &BridgeHandle,
    request: &CompactionRequest,
    summary: &str,
) -> Value {
    client.post(format!("{}/mcp",bridge.base_url())).json(&json!({"jsonrpc":"2.0","id":9,"method":"tools/call","params":{"name":"codex_tool_call","arguments":{"turn_token":request.control_token,"wire_name":"codex.control.compaction_handoff","arguments":{"handoff_id":request.handoff_id,"summary":summary}}}})).send().await.unwrap().json().await.unwrap()
}
#[tokio::test]
async fn compact_v1_is_replacement_history_and_one_shot_checkpoint_only() {
    let (bridge, mut browser) = start().await;
    let client = reqwest::Client::new();
    let raw = json!({"model":"fixture","client_metadata":meta(),"input":[{"type":"message","role":"user","id":"human","extra":7,"content":"actual task"},{"role":"assistant","content":"earlier answer"}]});
    let pending = {
        let client = client.clone();
        let url = format!("{}/responses/compact", bridge.base_url());
        let raw = raw.clone();
        tokio::spawn(async move { client.post(url).json(&raw).send().await.unwrap() })
    };
    let BrowserRequest::Compaction(request) = browser.recv().await.unwrap() else {
        panic!("must not submit an ordinary turn")
    };
    assert!(request.source_request_id.is_none());
    assert!(request.task.tools.is_empty());
    assert!(request.prompt.contains("codex.control.compaction_handoff"));
    assert!(submit(&client, &bridge, &request, "premature").await["error"].is_object());
    bridge
        .confirm_compaction_source_settled(&request.request_id, &request.owner)
        .unwrap();
    assert_eq!(
        submit(&client, &bridge, &request, "actual checkpoint").await["result"]["structuredContent"]
            ["submitted"],
        true
    );
    assert!(submit(&client, &bridge, &request, "second").await["error"].is_object());
    assert!(
        !pending.is_finished(),
        "receipt is not physical browser settlement"
    );
    request.completion.send(Ok(())).unwrap();
    let response = pending.await.unwrap();
    assert_eq!(response.status(), 200);
    let output: Value = response.json().await.unwrap();
    assert_eq!(output["output"][0]["id"], "human");
    assert_eq!(output["output"][0]["extra"], 7);
    assert_eq!(output["output"].as_array().unwrap().len(), 2);
    assert_eq!(output["output"][1]["role"], "user");
    assert!(output.to_string().contains("actual checkpoint"));
    assert!(!output.to_string().contains("encrypted_content"));
    let replay: Value = client
        .post(format!("{}/responses/compact", bridge.base_url()))
        .json(&raw)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(replay, output);
    assert!(browser.try_recv().is_err());
    bridge.close().await.unwrap();
}
#[test]
fn compact_v1_keeps_source_metadata_newest_images_and_utf16_text_budget() {
    let mut input:Vec<Value>=(0..12).map(|i|json!({"type":"message","role":"user","id":format!("u{i}"),"content":[{"type":"input_text","text":format!("task{i}")},{"type":"input_image","image_url":format!("https://fixture.invalid/{i}"),"detail":"original"}]})).collect();
    input.push(json!({"type":"message","role":"user","id":"refresh","internal_chat_message_metadata_passthrough":{"turn_id":"turn","content_item_kinds":["agents_md.instructions"]},"content":"rules"}));
    input.push(json!({"role":"user","content":"<goal_context>steering</goal_context>"}));
    let output = build_compact_v1_output(&json!(input), "checkpoint");
    assert_eq!(output.len(), 13);
    assert!(!json!(output).to_string().contains("refresh"));
    let images: Vec<_> = output
        .iter()
        .flat_map(|m| m["content"].as_array().into_iter().flatten())
        .filter(|b| b["type"] == "input_image")
        .collect();
    assert_eq!(images.len(), 10);
    assert_eq!(images[0]["image_url"], "https://fixture.invalid/2");
    let output = build_compact_v1_output(
        &json!([{"role":"user","content":"a".repeat(80_002)}]),
        "summary",
    );
    assert_eq!(
        output[0]["content"][0]["text"].as_str().unwrap().len(),
        80_000
    );
}
fn terminal(body: &str) -> Value {
    body.lines()
        .filter_map(|s| s.strip_prefix("data: "))
        .filter_map(|s| serde_json::from_str::<Value>(s).ok())
        .find(|v| v["type"] == "response.completed")
        .unwrap()["response"]
        .clone()
}
async fn rpc(client: &reqwest::Client, bridge: &BridgeHandle, name: &str, args: Value) -> Value {
    client.post(format!("{}/mcp",bridge.base_url())).json(&json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":name,"arguments":args}})).send().await.unwrap().json().await.unwrap()
}
#[tokio::test]
async fn mid_tool_checkpoint_keeps_canonical_results_and_reuses_exact_owner() {
    let (bridge, mut browser) = start().await;
    let client = reqwest::Client::new();
    let human = json!({"type":"message","role":"user","id":"human","content":"actual task"});
    let first=client.post(format!("{}/responses",bridge.base_url())).json(&json!({"model":"fixture","stream":true,"client_metadata":meta(),"input":[human.clone()],"tools":[{"type":"function","name":"read"}]})).send().await.unwrap();
    let BrowserRequest::Turn(turn) = browser.recv().await.unwrap() else {
        panic!()
    };
    let token = turn.turn_token.clone();
    let url = format!("{}/mcp", bridge.base_url());
    let tool = tokio::spawn(async move {
        reqwest::Client::new().post(url).json(&json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"codex_tool_call","arguments":{"turn_token":token,"wire_name":"read"}}})).send().await.unwrap().json::<Value>().await.unwrap()
    });
    let BrowserRequest::ToolCall(call) = browser.recv().await.unwrap() else {
        panic!()
    };
    let call_id = call.call_id.clone();
    bridge.forward_native_tool(call).await.unwrap();
    let first = terminal(&first.text().await.unwrap());
    assert_eq!(first["end_turn"], false);
    let canonical = json!({"content":[{"type":"text","text":"actual harness result"}],"structuredContent":{"receipt":1},"_meta":{"keep":true},"isError":false});
    let body = json!({"model":"fixture","client_metadata":{"x-codex-turn-metadata":{"thread_id":"wrong","turn_id":"wrong"}},"input":[human,{"type":"function_call_output","call_id":call_id,"output":canonical.to_string()}]});
    let compact = {
        let client = client.clone();
        let url = format!("{}/responses/compact", bridge.base_url());
        tokio::spawn(async move {
            client
                .post(url)
                .header(
                    "x-codex-turn-metadata",
                    meta()["x-codex-turn-metadata"].to_string(),
                )
                .json(&body)
                .send()
                .await
                .unwrap()
        })
    };
    let BrowserRequest::Compaction(mut checkpoint) = browser.recv().await.unwrap() else {
        panic!()
    };
    assert_eq!(
        checkpoint.source_request_id.as_deref(),
        Some(turn.request_id.as_str())
    );
    assert!(!*checkpoint.source_finished.borrow());
    assert_eq!(tool.await.unwrap()["result"], canonical);
    assert!(
        bridge
            .confirm_compaction_source_settled(&checkpoint.request_id, &checkpoint.owner)
            .is_err()
    );
    let stopped = rpc(
        &client,
        &bridge,
        "codex_tool_call",
        json!({"turn_token":turn.turn_token,"wire_name":"read"}),
    )
    .await;
    assert_eq!(stopped["result"]["isError"], true);
    assert!(stopped.to_string().contains("tool was not executed"));
    assert!(browser.try_recv().is_err());
    let inventory = rpc(
        &client,
        &bridge,
        "codex_tool_inventory",
        json!({"turn_token":turn.turn_token}),
    )
    .await;
    assert_eq!(inventory["result"]["structuredContent"]["total"], 0);
    turn.events
        .send(BrowserEvent::TextDelta("source normal final".into()))
        .await
        .unwrap();
    turn.events
        .send(BrowserEvent::Completed {
            output: vec![],
            usage: None,
        })
        .await
        .unwrap();
    tokio::time::timeout(
        Duration::from_secs(1),
        checkpoint.source_finished.wait_for(|v| *v),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(turn.cancellation.borrow().is_none());
    assert!(bridge.authorize(&turn.turn_token, &turn.owner).is_err());
    bridge
        .confirm_compaction_source_settled(&checkpoint.request_id, &checkpoint.owner)
        .unwrap();
    assert!(
        rpc(
            &client,
            &bridge,
            "codex_tool_inventory",
            json!({"turn_token":checkpoint.control_token})
        )
        .await["error"]
            .is_object()
    );
    assert_eq!(
        submit(&client, &bridge, &checkpoint, "actual checkpoint").await["result"]["structuredContent"]
            ["submitted"],
        true
    );
    assert!(turn.cancellation.borrow().is_none());
    checkpoint.completion.send(Ok(())).unwrap();
    let response = compact.await.unwrap();
    assert_eq!(response.status(), 200);
    let response: Value = response.json().await.unwrap();
    assert!(
        response
            .to_string()
            .contains("CODEX_LATEST_USER_PROMPT_JSON")
    );
    let mut retired = turn.cancellation.clone();
    tokio::time::timeout(Duration::from_secs(1), retired.wait_for(|v| v.is_some()))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(bridge.counters().active_turns, 0);
    assert_eq!(bridge.counters().active_compactions, 0);
    bridge.close().await.unwrap();
}
#[tokio::test]
async fn physical_cleanup_is_required_after_missing_receipt_and_cancellation() {
    let (bridge, mut browser) = start().await;
    let client = reqwest::Client::new();
    let url = format!("{}/responses/compact", bridge.base_url());
    let pending = tokio::spawn(async move {
        client
            .post(url)
            .json(&json!({"model":"fixture","input":[{"role":"user","id":"h","content":"task"}]}))
            .send()
            .await
            .unwrap()
    });
    let BrowserRequest::Compaction(mut checkpoint) = browser.recv().await.unwrap() else {
        panic!()
    };
    assert!(bridge.cancel(&checkpoint.request_id));
    tokio::time::timeout(
        Duration::from_secs(1),
        checkpoint.cancellation.wait_for(|v| v.is_some()),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(pending.await.unwrap().status(), 409);
    assert_eq!(bridge.counters().active_compactions, 1);
    assert!(bridge.set_mode(ExecutionMode::Manual).is_err());
    checkpoint.completion.send(Ok(())).unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        while bridge.counters().active_compactions != 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    bridge.close().await.unwrap();
}
async fn boundary(
    bridge: &BridgeHandle,
    browser: &mut tokio::sync::mpsc::Receiver<BrowserRequest>,
    mode: ExecutionMode,
) -> (
    BrowserTurn,
    String,
    Value,
    Value,
    tokio::task::JoinHandle<Value>,
) {
    bridge.set_mode(mode).unwrap();
    let client = reqwest::Client::new();
    let human = json!({"type":"message","role":"user","id":"h","content":"actual task"});
    let response=client.post(format!("{}/responses",bridge.base_url())).json(&json!({"model":"fixture","stream":true,"client_metadata":meta(),"input":[human.clone()],"tools":[{"type":"function","name":"read"}]})).send().await.unwrap();
    let BrowserRequest::Turn(turn) = browser.recv().await.unwrap() else {
        panic!()
    };
    if mode == ExecutionMode::Manual {
        bridge
            .confirm_manual(&turn.request_id, &turn.owner)
            .unwrap();
        assert_eq!(
            rpc(
                &client,
                bridge,
                "codex_turn_start",
                json!({"request_id":turn.turn_token})
            )
            .await["result"]["structuredContent"]["started"],
            true
        );
    }
    let mut args = json!({"wire_name":"read"});
    args[if mode == ExecutionMode::Manual {
        "request_id"
    } else {
        "turn_token"
    }] = json!(turn.turn_token);
    let url = format!("{}/mcp", bridge.base_url());
    let tool = tokio::spawn(async move {
        reqwest::Client::new().post(url).json(&json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"codex_tool_call","arguments":args}})).send().await.unwrap().json::<Value>().await.unwrap()
    });
    let BrowserRequest::ToolCall(call) = browser.recv().await.unwrap() else {
        panic!()
    };
    let call_id = call.call_id.clone();
    bridge.forward_native_tool(call).await.unwrap();
    let first = terminal(&response.text().await.unwrap());
    (turn, call_id, first, human, tool)
}
#[tokio::test]
async fn manual_mid_tool_checkpoint_completes_original_request_without_second_message() {
    let (bridge, mut browser) = start().await;
    let client = reqwest::Client::new();
    let (turn, call_id, _, human, tool) =
        boundary(&bridge, &mut browser, ExecutionMode::Manual).await;
    let canonical = json!({"content":[{"type":"text","text":"actual result"}],"_meta":{"receipt":9},"isError":false});
    let url = format!("{}/responses/compact", bridge.base_url());
    let pending = tokio::spawn(async move {
        reqwest::Client::new().post(url).json(&json!({"model":"fixture","client_metadata":meta(),"input":[human,{"type":"function_call_output","call_id":call_id,"output":canonical.to_string()}]})).send().await.unwrap()
    });
    let BrowserRequest::Compaction(mut request) = browser.recv().await.unwrap() else {
        panic!()
    };
    assert!(request.reuse_manual_response);
    assert_eq!(
        request.source_request_id.as_deref(),
        Some(turn.request_id.as_str())
    );
    let result = tool.await.unwrap();
    assert_eq!(result["result"]["content"][0]["text"], "actual result");
    assert_eq!(result["result"]["_meta"]["receipt"], 9);
    assert_eq!(result["result"]["isError"], false);
    assert!(
        result["result"]["content"][1]["text"]
            .as_str()
            .unwrap()
            .contains("codex_turn_complete")
    );
    let ack = rpc(
        &client,
        &bridge,
        "codex_turn_complete",
        json!({"request_id":turn.turn_token,"final_answer":"actual manual checkpoint"}),
    )
    .await;
    assert_eq!(ack["result"]["structuredContent"]["completed"], true);
    tokio::time::timeout(Duration::from_secs(1), request.accepted.wait_for(|v| *v))
        .await
        .unwrap()
        .unwrap();
    assert!(browser.try_recv().is_err());
    assert!(turn.cancellation.borrow().is_none());
    request.completion.send(Ok(())).unwrap();
    let result: Value = pending.await.unwrap().json().await.unwrap();
    assert!(result.to_string().contains("actual manual checkpoint"));
    let mut retired = turn.cancellation.clone();
    tokio::time::timeout(Duration::from_secs(1), retired.wait_for(|v| v.is_some()))
        .await
        .unwrap()
        .unwrap();
    bridge.close().await.unwrap();
}
#[tokio::test]
async fn invalid_compaction_leaves_owner_untouched_and_normal_final_remains_replayable() {
    let (bridge, mut browser) = start().await;
    let client = reqwest::Client::new();
    let (turn, call_id, first, human, tool) =
        boundary(&bridge, &mut browser, ExecutionMode::Automatic).await;
    let invalid = client
        .post(format!("{}/responses/compact", bridge.base_url()))
        .json(&json!({"model":"fixture","client_metadata":meta(),"input":[human.clone()]}))
        .send()
        .await
        .unwrap();
    assert_eq!(invalid.status(), 409);
    assert!(!tool.is_finished());
    assert!(browser.try_recv().is_err());
    assert!(bridge.authorize(&turn.turn_token, &turn.owner).is_ok());
    let result_item =
        json!({"type":"function_call_output","call_id":call_id,"output":"actual canonical result"});
    let body =
        json!({"model":"fixture","client_metadata":meta(),"input":[human,result_item.clone()]});
    let url = format!("{}/responses/compact", bridge.base_url());
    let pending = tokio::spawn(async move {
        reqwest::Client::new()
            .post(url)
            .json(&body)
            .send()
            .await
            .unwrap()
    });
    let BrowserRequest::Compaction(mut request) = browser.recv().await.unwrap() else {
        panic!()
    };
    assert_eq!(
        tool.await.unwrap()["result"]["content"][0]["text"],
        "actual canonical result"
    );
    turn.events
        .send(BrowserEvent::TextDelta("publishable ordinary final".into()))
        .await
        .unwrap();
    turn.events
        .send(BrowserEvent::Completed {
            output: vec![],
            usage: None,
        })
        .await
        .unwrap();
    tokio::time::timeout(
        Duration::from_secs(1),
        request.source_finished.wait_for(|v| *v),
    )
    .await
    .unwrap()
    .unwrap();
    bridge
        .confirm_compaction_source_settled(&request.request_id, &request.owner)
        .unwrap();
    assert_eq!(
        submit(&client, &bridge, &request, "checkpoint").await["result"]["structuredContent"]["submitted"],
        true
    );
    request.completion.send(Ok(())).unwrap();
    assert_eq!(pending.await.unwrap().status(), 200);
    let replay:Value=client.post(format!("{}/responses",bridge.base_url())).json(&json!({"model":"fixture","previous_response_id":first["id"],"client_metadata":meta(),"input":[result_item]})).send().await.unwrap().json().await.unwrap();
    assert_eq!(replay["status"], "completed");
    assert_eq!(
        replay["output"][0]["content"][0]["text"],
        "publishable ordinary final"
    );
    assert!(browser.try_recv().is_err());
    bridge.close().await.unwrap();
}

#[tokio::test]
async fn native_and_remote_checkpoint_responses_stream_and_retry_without_new_turn() {
    for native in [true, false] {
        let (bridge, mut browser) = start().await;
        let client = reqwest::Client::new();
        let (turn, call_id, _, human, tool) =
            boundary(&bridge, &mut browser, ExecutionMode::Automatic).await;
        let mut metadata = meta();
        if native {
            metadata["x-codex-turn-metadata"]["request_kind"] = json!("compaction");
            metadata["x-codex-turn-metadata"]["compaction"] =
                json!({"implementation":"responses","strategy":"memento"});
        }
        let mut input = vec![
            human,
            json!({"type":"function_call_output","call_id":call_id,"output":"actual result"}),
        ];
        if !native {
            input.push(json!({"type":"compaction_trigger"}));
        }
        let body =
            json!({"model":"fixture","stream":true,"client_metadata":metadata,"input":input});
        let response = client
            .post(format!("{}/responses", bridge.base_url()))
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let BrowserRequest::Compaction(mut request) = browser.recv().await.unwrap() else {
            panic!()
        };
        assert_eq!(
            tool.await.unwrap()["result"]["content"][0]["text"],
            "actual result"
        );
        turn.events
            .send(BrowserEvent::Completed {
                output: vec![],
                usage: None,
            })
            .await
            .unwrap();
        tokio::time::timeout(
            Duration::from_secs(1),
            request.source_finished.wait_for(|v| *v),
        )
        .await
        .unwrap()
        .unwrap();
        bridge
            .confirm_compaction_source_settled(&request.request_id, &request.owner)
            .unwrap();
        assert_eq!(
            submit(&client, &bridge, &request, "actual checkpoint").await["result"]["structuredContent"]
                ["submitted"],
            true
        );
        request.completion.send(Ok(())).unwrap();
        let first = terminal(&response.text().await.unwrap());
        assert_eq!(first["output"].as_array().unwrap().len(), 1);
        assert_eq!(
            first["output"][0]["type"],
            if native { "message" } else { "compaction" }
        );
        let replay = client
            .post(format!("{}/responses", bridge.base_url()))
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(
            terminal(&replay.text().await.unwrap())["output"][0][if native {
                "content"
            } else {
                "encrypted_content"
            }],
            first["output"][0][if native {
                "content"
            } else {
                "encrypted_content"
            }]
        );
        assert!(browser.try_recv().is_err());
        bridge.close().await.unwrap();
    }
}

#[tokio::test]
async fn scoped_control_rejects_wrong_handoff_owner_conflicting_summary_and_expiry() {
    let (bridge, mut browser) = BridgeServer::bind(
        "127.0.0.1:0".parse().unwrap(),
        BridgeConfig {
            turn_ttl: Duration::from_millis(150),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let client = reqwest::Client::new();
    let url = format!("{}/responses/compact", bridge.base_url());
    let pending = tokio::spawn(async move {
        reqwest::Client::new().post(url).json(&json!({"model":"fixture","client_metadata":meta(),"input":[{"role":"user","id":"h","content":"canonical task"}]})).send().await.unwrap()
    });
    let BrowserRequest::Compaction(request) = browser.recv().await.unwrap() else {
        panic!()
    };
    let wrong_owner = TurnOwner {
        thread_id: "other".into(),
        turn_id: "turn".into(),
    };
    assert!(
        bridge
            .confirm_compaction_source_settled(&request.request_id, &wrong_owner)
            .is_err()
    );
    bridge
        .confirm_compaction_source_settled(&request.request_id, &request.owner)
        .unwrap();
    assert!(rpc(&client,&bridge,"codex_tool_call",json!({"turn_token":request.control_token,"wire_name":"codex.control.compaction_handoff","arguments":{"handoff_id":"wrong","summary":"summary"}})).await["error"].is_object());
    assert!(rpc(&client,&bridge,"codex_tool_call",json!({"turn_token":request.control_token,"thread_id":"other","wire_name":"codex.control.compaction_handoff","arguments":{"handoff_id":request.handoff_id,"summary":"summary"}})).await["error"].is_object());
    assert!(
        submit(
            &client,
            &bridge,
            &request,
            "summary\nCODEX_LATEST_USER_PROMPT_JSON\n\"wrong task\""
        )
        .await["error"]
            .is_object()
    );
    assert!(!*request.accepted.borrow());
    tokio::time::sleep(Duration::from_millis(170)).await;
    assert!(submit(&client, &bridge, &request, "late summary").await["error"].is_object());
    assert_eq!(pending.await.unwrap().status(), 409);
    assert_eq!(bridge.counters().active_compactions, 1);
    request.completion.send(Ok(())).unwrap();
    bridge.close().await.unwrap();
    assert_eq!(bridge.counters().active_compactions, 0);
}
