use crate::{assets, input::TextInput, presentation};
use cw_bridge::{BridgeHandle, BrowserRequest, BrowserTurn};
use cw_browser::BrowserEngine;
use cw_contracts::{BrowserInteractionMode, LauncherState, Surface};
use cw_runtime::{Profile, RuntimeFacade};
use gpui::{
    Animation, AnimationExt, AnyElement, Context, Entity, FocusHandle, FontWeight, MouseButton,
    SharedString, Window, div, prelude::*, px, rgb, rgba, svg,
};
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    sync::{Arc, mpsc},
    time::Duration,
};

pub const REFERENCE_VERSION: &str = "6.1.10";
pub const GITHUB_URL: &str = "https://github.com/miuuyy/codex-chatgpt-web";
pub const X_URL: &str = "https://x.com/miu21590";
pub const CONNECTORS_URL: &str = "https://chatgpt.com/#settings/Plugins";
pub const TUNNELS_URL: &str = "https://platform.openai.com/settings/organization/tunnels";
pub const KEYS_URL: &str = "https://platform.openai.com/settings/organization/api-keys";
const BG: u32 = 0x181818;
const SIDEBAR: u32 = 0x212121;
const ACCENT: u32 = 0x99ceff;
const TEXT: u32 = 0xffffff;
const SECONDARY: u32 = 0xbababa;
const DIM: u32 = 0x8c8c8c;

#[derive(Clone)]
pub enum Action {
    Navigate(Surface),
    ToggleSidebar,
    Language(String),
    LanguageMenu,
    Mode(BrowserInteractionMode),
    WelcomeNext,
    WelcomeBack,
    Social(&'static str),
    External(&'static str),
    Preference(&'static str, bool),
    OpenBrowser,
    CheckLogin,
    PasskeyStart,
    PasskeyContinue,
    CancelSession,
    Back,
    Forward,
    Reload,
    Zoom(i8),
    SelectTab(String),
    CloseTab(String),
    Doctor,
    Smoke,
    Install,
    RemoveIntegration,
    CancelTurns,
    ExportLogs,
    McpStep(u8),
    SaveMcp,
    VerifyMcp,
    ChangeConnector,
    Tracking,
    RefreshLimits,
    CopyManual,
    ConfirmManual,
    Logout,
    DismissReminder,
    DismissError,
}

pub struct BrowserTab {
    pub id: String,
    pub title: String,
    pub webview: Entity<gpui_cef::Webview>,
    pub engine: Option<Arc<BrowserEngine>>,
    pub visible: bool,
    pub authenticated: bool,
    pub capabilities: Option<cw_browser::CapabilityInspection>,
    pub request_id: Option<String>,
    pub last_request_id: Option<String>,
    pub current_model: Option<String>,
    pub local_cancellation: Option<cw_browser::Cancellation>,
    pub quarantined: bool,
    pub close_requested: bool,
    pub checkpoint_retirement: Option<tokio::sync::oneshot::Sender<Result<(), String>>>,
    pub manual_checkpoint: Option<ManualCheckpoint>,
    pub owner_key: Option<String>,
    pub epoch: u64,
    pub status: String,
    pub zoom: f32,
    pub manual: Option<ManualTurn>,
}

pub struct ManualTurn {
    pub turn: BrowserTurn,
    pub copied_at_ms: u64,
    pub sent: bool,
}

pub struct ManualCheckpoint {
    pub request: cw_bridge::CompactionRequest,
    pub copied_at_ms: u64,
    pub sent: bool,
}

pub enum UiEvent {
    McpVerified {
        runtime: RuntimeFacade,
        ticket: Option<crate::session::ProbeTicket>,
        guard: cw_bridge::SessionChangeGuard,
        result: Result<Value, String>,
        cleanup_confirmed: bool,
    },
    LoginWaiting { id: String, cancellation: cw_runtime::SignInCancellation },
    SessionFinished {
        id: String,
        result: Result<cw_browser::SessionReceipt, cw_browser::SessionFailure>,
        guard: cw_bridge::SessionChangeGuard,
    },
    ToolForwardFailed(String),
    SmokeEnded {
        id: String,
        epoch: u64,
        result: Result<cw_browser::CapabilityInspection, String>,
        cleanup_confirmed: bool,
    },
    CheckpointEnded {
        id: String,
        request_id: String,
        epoch: u64,
        result: Result<(), String>,
        unsettled: Option<tokio::sync::oneshot::Sender<Result<(), String>>>,
    },
    EngineReady {
        id: String,
        result: Result<Arc<BrowserEngine>, String>,
    },
    SessionProbe {
        ticket: crate::session::ProbeTicket,
        result: Result<crate::session::ProbeObservation, String>,
        guard: cw_bridge::SessionChangeGuard,
    },
    Progress {
        id: String,
        event: cw_browser::Progress,
    },
    TurnEnded {
        id: String,
        request_id: String,
        epoch: u64,
        result: Result<(), String>,
        cleanup_confirmed: bool,
    },
    RuntimeOperation {
        name: String,
        runtime: RuntimeFacade,
        result: Result<Value, String>,
    },
}

pub struct Launcher {
    pub probes: crate::session::ProbeTracker,
    pub probe_inflight: bool,
    pub session_account: Option<cw_browser::CapabilityInspection>,
    pub session_job: Option<crate::session_login::SessionJob>,
    pub session_quarantine: Option<cw_bridge::SessionChangeGuard>,
    pub verification_cancellation: Option<cw_browser::Cancellation>,
    pub profile: Profile,
    pub runtime: Option<RuntimeFacade>,
    pub state: LauncherState,
    pub surface: Surface,
    pub stage: u8,
    pub language: String,
    pub selected_mode: BrowserInteractionMode,
    pub language_menu: bool,
    pub sidebar_drag: Option<(gpui::Pixels, f64)>,
    pub compact: bool,
    pub error: Option<String>,
    pub operation: Option<String>,
    pub doctor: Option<Value>,
    pub logs: VecDeque<Value>,
    pub tabs: Vec<BrowserTab>,
    pub selected_tab: Option<String>,
    pub pending_turns: VecDeque<BrowserTurn>,
    pub pending_compactions: VecDeque<cw_bridge::CompactionRequest>,
    pub unresolved_checkpoint_retirements: Vec<tokio::sync::oneshot::Sender<Result<(), String>>>,
    pub pending_smoke: Option<String>,
    pub reminder_visible: bool,
    pub manual_tick_second: u64,
    pub bridge: BridgeHandle,
    pub browser_requests: tokio::sync::mpsc::Receiver<BrowserRequest>,
    pub executor: Arc<tokio::runtime::Runtime>,
    pub sender: mpsc::Sender<UiEvent>,
    pub receiver: mpsc::Receiver<UiEvent>,
    pub cdp_port: u16,
    pub fixture_url: Option<String>,
    pub tunnel_id: Entity<TextInput>,
    pub runtime_key: Entity<TextInput>,
    pub connector_suffix: Entity<TextInput>,
    pub focus: FocusHandle,
    pub limits: Value,
    pub limits_store: Option<cw_contracts::limits::LimitsStore>,
    pub window_size: (f32, f32),
    pub window_persistence: crate::platform::WindowPersistence,
}

impl Launcher {
    pub fn new(
        profile: Profile,
        mut state: LauncherState,
        bridge: BridgeHandle,
        browser_requests: tokio::sync::mpsc::Receiver<BrowserRequest>,
        executor: Arc<tokio::runtime::Runtime>,
        cdp_port: u16,
        fixture_url: Option<String>,
        restored_window: cw_runtime::WindowState,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        state.codex_catalog_verified = Some(false);
        let language = state.language.clone().unwrap_or_else(|| "en".into());
        let surface = match presentation::initial_surface(
            state.browser_interaction_mode == BrowserInteractionMode::Manual,
            state.core_setup_complete == Some(true),
            state.codex_catalog_verified == Some(true),
        ) {
            "mcp" => Surface::Mcp,
            "setup" => Surface::Setup,
            _ => Surface::Browser,
        };
        let (sender, receiver) = mpsc::channel();
        let (limits_store, limits) = match cw_contracts::limits::LimitsStore::open(profile.user_data().join("limits.json")) {
            Ok(mut store) => {
                let snapshot = match store.snapshot(now_ms()) {
                    Ok(value) => serde_json::to_value(value).expect("serializable limits snapshot"),
                    Err(error) => json!({"readError":error.to_string()}),
                };
                (Some(store), snapshot)
            }
            Err(error) => (None, json!({"readError":error.to_string()})),
        };
        let mut this = Self {
            probes: crate::session::ProbeTracker::default(),
            probe_inflight: false,
            session_account: None,
            session_job: None,
            session_quarantine: None,
            verification_cancellation: None,
            runtime: Some(RuntimeFacade::new(profile.clone())),
            window_persistence: crate::platform::WindowPersistence::new(profile.clone(), restored_window),
            profile,
            stage: u8::from(state.language.is_some()),
            selected_mode: state.browser_interaction_mode,
            state,
            surface,
            language,
            language_menu: false,
            sidebar_drag: None,
            compact: false,
            error: None,
            operation: None,
            doctor: None,
            logs: VecDeque::new(),
            tabs: Vec::new(),
            selected_tab: None,
            pending_turns: VecDeque::new(),
            pending_compactions: VecDeque::new(),
            unresolved_checkpoint_retirements: Vec::new(),
            pending_smoke: None,
            reminder_visible: false,
            manual_tick_second: 0,
            bridge,
            browser_requests,
            executor,
            sender,
            receiver,
            cdp_port,
            fixture_url,
            tunnel_id: cx.new(|cx| TextInput::new("", "Tunnel ID", false, cx)),
            runtime_key: cx.new(|cx| TextInput::new("", "API key", true, cx)),
            connector_suffix: cx.new(|cx| TextInput::new("Native2", "Plugin name", false, cx)),
            focus: cx.focus_handle(),
            limits,
            limits_store,
            window_size: (0., 0.),
        };
        this.log("info", "launcher.started", "Native profile opened");
        this.create_browser(window, cx);
        cx.observe_window_bounds(window, |this, window, _| {
            if let Err(error) = this.window_persistence.changed(window) {
                this.log("error", "window.save-failed", &error.to_string());
            }
        }).detach();
        cx.on_app_quit(|this, _| {
            this.probes.invalidate();
            if let Some(signal) = &this.verification_cancellation { signal.cancel(); }
            if let Some(job) = this.session_job.as_mut() { job.cancel(); }
            if let Err(error) = this.window_persistence.flush() {
                log::error!("Cannot save native window on exit: {error}");
            }
            async {}
        }).detach();
        cx.spawn_in(window, async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(100))
                    .await;
                if this
                    .update_in(cx, |this, window, cx| this.poll(window, cx))
                    .is_err()
                {
                    break;
                }
            }
        })
        .detach();
        this
    }

