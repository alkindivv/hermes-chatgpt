//! Dedicated summary-only receipt settlement. No work-tool dispatch or native commands.
use crate::{BrowserEngine, Cancellation, Error, InteractionMode, Ownership, Result, TurnInput};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::time::Duration;
use tokio::sync::watch;
#[derive(Debug, Clone)]
pub struct CheckpointRequest {
    pub source_owner: Ownership,
    pub turn: TurnInput,
    pub control_token: String,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum CheckpointOutcome {
    Accepted,
    Cancelled,
    ReceiptClosed,
    ReceiptTimedOut,
    Failed(String),
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum CheckpointCleanup {
    Confirmed,
    Unconfirmed { reason: String },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckpointSettlement {
    pub ownership: Ownership,
    pub receipt_accepted: bool,
    pub outcome: CheckpointOutcome,
    pub cleanup: CheckpointCleanup,
}
impl CheckpointSettlement {
    /// Primary may retire the checkpoint only when both independent proofs hold.
    pub fn retirement_confirmed(&self) -> bool {
        self.receipt_accepted && self.cleanup == CheckpointCleanup::Confirmed
    }
}
impl BrowserEngine {
    /// Call only after the source helper has returned. Uses its exact owner and the next epoch.
    /// Manual is rejected before page access. An external receipt, never DOM text, accepts a summary.
    /// Once Send is attempted, all exits await owned-helper cleanup and report its proof explicitly.
    pub async fn settle_checkpoint(
        &self,
        request: CheckpointRequest,
        mut accepted: watch::Receiver<bool>,
        cancellation: Cancellation,
    ) -> Result<CheckpointSettlement> {
        let input = &request.turn;
        if input.interaction_mode == InteractionMode::Manual {
            return Err(Error::ManualDomForbidden);
        }
        if cancellation.is_cancelled() {
            return Err(Error::Cancelled);
        }
        if *accepted.borrow() {
            return Err(Error::Dom(
                "checkpoint receipt was already true before submission".into(),
            ));
        }
        if input.ownership.task_id != request.source_owner.task_id
            || input.ownership.epoch
                != request
                    .source_owner
                    .epoch
                    .checked_add(1)
                    .ok_or(Error::Ownership)?
        {
            return Err(Error::Ownership);
        }
        if input.turn_token.is_some()
            || !input.attachments.is_empty()
            || !crate::unique_literal_token(&input.full_prompt, &request.control_token)
        {
            return Err(Error::Dom("checkpoint requires one literal summary-only control token, no work turn token or attachments".into()));
        }
        let mut state = tokio::select! {biased;_=cancellation.cancelled()=>return Err(Error::Cancelled),state=self.state.lock()=>state};
        if state.cleanup_unconfirmed {
            return Err(Error::CleanupUnconfirmed);
        }
        let key = (input.ownership.task_id.clone(), input.ownership.epoch);
        if state.attempts.contains(&key) {
            return Err(Error::DuplicateSend);
        }
        let fresh =
            state.owner.is_none() && request.source_owner.epoch == 0 && state.attempts.is_empty();
        if !fresh && state.owner.as_ref() != Some(&request.source_owner) {
            return Err(Error::Ownership);
        }
        let prepared = self
            .prepare_submission(input, &cancellation, &mut state)
            .await?;
        if *accepted.borrow() {
            self.rollback_preparation(input, &prepared.baseline, &mut state)
                .await?;
            return Err(Error::Dom(
                "checkpoint receipt changed before its Send".into(),
            ));
        }
        if cancellation.is_cancelled() {
            self.rollback_preparation(input, &prepared.baseline, &mut state)
                .await?;
            return Err(Error::Cancelled);
        }
        state.owner = Some(input.ownership.clone());
        state.attempts.insert(key); // Same latch as normal submit, before potentially uncertain Send.
        // Keep the target quarantined if this future is dropped during Send/settlement/cleanup.
        state.cleanup_unconfirmed = true;
        let sent = self.send_prepared(input, &prepared, &cancellation).await;
        let mut outcome = if let Err(error) = sent {
            Some(if matches!(error, Error::Cancelled) {
                CheckpointOutcome::Cancelled
            } else {
                CheckpointOutcome::Failed(error.to_string())
            })
        } else {
            None
        };
        if outcome.is_none() {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(180);
            loop {
                if *accepted.borrow_and_update() {
                    outcome = Some(CheckpointOutcome::Accepted);
                    break;
                }
                tokio::select! {biased;
                    receipt=accepted.changed()=>{if receipt.is_err(){outcome=Some(CheckpointOutcome::ReceiptClosed);break}},
                    _=cancellation.cancelled()=>{outcome=Some(CheckpointOutcome::Cancelled);break},
                    _=tokio::time::sleep_until(deadline)=>{outcome=Some(CheckpointOutcome::ReceiptTimedOut);break},
                }
            }
        }
        // Fresh token: caller cancellation must not abandon cleanup. No spawned helper remains.
        let cleanup = match self
            .cleanup_after_send(&prepared.baseline, input, Some(&request.control_token))
            .await
        {
            Ok(()) => {
                state.cleanup_unconfirmed = false;
                CheckpointCleanup::Confirmed
            }
            Err(error) => {
                state.cleanup_unconfirmed = true;
                CheckpointCleanup::Unconfirmed {
                    reason: error.to_string(),
                }
            }
        };
        Ok(CheckpointSettlement {
            ownership: input.ownership.clone(),
            receipt_accepted: matches!(outcome, Some(CheckpointOutcome::Accepted))
                || *accepted.borrow(),
            outcome: outcome.unwrap(),
            cleanup,
        })
    }
    pub(super) async fn cleanup_after_send(
        &self,
        baseline: &crate::Snapshot,
        input: &TurnInput,
        control_token: Option<&str>,
    ) -> Result<()> {
        let document_id = &baseline.document_id;
        let cleanup = Cancellation::default();
        // Awaited CDP evaluations form an execution barrier after the synchronous Send operation.
        let probe = self
            .page(
                "checkpointStop",
                json!({"document_id":document_id}),
                &cleanup,
            )
            .await?;
        if probe != true {
            let final_state = self.snapshot(control_token, &cleanup).await?;
            let user = crate::new_turn_identity(
                &baseline.ids,
                final_state.users.iter().map(|u| u.id.as_str()),
            )?;
            let response = crate::new_turn_identity(
                &baseline.ids,
                final_state.responses.iter().map(|r| r.id.as_str()),
            )?;
            let own_user = user
                .as_ref()
                .and_then(|id| final_state.users.iter().find(|u| &u.id == id))
                .is_some_and(|u| {
                    control_token.is_some_and(|token| u.tokens == [token])
                        || u.text.as_deref() == Some(input.full_prompt.as_str())
                });
            if final_state.document_id != *document_id
                || final_state.running
                || !own_user
                || !response.as_ref().is_some_and(|id| {
                    final_state
                        .responses
                        .iter()
                        .any(|r| &r.id == id && r.complete)
                })
            {
                return Err(Error::Dom(
                    "owned checkpoint had no provable stop/final boundary".into(),
                ));
            }
        }
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        let mut idle = 0;
        loop {
            if self
                .page(
                    "checkpointIdle",
                    json!({"document_id":document_id}),
                    &cleanup,
                )
                .await?
                == true
            {
                idle += 1;
                if idle >= 2 {
                    return Ok(());
                }
            } else {
                idle = 0
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(Error::Dom(
                    "owned checkpoint generation did not become idle".into(),
                ));
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::{SinkExt, StreamExt};
    use std::sync::{Arc, Mutex};
    use tokio::{net::TcpListener, sync::Notify};
    struct Fixture {
        engine: Arc<BrowserEngine>,
        sent: Arc<Notify>,
        log: Arc<Mutex<Vec<String>>>,
        task: tokio::task::JoinHandle<()>,
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            self.task.abort();
        }
    }
    fn request() -> CheckpointRequest {
        CheckpointRequest {
            source_owner: Ownership {
                task_id: "thread".into(),
                epoch: 1,
            },
            turn: TurnInput {
                connector_name: None,
                full_prompt: "CHECKPOINT summary only: control_unique_123".into(),
                model: "chatgpt-web/gpt-6-sol".into(),
                effort: "high".into(),
                attachments: vec![],
                interaction_mode: InteractionMode::Automatic,
                ownership: Ownership {
                    task_id: "thread".into(),
                    epoch: 2,
                },
                turn_token: None,
            },
            control_token: "control_unique_123".into(),
        }
    }
    async fn fixture(cleanup_failure: bool, early_final: bool) -> Fixture {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!(
            "ws://127.0.0.1:{}/devtools/page/checkpoint-owned",
            listener.local_addr().unwrap().port()
        );
        let sent = Arc::new(Notify::new());
        let signal = sent.clone();
        let log = Arc::new(Mutex::new(Vec::new()));
        let recorded = log.clone();
        let task = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(socket).await.unwrap();
            let mut submitted = false;
            while let Some(Ok(message)) = ws.next().await {
                let v: serde_json::Value = serde_json::from_slice(&message.into_data()).unwrap();
                let expr = v["params"]["expression"].as_str().unwrap_or("");
                let op = [
                    "ready",
                    "account",
                    "capabilityState",
                    "capabilityPicker",
                    "model",
                    "verifyModel",
                    "snapshot",
                    "connectorSnapshot",
                    "focus",
                    "verifyPrompt",
                    "send",
                    "checkpointStop",
                    "checkpointIdle",
                ]
                .into_iter()
                .find(|op| expr.contains(&format!(")({},", json!(op))));
                let op = op.unwrap_or("other");
                recorded.lock().unwrap().push(op.into());
                if op == "checkpointStop" && cleanup_failure {
                    ws.send(
                        json!({"id":v["id"],"error":{"code":-1,"message":"cleanup refused"}})
                            .to_string()
                            .into(),
                    )
                    .await
                    .unwrap();
                    continue;
                }
                let value = match op {
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
                    "connectorSnapshot" => {
                        json!({"document_id":"doc","text":"","selected":[],"rows":[],"has_attachments":false})
                    }
                    "snapshot" if submitted && early_final => {
                        json!({"document_id":"doc","ids":["u","a"],"users":[{"id":"u","text":"CHECKPOINT summary only: control_unique_123","tokens":["control_unique_123"]}],"responses":[{"id":"a","text":"not a receipt","html":"<unsupported>not markdown</unsupported>","commentary":"","complete":true}],"running":false})
                    }
                    "snapshot" => {
                        json!({"document_id":"doc","ids":[],"users":[],"responses":[],"running":false})
                    }
                    "send" => {
                        submitted = true;
                        signal.notify_one();
                        json!(true)
                    }
                    "checkpointStop" => json!(!early_final),
                    _ => json!(true),
                };
                if ws
                    .send(
                        json!({"id":v["id"],"result":{"result":{"value":value}}})
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
        let engine = Arc::new(BrowserEngine::connect(&url).await.unwrap());
        engine.state.lock().await.owner = Some(request().source_owner);
        Fixture {
            engine,
            sent,
            log,
            task,
        }
    }
    #[tokio::test]
    async fn accepted_before_dom_final_stops_and_awaits_cleanup_once() {
        let f = fixture(false, false).await;
        let (tx, rx) = watch::channel(false);
        let engine = f.engine.clone();
        let work = tokio::spawn(async move {
            engine
                .settle_checkpoint(request(), rx, Cancellation::default())
                .await
        });
        f.sent.notified().await;
        tx.send(true).unwrap();
        let result = work.await.unwrap().unwrap();
        assert_eq!(result.outcome, CheckpointOutcome::Accepted);
        assert!(result.receipt_accepted);
        assert_eq!(result.cleanup, CheckpointCleanup::Confirmed);
        assert!(result.retirement_confirmed());
        {
            let log = f.log.lock().unwrap();
            assert_eq!(log.iter().filter(|op| op.as_str() == "send").count(), 1);
            assert_eq!(
                log.iter()
                    .filter(|op| op.as_str() == "checkpointIdle")
                    .count(),
                2
            );
        }
        let (_, rx) = watch::channel(false);
        assert!(matches!(
            f.engine
                .settle_checkpoint(request(), rx, Cancellation::default())
                .await,
            Err(Error::DuplicateSend)
        ));
    }
    #[tokio::test]
    async fn ordinary_cancel_after_send_waits_for_idle_before_reuse() {
        let f = fixture(false, false).await;
        let cancel = Cancellation::default();
        let token = cancel.clone();
        let engine = f.engine.clone();
        let (progress, _) = tokio::sync::mpsc::unbounded_channel();
        let work =
            tokio::spawn(async move { engine.submit(request().turn, progress, token).await });
        f.sent.notified().await;
        cancel.cancel();
        assert!(matches!(work.await.unwrap(), Err(Error::Cancelled)));
        let log = f.log.lock().unwrap();
        assert_eq!(
            log.iter()
                .filter(|op| op.as_str() == "checkpointStop")
                .count(),
            1
        );
        assert_eq!(
            log.iter()
                .filter(|op| op.as_str() == "checkpointIdle")
                .count(),
            2
        );
    }

    #[tokio::test]
    async fn ordinary_cleanup_failure_quarantines_the_next_epoch() {
        let f = fixture(true, false).await;
        let cancel = Cancellation::default();
        let token = cancel.clone();
        let engine = f.engine.clone();
        let (progress, _) = tokio::sync::mpsc::unbounded_channel();
        let work =
            tokio::spawn(async move { engine.submit(request().turn, progress, token).await });
        f.sent.notified().await;
        cancel.cancel();
        assert!(matches!(
            work.await.unwrap(),
            Err(Error::CleanupUnconfirmed)
        ));
        let mut next = request().turn;
        next.ownership.epoch = 3;
        let (progress, _) = tokio::sync::mpsc::unbounded_channel();
        assert!(matches!(
            f.engine
                .submit(next, progress, Cancellation::default())
                .await,
            Err(Error::CleanupUnconfirmed)
        ));
        assert_eq!(
            f.log
                .lock()
                .unwrap()
                .iter()
                .filter(|op| op.as_str() == "send")
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn fresh_compaction_page_starts_with_epoch_zero_without_a_source_send() {
        let f = fixture(false, false).await;
        f.engine.state.lock().await.owner = None;
        let mut request = request();
        request.source_owner.epoch = 0;
        request.turn.ownership.epoch = 1;
        let (accepted, receipt) = watch::channel(false);
        let engine = f.engine.clone();
        let work = tokio::spawn(async move {
            engine
                .settle_checkpoint(request, receipt, Cancellation::default())
                .await
        });
        tokio::time::timeout(Duration::from_secs(5), f.sent.notified())
            .await
            .expect("checkpoint must dispatch Send before the receipt is accepted");
        accepted.send(true).unwrap();
        let result = work.await.unwrap().unwrap();
        assert!(result.retirement_confirmed());
        assert_eq!(
            f.log
                .lock()
                .unwrap()
                .iter()
                .filter(|op| op.as_str() == "send")
                .count(),
            1
        );
    }
    #[tokio::test]
    async fn cancellation_after_send_awaits_cleanup_without_accepting() {
        let f = fixture(false, false).await;
        let (_tx, rx) = watch::channel(false);
        let cancel = Cancellation::default();
        let token = cancel.clone();
        let engine = f.engine.clone();
        let work =
            tokio::spawn(async move { engine.settle_checkpoint(request(), rx, token).await });
        f.sent.notified().await;
        cancel.cancel();
        let result = work.await.unwrap().unwrap();
        assert_eq!(result.outcome, CheckpointOutcome::Cancelled);
        assert!(!result.receipt_accepted);
        assert_eq!(result.cleanup, CheckpointCleanup::Confirmed);
    }
    #[tokio::test]
    async fn early_dom_final_never_substitutes_for_external_receipt() {
        let f = fixture(false, true).await;
        let (tx, rx) = watch::channel(false);
        let engine = f.engine.clone();
        let mut work = tokio::spawn(async move {
            engine
                .settle_checkpoint(request(), rx, Cancellation::default())
                .await
        });
        f.sent.notified().await;
        assert!(
            tokio::time::timeout(Duration::from_millis(100), &mut work)
                .await
                .is_err()
        );
        tx.send(true).unwrap();
        let result = work.await.unwrap().unwrap();
        assert!(result.receipt_accepted);
        assert_eq!(result.cleanup, CheckpointCleanup::Confirmed); // Final HTML is deliberately unsupported: no renderer was awaited.
    }
    #[tokio::test]
    async fn cleanup_failure_does_not_acknowledge_retirement_or_allow_another_epoch() {
        let f = fixture(true, false).await;
        let (tx, rx) = watch::channel(false);
        let engine = f.engine.clone();
        let work = tokio::spawn(async move {
            engine
                .settle_checkpoint(request(), rx, Cancellation::default())
                .await
        });
        f.sent.notified().await;
        tx.send(true).unwrap();
        let result = work.await.unwrap().unwrap();
        assert!(result.receipt_accepted);
        assert!(matches!(
            result.cleanup,
            CheckpointCleanup::Unconfirmed { .. }
        ));
        assert!(!result.retirement_confirmed());
        let mut next = request().turn;
        next.ownership.epoch = 3;
        let (progress, _) = tokio::sync::mpsc::unbounded_channel();
        assert!(matches!(
            f.engine
                .submit(next, progress, Cancellation::default())
                .await,
            Err(Error::CleanupUnconfirmed)
        ));
    }
    #[tokio::test]
    async fn owner_mismatch_and_manual_do_not_touch_page() {
        let f = fixture(false, false).await;
        let (_tx, rx) = watch::channel(false);
        let mut wrong = request();
        wrong.source_owner.task_id = "other".into();
        assert!(matches!(
            f.engine
                .settle_checkpoint(wrong, rx, Cancellation::default())
                .await,
            Err(Error::Ownership)
        ));
        let (_tx, rx) = watch::channel(false);
        let mut manual = request();
        manual.turn.interaction_mode = InteractionMode::Manual;
        assert!(matches!(
            f.engine
                .settle_checkpoint(manual, rx, Cancellation::default())
                .await,
            Err(Error::ManualDomForbidden)
        ));
        assert!(f.log.lock().unwrap().is_empty());
    }
    #[tokio::test]
    async fn closed_receipt_does_not_claim_acceptance() {
        let f = fixture(false, false).await;
        let (tx, rx) = watch::channel(false);
        let engine = f.engine.clone();
        let work = tokio::spawn(async move {
            engine
                .settle_checkpoint(request(), rx, Cancellation::default())
                .await
        });
        f.sent.notified().await;
        drop(tx);
        let result = work.await.unwrap().unwrap();
        assert_eq!(result.outcome, CheckpointOutcome::ReceiptClosed);
        assert!(!result.receipt_accepted);
        assert_eq!(result.cleanup, CheckpointCleanup::Confirmed);
    }
}
