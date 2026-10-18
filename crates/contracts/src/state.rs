//! Launcher state policy from upstream state.cjs (MIT; see LICENSE.upstream).
use crate::{ContractError, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum BrowserInteractionMode {
    #[default]
    Automatic,
    Manual,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Surface {
    #[default]
    Browser,
    Setup,
    Mcp,
    Activity,
    Limits,
    Settings,
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LauncherState {
    pub version: u8,
    pub language: Option<String>,
    pub onboarding_complete: bool,
    pub github_opened: bool,
    pub x_opened: bool,
    pub auto_start: bool,
    pub keep_running_on_close: bool,
    pub show_browser_during_turns: bool,
    pub auto_approve_tool_calls: bool,
    pub browser_interaction_mode: BrowserInteractionMode,
    pub experimental_bigger_context: bool,
    pub composer_predictions_enabled: bool,
    pub experimental_skill_attachments: bool,
    pub experimental_fresh_conversation_per_turn: bool,
    pub use_saved_chats: bool,
    pub zero_risk_pro_enabled: bool,
    pub browser_smoke_passed: bool,
    pub browser_smoke_version: Option<String>,
    pub sidebar_open: bool,
    pub sidebar_width: f64,
    pub mcp_guide_step: u8,
    pub session_refresh_reminder_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bigger_context_available: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub core_setup_complete: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub codex_catalog_verified: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mcp_setup_complete: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mcp_runtime_installed: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub codex_restart_required: Option<bool>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}
impl Default for LauncherState {
    fn default() -> Self {
        serde_json::from_str(include_str!("fixtures/default-state.json"))
            .expect("pinned default state")
    }
}
impl LauncherState {
    /// Upstream readState repair, including preserving unknown extension fields.
    pub fn normalize(value: Value) -> Result<Self> {
        let defaults: Value = serde_json::from_str(include_str!("fixtures/default-state.json"))?;
        let Some(input) = value
            .as_object()
            .filter(|v| v.get("version") == Some(&Value::from(1)))
        else {
            return Ok(Self::default());
        };
        let mut m = defaults.as_object().unwrap().clone();
        m.extend(input.clone());
        m.remove("bridgeEnabled");
        for (k, v) in defaults.as_object().unwrap() {
            if v.is_boolean() && !m[k].is_boolean() {
                m.insert(k.clone(), v.clone());
            }
        }
        let langs: Value = serde_json::from_str(include_str!("fixtures/languages.json"))?;
        if !m["language"].is_null()
            && !m["language"]
                .as_str()
                .is_some_and(|s| langs.get(s).is_some())
        {
            m.insert("language".into(), Value::Null);
        }
        if !matches!(
            m["browserInteractionMode"].as_str(),
            Some("automatic" | "manual")
        ) {
            m.insert("browserInteractionMode".into(), Value::from("automatic"));
        }
        if m.get("coreSetupComplete") != Some(&Value::Bool(true)) {
            if m["onboardingComplete"] != true {
                m.insert("browserInteractionMode".into(), Value::from("automatic"));
            }
            m.insert("zeroRiskProEnabled".into(), Value::Bool(false));
        }
        if !m["browserSmokeVersion"].is_null()
            && !m["browserSmokeVersion"]
                .as_str()
                .is_some_and(|s| s.encode_utf16().count() <= 128)
        {
            m.insert("browserSmokeVersion".into(), Value::Null);
        }
        if !m["sidebarWidth"]
            .as_f64()
            .is_some_and(|n| (240.0..=420.0).contains(&n))
        {
            m.insert("sidebarWidth".into(), Value::from(252));
        }
        if !m["mcpGuideStep"]
            .as_f64()
            .is_some_and(|n| n.fract() == 0.0 && (0.0..=2.0).contains(&n))
        {
            m.insert("mcpGuideStep".into(), Value::from(0));
        } else {
            let n = m["mcpGuideStep"].as_f64().unwrap() as u8;
            m.insert("mcpGuideStep".into(), Value::from(n));
        }
        if !m["sessionRefreshReminderAt"].is_null()
            && !m["sessionRefreshReminderAt"]
                .as_str()
                .is_some_and(valid_date)
        {
            m.insert("sessionRefreshReminderAt".into(), Value::Null);
        }
        for k in [
            "biggerContextAvailable",
            "coreSetupComplete",
            "codexCatalogVerified",
            "mcpSetupComplete",
            "mcpRuntimeInstalled",
            "codexRestartRequired",
        ] {
            if m.get(k).is_some_and(|v| !v.is_boolean()) {
                m.remove(k);
            }
        }
        Ok(serde_json::from_value(Value::Object(m))?)
    }
    pub fn set_preference(&mut self, key: &str, value: &Value) -> Result<()> {
        match key {
            "keepRunningOnClose" => self.keep_running_on_close = value == true,
            "showBrowserDuringTurns" => self.show_browser_during_turns = value == true,
            _ => return Err(ContractError::Policy("Unknown preference".into())),
        };
        Ok(())
    }
    pub fn set_sidebar(&mut self, open: bool, width: f64) -> Result<()> {
        if !width.is_finite() || !(240.0..=420.0).contains(&width) {
            return Err(ContractError::Policy(
                "Sidebar width must be between 240 and 420".into(),
            ));
        }
        self.sidebar_open = open;
        self.sidebar_width = width.round();
        Ok(())
    }
    pub fn set_mcp_step(&mut self, step: u8) -> Result<()> {
        if step > 2 {
            return Err(ContractError::Policy("Invalid MCP guide step".into()));
        }
        self.mcp_guide_step = step;
        Ok(())
    }
    pub fn set_language(&mut self, language: &str) -> Result<()> {
        let langs: Value = serde_json::from_str(include_str!("fixtures/languages.json"))?;
        if langs.get(language).is_none() {
            return Err(ContractError::Policy("Unsupported language".into()));
        }
        self.language = Some(language.into());
        Ok(())
    }
}
fn valid_date(s: &str) -> bool {
    chrono::DateTime::parse_from_rfc3339(s).is_ok()
        || chrono::DateTime::parse_from_rfc2822(s).is_ok()
        || chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").is_ok()
}
pub const SESSION_REFRESH_REMINDER_INTERVAL_MS: i64 = 172800000;
pub const SIDEBAR_MIN_WIDTH: u16 = 240;
pub const SIDEBAR_MAX_WIDTH: u16 = 420;
pub fn next_session_refresh_reminder_at(now: f64) -> Result<String> {
    if !now.is_finite() {
        return Err(ContractError::Policy(
            "Session refresh reminder time must be finite".into(),
        ));
    }
    let n = now + SESSION_REFRESH_REMINDER_INTERVAL_MS as f64;
    if n.abs() > 8640000000000000.0 {
        return Err(ContractError::Policy("Invalid time value".into()));
    }
    chrono::DateTime::from_timestamp_millis(n as i64)
        .map(|d| d.to_rfc3339_opts(chrono::SecondsFormat::Millis, true))
        .ok_or_else(|| ContractError::Policy("Invalid time value".into()))
}

/// State persistence mirrors createStateStore: reads repair; update merges without
/// re-normalizing, forces version 1, and advances memory only after atomic save.
pub struct StateStore {
    path: std::path::PathBuf,
    state: LauncherState,
}
impl StateStore {
    pub fn open(path: impl AsRef<std::path::Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let state = std::fs::read(&path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .and_then(|value| LauncherState::normalize(value).ok())
            .unwrap_or_default();
        Ok(Self { path, state })
    }
    pub fn read(&self) -> LauncherState {
        self.state.clone()
    }
    pub fn update(&mut self, patch: Map<String, Value>) -> Result<LauncherState> {
        let mut value = serde_json::to_value(&self.state)?
            .as_object()
            .unwrap()
            .clone();
        value.extend(patch);
        value.insert("version".into(), Value::from(1));
        let next: LauncherState = serde_json::from_value(Value::Object(value))?;
        let mut content = serde_json::to_vec_pretty(&next)?;
        content.push(b'\n');
        write_private_atomic(&self.path, &content)?;
        self.state = next;
        Ok(self.read())
    }
    pub fn set_preference(&mut self, key: &str, value: &Value) -> Result<LauncherState> {
        let mut next = self.read();
        next.set_preference(key, value)?;
        let mut patch = Map::new();
        patch.insert(key.into(), Value::Bool(value == true));
        self.update(patch)
    }
}
pub(crate) fn write_private_atomic(path: &std::path::Path, content: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(std::path::Path::new("."));
    std::fs::create_dir_all(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(parent, std::fs::Permissions::from_mode(0o700));
    }
    let mut file = tempfile::Builder::new()
        .prefix(".state-")
        .tempfile_in(parent)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.as_file()
            .set_permissions(std::fs::Permissions::from_mode(0o600))?;
    }
    file.write_all(content)?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|e| e.error)?;
    Ok(())
}
