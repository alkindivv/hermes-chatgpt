use cw_bridge::*;
use serde_json::{Value, json};
async fn fixture() -> (BridgeHandle, tokio::sync::mpsc::Receiver<BrowserRequest>) {
    BridgeServer::bind("127.0.0.1:0".parse().unwrap(), BridgeConfig::default())
        .await
        .unwrap()
}
#[tokio::test]
async fn scoped_notification_cancellation_and_logout() {
    let (bridge, mut browser) = fixture().await;
    let client = reqwest::Client::new();
    bridge
        .set_native_model_catalog(json!({"models":[]}))
        .unwrap();
    let response=client.post(format!("{}/responses",bridge.base_url())).json(&json!({"model":"fixture","stream":true,"input":"x","tools":[{"type":"function","name":"exec_command","parameters":{"properties":{"cmd":{},"sandbox_permissions":{},"justification":{}}}}]})).send().await.unwrap();
    let BrowserRequest::Turn(turn) = browser.recv().await.unwrap() else {
        panic!()
    };
    assert!(bridge.clear_account_capabilities().is_err());
    let endpoint = format!("{}/mcp", bridge.base_url());
    let request = json!({"jsonrpc":"2.0","id":9,"method":"tools/call","params":{"name":"codex_exec","arguments":{"turn_token":turn.turn_token,"cmd":"fixture-only","sandbox_permissions":"require_escalated","justification":"outer approval"}}});
    let rpc = tokio::spawn({
        let endpoint = endpoint.clone();
        async move {
            reqwest::Client::new()
                .post(endpoint)
                .json(&request)
                .send()
                .await
                .unwrap()
                .json::<Value>()
                .await
                .unwrap()
        }
    });
    let BrowserRequest::ToolCall(mut call) = browser.recv().await.unwrap() else {
        panic!()
    };
    assert_eq!(call.wire_name, "exec_command");
    assert_eq!(
        call.arguments.as_ref().unwrap()["sandbox_permissions"],
        "require_escalated"
    );
    let wrong = json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":9,"turn_token":"wrong-owner"}});
    assert_eq!(
        client
            .post(&endpoint)
            .json(&wrong)
            .send()
            .await
            .unwrap()
            .status(),
        202
    );
    assert!(!rpc.is_finished());
    assert!(call.cancellation.borrow().is_none());
    let correct = json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":9,"turn_token":turn.turn_token}});
    client.post(&endpoint).json(&correct).send().await.unwrap();
    let result = rpc.await.unwrap();
    assert!(
        result["error"]["message"]
            .as_str()
            .unwrap()
            .contains("cancelled")
    );
    call.cancellation.wait_for(|v| v.is_some()).await.unwrap();
    assert!(bridge.forward_native_tool(call).await.is_err());
    assert!(bridge.authorize(&turn.turn_token, &turn.owner).is_ok());
    assert_eq!(bridge.counters().pending_tool_calls, 0);
    let ledger = bridge.mcp_observations();
    assert!(ledger.iter().any(|e| e.event == "cancellation_observed"));
    assert!(ledger.iter().any(|e| e.event == "reply_prepared"));
    assert!(
        !serde_json::to_string(&ledger)
            .unwrap()
            .contains("fixture-only")
    );
    bridge.cancel(&turn.request_id);
    response.text().await.unwrap();
    bridge.clear_account_capabilities().unwrap();
    let status = bridge.catalog_status();
    assert!(!status.verified && !status.native_catalog_available && !status.capabilities_observed);
    bridge.close().await.unwrap();
}
#[tokio::test]
async fn alias_patch_and_wait_inventory_are_actual_registry_only() {
    let (bridge, mut browser) = fixture().await;
    let client = reqwest::Client::new();
    let response=client.post(format!("{}/responses",bridge.base_url())).json(&json!({"model":"fixture","stream":true,"input":"x","tools":[{"type":"custom","name":"apply_patch"},{"type":"namespace","name":"multi_agent_v2","tools":[{"type":"function","name":"wait_agent","parameters":{"properties":{"timeout_ms":{"default":1000}},"required":["ids"]}}]}]})).send().await.unwrap();
    let BrowserRequest::Turn(turn) = browser.recv().await.unwrap() else {
        panic!()
    };
    let endpoint = format!("{}/mcp", bridge.base_url());
    let inventory:Value=client.post(&endpoint).json(&json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"codex_tool_inventory","arguments":{"turn_token":turn.turn_token}}})).send().await.unwrap().json().await.unwrap();
    let wait = &inventory["result"]["structuredContent"]["tools"][1];
    assert_eq!(
        wait["parameters"]["properties"]["timeout_ms"]["const"],
        30000
    );
    assert!(
        wait["parameters"]["properties"]["timeout_ms"]
            .get("default")
            .is_none()
    );
    let invalid:Value=client.post(&endpoint).json(&json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"codex_tool_call","arguments":{"turn_token":turn.turn_token,"wire_name":"multi_agent_v2__wait_agent","arguments":{"timeout_ms":1000}}}})).send().await.unwrap().json().await.unwrap();
    assert!(
        invalid["error"]["message"]
            .as_str()
            .unwrap()
            .contains("30000")
    );
    assert!(browser.try_recv().is_err());
    let token = turn.turn_token.clone();
    let rpc = tokio::spawn(async move {
        reqwest::Client::new().post(endpoint).json(&json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"codex_apply_patch","arguments":{"turn_token":token,"patch":"real-patch-text"}}})).send().await.unwrap().json::<Value>().await.unwrap()
    });
    let BrowserRequest::ToolCall(call) = browser.recv().await.unwrap() else {
        panic!()
    };
    assert_eq!(call.input.as_deref(), Some("real-patch-text"));
    assert!(call.arguments.is_none());
    call.result
        .send(Ok(json!({"content":[],"_meta":{"receipt":"actual"}})))
        .unwrap();
    assert_eq!(rpc.await.unwrap()["result"]["_meta"]["receipt"], "actual");
    bridge.cancel(&turn.request_id);
    response.text().await.unwrap();
    bridge.close().await.unwrap();
}