    pub fn text(&self, key: &str) -> String {
        assets::text(&self.language, key)
    }
    pub fn manual(&self) -> bool {
        self.state.browser_interaction_mode == BrowserInteractionMode::Manual
    }
    pub fn busy(&self) -> bool {
        let counters = self.bridge.counters();
        self.probe_inflight || self.session_job.is_some() || self.session_quarantine.is_some() || self.operation.is_some() || counters.active_turns > 0 || counters.active_compactions > 0 || counters.active_native_requests > 0 || counters.pending_tool_calls > 0 || self.tabs.iter().any(|tab| tab.request_id.is_some() || tab.quarantined)
    }
    pub fn tab(&self) -> Option<&BrowserTab> {
        self.tabs
            .iter()
            .find(|t| Some(&t.id) == self.selected_tab.as_ref())
    }
    pub fn tab_mut(&mut self) -> Option<&mut BrowserTab> {
        self.tabs
            .iter_mut()
            .find(|t| Some(&t.id) == self.selected_tab.as_ref())
    }

    pub fn log(&mut self, level: &str, event: &str, detail: &str) {
        self.logs.push_back(json!({"at":chrono::Utc::now().to_rfc3339(),"level":level,"event":event,"detail":cw_runtime::redact(detail)}));
        while self.logs.len() > 500 {
            self.logs.pop_front();
        }
    }

    pub fn persist(&mut self, update: impl FnOnce(&mut LauncherState)) -> anyhow::Result<()> {
        let mut next = self.state.clone();
        update(&mut next);
        self.profile
            .write_json("user-data/launcher-state.json", &next)?;
        self.state = next;
        Ok(())
    }

    pub fn dispatch(&mut self, action: Action, window: &mut Window, cx: &mut Context<Self>) {
        if let Err(error) = self.perform(action, window, cx) {
            let message = error.to_string();
            self.log("error", "operation.failed", &message);
            self.error = Some(message);
        }
        cx.notify();
    }

