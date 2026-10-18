//! Launcher integration for cancellable inspections of the application's own browser profile.
use crate::{session::{self, ProbeKind, ProbeObservation, ProbeTarget, ProbeTicket}, ui::*};
use anyhow::{Context as _, Result, ensure};
use cw_bridge::SessionChangeGuard;
use gpui::{Context, Window};

impl Launcher {
    pub(crate) fn probe_snapshot(&self, id: &str, cx: &Context<Self>) -> Option<(String, u64, String, bool)> {
        self.tabs.iter().find(|tab| tab.id == id).map(|tab| {
            let view = tab.webview.read(cx);
            (tab.id.clone(), tab.epoch, view.url(), tab.request_id.is_none()
                && !tab.close_requested && !tab.quarantined && !view.is_loading())
        })
    }

    pub(crate) fn reconcile_probe(&mut self, cx: &Context<Self>) {
        let Some(ticket) = self.probes.current() else { return; };
        let snapshot = self.probe_snapshot(&ticket.id, cx);
        if !self.probes.target_matches(target(&snapshot), !self.manual()) {
            // Keep the worker's admission lease until its completion event is consumed.
            self.probes.invalidate();
        }
    }

    pub(crate) fn start_probe(&mut self, id: &str, kind: ProbeKind, cx: &Context<Self>) -> Result<()> {
        ensure!(!self.manual(), "Zero Risk does not inspect browser sessions");
        ensure!(!self.busy() && self.pending_turns.is_empty() && self.pending_compactions.is_empty(),
            "Finish the current operation before checking the browser session");
        let snapshot = self.probe_snapshot(id, cx);
        let engine = self.tabs.iter().find(|tab| tab.id == id).and_then(|tab| tab.engine.clone())
            .context("The owned browser is still initializing")?;
        let guard = self.bridge.begin_session_change().map_err(anyhow::Error::msg)?;
        let (ticket, cancellation) = self.probes.begin(kind, target(&snapshot).context("The browser tab was closed")?, true)
            .context("Wait for the ChatGPT page to finish loading before checking the session")?;
        self.probe_inflight = true;
        if kind == ProbeKind::Account { self.operation = Some("Checking ChatGPT plan…".into()); }
        let sender = self.sender.clone();
        self.executor.spawn(async move {
            let result = session::inspect(&engine, kind, cancellation).await;
            let _ = sender.send(UiEvent::SessionProbe { ticket, result, guard });
        });
        Ok(())
    }

    pub(crate) fn forget_session_evidence(&mut self) -> Result<()> {
        self.probes.invalidate();
        self.bridge.clear_account_capabilities().map_err(anyhow::Error::msg)?;
        self.session_account = None;
        for tab in &mut self.tabs {
            tab.authenticated = false;
            tab.capabilities = None;
            // BrowserEngine retains its task/epoch ownership. Retire the page instead of relabeling it.
            if tab.owner_key.is_some() { tab.close_requested = true; }
        }
        self.persist(|state| {
            state.browser_smoke_passed = false;
            state.codex_catalog_verified = Some(false);
            state.bigger_context_available = Some(false);
            state.mcp_setup_complete = Some(false);
        })
    }

    pub(crate) fn finish_probe(
        &mut self, ticket: ProbeTicket, result: Result<ProbeObservation, String>,
        _guard: SessionChangeGuard, window: &mut Window, cx: &mut Context<Self>,
    ) {
        self.probe_inflight = false;
        if ticket.kind == ProbeKind::Account { self.operation = None; }
        let snapshot = self.probe_snapshot(&ticket.id, cx);
        let automatic = !self.manual();
        if !self.probes.finish(&ticket, target(&snapshot), automatic) { return; }
        let result = match result {
            Ok(ProbeObservation::SignedOut) => self.forget_session_evidence().map(|_| {
                self.log("info", "browser.session-checked", "The owned browser has no authenticated server session");
            }),
            Ok(ProbeObservation::Capabilities(inspection)) => self.accept_capabilities(&ticket.id, inspection, window, cx),
            Ok(ProbeObservation::Account(account)) => {
                if self.session_account.as_ref().is_none_or(|previous| previous.account.account_key != account.account_key) {
                    self.forget_session_evidence().and_then(|_| Err(anyhow::anyhow!("The account changed. Check the new browser session before enabling usage tracking")))
                } else if let Some(store) = self.limits_store.as_mut() {
                    store.configure(&account.account_key, account.limits_plan, now_ms())
                        .map_err(anyhow::Error::from)
                        .and_then(|snapshot| Ok(serde_json::to_value(snapshot)?)).map(|value| self.limits = value)
                } else {
                    Err(anyhow::anyhow!("The private limits store could not be opened"))
                }
            }
            Err(message) => Err(anyhow::Error::msg(message)),
        };
        if let Err(error) = result {
            self.log("warning", "browser.inspection-failed", &error.to_string());
            self.error = Some(error.to_string());
        }
    }

