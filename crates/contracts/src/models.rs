//! Model routes, context/transport budgets and native catalog policy (upstream MIT).
use crate::{BrowserInteractionMode, ContractError, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
fn error(s: impl Into<String>) -> ContractError {
    ContractError::Policy(s.into())
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CodexEffort {
    Low,
    Medium,
    High,
    Xhigh,
    Max,
    Ultra,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AdapterEffort {
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}
impl AdapterEffort {
    fn codex(self) -> CodexEffort {
        match self {
            Self::Low => CodexEffort::Low,
            Self::Medium => CodexEffort::Medium,
            Self::High => CodexEffort::High,
            Self::Xhigh => CodexEffort::Xhigh,
            Self::Max => CodexEffort::Max,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BackendModel {
    #[serde(rename = "gpt-5.6-sol")]
    Sol,
    #[serde(rename = "gpt-5.6-luna")]
    Luna,
    #[serde(rename = "chatgpt-web-zero-risk")]
    ZeroRisk,
    #[serde(rename = "chatgpt-web-zero-risk-pro")]
    ZeroRiskPro,
}
impl BackendModel {
    pub fn is_zero_risk(self) -> bool {
        matches!(self, Self::ZeroRisk | Self::ZeroRiskPro)
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ModelFamily {
    #[serde(rename = "5.6")]
    Gpt56,
    #[serde(rename = "6")]
    Gpt6,
}
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatGptWebAccountCapabilities {
    pub sol_available: bool,
    pub pro_available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub extra_high_available: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub experimental_bigger_context: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub browser_interaction_mode: Option<BrowserInteractionMode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub zero_risk_pro_enabled: Option<bool>,
}
impl ChatGptWebAccountCapabilities {
    fn bigger(&self) -> bool {
        self.experimental_bigger_context == Some(true)
    }
    fn manual(&self) -> bool {
        self.browser_interaction_mode == Some(BrowserInteractionMode::Manual)
    }
}
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatGptWebModelRoute {
    pub slug: String,
    pub display_name: String,
    pub description: String,
    pub interaction_mode: BrowserInteractionMode,
    pub backend_model: BackendModel,
    pub codex_effort: CodexEffort,
    pub adapter_effort: AdapterEffort,
    pub requires_pro: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_family: Option<ModelFamily>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub requires_extra_high: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub legacy: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supported_codex_efforts: Option<Vec<CodexEffort>>,
}
pub fn all_model_routes() -> Vec<ChatGptWebModelRoute> {
    serde_json::from_str(include_str!("fixtures/routes.json")).expect("pinned routes")
}
pub fn is_chat_gpt_web_model_slug(s: &str) -> bool {
    s.starts_with("chatgpt-web/")
}
pub fn available_chat_gpt_web_model_routes(
    c: &ChatGptWebAccountCapabilities,
    include_legacy: bool,
) -> Result<Vec<ChatGptWebModelRoute>> {
    if c.manual() && c.bigger() {
        return Err(error("Zero Risk does not support Bigger Context"));
    }
    let mut routes: Vec<_> = all_model_routes()
        .into_iter()
        .filter(|r| {
            if c.manual() {
                return r.interaction_mode == BrowserInteractionMode::Manual
                    && (r.backend_model != BackendModel::ZeroRiskPro
                        || c.zero_risk_pro_enabled == Some(true));
            }
            r.interaction_mode == BrowserInteractionMode::Automatic
                && (include_legacy || r.legacy != Some(true))
                && if !c.sol_available {
                    r.backend_model == BackendModel::Luna
                } else {
                    r.backend_model == BackendModel::Sol
                        && (!r.requires_pro || c.pro_available)
                        && (r.requires_extra_high != Some(true)
                            || c.extra_high_available == Some(true))
                }
        })
        .collect();
    // Upstream picker order: current routes followed by legacy routes.
    routes.sort_by_key(|r| r.legacy == Some(true));
    Ok(routes)
}
pub fn chat_gpt_web_route_efforts(
    r: &ChatGptWebModelRoute,
    c: &ChatGptWebAccountCapabilities,
) -> Vec<CodexEffort> {
    r.supported_codex_efforts
        .clone()
        .unwrap_or_else(|| vec![r.codex_effort])
        .into_iter()
        .filter(|e| *e != CodexEffort::Xhigh || c.extra_high_available == Some(true))
        .collect()
}
pub fn require_chat_gpt_web_model_route(
    id: &str,
    c: &ChatGptWebAccountCapabilities,
    reasoning: Option<&str>,
) -> Result<ChatGptWebModelRoute> {
    if c.manual() && c.bigger() {
        return Err(error("Zero Risk does not support Bigger Context"));
    }
    let mut r = all_model_routes()
        .into_iter()
        .find(|r| r.slug == id)
        .ok_or_else(|| error(format!("ChatGPT web model is not enabled: {id}")))?;
    let message = if c.manual() {
        if r.interaction_mode != BrowserInteractionMode::Manual {
            Some("is not available while Zero Risk is enabled")
        } else if r.backend_model == BackendModel::ZeroRiskPro
            && c.zero_risk_pro_enabled != Some(true)
        {
            Some("is not enabled in Zero Risk model settings")
        } else {
            return Ok(r);
        }
    } else if r.interaction_mode == BrowserInteractionMode::Manual {
        Some("is only available while Zero Risk is enabled")
    } else if r.backend_model == BackendModel::Luna && c.sol_available {
        Some("is only available for Luna-only accounts")
    } else if r.backend_model != BackendModel::Luna && !c.sol_available {
        Some("is not available for this Luna-only account")
    } else if (r.requires_pro && !c.pro_available)
        || (r.requires_extra_high == Some(true) && c.extra_high_available != Some(true))
    {
        Some("is not available for this account")
    } else {
        None
    };
    if let Some(m) = message {
        return Err(error(format!("{} {m}", r.display_name)));
    }
    if r.supported_codex_efforts.is_some() {
        if let Some(s) = reasoning {
            let effort: CodexEffort = serde_json::from_value(json!(s)).map_err(|_| {
                error(format!(
                    "{} does not support effort {s:?} for this account",
                    r.display_name
                ))
            })?;
            if !chat_gpt_web_route_efforts(&r, c).contains(&effort) {
                return Err(error(format!(
                    "{} does not support effort {s:?} for this account",
                    r.display_name
                )));
            }
            r.codex_effort = effort;
            r.adapter_effort = serde_json::from_value(json!(s))?;
        }
    }
    Ok(r)
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatGptWebContextLimits {
    pub context_window: u64,
    pub effective_context_window_percent: u64,
    pub auto_compact_token_limit: u64,
}
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatGptWebTransportLimits {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub browser_message_token_limit: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub browser_composer_char_limit: Option<u64>,
}
fn context(w: u64, a: u64) -> ChatGptWebContextLimits {
    ChatGptWebContextLimits {
        context_window: w,
        effective_context_window_percent: ((a as f64 / w as f64) * 100.0).round() as u64,
        auto_compact_token_limit: a,
    }
}
pub fn supports_chat_gpt_web_bigger_context(
    b: BackendModel,
    e: AdapterEffort,
    f: Option<ModelFamily>,
) -> bool {
    if b == BackendModel::Luna {
        matches!(e, AdapterEffort::Low | AdapterEffort::Medium)
    } else {
        b == BackendModel::Sol && (f != Some(ModelFamily::Gpt6) || e != AdapterEffort::Low)
    }
}
pub fn resolve_chat_gpt_web_context_limits(
    b: BackendModel,
    e: AdapterEffort,
    c: &ChatGptWebAccountCapabilities,
    f: Option<ModelFamily>,
) -> Result<ChatGptWebContextLimits> {
    if b.is_zero_risk() {
        if c.bigger() {
            return Err(error("Zero Risk does not support Bigger Context"));
        }
        return Ok(if b == BackendModel::ZeroRiskPro {
            context(336579, 285000)
        } else {
            context(123000, 96000)
        });
    }
    if b == BackendModel::Luna {
        return Ok(if c.bigger() {
            context(84000, 59424)
        } else {
            context(1050000, 1050000)
        });
    }
    let l = if c.pro_available {
        context(
            if e == AdapterEffort::Max {
                112193
            } else {
                111193
            },
            95000,
        )
    } else {
        match e {
            AdapterEffort::Low => context(41000, 32000),
            AdapterEffort::Medium | AdapterEffort::High => context(90000, 80000),
            AdapterEffort::Xhigh if c.extra_high_available == Some(true) => context(90000, 80000),
            _ => {
                return Err(error(format!(
                    "ChatGPT Plus context limit is not defined for unavailable effort: {}",
                    effort_name(e.codex())
                )));
            }
        }
    };
    if !c.bigger() || !supports_chat_gpt_web_bigger_context(b, e, f) {
        return Ok(l);
    }
    Ok(if f == Some(ModelFamily::Gpt6) && e != AdapterEffort::Max {
        if c.pro_available {
            context(240000, 220000)
        } else {
            context(120000, 110000)
        }
    } else {
        context(l.context_window * 3, l.auto_compact_token_limit * 3)
    })
}
pub fn resolve_chat_gpt_web_transport_limits(
    b: BackendModel,
    e: AdapterEffort,
    c: &ChatGptWebAccountCapabilities,
) -> Result<ChatGptWebTransportLimits> {
    let (tokens, chars) = if b.is_zero_risk() {
        (None, None)
    } else if b == BackendModel::Luna {
        (Some(19808), None)
    } else if !c.pro_available {
        match e {
            AdapterEffort::Low => (None, Some(211256)),
            AdapterEffort::Medium | AdapterEffort::High => (None, Some(500000)),
            AdapterEffort::Xhigh if c.extra_high_available == Some(true) => (None, Some(500000)),
            _ => {
                return Err(error(format!(
                    "ChatGPT Plus transport limit is not defined for unavailable effort: {}",
                    effort_name(e.codex())
                )));
            }
        }
    } else {
        match e {
            AdapterEffort::Low => (Some(103000), Some(545000)),
            AdapterEffort::Max => (Some(104000), Some(1635000)),
            _ => (Some(103000), Some(500000)),
        }
    };
    Ok(ChatGptWebTransportLimits {
        browser_message_token_limit: tokens,
        browser_composer_char_limit: chars,
    })
}
pub fn chat_gpt_web_image_token_reserve(detail: Option<&str>) -> u64 {
    if detail == Some("original") {
        8192
    } else {
        4096
    }
}
pub fn resolve_chat_gpt_web_message_token_budget(
    b: BackendModel,
    e: AdapterEffort,
    c: &ChatGptWebAccountCapabilities,
    image_tokens: u64,
) -> Result<u64> {
    if b.is_zero_risk() {
        return Err(error("Zero Risk has no automatic message budget"));
    }
    if b == BackendModel::Luna {
        return Ok(19808_u64.saturating_sub(image_tokens));
    }
    let mut c = c.clone();
    c.experimental_bigger_context = Some(false);
    let w = resolve_chat_gpt_web_context_limits(b, e, &c, None)?.context_window;
    Ok(w.saturating_sub(8193).saturating_sub(image_tokens).min(
        resolve_chat_gpt_web_transport_limits(b, e, &c)?
            .browser_message_token_limit
            .unwrap_or(u64::MAX),
    ))
}
pub fn resolve_chat_gpt_web_staging_token_budget(
    b: BackendModel,
    e: AdapterEffort,
    c: &ChatGptWebAccountCapabilities,
) -> Result<u64> {
    let n = resolve_chat_gpt_web_message_token_budget(b, e, c, 0)?;
    if b == BackendModel::Luna || e != AdapterEffort::Low || c.pro_available {
        return Ok(n);
    }
    Ok(n.min(23807))
}
fn effort_name(e: CodexEffort) -> String {
    serde_json::to_value(e).unwrap().as_str().unwrap().into()
}
/// Catalog snake_case fields are upstream wire metadata, deliberately preserved.
pub fn build_chat_gpt_web_model(
    template: &Value,
    r: &ChatGptWebModelRoute,
    c: &ChatGptWebAccountCapabilities,
    compatibility_v1: bool,
) -> Result<Value> {
    let t = template
        .as_object()
        .ok_or_else(|| error("native Codex model template must be a JSON object"))?;
    let slug = t
        .get("slug")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty() && !is_chat_gpt_web_model_slug(s));
    if slug.is_none() {
        return Err(error(
            "ChatGPT Web model template must be a native Codex model",
        ));
    }
    let limits =
        resolve_chat_gpt_web_context_limits(r.backend_model, r.adapter_effort, c, r.model_family)?;
    let efforts = chat_gpt_web_route_efforts(r, c);
    for e in &efforts {
        let a = if r.supported_codex_efforts.is_some() {
            serde_json::from_value(json!(e)).map_err(|_| error("Ultra is not a browser effort"))?
        } else {
            r.adapter_effort
        };
        if resolve_chat_gpt_web_context_limits(r.backend_model, a, c, r.model_family)? != limits {
            return Err(error(format!(
                "Cannot group different context budgets under {}",
                r.slug
            )));
        }
    }
    let mut m = t.clone();
    if let Some(p) = t.get("priority") {
        let n = p
            .as_f64()
            .filter(|n| n.fract() == 0.0 && n.abs() <= 9007199254740991.0)
            .ok_or_else(|| error("Native Codex model template priority must be an integer"))?;
        let bump = compatibility_v1
            && matches!(
                r.slug.as_str(),
                "chatgpt-web/light"
                    | "chatgpt-web/gpt-5.6-sol-instant"
                    | "chatgpt-web/gpt-6-sol-instant"
            );
        if bump && n == 9007199254740991.0 {
            return Err(error(
                "Native Codex model template priority cannot reserve the Compatibility V1 roster",
            ));
        }
        m.insert("priority".into(), json!(n as i64 + i64::from(bump)));
    }
    if compatibility_v1 {
        m.insert("multi_agent_version".into(), json!("v1"));
    }
    let levels = efforts
        .iter()
        .map(|e| {
            let mut level = t
                .get("supported_reasoning_levels")
                .and_then(Value::as_array)
                .and_then(|a| a.iter().find(|v| v.is_object() && v["effort"] == json!(e)))
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            let name = effort_name(*e);
            let desc = if efforts.len() == 1 {
                r.display_name.clone()
            } else if r.backend_model == BackendModel::Luna {
                if *e == CodexEffort::Low {
                    "Ordinary Luna".into()
                } else {
                    "Think".into()
                }
            } else {
                format!(
                    "{} — {}",
                    r.display_name,
                    if *e == CodexEffort::Xhigh {
                        "Extra High"
                    } else {
                        &name
                    }
                )
            };
            level.insert("effort".into(), json!(e));
            level.insert("description".into(), json!(desc));
            Value::Object(level)
        })
        .collect::<Vec<_>>();
    let overrides = json!({"slug":r.slug,"display_name":r.display_name,"description":r.description,"input_modalities":if r.interaction_mode==BrowserInteractionMode::Manual {vec!["text"]}else {vec!["text","image"]},"visibility":if r.legacy==Some(true){"hide"}else {"list"},"supported_in_api":true,"tool_mode":null,"upgrade":null,"default_reasoning_level":r.codex_effort,"supported_reasoning_levels":levels,"context_window":limits.context_window,"max_context_window":limits.context_window,"effective_context_window_percent":limits.effective_context_window_percent,"auto_compact_token_limit":limits.auto_compact_token_limit,"additional_speed_tiers":[],"service_tiers":[],"default_service_tier":null});
    m.extend(overrides.as_object().unwrap().clone());
    m.remove("comp_hash");
    m.remove("availability_nux");
    Ok(Value::Object(m))
}
pub fn augment_native_model_catalog(
    value: &Value,
    c: &ChatGptWebAccountCapabilities,
    full_mode: bool,
    compatibility_v1: bool,
    context_override: Option<u64>,
) -> Result<Value> {
    let mut catalog = value
        .as_object()
        .cloned()
        .ok_or_else(|| error("native Codex models response must be a JSON object"))?;
    let rows = catalog
        .get("models")
        .and_then(Value::as_array)
        .ok_or_else(|| error("Native Codex models response is missing a models array"))?;
    let mut native: Vec<Value> = rows
        .iter()
        .filter(|v| {
            !v.get("slug")
                .and_then(Value::as_str)
                .is_some_and(is_chat_gpt_web_model_slug)
        })
        .cloned()
        .collect();
    if compatibility_v1 {
        for v in &mut native {
            if let Some(m) = v.as_object_mut() {
                if m.get("multi_agent_version") != Some(&json!("disabled")) {
                    m.insert("multi_agent_version".into(), json!("v1"));
                }
            }
        }
    }
    let template=native.iter().find(|v|v["slug"].as_str().is_some_and(|s|!s.is_empty())&&v["visibility"]=="list"&&v["supported_reasoning_levels"].is_array()&&(!full_mode||v["tool_mode"].as_str().is_some_and(|s|!s.is_empty()))).cloned().ok_or_else(||error(if full_mode {"Native Codex models response has no list-visible, tool-capable model with reasoning metadata"}else {"Native Codex models response has no list-visible model with reasoning metadata"}))?;
    if let Some(w) = context_override {
        for v in &mut native {
            if v.get("slug")
                .and_then(Value::as_str)
                .is_none_or(|s| s.is_empty())
            {
                continue;
            }
            let current = &v["max_context_window"];
            if !current.is_null()
                && !current
                    .as_f64()
                    .is_some_and(|n| n > 0.0 && n.fract() == 0.0 && n <= 9007199254740991.0)
            {
                return Err(error(format!(
                    "Native {} max_context_window must be a positive integer",
                    v["slug"].as_str().unwrap()
                )));
            }
            if current.is_null() || current.as_f64().unwrap() < w as f64 {
                v["max_context_window"] = json!(w);
            }
        }
    }
    for r in available_chat_gpt_web_model_routes(c, true)? {
        native.push(build_chat_gpt_web_model(
            &template,
            &r,
            c,
            compatibility_v1,
        )?);
    }
    catalog.insert("models".into(), json!(native));
    Ok(Value::Object(catalog))
}

// Pinned upstream exported constants.
pub const CHATGPT_WEB_MODEL_PREFIX: &str = "chatgpt-web/";
pub const CHATGPT_WEB_BACKEND_MODEL: &str = "gpt-5.6-sol";
pub const CHATGPT_WEB_LUNA_BACKEND_MODEL: &str = "gpt-5.6-luna";
pub const CHATGPT_WEB_GPT6_INSTANT_BIGGER_CONTEXT_ERROR: &str = "GPT-6 Instant uses standard context. Select Medium, High or an available Extra High or Pro mode to use Bigger Context.";
pub const CHATGPT_WEB_ZERO_RISK_BACKEND_MODEL: &str = "chatgpt-web-zero-risk";
pub const CHATGPT_WEB_ZERO_RISK_PRO_BACKEND_MODEL: &str = "chatgpt-web-zero-risk-pro";
pub const CHATGPT_WEB_INSTANT_CONTEXT_WINDOW: u64 = 41_000;
pub const CHATGPT_WEB_INSTANT_AUTO_COMPACT_TOKEN_LIMIT: u64 = 32_000;
pub const CHATGPT_WEB_ZERO_RISK_CONTEXT_WINDOW: u64 = CHATGPT_WEB_INSTANT_CONTEXT_WINDOW * 3;
pub const CHATGPT_WEB_ZERO_RISK_AUTO_COMPACT_TOKEN_LIMIT: u64 =
    CHATGPT_WEB_INSTANT_AUTO_COMPACT_TOKEN_LIMIT * 3;
pub const CHATGPT_WEB_MEDIUM_HIGH_CONTEXT_WINDOW: u64 = 90_000;
pub const CHATGPT_WEB_MEDIUM_HIGH_AUTO_COMPACT_TOKEN_LIMIT: u64 = 80_000;
pub const CHATGPT_WEB_INSTANT_COMPOSER_CHAR_LIMIT: u64 = 211_256;
pub const CHATGPT_WEB_MEDIUM_HIGH_COMPOSER_CHAR_LIMIT: u64 = 500_000;
pub const CHATGPT_WEB_PLATFORM_RESERVE_TOKENS: u64 = 8_192;
pub const CHATGPT_WEB_PRO_AUTO_COMPACT_TOKEN_LIMIT: u64 = 95_000;
pub const CHATGPT_WEB_PRO_STANDARD_MESSAGE_TOKEN_LIMIT: u64 = 103_000;
pub const CHATGPT_WEB_PRO_MODEL_MESSAGE_TOKEN_LIMIT: u64 = 104_000;
pub const CHATGPT_WEB_PRO_STANDARD_CONTEXT_WINDOW: u64 =
    CHATGPT_WEB_PRO_STANDARD_MESSAGE_TOKEN_LIMIT + CHATGPT_WEB_PLATFORM_RESERVE_TOKENS + 1;
pub const CHATGPT_WEB_PRO_MODEL_CONTEXT_WINDOW: u64 =
    CHATGPT_WEB_PRO_MODEL_MESSAGE_TOKEN_LIMIT + CHATGPT_WEB_PLATFORM_RESERVE_TOKENS + 1;
pub const CHATGPT_WEB_ZERO_RISK_PRO_CONTEXT_WINDOW: u64 = CHATGPT_WEB_PRO_MODEL_CONTEXT_WINDOW * 3;
pub const CHATGPT_WEB_ZERO_RISK_PRO_AUTO_COMPACT_TOKEN_LIMIT: u64 =
    CHATGPT_WEB_PRO_AUTO_COMPACT_TOKEN_LIMIT * 3;
pub const CHATGPT_WEB_PRO_INSTANT_COMPOSER_CHAR_LIMIT: u64 = 545_000;
pub const CHATGPT_WEB_PRO_REASONING_COMPOSER_CHAR_LIMIT: u64 =
    CHATGPT_WEB_MEDIUM_HIGH_COMPOSER_CHAR_LIMIT;
pub const CHATGPT_WEB_PRO_MODEL_COMPOSER_CHAR_LIMIT: u64 = 1_635_000;
pub const CHATGPT_WEB_LUNA_CONTEXT_WINDOW: u64 = 1_050_000;
pub const CHATGPT_LUNA_BROWSER_INPUT_TOKEN_BUDGET: u64 = 28_000;
pub const CHATGPT_WEB_BIGGER_CONTEXT_MULTIPLIER: u64 = 3;
pub const CHATGPT_WEB_LUNA_BIGGER_CONTEXT_WINDOW: u64 =
    CHATGPT_LUNA_BROWSER_INPUT_TOKEN_BUDGET * CHATGPT_WEB_BIGGER_CONTEXT_MULTIPLIER;
pub const CHATGPT_WEB_LUNA_BIGGER_AUTO_COMPACT_TOKEN_LIMIT: u64 =
    (CHATGPT_LUNA_BROWSER_INPUT_TOKEN_BUDGET - CHATGPT_WEB_PLATFORM_RESERVE_TOKENS)
        * CHATGPT_WEB_BIGGER_CONTEXT_MULTIPLIER;
pub const CHATGPT_WEB_GPT6_SOL_BIGGER_CONTEXT_WINDOW: u64 = 240_000;
pub const CHATGPT_WEB_GPT6_SOL_BIGGER_AUTO_COMPACT_TOKEN_LIMIT: u64 = 220_000;
pub const CHATGPT_WEB_GPT6_PLUS_BIGGER_CONTEXT_WINDOW: u64 = 120_000;
pub const CHATGPT_WEB_GPT6_PLUS_BIGGER_AUTO_COMPACT_TOKEN_LIMIT: u64 = 110_000;

pub type ChatGptWebCodexEffort = CodexEffort;
pub type ChatGptWebAdapterEffort = AdapterEffort;
pub type ChatGptWebBackendModel = BackendModel;
pub type ChatGptWebModelFamily = ModelFamily;
