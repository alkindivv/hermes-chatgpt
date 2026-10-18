use anyhow::{Context as _, Result, ensure};
use crate::{session::{ProbeKind, ProbeTarget, ProbeTicket}, ui::*};
use cw_bridge::SessionChangeGuard;
use cw_browser::{Cancellation, ConnectorReceipt, InteractionMode};
use cw_runtime::RuntimeFacade;
use gpui::Context;
use serde_json::{Value, json};

fn verified_result(manual: bool, health: Value, name: &str, receipt: Option<ConnectorReceipt>) -> Result<Value> {
    ensure!(health["ready"] == true, "The application's owned MCP runtime is not ready");
    if !manual {
        let selected = receipt.as_ref().context("The configured connector has not been selected and verified")?;
        ensure!(selected.name == name && !selected.document_id.is_empty() && selected.composer_cleared,
            "Connector verification did not prove the exact name and an empty composer");
    }
    Ok(json!({"ready":true,"runtime":health,"connectorSelected":!manual,
        "connectorName":name,"manualSelectionRequired":manual,"toolRoundtripVerified":false}))
}

impl Launcher {
    pub(crate) fn start_mcp_verification(&mut self, cx: &Context<Self>) -> Result<()> {
        ensure!(!self.busy() && self.pending_turns.is_empty() && self.pending_compactions.is_empty(),
            "Finish active work before checking the MCP connector");
        ensure!(self.runtime.is_some(), "The owned runtime is busy");
        let manual = self.manual();
        let name = self.connector_name();
        let id = self.selected_tab.clone();
        let snapshot = id.as_ref().and_then(|id| self.probe_snapshot(id, cx));
        let engine = if manual { None } else {
            ensure!(self.session_account.is_some(), "Verify the owned ChatGPT session before checking its connector");
            Some(self.tab().and_then(|tab| tab.engine.clone()).context("The owned browser is not ready")?)
        };
        self.persist(|state| state.mcp_setup_complete = Some(false))?;
        let guard = self.bridge.begin_session_change().map_err(anyhow::Error::msg)?;
        let (ticket, cancellation) = if manual { (None, Cancellation::default()) } else {
            let (id, epoch, url, available) = snapshot.as_ref().context("The browser tab is unavailable")?;
            let (ticket, cancellation) = self.probes.begin(ProbeKind::Connector,
                ProbeTarget { id, epoch:*epoch, url, available:*available }, true)
                .context("Wait for the owned ChatGPT page to finish loading")?;
            (Some(ticket), cancellation)
        };
        self.probe_inflight = ticket.is_some();
        self.verification_cancellation = Some(cancellation.clone());
        let mut runtime = self.runtime.take().expect("checked runtime");
        self.operation = Some(self.text("checkingLocalRuntime"));
        let sender = self.sender.clone();
        self.executor.spawn(async move {
            let mut cleanup_confirmed = true;
            let result: Result<Value> = async {
                ensure!(!cancellation.is_cancelled(), "MCP verification cancelled");
                let health = runtime.verify_mcp().await?;
                ensure!(health["ready"] == true, "The application's owned MCP runtime is not ready");
                let receipt = if let Some(engine) = engine {
                    match engine.verify_connector(&name, InteractionMode::Automatic, cancellation.clone()).await {
                        Ok(receipt) => Some(receipt),
                        Err(error) => {
                            cleanup_confirmed = !matches!(error, cw_browser::Error::CleanupUnconfirmed);
                            return Err(anyhow::Error::from(error));
                        }
                    }
                } else { None };
                ensure!(!cancellation.is_cancelled(), "MCP verification cancelled");
                verified_result(manual, health, &name, receipt)
            }.await;
            let _ = sender.send(UiEvent::McpVerified { runtime, ticket, guard,
                cleanup_confirmed, result:result.map_err(|error| error.to_string()) });
        });
        Ok(())
    }

    pub(crate) fn finish_mcp_verification(&mut self, runtime: RuntimeFacade, ticket: Option<ProbeTicket>,
        guard: SessionChangeGuard, result: Result<Value, String>, cleanup_confirmed: bool, cx: &Context<Self>) {
        self.runtime = Some(runtime);
        self.operation = None;
        self.probe_inflight = false;
        let cancelled = self.verification_cancellation.take().is_some_and(|signal| signal.is_cancelled());
        let current = if let Some(ticket) = &ticket {
            let snapshot = self.probe_snapshot(&ticket.id, cx);
            let target = snapshot.as_ref().map(|(id,epoch,url,available)| ProbeTarget { id,epoch:*epoch,url,available:*available });
            self.probes.finish(ticket, target, !self.manual())
        } else { self.manual() };
        if !cleanup_confirmed {
            if let Some(ticket) = &ticket {
                if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == ticket.id) {
                    tab.quarantined = true;
                    tab.status = "cleanup-unconfirmed".into();
                }
            }
            self.session_quarantine = Some(guard);
            self.error = Some("Connector cleanup could not be confirmed. The page remains reserved".into());
            return;
        }
        if cancelled || !current { return; }
        match result {
            Ok(value) if value["ready"] == true => {
                if let Err(error) = self.persist(|state| state.mcp_setup_complete = Some(true)) {
                    self.error = Some(error.to_string()); return;
                }
                self.log("info", "mcp.verified", if self.manual() {
                    "The owned Zero Risk runtime is healthy. Select the configured connector manually for every turn"
                } else {
                    "The owned runtime is healthy and the exact connector was selected and cleared without sending a message"
                });
            }
            Ok(_) => self.error = Some("MCP verification did not return a ready result".into()),
            Err(error) => { self.log("error", "mcp.verification-failed", &error); self.error = Some(error); }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn receipt(name: &str, cleared: bool) -> ConnectorReceipt {
        ConnectorReceipt { name:name.into(), document_id:"owned-document".into(), composer_cleared:cleared }
    }
    #[test]
    fn automatic_verification_requires_health_exact_selection_and_cleanup() {
        assert!(verified_result(false, json!({"ready":true}), "Codex Native2", None).is_err());
        assert!(verified_result(false, json!({"ready":false}), "Codex Native2", Some(receipt("Codex Native2",true))).is_err());
        assert!(verified_result(false, json!({"ready":true}), "Codex Native2", Some(receipt("Codex Other",true))).is_err());
        assert!(verified_result(false, json!({"ready":true}), "Codex Native2", Some(receipt("Codex Native2",false))).is_err());
        let verified = verified_result(false, json!({"ready":true}), "Codex Native2", Some(receipt("Codex Native2",true))).unwrap();
        assert_eq!(verified["connectorSelected"], true);
        assert_eq!(verified["toolRoundtripVerified"], false);
    }
    #[test]
    fn manual_verification_needs_only_fresh_local_health_and_keeps_manual_selection_explicit() {
        assert!(verified_result(true, json!({"ready":false}), "Codex Zero Risk", None).is_err());
        let verified = verified_result(true, json!({"ready":true}), "Codex Zero Risk", None).unwrap();
        assert_eq!(verified["ready"], true);
        assert_eq!(verified["manualSelectionRequired"], true);
        assert_eq!(verified["connectorSelected"], false);
    }
}