#[tokio::test]
async fn duplicate_rpc_ids_are_uncorrelated_and_cannot_cancel_either_call() {
    let (bridge, mut browser) = fixture().await;
    let client = reqwest::Client::new();
    let response=client.post(format!("{}/responses",bridge.base_url())).json(&json!({"model":"fixture","stream":true,"input":"x","tools":[{"type":"function","name":"read"}]})).send().await.unwrap();
    let BrowserRequest::Turn(turn) = browser.recv().await.unwrap() else {
        panic!()
    };
    let endpoint = format!("{}/mcp", bridge.base_url());
    let request = json!({"jsonrpc":"2.0","id":"same","method":"tools/call","params":{"name":"codex_tool_call","arguments":{"turn_token":turn.turn_token,"wire_name":"read","arguments":{}}}});
    let mut pending = Vec::new();
    let mut calls = Vec::new();
    for _ in 0..2 {
        pending.push(tokio::spawn({
            let endpoint = endpoint.clone();
            let request = request.clone();
            async move {
                reqwest::Client::new()
                    .post(endpoint)
                    .json(&request)
                    .send()
                    .await
                    .unwrap()
                    .json::<Value>()
                    .await
                    .unwrap()
            }
        }));
        let BrowserRequest::ToolCall(call) = browser.recv().await.unwrap() else {
            panic!()
        };
        calls.push(call);
    }
    client.post(&endpoint).json(&json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":"same","turn_token":turn.turn_token}})).send().await.unwrap();
    assert!(calls.iter().all(|c| c.cancellation.borrow().is_none()));
    for call in calls {
        call.result
            .send(Ok(json!({"content":[],"isError":false})))
            .unwrap();
    }
    for task in pending {
        assert!(task.await.unwrap()["result"]["content"].is_array());
    }
    let ledger = bridge.mcp_observations();
    assert!(
        ledger
            .iter()
            .any(|e| e.event == "uncorrelated_duplicate_id")
    );
    assert!(
        ledger
            .iter()
            .any(|e| e.event == "cancellation_uncorrelated")
    );
    assert!(!ledger.iter().any(|e| e.event == "reply_prepared"));
    bridge.cancel(&turn.request_id);
    response.text().await.unwrap();
    bridge.clear_account_capabilities().unwrap();
    bridge.close().await.unwrap();
}

#[tokio::test]
async fn cancellation_after_outer_handoff_consumes_real_continuation_without_new_prompt() {
    let (bridge, mut browser) = fixture().await;
    let client = reqwest::Client::new();
    let response=client.post(format!("{}/responses",bridge.base_url())).json(&json!({"model":"fixture","stream":true,"input":"x","tools":[{"type":"function","name":"read"}]})).send().await.unwrap();
    let BrowserRequest::Turn(turn) = browser.recv().await.unwrap() else {
        panic!()
    };
    let endpoint = format!("{}/mcp", bridge.base_url());
    let token = turn.turn_token.clone();
    let rpc = tokio::spawn({
        let endpoint = endpoint.clone();
        async move {
            reqwest::Client::new().post(endpoint).json(&json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"codex_tool_call","arguments":{"turn_token":token,"wire_name":"read","arguments":{}}}})).send().await.unwrap().json::<Value>().await.unwrap()
        }
    });
    let BrowserRequest::ToolCall(call) = browser.recv().await.unwrap() else {
        panic!()
    };
    let call_id = call.call_id.clone();
    bridge.forward_native_tool(call).await.unwrap();
    let body = response.text().await.unwrap();
    let first = body
        .lines()
        .filter_map(|l| l.strip_prefix("data: "))
        .filter_map(|s| serde_json::from_str::<Value>(s).ok())
        .find(|v| v["type"] == "response.completed")
        .unwrap()["response"]
        .clone();
    client.post(&endpoint).json(&json!({"jsonrpc":"2.0","method":"notifications/cancelled","params":{"requestId":1,"turn_token":turn.turn_token}})).send().await.unwrap();
    assert!(rpc.await.unwrap()["error"].is_object());
    let resumed=client.post(format!("{}/responses",bridge.base_url())).json(&json!({"model":"fixture","stream":true,"previous_response_id":first["id"],"input":[{"type":"function_call_output","call_id":call_id,"output":"actual outer receipt"}]})).send().await.unwrap();
    assert_eq!(resumed.status(), 200);
    assert!(browser.try_recv().is_err());
    assert!(bridge.authorize(&turn.turn_token, &turn.owner).is_ok());
    turn.events
        .send(BrowserEvent::Completed {
            output: vec![],
            usage: None,
        })
        .await
        .unwrap();
    assert!(resumed.text().await.unwrap().contains("response.completed"));
    bridge.close().await.unwrap();
}
