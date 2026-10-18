//! Native desktop policies for the separate GPUI application identity.
use crate::*;
pub const APPLICATION_ID: &str = "dev.alkindi.codex-web-gpui";
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct LoginItemStatus {
    pub supported: bool,
    pub enabled: bool,
    pub requires_approval: bool,
}
/// Implementations must scope all operations to identity(), never enumerate/remove other items.
pub trait LoginItemBackend {
    fn identity(&self) -> &str;
    fn status(&self) -> Result<LoginItemStatus>;
    fn set_enabled(&self, enabled: bool) -> Result<()>;
}
pub struct LaunchOnLogin<B> {
    profile: Profile,
    backend: B,
}
impl<B: LoginItemBackend> LaunchOnLogin<B> {
    pub fn new(profile: Profile, backend: B) -> Result<Self> {
        ensure!(
            backend.identity() == APPLICATION_ID,
            "foreign login item identity"
        );
        Ok(Self { profile, backend })
    }
    pub fn status(&self) -> Result<LoginItemStatus> {
        ensure!(
            self.backend.identity() == APPLICATION_ID,
            "foreign login item identity"
        );
        self.backend.status()
    }
    pub fn set_enabled(&self, enabled: bool) -> Result<LoginItemStatus> {
        let before = self.status()?;
        ensure!(before.supported, "launch on login unsupported");
        if before.enabled != enabled || before.requires_approval {
            self.backend.set_enabled(enabled)?;
        }
        let status = self.status()?;
        ensure!(
            status.supported && status.enabled == enabled && !status.requires_approval,
            "operating system did not confirm requested launch-on-login state (approval may be required)"
        );
        self.profile.write_json(
            "runtime/login-item.json",
            &json!({"version":1,"identity":APPLICATION_ID,"enabled":status.enabled}),
        )?;
        Ok(status)
    }
    pub fn remove(&self) -> Result<LoginItemStatus> {
        self.set_enabled(false)
    }
}
/// Onboarding remains visible even when a login launch or --hidden requested background startup.
pub fn should_start_hidden(hidden_requested: bool, onboarding_complete: bool) -> bool {
    hidden_requested && onboarding_complete
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WindowBounds {
    pub width: f64,
    pub height: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub x: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub y: Option<f64>,
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct WindowState {
    pub bounds: WindowBounds,
    pub maximized: bool,
    pub fullscreen: bool,
}
#[derive(Clone, Copy, Debug)]
pub struct WorkArea {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}
impl Default for WindowState {
    fn default() -> Self {
        Self {
            bounds: WindowBounds {
                width: 1120.,
                height: 720.,
                x: None,
                y: None,
            },
            maximized: false,
            fullscreen: false,
        }
    }
}
fn number(v: &Value, fallback: f64) -> f64 {
    v.as_f64().filter(|v| v.is_finite()).unwrap_or(fallback)
}
pub fn normalize_window_state(value: &Value, displays: &[WorkArea]) -> WindowState {
    let b = &value["bounds"];
    let mut state = WindowState {
        bounds: WindowBounds {
            width: number(&b["width"], 1120.).clamp(720., 16384.),
            height: number(&b["height"], 720.).clamp(600., 16384.),
            x: None,
            y: None,
        },
        maximized: value["maximized"] == true,
        fullscreen: value["fullscreen"] == true,
    };
    let (Some(x), Some(y)) = (
        b["x"].as_f64().filter(|v| v.is_finite()),
        b["y"].as_f64().filter(|v| v.is_finite()),
    ) else {
        return state;
    };
    if displays.is_empty() {
        state.bounds.x = Some(x);
        state.bounds.y = Some(y);
        return state;
    }
    let mut selected = None;
    let mut overlap = 0.;
    for area in displays {
        if ![area.x, area.y, area.width, area.height]
            .iter()
            .all(|n| n.is_finite())
            || area.width <= 0.
            || area.height <= 0.
        {
            continue;
        }
        let width = ((x + state.bounds.width).min(area.x + area.width) - x.max(area.x)).max(0.);
        let height = ((y + state.bounds.height).min(area.y + area.height) - y.max(area.y)).max(0.);
        if width * height > overlap {
            overlap = width * height;
            selected = Some(area);
        }
    }
    if let Some(area) = selected {
        state.bounds.width = state.bounds.width.min(area.width).max(720.);
        state.bounds.height = state.bounds.height.min(area.height).max(600.);
        state.bounds.x = Some(x.min(area.x + area.width - state.bounds.width).max(area.x));
        state.bounds.y = Some(
            y.min(area.y + area.height - state.bounds.height)
                .max(area.y),
        );
    }
    state
}
fn valid_bounds(b: &WindowBounds) -> bool {
    b.width.is_finite()
        && (720. ..=16384.).contains(&b.width)
        && b.height.is_finite()
        && (600. ..=16384.).contains(&b.height)
        && match (b.x, b.y) {
            (None, None) => true,
            (Some(x), Some(y)) => x.is_finite() && y.is_finite(),
            _ => false,
        }
}
#[derive(Clone, Debug)]
pub struct WindowSnapshot {
    pub bounds: WindowBounds,
    pub normal_bounds: WindowBounds,
    pub maximized: bool,
    pub fullscreen: bool,
    pub destroyed: bool,
}
pub fn capture_window_state(snapshot: &WindowSnapshot) -> Result<Option<WindowState>> {
    if snapshot.destroyed {
        return Ok(None);
    }
    let bounds = if snapshot.maximized || snapshot.fullscreen {
        &snapshot.normal_bounds
    } else {
        &snapshot.bounds
    };
    ensure!(
        valid_bounds(bounds),
        "invalid window bounds; retaining last valid state"
    );
    Ok(Some(WindowState {
        bounds: bounds.clone(),
        maximized: snapshot.maximized,
        fullscreen: snapshot.fullscreen,
    }))
}
pub struct WindowStateStore {
    profile: Profile,
}
impl WindowStateStore {
    pub fn new(profile: Profile) -> Self {
        Self { profile }
    }
    pub fn load(&self, displays: &[WorkArea]) -> Result<WindowState> {
        let path = self.profile.path("user-data/window-state.json")?;
        let value = fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or(Value::Null);
        Ok(normalize_window_state(&value, displays))
    }
    pub fn save(&self, state: &WindowState) -> Result<()> {
        ensure!(
            valid_bounds(&state.bounds),
            "invalid window state; retaining prior file"
        );
        self.profile
            .write_json("user-data/window-state.json", state)
    }
}
pub struct WindowStateTracker {
    store: WindowStateStore,
    pending: Option<(WindowState, Instant)>,
}
impl WindowStateTracker {
    pub fn new(profile: Profile) -> Self {
        Self {
            store: WindowStateStore::new(profile),
            pending: None,
        }
    }
    pub fn schedule(&mut self, snapshot: &WindowSnapshot, now: Instant) -> Result<()> {
        if let Some(state) = capture_window_state(snapshot)? {
            self.pending = Some((state, now));
        } else {
            self.pending = None;
        }
        Ok(())
    }
    pub fn flush_due(&mut self, now: Instant) -> Result<bool> {
        let Some((state, at)) = self.pending.as_ref() else {
            return Ok(false);
        };
        if now.saturating_duration_since(*at) < Duration::from_millis(400) {
            return Ok(false);
        }
        self.store.save(state)?;
        self.pending = None;
        Ok(true)
    }
    pub fn close(&mut self, snapshot: &WindowSnapshot) -> Result<()> {
        if let Some(state) = capture_window_state(snapshot)? {
            self.store.save(&state)?;
        }
        self.pending = None;
        Ok(())
    }
}
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub use macos::{NativeLoginItem, autostart_status, set_autostart};

#[cfg(test)]
mod tests {
    use super::*;
    use std::{cell::RefCell, rc::Rc};
    struct Fixture {
        identity: String,
        items: Rc<RefCell<std::collections::BTreeMap<String, bool>>>,
        accept: bool,
    }
    impl LoginItemBackend for Fixture {
        fn identity(&self) -> &str {
            &self.identity
        }
        fn status(&self) -> Result<LoginItemStatus> {
            Ok(LoginItemStatus {
                supported: true,
                enabled: *self.items.borrow().get(&self.identity).unwrap_or(&false),
                requires_approval: false,
            })
        }
        fn set_enabled(&self, enabled: bool) -> Result<()> {
            if self.accept {
                self.items
                    .borrow_mut()
                    .insert(self.identity.clone(), enabled);
            }
            Ok(())
        }
    }
    fn profile() -> Result<(tempfile::TempDir, Profile)> {
        let t = tempfile::tempdir()?;
        let p = Profile::isolated(fs::canonicalize(t.path())?.join("owned"))?;
        Ok((t, p))
    }
    #[test]
    fn login_readback_and_only_own_removal() -> Result<()> {
        let (_t, p) = profile()?;
        let items = Rc::new(RefCell::new(std::collections::BTreeMap::from([(
            "dev.codexwebgpt.launcher".into(),
            true,
        )])));
        let controller = LaunchOnLogin::new(
            p.clone(),
            Fixture {
                identity: APPLICATION_ID.into(),
                items: items.clone(),
                accept: true,
            },
        )?;
        assert!(!controller.status()?.enabled);
        assert!(controller.set_enabled(true)?.enabled);
        assert!(!controller.remove()?.enabled);
        assert_eq!(items.borrow()["dev.codexwebgpt.launcher"], true);
        let denied = LaunchOnLogin::new(
            p.clone(),
            Fixture {
                identity: APPLICATION_ID.into(),
                items: items.clone(),
                accept: false,
            },
        )?;
        assert!(denied.set_enabled(true).is_err());
        assert!(
            LaunchOnLogin::new(
                p,
                Fixture {
                    identity: "dev.codexwebgpt.launcher".into(),
                    items,
                    accept: true
                }
            )
            .is_err()
        );
        Ok(())
    }
    #[test]
    fn display_policy_matches_upstream() -> Result<()> {
        let area = [WorkArea {
            x: 0.,
            y: 0.,
            width: 1512.,
            height: 982.,
        }];
        for (x, y, expected_x, expected_y) in [
            (1511., 400., 392., 262.),
            (100., -700., 100., 0.),
            (100., 981., 100., 262.),
        ] {
            let s = normalize_window_state(
                &json!({"bounds":{"width":1120,"height":720,"x":x,"y":y}}),
                &area,
            );
            assert_eq!(
                (s.bounds.x, s.bounds.y),
                (Some(expected_x), Some(expected_y))
            );
        }
        let s = normalize_window_state(
            &json!({"bounds":{"width":200,"height":100,"x":9000,"y":9000}}),
            &area,
        );
        assert_eq!(
            (s.bounds.width, s.bounds.height, s.bounds.x),
            (720., 600., None)
        );
        let s = normalize_window_state(
            &json!({"bounds":{"width":99999999,"height":99999999}}),
            &area,
        );
        assert_eq!((s.bounds.width, s.bounds.height), (16384., 16384.));
        Ok(())
    }
    #[test]
    fn persist_normal_bounds_debounce_and_retain_after_display_change() -> Result<()> {
        let (_t, p) = profile()?;
        let bounds = WindowBounds {
            width: 1120.,
            height: 720.,
            x: Some(-1000.),
            y: Some(50.),
        };
        let snapshot = WindowSnapshot {
            bounds: WindowBounds {
                width: 1920.,
                height: 1080.,
                x: Some(0.),
                y: Some(0.),
            },
            normal_bounds: bounds.clone(),
            maximized: true,
            fullscreen: false,
            destroyed: false,
        };
        let store = WindowStateStore::new(p.clone());
        let mut tracker = WindowStateTracker::new(p.clone());
        let now = Instant::now();
        tracker.schedule(&snapshot, now)?;
        assert!(!tracker.flush_due(now + Duration::from_millis(399))?);
        assert!(tracker.flush_due(now + Duration::from_millis(400))?);
        let bytes = fs::read(p.path("user-data/window-state.json")?)?;
        assert_eq!(store.load(&[])?.bounds, bounds);
        let disconnected = [WorkArea {
            x: 0.,
            y: 0.,
            width: 1512.,
            height: 982.,
        }];
        let state = store.load(&disconnected)?;
        assert_eq!(state.bounds.x, Some(0.));
        assert_eq!(fs::read(p.path("user-data/window-state.json")?)?, bytes);
        let mut invalid = snapshot.clone();
        invalid.normal_bounds.width = f64::NAN;
        assert!(tracker.close(&invalid).is_err());
        assert_eq!(store.load(&[])?.bounds, bounds);
        let restored = [WorkArea {
            x: -1280.,
            y: 0.,
            width: 1280.,
            height: 800.,
        }];
        assert_eq!(store.load(&restored)?.bounds.x, Some(-1120.));
        assert_eq!(
            fs::metadata(p.path("user-data/window-state.json")?)?
                .permissions()
                .mode()
                & 0o077,
            0
        );
        Ok(())
    }
    #[test]
    fn approval_is_not_enabled_and_onboarding_stays_visible() -> Result<()> {
        struct Approval;
        impl LoginItemBackend for Approval {
            fn identity(&self) -> &str {
                APPLICATION_ID
            }
            fn status(&self) -> Result<LoginItemStatus> {
                Ok(LoginItemStatus {
                    supported: true,
                    enabled: false,
                    requires_approval: true,
                })
            }
            fn set_enabled(&self, _: bool) -> Result<()> {
                Ok(())
            }
        }
        let (_t, p) = profile()?;
        let login = LaunchOnLogin::new(p.clone(), Approval)?;
        assert!(!login.status()?.enabled);
        assert!(login.set_enabled(true).is_err());
        assert!(login.remove().is_err());
        assert!(!p.path("runtime/login-item.json")?.exists());
        assert!(!should_start_hidden(true, false));
        assert!(should_start_hidden(true, true));
        assert!(!should_start_hidden(false, true));
        Ok(())
    }
    #[test]
    fn invalid_state_defaults_and_close_flushes_without_destroyed_write() -> Result<()> {
        let (_t, p) = profile()?;
        let store = WindowStateStore::new(p.clone());
        p.write_json(
            "user-data/window-state.json",
            &json!({"bounds":{"width":"bad","height":null,"x":1},"maximized":"true"}),
        )?;
        assert_eq!(store.load(&[])?, WindowState::default());
        let snapshot = WindowSnapshot {
            bounds: WindowState::default().bounds,
            normal_bounds: WindowState::default().bounds,
            maximized: false,
            fullscreen: true,
            destroyed: false,
        };
        let mut tracker = WindowStateTracker::new(p.clone());
        tracker.schedule(&snapshot, Instant::now())?;
        tracker.close(&snapshot)?;
        assert!(store.load(&[])?.fullscreen);
        let before = fs::read(p.path("user-data/window-state.json")?)?;
        let mut destroyed = snapshot;
        destroyed.destroyed = true;
        tracker.close(&destroyed)?;
        assert_eq!(fs::read(p.path("user-data/window-state.json")?)?, before);
        Ok(())
    }
}
