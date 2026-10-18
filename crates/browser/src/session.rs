//! Explicit replacement of session state in a caller-owned, exclusively leased CEF profile.
use crate::*;
use cw_runtime::ValidatedLoginState;

pub struct SessionReceipt { pub inspection: Option<CapabilityInspection> }
#[derive(Debug, thiserror::Error)]
#[error("{message}")]
pub struct SessionFailure { pub message: String, pub cleanup_confirmed: bool }

impl BrowserEngine {
    /// The launcher parks every other view in this same isolated profile before replacement.
    pub async fn park_for_session_change(&self, cancellation: Cancellation) -> Result<()> {
        if self.state.lock().await.cleanup_unconfirmed { return Err(Error::CleanupUnconfirmed); }
        self.navigate_owned_document("about:blank", cancellation).await
    }

    pub async fn replace_login_state(&self, login: Option<&ValidatedLoginState>, mode: InteractionMode, cancellation: Cancellation) -> std::result::Result<SessionReceipt, SessionFailure> {
        if mode == InteractionMode::Manual || cancellation.is_cancelled() {
            return Err(SessionFailure { message: if mode == InteractionMode::Manual { "Zero Risk does not import or inspect browser sessions" } else { "Session change cancelled" }.into(), cleanup_confirmed: true });
        }
        if self.park_for_session_change(cancellation.clone()).await.is_err() {
            return Err(SessionFailure { message: "The owned browser could not be parked before the session change".into(), cleanup_confirmed: false });
        }
        let result = self.replace_session_inner(login, &cancellation).await;
        if let Ok(receipt) = result { return Ok(receipt); }
        // Cancellation can arrive after Chromium accepted a cookie write. Roll back with a fresh signal.
        let cleanup = Cancellation::default();
        let confirmed = async {
            self.park_for_session_change(cleanup.clone()).await?;
            self.clear_session_data(login, &cleanup).await?;
            self.navigate_with_cancellation(cw_runtime::signin::TEMPORARY_CHAT_URL, cleanup.clone()).await?;
            let auth = self.inspect_auth_for_mode(InteractionMode::Automatic, cleanup).await?;
            if auth.server_authenticated || !auth.signed_out_confirmed { return Err(Error::Unauthenticated); }
            Ok::<(), Error>(())
        }.await.is_ok();
        if !confirmed { self.state.lock().await.cleanup_unconfirmed = true; }
        Err(SessionFailure {
            message: if confirmed { "The session change failed. The partial session was cleared and sign-out was verified" }
                else { "The session change failed and cleanup could not be verified. The browser remains reserved" }.into(),
            cleanup_confirmed: confirmed,
        })
    }

    async fn session_command(&self, method: &str, params: Value, cancellation: &Cancellation) -> Result<Value> {
        self.client.request(method, params, Duration::from_secs(10), cancellation).await
            .map_err(|_| Error::Dom(format!("Owned session command failed: {method}")))
    }

    async fn clear_session_data(&self, login: Option<&ValidatedLoginState>, cancellation: &Cancellation) -> Result<()> {
        self.session_command("Network.clearBrowserCookies", json!({}), cancellation).await?;
        self.session_command("Network.clearBrowserCache", json!({}), cancellation).await?;
        let mut origins = std::collections::BTreeSet::from([
            "https://chatgpt.com".to_string(), "https://openai.com".to_string(),
            "https://auth.openai.com".to_string(), "https://auth0.openai.com".to_string(),
        ]);
        if let Some(login) = login {
            for cookie in login.storage_state()["cookies"].as_array().expect("validated cookies") {
                origins.insert(format!("https://{}", cookie["domain"].as_str().expect("validated domain").trim_start_matches('.')));
            }
        }
        for origin in origins {
            self.session_command("Storage.clearDataForOrigin", json!({"origin":origin,"storageTypes":"all"}), cancellation).await?;
        }
        Ok(())
    }

