mod assets;
mod backend;
mod checkpoints;
mod delivery;
mod input;
mod limits_view;
mod platform;
mod presentation;
mod session;
mod session_ui;
mod session_login;
mod mcp_ui;
mod ui;

use anyhow::{Context as _, Result, bail, ensure};
use cw_bridge::{BridgeConfig, BridgeServer, ExecutionMode};
use cw_contracts::{BrowserInteractionMode, LauncherState, Surface};
use cw_runtime::Profile;
use gpui::{
    App, AppContext, KeyBinding, Menu, MenuItem, TitlebarOptions,
    WindowOptions, actions, point, px, size,
};
use serde_json::{Value, json};
use std::{
    fs,
    net::{Ipv4Addr, SocketAddr, TcpListener},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

actions!(codex_web_gpui, [Quit, ShowWindow]);

struct Options {
    profile: Option<PathBuf>,
    hidden: bool,
    diagnostics: bool,
    fixture_url: Option<String>,
    preview: Option<Surface>,
}

impl Options {
    fn parse() -> Result<Self> {
        let mut result = Self {
            profile: None,
            hidden: false,
            diagnostics: false,
            fixture_url: None,
            preview: None,
        };
        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--profile" => {
                    result.profile = Some(PathBuf::from(
                        args.next().context("--profile requires an absolute path")?,
                    ))
                }
                "--hidden" => result.hidden = true,
                "--diagnostics" => result.diagnostics = true,
                "--fixture-url" => {
                    let value = args.next().context("--fixture-url requires an address")?;
                    let url = url::Url::parse(&value)?;
                    ensure!(
                        url.scheme() == "http" && url.host_str() == Some("127.0.0.1"),
                        "Fixtures must use an explicit local HTTP server"
                    );
                    result.fixture_url = Some(value);
                }
                "--preview-surface" => {
                    result.preview = Some(match args.next().as_deref() {
                        Some("browser") => Surface::Browser,
                        Some("setup") => Surface::Setup,
                        Some("mcp") => Surface::Mcp,
                        Some("activity") => Surface::Activity,
                        Some("limits") => Surface::Limits,
                        Some("settings") => Surface::Settings,
                        _ => bail!("Unknown preview surface"),
                    })
                }
                "--version" => {
                    println!(
                        "Codex Web GPUI 0.1.0; upstream reference {}",
                        ui::REFERENCE_VERSION
                    );
                    std::process::exit(0);
                }
                "--help" => {
                    println!(
                        "Codex Web GPUI\n  --profile /absolute/owned/profile\n  --hidden\n  --diagnostics\nDevelopment inspection requires --profile:\n  --preview-surface browser|setup|mcp|activity|limits|settings\n  --fixture-url http://127.0.0.1:<port>/fixture"
                    );
                    std::process::exit(0);
                }
                _ => bail!("Unknown option: {arg}"),
            }
        }
        ensure!(
            result.profile.is_some() || (result.preview.is_none() && result.fixture_url.is_none()),
            "A development preview requires an explicit isolated profile"
        );
        Ok(result)
    }
}

