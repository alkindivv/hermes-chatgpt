use cw_browser::Cancellation;

/// Replays launcher settings into the owned bridge. The caller holds an idle session lease.
pub fn apply_launcher_settings(bridge: &cw_bridge::BridgeHandle, state: &cw_contracts::LauncherState) -> Result<(), String> {
    let manual = state.browser_interaction_mode == cw_contracts::BrowserInteractionMode::Manual;
    bridge.set_mode(if manual { cw_bridge::ExecutionMode::Manual } else { cw_bridge::ExecutionMode::Automatic })?;
    bridge.set_route_preferences(cw_bridge::RoutePreferences {
        experimental_bigger_context: !manual && state.experimental_bigger_context,
        zero_risk_pro_enabled: state.zero_risk_pro_enabled,
    })?;
    if state.core_setup_complete == Some(true) {
        bridge.set_native_passthrough_base("https://chatgpt.com/backend-api/codex")?;
    } else {
        bridge.disable_native_passthrough();
    }
    Ok(())
}

pub enum ProbeObservation {
    SignedOut,
    Capabilities(cw_browser::CapabilityInspection),
    Account(cw_browser::AccountInspection),
}

pub async fn inspect(
    engine: &cw_browser::BrowserEngine,
    kind: ProbeKind,
    cancellation: Cancellation,
) -> Result<ProbeObservation, String> {
    use cw_browser::InteractionMode::Automatic;
    if kind == ProbeKind::Account {
        return engine.inspect_account(Automatic, cancellation).await
            .map(ProbeObservation::Account).map_err(|error| error.to_string());
    }
    let auth = engine.inspect_auth_for_mode(Automatic, cancellation.clone()).await
        .map_err(|error| error.to_string())?;
    if !auth.server_authenticated {
        return if auth.signed_out_confirmed { Ok(ProbeObservation::SignedOut) }
            else { Err("The browser session could not be verified. Retry when the page and network are ready".into()) };
    }
    engine.inspect_capabilities(Automatic, cancellation).await
        .map(ProbeObservation::Capabilities).map_err(|error| error.to_string())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProbeKind { Auth, Capabilities, Account, Connector }

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeTicket {
    pub id: String,
    pub kind: ProbeKind,
    generation: u64,
    serial: u64,
    epoch: u64,
    url: String,
}

pub struct ProbeTarget<'a> {
    pub id: &'a str,
    pub epoch: u64,
    pub url: &'a str,
    pub available: bool,
}

pub fn is_chatgpt_origin(value: &str) -> bool {
    url::Url::parse(value).is_ok_and(|url| {
        url.scheme() == "https" && url.host_str() == Some("chatgpt.com")
            && url.port_or_known_default() == Some(443)
            && url.username().is_empty() && url.password().is_none()
    })
}

#[derive(Default)]
pub struct ProbeTracker {
    generation: u64,
    serial: u64,
    active: Option<(ProbeTicket, Cancellation)>,
}

impl ProbeTracker {
    pub fn begin(&mut self, kind: ProbeKind, target: ProbeTarget<'_>, automatic: bool) -> Option<(ProbeTicket, Cancellation)> {
        if self.active.is_some() || !automatic || !target.available || !is_chatgpt_origin(target.url) {
            return None;
        }
        self.serial = self.serial.checked_add(1).expect("probe serial exhausted");
        let ticket = ProbeTicket { id: target.id.into(), kind, generation: self.generation, serial: self.serial, epoch: target.epoch, url: target.url.into() };
        let cancellation = Cancellation::default();
        self.active = Some((ticket.clone(), cancellation.clone()));
        Some((ticket, cancellation))
    }

    pub fn finish(&mut self, ticket: &ProbeTicket, target: Option<ProbeTarget<'_>>, automatic: bool) -> bool {
        if !self.active.as_ref().is_some_and(|(active, _)| active == ticket) { return false; }
        let (_, signal) = self.active.take().expect("matched active probe");
        let valid = automatic && !signal.is_cancelled() && ticket.generation == self.generation
            && target.is_some_and(|target| target.available && target.id == ticket.id
                && target.epoch == ticket.epoch && target.url == ticket.url && is_chatgpt_origin(target.url));
        signal.cancel();
        valid
    }

    pub fn invalidate(&mut self) {
        if let Some((_, signal)) = self.active.take() { signal.cancel(); }
        self.generation = self.generation.checked_add(1).expect("session generation exhausted");
    }

    pub fn is_active(&self) -> bool { self.active.is_some() }
    pub fn current(&self) -> Option<&ProbeTicket> { self.active.as_ref().map(|(ticket, _)| ticket) }
    pub fn target_matches(&self, target: Option<ProbeTarget<'_>>, automatic: bool) -> bool {
        self.active.as_ref().is_some_and(|(ticket, signal)| {
            automatic && !signal.is_cancelled() && ticket.generation == self.generation
                && target.is_some_and(|target| target.available && target.id == ticket.id
                    && target.epoch == ticket.epoch && target.url == ticket.url)
        })
    }
}

impl Drop for ProbeTracker {
    fn drop(&mut self) { self.invalidate(); }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page<'a>(id: &'a str, url: &'a str, epoch: u64) -> ProbeTarget<'a> {
        ProbeTarget { id, url, epoch, available: true }
    }

