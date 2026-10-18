use cw_contracts::{BrowserInteractionMode, LauncherState, limits::*, models::*, state::*};
use serde_json::json;
#[test]
fn defaults_and_repair_match_reference() {
    let d = LauncherState::default();
    let v = serde_json::to_value(&d).unwrap();
    assert_eq!(v["sidebarWidth"], 252.0);
    assert_eq!(v["composerPredictionsEnabled"], true);
    assert_eq!(v.as_object().unwrap().len(), 22);
    let s=LauncherState::normalize(json!({"version":1,"onboardingComplete":true,"browserInteractionMode":"manual","zeroRiskProEnabled":true,"sidebarWidth":900,"bridgeEnabled":true,"custom":4})).unwrap();
    assert_eq!(s.browser_interaction_mode, BrowserInteractionMode::Manual);
    assert!(!s.zero_risk_pro_enabled);
    assert_eq!(s.sidebar_width, 252.0);
    let v = serde_json::to_value(s).unwrap();
    assert!(v.get("bridgeEnabled").is_none());
    assert_eq!(v["custom"], 4);
    assert_eq!(LauncherState::normalize(json!({"version":2})).unwrap(), d);
}
#[test]
fn preferences_and_sidebar_are_explicit() {
    let mut s = LauncherState::default();
    assert!(s.set_preference("autoStart", &json!(true)).is_err());
    s.set_preference("keepRunningOnClose", &json!("true"))
        .unwrap();
    assert!(!s.keep_running_on_close);
    assert!(s.set_sidebar(true, 239.9).is_err());
    s.set_sidebar(false, 300.5).unwrap();
    assert_eq!(s.sidebar_width, 301.0);
    assert_eq!(
        next_session_refresh_reminder_at(0.0).unwrap(),
        "1970-01-03T00:00:00.000Z"
    );
    assert!(next_session_refresh_reminder_at(f64::NAN).is_err());
}
fn plus() -> ChatGptWebAccountCapabilities {
    ChatGptWebAccountCapabilities {
        sol_available: true,
        pro_available: false,
        ..Default::default()
    }
}
#[test]
fn routes_preserve_accounts_and_legacy_efforts() {
    let c = plus();
    assert_eq!(
        available_chat_gpt_web_model_routes(&c, false)
            .unwrap()
            .len(),
        4
    );
    assert!(require_chat_gpt_web_model_route("chatgpt-web/pro", &c, None).is_err());
    let mut c = c;
    c.extra_high_available = Some(true);
    let r = require_chat_gpt_web_model_route("chatgpt-web/gpt-6-sol", &c, Some("xhigh")).unwrap();
    assert_eq!(r.adapter_effort, AdapterEffort::Xhigh);
    c.pro_available = true;
    let r = require_chat_gpt_web_model_route("chatgpt-web/pro", &c, Some("low")).unwrap();
    assert_eq!(r.codex_effort, CodexEffort::Ultra);
    assert_eq!(r.adapter_effort, AdapterEffort::Max);
    c.browser_interaction_mode = Some(BrowserInteractionMode::Manual);
    assert_eq!(
        available_chat_gpt_web_model_routes(&c, false)
            .unwrap()
            .len(),
        1
    );
    assert!(require_chat_gpt_web_model_route("chatgpt-web/gpt-6-sol", &c, None).is_err());
    c.experimental_bigger_context = Some(true);
    assert!(available_chat_gpt_web_model_routes(&c, false).is_err());
    let free = ChatGptWebAccountCapabilities::default();
    assert_eq!(
        available_chat_gpt_web_model_routes(&free, true)
            .unwrap()
            .len(),
        3
    );
    let think = require_chat_gpt_web_model_route("chatgpt-web/think", &free, Some("high")).unwrap();
    assert_eq!(think.adapter_effort, AdapterEffort::Medium);
    assert_eq!(think.codex_effort, CodexEffort::Low);
}
#[test]
fn context_and_transport_have_independent_boundaries() {
    let mut c = plus();
    c.experimental_bigger_context = Some(true);
    assert_eq!(
        resolve_chat_gpt_web_context_limits(
            BackendModel::Sol,
            AdapterEffort::Low,
            &c,
            Some(ModelFamily::Gpt6)
        )
        .unwrap()
        .context_window,
        41000
    );
    assert_eq!(
        resolve_chat_gpt_web_context_limits(
            BackendModel::Sol,
            AdapterEffort::High,
            &c,
            Some(ModelFamily::Gpt6)
        )
        .unwrap()
        .context_window,
        120000
    );
    c.pro_available = true;
    assert_eq!(
        resolve_chat_gpt_web_context_limits(
            BackendModel::Sol,
            AdapterEffort::High,
            &c,
            Some(ModelFamily::Gpt6)
        )
        .unwrap()
        .context_window,
        240000
    );
    assert_eq!(
        resolve_chat_gpt_web_context_limits(
            BackendModel::Sol,
            AdapterEffort::Max,
            &c,
            Some(ModelFamily::Gpt6)
        )
        .unwrap()
        .context_window,
        336579
    );
    assert_eq!(
        resolve_chat_gpt_web_transport_limits(BackendModel::Luna, AdapterEffort::Low, &c)
            .unwrap()
            .browser_message_token_limit,
        Some(19808)
    );
    assert!(
        resolve_chat_gpt_web_context_limits(BackendModel::ZeroRisk, AdapterEffort::Low, &c, None)
            .is_err()
    );
}
#[test]
fn catalog_keeps_native_metadata_without_fabricating_capabilities() {
    let native = json!({"models":[{"slug":"native","visibility":"list","tool_mode":"code","priority":2,"multi_agent_version":"disabled","supported_reasoning_levels":[{"effort":"high","extra":7}],"comp_hash":"native","max_context_window":320000}]});
    let result = augment_native_model_catalog(&native, &plus(), true, false, None).unwrap();
    assert_eq!(result["models"][0], native["models"][0]);
    let web = &result["models"][2];
    assert_eq!(web["supported_reasoning_levels"][1]["extra"], 7);
    assert!(web.get("comp_hash").is_none());
    assert_eq!(web["tool_mode"], json!(null));
    assert!(
        augment_native_model_catalog(&json!({"models":[]}), &plus(), true, false, None).is_err()
    );
}
#[test]
fn receipts_are_scoped_deduplicated_and_rolling() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = LimitsStore::open(dir.path().join("limits.json")).unwrap();
    let a = "a".repeat(64);
    let b = "b".repeat(64);
    let t = 1000;
    s.configure(&a, Plan::Pro200, t).unwrap();
    for (id, model) in [
        ("six", ReceiptModel::Gpt6Pro),
        ("five", ReceiptModel::Gpt56Pro),
        ("unknown", ReceiptModel::ProUnknown),
        ("other", ReceiptModel::Other),
    ] {
        assert!(s.record(&a, id, model, t, t).unwrap());
    }
    assert!(!s.record(&a, "six", ReceiptModel::Gpt6Pro, t, t).unwrap());
    assert!(
        !s.record(&b, "inactive", ReceiptModel::Gpt6Pro, t, t)
            .unwrap()
    );
    let v = s.snapshot(t).unwrap();
    assert_eq!(v.total_messages, 4);
    assert_eq!(
        v.windows.iter().map(|w| w.used).collect::<Vec<_>>(),
        vec![1, 1, 3]
    );
    assert!(v.incomplete);
    assert!(
        s.record(&a, "future", ReceiptModel::Other, t + 1, t)
            .is_err()
    );
    assert_eq!(s.snapshot(t + DAY_MS).unwrap().windows[2].used, 0);
    assert_eq!(s.snapshot(t + RETENTION_MS).unwrap().total_messages, 0);
}
#[test]
fn failed_persistence_does_not_commit_and_corruption_is_preserved() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("limits.json");
    let a = "a".repeat(64);
    let mut s = LimitsStore::open(&path).unwrap();
    s.configure(&a, Plan::Pro100, 0).unwrap();
    std::fs::remove_file(&path).unwrap();
    std::fs::create_dir(&path).unwrap();
    assert!(s.record(&a, "retry", ReceiptModel::Gpt6Pro, 0, 0).is_err());
    assert_eq!(s.snapshot(0).unwrap().total_messages, 0);
    std::fs::remove_dir(&path).unwrap();
    assert!(s.record(&a, "retry", ReceiptModel::Gpt6Pro, 0, 0).unwrap());
    std::fs::write(&path, "{broken").unwrap();
    assert!(LimitsStore::open(&path).is_err());
    assert_eq!(std::fs::read_to_string(path).unwrap(), "{broken");
}

