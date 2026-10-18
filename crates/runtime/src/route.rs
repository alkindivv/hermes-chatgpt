//! Native built-in OpenAI route: Responses route locally; Voice bypasses the bridge.
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preserves_native_provider_and_per_key_restore() -> anyhow::Result<()> {
        let t = tempfile::tempdir()?;
        let p = crate::Profile::isolated(std::fs::canonicalize(t.path())?.join("profile"))?;
        let prior=b"\xef\xbb\xbfmodel_provider = 'openai' # native\r\nmodel = 'native-model'\r\n[features]\r\nfoo = true\r\n";
        std::fs::write(p.codex_home().join("config.toml"), prior)?;
        let j = crate::IntegrationJournal::new(p.clone());
        j.install_native_route("http://127.0.0.1:9999")?;
        let path = p.codex_home().join("config.toml");
        let text = std::fs::read_to_string(&path)?;
        assert!(text.contains("openai_base_url = \"http://127.0.0.1:9999/v1\""));
        assert!(text.contains("model_provider = 'openai' # native"));
        std::fs::write(&path, text.replace("foo = true", "foo = false"))?;
        j.restore()?;
        assert_eq!(
            std::fs::read(&path)?,
            String::from_utf8(prior.to_vec())?
                .replace("foo = true", "foo = false")
                .as_bytes()
        );
        Ok(())
    }
    #[test]
    fn native_route_conflict_and_versions() -> anyhow::Result<()> {
        assert!(compile_native_route("model_provider='custom'\n", "http://127.0.0.1:1").is_err());
        assert!(
            compile_native_route("openai_base_url='https://other'\n", "http://127.0.0.1:1")
                .is_err()
        );
        assert!(compile_native_route("", "https://example.com").is_err());
        Ok(())
    }
}
use crate::{IntegrationJournal, Journal, read_optional};
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
const MARKER: &str =
    "# Managed by codex-chatgpt-web: Responses use the local bridge; Voice stays on ChatGPT.";