    #[test]
    fn only_exact_https_chatgpt_origin_can_start_an_automatic_probe() {
        let mut probes = ProbeTracker::default();
        for url in ["https://chatgpt.com.evil.test/", "https://chatgpt.com@evil.test/", "http://chatgpt.com/", "https://chatgpt.com:444/", "https://user@chatgpt.com/", "about:blank"] {
            assert!(probes.begin(ProbeKind::Auth, page("one", url, 0), true).is_none(), "accepted {url}");
        }
        assert!(probes.begin(ProbeKind::Auth, page("one", "https://chatgpt.com/", 0), false).is_none());
        let (ticket, _) = probes.begin(ProbeKind::Auth, page("one", "https://chatgpt.com:443/?temporary-chat=true", 0), true).unwrap();
        assert!(probes.finish(&ticket, Some(page("one", "https://chatgpt.com:443/?temporary-chat=true", 0)), true));
    }

    #[test]
    fn logout_cancels_old_probe_and_its_reply_cannot_finish_a_new_probe() {
        let mut probes = ProbeTracker::default();
        let target = || page("one", "https://chatgpt.com/", 0);
        let (old, cancelled) = probes.begin(ProbeKind::Capabilities, target(), true).unwrap();
        probes.invalidate();
        assert!(cancelled.is_cancelled());
        let (new, active) = probes.begin(ProbeKind::Auth, target(), true).unwrap();
        assert!(!probes.finish(&old, Some(target()), true));
        assert!(probes.is_active());
        assert!(!active.is_cancelled());
        assert!(probes.finish(&new, Some(target()), true));
        assert!(!probes.is_active());
    }

    #[test]
    fn navigation_turn_reuse_closed_tab_and_manual_mode_reject_stale_results() {
        for changed in 0..5 {
            let mut probes = ProbeTracker::default();
            let (ticket, _) = probes.begin(ProbeKind::Account, page("one", "https://chatgpt.com/", 1), true).unwrap();
            let mut target = page("one", "https://chatgpt.com/", 1);
            match changed {
                0 => target.url = "https://chatgpt.com/c/another",
                1 => target.epoch = 2,
                2 => target.available = false,
                _ => {}
            }
            assert!(!probes.finish(&ticket, if changed == 3 { None } else { Some(target) }, changed != 4));
            assert!(!probes.is_active());
        }
    }

    #[test]
    fn one_profile_probe_at_a_time_and_drop_cancels_its_worker() {
        let mut probes = ProbeTracker::default();
        let (_, signal) = probes.begin(ProbeKind::Auth, page("one", "https://chatgpt.com/", 0), true).unwrap();
        assert!(probes.begin(ProbeKind::Auth, page("two", "https://chatgpt.com/", 0), true).is_none());
        drop(probes);
        assert!(signal.is_cancelled());
    }

    #[tokio::test]
    async fn launcher_settings_enable_manual_pro_without_account_observation() {
        use cw_bridge::{BridgeConfig, BridgeServer, BrowserRequest, ExecutionMode};
        use cw_contracts::{BrowserInteractionMode, LauncherState};
        use serde_json::json;
        let (bridge, mut requests) = BridgeServer::bind("127.0.0.1:0".parse().unwrap(), BridgeConfig::default()).await.unwrap();
        let mut state = LauncherState::default();
        state.browser_interaction_mode = BrowserInteractionMode::Manual;
        state.zero_risk_pro_enabled = true;
        let lease = bridge.begin_session_change().unwrap();
        apply_launcher_settings(&bridge, &state).unwrap();
        drop(lease);
        assert_eq!(bridge.execution_mode(), ExecutionMode::Manual);
        assert!(!bridge.catalog_status().capabilities_observed);
        let response = reqwest::Client::new().post(format!("{}/responses", bridge.base_url()))
            .json(&json!({"model":"chatgpt-web/zero-risk-pro","input":"fixture","stream":true})).send().await.unwrap();
        assert_eq!(response.status(), 200);
        let BrowserRequest::Turn(turn) = requests.recv().await.unwrap() else { panic!("expected manual turn") };
        assert_eq!(turn.mode, ExecutionMode::Manual);
        assert!(bridge.authorize(&turn.turn_token, &turn.owner).is_err());
        bridge.cancel(&turn.request_id);
        response.text().await.unwrap();
        bridge.close().await.unwrap();
    }

    #[tokio::test]
    async fn installed_route_enables_incoming_auth_passthrough_but_never_invents_catalog_proof() {
        use cw_bridge::{BridgeConfig, BridgeServer};
        use cw_contracts::LauncherState;
        use serde_json::json;
        let (bridge, mut requests) = BridgeServer::bind("127.0.0.1:0".parse().unwrap(), BridgeConfig::default()).await.unwrap();
        let mut state = LauncherState::default();
        state.core_setup_complete = Some(true);
        state.codex_catalog_verified = Some(true);
        let lease = bridge.begin_session_change().unwrap();
        apply_launcher_settings(&bridge, &state).unwrap();
        drop(lease);
        let response = reqwest::Client::new().post(format!("{}/responses", bridge.base_url()))
            .json(&json!({"model":"native-fixture","stream":true,"input":"fixture"})).send().await.unwrap();
        assert_eq!(response.status(), 401, "Native requests require incoming harness auth before any network fetch");
        assert!(requests.try_recv().is_err());
        assert!(!bridge.catalog_status().verified, "Stored UI flags are not current official evidence");
        bridge.close().await.unwrap();
    }
}