    pub(crate) fn button(
        &self,
        id: &str,
        label: impl Into<SharedString>,
        primary: bool,
        enabled: bool,
        action: Action,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .id(SharedString::from(id.to_owned()))
            .flex()
            .items_center()
            .justify_center()
            .h(px(32.))
            .px(px(12.))
            .rounded(px(7.))
            .border_1()
            .border_color(if primary {
                rgba(0xffffff00)
            } else {
                rgba(0xffffff29)
            })
            .bg(if primary { rgb(TEXT) } else { rgb(0x232323) })
            .text_color(if primary { rgb(BG) } else { rgb(TEXT) })
            .text_size(px(12.))
            .font_weight(FontWeight::MEDIUM)
            .when(!enabled, |el| el.opacity(0.38))
            .when(enabled, |el| {
                el.cursor_pointer()
                    .hover(move |s| {
                        s.bg(if primary {
                            rgb(0xdfdfdf)
                        } else {
                            rgb(0x303030)
                        })
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.dispatch(action.clone(), window, cx)
                    }))
            })
            .child(label.into())
            .into_any_element()
    }

    pub(crate) fn icon_button(
        &self,
        name: &'static str,
        label: &'static str,
        enabled: bool,
        action: Action,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .id(label)
            .flex()
            .items_center()
            .justify_center()
            .size(px(30.))
            .rounded(px(8.))
            .text_color(rgb(if enabled { SECONDARY } else { 0x555555 }))
            .when(enabled, |el| {
                el.cursor_pointer()
                    .hover(|s| s.bg(rgba(0xffffff12)).text_color(rgb(TEXT)))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.dispatch(action.clone(), window, cx)
                    }))
            })
            .child(icon(name, 16.))
            .into_any_element()
    }

    pub(crate) fn section(&self, text: impl Into<SharedString>, spaced: bool) -> AnyElement {
        div()
            .text_size(px(11.))
            .font_weight(FontWeight::MEDIUM)
            .text_color(rgb(DIM))
            .mb(px(10.))
            .when(spaced, |d| d.mt(px(30.)))
            .child(text.into())
            .into_any_element()
    }

    pub(crate) fn heading(&self, title: impl Into<SharedString>, subtitle: Option<String>) -> AnyElement {
        div()
            .flex()
            .flex_col()
            .mb(px(20.))
            .child(
                div()
                    .text_size(px(24.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(rgb(TEXT))
                    .child(title.into()),
            )
            .children(subtitle.map(|text| {
                div()
                    .mt(px(10.))
                    .max_w(px(580.))
                    .text_size(px(13.))
                    .line_height(px(20.))
                    .text_color(rgb(SECONDARY))
                    .child(text)
            }))
            .into_any_element()
    }

    pub(crate) fn notice(&self, text: impl Into<SharedString>, warning: bool) -> AnyElement {
        div()
            .flex()
            .gap(px(10.))
            .py(px(14.))
            .text_size(px(12.))
            .line_height(px(19.))
            .text_color(rgb(if warning { 0xffb17f } else { DIM }))
            .child(icon("info", 15.))
            .child(div().flex_1().child(text.into()))
            .into_any_element()
    }

    fn switch_row(
        &self,
        key: &'static str,
        label: &str,
        body: &str,
        checked: bool,
        enabled: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .flex()
            .items_center()
            .gap(px(22.))
            .py(px(17.))
            .border_b_1()
            .border_color(rgba(0xffffff14))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .gap(px(6.))
                    .child(
                        div()
                            .text_size(px(13.))
                            .text_color(rgb(TEXT))
                            .child(label.to_owned()),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .line_height(px(18.))
                            .text_color(rgb(DIM))
                            .child(body.to_owned()),
                    ),
            )
            .child(
                div()
                    .id(key)
                    .flex()
                    .items_center()
                    .w(px(30.))
                    .h(px(18.))
                    .p(px(2.))
                    .rounded(px(9.))
                    .bg(rgb(if checked { 0xf1f1f1 } else { 0x4b4b4b }))
                    .when(checked, |d| d.justify_end())
                    .when(!enabled, |d| d.opacity(0.38))
                    .when(enabled, |d| {
                        d.cursor_pointer()
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.dispatch(Action::Preference(key, !checked), window, cx);
                            }))
                    })
                    .child(div().size(px(14.)).rounded_full().bg(rgb(if checked {
                        0x252525
                    } else {
                        0xf1f1f1
                    }))),
            )
            .into_any_element()
    }

    fn mode_picker(&self, onboarding: bool, cx: &mut Context<Self>) -> AnyElement {
        let mode = if onboarding {
            self.selected_mode
        } else {
            self.state.browser_interaction_mode
        };
        let rows = [
            (
                BrowserInteractionMode::Automatic,
                "automaticInteraction",
                "automaticInteractionBody",
            ),
            (
                BrowserInteractionMode::Manual,
                "manualInteraction",
                "manualInteractionBody",
            ),
        ];
        div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .py(px(12.))
            .children(rows.map(|(value, title, body)| {
                let selected = mode == value;
                div()
                    .id(title)
                    .flex()
                    .gap(px(12.))
                    .p(px(14.))
                    .rounded(px(10.))
                    .border_1()
                    .border_color(if selected {
                        rgba(0xffffff66)
                    } else {
                        rgba(0xffffff20)
                    })
                    .bg(if selected {
                        rgba(0xffffff08)
                    } else {
                        rgba(0xffffff00)
                    })
                    .cursor_pointer()
                    .hover(|s| s.bg(rgba(0xffffff0c)))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.dispatch(Action::Mode(value), window, cx)
                    }))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .flex()
                            .flex_col()
                            .gap(px(6.))
                            .child(
                                div()
                                    .text_size(px(13.))
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(self.text(title)),
                            )
                            .child(
                                div()
                                    .text_size(px(12.))
                                    .line_height(px(18.))
                                    .text_color(rgb(DIM))
                                    .child(self.text(body)),
                            ),
                    )
                    .child(radio(selected))
            }))
            .into_any_element()
    }

    fn render_welcome(&self, cx: &mut Context<Self>) -> AnyElement {
        let title =
            ["chooseLanguage", "interactionMode", "supportTitle"][self.stage.min(2) as usize];
        let subtitle = [
            "chooseLanguageHint",
            "interactionModeOnboardingBody",
            "supportBody",
        ][self.stage.min(2) as usize];
        let stage_content = if self.stage == 0 {
            div()
                .mt(px(34.))
                .border_t_1()
                .border_color(rgba(0xffffff14))
                .children(["en", "zh-CN", "zh-TW", "ja", "ko"].map(|language| {
                    let meta = &assets::LANGUAGES[language];
                    let chosen = self.language == language;
                    div()
                        .id(language)
                        .flex()
                        .items_center()
                        .gap(px(10.))
                        .h(px(58.))
                        .px(px(8.))
                        .border_b_1()
                        .border_color(rgba(0xffffff14))
                        .cursor_pointer()
                        .when(chosen, |d| d.bg(rgba(0xffffff0a)))
                        .hover(|s| s.bg(rgba(0xffffff0f)))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.dispatch(Action::Language(language.into()), window, cx)
                        }))
                        .child(
                            div()
                                .w(px(34.))
                                .text_size(px(12.))
                                .text_color(rgb(DIM))
                                .child(meta["marker"].as_str().unwrap_or(language).to_owned()),
                        )
                        .child(
                            div()
                                .flex_1()
                                .text_size(px(14.))
                                .child(meta["label"].as_str().unwrap_or(language).to_owned()),
                        )
                        .child(radio(chosen))
                }))
                .into_any_element()
        } else if self.stage == 1 {
            div()
                .mt(px(22.))
                .child(self.mode_picker(true, cx))
                .into_any_element()
        } else {
            div()
                .mt(px(34.))
                .border_t_1()
                .border_color(rgba(0xffffff14))
                .children(
                    [
                        (
                            "github",
                            if self.state.github_opened {
                                "starred"
                            } else {
                                "star"
                            },
                            self.state.github_opened,
                        ),
                        (
                            "x",
                            if self.state.x_opened {
                                "followed"
                            } else {
                                "follow"
                            },
                            self.state.x_opened,
                        ),
                    ]
                    .map(|(target, key, done)| {
                        div()
                            .id(target)
                            .flex()
                            .items_center()
                            .gap(px(12.))
                            .h(px(58.))
                            .px(px(8.))
                            .border_b_1()
                            .border_color(rgba(0xffffff14))
                            .cursor_pointer()
                            .hover(|s| s.bg(rgba(0xffffff0a)))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.dispatch(Action::Social(target), window, cx)
                            }))
                            .child(icon(target, 19.))
                            .child(div().flex_1().child(self.text(key)))
                            .child(icon(if done { "check" } else { "external" }, 16.))
                    }),
                )
                .into_any_element()
        };
        let ready = self.stage != 2
            || presentation::can_finish_welcome(
                self.state.github_opened,
                self.state.x_opened,
                self.busy(),
            );
        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(rgb(BG))
            .child(
                div()
                    .flex()
                    .h(px(46.))
                    .items_center()
                    .justify_between()
                    .pl(px(96.))
                    .pr(px(20.))
                    .on_mouse_down(MouseButton::Left, |_, window, _| window.start_window_move())
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(9.))
                            .text_size(px(13.))
                            .text_color(rgb(SECONDARY))
                            .child(icon("brand", 18.))
                            .child(self.text("product")),
                    )
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(rgb(DIM))
                            .child(format!("v{REFERENCE_VERSION}")),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .px(px(28.))
                    .child(
                        div()
                            .w_full()
                            .max_w(px(520.))
                            .py(px(24.))
                            .pb(px(48.))
                            .child(
                                div()
                                    .mb(px(14.))
                                    .text_size(px(12.))
                                    .font_family("SF Mono")
                                    .text_color(rgb(DIM))
                                    .child(format!("0{}", self.stage + 1)),
                            )
                            .child(
                                div()
                                    .text_size(px(36.))
                                    .font_weight(FontWeight::MEDIUM)
                                    .line_height(px(40.))
                                    .child(self.text(title)),
                            )
                            .child(
                                div()
                                    .mt(px(14.))
                                    .text_size(px(13.))
                                    .line_height(px(21.))
                                    .text_color(rgb(SECONDARY))
                                    .child(self.text(subtitle)),
                            )
                            .child(stage_content)
                            .with_animation(
                                ("welcome-stage", self.stage as usize),
                                Animation::new(Duration::from_millis(300)),
                                |el, progress| el.opacity(progress),
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .h(px(74.))
                    .items_center()
                    .justify_between()
                    .px(px(24.))
                    .pb(px(16.))
                    .child(div().w(px(150.)).when(self.stage > 0, |d| {
                        d.child(self.button(
                            "welcome-back",
                            self.text("previous"),
                            false,
                            !self.busy(),
                            Action::WelcomeBack,
                            cx,
                        ))
                    }))
                    .child(div().flex().gap(px(5.)).children((0..3).map(|i| {
                        div()
                            .w(px(if i == self.stage { 22. } else { 6. }))
                            .h(px(4.))
                            .rounded_full()
                            .bg(rgb(if i <= self.stage { TEXT } else { 0x454545 }))
                    })))
                    .child(self.button(
                        "welcome-next",
                        self.text(if self.stage == 2 {
                            "finishWelcome"
                        } else {
                            "continue"
                        }),
                        true,
                        ready,
                        Action::WelcomeNext,
                        cx,
                    )),
            )
            .into_any_element()
    }

    fn nav_row(
        &self,
        surface: Surface,
        name: &'static str,
        label: String,
        attention: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let active = self.surface == surface;
        div()
            .id(name)
            .flex()
            .items_center()
            .h(px(30.))
            .px(px(8.))
            .gap(px(9.))
            .rounded(px(7.))
            .text_size(px(12.))
            .text_color(rgb(if active { TEXT } else { SECONDARY }))
            .when(active, |d| d.bg(rgba(0xffffff12)))
            .cursor_pointer()
            .hover(|s| s.bg(rgba(0xffffff0d)))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.dispatch(Action::Navigate(surface), window, cx)
            }))
            .child(icon(name, 16.))
            .child(div().flex_1().child(label))
            .when(attention, |d| {
                d.child(div().size(px(5.)).rounded_full().bg(rgb(0xff8549)))
            })
            .into_any_element()
    }

    fn render_sidebar(&self, cx: &mut Context<Self>) -> AnyElement {
        let authed = self.tabs.iter().any(|t| t.authenticated);
        div()
            .flex()
            .flex_col()
            .w(px(self.state.sidebar_width as f32))
            .flex_shrink_0()
            .h_full()
            .bg(rgb(SIDEBAR))
            .px(px(12.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .h(px(48.))
                    .gap(px(8.))
                    .px(px(4.))
                    .child(icon("brand", 19.))
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(12.))
                            .font_weight(FontWeight::MEDIUM)
                            .child(self.text("product")),
                    )
                    .child(self.icon_button(
                        "github",
                        "sidebar-github",
                        true,
                        Action::External(GITHUB_URL),
                        cx,
                    ))
                    .child(self.icon_button("x", "sidebar-x", true, Action::External(X_URL), cx)),
            )
            .child(
                div()
                    .mt(px(10.))
                    .px(px(8.))
                    .child(self.section(self.text("workspace"), false)),
            )
            .child(self.nav_row(
                Surface::Browser,
                "browser",
                self.text("browser"),
                !self.manual() && !authed,
                cx,
            ))
            .child(
                div()
                    .mt(px(24.))
                    .px(px(8.))
                    .child(self.section(self.text("configuration"), false)),
            )
            .child(self.nav_row(
                Surface::Setup,
                "setup",
                self.text("setup"),
                self.state.core_setup_complete != Some(true),
                cx,
            ))
            .child(self.nav_row(Surface::Mcp, "mcp", "MCP".into(), false, cx))
            .child(
                div()
                    .mt(px(24.))
                    .px(px(8.))
                    .child(self.section(self.text("runtime"), false)),
            )
            .child(self.nav_row(
                Surface::Activity,
                "activity",
                self.text("activity"),
                false,
                cx,
            ))
            .child(self.nav_row(Surface::Limits, "logs", "Limits".into(), false, cx))
            .child(div().flex_1())
            .child(div().pb(px(12.)).child(self.nav_row(
                Surface::Settings,
                "settings",
                self.text("settings"),
                false,
                cx,
            )))
            .into_any_element()
    }

    fn render_titlebar(&self, cx: &mut Context<Self>) -> AnyElement {
        let browser = self.surface == Surface::Browser;
        let tab = self.tab();
        let navigation = tab.is_some_and(|t| t.request_id.is_none());
        let left_width = if self.state.sidebar_open && !self.compact {
            self.state.sidebar_width as f32
        } else {
            112.
        };
        div()
            .flex()
            .h(px(46.))
            .flex_shrink_0()
            .bg(rgb(BG))
            .child(
                div()
                    .flex()
                    .items_center()
                    .w(px(left_width))
                    .flex_shrink_0()
                    .pl(px(76.))
                    .bg(rgb(if self.state.sidebar_open && !self.compact {
                        SIDEBAR
                    } else {
                        BG
                    }))
                    .child(self.icon_button(
                        "sidebar",
                        "toggle-sidebar",
                        true,
                        Action::ToggleSidebar,
                        cx,
                    )),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .items_center()
                    .gap(px(2.))
                    .px(px(10.))
                    .when(!browser, |d| {
                        d.on_mouse_down(MouseButton::Left, |_, w, _| w.start_window_move())
                    })
                    .when(browser, |d| {
                        d.child(self.icon_button(
                            "back",
                            "browser-back",
                            navigation && tab.is_some_and(|t| t.webview.read(cx).can_go_back()),
                            Action::Back,
                            cx,
                        ))
                        .child(self.icon_button(
                            "forward",
                            "browser-forward",
                            navigation && tab.is_some_and(|t| t.webview.read(cx).can_go_forward()),
                            Action::Forward,
                            cx,
                        ))
                        .child(self.icon_button(
                            "reload",
                            "browser-reload",
                            navigation,
                            Action::Reload,
                            cx,
                        ))
                        .child(
                            div()
                                .flex_1()
                                .text_size(px(11.))
                                .text_color(rgb(DIM))
                                .pl(px(8.))
                                .overflow_hidden()
                                .child(
                                    tab.map(|t| {
                                        let url = t.webview.read(cx).url();
                                        if url.starts_with("about:") {
                                            self.text("browserAddress")
                                        } else {
                                            url
                                        }
                                    })
                                    .unwrap_or_default(),
                                ),
                        )
                        .child(self.icon_button(
                            "minus",
                            "zoom-out",
                            tab.is_some(),
                            Action::Zoom(-1),
                            cx,
                        ))
                        .child(
                            div()
                                .id("zoom-reset")
                                .text_size(px(11.))
                                .w(px(38.))
                                .text_color(rgb(DIM))
                                .cursor_pointer()
                                .on_click(cx.listener(|this, _, w, cx| {
                                    this.dispatch(Action::Zoom(0), w, cx)
                                }))
                                .child(format!("{}%", (tab.map_or(1., |t| t.zoom) * 100.).round())),
                        )
                        .child(self.icon_button(
                            "plus",
                            "zoom-in",
                            tab.is_some(),
                            Action::Zoom(1),
                            cx,
                        ))
                    }),
            )
            .into_any_element()
    }

    pub(crate) fn content(&self, children: Vec<AnyElement>, narrow: bool) -> AnyElement {
        div()
            .id("content-scroll")
            .flex()
            .flex_col()
            .size_full()
            .overflow_y_scroll()
            .px(px(32.))
            .pt(px(4.))
            .pb(px(48.))
            .child(
                div()
                    .w_full()
                    .max_w(px(if narrow { 672. } else { 720. }))
                    .mx_auto()
                    .children(children),
            )
            .into_any_element()
    }

    fn setup_row(
        &self,
        number: usize,
        title: &str,
        body: &str,
        done: bool,
        action_label: &str,
        enabled: bool,
        action: Action,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        div()
            .flex()
            .gap(px(16.))
            .py(px(22.))
            .border_b_1()
            .border_color(rgba(0xffffff14))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(px(24.))
                    .rounded_full()
                    .border_1()
                    .border_color(rgba(0xffffff33))
                    .text_size(px(11.))
                    .text_color(rgb(if done { 0x40c977 } else { DIM }))
                    .child(if done {
                        icon("check", 14.).into_any_element()
                    } else {
                        div().child(number.to_string()).into_any_element()
                    }),
            )
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .child(
                        div()
                            .text_size(px(14.))
                            .font_weight(FontWeight::MEDIUM)
                            .child(title.to_owned()),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .line_height(px(19.))
                            .text_color(rgb(DIM))
                            .child(body.to_owned()),
                    )
                    .child(div().mt(px(5.)).flex().child(self.button(
                        &format!("setup-{number}"),
                        action_label.to_owned(),
                        false,
                        enabled,
                        action,
                        cx,
                    ))),
            )
            .into_any_element()
    }

    fn render_setup(&self, cx: &mut Context<Self>) -> AnyElement {
        let authed = self.tabs.iter().any(|t| t.authenticated);
        let mut children = vec![
            self.heading(self.text("setupTitle"), Some(self.text("setupSubtitle"))),
            self.section(self.text("coreSetup"), false),
        ];
        if !self.manual() {
            children.push(self.setup_row(
                1,
                &self.text("stepAccount"),
                &self.text("stepAccountBody"),
                authed,
                &self.text(if authed { "signedIn" } else { "signIn" }),
                !self.busy(),
                if authed { Action::CheckLogin } else { Action::OpenBrowser },
                cx,
            ));
            children.push(self.setup_row(
                2,
                &self.text("stepSmoke"),
                &self.text("stepSmokeBody"),
                self.state.browser_smoke_passed,
                &self.text(if self.state.browser_smoke_passed {
                    "smokePassed"
                } else {
                    "runSmoke"
                }),
                authed && !self.busy(),
                Action::Smoke,
                cx,
            ));
        }
        children.push(self.setup_row(
            if self.manual() { 1 } else { 3 },
            &self.text("stepInstall"),
            &self.text("stepInstallBody"),
            self.state.core_setup_complete == Some(true),
            &self.text(if self.state.core_setup_complete == Some(true) {
                "reinstall"
            } else {
                "install"
            }),
            !self.busy() && (self.manual() || self.state.browser_smoke_passed),
            Action::Install,
            cx,
        ));
        if self.state.codex_restart_required == Some(true) {
            children.push(self.notice(self.text("restartCodex"), true));
        }
        children.push(
            div()
                .mt(px(24.))
                .flex()
                .items_center()
                .justify_between()
                .child(div().text_size(px(13.)).child(self.text("mcpTitle")))
                .child(self.button(
                    "go-mcp",
                    self.text("configureMcp"),
                    false,
                    !self.busy(),
                    Action::Navigate(Surface::Mcp),
                    cx,
                ))
                .into_any_element(),
        );
        self.content(children, false)
    }

    fn render_mcp(&self, cx: &mut Context<Self>) -> AnyElement {
        let step = self.state.mcp_guide_step.min(2);
        let mut children =
            vec![self.heading(self.text("mcpTitle"), Some(self.text("mcpSubtitle")))];
        children.push(
            div()
                .flex()
                .gap(px(20.))
                .py(px(20.))
                .children((0..3u8).map(|i| {
                    div()
                        .id(("mcp-step", i as usize))
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .text_size(px(12.))
                        .text_color(rgb(if i == step { TEXT } else { DIM }))
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _, w, cx| {
                            this.dispatch(Action::McpStep(i), w, cx)
                        }))
                        .child(
                            div()
                                .size(px(22.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded_full()
                                .border_1()
                                .border_color(rgba(0xffffff33))
                                .child((i + 1).to_string()),
                        )
                        .child(self.text(["mcpStepOne", "mcpStepTwo", "mcpStepThree"][i as usize]))
                }))
                .into_any_element(),
        );
        children.push(
            div()
                .h(px(1.))
                .bg(rgba(0xffffff14))
                .mb(px(26.))
                .into_any_element(),
        );
        children.push(
            div()
                .text_size(px(18.))
                .font_weight(FontWeight::MEDIUM)
                .mb(px(12.))
                .child(self.text(["mcpStepOne", "mcpStepTwo", "mcpStepThree"][step as usize]))
                .into_any_element(),
        );
        children.push(
            div()
                .text_size(px(13.))
                .line_height(px(21.))
                .text_color(rgb(SECONDARY))
                .child(self.text(if step == 2 && self.manual() {
                    "manualMcpStepThreeBody"
                } else {
                    ["mcpStepOneBody", "mcpStepTwoBody", "mcpStepThreeBody"][step as usize]
                }))
                .into_any_element(),
        );
        match step {
            0 => children.push(
                div()
                    .flex()
                    .gap(px(10.))
                    .mt(px(24.))
                    .child(self.button(
                        "open-tunnels",
                        self.text("openTunnels"),
                        false,
                        true,
                        Action::External(TUNNELS_URL),
                        cx,
                    ))
                    .child(self.button(
                        "open-keys",
                        self.text("openKeys"),
                        false,
                        true,
                        Action::External(KEYS_URL),
                        cx,
                    ))
                    .into_any_element(),
            ),
            1 => {
                children.push(
                    div()
                        .mt(px(24.))
                        .flex()
                        .flex_col()
                        .gap(px(10.))
                        .child(
                            div()
                                .text_size(px(12.))
                                .text_color(rgb(DIM))
                                .child(self.text("tunnelId")),
                        )
                        .child(self.tunnel_id.clone())
                        .child(
                            div()
                                .mt(px(12.))
                                .text_size(px(12.))
                                .text_color(rgb(DIM))
                                .child(self.text("runtimeKey")),
                        )
                        .child(self.runtime_key.clone())
                        .child(div().mt(px(10.)).flex().child(self.button(
                            "connect-harness",
                            self.text("connect"),
                            true,
                            !self.busy(),
                            Action::SaveMcp,
                            cx,
                        )))
                        .into_any_element(),
                );
                children.push(self.notice(self.text("mcpStepTwoHint"), false));
            }
            _ => {
                children.push(
                    div()
                        .mt(px(22.))
                        .p(px(18.))
                        .rounded(px(10.))
                        .border_1()
                        .border_color(rgba(0xffffff20))
                        .child(
                            div()
                                .text_size(px(11.))
                                .text_color(rgb(DIM))
                                .mb(px(10.))
                                .child(self.text("connectorName")),
                        )
                        .child(
                            div()
                                .font_family("SF Mono")
                                .text_size(px(15.))
                                .child(self.connector_name()),
                        )
                        .into_any_element(),
                );
                children.push(self.notice(
                    self.text(if self.manual() {
                        "manualConnectorNotice"
                    } else {
                        "connectorMigrationNotice"
                    }),
                    false,
                ));
                children.push(
                    div()
                        .flex()
                        .gap(px(10.))
                        .child(self.button(
                            "open-connectors",
                            self.text("openConnectors"),
                            false,
                            true,
                            Action::External(CONNECTORS_URL),
                            cx,
                        ))
                        .child(self.button(
                            "verify-mcp",
                            self.text("verifyRuntime"),
                            true,
                            !self.busy(),
                            Action::VerifyMcp,
                            cx,
                        ))
                        .into_any_element(),
                );
            }
        }
        children.push(
            div()
                .flex()
                .justify_between()
                .mt(px(36.))
                .child(self.button(
                    "mcp-prev",
                    self.text("previous"),
                    false,
                    step > 0,
                    Action::McpStep(step.saturating_sub(1)),
                    cx,
                ))
                .child(self.button(
                    "mcp-next",
                    self.text(if step == 2 { "done" } else { "next" }),
                    false,
                    true,
                    if step == 2 {
                        Action::Navigate(Surface::Setup)
                    } else {
                        Action::McpStep(step + 1)
                    },
                    cx,
                ))
                .into_any_element(),
        );
        self.content(children, false)
    }

    fn render_activity(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut children = vec![self.heading(
            self.text("activityTitle"),
            Some(self.text("activitySubtitle")),
        )];
        for tab in self.tabs.iter().filter(|t| t.request_id.is_some()) {
            children.push(
                div()
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .py(px(14.))
                    .child(div().size(px(6.)).rounded_full().bg(rgb(ACCENT)))
                    .child(div().flex_1().text_size(px(13.)).child(tab.title.clone()))
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(rgb(DIM))
                            .child(tab.status.clone()),
                    )
                    .child(self.button(
                        &format!("activity-{}", tab.id),
                        self.text("openChatgpt"),
                        false,
                        true,
                        Action::SelectTab(tab.id.clone()),
                        cx,
                    ))
                    .into_any_element(),
            );
        }
        children.push(
            div()
                .flex()
                .items_center()
                .justify_between()
                .mb(px(14.))
                .child(self.section(self.text("recentActivity"), false))
                .child(self.button(
                    "export-logs",
                    self.text("exportSafeLog"),
                    false,
                    true,
                    Action::ExportLogs,
                    cx,
                ))
                .into_any_element(),
        );
        if self.logs.is_empty() {
            children.push(self.notice(self.text("noLogs"), false));
        }
        for log in self.logs.iter().rev() {
            let level = log["level"].as_str().unwrap_or("info");
            children.push(
                div()
                    .flex()
                    .gap(px(12.))
                    .py(px(14.))
                    .border_b_1()
                    .border_color(rgba(0xffffff14))
                    .child(div().mt(px(6.)).size(px(5.)).rounded_full().bg(rgb(
                        if level == "error" {
                            0xff6764
                        } else if level == "warning" {
                            0xff8549
                        } else {
                            0x40c977
                        },
                    )))
                    .child(
                        div()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .gap(px(5.))
                            .child(
                                div().text_size(px(12.)).child(
                                    log["event"].as_str().unwrap_or_default().replace('.', " "),
                                ),
                            )
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .line_height(px(17.))
                                    .text_color(rgb(DIM))
                                    .child(log["detail"].as_str().unwrap_or_default().to_owned()),
                            ),
                    )
                    .child(
                        div().text_size(px(10.)).text_color(rgb(DIM)).child(
                            log["at"]
                                .as_str()
                                .and_then(|s| s.get(11..19))
                                .unwrap_or_default()
                                .to_owned(),
                        ),
                    )
                    .into_any_element(),
            );
        }
        self.content(children, false)
    }

    fn render_limits(&self, cx: &mut Context<Self>) -> AnyElement {
        crate::limits_view::render_limits_surface(self, cx)
    }

    fn render_settings(&self, cx: &mut Context<Self>) -> AnyElement {
        let manual = self.manual();
        let ready = self.state.core_setup_complete == Some(true) && !self.busy();
        let mut children = vec![
            self.heading(self.text("settingsTitle"), None),
            self.section(self.text("general"), false),
        ];
        children.push(self.switch_row(
            "autoStart",
            &self.text("launchAtLogin"),
            &self.text("launchAtLoginBody"),
            self.state.auto_start,
            !self.busy(),
            cx,
        ));
        children.push(self.mode_picker(false, cx));
        children.push(
            div()
                .flex()
                .items_center()
                .gap(px(20.))
                .py(px(17.))
                .child(
                    div()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .gap(px(6.))
                        .text_size(px(13.))
                        .child(self.text("pluginName"))
                        .child(
                            div()
                                .text_size(px(12.))
                                .text_color(rgb(DIM))
                                .child(self.text("pluginNameBody")),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .w(px(215.))
                        .text_size(px(12.))
                        .child("Codex")
                        .child(div().flex_1().child(self.connector_suffix.clone())),
                )
                .into_any_element(),
        );
        children.push(
            div()
                .flex()
                .items_center()
                .justify_between()
                .pb(px(14.))
                .child(
                    div()
                        .font_family("SF Mono")
                        .text_size(px(11.))
                        .text_color(rgb(DIM))
                        .child(self.connector_name()),
                )
                .child(self.button(
                    "change-name",
                    self.text("pluginNameChange"),
                    false,
                    ready,
                    Action::ChangeConnector,
                    cx,
                ))
                .into_any_element(),
        );
        let rows = [
            (
                "keepRunningOnClose",
                "keepRunningOnClose",
                "keepRunningOnCloseBody",
                self.state.keep_running_on_close,
                !self.busy(),
            ),
            (
                "showBrowserDuringTurns",
                "showDuringTurns",
                "showDuringTurnsBody",
                self.state.show_browser_during_turns,
                !manual,
            ),
            (
                "experimentalBiggerContext",
                "biggerContext",
                if manual {
                    "manualBiggerContextUnavailable"
                } else {
                    "biggerContextBody"
                },
                self.state.experimental_bigger_context,
                ready && !manual && self.state.bigger_context_available == Some(true),
            ),
            (
                "composerPredictionsEnabled",
                "composerPredictions",
                "composerPredictionsBody",
                self.state.composer_predictions_enabled,
                ready,
            ),
            (
                "experimentalSkillAttachments",
                "skillAttachments",
                if manual {
                    "manualSkillAttachmentsUnavailable"
                } else {
                    "skillAttachmentsBody"
                },
                self.state.experimental_skill_attachments,
                ready && !manual,
            ),
            (
                "experimentalFreshConversationPerTurn",
                "freshConversation",
                if manual {
                    "manualFreshConversationUnavailable"
                } else {
                    "freshConversationBody"
                },
                self.state.experimental_fresh_conversation_per_turn,
                ready && !manual,
            ),
            (
                "autoApproveToolCalls",
                "autoApproveTools",
                if manual {
                    "manualAutoApproveUnavailable"
                } else {
                    "autoApproveToolsBody"
                },
                self.state.auto_approve_tool_calls && !manual,
                ready && !manual,
            ),
            (
                "useSavedChats",
                "savedChats",
                "savedChatsBody",
                self.state.use_saved_chats,
                ready,
            ),
        ];
        for (key, title, body, value, enabled) in rows {
            children.push(self.switch_row(
                key,
                &self.text(title),
                &self.text(body),
                value,
                enabled,
                cx,
            ));
        }
        children.push(
            div()
                .relative()
                .flex()
                .justify_between()
                .items_center()
                .py(px(18.))
                .child(div().text_size(px(13.)).child(self.text("language")))
                .child(
                    self.button(
                        "language-menu",
                        assets::LANGUAGES[&self.language]["label"]
                            .as_str()
                            .unwrap_or("English")
                            .to_owned(),
                        false,
                        true,
                        Action::LanguageMenu,
                        cx,
                    ),
                )
                .when(self.language_menu, |d| {
                    d.child(
                        div()
                            .absolute()
                            .right(px(0.))
                            .top(px(52.))
                            .w(px(180.))
                            .p(px(5.))
                            .rounded(px(10.))
                            .bg(rgb(0x282828))
                            .border_1()
                            .border_color(rgba(0xffffff29))
                            .children(["en", "zh-CN", "zh-TW", "ja", "ko"].map(|language| {
                                div()
                                    .id(SharedString::from(format!("language-choice-{language}")))
                                    .px(px(10.))
                                    .py(px(9.))
                                    .text_size(px(12.))
                                    .rounded(px(5.))
                                    .cursor_pointer()
                                    .hover(|s| s.bg(rgba(0xffffff12)))
                                    .on_click(cx.listener(move |this, _, w, cx| {
                                        this.dispatch(Action::Language(language.into()), w, cx)
                                    }))
                                    .child(
                                        assets::LANGUAGES[language]["label"]
                                            .as_str()
                                            .unwrap_or(language)
                                            .to_owned(),
                                    )
                            })),
                    )
                })
                .into_any_element(),
        );
        children.push(self.section(self.text("diagnostics"), true));
        for (id, name, title, body, action) in [
            ("doctor", "activity", "runDoctor", "status", Action::Doctor),
            (
                "cancel-turns",
                "close",
                "cancelTurns",
                "cancelTurnsBody",
                Action::CancelTurns,
            ),
            (
                "remove-integration",
                "close",
                "uninstallIntegration",
                "uninstallIntegrationBody",
                Action::RemoveIntegration,
            ),
        ] {
            children.push(
                div()
                    .id(id)
                    .flex()
                    .items_center()
                    .gap(px(14.))
                    .py(px(17.))
                    .border_b_1()
                    .border_color(rgba(0xffffff14))
                    .cursor_pointer()
                    .hover(|s| s.bg(rgba(0xffffff06)))
                    .on_click(
                        cx.listener(move |this, _, w, cx| this.dispatch(action.clone(), w, cx)),
                    )
                    .child(icon(name, 17.))
                    .child(
                        div()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .gap(px(5.))
                            .text_size(px(13.))
                            .child(self.text(title))
                            .child(
                                div()
                                    .text_size(px(11.))
                                    .line_height(px(17.))
                                    .text_color(rgb(DIM))
                                    .child(self.text(body)),
                            ),
                    )
                    .child(icon("chevron", 15.))
                    .into_any_element(),
            );
        }
        if let Some(report) = &self.doctor {
            if let Some(checks) = report["checks"].as_array() {
                for check in checks {
                    children.push(
                        self.notice(
                            check["message"]
                                .as_str()
                                .unwrap_or("Unknown check")
                                .to_owned(),
                            check["ok"] != true,
                        ),
                    );
                }
            }
        }
        children.push(
            div()
                .flex()
                .gap(px(12.))
                .items_center()
                .mt(px(34.))
                .text_size(px(12.))
                .child(icon("brand", 24.))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(5.))
                        .child(self.text("product"))
                        .child(
                            div()
                                .text_size(px(11.))
                                .text_color(rgb(DIM))
                                .child(format!("macOS · v{REFERENCE_VERSION}")),
                        ),
                )
                .into_any_element(),
        );
        self.content(children, true)
    }

    fn render_browser(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut body = div().relative().flex().flex_col().size_full();
        if !self.manual() {
            use crate::session_login::LoginPhase;
            let phase = self.session_job.as_ref().map(|job| job.phase);
            let waiting = phase == Some(LoginPhase::Waiting);
            let importing = matches!(phase, Some(LoginPhase::Importing | LoginPhase::Cancelling));
            let cancel = match self.language.as_str() {
                "zh-CN" | "zh-TW" => "取消", "ja" => "キャンセル", "ko" => "취소", _ => "Cancel",
            };
            let mut toolbar = div().flex().items_center().gap(px(8.)).h(px(46.))
                .flex_shrink_0().px(px(14.)).border_b_1().border_color(rgba(0xffffff14))
                .child(div().flex_1().text_size(px(12.)).text_color(rgb(DIM)).child(self.text(
                    if self.probe_inflight { "checkingSignIn" } else if self.session_account.is_some() { "signedIn" } else { "signIn" },
                )));
            if self.session_account.is_none() || phase.is_some() {
                toolbar = toolbar.child(self.button("browser-passkey", self.text(
                    if importing { "passkeyImporting" } else if waiting { "passkeyContinue" }
                    else if phase.is_some() { "passkeyOpening" } else { "passkeySignIn" },
                ), false, waiting || !self.busy(), if waiting { Action::PasskeyContinue } else { Action::PasskeyStart }, cx));
            } else {
                toolbar = toolbar.child(self.button("browser-logout", self.text("logOut"), false, !self.busy(), Action::Logout, cx));
            }
            if phase.is_some() {
                toolbar = toolbar.child(self.button("session-cancel", cancel, false,
                    phase != Some(LoginPhase::Cancelling), Action::CancelSession, cx));
            }
            body = body.child(toolbar);
            if matches!(phase, Some(LoginPhase::Opening | LoginPhase::Waiting | LoginPhase::Importing | LoginPhase::Cancelling)) {
                body = body.child(div().flex().flex_col().gap(px(5.)).px(px(20.)).py(px(14.))
                    .border_b_1().border_color(rgba(0xffffff14))
                    .child(div().text_size(px(13.)).font_weight(FontWeight::MEDIUM).child(self.text(
                        if importing { "passkeyImporting" } else if waiting { "passkeyWaitingTitle" } else { "passkeyOpening" },
                    )))
                    .child(div().text_size(px(12.)).line_height(px(18.)).text_color(rgb(SECONDARY)).child(self.text(
                        if importing { "passkeyImportingBody" } else if waiting { "passkeyContinueBody" } else { "passkeyOpeningBody" },
                    ))));
            }
        }
        if self.tabs.iter().any(|t| t.request_id.is_some()) || self.tabs.len() > 1 {
            body = body.child(
                div()
                    .flex()
                    .h(px(32.))
                    .gap(px(2.))
                    .px(px(10.))
                    .items_center()
                    .border_b_1()
                    .border_color(rgba(0xffffff14))
                    .children(self.tabs.iter().map(|tab| {
                        let id = tab.id.clone();
                        let close_id = id.clone();
                        div()
                            .id(SharedString::from(format!("tab-{id}")))
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .h(px(26.))
                            .px(px(10.))
                            .max_w(px(220.))
                            .rounded(px(6.))
                            .when(self.selected_tab.as_ref() == Some(&id), |d| {
                                d.bg(rgba(0xffffff12))
                            })
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _, w, cx| {
                                this.dispatch(Action::SelectTab(id.clone()), w, cx)
                            }))
                            .child(
                                div()
                                    .flex_1()
                                    .text_size(px(11.))
                                    .overflow_hidden()
                                    .child(tab.title.clone()),
                            )
                            .child(
                                div()
                                    .id(SharedString::from(format!("close-{close_id}")))
                                    .size(px(16.))
                                    .on_click(cx.listener(move |this, _, w, cx| {
                                        cx.stop_propagation();
                                        this.dispatch(Action::CloseTab(close_id.clone()), w, cx);
                                    }))
                                    .child(icon("close", 11.)),
                            )
                    })),
            );
        }
        if let Some(tab) = self.tab().filter(|t| t.visible) {
            body = body.child(
                div()
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_hidden()
                    .child(tab.webview.clone()),
            );
            let manual = tab.manual.as_ref().map(|manual| (manual.copied_at_ms, manual.sent, false))
                .or_else(|| tab.manual_checkpoint.as_ref().map(|manual| (manual.copied_at_ms, manual.sent, true)));
            if let Some((copied_at_ms, sent, checkpoint)) = manual {
                let remaining = presentation::manual_deadline_ms(checkpoint, copied_at_ms)
                    .saturating_sub(now_ms())
                    / 1000;
                body = body.child(
                    div()
                        .absolute()
                        .bottom(px(16.))
                        .left(px(20.))
                        .right(px(20.))
                        .p(px(18.))
                        .rounded(px(14.))
                        .bg(rgb(SIDEBAR))
                        .border_1()
                        .border_color(rgba(0xffffff29))
                        .child(
                            div()
                                .text_size(px(13.))
                                .font_weight(FontWeight::MEDIUM)
                                .mb(px(8.))
                                .child(self.text(if sent {
                                    "manualPromptWaiting"
                                } else {
                                    "manualPromptTitle"
                                })),
                        )
                        .child(
                            div()
                                .text_size(px(12.))
                                .line_height(px(19.))
                                .text_color(rgb(DIM))
                                .mb(px(14.))
                                .child(self.text("manualPromptInstruction")),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(8.))
                                .child(self.button(
                                    "manual-copy",
                                    self.text("manualPromptCopy"),
                                    false,
                                    !sent,
                                    Action::CopyManual,
                                    cx,
                                ))
                                .child(self.button(
                                    "manual-sent",
                                    self.text("manualPromptSent"),
                                    true,
                                    !sent && remaining > 0,
                                    Action::ConfirmManual,
                                    cx,
                                ))
                                .child(self.button(
                                    "manual-cancel",
                                    self.text("manualPromptCancel"),
                                    false,
                                    true,
                                    Action::CancelTurns,
                                    cx,
                                ))
                                .child(div().flex_1())
                                .when(!sent, |row| row.child(div().text_size(px(11.)).text_color(rgb(DIM)).child(
                                    format!("{remaining} {}", self.text("manualPromptSeconds")),
                                ))),
                        ),
                );
            }
        } else {
            body = body.child(
                div()
                    .flex()
                    .flex_1()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(12.))
                    .child(div().text_color(rgb(DIM)).child(icon("browser", 30.)))
                    .child(
                        div()
                            .text_size(px(15.))
                            .font_weight(FontWeight::MEDIUM)
                            .child(self.text("noActiveTask")),
                    )
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(rgb(DIM))
                            .max_w(px(380.))
                            .text_center()
                            .child(self.text("noActiveTaskBody")),
                    )
                    .child(div().mt(px(8.)).child(self.button(
                        "open-browser",
                        self.text("openChatgpt"),
                        false,
                        true,
                        Action::OpenBrowser,
                        cx,
                    ))),
            );
        }
        body.into_any_element()
    }

    fn render_shell(&self, cx: &mut Context<Self>) -> AnyElement {
        let content = match self.surface {
            Surface::Browser => self.render_browser(cx),
            Surface::Setup => self.render_setup(cx),
            Surface::Mcp => self.render_mcp(cx),
            Surface::Activity => self.render_activity(cx),
            Surface::Limits => self.render_limits(cx),
            Surface::Settings => self.render_settings(cx),
        };
        let sidebar = self.state.sidebar_open;
        div()
            .flex()
            .flex_col()
            .size_full()
            .child(self.render_titlebar(cx))
            .child(
                div()
                    .relative()
                    .flex()
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_hidden()
                    .when(sidebar && !self.compact, |d| {
                        d.child(self.render_sidebar(cx)).child(
                            div()
                                .id("resize-sidebar")
                                .w(px(3.))
                                .h_full()
                                .cursor(gpui::CursorStyle::ResizeLeftRight)
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, event: &gpui::MouseDownEvent, _, cx| {
                                        this.sidebar_drag =
                                            Some((event.position.x, this.state.sidebar_width));
                                        cx.stop_propagation();
                                    }),
                                ),
                        )
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .h_full()
                            .overflow_hidden()
                            .child(content),
                    )
                    .when(sidebar && self.compact, |d| {
                        d.child(
                            div()
                                .absolute()
                                .inset_0()
                                .bg(rgba(0x00000066))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(|this, _, w, cx| {
                                        this.dispatch(Action::ToggleSidebar, w, cx)
                                    }),
                                ),
                        )
                        .child(
                            div()
                                .absolute()
                                .left(px(0.))
                                .top(px(0.))
                                .bottom(px(0.))
                                .child(self.render_sidebar(cx)),
                        )
                    }),
            )
            .into_any_element()
    }
}

