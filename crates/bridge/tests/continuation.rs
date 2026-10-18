use cw_bridge::*;
use serde_json::{Value, json};
use std::time::Duration;

async fn start() -> (BridgeHandle, tokio::sync::mpsc::Receiver<BrowserRequest>) {
    BridgeServer::bind(
        "127.0.0.1:0".parse().unwrap(),
        BridgeConfig {
            turn_ttl: Duration::from_secs(5),
            ..Default::default()
        },
    )
    .await
    .unwrap()
}
fn metadata(owner: &str) -> Value {
    json!({"x-codex-turn-metadata":{"thread_id":owner,"turn_id":"turn"}})
}
fn terminal(body: &str) -> Value {
    body.lines()
        .filter_map(|l| l.strip_prefix("data: "))
        .filter_map(|s| serde_json::from_str::<Value>(s).ok())
        .filter(|v| v["type"] == "response.completed")
        .last()
        .unwrap()["response"]
        .clone()
}
#[tokio::test]
async fn outer_harness_roundtrip_function_and_custom_without_new_browser_prompt() {
    for custom in [false, true] {
        let (bridge, mut browser) = start().await;
        let client = reqwest::Client::new();
        let initial = json!({"model":"fixture","stream":true,"client_metadata":metadata("owner"),"input":"do work","tools":if custom {json!([{"type":"custom","name":"apply_patch"}])} else {json!([{"type":"namespace","name":"files","tools":[{"type":"function","name":"read","parameters":{"type":"object"}}]}])}});
        let response = client
            .post(format!("{}/v1/responses", bridge.base_url()))
            .json(&initial)
            .send()
            .await
            .unwrap();
        let BrowserRequest::Turn(turn) = browser.recv().await.unwrap() else {
            panic!()
        };
        let endpoint = format!("{}/mcp", bridge.base_url());
        let args = if custom {
            json!({"turn_token":turn.turn_token,"wire_name":"apply_patch","input":"*** Begin Patch\n*** End Patch"})
        } else {
            json!({"turn_token":turn.turn_token,"wire_name":"files__read","arguments":{"path":"fixture"}})
        };
        let pending = tokio::spawn(async move {
            reqwest::Client::new().post(endpoint).json(&json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"codex_tool_call","arguments":args}})).send().await.unwrap().json::<Value>().await.unwrap()
        });
        let BrowserRequest::ToolCall(call) = browser.recv().await.unwrap() else {
            panic!()
        };
        let call_id = call.call_id.clone();
        bridge.handoff_tool(call).await.unwrap();
        let first = terminal(&response.text().await.unwrap());
        assert_eq!(first["end_turn"], false);
        assert_eq!(first["output"][0]["call_id"], call_id);
        assert_eq!(
            first["output"][0]["type"],
            if custom {
                "custom_tool_call"
            } else {
                "function_call"
            }
        );
        if !custom {
            assert_eq!(first["output"][0]["name"], "read");
            assert_eq!(first["output"][0]["namespace"], "files");
        }
        assert!(!pending.is_finished());
        assert!(bridge.authorize(&turn.turn_token, &turn.owner).is_ok());
        let resumed=client.post(format!("{}/v1/responses",bridge.base_url())).json(&json!({"model":"fixture","stream":true,"client_metadata":metadata("owner"),"previous_response_id":first["id"],"input":[{"type":if custom {"custom_tool_call_output"} else {"function_call_output"},"call_id":call_id,"output":[{"type":"input_text","text":"{\"real\":true}"},{"type":"input_image","image_url":"data:image/png;base64,AA"}]}]})).send().await.unwrap();
        assert_eq!(resumed.status(), 200);
        let result = pending.await.unwrap();
        assert_eq!(result["result"]["structuredContent"], json!({"real":true}));
        assert_eq!(
            result["result"]["content"][1],
            json!({"type":"image","data":"AA","mimeType":"image/png"})
        );
        assert!(browser.try_recv().is_err());
        turn.events
            .send(BrowserEvent::TextDelta("actual final".into()))
            .await
            .unwrap();
        turn.events
            .send(BrowserEvent::Completed {
                output: vec![],
                usage: None,
            })
            .await
            .unwrap();
        let final_response = terminal(&resumed.text().await.unwrap());
        assert_eq!(final_response["end_turn"], true);
        assert_eq!(bridge.counters().active_turns, 0);
        bridge.close().await.unwrap();
    }
}

