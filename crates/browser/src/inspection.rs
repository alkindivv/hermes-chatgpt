use crate::{BrowserEngine, Cancellation, Error, InteractionMode, Result};
use cw_contracts::{
    limits::Plan,
    models::{
        self, BackendModel, ChatGptWebAccountCapabilities, ChatGptWebModelRoute, CodexEffort,
        ModelFamily,
    },
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::time::Duration;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AccountInspection {
    pub account_key: String,
    pub plan_type: String,
    pub personal: bool,
    pub needs_attention: bool,
    pub limits_plan: Plan,
    pub server_authenticated: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AvailableRoute {
    pub route: ChatGptWebModelRoute,
    pub efforts: Vec<CodexEffort>,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapabilityInspection {
    pub account: AccountInspection,
    pub capabilities: ChatGptWebAccountCapabilities,
    pub families: Vec<ModelFamily>,
    pub available_efforts: Vec<bool>,
    pub think_available: bool,
    pub routes: Vec<AvailableRoute>,
}
#[derive(Debug, Clone, Copy)]
pub struct CapabilityOptions {
    pub selector_timeout: Duration,
    pub stable_absence: Duration,
}
impl Default for CapabilityOptions {
    fn default() -> Self {
        Self {
            selector_timeout: Duration::from_secs(30),
            stable_absence: Duration::from_secs(3),
        }
    }
}
/// A request for primary to apply using CefBrowserHost::set_zoom_level; not an applied-state receipt.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct NativeZoomRequest {
    pub factor: f64,
    pub cef_zoom_level: f64,
}
#[derive(Deserialize)]
struct Identity {
    user_id: String,
    account_id: String,
    plan_type: String,
    structure: String,
    needs_attention: bool,
    server_authenticated: bool,
}
fn account(identity: Identity) -> Result<AccountInspection> {
    if !identity.server_authenticated {
        return Err(Error::Unauthenticated);
    }
    if [
        &identity.user_id,
        &identity.account_id,
        &identity.plan_type,
        &identity.structure,
    ]
    .iter()
    .any(|s| s.is_empty() || s.len() > 256)
    {
        return Err(Error::Dom("server account identity unavailable".into()));
    }
    let personal = identity.structure == "personal";
    let limits_plan = match (personal, identity.plan_type.as_str()) {
        (true, "pro") => Plan::Pro200,
        (true, "prolite") => Plan::Pro100,
        _ => Plan::Unsupported,
    };
    if limits_plan != Plan::Unsupported && identity.needs_attention {
        return Err(Error::Dom(
            "subscription requires attention; limits plan not confirmed".into(),
        ));
    }
    let mut hash = Sha256::new();
    hash.update(identity.user_id.as_bytes());
    hash.update([0]);
    hash.update(identity.account_id.as_bytes());
    Ok(AccountInspection {
        account_key: hex::encode(hash.finalize()),
        plan_type: identity.plan_type,
        personal,
        needs_attention: identity.needs_attention,
        limits_plan,
        server_authenticated: true,
    })
}
#[derive(Deserialize)]
struct Probe {
    composer_ready: bool,
    document_ready: bool,
    selector_count: usize,
    think_available: bool,
}
#[derive(Deserialize)]
struct Picker {
    available: Vec<bool>,
    families: Vec<ModelFamily>,
}
fn route_efforts(
    route: &ChatGptWebModelRoute,
    caps: &ChatGptWebAccountCapabilities,
    available: &[bool],
    think: bool,
) -> Vec<CodexEffort> {
    models::chat_gpt_web_route_efforts(route, caps)
        .into_iter()
        .filter(|effort| {
            if route.backend_model == BackendModel::Luna {
                return if route.supported_codex_efforts.is_none() {
                    route.adapter_effort == models::AdapterEffort::Low || think
                } else {
                    *effort == CodexEffort::Low || think
                };
            }
            let adapter = if route.supported_codex_efforts.is_some() {
                serde_json::from_value(json!(effort)).ok()
            } else {
                Some(route.adapter_effort)
            };
            adapter.is_some_and(|e| {
                available.get(crate::selection::effort_index(e)).copied() == Some(true)
            })
        })
        .collect()
}
pub(crate) fn available_routes(
    caps: &ChatGptWebAccountCapabilities,
    families: &[ModelFamily],
    available: &[bool],
    think: bool,
) -> Result<Vec<AvailableRoute>> {
    Ok(models::available_chat_gpt_web_model_routes(caps, true)
        .map_err(|e| Error::Model(e.to_string()))?
        .into_iter()
        .filter(|r| r.model_family.is_none_or(|f| families.contains(&f)))
        .filter_map(|route| {
            let efforts = route_efforts(&route, caps, available, think);
            (!efforts.is_empty()).then_some(AvailableRoute { route, efforts })
        })
        .collect())
}
impl BrowserEngine {
    /// Pure host request; no CDP or DOM operation in either interaction mode.
    pub fn zoom(&self, factor: f64) -> Result<NativeZoomRequest> {
        if !factor.is_finite() || factor <= 0.0 {
            return Err(Error::Dom("zoom factor must be finite and positive".into()));
        }
        Ok(NativeZoomRequest {
            factor,
            cef_zoom_level: factor.ln() / 1.2_f64.ln(),
        })
    }
    /// Explicit automatic-only readiness probe. Manual callers use primary CEF load events.
    pub async fn wait_ready(&self, timeout: Duration, cancellation: Cancellation) -> Result<()> {
        let _state = tokio::select! {_=cancellation.cancelled()=>return Err(Error::Cancelled),state=self.state.lock()=>state};
        self.wait_ready_inner(timeout, &cancellation).await
    }
    pub(crate) async fn wait_ready_inner(
        &self,
        timeout: Duration,
        cancel: &Cancellation,
    ) -> Result<()> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            match tokio::time::timeout_at(deadline, self.page("ready", Value::Null, cancel))
                .await
                .map_err(|_| Error::Timeout)?
            {
                Ok(v) if v == true => return Ok(()),
                Err(Error::Cancelled) => return Err(Error::Cancelled),
                Err(error @ Error::Transport(_)) => return Err(error),
                _ => {}
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(Error::Timeout);
            }
            tokio::select! {_=cancel.cancelled()=>return Err(Error::Cancelled),_=tokio::time::sleep(Duration::from_millis(100))=>{}}
        }
    }
    async fn account_once(&self, cancel: &Cancellation) -> Result<AccountInspection> {
        let value = self.page("account", Value::Null, cancel).await?;
        account(
            serde_json::from_value(value)
                .map_err(|_| Error::Dom("server account identity unavailable".into()))?,
        )
    }
    /// Server account digest and limits plan without changing the model picker.
    pub async fn inspect_account(
        &self,
        mode: InteractionMode,
        cancellation: Cancellation,
    ) -> Result<AccountInspection> {
        if mode == InteractionMode::Manual {
            return Err(Error::ManualDomForbidden);
        }
        let _state = tokio::select! {_=cancellation.cancelled()=>return Err(Error::Cancelled),state=self.state.lock()=>state};
        let before = self.account_once(&cancellation).await?;
        let after = self.account_once(&cancellation).await?;
        if before != after {
            return Err(Error::Dom(
                "account changed during limits inspection".into(),
            ));
        }
        Ok(after)
    }
    /// Manual is rejected before any page operation. No defaults stand in for missing evidence.
    pub async fn inspect_capabilities(
        &self,
        mode: InteractionMode,
        cancellation: Cancellation,
    ) -> Result<CapabilityInspection> {
        self.inspect_capabilities_with_options(mode, CapabilityOptions::default(), cancellation)
            .await
    }
    pub async fn inspect_capabilities_with_options(
        &self,
        mode: InteractionMode,
        options: CapabilityOptions,
        cancellation: Cancellation,
    ) -> Result<CapabilityInspection> {
        if mode == InteractionMode::Manual {
            return Err(Error::ManualDomForbidden);
        }
        if options.stable_absence.is_zero() || options.selector_timeout < options.stable_absence {
            return Err(Error::Dom("capability inspection requires a positive stable-absence window within its selector budget".into()));
        }
        let _state = tokio::select! {_=cancellation.cancelled()=>return Err(Error::Cancelled),state=self.state.lock()=>state};
        self.wait_ready_inner(options.selector_timeout, &cancellation)
            .await?;
        self.inspect_capabilities_inner(options, &cancellation)
            .await
    }
    pub(crate) async fn inspect_capabilities_inner(
        &self,
        options: CapabilityOptions,
        cancel: &Cancellation,
    ) -> Result<CapabilityInspection> {
        let before = self.account_once(cancel).await?;
        let deadline = tokio::time::Instant::now() + options.selector_timeout;
        let mut absence = None;
        let mut presence = 0;
        let (picker, think) = loop {
            let probe: Probe =
                serde_json::from_value(self.page("capabilityState", Value::Null, cancel).await?)
                    .map_err(|e| Error::Dom(e.to_string()))?;
            if probe.selector_count > 1 {
                return Err(Error::Dom("multiple composer model controls".into()));
            }
            if probe.composer_ready && probe.document_ready && probe.selector_count == 1 {
                presence += 1;
                absence = None;
                if presence >= 2 {
                    let picker = serde_json::from_value::<Picker>(
                        self.page("capabilityPicker", Value::Null, cancel).await?,
                    )
                    .map_err(|e| Error::Dom(e.to_string()))?;
                    if picker.available.is_empty() || picker.available.len() > 5 {
                        return Err(Error::Dom("effort availability not proven".into()));
                    }
                    break (Some(picker), probe.think_available);
                }
            } else {
                presence = 0;
                if probe.composer_ready && probe.document_ready && probe.selector_count == 0 {
                    absence.get_or_insert_with(tokio::time::Instant::now);
                } else {
                    absence = None;
                }
            }
            if tokio::time::Instant::now() >= deadline {
                if absence.is_some_and(|since| since.elapsed() >= options.stable_absence) {
                    break (None, probe.think_available);
                }
                return Err(Error::Dom(
                    "capability probe did not reach stable evidence".into(),
                ));
            }
            tokio::select! {_=cancel.cancelled()=>return Err(Error::Cancelled),_=tokio::time::sleep(Duration::from_millis(100))=>{}}
        };
        let after = self.account_once(cancel).await?;
        if before != after {
            return Err(Error::Dom(
                "account changed during capability inspection".into(),
            ));
        }
        let (sol_available, families, available_efforts) = if let Some(picker) = picker {
            (true, picker.families, picker.available)
        } else {
            (false, Vec::new(), Vec::new())
        };
        let caps = ChatGptWebAccountCapabilities {
            sol_available,
            pro_available: available_efforts.get(4) == Some(&true),
            extra_high_available: Some(available_efforts.get(3) == Some(&true)),
            ..Default::default()
        };
        let routes = available_routes(&caps, &families, &available_efforts, think)?;
        Ok(CapabilityInspection {
            account: after,
            capabilities: caps,
            families,
            available_efforts,
            think_available: think,
            routes,
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn identity(plan: &str, structure: &str) -> Identity {
        Identity {
            user_id: "user".into(),
            account_id: "account".into(),
            plan_type: plan.into(),
            structure: structure.into(),
            needs_attention: false,
            server_authenticated: true,
        }
    }
    #[test]
    fn plan_and_digest_are_server_bound() {
        let pro = account(identity("pro", "personal")).unwrap();
        assert_eq!(pro.limits_plan, Plan::Pro200);
        assert_eq!(
            pro.account_key,
            hex::encode(Sha256::digest(b"user\0account"))
        );
        assert_eq!(
            account(identity("prolite", "personal"))
                .unwrap()
                .limits_plan,
            Plan::Pro100
        );
        assert_eq!(
            account(identity("pro", "workspace")).unwrap().limits_plan,
            Plan::Unsupported
        );
        let mut bad = identity("pro", "personal");
        bad.server_authenticated = false;
        assert!(matches!(account(bad), Err(Error::Unauthenticated)));
        let mut bad = identity("pro", "personal");
        bad.needs_attention = true;
        assert!(account(bad).is_err());
    }
}
