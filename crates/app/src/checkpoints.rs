//! Retained checkpoint decisions require both semantic and physical source settlement.
use crate::ui::{Launcher, ManualCheckpoint, UiEvent, now_ms};
use cw_bridge::{CompactionRequest, ExecutionMode};
use cw_browser::{Cancellation, CheckpointCleanup, CheckpointOutcome, CheckpointRequest, InteractionMode, Ownership, TurnInput};
use gpui::{ClipboardItem, Context, Window};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SourceStatus {
    Fresh,
    Running,
    Manual,
    Settled,
    Missing,
    Quarantined,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum NextAction {
    Wait,
    CancelSource,
    PrepareCheckpoint,
    CompleteManual,
    RetireCancelled,
    Quarantine,
}

fn next_action(
    source: SourceStatus,
    source_finished: bool,
    reuse_manual: bool,
    accepted: bool,
    cancelled: bool,
) -> NextAction {
    if matches!(source, SourceStatus::Missing | SourceStatus::Quarantined) {
        return NextAction::Quarantine;
    }
    if reuse_manual {
        if !matches!(source, SourceStatus::Manual | SourceStatus::Settled) {
            return NextAction::Quarantine;
        }
        return if cancelled {
            NextAction::RetireCancelled
        } else if source_finished && accepted {
            NextAction::CompleteManual
        } else { NextAction::Wait };
    }
    if source == SourceStatus::Running {
        return if cancelled { NextAction::CancelSource } else { NextAction::Wait };
    }
    if cancelled { return NextAction::RetireCancelled; }
    if source_finished { NextAction::PrepareCheckpoint } else { NextAction::Wait }
}

impl Launcher {
    pub(crate) fn poll_checkpoints(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let mut changed = false;
        for tab in &mut self.tabs {
            let Some(manual) = &tab.manual_checkpoint else { continue; };
            if !manual.sent && now_ms() > crate::presentation::manual_deadline_ms(true, manual.copied_at_ms) {
                self.bridge.cancel(&manual.request.request_id);
            }
            let cancelled = manual.request.cancellation.borrow().clone();
            let accepted = *manual.request.accepted.borrow();
            if accepted || cancelled.is_some() {
                let manual = tab.manual_checkpoint.take().expect("selected manual checkpoint");
                // Manual mode never started an automatic helper; only the actual MCP receipt accepts it.
                let result = cancelled.map_or(Ok(()), Err);
                let _ = manual.request.completion.send(result.clone());
                tab.last_request_id = tab.request_id.take();
                tab.status = if result.is_ok() { "completed" } else { "cancelled" }.into();
                changed = true;
            }
        }

        let Some(request) = self.pending_compactions.pop_front() else { return changed; };
        let source_index = request.source_request_id.as_ref().and_then(|source| {
            self.tabs.iter().position(|tab| tab.request_id.as_ref() == Some(source) || tab.last_request_id.as_ref() == Some(source))
        });
        let source = if request.source_request_id.is_none() {
            SourceStatus::Fresh
        } else if let Some(index) = source_index {
            let tab = &self.tabs[index];
            if tab.quarantined { SourceStatus::Quarantined }
            else if tab.manual.is_some() { SourceStatus::Manual }
            else if tab.request_id.is_some() { SourceStatus::Running }
            else { SourceStatus::Settled }
        } else { SourceStatus::Missing };
        let cancelled = request.cancellation.borrow().clone();
        let action = next_action(source, *request.source_finished.borrow(), request.reuse_manual_response, *request.accepted.borrow(), cancelled.is_some());
        match action {
            NextAction::Wait | NextAction::CancelSource => {
                if action == NextAction::CancelSource {
                    if let Some(signal) = source_index.and_then(|index| self.tabs[index].local_cancellation.as_ref()) {
                        signal.cancel();
                    }
                }
                self.pending_compactions.push_back(request);
                return changed;
            }
            NextAction::Quarantine => {
                self.bridge.cancel(&request.request_id);
                if let Some(index) = source_index { self.tabs[index].quarantined = true; }
                // A missing or unconfirmed source is not a physical-cleanup acknowledgment.
                self.unresolved_checkpoint_retirements.push(request.completion);
                self.error = Some("The retained source browser has no confirmed cleanup boundary; its checkpoint cannot be released".into());
                return true;
            }
            NextAction::RetireCancelled | NextAction::CompleteManual => {
                if source == SourceStatus::Manual {
                    if let Some(index) = source_index {
                        let tab = &mut self.tabs[index];
                        tab.last_request_id = tab.request_id.take();
                        tab.manual = None;
                        tab.status = if action == NextAction::CompleteManual { "completed" } else { "cancelled" }.into();
                    }
                }
                let result = if action == NextAction::CompleteManual { Ok(()) } else { Err(cancelled.unwrap_or_else(|| "Checkpoint cancelled".into())) };
                let _ = request.completion.send(result);
                return true;
            }
            NextAction::PrepareCheckpoint => {}
        }
        let index = source_index.or_else(|| self.tabs.iter().position(|tab| {
            tab.request_id.is_none() && tab.owner_key.is_none() && !tab.quarantined && !tab.close_requested
        }));
        let Some(index) = index else {
            if self.tabs.len() >= 5 {
                let recyclable = self.tabs.iter().position(|tab| {
                    tab.request_id.is_none() && !tab.quarantined && !self.pending_compactions.iter().any(|pending| {
                        pending.source_request_id.as_ref().is_some_and(|source| tab.last_request_id.as_ref() == Some(source))
                    })
                });
                if let Some(index) = recyclable { self.tabs.remove(index); }
            }
            if self.tabs.len() < 5 { self.create_browser(window, cx); changed = true; }
            self.pending_compactions.push_back(request);
            return changed;
        };
        if self.tabs[index].engine.is_none() {
            self.pending_compactions.push_back(request);
            return changed;
        }
        if request.mode == ExecutionMode::Manual {
            cx.write_to_clipboard(ClipboardItem::new_string(request.prompt.clone()));
            let tab = &mut self.tabs[index];
            tab.request_id = Some(request.request_id.clone());
            tab.owner_key = Some(format!("checkpoint:{}", request.request_id));
            tab.status = "waiting".into();
            tab.visible = true;
            if tab.webview.read(cx).url().starts_with("about:") {
                tab.webview.update(cx, |view, _| view.load_url(crate::backend::chat_url(false)));
            }
            tab.manual_checkpoint = Some(ManualCheckpoint { request, copied_at_ms: now_ms(), sent: false });
            self.selected_tab = Some(tab.id.clone());
            self.surface = cw_contracts::Surface::Browser;
        } else {
            self.start_checkpoint(index, request);
        }
        true
    }

    fn start_checkpoint(&mut self, index: usize, request: CompactionRequest) {
        let connector_name = self.connector_name();
        if let Err(error) = self.bridge.confirm_compaction_source_settled(&request.request_id, &request.owner) {
            let _ = request.completion.send(Err(error.clone()));
            self.error = Some(error);
            return;
        }
        let fresh = request.source_request_id.is_none();
        let tab = &mut self.tabs[index];
        let source_owner = Ownership { task_id: request.owner.thread_id.clone(), epoch: tab.epoch };
        let Some(epoch) = tab.epoch.checked_add(1) else {
            let _ = request.completion.send(Err("Browser epoch exhausted".into()));
            self.error = Some("Browser epoch exhausted".into());
            return;
        };
        tab.epoch = epoch;
        tab.request_id = Some(request.request_id.clone());
        tab.current_model = Some(request.model.clone());
        if fresh { tab.owner_key = Some(format!("checkpoint:{}", request.request_id)); }
        tab.status = "compacting".into();
        let cancellation = Cancellation::default();
        tab.local_cancellation = Some(cancellation.clone());
        if self.state.show_browser_during_turns {
            tab.visible = true;
            self.selected_tab = Some(tab.id.clone());
            self.surface = cw_contracts::Surface::Browser;
        }
        let id = tab.id.clone();
        let request_id = request.request_id.clone();
        let engine = tab.engine.clone().expect("initialized checkpoint target");
        let turn = TurnInput {
            connector_name: Some(connector_name),
            full_prompt: request.prompt,
            model: request.model.clone(),
            effort: request.effort.unwrap_or_else(|| crate::backend::default_effort(&request.model).into()),
            attachments: vec![],
            interaction_mode: InteractionMode::Automatic,
            ownership: Ownership { task_id: request.owner.thread_id, epoch },
            turn_token: None,
        };
        let tx = self.sender.clone();
        self.executor.spawn(async move {
            let mut watch = request.cancellation;
            let signal = cancellation.clone();
            let monitor = tokio::spawn(async move {
                let _ = watch.wait_for(|reason| reason.is_some()).await;
                signal.cancel();
            });
            let outcome = async {
                if fresh { engine.navigate_with_cancellation(crate::backend::chat_url(false), cancellation.clone()).await?; }
                engine.settle_checkpoint(
                    CheckpointRequest { source_owner, turn, control_token: request.control_token },
                    request.accepted, cancellation,
                ).await
            }.await;
            monitor.abort();
            let (result, unsettled) = match outcome {
                Ok(settlement) if settlement.cleanup == CheckpointCleanup::Confirmed => {
                    let result = if settlement.retirement_confirmed() && settlement.outcome == CheckpointOutcome::Accepted {
                        Ok(())
                    } else { Err(format!("Checkpoint did not complete: {:?}", settlement.outcome)) };
                    let _ = request.completion.send(result.clone());
                    (result, None)
                }
                Ok(settlement) => (Err(format!("Checkpoint cleanup is unconfirmed: {:?}", settlement.cleanup)), Some(request.completion)),
                Err(cw_browser::Error::CleanupUnconfirmed) => (Err("Owned browser cleanup is unconfirmed".into()), Some(request.completion)),
                Err(error) => {
                    let result = Err(error.to_string());
                    let _ = request.completion.send(result.clone());
                    (result, None)
                }
            };
            let _ = tx.send(UiEvent::CheckpointEnded { id, request_id, epoch, result, unsettled });
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semantic_source_completion_never_releases_a_running_or_unconfirmed_helper() {
        assert_eq!(next_action(SourceStatus::Running, true, false, false, false), NextAction::Wait);
        assert_eq!(next_action(SourceStatus::Quarantined, true, false, true, false), NextAction::Quarantine);
        assert_eq!(next_action(SourceStatus::Missing, true, false, false, false), NextAction::Quarantine);
        assert_eq!(next_action(SourceStatus::Settled, false, false, false, false), NextAction::Wait);
        assert_eq!(next_action(SourceStatus::Settled, true, false, false, false), NextAction::PrepareCheckpoint);
    }

    #[test]
    fn cancellation_requires_source_cleanup_before_retirement() {
        assert_eq!(next_action(SourceStatus::Running, false, false, false, true), NextAction::CancelSource);
        assert_eq!(next_action(SourceStatus::Quarantined, true, false, false, true), NextAction::Quarantine);
        assert_eq!(next_action(SourceStatus::Settled, false, false, false, true), NextAction::RetireCancelled);
        assert_eq!(next_action(SourceStatus::Fresh, true, false, false, true), NextAction::RetireCancelled);
    }

    #[test]
    fn manual_retained_checkpoint_uses_the_existing_response_without_a_second_prompt() {
        assert_eq!(next_action(SourceStatus::Manual, true, true, false, false), NextAction::Wait);
        assert_eq!(next_action(SourceStatus::Manual, true, true, true, false), NextAction::CompleteManual);
        assert_eq!(next_action(SourceStatus::Manual, false, true, false, true), NextAction::RetireCancelled);
        assert_eq!(next_action(SourceStatus::Fresh, true, false, false, false), NextAction::PrepareCheckpoint);
        assert_eq!(next_action(SourceStatus::Running, true, true, true, false), NextAction::Quarantine);
    }
}