const VOICE: &str = "https://chatgpt.com/backend-api/codex";
#[derive(Serialize, Deserialize)]
pub(crate) struct ManagedAssignment {
    key: String,
    installed_line: String,
    previous_line: Option<String>,
}
fn parsed(text: &str) -> Result<toml_edit::DocumentMut> {
    Ok(text.trim_start_matches('\u{feff}').parse()?)
}
fn top_range(text: &str, key: &str) -> Option<std::ops::Range<usize>> {
    let body = text.trim_start_matches('\u{feff}');
    let doc = toml_edit::ImDocument::parse(body).ok()?;
    let item = doc.get(key)?;
    let span = item.as_value()?.span()?;
    if body[span.clone()].contains('\n') {
        return None;
    }
    let start = body[..span.start].rfind('\n').map_or(0, |i| i + 1);
    let end = body[span.end..]
        .find('\n')
        .map_or(body.len(), |i| span.end + i + 1);
    let offset = text.len() - body.len();
    Some(start + offset..end + offset)
}
fn top_line(text: &str, key: &str) -> Option<String> {
    top_range(text, key).map(|r| text[r].to_owned())
}
fn compile(text: &str, endpoint: &str) -> Result<(String, Vec<ManagedAssignment>)> {
    let endpoint = crate::mcp::loopback_endpoint(endpoint)?;
    ensure!(
        endpoint.path() == "/" || endpoint.path() == "/v1",
        "bridge endpoint must be root or /v1"
    );
    let doc = parsed(text)?;
    ensure!(
        doc.get("model_provider")
            .is_none_or(|v| v.as_str() == Some("openai")),
        "custom provider bypasses built-in routing"
    );
    ensure!(
        !doc.contains_key("openai_base_url"),
        "existing route conflict"
    );
    ensure!(
        doc.get("experimental_realtime_webrtc_call_base_url")
            .is_none_or(|v| v.as_str() == Some(VOICE)),
        "existing Voice route conflict"
    );
    ensure!(!text.contains(MARKER), "untracked managed marker");
    let eol = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let bom = if text.starts_with('\u{feff}') {
        "\u{feff}"
    } else {
        ""
    };
    let mut body = text.trim_start_matches('\u{feff}').to_owned();
    let mut prefix = format!("{MARKER}{eol}");
    let mut keys = Vec::new();
    for (key, value) in [
        (
            "openai_base_url",
            format!("{}/v1", endpoint.origin().ascii_serialization()),
        ),
        ("experimental_realtime_webrtc_call_base_url", VOICE.into()),
    ] {
        let previous_line = top_line(text, key);
        ensure!(
            !doc.contains_key(key) || previous_line.is_some(),
            "managed assignment must be a single top-level line"
        );
        let installed_line = format!("{key} = {}{eol}", serde_json::to_string(&value)?);
        if previous_line.is_some() {
            let range = top_range(&body, key)
                .ok_or_else(|| anyhow::anyhow!("managed assignment span missing"))?;
            body.replace_range(range, &installed_line);
        } else {
            prefix.push_str(&installed_line);
        }
        keys.push(ManagedAssignment {
            key: key.into(),
            installed_line,
            previous_line,
        });
    }
    Ok((format!("{bom}{prefix}{body}"), keys))
}
/// Produces the native built-in provider route, without selecting a custom model provider.
pub fn compile_native_route(text: &str, endpoint: &str) -> Result<String> {
    Ok(compile(text, endpoint)?.0)
}
impl IntegrationJournal {
    pub fn install_native_route(&self, endpoint: &str) -> Result<()> {
        self.locked(|| {
            ensure!(
                !self.profile.path("runtime/integration.json")?.exists(),
                "restore existing journal first"
            );
            let previous = read_optional(&self.profile.path("codex-home/config.toml")?)?;
            let baseline = std::str::from_utf8(previous.as_deref().unwrap_or_default())?;
            let (installed, managed) = compile(baseline, endpoint)?;
            self.profile.write_json(
                "runtime/integration.json",
                &Journal {
                    version: 1,
                    managed: Some(managed),
                    previous,
                    installed: installed.as_bytes().to_vec(),
                },
            )?;
            self.profile
                .write("codex-home/config.toml", installed.as_bytes())
        })
    }
}
pub(crate) fn restore_native(
    journal: &IntegrationJournal,
    j: &Journal,
    current: Option<&[u8]>,
) -> Result<()> {
    let profile = &journal.profile;
    if current == j.previous.as_deref() || current == Some(j.installed.as_slice()) {
        match j.previous {
            Some(ref bytes) => profile.write("codex-home/config.toml", bytes)?,
            None => {
                if current.is_some() {
                    std::fs::remove_file(profile.path("codex-home/config.toml")?)?;
                }
            }
        }
    } else {
        let mut text = std::str::from_utf8(
            current.ok_or_else(|| anyhow::anyhow!("managed config disappeared"))?,
        )?
        .to_owned();
        let doc = parsed(&text)?;
        ensure!(
            doc.get("model_provider")
                .is_none_or(|v| v.as_str() == Some("openai")),
            "provider changed after install"
        );
        for key in j.managed.as_ref().unwrap() {
            ensure!(
                top_line(&text, &key.key).as_deref() == Some(&key.installed_line),
                "managed key changed: {}",
                key.key
            );
        }
        for key in j.managed.as_ref().unwrap() {
            let range = top_range(&text, &key.key)
                .ok_or_else(|| anyhow::anyhow!("managed key span missing"))?;
            text.replace_range(range, key.previous_line.as_deref().unwrap_or(""));
        }
        let marker = if j.installed.windows(2).any(|v| v == b"\r\n") {
            format!("{MARKER}\r\n")
        } else {
            format!("{MARKER}\n")
        };
        ensure!(text.matches(&marker).count() == 1, "managed marker changed");
        text = text.replacen(&marker, "", 1);
        parsed(&text)?;
        profile.write("codex-home/config.toml", text.as_bytes())?;
    }
    std::fs::remove_file(profile.path("runtime/integration.json")?)?;
    Ok(())
}
/// Dynamic wire catalog from an actual native catalog; no static model_catalog_json setting.
pub fn dynamic_catalog(
    native: &Value,
    capabilities: &cw_contracts::models::ChatGptWebAccountCapabilities,
    full_mode: bool,
    context_override: Option<u64>,
) -> Result<Value> {
    Ok(cw_contracts::models::augment_native_model_catalog(
        native,
        capabilities,
        full_mode,
        false,
        context_override,
    )?)
}
#[cfg(test)]
mod safety_tests {
    use super::*;
    #[test]
    fn owned_key_edit_and_future_journal_refused() -> Result<()> {
        let t = tempfile::tempdir()?;
        let p = crate::Profile::isolated(std::fs::canonicalize(t.path())?.join("owned"))?;
        let j = IntegrationJournal::new(p.clone());
        j.install_native_route("http://127.0.0.1:9001")?;
        j.restore()?;
        assert!(!p.codex_home().join("config.toml").exists());
        j.install_native_route("http://127.0.0.1:9001")?;
        let path = p.codex_home().join("config.toml");
        let text = std::fs::read_to_string(&path)?.replace(":9001/v1", ":9002/v1");
        std::fs::write(&path, &text)?;
        assert!(j.restore().is_err());
        assert_eq!(std::fs::read_to_string(&path)?, text);
        let mut journal: Value = p.read_json("runtime/integration.json")?;
        journal["version"] = 999.into();
        p.write_json("runtime/integration.json", &journal)?;
        assert!(j.restore().unwrap_err().to_string().contains("version"));
        Ok(())
    }
    #[test]
    fn dynamic_catalog_keeps_native_rows() -> Result<()> {
        let native = serde_json::json!({"models":[{"slug":"gpt-native","visibility":"list","tool_mode":"function","supported_reasoning_levels":[{"effort":"low","description":"Low"}],"multi_agent_version":"v2","priority":0}],"etag":"fixture"});
        let augmented = dynamic_catalog(
            &native,
            &cw_contracts::models::ChatGptWebAccountCapabilities::default(),
            true,
            None,
        )?;
        assert_eq!(augmented["models"][0], native["models"][0]);
        assert_eq!(augmented["etag"], "fixture");
        assert!(augmented["models"].as_array().unwrap().len() > 1);
        Ok(())
    }
}
#[cfg(test)]
mod multiline_safety {
    use super::*;
    #[test]
    fn decoy_assignment_inside_unrelated_multiline_is_preserved() -> Result<()> {
        let t = tempfile::tempdir()?;
        let p = crate::Profile::isolated(std::fs::canonicalize(t.path())?.join("owned"))?;
        let journal = IntegrationJournal::new(p.clone());
        journal.install_native_route("http://127.0.0.1:9001")?;
        let path = p.codex_home().join("config.toml");
        let installed = std::fs::read_to_string(&path)?;
        let notes = "notes = '''\nopenai_base_url = \"http://127.0.0.1:9001/v1\"\n'''\n";
        std::fs::write(&path, format!("{notes}{installed}"))?;
        journal.restore()?;
        assert_eq!(std::fs::read_to_string(path)?, notes);
        Ok(())
    }
}
