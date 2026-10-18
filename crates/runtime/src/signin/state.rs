use crate::*;
const MAX_STRING: usize = 2 * 1024 * 1024;
fn string<'a>(v: &'a Value, nonempty: bool) -> Result<&'a str> {
    let s = v
        .as_str()
        .ok_or_else(|| anyhow::anyhow!("invalid login-state string"))?;
    ensure!(
        (!nonempty || !s.is_empty()) && s.encode_utf16().count() <= MAX_STRING,
        "invalid login-state string length"
    );
    Ok(s)
}
fn allowed_domain(domain: &str) -> Option<(String, bool)> {
    let include = domain.starts_with('.');
    let hostname = domain
        .strip_prefix('.')
        .unwrap_or(domain)
        .to_ascii_lowercase();
    if hostname.is_empty()
        || hostname.starts_with('.')
        || hostname.ends_with('.')
        || hostname.contains("..")
        || !hostname
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
    {
        return None;
    }
    let url = reqwest::Url::parse(&format!("https://{hostname}/")).ok()?;
    if url.host_str() != Some(&hostname)
        || url.port().is_some()
        || !(hostname == "chatgpt.com"
            || hostname.ends_with(".chatgpt.com")
            || hostname == "openai.com"
            || hostname.ends_with(".openai.com"))
    {
        return None;
    }
    Some((hostname, include))
}
/// Sensitive validated state. Debug intentionally never exposes cookies or local storage.
pub struct ValidatedLoginState {
    storage: Value,
    electron: Value,
}
impl std::fmt::Debug for ValidatedLoginState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ValidatedLoginState([redacted])")
    }
}
impl ValidatedLoginState {
    /// Only pass to a browser session which the caller explicitly owns. Never log this value.
    pub fn storage_state(&self) -> &Value {
        &self.storage
    }
    /// Upstream Electron-style portable cookie/localStorage mapping, also sensitive.
    pub fn portable_state(&self) -> &Value {
        &self.electron
    }
    pub fn cookie_count(&self) -> usize {
        self.storage["cookies"].as_array().unwrap().len()
    }
    pub fn local_storage_count(&self) -> usize {
        self.electron["localStorage"].as_array().unwrap().len()
    }
}
pub fn validate_passkey_login_state(value: &Value) -> Result<ValidatedLoginState> {
    ensure!(value.is_object(), "invalid login storage-state object");
    let cookies = value["cookies"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("invalid cookie collection"))?;
    let origins = value["origins"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("invalid origin collection"))?;
    ensure!(
        cookies.len() <= 4096 && origins.len() <= 128,
        "login-state collection limit"
    );
    let mut portable = Vec::new();
    let mut storage = Vec::new();
    let mut local = Vec::new();
    for cookie in cookies {
        ensure!(cookie.is_object(), "invalid login cookie");
        if cookie.get("partitionKey").is_some() {
            continue;
        }
        let domain = string(&cookie["domain"], true)?;
        let Some((hostname, include)) = allowed_domain(domain) else {
            continue;
        };
        let name = string(&cookie["name"], true)?;
        let value = string(&cookie["value"], false)?;
        let path = string(&cookie["path"], true)?;
        ensure!(
            path.starts_with('/')
                && !path
                    .chars()
                    .any(|c| c <= '\u{1f}' || c == '\u{7f}' || c == '?' || c == '#'),
            "invalid login cookie path"
        );
        let secure = cookie["secure"]
            .as_bool()
            .ok_or_else(|| anyhow::anyhow!("invalid cookie security flags"))?;
        let http_only = cookie["httpOnly"]
            .as_bool()
            .ok_or_else(|| anyhow::anyhow!("invalid cookie security flags"))?;
        let same = string(&cookie["sameSite"], true)?;
        let mapped = match same {
            "Strict" => "strict",
            "Lax" => "lax",
            "None" => "no_restriction",
            _ => bail!("invalid cookie SameSite"),
        };
        let expires = cookie["expires"]
            .as_f64()
            .filter(|v| v.is_finite())
            .ok_or_else(|| anyhow::anyhow!("invalid cookie expiry"))?;
        let mut out = json!({"url":reqwest::Url::parse(&format!("https://{hostname}{path}"))?.as_str(),"name":name,"value":value,"path":path,"secure":secure,"httpOnly":http_only,"sameSite":mapped});
        if include {
            out["domain"] = format!(".{hostname}").into();
        }
        if expires > 0. {
            out["expirationDate"] = expires.into();
        }
        portable.push(out);
        storage.push(json!({"name":name,"value":value,"domain":if include{format!(".{hostname}")}else{hostname},"path":path,"secure":secure,"httpOnly":http_only,"sameSite":same,"expires":expires}));
    }
    ensure!(
        !storage.is_empty(),
        "no ChatGPT/OpenAI cookies in login capture"
    );
    for origin in origins {
        ensure!(
            origin.is_object() && origin["origin"].is_string(),
            "invalid login origin state"
        );
        if origin["origin"] != "https://chatgpt.com" {
            continue;
        }
        let entries = origin["localStorage"]
            .as_array()
            .ok_or_else(|| anyhow::anyhow!("invalid ChatGPT local storage"))?;
        ensure!(
            entries.len() <= 4096 && local.len() + entries.len() <= 4096,
            "local-storage collection limit"
        );
        for entry in entries {
            ensure!(entry.is_object(), "invalid local-storage entry");
            local.push(json!({"name":string(&entry["name"],false)?,"value":string(&entry["value"],false)?}));
        }
    }
    let storage = json!({"cookies":storage,"origins":[{"origin":"https://chatgpt.com","localStorage":local}]});
    ensure!(
        serde_json::to_vec(&storage)?.len() <= 16 * 1024 * 1024,
        "login capture exceeds 16 MiB"
    );
    Ok(ValidatedLoginState {
        storage,
        electron: json!({"cookies":portable,"localStorage":local}),
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    pub(super) fn cookie(domain: &str) -> Value {
        json!({"name":"fixture","value":"fixture-only-secret","domain":domain,"path":"/","expires":-1,"secure":true,"httpOnly":true,"sameSite":"Lax"})
    }
    #[test]
    fn source_allowlist_and_host_only_mapping() -> Result<()> {
        let mut partition = cookie(".chatgpt.com");
        partition["partitionKey"] = "https://accounts.google.com".into();
        let value = json!({"cookies":[cookie(".chatgpt.com"),cookie("auth.openai.com"),partition,cookie(".accounts.google.com"),cookie("..chatgpt.com"),cookie("attacker@chatgpt.com"),cookie("chatgpt.com:443"),cookie(".chatgpt.com.evil")],"origins":[{"origin":"https://chatgpt.com","localStorage":[{"name":"fixture","value":"secret"}]},{"origin":"https://auth.openai.com","localStorage":[]}]});
        let state = validate_passkey_login_state(&value)?;
        assert_eq!(state.cookie_count(), 2);
        assert_eq!(state.local_storage_count(), 1);
        assert!(state.portable_state()["cookies"][1].get("domain").is_none());
        assert!(!format!("{state:?}").contains("secret"));
        Ok(())
    }
    #[test]
    fn malformed_allowed_cookie_and_limits_fail_closed() {
        for (key, value) in [
            ("path", json!("relative")),
            ("path", json!("/?secret")),
            ("sameSite", json!("unknown")),
            ("expires", Value::Null),
            ("secure", json!(1)),
        ] {
            let mut c = cookie(".chatgpt.com");
            c[key] = value;
            assert!(validate_passkey_login_state(&json!({"cookies":[c],"origins":[]})).is_err());
        }
        assert!(
            validate_passkey_login_state(&json!({"cookies":[cookie(".google.com")],"origins":[]}))
                .is_err()
        );
        assert!(
            validate_passkey_login_state(
                &json!({"cookies":vec![cookie(".chatgpt.com");4097],"origins":[]})
            )
            .is_err()
        );
    }
}