#[tokio::test]
async fn native_text_compaction_produces_assistant_summary_not_encrypted_item() {
    let (bridge, mut browser) = start().await;
    let response=reqwest::Client::new().post(format!("{}/v1/responses",bridge.base_url())).json(&json!({"model":"fixture","stream":true,"client_metadata":{"x-codex-turn-metadata":{"request_kind":"compaction","compaction":{"implementation":"responses","strategy":"memento"}}},"input":[{"role":"user","content":"task history"}]})).send().await.unwrap();
    assert_eq!(response.status(), 200);
    let BrowserRequest::Turn(turn) = browser.recv().await.unwrap() else {
        panic!()
    };
    assert_eq!(turn.task.compaction, CompactionMode::NativeText);
    assert!(turn.task.prompt.contains("CONTEXT CHECKPOINT COMPACTION"));
    turn.events
        .send(BrowserEvent::TextDelta("real summary".into()))
        .await
        .unwrap();
    turn.events
        .send(BrowserEvent::Completed {
            output: vec![],
            usage: None,
        })
        .await
        .unwrap();
    let final_response = terminal(&response.text().await.unwrap());
    assert_eq!(final_response["output"].as_array().unwrap().len(), 1);
    assert_eq!(final_response["output"][0]["type"], "message");
    assert_eq!(
        final_response["output"][0]["content"][0]["text"],
        "real summary"
    );
    bridge.close().await.unwrap();
}

#[tokio::test]
async fn catalog_is_original_native_plus_observed_routes_and_mode_switch_is_idle_only() {
    let (bridge, mut browser) = start().await;
    let client = reqwest::Client::new();
    let missing = client
        .get(format!("{}/models", bridge.base_url()))
        .send()
        .await
        .unwrap();
    assert_eq!(missing.status(), 503);
    let native = json!({"models":[{"slug":"native-fixture","visibility":"list","tool_mode":"function","supported_reasoning_levels":[{"effort":"high","description":"official","unknown":7}],"multi_agent_version":"v2","context_window":12345,"unknown_native_field":{"keep":true}}],"unknown_catalog":"original"});
    bridge.set_native_model_catalog(native.clone()).unwrap();
    bridge
        .set_account_capabilities(cw_contracts::models::ChatGptWebAccountCapabilities {
            sol_available: true,
            pro_available: false,
            extra_high_available: Some(false),
            ..Default::default()
        })
        .unwrap();
    assert!(bridge.catalog_status().capabilities_observed);
    assert!(!bridge.catalog_status().verified);
    assert!(!bridge.catalog_status().augmentation_succeeded);
    let catalog: Value = client
        .get(format!("{}/v1/models", bridge.base_url()))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(bridge.catalog_status().augmentation_succeeded);
    assert!(!bridge.catalog_status().native_catalog_fetched);
    assert!(!bridge.catalog_status().verified);
    assert_eq!(bridge.catalog_status().last_http_status, Some(200));
    assert!(bridge.catalog_status().routed_models > 0);
    assert_eq!(catalog["models"][0], native["models"][0]);
    assert_eq!(catalog["unknown_catalog"], "original");
    assert!(
        catalog["models"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["slug"] == "chatgpt-web/gpt-6-sol")
    );
    assert!(
        !catalog["models"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["slug"] == "chatgpt-web/gpt-6-pro")
    );
    let addr = bridge.local_addr;
    bridge.set_mode(ExecutionMode::Manual).unwrap();
    assert!(!bridge.catalog_status().augmentation_succeeded);
    assert!(!bridge.catalog_status().verified);
    assert_eq!(bridge.local_addr, addr);
    let manual: Value = client
        .get(format!("{}/models", bridge.base_url()))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        manual["models"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["slug"] == "chatgpt-web/zero-risk")
    );
    assert!(
        !manual["models"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["slug"] == "chatgpt-web/gpt-6-sol")
    );
    let response = client
        .post(format!("{}/v1/responses", bridge.base_url()))
        .json(&json!({"model":"chatgpt-web/zero-risk","stream":true,"input":"fixture"}))
        .send()
        .await
        .unwrap();
    let BrowserRequest::Turn(turn) = browser.recv().await.unwrap() else {
        panic!()
    };
    assert!(bridge.set_mode(ExecutionMode::Automatic).is_err());
    bridge.cancel(&turn.request_id);
    response.text().await.unwrap();
    bridge.set_mode(ExecutionMode::Automatic).unwrap();
    assert_eq!(bridge.local_addr, addr);
    assert!(
        bridge
            .set_native_passthrough_base("http://127.0.0.1:1234")
            .is_err()
    );
    assert!(
        bridge
            .set_native_passthrough_base("https://chatgpt.com.evil.invalid/backend-api/codex")
            .is_err()
    );
    bridge
        .set_native_passthrough_base("https://chatgpt.com/backend-api/codex")
        .unwrap();
    let refused = client
        .post(format!("{}/v1/responses", bridge.base_url()))
        .json(&json!({"model":"native-fixture","input":"fixture"}))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), 401);
    assert!(browser.try_recv().is_err());
    bridge.close().await.unwrap();
}