    async fn replace_session_inner(&self, login: Option<&ValidatedLoginState>, cancellation: &Cancellation) -> Result<SessionReceipt> {
        self.clear_session_data(login, cancellation).await?;
        if let Some(login) = login {
            let cookies: Vec<Value> = login.storage_state()["cookies"].as_array().expect("validated cookies").iter().map(|cookie| {
                let domain = cookie["domain"].as_str().expect("validated domain");
                let mut value = cookie.clone();
                value["url"] = json!(format!("https://{}{}", domain.trim_start_matches('.'), cookie["path"].as_str().expect("validated path")));
                if !domain.starts_with('.') { value.as_object_mut().unwrap().remove("domain"); }
                if cookie["expires"].as_f64().is_none_or(|expiry| expiry <= 0.) { value.as_object_mut().unwrap().remove("expires"); }
                value
            }).collect();
            self.session_command("Network.setCookies", json!({"cookies":cookies}), cancellation).await?;
        }
        self.navigate_with_cancellation(cw_runtime::signin::TEMPORARY_CHAT_URL, cancellation.clone()).await?;
        if let Some(login) = login {
            let entries = login.portable_state()["localStorage"].as_array().expect("validated local storage");
            if !entries.is_empty() {
                let frame = self.session_command("Page.getFrameTree", json!({}), cancellation).await?;
                let parsed = url::Url::parse(frame["frameTree"]["frame"]["url"].as_str().unwrap_or(""))
                    .map_err(|_| Error::Dom("Session import has no main-frame identity".into()))?;
                if parsed.origin().ascii_serialization() != "https://chatgpt.com" || !parsed.username().is_empty() || parsed.password().is_some() {
                    return Err(Error::Dom("Session import reached an unexpected origin".into()));
                }
                self.session_command("DOMStorage.enable", json!({}), cancellation).await?;
                for entry in entries {
                    self.session_command("DOMStorage.setDOMStorageItem", json!({"storageId":{"securityOrigin":"https://chatgpt.com","isLocalStorage":true},"key":entry["name"],"value":entry["value"]}), cancellation).await?;
                }
                self.navigate_with_cancellation(cw_runtime::signin::TEMPORARY_CHAT_URL, cancellation.clone()).await?;
            }
            let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
            loop {
                let auth = self.inspect_auth_for_mode(InteractionMode::Automatic, cancellation.clone()).await?;
                if auth.server_authenticated { break; }
                if tokio::time::Instant::now() >= deadline { return Err(Error::Unauthenticated); }
                tokio::select! { _=cancellation.cancelled()=>return Err(Error::Cancelled), _=tokio::time::sleep(Duration::from_millis(250))=>{} }
            }
            let inspection = self.inspect_capabilities(InteractionMode::Automatic, cancellation.clone()).await?;
            Ok(SessionReceipt { inspection: Some(inspection) })
        } else {
            let auth = self.inspect_auth_for_mode(InteractionMode::Automatic, cancellation.clone()).await?;
            if auth.server_authenticated || !auth.signed_out_confirmed { return Err(Error::Unauthenticated); }
            Ok(SessionReceipt { inspection: None })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::{SinkExt, StreamExt};
    use std::sync::{Arc, Mutex as StdMutex};
    use tokio::{net::TcpListener, sync::Notify};

    #[derive(Clone, Copy)]
    enum Scenario { Import, Logout, CookieFailure, CleanupFailure, CancelAfterCookie }
    struct Fixture {
        url: String,
        calls: Arc<StdMutex<Vec<Value>>>,
        cookie_seen: Arc<Notify>,
        task: tokio::task::JoinHandle<()>,
    }
    impl Drop for Fixture { fn drop(&mut self) { self.task.abort(); } }
    async fn fixture(scenario: Scenario) -> Fixture {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("ws://127.0.0.1:{}/devtools/page/owned-session", listener.local_addr().unwrap().port());
        let calls = Arc::new(StdMutex::new(Vec::new()));
        let recorded = calls.clone();
        let cookie_seen = Arc::new(Notify::new());
        let notify = cookie_seen.clone();
        let task = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut ws = tokio_tungstenite::accept_async(socket).await.unwrap();
            let mut authed = matches!(scenario, Scenario::Logout);
            let mut origin = "https://chatgpt.com".to_string();
            let mut clears = 0;
            while let Some(Ok(message)) = ws.next().await {
                let request: Value = serde_json::from_slice(&message.into_data()).unwrap();
                recorded.lock().unwrap().push(request.clone());
                let method = request["method"].as_str().unwrap();
                let mut error = None;
                let result = match method {
                    "Page.enable" | "Page.setLifecycleEventsEnabled" | "Network.enable" | "Network.clearBrowserCache"
                    | "Storage.clearDataForOrigin" | "DOMStorage.enable" | "DOMStorage.setDOMStorageItem" => json!({}),
                    "Page.navigate" => {
                        origin = if request["params"]["url"] == "about:blank" { "null" } else { "https://chatgpt.com" }.into();
                        ws.send(json!({"id":request["id"],"result":{"frameId":"ours","loaderId":"loaded"}}).to_string().into()).await.unwrap();
                        ws.send(json!({"method":"Page.lifecycleEvent","params":{"name":"load","frameId":"ours","loaderId":"loaded"}}).to_string().into()).await.unwrap();
                        continue;
                    }
                    "Network.clearBrowserCookies" => {
                        clears += 1;
                        if matches!(scenario, Scenario::CleanupFailure) && clears > 1 {
                            error = Some(json!({"message":"fixture-secret cleanup error"}));
                        } else { authed = false; }
                        json!({})
                    }
                    "Network.setCookies" => {
                        authed = true;
                        notify.notify_one();
                        if matches!(scenario, Scenario::CancelAfterCookie) { continue; }
                        if matches!(scenario, Scenario::CookieFailure | Scenario::CleanupFailure) {
                            error = Some(json!({"message":"fixture-secret cookie error"}));
                        }
                        json!({})
                    }
                    "Page.getFrameTree" => json!({"frameTree":{"frame":{"id":"ours","url":if origin=="null"{"about:blank"}else{"https://chatgpt.com/?temporary-chat=true"},"securityOrigin":origin}}}),
                    "Runtime.evaluate" => {
                        let expression = request["params"]["expression"].as_str().unwrap();
                        let value = if expression.contains(")(\"auth\",") {
                            json!({"readable":true,"composer":true,"server_authenticated":authed,"signed_out_confirmed":!authed})
                        } else if expression.contains(")(\"ready\",") { json!(true) }
                        else if expression.contains(")(\"account\",") { json!({"user_id":"fixture-user","account_id":"fixture-account","plan_type":"prolite","structure":"personal","needs_attention":false,"server_authenticated":authed}) }
                        else if expression.contains(")(\"capabilityState\",") { json!({"composer_ready":true,"document_ready":true,"selector_count":1,"think_available":false}) }
                        else if expression.contains(")(\"capabilityPicker\",") { json!({"available":[true,true,true,true,false],"families":["5.6"]}) }
                        else { panic!("unexpected session expression"); };
                        json!({"result":{"value":value}})
                    }
                    _ => panic!("unexpected session command {method}"),
                };
                let reply = if let Some(error) = error { json!({"id":request["id"],"error":error}) }
                    else { json!({"id":request["id"],"result":result}) };
                if ws.send(reply.to_string().into()).await.is_err() { break; }
            }
        });
        Fixture { url, calls, cookie_seen, task }
    }
    fn login() -> ValidatedLoginState {
        cw_runtime::validate_passkey_login_state(&json!({"cookies":[{"name":"session","value":"fixture-secret","domain":"chatgpt.com","path":"/","expires":-1,"httpOnly":true,"secure":true,"sameSite":"Lax"}],"origins":[{"origin":"https://chatgpt.com","localStorage":[{"name":"fixture","value":"value"}]}]})).unwrap()
    }

