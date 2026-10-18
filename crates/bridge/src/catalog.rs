//! Official catalog injection and credential-transparent native transport.
use super::*;
use axum::{
    body::Body,
    extract::OriginalUri,
    http::{HeaderMap, Uri},
};
use cw_contracts::{
    BrowserInteractionMode,
    models::{self, ChatGptWebAccountCapabilities},
};

struct NativeLease(Arc<Inner>);
impl Drop for NativeLease {
    fn drop(&mut self) {
        self.0.native_requests.fetch_sub(1, Ordering::SeqCst);
    }
}
impl NativeLease {
    fn acquire(inner: &Arc<Inner>) -> Result<Self, Response> {
        let _turns = inner.turns.lock().unwrap();
        if inner.admission_paused() {
            return Err(error(
                StatusCode::SERVICE_UNAVAILABLE,
                "draining",
                "bridge admission is paused",
            ));
        }
        inner.native_requests.fetch_add(1, Ordering::SeqCst);
        Ok(Self(inner.clone()))
    }
}
struct NativeResponse {
    response: reqwest::Response,
    lease: NativeLease,
}
const OFFICIAL_BASE: &str = "https://chatgpt.com/backend-api/codex";
#[derive(Debug, Clone)]
pub struct ModelCatalogOptions {
    pub full_mode: bool,
    pub compatibility_v1: bool,
    pub context_override: Option<u64>,
}
/// User choices are distinct from evidence about the signed-in account.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RoutePreferences {
    pub experimental_bigger_context: bool,
    pub zero_risk_pro_enabled: bool,
}
impl Default for ModelCatalogOptions {
    fn default() -> Self {
        Self {
            full_mode: true,
            compatibility_v1: false,
            context_override: None,
        }
    }
}
#[derive(Debug, Clone, Default, Serialize)]
pub struct CatalogStatus {
    pub capabilities_observed: bool,
    pub native_catalog_available: bool,
    pub native_catalog_fetched: bool,
    pub augmentation_succeeded: bool,
    /// True only for an official fetched catalog successfully augmented under current settings.
    pub verified: bool,
    pub last_http_status: Option<u16>,
    pub last_error: Option<String>,
    pub successful_requests: u64,
    pub native_models: usize,
    pub routed_models: usize,
    pub last_success_at: Option<String>,
}
#[derive(Default)]
pub(super) struct CatalogState {
    observed: Option<ChatGptWebAccountCapabilities>,
    preferences: RoutePreferences,
    native: Option<Value>,
    options: ModelCatalogOptions,
    base: Option<String>,
    status: CatalogStatus,
}
impl CatalogState {
    pub(super) fn invalidate(&mut self) {
        self.status.augmentation_succeeded = false;
        self.status.verified = false;
        self.status.last_http_status = None;
        self.status.last_error = None;
    }
    fn failure(&mut self, status: StatusCode, message: &str) {
        self.status.verified = false;
        self.status.augmentation_succeeded = false;
        self.status.last_http_status = Some(status.as_u16());
        self.status.last_error = Some(message.into());
    }
}
impl BridgeHandle {
    /// Explicit browser-observed capabilities; no guessed account/model support.
    /// Source JSON names: solAvailable, proAvailable, extraHighAvailable, etc.
    pub fn set_account_capabilities(
        &self,
        caps: ChatGptWebAccountCapabilities,
    ) -> Result<(), String> {
        let mut state = self.inner.catalog.lock().unwrap();
        let changed = state
            .observed
            .as_ref()
            .map(|old| serde_json::to_value(old).unwrap())
            != Some(serde_json::to_value(&caps).map_err(|e| e.to_string())?);
        if changed {
            state.invalidate();
        }
        if let Some(enabled) = caps.experimental_bigger_context {
            state.preferences.experimental_bigger_context = enabled;
        }
        if let Some(enabled) = caps.zero_risk_pro_enabled {
            state.preferences.zero_risk_pro_enabled = enabled;
        }
        state.observed = Some(caps);
        state.status.capabilities_observed = true;
        Ok(())
    }
    /// Explicit logout: no credentials are retained by this bridge. Cached catalog and history are invalidated.
    pub fn clear_account_capabilities(&self) -> Result<(), String> {
        let turns = self.inner.turns.lock().unwrap();
        let mut compactions = self.inner.compactions.lock().unwrap();
        if !turns.is_empty()
            || self.inner.pending_tools.load(Ordering::SeqCst) != 0
            || self.inner.native_requests.load(Ordering::SeqCst) != 0
            || compactions.active_count() != 0
        {
            return Err(
                "account capabilities cannot clear while owners or requests are active".into(),
            );
        }
        let mut state = self.inner.catalog.lock().unwrap();
        state.observed = None;
        state.native = None;
        state.status = CatalogStatus::default();
        self.inner.history.lock().unwrap().clear();
        compactions.clear_idle();
        self.inner.mcp_ledger.lock().unwrap().clear_idle();
        Ok(())
    }
    pub fn catalog_status(&self) -> CatalogStatus {
        self.inner.catalog.lock().unwrap().status.clone()
    }
    /// Applies explicit launcher settings without fabricating an account observation.
    pub fn set_route_preferences(&self, preferences: RoutePreferences) -> Result<(), String> {
        let turns = self.inner.turns.lock().unwrap();
        if !turns.is_empty()
            || self.inner.pending_tools.load(Ordering::SeqCst) != 0
            || self.inner.native_requests.load(Ordering::SeqCst) != 0
            || self.inner.compactions.lock().unwrap().active_count() != 0
        {
            return Err("route preferences cannot change while owners or requests are active".into());
        }
        if self.inner.execution_mode() == ExecutionMode::Manual && preferences.experimental_bigger_context {
            return Err("Zero Risk does not support Bigger Context".into());
        }
        let mut state = self.inner.catalog.lock().unwrap();
        if state.preferences != preferences {
            state.preferences = preferences;
            state.invalidate();
        }
        Ok(())
    }
    /// Original official `models` payload, including all unknown metadata.
    pub fn set_native_model_catalog(&self, catalog: Value) -> Result<(), String> {
        if !catalog["models"].is_array() {
            return Err("native catalog requires models array".into());
        }
        let mut state = self.inner.catalog.lock().unwrap();
        state.invalidate();
        state.status.native_catalog_available = true;
        state.status.native_catalog_fetched = false;
        state.native = Some(catalog);
        Ok(())
    }
    pub fn set_model_catalog_options(&self, options: ModelCatalogOptions) -> Result<(), String> {
        if options.context_override == Some(0) {
            return Err("context override must be positive".into());
        }
        let mut state = self.inner.catalog.lock().unwrap();
        state.invalidate();
        state.options = options;
        Ok(())
    }
    /// Opt-in. Never accepts a third-party origin or credentials in a URL.
    pub fn set_native_passthrough_base(&self, base: &str) -> Result<(), String> {
        if base.trim_end_matches('/') != OFFICIAL_BASE {
            return Err(
                "native passthrough base must be https://chatgpt.com/backend-api/codex".into(),
            );
        }
        self.inner.catalog.lock().unwrap().base = Some(OFFICIAL_BASE.into());
        Ok(())
    }
    pub fn disable_native_passthrough(&self) {
        let mut state = self.inner.catalog.lock().unwrap();
        state.base = None;
        state.invalidate();
    }
}
fn capabilities(
    inner: &Inner,
    state: &CatalogState,
) -> Result<ChatGptWebAccountCapabilities, String> {
    let manual = inner.execution_mode() == ExecutionMode::Manual;
    let mut caps = if manual {
        ChatGptWebAccountCapabilities::default()
    } else {
        state.observed.clone().ok_or("account capabilities have not been observed")?
    };
    caps.experimental_bigger_context = Some(!manual && state.preferences.experimental_bigger_context);
    caps.zero_risk_pro_enabled = Some(state.preferences.zero_risk_pro_enabled);
    caps.browser_interaction_mode = Some(if manual {
        BrowserInteractionMode::Manual
    } else {
        BrowserInteractionMode::Automatic
    });
    Ok(caps)
}
pub(super) fn validate_route(inner: &Inner, raw: &mut Value) -> Result<(), String> {
    let Some(slug) = raw["model"]
        .as_str()
        .filter(|s| s.starts_with("chatgpt-web/"))
    else {
        return Ok(());
    };
    let state = inner.catalog.lock().unwrap();
    let caps = capabilities(inner, &state)?;
    let route =
        models::require_chat_gpt_web_model_route(slug, &caps, raw["reasoning"]["effort"].as_str())
            .map_err(|e| e.to_string())?;
    // Source fixed routes select their adapter effort; grouped routes honor the
    // validated requested effort. Preserve other reasoning configuration fields.
    if !raw["reasoning"].is_object() {
        raw["reasoning"] = json!({});
    }
    raw["reasoning"]["effort"] = json!(route.adapter_effort);
    Ok(())
}
fn catalog_error(
    inner: &Inner,
    status: StatusCode,
    code: &str,
    message: impl Into<String>,
) -> Response {
    let message = message.into();
    inner.catalog.lock().unwrap().failure(status, &message);
    error(status, code, message)
}
pub(super) async fn models(
    State(inner): State<Arc<Inner>>,
    headers: HeaderMap,
    OriginalUri(uri): OriginalUri,
) -> Response {
    if inner.admission_paused() {
        return catalog_error(
            &inner,
            StatusCode::SERVICE_UNAVAILABLE,
            "draining",
            "bridge admission is paused",
        );
    }
    if inner.execution_mode() != ExecutionMode::Manual
        && inner.catalog.lock().unwrap().observed.is_none()
    {
        return catalog_error(
            &inner,
            StatusCode::SERVICE_UNAVAILABLE,
            "capabilities_unobserved",
            "account capabilities have not been observed",
        );
    }
    let need_native = inner.catalog.lock().unwrap().native.is_none();
    if need_native && inner.catalog.lock().unwrap().base.is_some() {
        let native = match forward(&inner, &headers, &uri, None, "models").await {
            Ok(native) => native,
            Err(response) => {
                inner
                    .catalog
                    .lock()
                    .unwrap()
                    .failure(response.status(), "official native catalog fetch failed");
                return response;
            }
        };
        if !native.response.status().is_success() {
            inner.catalog.lock().unwrap().failure(
                native.response.status(),
                "official native catalog returned failure",
            );
            return relay(native);
        }
        let NativeResponse {
            response,
            lease: _lease,
        } = native;
        let value = match response.json::<Value>().await {
            Ok(value) => value,
            Err(e) => {
                return catalog_error(
                    &inner,
                    StatusCode::BAD_GATEWAY,
                    "native_catalog",
                    e.to_string(),
                );
            }
        };
        let mut state = inner.catalog.lock().unwrap();
        state.invalidate();
        state.native = Some(value);
        state.status.native_catalog_available = true;
        state.status.native_catalog_fetched = true;
    }
    let mut state = inner.catalog.lock().unwrap();
    let result = (|| {
        let caps = capabilities(&inner, &state)?;
        let native = state
            .native
            .as_ref()
            .ok_or("original native model catalog has not been supplied")?;
        models::augment_native_model_catalog(
            native,
            &caps,
            state.options.full_mode,
            state.options.compatibility_v1,
            state.options.context_override,
        )
        .map_err(|e| e.to_string())
    })();
    match result {
        Ok(catalog) => {
            state.status.augmentation_succeeded = true;
            state.status.verified = state.status.native_catalog_fetched;
            state.status.last_http_status = Some(200);
            state.status.last_error = None;
            state.status.successful_requests += 1;
            state.status.last_success_at = Some(chrono::Utc::now().to_rfc3339());
            let rows = catalog["models"].as_array().unwrap();
            state.status.routed_models = rows
                .iter()
                .filter(|row| {
                    row["slug"]
                        .as_str()
                        .is_some_and(|s| s.starts_with("chatgpt-web/"))
                })
                .count();
            state.status.native_models = rows.len() - state.status.routed_models;
            Json(catalog).into_response()
        }
        Err(message) => {
            let status = if state.native.is_none() {
                StatusCode::SERVICE_UNAVAILABLE
            } else {
                StatusCode::BAD_GATEWAY
            };
            state.failure(status, &message);
            error(status, "invalid_native_catalog", message)
        }
    }
}

