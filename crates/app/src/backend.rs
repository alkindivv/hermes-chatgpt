use crate::ui::*;
use anyhow::{Context as _, Result, bail, ensure};
use base64::Engine;
use cw_bridge::{BrowserRequest, BrowserTurn, ExecutionMode};
use cw_browser::{
    Attachment, BrowserEngine, Cancellation, InteractionMode, Ownership, Progress, TurnInput,
    TurnResult,
};
use cw_contracts::{BrowserInteractionMode, Surface};
use gpui::{AppContext, ClipboardItem, Context, Window};
use serde_json::{Value, json};
use std::{fs, io::Write, os::unix::fs::OpenOptionsExt, sync::Arc, time::Duration};

impl Launcher {
    pub(crate) fn connector_name(&self) -> String {
        let key = if self.manual() {
            "manualConnectorName"
        } else {
            "automaticConnectorName"
        };
        self.state
            .extra
            .get(key)
            .and_then(Value::as_str)
            .unwrap_or(if self.manual() {
                "Codex Zero Risk"
            } else {
                "Codex Native2"
            })
            .to_owned()
    }

    pub(crate) fn perform(
        &mut self,
        action: Action,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        if self.session_job.is_some() && !matches!(action,
            Action::Navigate(_) | Action::ToggleSidebar | Action::DismissError | Action::External(_)
            | Action::SelectTab(_) | Action::PasskeyContinue | Action::CancelSession) {
            bail!("Finish or cancel the current browser session operation first");
        }
        if self.probe_inflight && !matches!(action,
            Action::Navigate(_) | Action::ToggleSidebar | Action::DismissError | Action::CancelTurns
            | Action::External(_) | Action::SelectTab(_) | Action::CloseTab(_)
            | Action::Back | Action::Forward | Action::Reload | Action::Zoom(_)) {
            bail!("Wait for the browser inspection to finish, or cancel it first");
        }
        if self.runtime.is_none()
            && !matches!(
                action,
                Action::Navigate(_)
                    | Action::ToggleSidebar
                    | Action::DismissError
                    | Action::CancelTurns
                    | Action::External(_)
                    | Action::SelectTab(_)
            )
        {
            bail!("Wait for the current runtime operation to finish");
        }
        match action {
            Action::DismissError => self.error = None,
            Action::Navigate(surface) => {
                self.surface = surface;
                if self.compact {
                    self.state.sidebar_open = false;
                }
            }
            Action::ToggleSidebar => self.persist(|s| s.sidebar_open = !s.sidebar_open)?,
            Action::LanguageMenu => self.language_menu = !self.language_menu,
            Action::Language(language) => {
                ensure!(
                    crate::assets::LANGUAGES.get(&language).is_some(),
                    "Unknown language"
                );
                if self.state.onboarding_complete {
                    self.persist(|s| s.language = Some(language.clone()))?;
                }
                self.language = language;
                self.language_menu = false;
            }
            Action::WelcomeNext => match self.stage {
                0 => {
                    let language = self.language.clone();
                    self.persist(|s| s.language = Some(language))?;
                    self.stage = 1;
                }
                1 => self.stage = 2,
                _ => {
                    ensure!(
                        crate::presentation::can_finish_welcome(
                            self.state.github_opened,
                            self.state.x_opened,
                            self.busy()
                        ),
                        "Complete the welcome steps first"
                    );
                    let language = self.language.clone();
                    let mode = self.selected_mode;
                    self.persist(|s| {
                        s.language = Some(language);
                        s.browser_interaction_mode = mode;
                        s.onboarding_complete = true;
                    })?;
                    self.surface = if mode == BrowserInteractionMode::Manual {
                        Surface::Mcp
                    } else {
                        Surface::Setup
                    };
                    self.set_bridge_mode(mode)?;
                }
            },
            Action::WelcomeBack => self.stage = self.stage.saturating_sub(1),
            Action::Social(target) => {
                let url = match target {
                    "github" => GITHUB_URL,
                    "x" => X_URL,
                    _ => bail!("Unknown social target"),
                };
                cx.open_url(url);
                self.persist(|s| {
                    if target == "github" {
                        s.github_opened = true
                    } else {
                        s.x_opened = true
                    }
                })?;
            }
            Action::External(url) => {
                ensure!(
                    [GITHUB_URL, X_URL, CONNECTORS_URL, TUNNELS_URL, KEYS_URL, cw_contracts::limits::SOURCE_URL].contains(&url),
                    "External URL is not in the launcher allowlist"
                );
                cx.open_url(url);
            }
            Action::Mode(mode) => {
                if !self.state.onboarding_complete {
                    self.selected_mode = mode;
                    return Ok(());
                }
                ensure!(
                    self.bridge.counters().active_turns == 0,
                    "Finish or cancel active tasks before changing interaction mode"
                );
                let mut next = self.state.clone();
                next.browser_interaction_mode = mode;
                next.experimental_bigger_context &= mode != BrowserInteractionMode::Manual;
                self.commit_runtime_state(next)?;
                self.selected_mode = mode;
                let suffix = self
                    .connector_name()
                    .strip_prefix("Codex ")
                    .unwrap_or("Native2")
                    .to_owned();
                self.connector_suffix
                    .update(cx, |input, cx| input.set_value(suffix, cx));
                if self.state.core_setup_complete == Some(true) {
                    self.surface = Surface::Mcp;
                }
            }
            Action::Preference(key, enabled) => {
                ensure!(
                    self.bridge.counters().active_turns == 0,
                    "Finish or cancel active tasks before changing runtime preferences"
                );
                match key {
                    "keepRunningOnClose" | "showBrowserDuringTurns" => {
                        ensure!(
                            key != "showBrowserDuringTurns" || !self.manual(),
                            "Zero Risk always requires a visible browser"
                        );
                        self.persist(|s| {
                            let _ = s.set_preference(key, &json!(enabled));
                        })?;
                    }
                    "autoStart" => {
                        crate::platform::set_autostart(enabled, &self.profile)?;
                        self.persist(|s| s.auto_start = enabled)?;
                    }
                    _ => {
                        ensure!(
                            self.state.core_setup_complete == Some(true),
                            "Complete core setup before changing this preference"
                        );
                        ensure!(
                            !self.manual()
                                || matches!(key, "composerPredictionsEnabled" | "useSavedChats" | "zeroRiskProEnabled"),
                            "This preference is unavailable in Zero Risk"
                        );
                        if key == "experimentalBiggerContext" && enabled {
                            ensure!(
                                self.state.bigger_context_available == Some(true),
                                "Bigger Context requires verified model support"
                            );
                        }
                        let mut value = serde_json::to_value(&self.state)?;
                        ensure!(
                            value.get(key).is_some_and(Value::is_boolean),
                            "Unknown runtime preference"
                        );
                        value[key] = json!(enabled);
                        let next = cw_contracts::LauncherState::normalize(value)?;
                        self.commit_runtime_state(next)?;
                    }
                }
            }
            Action::OpenBrowser => {
                if self.tabs.is_empty() {
                    self.create_browser(window, cx);
                }
                let saved = self.state.use_saved_chats;
                let fixture = self.fixture_url.clone();
                if let Some(tab) = self.tab_mut() {
                    tab.visible = true;
                    if tab.webview.read(cx).url().starts_with("about:") && tab.engine.is_some() {
                        let url = fixture.unwrap_or_else(|| chat_url(saved).into());
                        tab.webview.update(cx, |view, _| view.load_url(&url));
                    }
                }
                self.surface = Surface::Browser;
                if self.compact {
                    self.state.sidebar_open = false;
                }
            }
            Action::CheckLogin => {
                let id = self.tab().context("Browser is not available")?.id.clone();
                self.start_probe(&id, crate::session::ProbeKind::Capabilities, cx)?;
            }
            Action::PasskeyStart => self.start_passkey()?,
            Action::PasskeyContinue => {
                self.session_job.as_mut().context("No passkey login is waiting")?.continue_login()?;
                self.operation = Some(self.text("passkeyImporting"));
            }
            Action::CancelSession => {
                if let Some(job) = self.session_job.as_mut() { job.cancel(); }
            }
            Action::Back | Action::Forward | Action::Reload => {
                self.probes.invalidate();
                let tab = self
                    .tab()
                    .ok_or_else(|| anyhow::anyhow!("Browser is not available"))?;
                ensure!(
                    tab.request_id.is_none(),
                    "Navigation is locked during an active task"
                );
                tab.webview.update(cx, |view, _| match action {
                    Action::Back => view.go_back(),
                    Action::Forward => view.go_forward(),
                    _ => view.reload(),
                });
            }
            Action::Zoom(change) => {
                if let Some(tab) = self.tab_mut() {
                    tab.zoom = if change == 0 {
                        1.
                    } else {
                        (tab.zoom + f32::from(change) * 0.1).clamp(0.5, 2.)
                    };
                    tab.webview
                        .update(cx, |view, _| view.set_zoom_factor(tab.zoom as f64));
                }
            }
            Action::SelectTab(id) => {
                ensure!(self.tabs.iter().any(|t| t.id == id), "Unknown browser tab");
                self.selected_tab = Some(id);
                self.surface = Surface::Browser;
            }
            Action::CloseTab(id) => {
                if self.probes.current().is_some_and(|ticket| ticket.id == id) { self.probes.invalidate(); }
                if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == id) {
                    ensure!(!tab.quarantined, "Browser cleanup is unconfirmed. This tab remains reserved until the native app closes");
                    tab.close_requested = true;
                    if let Some(request) = &tab.request_id {
                        self.bridge.cancel(request);
                    }
                    if let Some(signal) = &tab.local_cancellation { signal.cancel(); }
                    for checkpoint in &self.pending_compactions {
                        if checkpoint.source_request_id.as_ref().is_some_and(|source| tab.request_id.as_ref() == Some(source) || tab.last_request_id.as_ref() == Some(source)) {
                            self.bridge.cancel(&checkpoint.request_id);
                        }
                    }
                }
                // poll removes the view only after its owned helper has reported cleanup.
            }
            Action::Doctor => {
                let native = self
                    .runtime
                    .as_ref()
                    .context("Runtime operation is still running")?
                    .diagnostics();
                let counters = self.bridge.counters();
                let auth = self.tabs.iter().any(|t| t.authenticated);
                self.doctor = Some(
                    json!({"ok":auth&&self.state.core_setup_complete==Some(true),"native":native,"checks":[
                    {"ok":true,"message":format!("Native Responses listener: {}",self.bridge.base_url())},
                    {"ok":true,"message":format!("Isolated Codex home: {}",self.profile.codex_home().display())},
                    {"ok":self.tabs.iter().any(|t|t.engine.is_some()),"message":"Application-owned Chromium target"},
                    {"ok":auth,"message":if auth{"Server-authenticated ChatGPT session verified"}else{"ChatGPT sign-in has not been verified in this profile"}},
                    {"ok":self.state.core_setup_complete==Some(true),"message":"Isolated Codex route installation"},
                    {"ok":false,"message":"Public MCP connector and tunnel require their own live acceptance test"}
                ],"bridge":counters}),
                );
                self.log(
                    "info",
                    "diagnostics.completed",
                    "Local checks completed; account and tunnel checks remain explicit",
                );
            }
            Action::CancelTurns => {
                self.probes.invalidate();
                if let Some(signal) = &self.verification_cancellation { signal.cancel(); }
                for tab in &self.tabs {
                    if let Some(cancellation) = &tab.local_cancellation {
                        cancellation.cancel();
                    }
                }
                let ids: Vec<_> = self
                    .tabs
                    .iter()
                    .filter_map(|t| t.request_id.clone())
                    .chain(self.pending_turns.iter().map(|t| t.request_id.clone()))
                    .chain(self.pending_compactions.iter().map(|t| t.request_id.clone()))
                    .collect();
                for id in &ids {
                    self.bridge.cancel(id);
                }
                self.log(
                    "info",
                    "turns.cancelled",
                    &format!("Cancelled {} application-owned turns", ids.len()),
                );
            }
            Action::ExportLogs => {
                let name = format!(
                    "user-data/diagnostic-{}.json",
                    chrono::Utc::now().format("%Y%m%d-%H%M%S")
                );
                self.profile.write_json(&name, &self.logs)?;
                let path = self.profile.path(name)?;
                cx.write_to_clipboard(ClipboardItem::new_string(path.display().to_string()));
                self.log("info", "logs.exported", "Diagnostic file path copied");
            }
            Action::McpStep(step) => {
                ensure!(step <= 2, "Invalid MCP guide step");
                self.persist(|s| s.mcp_guide_step = step)?;
            }
            Action::ChangeConnector => {
                let suffix = self.connector_suffix.read(cx).value();
                let suffix = suffix.trim();
                ensure!(
                    !suffix.is_empty()
                        && suffix.chars().count() <= 74
                        && !suffix.chars().any(char::is_control),
                    "Invalid plugin name"
                );
                let name = format!("Codex {suffix}");
                let manual = self.manual();
                let other = self
                    .state
                    .extra
                    .get(if manual {
                        "automaticConnectorName"
                    } else {
                        "manualConnectorName"
                    })
                    .and_then(Value::as_str)
                    .unwrap_or(if manual {
                        "Codex Native2"
                    } else {
                        "Codex Zero Risk"
                    });
                ensure!(
                    name != other,
                    "Automatic and Zero Risk require distinct plugin names"
                );
                self.persist(|s| {
                    s.extra.insert(
                        if manual {
                            "manualConnectorName"
                        } else {
                            "automaticConnectorName"
                        }
                        .into(),
                        json!(name),
                    );
                    s.mcp_setup_complete = Some(false);
                })?;
                self.surface = Surface::Mcp;
            }
            Action::CopyManual => {
                let tab = self
                    .tab_mut()
                    .ok_or_else(|| anyhow::anyhow!("No selected browser task"))?;
                if let Some(checkpoint) = tab.manual_checkpoint.as_mut() {
                    ensure!(!checkpoint.sent, "This checkpoint was already confirmed sent");
                    cx.write_to_clipboard(ClipboardItem::new_string(checkpoint.request.prompt.clone()));
                    checkpoint.copied_at_ms = now_ms();
                    return Ok(());
                }
                let manual = tab
                    .manual
                    .as_mut()
                    .ok_or_else(|| anyhow::anyhow!("No manual prompt is waiting"))?;
                ensure!(!manual.sent, "This prompt was already confirmed sent");
                cx.write_to_clipboard(ClipboardItem::new_string(manual.turn.task.prompt.clone()));
                manual.copied_at_ms = now_ms();
            }
            Action::ConfirmManual => {
                let tab = self
                    .tab()
                    .ok_or_else(|| anyhow::anyhow!("No selected browser task"))?;
                if let Some(checkpoint) = tab.manual_checkpoint.as_ref() {
                    ensure!(!checkpoint.sent && now_ms() <= crate::presentation::manual_deadline_ms(true, checkpoint.copied_at_ms), "Manual checkpoint confirmation expired");
                    self.bridge.confirm_compaction_source_settled(&checkpoint.request.request_id, &checkpoint.request.owner).map_err(anyhow::Error::msg)?;
                    self.tab_mut().and_then(|tab| tab.manual_checkpoint.as_mut()).expect("selected checkpoint").sent = true;
                    return Ok(());
                }
                let manual = tab
                    .manual
                    .as_ref()
                    .ok_or_else(|| anyhow::anyhow!("No manual prompt is waiting"))?;
                ensure!(
                    !manual.sent
                        && now_ms()
                            <= crate::presentation::manual_deadline_ms(false, manual.copied_at_ms),
                    "Manual confirmation expired"
                );
                self.bridge
                    .confirm_manual(&manual.turn.request_id, &manual.turn.owner)
                    .map_err(anyhow::Error::msg)?;
                if let Some(manual) = self.tab_mut().and_then(|t| t.manual.as_mut()) {
                    manual.sent = true;
                }
            }
            Action::DismissReminder => {
                let date = cw_contracts::state::next_session_refresh_reminder_at(now_ms() as f64)?;
                self.persist(|s| s.session_refresh_reminder_at = Some(date))?;
            }
            Action::Logout => self.start_logout()?,
            Action::Install => self.install_route()?,
            Action::RemoveIntegration => {
                ensure!(
                    self.bridge.counters().active_turns == 0,
                    "Finish or cancel tasks before removing the route"
                );
                self.runtime
                    .as_mut()
                    .context("Runtime operation is still running")?
                    .dispatch("integration.restore", Value::Null)?;
                self.persist(|s| {
                    s.core_setup_complete = Some(false);
                    s.codex_catalog_verified = Some(false);
                })?;
                self.bridge.disable_native_passthrough();
            }
            Action::Smoke => self.smoke_test(window, cx)?,
            Action::SaveMcp => self.save_mcp(cx)?,
            Action::VerifyMcp => self.start_mcp_verification(cx)?,
            Action::Tracking => self.refresh_limits(cx)?,
            Action::RefreshLimits => {
                let store = self.limits_store.as_mut().context("The private limits store could not be opened")?;
                self.limits = serde_json::to_value(store.snapshot(now_ms())?)?;
            }
        }
        Ok(())
    }

    fn set_bridge_mode(&self, mode: BrowserInteractionMode) -> Result<()> {
        let mut state = self.state.clone();
        state.browser_interaction_mode = mode;
        crate::session::apply_launcher_settings(&self.bridge, &state).map_err(anyhow::Error::msg)
    }

    pub(crate) fn create_browser(&mut self, window: &mut Window, cx: &mut Context<Self>) -> String {
        let id = uuid::Uuid::new_v4().simple().to_string();
        let marker = format!("about:blank#cw-{id}");
        let webview = cx.new(|cx| {
            gpui_cef::Webview::new(window, cx, gpui_cef::WebviewOptions::url(marker.clone()))
        });
        self.tabs.push(BrowserTab {
            id: id.clone(),
            title: self.text("chatgpt"),
            webview,
            engine: None,
            visible: false,
            authenticated: !self.manual() && self.session_account.is_some(),
            capabilities: if self.manual() { None } else { self.session_account.clone() },
            request_id: None,
            last_request_id: None,
            current_model: None,
            local_cancellation: None,
            quarantined: false,
            close_requested: false,
            checkpoint_retirement: None,
            manual_checkpoint: None,
            owner_key: None,
            epoch: 0,
            status: "initializing".into(),
            zoom: 1.,
            manual: None,
        });
        if self.selected_tab.as_ref().is_none_or(|selected| !self.tabs.iter().any(|tab| &tab.id == selected)) {
            self.selected_tab = Some(id.clone());
        }
        let tx = self.sender.clone();
        let owned_port = self.cdp_port;
        let result_id = id.clone();
        self.executor.spawn(async move {
            let result = connect_owned_target(owned_port, &marker)
                .await
                .map(Arc::new)
                .map_err(|e| e.to_string());
            let _ = tx.send(UiEvent::EngineReady {
                id: result_id,
                result,
            });
        });
        id
    }

    pub(crate) fn poll(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.reconcile_probe(cx);
        let mut changed = self.sync_catalog_status();
        if let Err(error) = self.window_persistence.flush_due() {
            self.log("error", "window.save-failed", &error.to_string());
        }
        while let Ok(event) = self.receiver.try_recv() {
            changed = true;
            match event {
                UiEvent::McpVerified { runtime, ticket, guard, result, cleanup_confirmed } =>
                    self.finish_mcp_verification(runtime, ticket, guard, result, cleanup_confirmed, cx),
                UiEvent::LoginWaiting { id, cancellation } => self.login_waiting(id, cancellation),
                UiEvent::SessionFinished { id, result, guard } => self.finish_session_change(id, result, guard, window, cx),
                UiEvent::EngineReady { id, result } => match result {
                    Ok(engine) => {
                        if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == id) {
                            tab.engine = Some(engine);
                            tab.status = "idle".into();
                            if tab.visible {
                                let url = self
                                    .fixture_url
                                    .clone()
                                    .unwrap_or_else(|| chat_url(self.state.use_saved_chats).into());
                                tab.webview.update(cx, |view, _| view.load_url(&url));
                            }
                        }
                        if self.pending_smoke.as_ref() == Some(&id) {
                            self.start_smoke(&id);
                        }
                    }
                    Err(error) => {
                        if self.pending_smoke.as_ref() == Some(&id) {
                            self.pending_smoke = None;
                            self.operation = None;
                        }
                        if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == id) {
                            tab.status = "error".into();
                        }
                        self.error = Some(error);
                    }
                },
                UiEvent::SessionProbe { ticket, result, guard } => {
                    self.finish_probe(ticket, result, guard, window, cx);
                }
                UiEvent::Progress { id, event } => {
                    let ownership = match &event {
                        Progress::Accepted { ownership, .. } | Progress::Commentary { ownership, .. } | Progress::Assistant { ownership, .. } => ownership,
                    };
                    if !self.tabs.iter().any(|tab| tab.id == id && tab.epoch == ownership.epoch && tab.request_id.is_some()) { continue; }
                    match event {
                    Progress::Accepted { user_turn_id, .. } => {
                        if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == id) {
                            tab.status = "running".into();
                            if let (Some(inspection), Some(store)) =
                                (&tab.capabilities, self.limits_store.as_mut())
                            {
                                if store.matches_account(&inspection.account.account_key) {
                                    let model = if tab.current_model.as_deref() == Some("chatgpt-web/gpt-6-pro")
                                    {
                                        cw_contracts::limits::ReceiptModel::Gpt6Pro
                                    } else if tab.current_model.as_deref() == Some("chatgpt-web/gpt-5.6-pro")
                                    {
                                        cw_contracts::limits::ReceiptModel::Gpt56Pro
                                    } else {
                                        cw_contracts::limits::ReceiptModel::Other
                                    };
                                    if let Err(error) = store.record(
                                        &inspection.account.account_key,
                                        &user_turn_id,
                                        model,
                                        now_ms(),
                                        now_ms(),
                                    ) {
                                        self.error = Some(error.to_string());
                                    }
                                    if let Ok(snapshot) = store.snapshot(now_ms()) {
                                        self.limits =
                                            serde_json::to_value(snapshot).unwrap_or_default();
                                    }
                                }
                            }
                        }
                        self.log(
                            "info",
                            "browser.send-accepted",
                            &format!("Accepted logical turn {user_turn_id}"),
                        );
                    }
                    Progress::Commentary { text, .. } => self.log("info", "turn.commentary", &text),
                    Progress::Assistant { .. } => {}
                    }
                }
                UiEvent::TurnEnded { id, request_id, epoch, result, cleanup_confirmed } => {
                    if !self.tabs.iter().any(|tab| tab.id == id && tab.epoch == epoch && tab.request_id.as_ref() == Some(&request_id)) { continue; }
                    if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == id) {
                        tab.last_request_id = Some(request_id);
                        if cleanup_confirmed { tab.request_id = None; }
                        tab.quarantined = !cleanup_confirmed;
                        tab.local_cancellation = None;
                        tab.manual = None;
                        tab.status = if !cleanup_confirmed { "cleanup-unconfirmed" } else if result.is_ok() { "completed" } else { "error" }.into();
                    }
                    if let Err(error) = result {
                        self.error = Some(error.clone());
                        self.log("error", "turn.failed", &error);
                    } else {
                        self.log("info", "turn.completed", "Browser turn completed");
                    }
                }
                UiEvent::SmokeEnded { id, epoch, result, cleanup_confirmed } => {
                    if !self.tabs.iter().any(|t| t.id == id && t.epoch == epoch) {
                        continue;
                    }
                    self.operation = None;
                    if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == id) {
                        if cleanup_confirmed { tab.request_id = None; }
                        tab.quarantined = !cleanup_confirmed;
                        tab.local_cancellation = None;
                        tab.status = if !cleanup_confirmed { "cleanup-unconfirmed" } else if result.is_ok() { "completed" } else { "error" }.into();
                    }
                    match result {
                        Ok(inspection) => {
                            if let Some(tab) = self.tabs.iter_mut().find(|t| t.id == id) {
                                tab.authenticated = true;
                                tab.capabilities = Some(inspection);
                            }
                            if let Err(error) = self.persist(|s| {
                                s.browser_smoke_passed = true;
                                s.browser_smoke_version = Some(REFERENCE_VERSION.into());
                            }) { self.error = Some(error.to_string()); }
                            self.log("info", "smoke.completed", "The owned smoke response matched the exact source marker");
                        }
                        Err(error) => {
                            self.log("error", "smoke.failed", &error);
                            self.error = Some(error);
                        }
                    }
                }
                UiEvent::CheckpointEnded { id, request_id, epoch, result, unsettled } => {
                    if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == id && tab.epoch == epoch && tab.request_id.as_ref() == Some(&request_id)) {
                        tab.local_cancellation = None;
                        tab.quarantined = unsettled.is_some();
                        tab.checkpoint_retirement = unsettled;
                        if !tab.quarantined { tab.last_request_id = tab.request_id.take(); }
                        tab.status = if tab.quarantined { "cleanup-unconfirmed" } else if result.is_ok() { "completed" } else { "error" }.into();
                    } else if let Some(retirement) = unsettled {
                        self.unresolved_checkpoint_retirements.push(retirement);
                    }
                    if let Err(error) = result {
                        self.log("error", "checkpoint.failed", &error);
                        self.error = Some(error);
                    } else {
                        self.log("info", "checkpoint.completed", "The summary receipt and owned browser cleanup were both confirmed");
                    }
                }
                UiEvent::RuntimeOperation {
                    name,
                    runtime,
                    result,
                } => {
                    self.runtime = Some(runtime);
                    self.operation = None;
                    match result {
                        Ok(value) => {
                            if name == "mcp-setup" && value["ready"] == true {
                                if let Err(error) = self.persist(|s| {
                                    s.mcp_runtime_installed = Some(true);
                                    s.mcp_guide_step = 2;
                                    s.mcp_setup_complete = Some(false);
                                }) {
                                    self.error = Some(error.to_string());
                                }
                                self.log("info","mcp.tunnel-ready","The application's private tunnel passed its health check; connector tool verification remains separate");
                            } else {
                                self.log(
                                    "info",
                                    &format!("{name}.completed"),
                                    "Owned runtime operation completed",
                                );
                            }
                        }
                        Err(error) => {
                            self.log("error", &format!("{name}.failed"), &error);
                            self.error = Some(error);
                        }
                    }
                }
                UiEvent::ToolForwardFailed(error) => {
                    self.log("error", "tool.forward-failed", &error);
                    self.error = Some(error);
                }
            }
        }
        while let Ok(request) = self.browser_requests.try_recv() {
            changed = true;
            match request {
                BrowserRequest::Turn(turn) => self.pending_turns.push_back(turn),
                BrowserRequest::Compaction(request) => self.pending_compactions.push_back(request),
                BrowserRequest::ToolCall(call) => {
                    // The outer native harness, rather than the browser page, owns execution and approval.
                    let bridge = self.bridge.clone();
                    let tx = self.sender.clone();
                    self.executor.spawn(async move {
                        if let Err(error) = bridge.forward_native_tool(call).await {
                            let _ = tx.send(UiEvent::ToolForwardFailed(error));
                        }
                    });
                }
            }
        }
        changed |= self.poll_checkpoints(window, cx);
        if !self.probe_inflight && self.operation.is_none() && self.pending_compactions.is_empty() {
        if let Some(turn) = self.pending_turns.pop_front() {
            if turn.cancellation.borrow().is_some() {
                changed = true;
            } else {
                let key = format!(
                    "{}\n{}\n{}",
                    turn.owner.thread_id,
                    turn.model,
                    turn.effort.as_deref().unwrap_or("")
                );
                let target = self
                    .tabs
                    .iter()
                    .position(|t| !t.quarantined && !t.close_requested && t.request_id.is_none() && t.owner_key.as_ref() == Some(&key))
                    .or_else(|| {
                        self.tabs
                            .iter()
                            .position(|t| !t.quarantined && !t.close_requested && t.request_id.is_none() && t.owner_key.is_none())
                    });
                if let Some(index) = target {
                    if self.tabs[index].engine.is_some() {
                        self.start_turn(index, turn, key, cx);
                        changed = true;
                    } else {
                        self.pending_turns.push_front(turn);
                    }
                } else if self.tabs.len() < 5 {
                    self.create_browser(window, cx);
                    self.pending_turns.push_front(turn);
                    changed = true;
                } else if let Some(index) = self.tabs.iter().position(|t| !t.quarantined && !t.close_requested && t.request_id.is_none()) {
                    let previous = self.tabs.remove(index);
                    drop(previous);
                    self.create_browser(window, cx);
                    self.pending_turns.push_front(turn);
                    changed = true;
                } else {
                    self.pending_turns.push_front(turn);
                }
            }
        }
        }
        let mut auth_checks = Vec::new();
        for tab in &mut self.tabs {
            let view = tab.webview.read(cx);
            if tab.visible && tab.request_id.is_none() {
                let loading = view.is_loading();
                if loading && tab.status != "loading" {
                    tab.status = "loading".into();
                    changed = true;
                }
                if !loading && tab.status == "loading" {
                    tab.status = "idle".into();
                    changed = true;
                    if self.state.browser_interaction_mode == BrowserInteractionMode::Automatic
                        && crate::session::is_chatgpt_origin(&view.url())
                    {
                        if let Some(engine) = &tab.engine {
                            auth_checks.push((tab.id.clone(), engine.clone()));
                        }
                    }
                }
                let title = view.title();
                if !title.is_empty() && title != tab.title {
                    tab.title = title;
                    changed = true;
                }
            }
            if let Some(manual) = &tab.manual {
                if !manual.sent
                    && now_ms()
                        > crate::presentation::manual_deadline_ms(false, manual.copied_at_ms)
                {
                    self.bridge.cancel(&manual.turn.request_id);
                }
                if manual.turn.cancellation.borrow().is_some() {
                    tab.manual = None;
                    tab.last_request_id = tab.request_id.take();
                    tab.status = "cancelled".into();
                    changed = true;
                }
            }
        }
        for (id, _) in auth_checks {
            if !self.busy() && self.start_probe(&id, crate::session::ProbeKind::Auth, cx).is_ok() { break; }
        }
        let reminder_visible = crate::presentation::session_reminder_due(
            self.tabs.iter().any(|tab| tab.authenticated),
            self.state.session_refresh_reminder_at.as_deref(),
            now_ms(),
        );
        if self.reminder_visible != reminder_visible {
            self.reminder_visible = reminder_visible;
            changed = true;
        }
        let second = now_ms() / 1000;
        if self.manual_tick_second != second && self.tabs.iter().any(|tab| {
            tab.manual.as_ref().is_some_and(|manual| !manual.sent) || tab.manual_checkpoint.as_ref().is_some_and(|manual| !manual.sent)
        }) {
            self.manual_tick_second = second;
            changed = true;
        }
        let count = self.tabs.len();
        self.tabs.retain(|tab| {
            self.session_job.is_some() || !tab.close_requested || tab.request_id.is_some() || tab.quarantined || self.pending_compactions.iter().any(|request| {
                request.source_request_id.as_ref().is_some_and(|source| tab.last_request_id.as_ref() == Some(source))
            })
        });
        if self.tabs.len() != count {
            changed = true;
            if !self.tabs.iter().any(|tab| Some(&tab.id) == self.selected_tab.as_ref()) {
                self.selected_tab = self.tabs.last().map(|tab| tab.id.clone());
            }
            if self.tabs.is_empty() { self.create_browser(window, cx); }
        }
        if changed {
            cx.notify();
        }
    }

    fn start_turn(&mut self, index: usize, turn: BrowserTurn, key: String, cx: &mut Context<Self>) {
        let manual = turn.mode == ExecutionMode::Manual;
        let tab = &mut self.tabs[index];
        tab.epoch += 1;
        tab.owner_key = Some(key);
        tab.request_id = Some(turn.request_id.clone());
        tab.current_model = Some(turn.model.clone());
        tab.title = turn.owner.thread_id.clone();
        tab.status = "running".into();
        let engine = tab.engine.clone().expect("selected initialized browser");
        let id = tab.id.clone();
        if manual {
            tab.visible = true;
            cx.write_to_clipboard(ClipboardItem::new_string(turn.task.prompt.clone()));
            if tab.webview.read(cx).url().starts_with("about:") {
                tab.webview.update(cx, |view, _| {
                    view.load_url(chat_url(self.state.use_saved_chats))
                });
            }
            tab.manual = Some(ManualTurn {
                turn,
                copied_at_ms: now_ms(),
                sent: false,
            });
            self.selected_tab = Some(id);
            self.surface = Surface::Browser;
            return;
        }
        let navigate = self.state.experimental_fresh_conversation_per_turn
            || tab.webview.read(cx).url().starts_with("about:");
        if self.state.show_browser_during_turns {
            tab.visible = true;
            self.selected_tab = Some(id.clone());
            self.surface = Surface::Browser;
        }
        let profile = self.profile.clone();
        let tx = self.sender.clone();
        let epoch = tab.epoch;
        let request_id = turn.request_id.clone();
        let cancellation = Cancellation::default();
        tab.local_cancellation = Some(cancellation.clone());
        let saved = self.state.use_saved_chats;
        let connector_name = self.connector_name();
        self.executor.spawn(async move {
            let cancel_for_watch = cancellation.clone();
            let mut watch = turn.cancellation.clone();
            let monitor = tokio::spawn(async move {
                if watch.borrow().is_some() {
                    cancel_for_watch.cancel();
                    return;
                }
                let _ = watch.wait_for(|v| v.is_some()).await;
                cancel_for_watch.cancel();
            });
            let (progress_tx, progress_rx) = tokio::sync::mpsc::unbounded_channel();
            let ui_tx = tx.clone();
            let progress_id = id.clone();
            let outcome = async {
                let attachments =
                    materialize_images(&profile, &turn.request_id, &turn.task.images).await?;
                if navigate {
                    engine.navigate_with_cancellation(chat_url(saved), cancellation.clone()).await?;
                }
                let input = TurnInput {
                    connector_name: Some(connector_name),
                    full_prompt: turn.task.prompt.clone(),
                    model: turn.model.clone(),
                    effort: turn
                        .effort
                        .clone()
                        .unwrap_or_else(|| default_effort(&turn.model).into()),
                    attachments,
                    interaction_mode: InteractionMode::Automatic,
                    ownership: Ownership {
                        task_id: turn.owner.thread_id.clone(),
                        epoch,
                    },
                    turn_token: Some(turn.turn_token.clone()),
                };
                Ok::<_, anyhow::Error>(
                    engine
                        .submit(input, progress_tx, cancellation.clone())
                        .await?,
                )
            };
            let result = crate::delivery::relay_browser_output(
                outcome,
                progress_rx,
                &turn.events,
                &cancellation,
                move |event| {
                    let _ = ui_tx.send(UiEvent::Progress { id: progress_id.clone(), event });
                },
            ).await;
            monitor.abort();
            let cleanup_confirmed = result.as_ref().err().is_none_or(|error| error.cleanup_confirmed);
            let _ = tx.send(UiEvent::TurnEnded { id, request_id, epoch, cleanup_confirmed, result: result.map_err(|error| error.message) });
        });
    }

    fn install_route(&mut self) -> Result<()> {
        ensure!(
            self.bridge.counters().active_turns == 0,
            "Finish active tasks before installation"
        );
        ensure!(
            self.manual() || self.state.browser_smoke_passed,
            "Run the signed-in browser test first"
        );
        let endpoint = self.bridge.base_url();
        cw_runtime::IntegrationJournal::new(self.profile.clone())
            .install_native_route(&endpoint)?;
        self.persist(|s| {
            s.core_setup_complete = Some(true);
            s.codex_restart_required = Some(true);
            s.codex_catalog_verified = Some(false);
        })?;
        crate::session::apply_launcher_settings(&self.bridge, &self.state).map_err(anyhow::Error::msg)?;
        self.log(
            "info",
            "integration.installed",
            "Installed route in the separate Codex home; catalog acceptance is still required",
        );
        Ok(())
    }

    fn smoke_test(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Result<()> {
        ensure!(!self.manual(), "The browser smoke test is unavailable in Zero Risk");
        ensure!(!self.busy(), "Finish the current operation before the browser test");
        ensure!(self.tabs.iter().any(|tab| tab.authenticated), "Sign in and verify the isolated browser first");
        let reusable = self.tabs.iter().find(|tab| {
            tab.request_id.is_none() && (tab.owner_key.is_none() || tab.owner_key.as_deref() == Some(&format!("smoke:{}", tab.id)))
        }).map(|tab| tab.id.clone());
        let id = if let Some(id) = reusable {
            id
        } else {
            ensure!(self.tabs.len() < 5, "Close an idle browser tab before starting a separate browser test");
            self.create_browser(window, cx)
        };
        self.operation = Some(self.text("smokeRunning"));
        self.pending_smoke = Some(id.clone());
        let tab = self.tabs.iter_mut().find(|tab| tab.id == id).expect("created smoke tab");
        tab.owner_key = Some(format!("smoke:{id}"));
        tab.local_cancellation = Some(Cancellation::default());
        tab.request_id = Some(format!("smoke:{id}"));
        tab.current_model = None;
        tab.status = "testing".into();
        if tab.engine.is_some() {
            self.start_smoke(&id);
        }
        Ok(())
    }

    fn start_smoke(&mut self, id: &str) {
        let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == id) else { return; };
        let Some(engine) = tab.engine.clone() else { return; };
        let cancellation = tab.local_cancellation.clone().expect("reserved smoke cancellation");
        tab.epoch += 1;
        let ownership = Ownership { task_id: format!("smoke:{id}"), epoch: tab.epoch };
        let epoch = tab.epoch;
        let id = id.to_owned();
        let saved = self.state.use_saved_chats;
        self.pending_smoke = None;
        let tx = self.sender.clone();
        self.executor.spawn(async move {
            let result = async {
                engine.navigate_with_cancellation(chat_url(saved), cancellation.clone()).await?;
                let inspection = engine.inspect_capabilities(InteractionMode::Automatic, cancellation.clone()).await?;
                let input = cw_browser::smoke_turn_input(&inspection, ownership)?;
                let (progress, _) = tokio::sync::mpsc::unbounded_channel();
                match engine.submit(input, progress, cancellation).await? {
                    TurnResult::Completed { text, .. } => cw_browser::validate_smoke_response(&text)?,
                    _ => return Err(cw_browser::Error::Dom("Automatic smoke returned a manual handoff".into())),
                }
                Ok::<_, cw_browser::Error>(inspection)
            }.await;
            let cleanup_confirmed = !matches!(&result, Err(cw_browser::Error::CleanupUnconfirmed));
            let _ = tx.send(UiEvent::SmokeEnded { id, epoch, cleanup_confirmed, result: result.map_err(|error| error.to_string()) });
        });
    }

    fn save_mcp(&mut self, cx: &mut Context<Self>) -> Result<()> {
        let tunnel_id = self.tunnel_id.read(cx).value();
        let key = self.runtime_key.read(cx).value();
        ensure!(
            !tunnel_id.trim().is_empty() && !key.trim().is_empty(),
            "Tunnel ID and API key are required"
        );
        ensure!(
            self.bridge.counters().active_turns == 0,
            "Finish active tasks before tunnel setup"
        );
        let mut runtime = self
            .runtime
            .take()
            .context("A runtime operation is already running")?;
        let tx = self.sender.clone();
        let endpoint = format!("{}/mcp", self.bridge.base_url());
        let mode = if self.manual() { "manual" } else { "automatic" };
        let name = self.connector_name();
        self.operation = Some(self.text("connecting"));
        self.runtime_key
            .update(cx, |input, cx| { input.reset(); cx.notify(); });
        self.executor.spawn(async move {
            let result = runtime
                .configure_install_start_mcp(
                    tunnel_id.trim(),
                    key.trim().as_bytes(),
                    &endpoint,
                    mode,
                    &name,
                )
                .await
                .map(|result| serde_json::to_value(result).unwrap_or_default())
                .map_err(|e| e.to_string());
            let _ = tx.send(UiEvent::RuntimeOperation {
                name: "mcp-setup".into(),
                runtime,
                result,
            });
        });
        Ok(())
    }

    fn refresh_limits(&mut self, cx: &Context<Self>) -> Result<()> {
        ensure!(!self.manual(), "Usage tracking is unavailable in Zero Risk");
        ensure!(
            self.tabs.iter().any(|t| t.authenticated),
            "Sign in before checking the account plan"
        );
        let id = self
            .tabs
            .iter()
            .find(|t| t.authenticated && t.request_id.is_none())
            .map(|t| t.id.clone())
            .context("No idle authenticated browser is available")?;
        self.start_probe(&id, crate::session::ProbeKind::Account, cx)
    }
}