    #[tokio::test]
    async fn imported_cookies_are_not_success_until_owned_server_and_account_verification() {
        let fixture = fixture(Scenario::Import).await;
        let engine = BrowserEngine::connect(&fixture.url).await.unwrap();
        let receipt = engine.replace_login_state(Some(&login()), InteractionMode::Automatic, Cancellation::default()).await.unwrap();
        assert!(receipt.inspection.unwrap().account.server_authenticated);
        let calls = fixture.calls.lock().unwrap();
        let at = |name| calls.iter().position(|call| call["method"] == name).unwrap();
        assert!(at("Page.navigate") < at("Network.clearBrowserCookies"));
        assert!(at("Network.clearBrowserCookies") < at("Network.setCookies"));
        assert!(at("Network.setCookies") < at("DOMStorage.setDOMStorageItem"));
        let cookie = &calls[at("Network.setCookies")]["params"]["cookies"][0];
        assert_eq!(cookie["name"], "session");
        assert_eq!(cookie["url"], "https://chatgpt.com/");
        assert!(cookie.get("domain").is_none() && cookie.get("expires").is_none());
        assert_eq!(calls[at("DOMStorage.setDOMStorageItem")]["params"]["storageId"]["securityOrigin"], "https://chatgpt.com");
        assert!(!calls.iter().any(|call| call["method"] == "Storage.getCookies"));
    }