pub(super) async fn compact(
    State(inner): State<Arc<Inner>>,
    headers: HeaderMap,
    OriginalUri(uri): OriginalUri,
    Json(mut raw): Json<Value>,
) -> Response {
    if let Some(response) = maybe_native(&inner, &headers, &uri, &raw, "responses/compact").await {
        return response;
    }
    if let Some(metadata) = headers
        .get("x-codex-turn-metadata")
        .and_then(|v| v.to_str().ok())
    {
        if !raw["client_metadata"].is_object() {
            raw["client_metadata"] = json!({});
        }
        raw["client_metadata"]["x-codex-turn-metadata"] = json!(metadata);
    }
    if let Err(e) = validate_route(&inner, &mut raw) {
        return error(StatusCode::BAD_REQUEST, "invalid_model", e);
    }
    if raw["model"].as_str().is_some_and(|s| {
        matches!(
            s,
            "chatgpt-web/luna" | "chatgpt-web/think" | "chatgpt-web/gpt-5.6-luna"
        )
    }) && !inner
        .catalog
        .lock()
        .unwrap()
        .preferences
        .experimental_bigger_context
    {
        return error(
            StatusCode::CONFLICT,
            "invalid_request_error",
            "ChatGPT Web Luna uses a rolling checkpoint on every completed browser turn; separate Codex compaction is disabled for this route.",
        );
    }
    compaction::compact_v1(inner, raw).await
}

