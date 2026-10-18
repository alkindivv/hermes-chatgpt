use anyhow::Result;
use cw_runtime::{Profile, WindowSnapshot, WindowState, WindowStateStore, WindowStateTracker};
use gpui::{App, Bounds, Window, WindowBounds, point, px, size};
use std::time::Instant;

/// Only the explicit preference action changes the NEW application's login item.
pub fn set_autostart(enabled: bool, profile: &Profile) -> Result<()> {
    cw_runtime::set_autostart(enabled, profile)?;
    Ok(())
}

pub fn restore_window(profile: &Profile, cx: &App) -> Result<(WindowState, WindowBounds)> {
    let displays = cx.displays().into_iter().map(|display| {
        let bounds = display.visible_bounds();
        cw_runtime::WorkArea {
            x: f32::from(bounds.origin.x) as f64,
            y: f32::from(bounds.origin.y) as f64,
            width: f32::from(bounds.size.width) as f64,
            height: f32::from(bounds.size.height) as f64,
        }
    }).collect::<Vec<_>>();
    let state = WindowStateStore::new(profile.clone()).load(&displays)?;
    let size = size(px(state.bounds.width as f32), px(state.bounds.height as f32));
    let bounds = match (state.bounds.x, state.bounds.y) {
        (Some(x), Some(y)) => Bounds::new(point(px(x as f32), px(y as f32)), size),
        _ => Bounds::centered(None, size, cx),
    };
    let mode = if state.fullscreen {
        WindowBounds::Fullscreen(bounds)
    } else if state.maximized {
        WindowBounds::Maximized(bounds)
    } else { WindowBounds::Windowed(bounds) };
    Ok((state, mode))
}

pub struct WindowPersistence {
    tracker: WindowStateTracker,
    store: WindowStateStore,
    last: WindowState,
}

impl WindowPersistence {
    pub fn new(profile: Profile, restored: WindowState) -> Self {
        Self { tracker: WindowStateTracker::new(profile.clone()), store: WindowStateStore::new(profile), last: restored }
    }
    pub fn changed(&mut self, window: &Window) -> Result<()> {
        let bounds = window.bounds();
        let maximized = window.is_maximized();
        let fullscreen = window.is_fullscreen();
        let current = cw_runtime::WindowBounds {
            x: Some(f32::from(bounds.origin.x) as f64),
            y: Some(f32::from(bounds.origin.y) as f64),
            width: f32::from(bounds.size.width) as f64,
            height: f32::from(bounds.size.height) as f64,
        };
        let normal = if maximized || fullscreen { self.last.bounds.clone() } else { current.clone() };
        let snapshot = WindowSnapshot { bounds: current, normal_bounds: normal, maximized, fullscreen, destroyed: false };
        if let Some(next) = cw_runtime::capture_window_state(&snapshot)? {
            self.last = next;
        }
        self.tracker.schedule(&snapshot, Instant::now())
    }
    pub fn flush_due(&mut self) -> Result<()> {
        self.tracker.flush_due(Instant::now())?;
        Ok(())
    }
    pub fn close(&mut self, window: &Window) -> Result<()> {
        self.changed(window)?;
        self.flush()
    }
    pub fn flush(&self) -> Result<()> {
        self.store.save(&self.last)
    }
}
