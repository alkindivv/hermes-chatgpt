//! Stateful CDP fixtures for draft ownership. They never connect an account or real browser.
use cw_browser::{
    Attachment, BrowserEngine, Cancellation, CheckpointRequest, Error, InteractionMode, Ownership,
    TurnInput,
};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    net::TcpListener,
    sync::{Notify, watch},
    task::JoinHandle,
};

#[derive(Clone, Copy)]
enum Failure {
    Upload,
    HoldUpload,
    Verify,
    HoldVerify,
    Cleanup,
    FinalGate,
    ExistingDraft,
    ExistingAttachment,
    Insert,
}
#[derive(Default)]
struct Draft {
    text: String,
    selected: Vec<Value>,
    attachments: Vec<String>,
    calls: Vec<(String, Value)>,
    clicks: usize,
}
struct Fixture {
    engine: Arc<BrowserEngine>,
    draft: Arc<Mutex<Draft>>,
    reached: Arc<Notify>,
    release: Arc<Notify>,
    file: tempfile::NamedTempFile,
    task: JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}
fn input() -> TurnInput {
    TurnInput {
        connector_name: Some("Codex Native2".into()),
        full_prompt: "owned prompt control_unique_123".into(),
        model: "chatgpt-web/gpt-6-sol".into(),
        effort: "high".into(),
        attachments: vec![],
        interaction_mode: InteractionMode::Automatic,
        ownership: Ownership {
            task_id: "owned".into(),
            epoch: 1,
        },
        turn_token: None,
    }
}
fn checkpoint() -> CheckpointRequest {
    CheckpointRequest {
        source_owner: Ownership {
            task_id: "owned".into(),
            epoch: 0,
        },
        turn: input(),
        control_token: "control_unique_123".into(),
    }
}
fn operation(request: &Value) -> (String, Value) {
    if request["method"] != "Runtime.evaluate" {
        return (
            request["method"].as_str().unwrap().into(),
            request["params"].clone(),
        );
    }
    let expr = request["params"]["expression"].as_str().unwrap();
    if let Some((_, args)) = expr.rsplit_once(")(\"") {
        let args: Vec<Value> = serde_json::from_str(&format!(
            "[\"{}]",
            args.strip_suffix(");})()").expect("page invocation suffix")
        ))
        .unwrap();
        return (args[0].as_str().unwrap().into(), args[1].clone());
    }
    ("abort".into(), Value::Null)
}
async fn fixture(failure: Failure) -> Fixture {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!(
        "ws://127.0.0.1:{}/devtools/page/preparation-fixture",
        listener.local_addr().unwrap().port()
    );
    let draft = Arc::new(Mutex::new(Draft::default()));
    if matches!(failure, Failure::ExistingDraft) {
        draft.lock().unwrap().text = "user draft".into();
    }
    if matches!(failure, Failure::ExistingAttachment) {
        draft
            .lock()
            .unwrap()
            .attachments
            .push("user-file.txt".into());
    }
    let observed = draft.clone();
    let reached = Arc::new(Notify::new());
    let signal = reached.clone();
    let release = Arc::new(Notify::new());
    let barrier = release.clone();
    let task = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_async(socket).await.unwrap();
        let mut highlighted = false;
        while let Some(Ok(message)) = ws.next().await {
            let request: Value = serde_json::from_slice(&message.into_data()).unwrap();
            let (op, arg) = operation(&request);
            let mut hold = false;
            let mut fail = false;
            let value = {
                let mut d = observed.lock().unwrap();
                d.calls.push((op.clone(), arg.clone()));
                match op.as_str() {
                    "ready" | "connectorFocus" | "verifyModel" | "abort" => json!(true),
                    "focus" => {
                        fail = !d.text.trim().is_empty();
                        json!(true)
                    }
                    "account" => {
                        json!({"user_id":"user","account_id":"account","plan_type":"pro","structure":"personal","needs_attention":false,"server_authenticated":true})
                    }
                    "capabilityState" => {
                        json!({"composer_ready":true,"document_ready":true,"selector_count":1,"think_available":false})
                    }
                    "capabilityPicker" => {
                        json!({"available":[true,true,true,true,true],"families":["6"]})
                    }
                    "model" => json!({"min":0,"max":4,"value":2}),
                    "snapshot" => {
                        json!({"document_id":"doc","ids":[],"users":[],"responses":[],"running":false})
                    }
                    "connectorSnapshot" => {
                        json!({"document_id":"doc","text":d.text,"selected":d.selected,
                        "rows":if d.text=="@codex" {vec![json!({"name":"Codex Native2","highlighted":highlighted,"disabled":false})]} else {vec![]},
                        "running":false,"has_attachments":!d.attachments.is_empty()})
                    }
                    "Input.insertText" => {
                        d.text.push_str(arg["text"].as_str().unwrap());
                        fail = matches!(failure, Failure::Insert)
                            && arg["text"] == input().full_prompt;
                        json!({})
                    }
                    "Input.dispatchKeyEvent" => {
                        if arg["type"] == "keyDown" {
                            match arg["key"].as_str().unwrap() {
                                "ArrowDown" => highlighted = true,
                                "Enter" => {
                                    d.text.clear();
                                    d.selected =
                                        vec![json!({"name":"Codex Native2","path":"app://owned"})];
                                }
                                "Backspace"
                                    if !matches!(failure, Failure::Cleanup)
                                        || d.text != input().full_prompt =>
                                {
                                    d.text.clear();
                                    d.selected.clear();
                                }
                                _ => {}
                            }
                        }
                        json!({})
                    }
                    "verifyPrompt" => {
                        fail = matches!(failure, Failure::Verify);
                        hold = matches!(failure, Failure::HoldVerify | Failure::FinalGate);
                        json!(true)
                    }
                    "Page.getFrameTree" => json!({"frameTree":{"frame":{"id":"owned-frame"}}}),
                    "Network.enable" => json!({}),
                    "DOM.getDocument" => json!({"root":{"nodeId":1}}),
                    "DOM.querySelectorAll" => json!({"nodeIds":[2]}),
                    "DOM.setFileInputFiles" => {
                        d.attachments = arg["files"]
                            .as_array()
                            .unwrap()
                            .iter()
                            .map(|p| {
                                std::path::Path::new(p.as_str().unwrap())
                                    .file_name()
                                    .unwrap()
                                    .to_string_lossy()
                                    .into_owned()
                            })
                            .collect();
                        hold = matches!(failure, Failure::HoldUpload);
                        fail = !hold;
                        json!({})
                    }
                    "clearAttachments" => {
                        d.attachments.clear();
                        json!(true)
                    }
                    "send" => {
                        // The DOM fixture independently executes the real asset's final gate.
                        let valid = arg["full_prompt"] == d.text
                            && arg["document_id"] == "doc"
                            && d.selected.len() == 1
                            && arg["connector_name"] == d.selected[0]["name"];
                        if !valid {
                            fail = true;
                        } else {
                            d.clicks += 1;
                        }
                        json!(true)
                    }
                    "checkpointStop" | "checkpointIdle" => json!(true),
                    _ => panic!("unexpected preparation operation: {op}"),
                }
            };
            if hold {
                signal.notify_one();
                if matches!(failure, Failure::FinalGate) {
                    barrier.notified().await;
                } else {
                    continue;
                }
            }
            let response = if fail {
                json!({"id":request["id"],"result":{"exceptionDetails":{"text":"fixture preparation failed"}}})
            } else if request["method"] == "Runtime.evaluate" {
                json!({"id":request["id"],"result":{"result":{"value":value}}})
            } else {
                json!({"id":request["id"],"result":value})
            };
            // Native CDP errors must use the protocol envelope, not a page exception envelope.
            let response = if fail && request["method"] != "Runtime.evaluate" {
                json!({"id":request["id"],"error":{"code":-1,"message":"fixture upload failed after file input mutation"}})
            } else {
                response
            };
            if ws.send(response.to_string().into()).await.is_err() {
                break;
            }
        }
    });
    Fixture {
        engine: Arc::new(BrowserEngine::connect(&url).await.unwrap()),
        draft,
        reached,
        release,
        file: tempfile::NamedTempFile::new().unwrap(),
        task,
    }
}
fn assert_clean(f: &Fixture) {
    let d = f.draft.lock().unwrap();
    assert!(
        d.calls
            .iter()
            .any(|(op, arg)| op == "Input.insertText" && arg["text"] == input().full_prompt),
        "fixture must reach the owned prompt edit"
    );
    assert!(
        d.text.is_empty() && d.selected.is_empty() && d.attachments.is_empty(),
        "owned draft remains: text={:?}, pills={}, attachments={:?}",
        d.text,
        d.selected.len(),
        d.attachments
    );
    assert_eq!(d.clicks, 0);
    assert!(
        !d.calls.iter().any(|(op, _)| op == "send"),
        "preparation failure must not dispatch Send"
    );
}
#[tokio::test]
async fn attachment_failure_cleans_the_owned_prompt_pill_and_files() {
    let f = fixture(Failure::Upload).await;
    let mut turn = input();
    turn.attachments.push(Attachment {
        path: f.file.path().into(),
    });
    let (progress, _) = tokio::sync::mpsc::unbounded_channel();
    assert!(
        f.engine
            .submit(turn, progress, Cancellation::default())
            .await
            .is_err()
    );
    assert_clean(&f);
}
#[tokio::test]
async fn failed_input_reply_after_native_edit_rolls_back_the_draft() {
    let f = fixture(Failure::Insert).await;
    let (progress, _) = tokio::sync::mpsc::unbounded_channel();
    assert!(matches!(
        f.engine
            .submit(input(), progress, Cancellation::default())
            .await,
        Err(Error::Protocol(_))
    ));
    assert_clean(&f);
}
#[tokio::test]
async fn cancellation_after_file_input_mutation_uses_fresh_cleanup() {
    let f = fixture(Failure::HoldUpload).await;
    let mut turn = input();
    turn.attachments.push(Attachment {
        path: f.file.path().into(),
    });
    let cancel = Cancellation::default();
    let token = cancel.clone();
    let engine = f.engine.clone();
    let (progress, _) = tokio::sync::mpsc::unbounded_channel();
    let work = tokio::spawn(async move { engine.submit(turn, progress, token).await });
    tokio::time::timeout(Duration::from_secs(4), f.reached.notified())
        .await
        .unwrap();
    cancel.cancel();
    assert!(matches!(work.await.unwrap(), Err(Error::Cancelled)));
    assert_clean(&f);
}
#[tokio::test]
async fn checkpoint_prompt_verification_failure_rolls_back() {
    let f = fixture(Failure::Verify).await;
    let (_tx, rx) = watch::channel(false);
    assert!(
        f.engine
            .settle_checkpoint(checkpoint(), rx, Cancellation::default())
            .await
            .is_err()
    );
    assert_clean(&f);
}
#[tokio::test]
async fn checkpoint_cancellation_after_edit_rolls_back() {
    let f = fixture(Failure::HoldVerify).await;
    let (_tx, rx) = watch::channel(false);
    let cancel = Cancellation::default();
    let token = cancel.clone();
    let engine = f.engine.clone();
    let work = tokio::spawn(async move { engine.settle_checkpoint(checkpoint(), rx, token).await });
    tokio::time::timeout(Duration::from_secs(4), f.reached.notified())
        .await
        .unwrap();
    cancel.cancel();
    assert!(matches!(work.await.unwrap(), Err(Error::Cancelled)));
    assert_clean(&f);
}
#[tokio::test]
async fn receipt_changed_after_preparation_cleans_before_returning() {
    let f = fixture(Failure::FinalGate).await;
    let (tx, rx) = watch::channel(false);
    let engine = f.engine.clone();
    let work = tokio::spawn(async move {
        engine
            .settle_checkpoint(checkpoint(), rx, Cancellation::default())
            .await
    });
    tokio::time::timeout(Duration::from_secs(4), f.reached.notified())
        .await
        .unwrap();
    tx.send(true).unwrap();
    f.release.notify_one();
    assert!(matches!(work.await.unwrap(), Err(Error::Dom(_))));
    assert_clean(&f);
}
#[tokio::test]
async fn abandoned_preparation_quarantines_the_engine() {
    for checkpoint_mode in [false, true] {
        let f = fixture(Failure::HoldVerify).await;
        let engine = f.engine.clone();
        let (_tx, rx) = watch::channel(false);
        let work = tokio::spawn(async move {
            if checkpoint_mode {
                engine
                    .settle_checkpoint(checkpoint(), rx, Cancellation::default())
                    .await
                    .map(|_| ())
            } else {
                let (progress, _) = tokio::sync::mpsc::unbounded_channel();
                engine
                    .submit(input(), progress, Cancellation::default())
                    .await
                    .map(|_| ())
            }
        });
        tokio::time::timeout(Duration::from_secs(4), f.reached.notified())
            .await
            .unwrap();
        work.abort();
        assert!(work.await.unwrap_err().is_cancelled());
        let (progress, _) = tokio::sync::mpsc::unbounded_channel();
        let retry = f
            .engine
            .submit(input(), progress, Cancellation::default())
            .await;
        assert!(
            matches!(retry, Err(Error::CleanupUnconfirmed)),
            "abandoned preparation was reusable: {retry:?}"
        );
    }
}
#[tokio::test]
async fn failed_preparation_cleanup_quarantines_the_engine() {
    let f = fixture(Failure::Cleanup).await;
    let mut turn = input();
    turn.attachments.push(Attachment {
        path: f.file.path().into(),
    });
    let (progress, _) = tokio::sync::mpsc::unbounded_channel();
    assert!(matches!(
        f.engine
            .submit(turn, progress, Cancellation::default())
            .await,
        Err(Error::CleanupUnconfirmed)
    ));
    let (progress, _) = tokio::sync::mpsc::unbounded_channel();
    assert!(matches!(
        f.engine
            .submit(input(), progress, Cancellation::default())
            .await,
        Err(Error::CleanupUnconfirmed)
    ));
}
#[tokio::test]
async fn existing_user_content_is_never_owned_or_cleared() {
    for failure in [Failure::ExistingDraft, Failure::ExistingAttachment] {
        let f = fixture(failure).await;
        let before = {
            let d = f.draft.lock().unwrap();
            (d.text.clone(), d.attachments.clone())
        };
        let mut turn = input();
        turn.connector_name = None;
        let (progress, _) = tokio::sync::mpsc::unbounded_channel();
        assert!(matches!(
            f.engine
                .submit(turn, progress, Cancellation::default())
                .await,
            Err(Error::Dom(_))
        ));
        let d = f.draft.lock().unwrap();
        assert_eq!((d.text.clone(), d.attachments.clone()), before);
        assert!(!d.calls.iter().any(|(op, _)| matches!(
            op.as_str(),
            "Input.insertText" | "Input.dispatchKeyEvent" | "clearAttachments"
        )));
    }
}
#[tokio::test]
async fn both_send_paths_carry_the_final_integrity_contract() {
    for checkpoint_mode in [false, true] {
        let f = fixture(Failure::FinalGate).await;
        let engine = f.engine.clone();
        let (tx, rx) = watch::channel(false);
        let cancel = Cancellation::default();
        let token = cancel.clone();
        let work = tokio::spawn(async move {
            if checkpoint_mode {
                engine
                    .settle_checkpoint(checkpoint(), rx, token)
                    .await
                    .map(|_| ())
            } else {
                let (progress, _) = tokio::sync::mpsc::unbounded_channel();
                engine.submit(input(), progress, token).await.map(|_| ())
            }
        });
        tokio::time::timeout(Duration::from_secs(4), f.reached.notified())
            .await
            .unwrap();
        f.draft
            .lock()
            .unwrap()
            .text
            .push_str(" user edit after preparation");
        f.release.notify_one();
        let _ = tokio::time::timeout(Duration::from_secs(4), work)
            .await
            .unwrap()
            .unwrap();
        drop(tx);
        cancel.cancel();
        let d = f.draft.lock().unwrap();
        let (_, args) = d
            .calls
            .iter()
            .find(|(op, _)| op == "send")
            .expect("final gate was not called");
        assert_eq!(args["document_id"], "doc");
        assert_eq!(args["full_prompt"], input().full_prompt);
        assert_eq!(args["connector_name"], "Codex Native2");
        assert_eq!(d.clicks, 0);
    }
}