pub(super) async fn maybe_native(
    inner: &Arc<Inner>,
    headers: &HeaderMap,
    uri: &Uri,
    raw: &Value,
    endpoint: &str,
) -> Option<Response> {
    let native = {
        let state = inner.catalog.lock().unwrap();
        state.base.is_some()
            && !raw["model"]
                .as_str()
                .unwrap_or("")
                .starts_with("chatgpt-web/")
    };
    if !native {
        return None;
    }
    if inner.admission_paused() {
        return Some(error(
            StatusCode::SERVICE_UNAVAILABLE,
            "draining",
            "bridge admission is paused",
        ));
    }
    Some(
        match forward(inner, headers, uri, Some(raw), endpoint).await {
            Ok(response) => relay(response),
            Err(e) => e,
        },
    )
}
fn hop_header(name: &str) -> bool {
    matches!(
        name,
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
            | "host"
            | "content-length"
    )
}
/// Build a request without consulting any credential store, profile or environment.
fn native_request(
    headers: &HeaderMap,
    uri: &Uri,
    body: Option<&Value>,
    endpoint: &str,
) -> Result<reqwest::Request, String> {
    if !matches!(endpoint, "models" | "responses" | "responses/compact") {
        return Err("unsupported native endpoint".into());
    }
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .filter(|s| s.starts_with("Bearer ") && s.len() > 7)
        .ok_or("Native Codex passthrough requires the incoming Bearer authorization")?;
    let mut url = reqwest::Url::parse(&format!("{OFFICIAL_BASE}/{endpoint}")).unwrap();
    url.set_query(uri.query());
    if endpoint == "models" && !url.query_pairs().any(|(k, _)| k == "client_version") {
        if let Some(version) = headers
            .get("user-agent")
            .and_then(|v| v.to_str().ok())
            .and_then(client_version)
        {
            url.query_pairs_mut()
                .append_pair("client_version", &version);
        }
    }
    let mut clean = HeaderMap::new();
    for (name, value) in headers {
        if !hop_header(name.as_str()) && !(endpoint == "models" && name == "if-none-match") {
            clean.insert(name.clone(), value.clone());
        }
    }
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .build()
        .map_err(|e| e.to_string())?;
    let builder = if endpoint == "models" {
        client.get(url)
    } else {
        client.post(url)
    }
    .headers(clean);
    let builder = if let Some(body) = body {
        builder.json(&scrub_native(body)?)
    } else {
        builder
    };
    builder.build().map_err(|e| e.to_string())
}
fn client_version(ua: &str) -> Option<String> {
    let (origin, tail) = ua.split_once('/')?;
    if !matches!(
        origin,
        "codex_cli_rs" | "codex-tui" | "codex_vscode" | "codex_atlas" | "codex_chatgpt_desktop"
    ) && !(origin.starts_with("Codex ") && origin.len() <= 70)
    {
        return None;
    }
    let version = tail.split_whitespace().next()?.split(['-', '+']).next()?;
    let parts: Vec<_> = version.split('.').collect();
    (parts.len() == 3
        && parts
            .iter()
            .all(|s| !s.is_empty() && s.len() <= 6 && s.chars().all(|c| c.is_ascii_digit())))
    .then(|| version.into())
}
async fn forward(
    inner: &Arc<Inner>,
    headers: &HeaderMap,
    uri: &Uri,
    body: Option<&Value>,
    endpoint: &str,
) -> Result<NativeResponse, Response> {
    let request = native_request(headers, uri, body, endpoint)
        .map_err(|e| error(StatusCode::UNAUTHORIZED, "native_authorization", e))?;
    // Redirects remain responses; incoming harness credentials never follow another origin.
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .build()
        .map_err(|e| error(StatusCode::BAD_GATEWAY, "native_transport", e.to_string()))?;
    let lease = NativeLease::acquire(inner)?;
    let mut stop = inner.shutdown.subscribe();
    let response = tokio::select! {
        biased;
        _=stop.wait_for(|v|*v)=>return Err(error(StatusCode::SERVICE_UNAVAILABLE,"bridge_closed","bridge closed")),
        response=client.execute(request)=>response.map_err(|e|error(StatusCode::BAD_GATEWAY,"native_transport",e.to_string()))?,
    };
    Ok(NativeResponse { response, lease })
}
fn relay(native: NativeResponse) -> Response {
    let NativeResponse { response, lease } = native;
    let status = response.status();
    let mut headers = HeaderMap::new();
    for (name, value) in response.headers() {
        if !hop_header(name.as_str()) {
            headers.insert(name.clone(), value.clone());
        }
    }
    let mut stop = lease.0.shutdown.subscribe();
    let (sender, receiver) = mpsc::channel(8);
    let is_sse = headers
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.contains("text/event-stream"));
    tokio::spawn(async move {
        let _lease = lease;
        let mut bytes = response.bytes_stream();
        let mut tail = vec![];
        let mut done = false;
        loop {
            let chunk = tokio::select! {biased;_=stop.wait_for(|v|*v)=>break,_=sender.closed()=>break,chunk=futures_util::StreamExt::next(&mut bytes)=>chunk};
            let Some(chunk) = chunk else {
                break;
            };
            if let Ok(chunk) = &chunk {
                if is_sse {
                    tail.extend_from_slice(chunk);
                    while let Some(end) = tail.iter().position(|b| *b == b'\n') {
                        let line = tail.drain(..=end).collect::<Vec<_>>();
                        if line
                            .strip_suffix(b"\n")
                            .unwrap_or(&line)
                            .strip_suffix(b"\r")
                            .unwrap_or(line.strip_suffix(b"\n").unwrap_or(&line))
                            == b"data: [DONE]"
                        {
                            done = true;
                        }
                    }
                    if tail == b"data: [DONE]" || tail == b"data: [DONE]\r" {
                        done = true;
                    }
                }
            } else if done {
                break;
            }
            let failed = chunk.is_err();
            if tokio::select! {biased;_=stop.wait_for(|v|*v)=>true,result=sender.send(chunk)=>result.is_err()}
            {
                break;
            }
            if done || failed {
                break;
            }
        }
    });
    let mut result = Response::new(Body::from_stream(ReceiverStream::new(receiver)));
    *result.status_mut() = status;
    *result.headers_mut() = headers;
    result
}

