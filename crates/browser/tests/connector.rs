use cw_browser::{BrowserEngine, Cancellation, Error, InteractionMode, Ownership, TurnInput};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{net::TcpListener, sync::Notify, task::JoinHandle};

#[derive(Clone, Copy)]
enum Scenario {
    Exact,
    Duplicate,
    Missing,
    WrongPill,
    Cancel,
    CleanupFailure,
    ExistingDraft,
    ExistingWrongPill,
    DisabledDuplicate,
    LateDuplicate,
    ActivationDuplicate,
    TransientDuplicate,
}
struct Fixture {
    url: String,
    calls: Arc<Mutex<Vec<Value>>>,
    typed: Arc<Notify>,
    task: JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

async fn fixture(scenario: Scenario) -> Fixture {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "ws://127.0.0.1:{}/devtools/page/owned-connector",
        listener.local_addr().unwrap().port()
    );
    let calls = Arc::new(Mutex::new(Vec::new()));
    let recorded = calls.clone();
    let typed = Arc::new(Notify::new());
    let notify = typed.clone();
    let task = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(socket).await.unwrap();
        let mut draft = if matches!(scenario, Scenario::ExistingDraft) {
            "user's existing draft".to_string()
        } else {
            String::new()
        };
        let mut selected: Vec<Value> = if matches!(scenario, Scenario::ExistingWrongPill) {
            vec![json!({"name":"User Connector","path":"app://user-connector"})]
        } else {
            Vec::new()
        };
        let mut highlighted = false;
        let mut menu_observations = 0;
        let mut clear_count = 0;
        let mut held_observation = false;
        while let Some(Ok(message)) = ws.next().await {
            let request: Value = serde_json::from_slice(&message.into_data()).unwrap();
            recorded.lock().unwrap().push(request.clone());
            let result = match request["method"].as_str().unwrap() {
                "Input.insertText" => {
                    draft.push_str(request["params"]["text"].as_str().unwrap());
                    if draft == "@codex" && !matches!(scenario, Scenario::Cancel) {
                        notify.notify_one();
                    }
                    json!({})
                }
                "Input.dispatchKeyEvent" => {
                    if request["params"]["type"] == "keyDown" {
                        match request["params"]["key"].as_str().unwrap() {
                            "Backspace" => {
                                clear_count += 1;
                                if !matches!(scenario, Scenario::CleanupFailure) || clear_count < 2
                                {
                                    draft.clear();
                                    selected.clear();
                                    highlighted = false;
                                }
                            }
                            "ArrowDown" => highlighted = true,
                            "Enter" if highlighted => {
                                draft.clear();
                                selected = vec![
                                    json!({"name":if matches!(scenario, Scenario::WrongPill) { "Codex Other" } else { "Codex Native2" },"path":"app://fixture-connector"}),
                                ];
                            }
                            _ => {}
                        }
                    }
                    json!({})
                }
                "Runtime.evaluate" => {
                    let expression = request["params"]["expression"].as_str().unwrap();
                    let value = if expression.contains(")(\"ready\",")
                        || expression.contains(")(\"connectorFocus\",")
                    {
                        json!(true)
                    } else if expression.contains(")(\"account\",") {
                        json!({"user_id":"user","account_id":"account","plan_type":"pro","structure":"personal","needs_attention":false,"server_authenticated":true})
                    } else if expression.contains(")(\"capabilityState\",") {
                        json!({"composer_ready":true,"document_ready":true,"selector_count":1,"think_available":false})
                    } else if expression.contains(")(\"capabilityPicker\",") {
                        json!({"available":[true,true,true,true,true],"families":["6"]})
                    } else if expression.contains(")(\"model\",") {
                        json!({"min":0,"max":4,"value":2})
                    } else if expression.contains(")(\"verifyModel\",") {
                        json!(true)
                    } else if expression.contains(")(\"snapshot\",") {
                        json!({"document_id":"owned-document","ids":[],"users":[],"responses":[],"running":false})
                    } else if expression.contains(")(\"connectorSnapshot\",") {
                        let mentioned = draft == "@codex";
                        if mentioned && matches!(scenario, Scenario::Cancel) && !held_observation {
                            // Deliberately hold one observation so cancellation follows a real edit.
                            held_observation = true;
                            notify.notify_one();
                            continue;
                        }
                        let rows = if mentioned && !matches!(scenario, Scenario::Missing) {
                            menu_observations += 1;
                            if matches!(scenario, Scenario::ActivationDuplicate) {
                                highlighted = true;
                            }
                            let target = json!({"name":"Codex Native2","highlighted":highlighted,"disabled":false});
                            if matches!(scenario, Scenario::Duplicate)
                                || matches!(scenario, Scenario::LateDuplicate) && highlighted
                                || matches!(scenario, Scenario::ActivationDuplicate)
                                    && menu_observations > 1
                                || matches!(scenario, Scenario::TransientDuplicate)
                                    && menu_observations == 2
                            {
                                vec![target.clone(), target]
                            } else if matches!(scenario, Scenario::DisabledDuplicate) {
                                vec![
                                    target,
                                    json!({"name":"Codex Native2","highlighted":false,"disabled":true}),
                                ]
                            } else if matches!(scenario, Scenario::TransientDuplicate) {
                                vec![target]
                            } else {
                                vec![
                                    json!({"name":"Codex Other","highlighted":!highlighted,"disabled":false}),
                                    target,
                                ]
                            }
                        } else {
                            vec![]
                        };
                        json!({"document_id":"owned-document","text":draft,"selected":selected,"rows":rows,"has_attachments":false,"personalization":"personalized"})
                    } else if expression.contains("__CW_BROWSER_ABORT_GENERATION__") {
                        json!(1)
                    } else {
                        panic!("unexpected connector page operation");
                    };
                    json!({"result":{"value":value}})
                }
                _ => panic!("unexpected connector command"),
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
    Fixture {
        url,
        calls,
        typed,
        task,
    }
}

#[tokio::test]
async fn exact_keyboard_selection_is_verified_then_cleared_without_sending_a_turn() {
    let fixture = fixture(Scenario::Exact).await;
    let engine = BrowserEngine::connect(&fixture.url).await.unwrap();
    let receipt = engine
        .verify_connector(
            "Codex Native2",
            InteractionMode::Automatic,
            Cancellation::default(),
        )
        .await
        .unwrap();
    assert_eq!(receipt.name, "Codex Native2");
    assert_eq!(receipt.document_id, "owned-document");
    assert!(receipt.composer_cleared);
    let calls = fixture.calls.lock().unwrap();
    assert!(
        calls
            .iter()
            .any(|call| call["params"]["key"] == "ArrowDown")
    );
    assert!(calls.iter().any(|call| call["params"]["key"] == "Enter"));
    assert!(
        calls
            .iter()
            .any(|call| call["params"]["key"] == "Backspace")
    );
    assert!(!calls.iter().any(|call| {
        call["params"]["expression"]
            .as_str()
            .is_some_and(|expression| expression.contains(")(\"send\","))
    }));
}

#[tokio::test]
async fn missing_duplicate_or_wrong_connector_never_becomes_verification_success() {
    for scenario in [Scenario::Missing, Scenario::Duplicate, Scenario::WrongPill] {
        let fixture = fixture(scenario).await;
        let engine = BrowserEngine::connect(&fixture.url).await.unwrap();
        assert!(
            engine
                .verify_connector(
                    "Codex Native2",
                    InteractionMode::Automatic,
                    Cancellation::default()
                )
                .await
                .is_err()
        );
        let calls = fixture.calls.lock().unwrap();
        assert!(
            calls
                .iter()
                .any(|call| call["params"]["key"] == "Backspace"),
            "owned mention must be removed after failure"
        );
        let entered = calls
            .iter()
            .filter_map(|call| call["params"]["text"].as_str())
            .collect::<String>();
        if matches!(scenario, Scenario::Missing) {
            assert_eq!(entered, "@codex@codex@codex");
        }
    }
}

#[tokio::test]
async fn cancellation_cleans_the_owned_mention_before_releasing_the_page() {
    let fixture = fixture(Scenario::Cancel).await;
    let engine = Arc::new(BrowserEngine::connect(&fixture.url).await.unwrap());
    let cancel = Cancellation::default();
    let task = tokio::spawn({
        let engine = engine.clone();
        let cancel = cancel.clone();
        async move {
            engine
                .verify_connector("Codex Native2", InteractionMode::Automatic, cancel)
                .await
        }
    });
    tokio::time::timeout(Duration::from_secs(3), fixture.typed.notified())
        .await
        .unwrap();
    cancel.cancel();
    assert!(matches!(task.await.unwrap(), Err(Error::Cancelled)));
    assert!(
        fixture
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(
                |call| call["params"]["key"] == "Backspace" && call["params"]["type"] == "keyDown"
            )
            .count()
            >= 2
    );
}

#[tokio::test]
async fn unconfirmed_connector_cleanup_quarantines_the_engine() {
    let fixture = fixture(Scenario::CleanupFailure).await;
    let engine = BrowserEngine::connect(&fixture.url).await.unwrap();
    assert!(matches!(
        engine
            .verify_connector(
                "Codex Native2",
                InteractionMode::Automatic,
                Cancellation::default()
            )
            .await,
        Err(Error::CleanupUnconfirmed)
    ));
    assert!(matches!(
        engine
            .verify_connector(
                "Codex Native2",
                InteractionMode::Automatic,
                Cancellation::default()
            )
            .await,
        Err(Error::CleanupUnconfirmed)
    ));
}

#[tokio::test]
async fn manual_mode_and_invalid_name_do_not_read_or_edit_the_page() {
    let fixture = fixture(Scenario::Exact).await;
    let engine = BrowserEngine::connect(&fixture.url).await.unwrap();
    assert!(matches!(
        engine
            .verify_connector(
                "Codex Native2",
                InteractionMode::Manual,
                Cancellation::default()
            )
            .await,
        Err(Error::ManualDomForbidden)
    ));
    assert!(
        engine
            .verify_connector(
                "bad\nname",
                InteractionMode::Automatic,
                Cancellation::default()
            )
            .await
            .is_err()
    );
    assert!(fixture.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn preexisting_user_draft_is_preserved_on_verification_refusal() {
    let fixture = fixture(Scenario::ExistingDraft).await;
    let engine = BrowserEngine::connect(&fixture.url).await.unwrap();
    assert!(
        engine
            .verify_connector(
                "Codex Native2",
                InteractionMode::Automatic,
                Cancellation::default()
            )
            .await
            .is_err()
    );
    assert!(
        !fixture
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|call| call["method"] == "Input.dispatchKeyEvent"
                || call["method"] == "Input.insertText")
    );
}

async fn assert_existing_connector_preserved(submit: bool) {
    let fixture = fixture(Scenario::ExistingWrongPill).await;
    let engine = BrowserEngine::connect(&fixture.url).await.unwrap();
    let result = if submit {
        let (progress, _) = tokio::sync::mpsc::unbounded_channel();
        engine
            .submit(
                TurnInput {
                    connector_name: Some("Codex Native2".into()),
                    full_prompt: "hello".into(),
                    model: "chatgpt-web/gpt-6-sol".into(),
                    effort: "high".into(),
                    attachments: vec![],
                    interaction_mode: InteractionMode::Automatic,
                    ownership: Ownership {
                        task_id: "owned".into(),
                        epoch: 1,
                    },
                    turn_token: None,
                },
                progress,
                Cancellation::default(),
            )
            .await
            .map(|_| ())
    } else {
        engine
            .verify_connector(
                "Codex Native2",
                InteractionMode::Automatic,
                Cancellation::default(),
            )
            .await
            .map(|_| ())
    };
    assert!(matches!(result, Err(Error::Dom(_))));
    assert!(
        !fixture
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|call| call["method"] == "Input.insertText"
                || call["method"] == "Input.dispatchKeyEvent"),
        "preflight refusal must preserve the user's connector; submit={submit}"
    );
}

#[tokio::test]
async fn verification_preserves_a_preexisting_wrong_connector() {
    assert_existing_connector_preserved(false).await;
}
#[tokio::test]
async fn submission_preserves_a_preexisting_wrong_connector() {
    assert_existing_connector_preserved(true).await;
}

async fn assert_duplicate_refused(scenario: Scenario) {
    let fixture = fixture(scenario).await;
    let engine = BrowserEngine::connect(&fixture.url).await.unwrap();
    let result = engine
        .verify_connector(
            "Codex Native2",
            InteractionMode::Automatic,
            Cancellation::default(),
        )
        .await;
    assert!(
        !fixture
            .calls
            .lock()
            .unwrap()
            .iter()
            .any(|call| call["params"]["key"] == "Enter"),
        "an ambiguous menu must never be activated"
    );
    assert!(matches!(result, Err(Error::Dom(message)) if message.contains("duplicate")));
}
#[tokio::test]
async fn disabled_duplicate_prevents_menu_activation() {
    assert_duplicate_refused(Scenario::DisabledDuplicate).await;
}
#[tokio::test]
async fn duplicate_after_arrow_down_prevents_menu_activation() {
    assert_duplicate_refused(Scenario::LateDuplicate).await;
}
#[tokio::test]
async fn duplicate_immediately_before_enter_prevents_menu_activation() {
    assert_duplicate_refused(Scenario::ActivationDuplicate).await;
}
#[tokio::test]
async fn every_arrow_refresh_checks_duplicates_even_on_the_last_iteration() {
    assert_duplicate_refused(Scenario::TransientDuplicate).await;
}