    pub(crate) fn accept_capabilities(&mut self, id: &str, inspection: cw_browser::CapabilityInspection, window: &mut Window, cx: &mut Context<Self>) -> Result<()> {
        ensure!(inspection.account.server_authenticated && !inspection.account.account_key.is_empty(), "Server account evidence is missing");
        let account_changed = self.session_account.as_ref().is_some_and(|previous|
            previous.account.account_key != inspection.account.account_key);
        if account_changed { self.forget_session_evidence()?; }
        let supports_bigger = inspection.routes.iter().any(|route| {
            cw_contracts::models::supports_chat_gpt_web_bigger_context(route.route.backend_model, route.route.adapter_effort, route.route.model_family)
        });
        let mut capabilities = inspection.capabilities.clone();
        capabilities.experimental_bigger_context = None;
        capabilities.zero_risk_pro_enabled = None;
        self.bridge.set_account_capabilities(capabilities).map_err(anyhow::Error::msg)?;
        let reminder = cw_contracts::state::next_session_refresh_reminder_at(now_ms() as f64)?;
        self.persist(|state| {
            state.bigger_context_available = Some(supports_bigger);
            if state.session_refresh_reminder_at.is_none() { state.session_refresh_reminder_at = Some(reminder); }
        })?;
        self.session_account = Some(inspection.clone());
        if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == id && !tab.close_requested) {
            tab.authenticated = true;
            tab.capabilities = Some(inspection.clone());
            tab.status = "ready".into();
        }
        if !self.tabs.iter().any(|tab| !tab.close_requested) {
            let fresh = self.create_browser(window, cx);
            self.selected_tab = Some(fresh);
        }
        self.log("info", "browser.models-verified", "The account and model controls were observed in the owned browser");
        Ok(())
    }

    pub(crate) fn commit_runtime_state(&mut self, mut next: cw_contracts::LauncherState) -> Result<()> {
        ensure!(!self.busy(), "Finish the current operation before changing runtime settings");
        let _guard = self.bridge.begin_session_change().map_err(anyhow::Error::msg)?;
        let mode_changed = next.browser_interaction_mode != self.state.browser_interaction_mode;
        next.codex_catalog_verified = Some(false);
        if mode_changed {
            next.experimental_bigger_context &= next.browser_interaction_mode != cw_contracts::BrowserInteractionMode::Manual;
        }
        session::apply_launcher_settings(&self.bridge, &next).map_err(anyhow::Error::msg)?;
        if let Err(error) = self.profile.write_json("user-data/launcher-state.json", &next) {
            session::apply_launcher_settings(&self.bridge, &self.state).map_err(anyhow::Error::msg)?;
            return Err(error);
        }
        self.state = next;
        if mode_changed { self.forget_session_evidence()?; }
        Ok(())
    }

    pub(crate) fn sync_catalog_status(&mut self) -> bool {
        let verified = self.state.core_setup_complete == Some(true) && self.bridge.catalog_status().verified;
        if self.state.codex_catalog_verified == Some(verified) { return false; }
        if let Err(error) = self.persist(|state| {
            state.codex_catalog_verified = Some(verified);
            if verified { state.codex_restart_required = Some(false); }
        }) { self.error = Some(error.to_string()); }
        true
    }
}

fn target(snapshot: &Option<(String, u64, String, bool)>) -> Option<ProbeTarget<'_>> {
    snapshot.as_ref().map(|(id, epoch, url, available)| ProbeTarget { id, epoch: *epoch, url, available: *available })
}