#[tokio::test]
async fn remote_v2_compaction_is_one_ocx1_item_and_replay_is_readable() {
    use base64::Engine;
    let (bridge, mut browser) = start().await;
    let response=reqwest::Client::new().post(format!("{}/v1/responses",bridge.base_url())).json(&json!({"model":"fixture","stream":true,"input":[{"role":"user","content":"history"},{"type":"compaction_trigger"}]})).send().await.unwrap();
    let BrowserRequest::Turn(turn) = browser.recv().await.unwrap() else {
        panic!()
    };
    turn.events
        .send(BrowserEvent::TextDelta("real checkpoint".into()))
        .await
        .unwrap();
    turn.events
        .send(BrowserEvent::Completed {
            output: vec![],
            usage: None,
        })
        .await
        .unwrap();
    let final_response = terminal(&response.text().await.unwrap());
    assert_eq!(final_response["output"].as_array().unwrap().len(), 1);
    let item = final_response["output"][0].clone();
    assert_eq!(item["type"], "compaction");
    let encoded = item["encrypted_content"]
        .as_str()
        .unwrap()
        .strip_prefix("ocx1:")
        .unwrap();
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .unwrap(),
        b"real checkpoint"
    );
    let task = compile_request(json!({"model":"fixture","stream":true,"input":[item]})).unwrap();
    assert!(task.prompt.contains("real checkpoint"));
    assert_eq!(task.compaction, CompactionMode::None);
    bridge.close().await.unwrap();
}

#[test]
fn pinned_prompt_contract_contains_exact_source_literals() {
    let source = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../vendor/upstream/src/adapters/chatgpt-web/prompt.ts"
    ))
    .unwrap();
    let fixture: Value =
        serde_json::from_str(include_str!("../src/fixtures/prompt-contract.json")).unwrap();
    for key in ["shared", "tools", "compaction"] {
        for literal in fixture[key].as_array().unwrap() {
            assert!(
                source.contains(literal.as_str().unwrap()),
                "missing upstream literal in {key}"
            );
        }
    }
    let task=compile_request(json!({"model":"fixture","stream":true,"input":[{"role":"system","content":"sys"},{"role":"developer","content":"dev"},{"type":"agent_message","author":"a","recipient":"b","content":"peer"},{"type":"function_call","call_id":"c","name":"read","namespace":"files","arguments":"{}"},{"type":"function_call_output","call_id":"c","output":"real"}]})).unwrap();
    assert_eq!(task.context["version"], 3);
    assert_eq!(task.context["system"], json!(["sys"]));
    assert_eq!(task.context["messages"][0]["role"], "developer");
    assert_eq!(task.context["messages"][1]["role"], "agent_message");
    assert_eq!(task.context["messages"][2]["role"], "assistant");
    assert_eq!(task.context["messages"][3]["role"], "tool_result");
    assert_eq!(task.context["messages"][3]["tool_namespace"], "files");
}

