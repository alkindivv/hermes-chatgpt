use crate::{BrowserEngine, Cancellation, CapabilityInspection, Error, Result};
use cw_contracts::models::{self, AdapterEffort, BackendModel, ChatGptWebModelRoute};
use serde_json::{Value, json};
use std::time::Duration;
pub(crate) fn effort_index(e: AdapterEffort) -> usize {
    match e {
        AdapterEffort::Low => 0,
        AdapterEffort::Medium => 1,
        AdapterEffort::High => 2,
        AdapterEffort::Xhigh => 3,
        AdapterEffort::Max => 4,
    }
}
pub(crate) fn resolve_route(
    model: &str,
    effort: &str,
    inspection: &CapabilityInspection,
) -> Result<ChatGptWebModelRoute> {
    let route =
        models::require_chat_gpt_web_model_route(model, &inspection.capabilities, Some(effort))
            .map_err(|e| Error::Model(e.to_string()))?;
    if !inspection.routes.iter().any(|available| {
        available.route.slug == route.slug && available.efforts.contains(&route.codex_effort)
    }) {
        return Err(Error::Model(format!(
            "{model}/{effort} is not available in the authenticated picker"
        )));
    }
    Ok(route)
}
impl BrowserEngine {
    pub(crate) async fn select_route(
        &self,
        route: &ChatGptWebModelRoute,
        cancel: &Cancellation,
    ) -> Result<()> {
        if route.backend_model == BackendModel::Luna {
            let enabled = route.adapter_effort == AdapterEffort::Medium;
            let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
            loop {
                if self
                    .page("think", json!({"enabled":enabled}), cancel)
                    .await?
                    == true
                {
                    return Ok(());
                }
                if tokio::time::Instant::now() >= deadline {
                    return Err(Error::Dom("Think state did not settle".into()));
                }
                tokio::select! {_=cancel.cancelled()=>return Err(Error::Cancelled),_=tokio::time::sleep(Duration::from_millis(100))=>{}}
            }
        }
        let index = effort_index(route.adapter_effort);
        let model_arg = json!({"family":route.model_family,"index":index});
        let slider = self.page("model", model_arg.clone(), cancel).await?;
        let mut current = slider["value"]
            .as_i64()
            .ok_or_else(|| Error::Dom("invalid effort slider".into()))?;
        let min = slider["min"]
            .as_i64()
            .ok_or_else(|| Error::Dom("missing effort minimum".into()))?;
        let target = min + index as i64;
        for _ in 0..(current - target).unsigned_abs().min(5) {
            let direction = if current < target { 1 } else { -1 };
            self.key(
                if direction > 0 {
                    "ArrowRight"
                } else {
                    "ArrowLeft"
                },
                cancel,
            )
            .await?;
            let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
            loop {
                let next = self.page("effortState", Value::Null, cancel).await?;
                if next["min"].as_i64() != Some(min) {
                    return Err(Error::Dom("effort range origin changed".into()));
                }
                let value = next["value"]
                    .as_i64()
                    .ok_or_else(|| Error::Dom("invalid effort position".into()))?;
                if value != current {
                    if value != current + direction {
                        return Err(Error::Dom("effort did not move exactly one step".into()));
                    }
                    current = value;
                    break;
                }
                if tokio::time::Instant::now() >= deadline {
                    return Err(Error::Dom("effort key did not take effect".into()));
                }
                tokio::select! {_=cancel.cancelled()=>return Err(Error::Cancelled),_=tokio::time::sleep(Duration::from_millis(50))=>{}}
            }
        }
        tokio::select! {_=cancel.cancelled()=>return Err(Error::Cancelled),_=tokio::time::sleep(Duration::from_millis(250))=>{}}
        self.page("verifyModel", model_arg, cancel).await?;
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{AccountInspection, inspection::available_routes};
    use cw_contracts::{
        limits::Plan,
        models::{ChatGptWebAccountCapabilities, ModelFamily},
    };
    fn inspection(sol: bool, think: bool) -> CapabilityInspection {
        let capabilities = ChatGptWebAccountCapabilities {
            sol_available: sol,
            pro_available: sol,
            extra_high_available: Some(sol),
            ..Default::default()
        };
        let families = if sol {
            vec![ModelFamily::Gpt6, ModelFamily::Gpt56]
        } else {
            vec![]
        };
        let available_efforts = if sol { vec![true; 5] } else { vec![] };
        let routes = available_routes(&capabilities, &families, &available_efforts, think).unwrap();
        CapabilityInspection {
            account: AccountInspection {
                account_key: "0".repeat(64),
                plan_type: "test".into(),
                personal: true,
                needs_attention: false,
                limits_plan: Plan::Unsupported,
                server_authenticated: true,
            },
            capabilities,
            families,
            available_efforts,
            think_available: think,
            routes,
        }
    }
    #[test]
    fn every_automatic_contract_identity_resolves_without_alias_invention() {
        for route in models::all_model_routes()
            .into_iter()
            .filter(|r| !r.backend_model.is_zero_risk())
        {
            let inspection = inspection(route.backend_model == BackendModel::Sol, true);
            for effort in models::chat_gpt_web_route_efforts(&route, &inspection.capabilities) {
                let effort = serde_json::to_value(effort).unwrap();
                let resolved =
                    resolve_route(&route.slug, effort.as_str().unwrap(), &inspection).unwrap();
                assert_eq!(resolved.backend_model, route.backend_model);
                assert_eq!(resolved.model_family, route.model_family);
            }
        }
        assert!(resolve_route("gpt-6", "high", &inspection(true, true)).is_err());
        assert!(resolve_route("chatgpt-web/gpt-6-pro", "high", &inspection(true, true)).is_err());
        assert!(resolve_route("chatgpt-web/gpt-6-sol", "high", &inspection(false, true)).is_err());
        assert!(resolve_route("chatgpt-web/gpt-5.6-luna", "low", &inspection(true, true)).is_err());
    }
    #[test]
    fn luna_and_legacy_think_keep_backend_and_mode() {
        let i = inspection(false, true);
        assert_eq!(
            resolve_route("chatgpt-web/gpt-5.6-luna", "medium", &i)
                .unwrap()
                .adapter_effort,
            AdapterEffort::Medium
        );
        assert_eq!(
            resolve_route("chatgpt-web/think", "low", &i)
                .unwrap()
                .adapter_effort,
            AdapterEffort::Medium
        );
        assert_eq!(
            resolve_route("chatgpt-web/luna", "high", &i)
                .unwrap()
                .adapter_effort,
            AdapterEffort::Low
        );
        assert!(resolve_route("chatgpt-web/think", "low", &inspection(false, false)).is_err());
    }
    #[test]
    fn observed_family_and_locked_ticks_filter_routes() {
        let mut i = inspection(true, true);
        i.families = vec![ModelFamily::Gpt56];
        i.available_efforts[4] = false;
        i.capabilities.pro_available = false;
        i.routes =
            available_routes(&i.capabilities, &i.families, &i.available_efforts, true).unwrap();
        assert!(resolve_route("chatgpt-web/gpt-6-sol", "high", &i).is_err());
        assert!(resolve_route("chatgpt-web/gpt-5.6-pro", "max", &i).is_err());
        assert!(resolve_route("chatgpt-web/gpt-5.6-sol", "high", &i).is_ok());
        // Legacy routes intentionally preserve their unpinned family and route-owned effort.
        assert_eq!(
            resolve_route("chatgpt-web/high", "low", &i)
                .unwrap()
                .adapter_effort,
            AdapterEffort::High
        );
    }
    #[test]
    fn source_smoke_uses_generic_sol_high_or_luna_low() {
        let owner = crate::Ownership {
            task_id: "smoke".into(),
            epoch: 1,
        };
        let sol = crate::smoke_turn_input(&inspection(true, true), owner.clone()).unwrap();
        assert_eq!(sol.model, "chatgpt-web/high");
        assert_eq!(sol.effort, "high");
        let luna = crate::smoke_turn_input(&inspection(false, true), owner).unwrap();
        assert_eq!(luna.model, "chatgpt-web/gpt-5.6-luna");
        assert_eq!(luna.effort, "low");
    }
}