#[test]
fn state_store_is_private_and_restart_stable() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("state.json");
    let mut store = StateStore::open(&path).unwrap();
    store.update(json!({"composerPredictionsEnabled":false,"language":"zh-CN","onboardingComplete":true,"browserInteractionMode":"manual"}).as_object().unwrap().clone()).unwrap();
    let state = StateStore::open(&path).unwrap().read();
    assert!(!state.composer_predictions_enabled);
    assert_eq!(
        state.browser_interaction_mode,
        BrowserInteractionMode::Manual
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[test]
fn receipt_capacity_and_strict_schema_reject_without_writes() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("limits.json");
    let a = "a".repeat(64);
    let events: Vec<_> = (0..20000)
        .map(|i| json!({"id":i.to_string(),"model":"gpt-6-pro","at":0}))
        .collect();
    let value = json!({"version":1,"activeAccountKey":a,"accounts":{a.clone():{"plan":"pro_200","initializedAt":0,"checkedAt":0,"events":events}}});
    let bytes = serde_json::to_vec(&value).unwrap();
    std::fs::write(&path, &bytes).unwrap();
    let mut store = LimitsStore::open(&path).unwrap();
    assert_eq!(
        store
            .record(&a, "overflow", ReceiptModel::Other, 0, 0)
            .unwrap_err()
            .code,
        "LIMITS_CAPACITY_EXCEEDED"
    );
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert!(
        store
            .record(
                &a,
                "after-expiry",
                ReceiptModel::Other,
                RETENTION_MS,
                RETENTION_MS
            )
            .unwrap()
    );
    assert_eq!(store.snapshot(RETENTION_MS).unwrap().total_messages, 1);
    let mut bad = value.clone();
    bad["accounts"][&a]["events"][0]["prompt"] = json!("secret");
    let bytes = serde_json::to_vec(&bad).unwrap();
    std::fs::write(&path, &bytes).unwrap();
    assert_eq!(
        LimitsStore::open(&path).err().unwrap().code,
        "LIMITS_CORRUPT_STATE"
    );
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
}
#[test]
fn catalog_compatibility_and_context_override_are_explicit() {
    let native = json!({"models":[{"slug":"disabled","multi_agent_version":"disabled","max_context_window":10},{"slug":"template","visibility":"list","supported_reasoning_levels":[],"tool_mode":"code","priority":2,"multi_agent_version":"v2","max_context_window":500000}]});
    let r = augment_native_model_catalog(&native, &plus(), true, true, Some(400000)).unwrap();
    assert_eq!(r["models"][0]["multi_agent_version"], "disabled");
    assert_eq!(r["models"][0]["max_context_window"], 400000);
    assert_eq!(r["models"][1]["max_context_window"], 500000);
    assert_eq!(r["models"][2]["priority"], 3);
    assert_eq!(r["models"][3]["priority"], 2);
    let mut bad = native;
    bad["models"][1]["priority"] = json!(9007199254740991_u64);
    assert!(augment_native_model_catalog(&bad, &plus(), true, true, None).is_err());
}
#[test]
fn context_matrix_and_fixed_legacy_routes() {
    for pro in [false, true] {
        for extra in [false, true] {
            for bigger in [false, true] {
                let c = ChatGptWebAccountCapabilities {
                    sol_available: true,
                    pro_available: pro,
                    extra_high_available: Some(extra),
                    experimental_bigger_context: Some(bigger),
                    ..Default::default()
                };
                for route in available_chat_gpt_web_model_routes(&c, true).unwrap() {
                    for effort in chat_gpt_web_route_efforts(&route, &c) {
                        let name = serde_json::to_value(effort).unwrap();
                        let resolved =
                            require_chat_gpt_web_model_route(&route.slug, &c, name.as_str())
                                .unwrap();
                        let limits = resolve_chat_gpt_web_context_limits(
                            resolved.backend_model,
                            resolved.adapter_effort,
                            &c,
                            resolved.model_family,
                        )
                        .unwrap();
                        assert!(limits.context_window >= limits.auto_compact_token_limit);
                        resolve_chat_gpt_web_transport_limits(
                            resolved.backend_model,
                            resolved.adapter_effort,
                            &c,
                        )
                        .unwrap();
                    }
                }
            }
        }
    }
    assert_eq!(
        resolve_chat_gpt_web_staging_token_budget(BackendModel::Sol, AdapterEffort::Low, &plus())
            .unwrap(),
        23807
    );
}