#[tokio::test]
async fn parallel_batch_replay_partial_duplicate_and_cross_owner_are_safe() {
    let (bridge, mut browser) = start().await;
    let client = reqwest::Client::new();
    let first=client.post(format!("{}/responses",bridge.base_url())).json(&json!({"model":"fixture","stream":true,"client_metadata":metadata("owner"),"input":"work","tools":[{"type":"function","name":"read"}]})).send().await.unwrap();
    let BrowserRequest::Turn(turn) = browser.recv().await.unwrap() else {
        panic!()
    };
    let mut tasks = vec![];
    for n in 0..2 {
        let endpoint = format!("{}/mcp", bridge.base_url());
        let token = turn.turn_token.clone();
        tasks.push(tokio::spawn(async move{reqwest::Client::new().post(endpoint).json(&json!({"jsonrpc":"2.0","id":n,"method":"tools/call","params":{"name":"codex_tool_call","arguments":{"turn_token":token,"wire_name":"read","arguments":{"n":n}}}})).send().await.unwrap().json::<Value>().await.unwrap()}));
    }
    let mut calls = vec![];
    for _ in 0..2 {
        let BrowserRequest::ToolCall(call) = browser.recv().await.unwrap() else {
            panic!()
        };
        calls.push(call);
    }
    let ids: Vec<_> = calls.iter().map(|c| c.call_id.clone()).collect();
    bridge.handoff_tools(calls).await.unwrap();
    let first = terminal(&first.text().await.unwrap());
    assert_eq!(first["output"].as_array().unwrap().len(), 2);
    let replay=client.post(format!("{}/responses",bridge.base_url())).json(&json!({"model":"fixture","stream":true,"client_metadata":metadata("owner"),"previous_response_id":first["id"],"input":[]})).send().await.unwrap();
    let replay = terminal(&replay.text().await.unwrap());
    assert_eq!(replay["output"], first["output"]);
    assert!(tasks.iter().all(|t| !t.is_finished()));
    let result = |id: &str| json!({"type":"function_call_output","call_id":id,"output":"actual"});
    for (owner, outputs) in [
        ("other", vec![result(&ids[0]), result(&ids[1])]),
        ("owner", vec![result(&ids[0])]),
        ("owner", vec![result(&ids[0]), result(&ids[0])]),
    ] {
        let response=client.post(format!("{}/responses",bridge.base_url())).json(&json!({"model":"fixture","stream":true,"client_metadata":metadata(owner),"previous_response_id":replay["id"],"input":outputs})).send().await.unwrap();
        assert_eq!(response.status(), 400);
    }
    assert!(browser.try_recv().is_err());
    assert!(tasks.iter().all(|t| !t.is_finished()));
    // Full history/call correlation works even without previous_response_id.
    let resumed=client.post(format!("{}/responses",bridge.base_url())).json(&json!({"model":"fixture","stream":true,"client_metadata":metadata("owner"),"input":[result(&ids[0]),result(&ids[1])]})).send().await.unwrap();
    assert_eq!(resumed.status(), 200);
    for task in tasks {
        assert_eq!(
            task.await.unwrap()["result"]["content"][0]["text"],
            "actual"
        );
    }
    turn.events
        .send(BrowserEvent::Completed {
            output: vec![],
            usage: None,
        })
        .await
        .unwrap();
    resumed.text().await.unwrap();
    bridge.close().await.unwrap();
}