    #[tokio::test]
    async fn logout_clears_owned_storage_and_requires_signed_out_server_evidence() {
        let fixture = fixture(Scenario::Logout).await;
        let engine = BrowserEngine::connect(&fixture.url).await.unwrap();
        let receipt = engine.replace_login_state(None, InteractionMode::Automatic, Cancellation::default()).await.unwrap();
        assert!(receipt.inspection.is_none());
        let calls = fixture.calls.lock().unwrap();
        assert!(calls.iter().any(|call| call["method"] == "Storage.clearDataForOrigin" && call["params"]["origin"] == "https://chatgpt.com"));
        assert!(!calls.iter().any(|call| call["method"] == "Network.setCookies"));
    }

    #[tokio::test]
    async fn partial_import_is_cleared_and_sensitive_protocol_errors_are_not_reported() {
        for scenario in [Scenario::CookieFailure, Scenario::CleanupFailure] {
            let fixture = fixture(scenario).await;
            let engine = BrowserEngine::connect(&fixture.url).await.unwrap();
            let error = engine.replace_login_state(Some(&login()), InteractionMode::Automatic, Cancellation::default()).await.err().unwrap();
            assert_eq!(error.cleanup_confirmed, matches!(scenario, Scenario::CookieFailure));
            assert!(!error.to_string().contains("fixture-secret"));
            assert!(fixture.calls.lock().unwrap().iter().filter(|call| call["method"] == "Network.clearBrowserCookies").count() >= 2);
            assert_eq!(engine.state.lock().await.cleanup_unconfirmed, matches!(scenario, Scenario::CleanupFailure));
        }
    }

    #[tokio::test]
    async fn cancellation_after_cookie_write_waits_for_fresh_cleanup() {
        let fixture = fixture(Scenario::CancelAfterCookie).await;
        let engine = Arc::new(BrowserEngine::connect(&fixture.url).await.unwrap());
        let cancel = Cancellation::default();
        let worker = tokio::spawn({ let engine = engine.clone(); let cancel = cancel.clone(); async move {
            engine.replace_login_state(Some(&login()), InteractionMode::Automatic, cancel).await
        }});
        tokio::time::timeout(Duration::from_secs(3), fixture.cookie_seen.notified()).await.unwrap();
        cancel.cancel();
        let error = worker.await.unwrap().err().unwrap();
        assert!(error.cleanup_confirmed);
        assert!(!engine.state.lock().await.cleanup_unconfirmed);
    }

    #[tokio::test]
    async fn manual_session_mutation_sends_no_commands() {
        let fixture = fixture(Scenario::Import).await;
        let engine = BrowserEngine::connect(&fixture.url).await.unwrap();
        assert!(engine.replace_login_state(Some(&login()), InteractionMode::Manual, Cancellation::default()).await.is_err());
        assert!(fixture.calls.lock().unwrap().is_empty());
    }
}
