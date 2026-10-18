//! Owned official tunnel installation and foreground supervision.
use crate::*;
use sha2::{Digest, Sha256};
use std::io::{Cursor, Read};
use tokio::io::AsyncReadExt;
const MAX_DOWNLOAD: usize = 100 * 1024 * 1024;
const MAX_UNPACKED: u64 = 400 * 1024 * 1024;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct McpSetupResult {
    pub ready: bool,
    pub health_verified_at: String,
    pub pid: u32,
    pub health_endpoint: String,
    pub profile_dir: PathBuf,
    pub profile_name: String,
    pub mode: String,
    pub connector_name: String,
    pub version: String,
}
pub(crate) struct OwnedMcp {
    child: ChildProcess,
    result: McpSetupResult,
    _lease: InstanceLease,
}
#[derive(Clone, Serialize, Deserialize)]
struct SavedMcp {
    version: u32,
    instance: String,
    tunnel_id: String,
    endpoint: String,
    mode: String,
    connector_name: String,
    profile_sha256: String,
    key_sha256: String,
    manifest_sha256: String,
}
struct InstanceLease {
    profile: Profile,
    marker: String,
    _file: fs::File,
}
impl InstanceLease {
    fn acquire(profile: &Profile, base: &str) -> Result<Self> {
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(profile.path(format!("{base}/owner.lock"))?)?;
        fs2::FileExt::try_lock_exclusive(&lock).map_err(|_| {
            anyhow::anyhow!("saved MCP instance is already owned by another runtime")
        })?;
        let marker = format!("{base}/owner-active.json");
        ensure!(
            !profile.path(&marker)?.exists(),
            "unclean MCP owner exit; refusing possible orphan duplication"
        );
        profile.write_json(&marker, &json!({"version":1,"runId":uuid::Uuid::new_v4()}))?;
        Ok(Self {
            profile: profile.clone(),
            marker,
            _file: lock,
        })
    }
}
impl Drop for InstanceLease {
    fn drop(&mut self) {
        if let Ok(path) = self.profile.path(&self.marker) {
            let _ = fs::remove_file(path);
        }
    }
}
fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
fn bundle_hashes(
    profile: &Profile,
    base: &str,
) -> Result<std::collections::BTreeMap<String, String>> {
    let mut hashes = std::collections::BTreeMap::new();
    for entry in fs::read_dir(profile.path(format!("{base}/bin"))?)? {
        let entry = entry?;
        let name = entry
            .file_name()
            .into_string()
            .map_err(|_| anyhow::anyhow!("non UTF-8 asset name"))?;
        hashes.insert(
            name.clone(),
            digest(&fs::read(profile.path(format!("{base}/bin/{name}"))?)?),
        );
    }
    Ok(hashes)
}
impl OwnedMcp {
    pub(crate) fn stop(&mut self) -> Result<()> {
        self.child.stop()
    }
}
pub(crate) fn loopback_endpoint(value: &str) -> Result<reqwest::Url> {
    let url = reqwest::Url::parse(value)?;
    ensure!(
        url.scheme() == "http"
            && matches!(url.host_str(), Some("127.0.0.1" | "[::1]" | "::1"))
            && url.port().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "explicit loopback HTTP endpoint required"
    );
    Ok(url)
}
fn validate_request(id: &str, key: &[u8], endpoint: &str, mode: &str, name: &str) -> Result<()> {
    ensure!(
        id.len() == 39
            && id.starts_with("tunnel_")
            && id[7..]
                .bytes()
                .all(|v| v.is_ascii_digit() || (b'a'..=b'f').contains(&v)),
        "invalid tunnel ID"
    );
    ensure!(
        !key.is_empty() && key.len() <= 64 * 1024,
        "explicit key required"
    );
    loopback_endpoint(endpoint)?;
    ensure!(
        matches!(mode, "automatic" | "manual"),
        "invalid browser mode"
    );
    ensure!(
        !name.trim().is_empty() && name.chars().count() <= 80,
        "explicit connector name required"
    );
    Ok(())
}
fn platform_asset() -> Result<String> {
    let os = match std::env::consts::OS {
        "macos" => "darwin",
        "linux" => "linux",
        _ => bail!("unsupported tunnel platform"),
    };
    let arch = match std::env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "amd64",
        _ => bail!("unsupported tunnel architecture"),
    };
    Ok(format!("tunnel-client-v{TUNNEL_VERSION}-{os}-{arch}.zip"))
}
async fn download(client: &reqwest::Client, url: &str, cap: usize) -> Result<Vec<u8>> {
    let mut response = client.get(url).send().await?.error_for_status()?;
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        ensure!(
            bytes.len() + chunk.len() <= cap,
            "download exceeds size limit"
        );
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}
fn unpack(profile: &Profile, base: &str, archive: &[u8]) -> Result<PathBuf> {
    let mut zip = zip::ZipArchive::new(Cursor::new(archive))?;
    let mut total = 0u64;
    let mut executable = None;
    let mut names = std::collections::HashSet::new();
    for i in 0..zip.len() {
        let mut file = zip.by_index(i)?;
        let enclosed = file
            .enclosed_name()
            .ok_or_else(|| anyhow::anyhow!("archive traversal"))?
            .to_path_buf();
        ensure!(
            file.unix_mode().is_none_or(|m| m & 0o170000 != 0o120000),
            "archive symlink forbidden"
        );
        if file.is_dir() {
            continue;
        }
        total = total
            .checked_add(file.size())
            .ok_or_else(|| anyhow::anyhow!("archive size overflow"))?;
        ensure!(total <= MAX_UNPACKED, "archive expansion limit");
        // Flatten only regular release assets into the UUID-owned binary directory.
        let name = enclosed
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("archive entry has no filename"))?
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("non UTF-8 archive name"))?;
        ensure!(names.insert(name.to_owned()), "duplicate archive basename");
        let mut bytes = Vec::new();
        (&mut file).take(MAX_UNPACKED + 1).read_to_end(&mut bytes)?;
        ensure!(
            bytes.len() as u64 <= file.size(),
            "archive entry size mismatch"
        );
        let path = format!("{base}/bin/{name}");
        profile.write(&path, &bytes)?;
        if name == "tunnel-client" || name == "cloudflared" {
            fs::set_permissions(profile.path(&path)?, fs::Permissions::from_mode(0o700))?;
        }
        if name == "tunnel-client" {
            executable = Some(profile.path(&path)?);
        }
    }
    executable.ok_or_else(|| anyhow::anyhow!("release archive has no native tunnel-client"))
}
async fn check_version(profile: &Profile, binary: &Path) -> Result<()> {
    let mut command = tokio::process::Command::new(binary);
    command
        .arg("--version")
        .env_clear()
        .env("HOME", profile.root())
        .current_dir(profile.root())
        .kill_on_drop(true)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn()?;
    let stdout = child.stdout.take().unwrap();
    let stderr = child.stderr.take().unwrap();
    let task = async {
        let out = async {
            let mut b = Vec::new();
            stdout.take(65537).read_to_end(&mut b).await?;
            Ok::<_, std::io::Error>(b)
        };
        let err = async {
            let mut b = Vec::new();
            stderr.take(65537).read_to_end(&mut b).await?;
            Ok::<_, std::io::Error>(b)
        };
        let (out, err) = tokio::try_join!(out, err)?;
        ensure!(
            out.len() <= 65536 && err.len() <= 65536,
            "version output too large"
        );
        ensure!(
            child.wait().await?.success(),
            "tunnel version command failed"
        );
        let output = format!(
            "{} {}",
            String::from_utf8_lossy(&out),
            String::from_utf8_lossy(&err)
        );
        ensure!(
            output
                .split_whitespace()
                .any(|word| word.trim_start_matches('v') == TUNNEL_VERSION),
            "tunnel version is not pinned v0.0.16"
        );
        Ok(())
    };
    tokio::time::timeout(Duration::from_secs(10), task)
        .await
        .map_err(|_| anyhow::anyhow!("version check timed out"))?
}
fn foreground_invocation(
    binary: &Path,
    config: &TunnelConfig,
    health_file: &Path,
) -> Result<Invocation> {
    Invocation::new(
        binary,
        vec![
            "run".into(),
            "--profile-dir".into(),
            config.profile_dir.to_string_lossy().into_owned(),
            "--profile".into(),
            config.profile_name.clone(),
            "--health.listen-addr".into(),
            "127.0.0.1:0".into(),
            "--health.url-file".into(),
            health_file.to_string_lossy().into_owned(),
        ],
    )
}
async fn ready(client: &reqwest::Client, endpoint: &str) -> Result<bool> {
    for path in ["/healthz", "/readyz"] {
        if !client
            .get(format!("{endpoint}{path}"))
            .send()
            .await?
            .status()
            .is_success()
        {
            return Ok(false);
        }
    }
    let bytes = download(
        client,
        &format!("{endpoint}/api/logs?limit=100"),
        1024 * 1024,
    )
    .await?;
    let logs: Value = serde_json::from_slice(&bytes)?;
    let events = logs["events"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("MCP diagnostics have no events array"))?;
    let cutoff = chrono::Utc::now() - chrono::Duration::minutes(2);
    for event in events {
        let recent = event["time"]
            .as_str()
            .and_then(|v| chrono::DateTime::parse_from_rfc3339(v).ok())
            .is_some_and(|t| t >= cutoff);
        let attrs = &event["attrs"];
        ensure!(
            !(recent
                && event["message"]
                    == "dispatcher received MCP upstream error; posted error response to control plane"
                && attrs["failure_source"] == "client_internal"
                && attrs["status_code"] == 502
                && attrs["upstream_response_received"] == false
                && matches!(
                    attrs["rpc_method"].as_str(),
                    Some("initialize" | "tools/call")
                )),
            "recent fatal MCP transport failure"
        );
    }
    Ok(true)
}
impl RuntimeFacade {
    /// Downloads only the pinned official release. Never call without explicit fresh credentials.
    pub async fn configure_install_start_mcp(
        &mut self,
        tunnel_id: &str,
        key: &[u8],
        endpoint: &str,
        mode: &str,
        name: &str,
    ) -> Result<McpSetupResult> {
        validate_request(tunnel_id, key, endpoint, mode, name)?;
        ensure!(self.mcp.is_none(), "stop owned MCP instance before setup");
        let client = reqwest::Client::builder()
            .no_proxy()
            .https_only(true)
            .timeout(Duration::from_secs(120))
            .build()?;
        let asset = platform_asset()?;
        let archive_url = format!("{TUNNEL_RELEASE_BASE}/{asset}");
        let sums_url = format!("{TUNNEL_RELEASE_BASE}/SHA256SUMS.txt");
        let (archive, sums) = tokio::try_join!(
            download(&client, &archive_url, MAX_DOWNLOAD),
            download(&client, &sums_url, 1024 * 1024)
        )?;
        self.configure_archive(
            tunnel_id,
            key,
            endpoint,
            mode,
            name,
            &asset,
            &archive,
            std::str::from_utf8(&sums)?,
        )
        .await
    }
    async fn configure_archive(
        &mut self,
        id: &str,
        key: &[u8],
        endpoint: &str,
        mode: &str,
        name: &str,
        asset: &str,
        archive: &[u8],
        sums: &str,
    ) -> Result<McpSetupResult> {
        validate_request(id, key, endpoint, mode, name)?;
        ensure!(self.mcp.is_none(), "owned MCP already active");
        ensure!(archive.len() <= MAX_DOWNLOAD, "archive too large");
        verify_tunnel_archive(asset, archive, sums)?;
        let config = self.configure_tunnel(id, key)?;
        let base = config
            .profile_dir
            .strip_prefix(self.profile.root())?
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("profile path is not UTF-8"))?
            .to_owned();
        let binary = unpack(&self.profile, &base, archive)?;
        let health_file = self.profile.path(format!("{base}/health.url"))?;
        // JSON is a YAML 1.2 subset. This is the pinned official named-profile schema.
        self.profile.write_json(format!("{base}/{}.yaml",config.profile_name),&json!({"config_version":1,"control_plane":{"tunnel_id":id,"api_key":format!("file:{}",config.runtime_key_file.display())},"mcp":{"server_urls":[{"channel":"main","url":endpoint}]},"health":{"listen_addr":"127.0.0.1:0","url_file":health_file},"admin_ui":{"open_browser":false}}))?;
        let hashes = bundle_hashes(&self.profile, &base)?;
        self.profile.write_json(format!("{base}/bin/tunnel-client-manifest.json"),&json!({"version":1,"tunnelClientVersion":TUNNEL_VERSION,"asset":asset,"archiveSha256":digest(archive),"binarySha256":digest(&fs::read(&binary)?),"files":hashes}))?;
        let saved = SavedMcp {
            version: 1,
            instance: config
                .profile_dir
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned(),
            tunnel_id: id.into(),
            endpoint: endpoint.into(),
            mode: mode.into(),
            connector_name: name.into(),
            profile_sha256: digest(&fs::read(
                self.profile
                    .path(format!("{base}/{}.yaml", config.profile_name))?,
            )?),
            key_sha256: digest(key),
            manifest_sha256: digest(&fs::read(
                self.profile
                    .path(format!("{base}/bin/tunnel-client-manifest.json"))?,
            )?),
        };
        self.launch_saved(saved).await
    }
    fn validate_saved(&self, saved: &SavedMcp) -> Result<(TunnelConfig, PathBuf, PathBuf)> {
        ensure!(saved.version == 1, "unsupported saved MCP schema");
        let id = uuid::Uuid::parse_str(&saved.instance)?;
        ensure!(id.to_string() == saved.instance, "noncanonical instance ID");
        let base = format!("tunnel/{}", saved.instance);
        let config = TunnelConfig {
            tunnel_id: saved.tunnel_id.clone(),
            profile_dir: self.profile.path(&base)?,
            runtime_key_file: self.profile.path(format!("{base}/runtime.key"))?,
            profile_name: format!("gpui-{}", saved.instance),
        };
        ensure!(
            fs::metadata(&config.runtime_key_file)?.permissions().mode() & 0o077 == 0,
            "saved credential file is not private"
        );
        ensure!(
            digest(&fs::read(&config.runtime_key_file)?) == saved.key_sha256,
            "saved MCP credential changed"
        );
        validate_request(
            &saved.tunnel_id,
            &fs::read(&config.runtime_key_file)?,
            &saved.endpoint,
            &saved.mode,
            &saved.connector_name,
        )?;
        let yaml = self
            .profile
            .path(format!("{base}/{}.yaml", config.profile_name))?;
        ensure!(
            digest(&fs::read(yaml)?) == saved.profile_sha256,
            "saved MCP profile changed"
        );
        let manifest_bytes = fs::read(
            self.profile
                .path(format!("{base}/bin/tunnel-client-manifest.json"))?,
        )?;
        ensure!(
            digest(&manifest_bytes) == saved.manifest_sha256,
            "saved binary manifest changed"
        );
        let manifest: Value = serde_json::from_slice(&manifest_bytes)?;
        ensure!(
            manifest["version"] == 1
                && manifest["tunnelClientVersion"] == TUNNEL_VERSION
                && manifest["asset"] == platform_asset()?,
            "unsupported saved binary manifest"
        );
        let files = manifest["files"]
            .as_object()
            .ok_or_else(|| anyhow::anyhow!("saved bundle hashes missing"))?;
        ensure!(
            files.contains_key("tunnel-client"),
            "saved executable hash missing"
        );
        for (name, hash) in files {
            ensure!(
                Path::new(name).components().count() == 1
                    && matches!(
                        Path::new(name).components().next(),
                        Some(Component::Normal(_))
                    ),
                "invalid saved asset name"
            );
            ensure!(
                hash.as_str()
                    == Some(&digest(&fs::read(
                        self.profile.path(format!("{base}/bin/{name}"))?
                    )?)),
                "saved binary asset changed"
            );
        }
        let binary = self.profile.path(format!("{base}/bin/tunnel-client"))?;
        ensure!(
            manifest["binarySha256"] == digest(&fs::read(&binary)?),
            "saved executable changed"
        );
        ensure!(
            fs::metadata(&binary)?.permissions().mode() & 0o111 != 0,
            "saved binary is not executable"
        );
        let health_file = self.profile.path(format!("{base}/health.url"))?;
        Ok((config, binary, health_file))
    }
    async fn launch_saved(&mut self, saved: SavedMcp) -> Result<McpSetupResult> {
        ensure!(self.mcp.is_none(), "stop current owned MCP child first");
        let (config, binary, health_file) = self.validate_saved(&saved)?;
        let base = format!("tunnel/{}", saved.instance);
        let lease = InstanceLease::acquire(&self.profile, &base)?;
        check_version(&self.profile, &binary).await?;
        match fs::remove_file(&health_file) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        let mut child = ChildProcess::spawn(
            &self.profile,
            &foreground_invocation(&binary, &config, &health_file)?,
        )?;
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(2))
            .build()?;
        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            ensure!(
                child.child.try_wait()?.is_none(),
                "owned MCP process exited before readiness"
            );
            if health_file.exists() {
                reject_symlinks(&health_file)?;
                let value = fs::read_to_string(&health_file)?;
                let url = loopback_endpoint(value.trim())?;
                ensure!(url.path() == "/", "health endpoint must have no path");
                let health = url.origin().ascii_serialization();
                match ready(&client, &health).await {
                    Ok(true) => {
                        ensure!(
                            child.child.try_wait()?.is_none(),
                            "owned MCP process exited during readiness probes"
                        );
                        let result = McpSetupResult {
                            ready: true,
                            health_verified_at: chrono::Utc::now().to_rfc3339(),
                            pid: child.pid(),
                            health_endpoint: health,
                            profile_dir: config.profile_dir,
                            profile_name: config.profile_name,
                            mode: saved.mode.clone(),
                            connector_name: saved.connector_name.clone(),
                            version: TUNNEL_VERSION.into(),
                        };
                        self.supervisor
                            .record("mcp-ready", "owned MCP health and transport verified");
                        let returned = result.clone();
                        self.profile.write_json("runtime/mcp-saved.json", &saved)?;
                        self.mcp = Some(OwnedMcp {
                            child,
                            result,
                            _lease: lease,
                        });
                        return Ok(returned);
                    }
                    Err(e) if e.to_string().contains("fatal MCP") => return Err(e),
                    _ => {}
                }
            }
            ensure!(Instant::now() < deadline, "owned MCP readiness timed out");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
    /// Fresh health result. Safe to call after moving this facade into a Tokio task.
    pub async fn verify_mcp(&mut self) -> Result<Value> {
        self.check_mcp_health().await
    }
    /// Fresh MCP status; equivalent to verify_mcp, never a descriptor-only success.
    pub async fn status(&mut self) -> Result<Value> {
        self.verify_mcp().await
    }

    /// Reconnect only this application's saved, hash-validated UUID-owned instance.
    pub async fn restart_saved_mcp(&mut self) -> Result<McpSetupResult> {
        let saved: SavedMcp = self.profile.read_json("runtime/mcp-saved.json")?;
        self.validate_saved(&saved)?;
        self.stop_mcp()?;
        self.launch_saved(saved).await
    }
    /// Revalidate owned process, healthz, readyz and MCP diagnostics before showing Connected.
    pub async fn check_mcp_health(&mut self) -> Result<Value> {
        let Some(owned) = self.mcp.as_mut() else {
            return Ok(json!({"ready":false,"state":"stopped"}));
        };
        if owned.child.child.try_wait()?.is_some() {
            self.mcp = None;
            return Ok(json!({"ready":false,"state":"stopped"}));
        }
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(2))
            .build()?;
        owned.result.ready = false;
        match ready(&client, &owned.result.health_endpoint).await {
            Ok(true) => {
                owned.result.ready = owned.child.child.try_wait()?.is_none();
                owned.result.health_verified_at = chrono::Utc::now().to_rfc3339();
            }
            Ok(false) => {}
            Err(error) => {
                self.supervisor
                    .record("mcp-health", &redact(&error.to_string()));
            }
        }
        self.mcp_status()
    }
    pub fn mcp_status(&mut self) -> Result<Value> {
        if let Some(owned) = self.mcp.as_mut() {
            if owned.child.child.try_wait()?.is_none() {
                return Ok(serde_json::to_value(&owned.result)?);
            }
            self.mcp = None;
        }
        Ok(json!({"ready":false,"state":"stopped"}))
    }
    pub fn stop_mcp(&mut self) -> Result<()> {
        self.supervisor.gate.begin()?;
        if let Some(owned) = self.mcp.as_mut() {
            owned.child.stop()?;
        }
        self.mcp = None;
        self.supervisor.gate.resume();
        self.supervisor
            .record("mcp-stop", "owned MCP child stopped");
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    fn fixture_archive(directory: &Path, health: &str) -> Result<Vec<u8>> {
        let source = directory.join("fixture.rs");
        let binary = directory.join("fixture-tunnel");
        let code=r#"use std::{env,fs,thread,time::Duration};fn main(){let a:Vec<String>=env::args().collect();if a.get(1).map(String::as_str)==Some("--version"){println!("tunnel-client 0.0.16");return;}assert_eq!(a[1],"run");for flag in ["--profile-dir","--profile","--health.listen-addr","--health.url-file"]{assert!(a.iter().any(|v|v==flag));}let n=a.iter().position(|v|v=="--health.url-file").unwrap();fs::write(&a[n+1],"HEALTH").unwrap();loop {thread::sleep(Duration::from_millis(50));}}"#.replace("HEALTH",health);
        fs::write(&source, code)?;
        let status = Command::new("rustc")
            .arg("--edition=2024")
            .arg(&source)
            .arg("-o")
            .arg(&binary)
            .status()?;
        ensure!(status.success(), "native fixture compilation failed");
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        zip.start_file(
            "release/tunnel-client",
            zip::write::FileOptions::default().unix_permissions(0o700),
        )?;
        zip.write_all(&fs::read(binary)?)?;
        zip.start_file(
            "release/fixture-companion.json",
            zip::write::FileOptions::default(),
        )?;
        zip.write_all(b"{\"fixture\":true}")?;
        Ok(zip.finish()?.into_inner())
    }
    async fn server(fatal: bool) -> Result<(String, tokio::task::JoinHandle<()>)> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let address = listener.local_addr()?;
        let handle = tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let mut bytes = [0; 4096];
                let n = stream.read(&mut bytes).await.unwrap_or(0);
                let request = String::from_utf8_lossy(&bytes[..n]);
                let body = if request.contains("/api/logs") {
                    if fatal {
                        json!({"events":[{"time":chrono::Utc::now().to_rfc3339(),"message":"dispatcher received MCP upstream error; posted error response to control plane","attrs":{"failure_source":"client_internal","status_code":502,"upstream_response_received":false,"rpc_method":"initialize"}}]}).to_string()
                    } else {
                        json!({"events":[]}).to_string()
                    }
                } else {
                    "ok".into()
                };
                let response = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                let _ = stream.write_all(response.as_bytes()).await;
            }
        });
        Ok((format!("http://{address}"), handle))
    }
    #[tokio::test]
    async fn native_fixture_install_start_and_fatal_health() -> Result<()> {
        let temp = tempfile::tempdir()?;
        let root = fs::canonicalize(temp.path())?;
        let (health, server) = server(false).await?;
        let archive = fixture_archive(&root, &health)?;
        let asset = platform_asset()?;
        let sums = format!("{}  {asset}", hex::encode(Sha256::digest(&archive)));
        let mut runtime = RuntimeFacade::new(Profile::isolated(root.join("owned"))?);
        assert!(
            runtime
                .configure_archive(
                    "tunnel_0123456789abcdef0123456789abcdef",
                    b"fixture-only-key",
                    "http://127.0.0.1:12345/mcp",
                    "automatic",
                    "Codex Fixture",
                    &asset,
                    b"tampered",
                    &sums
                )
                .await
                .is_err()
        );
        let result = runtime
            .configure_archive(
                "tunnel_0123456789abcdef0123456789abcdef",
                b"fixture-only-key",
                "http://127.0.0.1:12345/mcp",
                "automatic",
                "Codex Fixture",
                &asset,
                &archive,
                &sums,
            )
            .await?;
        assert!(result.ready);
        assert_eq!(runtime.check_mcp_health().await?["ready"], true);
        assert_eq!(runtime.mcp_status()?["pid"], result.pid);
        let profile = fs::read_to_string(
            result
                .profile_dir
                .join(format!("{}.yaml", result.profile_name)),
        )?;
        let profile: Value = serde_json::from_str(&profile)?;
        assert_eq!(profile["config_version"], 1);
        assert!(
            profile["control_plane"]["api_key"]
                .as_str()
                .unwrap()
                .starts_with("file:")
        );
        assert_eq!(profile["mcp"]["server_urls"][0]["channel"], "main");
        let guard = runtime.supervisor.gate.enter(Activity::Http)?;
        assert!(runtime.stop_mcp().is_err());
        drop(guard);
        server.abort();
        assert_eq!(runtime.check_mcp_health().await?["ready"], false);
        runtime.stop_mcp()?;
        assert_eq!(runtime.mcp_status()?["ready"], false);
        server.abort();
        let (bad_health, bad_server) = self::server(true).await?;
        let bad_archive = fixture_archive(&root, &bad_health)?;
        let sums = format!("{}  {asset}", hex::encode(Sha256::digest(&bad_archive)));
        let error = runtime
            .configure_archive(
                "tunnel_0123456789abcdef0123456789abcdef",
                b"fixture-only-key",
                "http://127.0.0.1:12345/mcp",
                "manual",
                "Codex Manual Fixture",
                &asset,
                &bad_archive,
                &sums,
            )
            .await
            .unwrap_err();
        assert!(error.to_string().contains("fatal MCP"));
        assert!(runtime.mcp.is_none());
        bad_server.abort();
        Ok(())
    }
    #[tokio::test]
    async fn version_and_endpoint_fail_closed() -> Result<()> {
        let t = tempfile::tempdir()?;
        let p = Profile::isolated(fs::canonicalize(t.path())?.join("owned"))?;
        assert!(check_version(&p, Path::new("/usr/bin/true")).await.is_err());
        for endpoint in [
            "https://127.0.0.1:9999",
            "http://example.com:99",
            "http://user@127.0.0.1:99",
            "http://127.0.0.1:99/?secret=x",
        ] {
            assert!(loopback_endpoint(endpoint).is_err());
        }
        Ok(())
    }
    #[tokio::test]
    async fn saved_owned_restart_and_tokio_move() -> Result<()> {
        let t = tempfile::tempdir()?;
        let root = fs::canonicalize(t.path())?;
        let (health, server) = server(false).await?;
        let archive = fixture_archive(&root, &health)?;
        let asset = platform_asset()?;
        let sums = format!("{}  {asset}", hex::encode(Sha256::digest(&archive)));
        let profile = Profile::isolated(root.join("owned"))?;
        let mut runtime = RuntimeFacade::new(profile.clone());
        let first = runtime
            .configure_archive(
                "tunnel_0123456789abcdef0123456789abcdef",
                b"fixture-only-key",
                "http://127.0.0.1:12345/mcp",
                "automatic",
                "Codex Fixture",
                &asset,
                &archive,
                &sums,
            )
            .await?;
        let (mut runtime, verified) = tokio::spawn(async move {
            let verified = runtime.verify_mcp().await;
            (runtime, verified)
        })
        .await?;
        assert_eq!(verified?["ready"], true);
        assert_eq!(runtime.status().await?["ready"], true);
        let mut other = RuntimeFacade::new(profile.clone());
        assert!(other.restart_saved_mcp().await.is_err());
        let busy = runtime.supervisor.gate.enter(Activity::Browser)?;
        assert!(runtime.restart_saved_mcp().await.is_err());
        drop(busy);
        let second = runtime.restart_saved_mcp().await?;
        assert_ne!(first.pid, second.pid);
        assert_eq!(first.profile_dir, second.profile_dir);
        drop(runtime); // orderly native application exit reaps the owned child
        let third = other.restart_saved_mcp().await?;
        assert_ne!(second.pid, third.pid);
        assert!(third.ready);
        other.stop_mcp()?;
        let marker = third.profile_dir.join("owner-active.json");
        fs::write(&marker, b"{\"version\":1}")?;
        assert!(
            other
                .restart_saved_mcp()
                .await
                .unwrap_err()
                .to_string()
                .contains("unclean")
        );
        fs::remove_file(marker)?;
        let key = third.profile_dir.join("runtime.key");
        let original_key = fs::read(&key)?;
        fs::write(&key, b"different-key")?;
        assert!(other.restart_saved_mcp().await.is_err());
        fs::write(&key, original_key)?;
        let companion = third.profile_dir.join("bin/fixture-companion.json");
        let original_companion = fs::read(&companion)?;
        fs::write(&companion, b"tampered-companion")?;
        assert!(other.restart_saved_mcp().await.is_err());
        fs::write(&companion, original_companion)?;
        let yaml = third
            .profile_dir
            .join(format!("{}.yaml", third.profile_name));
        let original = fs::read(&yaml)?;
        fs::write(&yaml, b"changed")?;
        assert!(other.restart_saved_mcp().await.is_err());
        fs::write(&yaml, original)?;
        let binary = third.profile_dir.join("bin/tunnel-client");
        let original = fs::read(&binary)?;
        fs::write(&binary, b"tampered")?;
        assert!(other.restart_saved_mcp().await.is_err());
        fs::write(&binary, original)?;
        let mut saved: Value = profile.read_json("runtime/mcp-saved.json")?;
        saved["instance"] = "../../outside".into();
        profile.write_json("runtime/mcp-saved.json", &saved)?;
        assert!(other.restart_saved_mcp().await.is_err());
        server.abort();
        Ok(())
    }
}