#[tokio::test]
async fn manual_connector_start_and_complete_use_mcp_without_browser_dom_success() {
    let (bridge, mut browser) = start().await;
    bridge.set_mode(ExecutionMode::Manual).unwrap();
    let client = reqwest::Client::new();
    let response = client
        .post(format!("{}/responses", bridge.base_url()))
        .json(&json!({"model":"fixture","stream":true,"input":"manual"}))
        .send()
        .await
        .unwrap();
    let BrowserRequest::Turn(turn) = browser.recv().await.unwrap() else {
        panic!()
    };
    let rpc = |name: &str, args: Value| json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":name,"arguments":args}});
    let endpoint = format!("{}/mcp", bridge.base_url());
    let start: Value = client
        .post(&endpoint)
        .json(&rpc(
            "codex_turn_start",
            json!({"request_id":turn.turn_token}),
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(start["result"]["structuredContent"]["started"], true);
    assert!(bridge.authorize(&turn.turn_token, &turn.owner).is_err());
    bridge
        .confirm_manual(&turn.request_id, &turn.owner)
        .unwrap();
    assert!(bridge.authorize(&turn.turn_token, &turn.owner).is_ok());
    let duplicate: Value = client
        .post(&endpoint)
        .json(&rpc(
            "codex_turn_start",
            json!({"request_id":turn.turn_token}),
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(duplicate["result"]["structuredContent"]["duplicate"], true);
    let complete: Value = client
        .post(&endpoint)
        .json(&rpc(
            "codex_turn_complete",
            json!({"request_id":turn.turn_token,"final_answer":"actual connector answer"}),
        ))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(complete["result"]["structuredContent"]["completed"], true);
    let completed = terminal(&response.text().await.unwrap());
    assert_eq!(
        completed["output"][0]["content"][0]["text"],
        "actual connector answer"
    );
    let mut retired = turn.cancellation.clone();
    tokio::time::timeout(
        Duration::from_secs(1),
        retired.wait_for(|value| value.is_some()),
    )
    .await
    .unwrap()
    .unwrap();

    assert!(browser.try_recv().is_err());
    bridge.close().await.unwrap();
}

#[test]
fn prompt_scrubs_retired_capabilities_and_limits_images_without_mutating_raw() {
    let old = format!("turn_{}", "A".repeat(32));
    let raw = json!({"model":"fixture","stream":true,"input":[{"role":"user","content":format!("earlier {old} call_{}","B".repeat(32))}]});
    let task = compile_request(raw.clone()).unwrap();
    assert_eq!(task.raw, raw);
    assert!(!task.prompt.contains(&old));
    assert!(task.prompt.contains("[retired turn handle]"));
    assert!(task.prompt.contains("call_BBBBB"));
    let blocks:Vec<_>=(0..12).map(|i|json!({"type":"input_image","image_url":format!("https://fixture.invalid/{i}.png")})).collect();
    let raw = json!({"model":"fixture","stream":true,"input":[{"role":"user","content":blocks}]});
    let task = compile_request(raw.clone()).unwrap();
    assert_eq!(task.raw, raw);
    assert_eq!(task.images.len(), 10);
    assert_eq!(task.images[0].image_url, "https://fixture.invalid/2.png");
    assert_eq!(task.images[0].r#ref, "codex-input-image-1");
    assert!(task.prompt.contains("older image not attached"));
    assert!(compile_request(json!({"model":"fixture","stream":true,"input":[{"type":"agent_message","content":[{"type":"encrypted_content","encrypted_content":"opaque"}]}]})).is_err());
}

#[tokio::test]
async fn forward_native_tool_preserves_actual_mcp_result_metadata() {
    let (bridge, mut browser) = start().await;
    let client = reqwest::Client::new();
    let response = client
        .post(format!("{}/responses", bridge.base_url()))
        .json(&json!({"model":"fixture","stream":true,"tools":[{"type":"function","name":"native_fixture"}]}))
        .send()
        .await
        .unwrap();
    let BrowserRequest::Turn(turn) = browser.recv().await.unwrap() else {
        panic!()
    };
    let token = turn.turn_token.clone();
    let endpoint = format!("{}/mcp", bridge.base_url());
    let pending = tokio::spawn(async move {
        reqwest::Client::new().post(endpoint).json(&json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"codex_tool_call","arguments":{"turn_token":token,"wire_name":"native_fixture"}}})).send().await.unwrap().json::<Value>().await.unwrap()
    });
    let BrowserRequest::ToolCall(call) = browser.recv().await.unwrap() else {
        panic!()
    };
    let call_id = call.call_id.clone();
    bridge.forward_native_tool(call).await.unwrap();
    let first = terminal(&response.text().await.unwrap());
    let actual = json!({"content":[{"type":"text","text":"actual tool error"},{"type":"image","data":"AA","mimeType":"image/png"}],"structuredContent":{"receipt":"outer-harness"},"_meta":{"resource":"ui://fixture","private":{"keep":true}},"isError":true,"unknown_native_field":[1,2]});
    let resumed=client.post(format!("{}/responses",bridge.base_url())).json(&json!({"model":"fixture","stream":true,"previous_response_id":first["id"],"input":[{"type":"function_call_output","call_id":call_id,"output":actual.to_string()}]})).send().await.unwrap();
    assert_eq!(resumed.status(), 200);
    assert_eq!(pending.await.unwrap()["result"], actual);
    turn.events
        .send(BrowserEvent::Completed {
            output: vec![],
            usage: None,
        })
        .await
        .unwrap();
    resumed.text().await.unwrap();
    bridge.close().await.unwrap();
}
