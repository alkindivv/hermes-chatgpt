use cw_browser::{BrowserEngine, Cancellation, CapabilityOptions, Error, InteractionMode};
use cw_contracts::limits::Plan;
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{net::TcpListener, task::JoinHandle};
#[derive(Clone, Copy)]
enum Scenario {
    Sol,
    Luna,
    ChangingAccount,
    Unproven,
    Loading,
}
struct Mock {
    url: String,
    log: Arc<Mutex<Vec<Value>>>,
    task: JoinHandle<()>,
}
impl Drop for Mock {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn mock(scenario: Scenario) -> Mock {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "ws://127.0.0.1:{}/devtools/page/owned-only",
        listener.local_addr().unwrap().port()
    );
    let log = Arc::new(Mutex::new(Vec::new()));
    let recorded = log.clone();
    let task = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(socket).await.unwrap();
        let mut accounts = 0;
        let mut readiness = 0;
        while let Some(Ok(message)) = ws.next().await {
            let request: Value = serde_json::from_slice(&message.into_data()).unwrap();
            recorded.lock().unwrap().push(request.clone());
            let method = request["method"].as_str().unwrap();
            if method == "Page.navigate" {
                ws.send(
                    json!({"id":request["id"],"result":{"frameId":"ours","loaderId":"new-loader"}})
                        .to_string()
                        .into(),
                )
                .await
                .unwrap();
                // An unrelated frame's load must not release navigation readiness.
                ws.send(json!({"method":"Page.lifecycleEvent","params":{"name":"load","frameId":"other","loaderId":"new-loader"}}).to_string().into()).await.unwrap();
                tokio::time::sleep(Duration::from_millis(35)).await;
                ws.send(json!({"method":"Page.lifecycleEvent","params":{"name":"load","frameId":"ours","loaderId":"new-loader"}}).to_string().into()).await.unwrap();
                continue;
            }
            let result = if method == "Runtime.evaluate" {
                let expr = request["params"]["expression"].as_str().unwrap();
                let value = if expr.contains(")(\"ready\",") {
                    readiness += 1;
                    json!(!matches!(scenario, Scenario::Loading) && readiness > 1)
                } else if expr.contains(")(\"account\",") {
                    accounts += 1;
                    json!({"user_id":"server-user","account_id":if matches!(scenario,Scenario::ChangingAccount)&&accounts>1{"switched"}else{"same"},"plan_type":"prolite","structure":"personal","needs_attention":false,"server_authenticated":!matches!(scenario,Scenario::Unproven)})
                } else if expr.contains(")(\"capabilityState\",") {
                    json!({"composer_ready":true,"document_ready":true,"selector_count":if matches!(scenario,Scenario::Luna){0}else{1},"think_available":matches!(scenario,Scenario::Luna)})
                } else if expr.contains(")(\"capabilityPicker\",") {
                    json!({"available":[true,true,true,true,false],"families":["5.6"]})
                } else if expr.contains("__CW_BROWSER_ABORT_GENERATION__=(") {
                    json!(true)
                } else {
                    panic!("unexpected page operation")
                };
                json!({"result":{"value":value}})
            } else {
                json!({})
            };
            if ws
                .send(
                    json!({"id":request["id"],"result":result})
                        .to_string()
                        .into(),
                )
                .await
                .is_err()
            {
                break;
            }
        }
    });
    Mock { url, log, task }
}
fn options() -> CapabilityOptions {
    CapabilityOptions {
        selector_timeout: Duration::from_millis(250),
        stable_absence: Duration::from_millis(100),
    }
}
#[tokio::test]
async fn account_digest_plan_and_exact_available_routes_require_evidence() {
    let mock = mock(Scenario::Sol).await;
    let engine = BrowserEngine::connect(&mock.url).await.unwrap();
    let inspection = engine
        .inspect_capabilities_with_options(
            InteractionMode::Automatic,
            options(),
            Cancellation::default(),
        )
        .await
        .unwrap();
    assert_eq!(inspection.account.limits_plan, Plan::Pro100);
    assert_eq!(inspection.account.account_key.len(), 64);
    assert!(!inspection.capabilities.pro_available);
    assert!(
        inspection
            .routes
            .iter()
            .any(|r| r.route.slug == "chatgpt-web/gpt-5.6-sol")
    );
    assert!(
        !inspection
            .routes
            .iter()
            .any(|r| r.route.slug.contains("gpt-6") || r.route.requires_pro)
    );
    assert!(
        inspection
            .routes
            .iter()
            .any(|r| r.route.slug == "chatgpt-web/high")
    );
}
#[tokio::test]
async fn stable_full_budget_absence_is_required_for_luna() {
    let mock = mock(Scenario::Luna).await;
    let engine = BrowserEngine::connect(&mock.url).await.unwrap();
    let started = tokio::time::Instant::now();
    let inspection = engine
        .inspect_capabilities_with_options(
            InteractionMode::Automatic,
            options(),
            Cancellation::default(),
        )
        .await
        .unwrap();
    assert!(started.elapsed() >= options().selector_timeout);
    assert!(!inspection.capabilities.sol_available);
    assert!(
        inspection
            .routes
            .iter()
            .any(|r| r.route.slug == "chatgpt-web/think")
    );
    assert!(
        inspection
            .routes
            .iter()
            .all(|r| r.route.backend_model == cw_contracts::models::BackendModel::Luna)
    );
}
#[tokio::test]
async fn changed_or_unproven_account_never_becomes_capability_success() {
    for scenario in [Scenario::ChangingAccount, Scenario::Unproven] {
        let mock = mock(scenario).await;
        let engine = BrowserEngine::connect(&mock.url).await.unwrap();
        assert!(
            engine
                .inspect_capabilities_with_options(
                    InteractionMode::Automatic,
                    options(),
                    Cancellation::default()
                )
                .await
                .is_err()
        );
    }
}
#[tokio::test]
async fn manual_inspection_and_host_zoom_issue_no_page_requests() {
    let mock = mock(Scenario::Sol).await;
    let engine = BrowserEngine::connect(&mock.url).await.unwrap();
    assert!(
        engine
            .inspect_capabilities(InteractionMode::Manual, Cancellation::default())
            .await
            .is_err()
    );
    assert!(matches!(
        engine
            .inspect_account(InteractionMode::Manual, Cancellation::default())
            .await,
        Err(Error::ManualDomForbidden)
    ));
    assert!(matches!(
        engine
            .inspect_auth_for_mode(InteractionMode::Manual, Cancellation::default())
            .await,
        Err(Error::ManualDomForbidden)
    ));
    assert!(
        engine
            .inspect_capabilities_with_options(
                InteractionMode::Automatic,
                CapabilityOptions {
                    selector_timeout: Duration::ZERO,
                    stable_absence: Duration::ZERO
                },
                Cancellation::default()
            )
            .await
            .is_err()
    );
    let zoom = engine.zoom(1.2).unwrap();
    assert!((zoom.cef_zoom_level - 1.0).abs() < 1e-9);
    assert_eq!(engine.zoom(1.0).unwrap().cef_zoom_level, 0.0);
    assert!(engine.zoom(f64::NAN).is_err());
    assert!(engine.zoom(0.0).is_err());
    tokio::time::sleep(Duration::from_millis(25)).await;
    assert!(mock.log.lock().unwrap().is_empty());
}
#[tokio::test]
async fn navigate_waits_only_owned_load_without_dom_then_readiness_waits_composer() {
    let mock = mock(Scenario::Sol).await;
    let engine = BrowserEngine::connect(&mock.url).await.unwrap();
    let start = tokio::time::Instant::now();
    engine.navigate("https://chatgpt.com/").await.unwrap();
    assert!(start.elapsed() >= Duration::from_millis(30));
    assert!(
        mock.log
            .lock()
            .unwrap()
            .iter()
            .all(|v| v["method"] != "Runtime.evaluate")
    );
    engine
        .wait_ready(Duration::from_secs(1), Cancellation::default())
        .await
        .unwrap();
    assert_eq!(
        mock.log
            .lock()
            .unwrap()
            .iter()
            .filter(|v| v["method"] == "Runtime.evaluate")
            .count(),
        2
    );
}
#[tokio::test]
async fn loading_page_readiness_times_out_and_is_cancellable() {
    let mock = mock(Scenario::Loading).await;
    let engine = BrowserEngine::connect(&mock.url).await.unwrap();
    assert!(matches!(
        engine
            .wait_ready(Duration::from_millis(30), Cancellation::default())
            .await,
        Err(Error::Timeout)
    ));
    let cancel = Cancellation::default();
    let trigger = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(10)).await;
        trigger.cancel();
    });
    assert!(matches!(
        engine.wait_ready(Duration::from_secs(5), cancel).await,
        Err(Error::Cancelled)
    ));
}

#[tokio::test]
async fn limits_identity_can_be_inspected_without_picker_or_composer() {
    let mock = mock(Scenario::Loading).await;
    let engine = BrowserEngine::connect(&mock.url).await.unwrap();
    let account = engine
        .inspect_account(InteractionMode::Automatic, Cancellation::default())
        .await
        .unwrap();
    assert_eq!(account.limits_plan, Plan::Pro100);
    let log = mock.log.lock().unwrap();
    assert_eq!(log.len(), 2);
    assert!(log.iter().all(|v| {
        v["params"]["expression"]
            .as_str()
            .unwrap()
            .contains(")(\"account\",")
    }));
}
