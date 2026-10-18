use cw_bridge::*;
use serde_json::{Value, json};
use std::time::Duration;

async fn fixture(mode: ExecutionMode) -> (BridgeHandle, tokio::sync::mpsc::Receiver<BrowserRequest>) {
    BridgeServer::bind(
        "127.0.0.1:0".parse().unwrap(),
        BridgeConfig { mode, turn_ttl: Duration::from_secs(5), ..Default::default() },
    ).await.unwrap()
}

#[tokio::test]
async fn manual_catalog_needs_no_account_probe_and_keeps_native_provenance_unverified() {
    let (bridge, mut browser) = fixture(ExecutionMode::Manual).await;
    bridge.set_native_model_catalog(json!({"models":[{"slug":"native-fixture","visibility":"list","tool_mode":"function","supported_reasoning_levels":[{"effort":"high","description":"fixture"}]}],"source":"fixture"})).unwrap();
    let response = reqwest::get(format!("{}/models", bridge.base_url())).await.unwrap();
    assert_eq!(response.status(), 200, "Manual must not require a DOM/account observation");
    let catalog: Value = response.json().await.unwrap();
    let slugs: Vec<_> = catalog["models"].as_array().unwrap().iter()
        .filter_map(|row| row["slug"].as_str()).collect();
    assert_eq!(slugs, ["native-fixture", "chatgpt-web/zero-risk"]);
    assert_eq!(catalog["source"], "fixture");
    let status = bridge.catalog_status();
    assert!(status.augmentation_succeeded);
    assert!(!status.capabilities_observed && !status.native_catalog_fetched && !status.verified);
    assert!(browser.try_recv().is_err(), "Catalog discovery must not start a browser probe");
    bridge.set_mode(ExecutionMode::Automatic).unwrap();
    let response = reqwest::get(format!("{}/models", bridge.base_url())).await.unwrap();
    assert_eq!(response.status(), 503, "Automatic still requires observed capabilities");
    bridge.close().await.unwrap();
}

#[tokio::test]
async fn explicit_pro_preference_is_independent_of_account_evidence_and_idle_only() {
    let (bridge, mut browser) = fixture(ExecutionMode::Manual).await;
    bridge.set_route_preferences(RoutePreferences { zero_risk_pro_enabled: true, ..Default::default() }).unwrap();
    assert!(!bridge.catalog_status().capabilities_observed);
    let response = reqwest::Client::new().post(format!("{}/responses", bridge.base_url()))
        .json(&json!({"model":"chatgpt-web/zero-risk-pro","stream":true,"input":"fixture"}))
        .send().await.unwrap();
    assert_eq!(response.status(), 200);
    let BrowserRequest::Turn(turn) = browser.recv().await.unwrap() else { panic!("expected turn") };
    assert!(bridge.set_route_preferences(RoutePreferences::default()).is_err());
    bridge.cancel(&turn.request_id);
    response.text().await.unwrap();
    bridge.clear_account_capabilities().unwrap();
    assert!(!bridge.catalog_status().capabilities_observed);
    assert!(bridge.set_route_preferences(RoutePreferences { experimental_bigger_context: true, ..Default::default() }).is_err());
    bridge.close().await.unwrap();
}

#[tokio::test]
async fn session_change_pauses_admission_without_closing_the_listener_or_adopting_active_work() {
    let (bridge, mut browser) = fixture(ExecutionMode::Manual).await;
    let client = reqwest::Client::new();
    let lease = bridge.begin_session_change().unwrap();
    assert!(bridge.begin_session_change().is_err());
    let blocked = client.post(format!("{}/responses", bridge.base_url()))
        .json(&json!({"model":"fixture","stream":true,"input":"fixture"})).send().await.unwrap();
    assert_eq!(blocked.status(), 503);
    assert!(browser.try_recv().is_err());
    assert_eq!(reqwest::get(format!("{}/models", bridge.base_url())).await.unwrap().status(), 503);
    drop(lease);
    let accepted = client.post(format!("{}/responses", bridge.base_url()))
        .json(&json!({"model":"fixture","stream":true,"input":"fixture"})).send().await.unwrap();
    assert_eq!(accepted.status(), 200);
    let BrowserRequest::Turn(turn) = browser.recv().await.unwrap() else { panic!("expected turn") };
    assert!(bridge.begin_session_change().is_err());
    bridge.cancel(&turn.request_id);
    accepted.text().await.unwrap();
    let lease = bridge.begin_session_change().unwrap();
    bridge.drain();
    drop(lease);
    assert!(bridge.is_draining(), "releasing a session lease must not undo shutdown");
    bridge.close().await.unwrap();
}

#[tokio::test]
async fn manual_route_admits_work_without_claiming_account_support() {
    let (bridge, mut browser) = fixture(ExecutionMode::Manual).await;
    let client = reqwest::Client::new();
    let response = client.post(format!("{}/responses", bridge.base_url()))
        .json(&json!({"model":"chatgpt-web/zero-risk","stream":true,"input":"fixture only"}))
        .send().await.unwrap();
    assert_eq!(response.status(), 200, "Manual routing must work before an account probe");
    let BrowserRequest::Turn(turn) = browser.recv().await.unwrap() else { panic!("expected turn") };
    assert_eq!(turn.mode, ExecutionMode::Manual);
    assert!(!bridge.catalog_status().capabilities_observed);
    assert!(bridge.authorize(&turn.turn_token, &turn.owner).is_err());
    bridge.cancel(&turn.request_id);
    assert!(response.text().await.unwrap().contains("client_cancelled"));
    let blocked = client.post(format!("{}/responses", bridge.base_url()))
        .json(&json!({"model":"chatgpt-web/zero-risk-pro","stream":true,"input":"fixture only"}))
        .send().await.unwrap();
    assert_eq!(blocked.status(), 400, "Pro requires an explicit preference");
    assert!(browser.try_recv().is_err());
    bridge.close().await.unwrap();
}