pub fn chat_url(saved: bool) -> &'static str {
    if saved {
        "https://chatgpt.com/"
    } else {
        "https://chatgpt.com/?temporary-chat=true"
    }
}
pub(crate) fn default_effort(model: &str) -> &'static str {
    if model.ends_with("-pro") {
        "max"
    } else if model.ends_with("-instant") {
        "low"
    } else {
        "high"
    }
}

async fn connect_owned_target(port: u16, marker: &str) -> Result<BrowserEngine> {
    ensure!(
        marker.starts_with("about:blank#cw-"),
        "Owned target marker missing"
    );
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .no_proxy()
        .build()?;
    for _ in 0..100 {
        if let Ok(response) = client
            .get(format!("http://127.0.0.1:{port}/json/list"))
            .send()
            .await
        {
            if let Ok(targets) = response.json::<Vec<Value>>().await {
                let matches: Vec<_> = targets
                    .iter()
                    .filter(|t| t["type"] == "page" && t["url"] == marker)
                    .collect();
                ensure!(matches.len() <= 1, "Owned target marker collision");
                if let Some(target) = matches.first() {
                    let ws = target["webSocketDebuggerUrl"]
                        .as_str()
                        .ok_or_else(|| anyhow::anyhow!("Owned page has no debugger URL"))?;
                    let url = url::Url::parse(ws)?;
                    ensure!(
                        url.port() == Some(port),
                        "Debugger port differs from this application's listener"
                    );
                    return Ok(BrowserEngine::connect(ws).await?);
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    bail!("The application-owned CEF target was not found on its assigned listener")
}

async fn materialize_images(
    profile: &cw_runtime::Profile,
    request: &str,
    images: &[cw_bridge::InputImage],
) -> Result<Vec<Attachment>> {
    let mut out = Vec::new();
    for (index, image) in images.iter().enumerate() {
        let (header, payload) = image.image_url.split_once(',').ok_or_else(|| {
            anyhow::anyhow!(
                "Only inline data image attachments are accepted by this native upload path"
            )
        })?;
        ensure!(
            header.starts_with("data:image/") && header.ends_with(";base64"),
            "Unsupported attachment URL; a data image is required"
        );
        ensure!(
            payload.len() <= 32 * 1024 * 1024,
            "Image attachment exceeds the byte limit"
        );
        let bytes = base64::engine::general_purpose::STANDARD.decode(payload)?;
        let suffix = match header {
            "data:image/png;base64" => "png",
            "data:image/jpeg;base64" => "jpg",
            "data:image/webp;base64" => "webp",
            "data:image/gif;base64" => "gif",
            _ => bail!("Unsupported inline image type"),
        };
        let relative = format!("runtime/{request}-image-{index}.{suffix}");
        let path = profile.path(relative)?;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        out.push(Attachment { path });
    }
    Ok(out)
}