fn main() {
    env_logger::init();
    if let Err(error) = run() {
        eprintln!("Codex Web GPUI: {error:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let options = Options::parse()?;
    let profile = match options.profile {
        Some(path) => Profile::isolated(path)?,
        None => Profile::default_isolated()?,
    };
    let saved = match profile.read_json::<Value>("user-data/launcher-state.json") {
        Ok(value) => LauncherState::normalize(value)?,
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
        {
            LauncherState::default()
        }
        Err(error) => return Err(error.context("Cannot read the isolated launcher state")),
    };
    if options.diagnostics {
        println!(
            "{}",
            serde_json::to_string_pretty(&cw_runtime::RuntimeFacade::new(profile).diagnostics())?
        );
        return Ok(());
    }
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(profile.path("runtime/launcher.lock")?)?;
    fs2::FileExt::try_lock_exclusive(&lock).context("This native profile is already open")?;
    let executor = Arc::new(
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?,
    );
    let mode = if saved.browser_interaction_mode == BrowserInteractionMode::Manual {
        ExecutionMode::Manual
    } else {
        ExecutionMode::Automatic
    };
    let preferred_port = profile
        .read_json::<Value>("runtime/listener.json")
        .ok()
        .and_then(|v| v["port"].as_u64())
        .and_then(|v| u16::try_from(v).ok())
        .unwrap_or(0);
    let (bridge, requests) = executor.block_on(BridgeServer::bind(
        SocketAddr::from((Ipv4Addr::LOCALHOST, preferred_port)),
        BridgeConfig {
            mode,
            ..Default::default()
        },
    ))?;
    session::apply_launcher_settings(&bridge, &saved).map_err(anyhow::Error::msg)?;
    profile.write_json("runtime/listener.json",&json!({"port":bridge.local_addr.port(),"pid":std::process::id(),"baseUrl":bridge.base_url()}))?;
    let reservation = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    let cdp_port = reservation.local_addr()?.port();
    drop(reservation);
    let cef_runtime = gpui_cef::init(gpui_cef::RuntimeOptions {
        cache_path: Some(profile.browser()),
        remote_debugging_port: Some(cdp_port),
        log_to_stderr: false,
        log_file: Some(profile.path("runtime/cef.log")?),
    })?;
    let cef = cef_runtime.clone();
    let close_bridge = bridge.clone();
    let app_executor = executor.clone();
    let private_profile = profile.clone();
    let mut initial = saved;
    if options.preview.is_some() {
        initial.onboarding_complete = true;
        initial.language = Some("en".into());
    }
    let options_preview = options.preview;
    let fixture_url = options.fixture_url;
    let hidden = cw_runtime::should_start_hidden(options.hidden, initial.onboarding_complete);
    let application = gpui_platform::application().with_assets(assets::Assets);
    application.on_reopen(|cx| {
        for window in cx.windows() {
            let _ = window.update(cx, |_, window, _| window.activate_window());
        }
        cx.activate(true);
    });
    application.run(move |cx: &mut App| {
        if let Err(error) = cef.start(cx) {
            log::error!("Cannot initialize native Chromium: {error}");
            cx.quit();
            return;
        }
        input::bind_keys(cx);
        cx.bind_keys([KeyBinding::new("cmd-q", Quit, None)]);
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.on_action(|_: &ShowWindow, cx| {
            for window in cx.windows() {
                let _ = window.update(cx, |_, window, _| window.activate_window());
            }
            cx.activate(true);
        });
        cx.set_menus([Menu {
            name: "Codex Web GPUI".into(),
            items: vec![MenuItem::action("Show Codex Web GPUI", ShowWindow), MenuItem::Separator, MenuItem::action("Quit Codex Web GPUI", Quit)],
            disabled: false,
        }]);
        let (restored_window, bounds) = match platform::restore_window(&private_profile, cx) {
            Ok(value) => value,
            Err(error) => {
                log::error!("Cannot restore native window: {error}");
                cx.quit();
                return;
            }
        };
        let result = cx.open_window(
            WindowOptions {
                window_bounds: Some(bounds),
                titlebar: Some(TitlebarOptions {
                    title: Some("Codex Web GPUI".into()),
                    appears_transparent: true,
                    traffic_light_position: Some(point(px(16.), px(17.))),
                }),
                window_min_size: Some(size(px(720.), px(600.))),
                app_owns_titlebar_drag: true,
                show: !hidden,
                focus: !hidden,
                ..Default::default()
            },
            move |window, cx| {
                let view = cx.new(|cx| {
                    let mut launcher = ui::Launcher::new(
                        private_profile,
                        initial,
                        bridge,
                        requests,
                        app_executor,
                        cdp_port,
                        fixture_url,
                        restored_window,
                        window,
                        cx,
                    );
                    if let Some(surface) = options_preview {
                        launcher.surface = surface;
                    }
                    launcher
                });
                let weak = view.downgrade();
                window.on_window_should_close(cx, move |window, cx| {
                    let keep = weak
                        .upgrade()
                        .is_some_and(|entity| entity.update(cx, |launcher, _| {
                            if let Err(error) = launcher.window_persistence.close(window) {
                                log::error!("Cannot save native window: {error}");
                            }
                            launcher.state.keep_running_on_close
                        }));
                    if keep {
                        cx.hide();
                        false
                    } else {
                        true
                    }
                });
                view
            },
        );
        match result {
            Ok(_window) => {
                if !hidden {
                    cx.activate(true);
                }
            }
            Err(error) => {
                log::error!("Cannot open native window: {error}");
                cx.quit();
            }
        }
        cx.on_window_closed(|cx, _| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();
    });
    let result = executor.block_on(async {
        tokio::time::timeout(Duration::from_secs(5), close_bridge.close()).await
    });
    if !matches!(result, Ok(Ok(()))) {
        log::warn!("Native bridge did not fully drain before the local shutdown deadline");
    }
    cef_runtime.shutdown();
    drop(lock);
    Ok(())
}