fn scrub_native(raw: &Value) -> Result<Value, String> {
    use base64::Engine;
    let Some(input) = raw["input"].as_array() else {
        return Ok(raw.clone());
    };
    let is_bridge = |item: &Value| {
        (item["type"] == "reasoning"
            && item["id"]
                .as_str()
                .and_then(|s| s.strip_prefix("rs_"))
                .is_some_and(|s| s.len() == 32 && s.bytes().all(|b| b.is_ascii_hexdigit()))
            && item["encrypted_content"].is_null()
            && (item["summary"].is_array() || item["content"].is_array()))
            || item["encrypted_content"]
                .as_str()
                .is_some_and(|s| s.starts_with("ocx1:") || s.starts_with("ocxr1:"))
    };
    if !input.iter().any(is_bridge) {
        return Ok(raw.clone());
    }
    let mut clean = raw.clone();
    let mut items = vec![];
    for item in input {
        let mut item = item.clone();
        if let Some(map) = item.as_object_mut() {
            map.remove("id");
        }
        if item["type"] == "compaction"
            && item["encrypted_content"]
                .as_str()
                .is_some_and(|s| s.starts_with("ocx1:"))
        {
            let summary = base64::engine::general_purpose::STANDARD
                .decode(
                    item["encrypted_content"]
                        .as_str()
                        .unwrap()
                        .trim_start_matches("ocx1:"),
                )
                .map_err(|_| "invalid bridge compaction envelope")?;
            let summary = String::from_utf8(summary).map_err(|_| "invalid summary UTF-8")?;
            item = json!({"type":"message","role":"user","content":[{"type":"input_text","text":format!("{}\n\n{summary}",crate::protocol::SUMMARY_PREFIX)}]});
        } else if item["type"] == "reasoning" && is_bridge(&item) {
            item.as_object_mut().unwrap().remove("encrypted_content");
            if !item["summary"].as_array().is_some_and(|a| !a.is_empty())
                && !item["content"].as_array().is_some_and(|a| !a.is_empty())
            {
                continue;
            }
        }
        items.push(item);
    }
    clean["input"] = json!(items);
    clean
        .as_object_mut()
        .unwrap()
        .remove("previous_response_id");
    Ok(clean)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn native_request_uses_only_official_origin_and_incoming_auth() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "authorization",
            "Bearer fixture-harness-token".parse().unwrap(),
        );
        headers.insert(
            "user-agent",
            "codex_cli_rs/0.150.0-alpha.1 (macOS)".parse().unwrap(),
        );
        headers.insert("if-none-match", "fixture".parse().unwrap());
        let request =
            native_request(&headers, &"/models?x=1".parse().unwrap(), None, "models").unwrap();
        assert_eq!(
            request.url().origin().ascii_serialization(),
            "https://chatgpt.com"
        );
        assert_eq!(request.url().path(), "/backend-api/codex/models");
        assert!(
            request
                .url()
                .query()
                .unwrap()
                .contains("client_version=0.150.0")
        );
        assert_eq!(
            request.headers()["authorization"],
            "Bearer fixture-harness-token"
        );
        assert!(request.headers().get("if-none-match").is_none());
        assert!(
            native_request(
                &HeaderMap::new(),
                &"/responses".parse().unwrap(),
                Some(&json!({})),
                "responses"
            )
            .is_err()
        );
    }
}
