//! Local receipt estimates, not account availability or server reset times.
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashSet},
    path::{Path, PathBuf},
};
pub const DAY_MS: u64 = 86400000;
pub const RETENTION_MS: u64 = 7 * DAY_MS;
pub const SOURCE_DATE: &str = "2026-09-19";
pub const SOURCE_URL: &str =
    "https://help.openai.com/en/articles/20001354-gpt-56-and-gpt-6-pro-in-chatgpt";
#[derive(Debug, Clone, Copy)]
pub struct StoreBounds {
    pub max_accounts: usize,
    pub max_events: usize,
    pub max_id_bytes: usize,
    pub max_file_bytes: usize,
}
pub const STORE_BOUNDS: StoreBounds = StoreBounds {
    max_accounts: 64,
    max_events: 20_000,
    max_id_bytes: 256,
    max_file_bytes: 16 * 1024 * 1024,
};
/// Complete pinned policy, including Business references not enabled by LimitsStore.
pub fn reference_policy() -> serde_json::Value {
    serde_json::from_str(include_str!("fixtures/limits-policy.json")).expect("pinned limits policy")
}
pub fn official_limits(plan: Plan) -> Vec<LimitsWindow> {
    let key = "0".repeat(64);
    let state = State {
        version: 1,
        active_account_key: Some(key.clone()),
        accounts: BTreeMap::from([(
            key,
            Account {
                plan,
                initialized_at: Some(0),
                checked_at: 0,
                events: vec![],
            },
        )]),
    };
    state.snapshot(0).windows
}
const SAFE: u64 = 9007199254740991;
#[derive(Debug, thiserror::Error)]
#[error("{code}: {message}")]
pub struct LimitsStoreError {
    pub code: &'static str,
    pub message: String,
}
type Result<T> = std::result::Result<T, LimitsStoreError>;
fn err(code: &'static str, message: impl Into<String>) -> LimitsStoreError {
    LimitsStoreError {
        code,
        message: message.into(),
    }
}
fn corrupt() -> LimitsStoreError {
    err(
        "LIMITS_CORRUPT_STATE",
        "Invalid limits store data; the file was not changed.",
    )
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Plan {
    #[serde(rename = "pro_100")]
    Pro100,
    #[serde(rename = "pro_200")]
    Pro200,
    #[serde(rename = "unsupported")]
    Unsupported,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReceiptModel {
    #[serde(rename = "gpt-6-pro")]
    Gpt6Pro,
    #[serde(rename = "gpt-5.6-pro")]
    Gpt56Pro,
    #[serde(rename = "pro-unknown")]
    ProUnknown,
    #[serde(rename = "other")]
    Other,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Event {
    id: String,
    model: ReceiptModel,
    at: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Account {
    plan: Plan,
    initialized_at: Option<u64>,
    checked_at: u64,
    events: Vec<Event>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct State {
    version: u8,
    active_account_key: Option<String>,
    accounts: BTreeMap<String, Account>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LimitsWindow {
    pub id: String,
    pub label: String,
    pub model: String,
    pub duration_ms: u64,
    pub limit: u64,
    pub used: usize,
    pub uncertain_used: usize,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LimitsSnapshot {
    pub enabled: bool,
    pub plan: Option<Plan>,
    pub tracking_since: Option<u64>,
    pub checked_at: Option<u64>,
    pub total_messages: usize,
    pub unknown_pro_messages: usize,
    pub incomplete: bool,
    pub windows: Vec<LimitsWindow>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disabled_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}
fn account_key(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())
}
fn clock(now: u64) -> Result<()> {
    if now > SAFE {
        Err(err(
            "LIMITS_INVALID_CLOCK",
            "The limits clock must return a nonnegative safe integer timestamp.",
        ))
    } else {
        Ok(())
    }
}
fn recent(at: u64, now: u64, duration: u64) -> bool {
    at as i128 > now as i128 - duration as i128
}
impl State {
    fn validate(&self) -> Result<()> {
        if self.accounts.len() > 64 {
            return Err(corrupt());
        }
        if let Some(k) = &self.active_account_key {
            if !account_key(k) || k != &k.to_lowercase() || !self.accounts.contains_key(k) {
                return Err(corrupt());
            }
        } else if !self.accounts.is_empty() {
            return Err(corrupt());
        }
        let mut count = 0;
        for (key, a) in &self.accounts {
            if !account_key(key)
                || key != &key.to_lowercase()
                || a.checked_at > SAFE
                || a.initialized_at.is_some_and(|t| t > SAFE)
                || a.initialized_at.is_none()
                    && (a.plan != Plan::Unsupported || !a.events.is_empty())
            {
                return Err(corrupt());
            }
            count += a.events.len();
            if count > 20000 {
                return Err(corrupt());
            }
            let mut ids = HashSet::new();
            for e in &a.events {
                if e.id.is_empty()
                    || e.id.len() > 256
                    || e.at > SAFE
                    || e.at < a.initialized_at.unwrap_or(0)
                    || !ids.insert(&e.id)
                {
                    return Err(corrupt());
                }
            }
        }
        Ok(())
    }
    fn retain(&mut self, now: u64) {
        for a in self.accounts.values_mut() {
            a.events.retain(|e| recent(e.at, now, RETENTION_MS));
        }
    }
    fn snapshot(&self, now: u64) -> LimitsSnapshot {
        let a = self
            .active_account_key
            .as_ref()
            .and_then(|k| self.accounts.get(k));
        let events: Vec<_> = a
            .map(|a| {
                a.events
                    .iter()
                    .filter(|e| recent(e.at, now, RETENTION_MS) && e.at <= now)
                    .collect()
            })
            .unwrap_or_default();
        let unknown = events
            .iter()
            .filter(|e| e.model == ReceiptModel::ProUnknown)
            .count();
        let specs = match a.map(|a| a.plan) {
            Some(Plan::Pro100) => vec![(
                "shared-7d",
                "Both Pro models · rolling last 7 days",
                "shared",
                RETENTION_MS,
                50,
            )],
            Some(Plan::Pro200) => vec![
                (
                    "gpt-6-pro-7d",
                    "GPT-6 Pro · rolling last 7 days",
                    "gpt-6-pro",
                    RETENTION_MS,
                    200,
                ),
                (
                    "gpt-5.6-pro-24h",
                    "GPT-5.6 Pro · rolling last 24 hours",
                    "gpt-5.6-pro",
                    DAY_MS,
                    170,
                ),
                (
                    "shared-24h",
                    "Both Pro models · rolling last 24 hours",
                    "shared",
                    DAY_MS,
                    200,
                ),
            ],
            _ => vec![],
        };
        let windows = specs
            .into_iter()
            .map(|(id, label, model, duration_ms, limit)| {
                let recent: Vec<_> = events
                    .iter()
                    .filter(|e| recent(e.at, now, duration_ms) && e.model != ReceiptModel::Other)
                    .collect();
                let used = recent
                    .iter()
                    .filter(|e| {
                        model == "shared"
                            || model == "gpt-6-pro" && e.model == ReceiptModel::Gpt6Pro
                            || model == "gpt-5.6-pro" && e.model == ReceiptModel::Gpt56Pro
                    })
                    .count();
                LimitsWindow {
                    id: id.into(),
                    label: label.into(),
                    model: model.into(),
                    duration_ms,
                    limit,
                    used,
                    uncertain_used: recent
                        .iter()
                        .filter(|e| e.model == ReceiptModel::ProUnknown)
                        .count(),
                }
            })
            .collect();
        LimitsSnapshot {
            enabled: a.is_some_and(|a| a.plan != Plan::Unsupported),
            plan: a.map(|a| a.plan),
            tracking_since: a.and_then(|a| a.initialized_at),
            checked_at: a.map(|a| a.checked_at),
            total_messages: events.len(),
            unknown_pro_messages: unknown,
            incomplete: unknown > 0,
            windows,
            disabled_reason: None,
            error: None,
        }
    }
}
/// One owner per file. Time is explicit so tests never touch live clocks or profiles.
pub struct LimitsStore {
    path: PathBuf,
    state: State,
}
impl LimitsStore {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        if path.as_os_str().is_empty() {
            return Err(err("LIMITS_INVALID_CONFIG", "A file path is required."));
        }
        let state = match std::fs::symlink_metadata(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => State {
                version: 1,
                active_account_key: None,
                accounts: BTreeMap::new(),
            },
            Err(_) => {
                return Err(err(
                    "LIMITS_READ_FAILED",
                    "Could not read the limits store.",
                ));
            }
            Ok(m) => {
                if !m.is_file() || m.len() > 16 * 1024 * 1024 {
                    return Err(corrupt());
                }
                let content = std::fs::read(&path)
                    .map_err(|_| err("LIMITS_READ_FAILED", "Could not read the limits store."))?;
                let v: serde_json::Value =
                    serde_json::from_slice(&content).map_err(|_| corrupt())?;
                if v.as_object().is_none_or(|m| !m.contains_key("version")) {
                    return Err(corrupt());
                }
                if v["version"] != 1 {
                    return Err(err(
                        "LIMITS_UNSUPPORTED_VERSION",
                        "Unsupported limits store version; the file was not changed.",
                    ));
                }
                let state: State = serde_json::from_value(v).map_err(|_| corrupt())?;
                state.validate()?;
                state
            }
        };
        Ok(Self { path, state })
    }
    fn commit(&mut self, next: State) -> Result<()> {
        let mut bytes = serde_json::to_vec(&next).map_err(|_| corrupt())?;
        bytes.push(b'\n');
        if bytes.len() > 16 * 1024 * 1024 {
            return Err(err(
                "LIMITS_CAPACITY_EXCEEDED",
                "Limits store file capacity reached; no history was discarded.",
            ));
        }
        if serde_json::to_value(&next).unwrap() == serde_json::to_value(&self.state).unwrap() {
            return Ok(());
        }
        crate::state::write_private_atomic(&self.path, &bytes)
            .map_err(|_| err("LIMITS_WRITE_FAILED", "Could not save the limits store."))?;
        self.state = next;
        Ok(())
    }
    pub fn matches_account(&self, key: &str) -> bool {
        account_key(key)
            && self.state.active_account_key.as_deref() == Some(key.to_lowercase().as_str())
    }
    pub fn configure(&mut self, key: &str, plan: Plan, now: u64) -> Result<LimitsSnapshot> {
        if !account_key(key) {
            return Err(err(
                "LIMITS_INVALID_CONFIG",
                "Expected a 64-hex accountKey and plan pro_100, pro_200, or unsupported.",
            ));
        }
        clock(now)?;
        let key = key.to_lowercase();
        let mut next = self.state.clone();
        next.retain(now);
        if !next.accounts.contains_key(&key) && next.accounts.len() >= 64 {
            return Err(err(
                "LIMITS_CAPACITY_EXCEEDED",
                "Limits account capacity reached; no accounts were discarded.",
            ));
        }
        let a = next.accounts.entry(key.clone()).or_insert(Account {
            plan,
            initialized_at: None,
            checked_at: now,
            events: vec![],
        });
        a.plan = plan;
        a.checked_at = now;
        if a.initialized_at.is_none() && plan != Plan::Unsupported {
            a.initialized_at = Some(now)
        }
        next.active_account_key = Some(key);
        self.commit(next)?;
        Ok(self.state.snapshot(now))
    }
    pub fn record(
        &mut self,
        key: &str,
        id: &str,
        model: ReceiptModel,
        at: u64,
        now: u64,
    ) -> Result<bool> {
        clock(now)?;
        if !account_key(key) || id.is_empty() || id.len() > 256 || at > SAFE {
            return Err(err(
                "LIMITS_INVALID_EVENT",
                "Expected a bounded receipt id, 64-hex accountKey, supported model, and nonnegative integer timestamp.",
            ));
        }
        if at > now {
            return Err(err(
                "LIMITS_CLOCK_SKEW",
                "Receipt time is ahead of the local clock; the message was not recorded.",
            ));
        }
        let key = key.to_lowercase();
        let mut next = self.state.clone();
        next.retain(now);
        let mut accepted = false;
        if next.active_account_key.as_ref() == Some(&key) {
            let a = &next.accounts[&key];
            if a.plan != Plan::Unsupported && now < a.initialized_at.unwrap_or(0) {
                return Err(err(
                    "LIMITS_CLOCK_SKEW",
                    "The local clock is before tracking began; the message was not recorded.",
                ));
            }
            accepted = a.plan != Plan::Unsupported
                && at >= a.initialized_at.unwrap_or(0)
                && recent(at, now, RETENTION_MS)
                && !a.events.iter().any(|e| e.id == id);
        }
        if accepted {
            if next
                .accounts
                .values()
                .map(|a| a.events.len())
                .sum::<usize>()
                >= 20000
            {
                return Err(err(
                    "LIMITS_CAPACITY_EXCEEDED",
                    "Limits receipt capacity reached; no recent history was discarded.",
                ));
            }
            next.accounts.get_mut(&key).unwrap().events.push(Event {
                id: id.into(),
                model,
                at,
            });
        }
        self.commit(next)?;
        Ok(accepted)
    }
    pub fn snapshot(&mut self, now: u64) -> Result<LimitsSnapshot> {
        clock(now)?;
        let mut next = self.state.clone();
        next.retain(now);
        self.commit(next)?;
        Ok(self.state.snapshot(now))
    }
}