impl Render for Launcher {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let width: f32 = window.viewport_size().width.into();
        let height: f32 = window.viewport_size().height.into();
        if self.window_size != (width, height) {
            self.window_size = (width, height);
            let compact = width <= 860.;
            if compact != self.compact {
                self.state.sidebar_open = !compact;
                self.compact = compact;
            }
        }
        let content = if self.state.onboarding_complete {
            self.render_shell(cx)
        } else {
            self.render_welcome(cx)
        };
        div()
            .relative()
            .size_full()
            .bg(rgb(BG))
            .text_color(rgb(TEXT))
            .font_family(".AppleSystemUIFont")
            .text_size(px(13.))
            .track_focus(&self.focus)
            .on_mouse_move(cx.listener(|this, event: &gpui::MouseMoveEvent, _, cx| {
                if let Some((start, width)) = this.sidebar_drag {
                    this.state.sidebar_width =
                        (width + f64::from(f32::from(event.position.x - start))).clamp(240., 420.);
                    cx.notify();
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    if this.sidebar_drag.take().is_some() {
                        let width = this.state.sidebar_width.round();
                        if let Err(e) = this.persist(|s| s.sidebar_width = width) {
                            this.error = Some(e.to_string());
                        }
                        cx.notify();
                    }
                }),
            )
            .child(content)
            .when(self.reminder_visible && self.state.onboarding_complete, |d| {
                d.child(div().absolute().top(px(54.)).left(px(24.)).right(px(24.))
                    .p(px(16.)).rounded(px(12.)).bg(rgb(0x282828)).border_1().border_color(rgba(0xffffff29))
                    .flex().items_center().gap(px(14.))
                    .child(icon("alert", 20.))
                    .child(div().flex_1().flex().flex_col().gap(px(5.))
                        .child(self.text("sessionReminderTitle"))
                        .child(div().text_size(px(12.)).text_color(rgb(SECONDARY)).child(self.text("sessionReminderBody"))))
                    .child(self.button("dismiss-session-reminder", self.text("dismiss"), false, !self.busy(), Action::DismissReminder, cx))
                    .child(self.button("logout-session-reminder", self.text("logOut"), true, !self.busy(), Action::Logout, cx)))
            })
            .when_some(self.operation.clone(), |d, operation| {
                d.child(
                    div()
                        .absolute()
                        .bottom(px(16.))
                        .right(px(20.))
                        .p(px(14.))
                        .rounded(px(10.))
                        .bg(rgb(0x282828))
                        .border_1()
                        .border_color(rgba(0xffffff29))
                        .text_size(px(12.))
                        .child(operation),
                )
            })
            .when_some(self.error.clone(), |d, error| {
                d.child(
                    div()
                        .absolute()
                        .bottom(px(18.))
                        .left(px(24.))
                        .right(px(24.))
                        .max_w(px(680.))
                        .mx_auto()
                        .p(px(16.))
                        .rounded(px(14.))
                        .bg(rgb(0x282828))
                        .border_1()
                        .border_color(rgba(0xffffff29))
                        .child(
                            div()
                                .flex()
                                .items_start()
                                .gap(px(12.))
                                .child(
                                    div()
                                        .mt(px(5.))
                                        .size(px(6.))
                                        .rounded_full()
                                        .bg(rgb(0xff6764)),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .flex()
                                        .flex_col()
                                        .gap(px(5.))
                                        .text_size(px(12.))
                                        .child(self.text("error"))
                                        .child(
                                            div()
                                                .text_size(px(12.))
                                                .line_height(px(18.))
                                                .text_color(rgb(SECONDARY))
                                                .child(error),
                                        ),
                                )
                                .child(self.button(
                                    "dismiss-error",
                                    self.text("dismiss"),
                                    false,
                                    true,
                                    Action::DismissError,
                                    cx,
                                )),
                        ),
                )
            })
    }
}

pub fn icon(name: &str, size: f32) -> gpui::Svg {
    svg()
        .path(SharedString::from(format!("icons/{name}.svg")))
        .size(px(size))
        .flex_shrink_0()
}

fn radio(selected: bool) -> AnyElement {
    div()
        .size(px(16.))
        .rounded_full()
        .border_1()
        .border_color(rgba(if selected { 0xffffffff } else { 0xffffff44 }))
        .flex()
        .items_center()
        .justify_center()
        .when(selected, |d| {
            d.child(div().size(px(7.)).rounded_full().bg(rgb(TEXT)))
        })
        .into_any_element()
}

pub fn now_ms() -> u64 {
    chrono::Utc::now().timestamp_millis().max(0) as u64
}
