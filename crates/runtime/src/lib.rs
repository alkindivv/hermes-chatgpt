//! Isolated native runtime. No PID adoption, shell invocation, or production configuration.
use anyhow::{Result, bail, ensure};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::{Value, json};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::{
    collections::VecDeque,
    fs::{self, OpenOptions},
    io::Write,
    path::{Component, Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

#[derive(Clone, Debug)]
pub struct Profile {
    root: PathBuf,
}
impl Profile {
    pub fn isolated(root: PathBuf) -> Result<Self> {
        ensure!(root.is_absolute(), "profile root must be absolute");
        ensure!(
            !root.components().any(|c| matches!(c, Component::ParentDir)),
            "parent traversal forbidden"
        );
        ensure!(
            !root.starts_with("/Applications"),
            "application installation is protected"
        );
        reject_symlinks(&root)?;
        if let Some(home) = std::env::var_os("HOME") {
            let home = PathBuf::from(home);
            for protected in [
                home.join(".codex"),
                home.join(".codex-chatgpt-web"),
                home.join("Library"),
            ] {
                ensure!(
                    !root.starts_with(&protected) && !protected.starts_with(&root),
                    "protected profile"
                );
            }
        }
        fs::create_dir_all(&root)?;
        let p = Self {
            root: fs::canonicalize(root)?,
        };
        fs::set_permissions(&p.root, fs::Permissions::from_mode(0o700))?;
        for name in ["codex-home", "browser", "user-data", "runtime", "tunnel"] {
            p.directory(name)?;
        }
        Ok(p)
    }
    pub fn default_isolated() -> Result<Self> {
        Self::isolated(
            PathBuf::from(std::env::var_os("HOME").ok_or_else(|| anyhow::anyhow!("HOME missing"))?)
                .join(".codex-web-gpui"),
        )
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn codex_home(&self) -> PathBuf {
        self.root.join("codex-home")
    }
    pub fn browser(&self) -> PathBuf {
        self.root.join("browser")
    }
    pub fn user_data(&self) -> PathBuf {
        self.root.join("user-data")
    }
    pub fn path(&self, relative: impl AsRef<Path>) -> Result<PathBuf> {
        let relative = relative.as_ref();
        ensure!(
            !relative.as_os_str().is_empty()
                && relative
                    .components()
                    .all(|c| matches!(c, Component::Normal(_))),
            "invalid owned relative path"
        );
        let path = self.root.join(relative);
        reject_symlinks(&path)?;
        Ok(path)
    }
    fn directory(&self, name: impl AsRef<Path>) -> Result<PathBuf> {
        let path = self.path(name)?;
        fs::create_dir_all(&path)?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700))?;
        Ok(path)
    }
    pub fn write_json<T: Serialize>(&self, name: impl AsRef<Path>, value: &T) -> Result<()> {
        self.write(name, &serde_json::to_vec_pretty(value)?)
    }
    pub fn read_json<T: DeserializeOwned>(&self, name: impl AsRef<Path>) -> Result<T> {
        Ok(serde_json::from_slice(&fs::read(self.path(name)?)?)?)
    }
    fn write(&self, name: impl AsRef<Path>, bytes: &[u8]) -> Result<()> {
        let path = self.path(name)?;
        let parent = path.parent().unwrap();
        if parent != self.root {
            self.directory(parent.strip_prefix(&self.root)?)?;
        }
        let tmp = parent.join(format!(".atomic-{}", uuid::Uuid::new_v4()));
        let result = (|| -> Result<()> {
            let mut f = OpenOptions::new()
                .write(true)
                .create_new(true)
                .mode(0o600)
                .open(&tmp)?;
            f.write_all(bytes)?;
            f.sync_all()?;
            reject_symlinks(&path)?;
            fs::rename(&tmp, &path)?;
            fs::File::open(parent)?.sync_all()?;
            Ok(())
        })();
        let _ = fs::remove_file(tmp);
        result
    }
}
fn reject_symlinks(path: &Path) -> Result<()> {
    let mut current = PathBuf::new();
    for c in path.components() {
        current.push(c);
        match fs::symlink_metadata(&current) {
            Ok(m) => ensure!(
                !m.file_type().is_symlink(),
                "symlink forbidden: {}",
                current.display()
            ),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
            Err(e) => return Err(e.into()),
        }
    }
    Ok(())
}
#[derive(Serialize, Deserialize)]
struct Journal {
    #[serde(default = "journal_version")]
    version: u32,
    #[serde(default)]
    managed: Option<Vec<route::ManagedAssignment>>,
    previous: Option<Vec<u8>>,
    installed: Vec<u8>,
}
fn journal_version() -> u32 {
    1
}
pub struct IntegrationJournal {
    profile: Profile,
}
impl IntegrationJournal {
    pub fn new(profile: Profile) -> Self {
        Self { profile }
    }
    fn locked<T>(&self, run: impl FnOnce() -> Result<T>) -> Result<T> {
        let f = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .mode(0o600)
            .open(self.profile.path("runtime/integration.lock")?)?;
        fs2::FileExt::lock_exclusive(&f)?;
        run()
    }
    pub fn install(&self, bytes: &[u8]) -> Result<()> {
        self.locked(|| {
            let config = self.profile.path("codex-home/config.toml")?;
            let journal = self.profile.path("runtime/integration.json")?;
            ensure!(
                !journal.exists(),
                "integration already journaled; restore first"
            );
            let previous = read_optional(&config)?;
            if let Some(ref prior) = previous {
                let text = std::str::from_utf8(prior)?.trim_start_matches('\u{feff}');
                let document = text.parse::<toml_edit::DocumentMut>()?;
                ensure!(
                    !document.contains_key("openai_base_url")
                        && !document.contains_key("model_provider")
                        && !document.contains_key("model_providers"),
                    "existing route conflict; refusing replacement"
                );
            }
            let text = std::str::from_utf8(bytes)?.trim_start_matches('\u{feff}');
            text.parse::<toml_edit::DocumentMut>()?;
            self.profile.write_json(
                "runtime/integration.json",
                &Journal {
                    version: 1,
                    managed: None,
                    previous,
                    installed: bytes.to_vec(),
                },
            )?;
            self.profile.write("codex-home/config.toml", bytes)
        })
    }
    pub fn restore(&self) -> Result<()> {
        self.locked(|| {
            let j: Journal = self.profile.read_json("runtime/integration.json")?;
            ensure!(j.version == 1, "unsupported owned journal version");
            let config = self.profile.path("codex-home/config.toml")?;
            let current = read_optional(&config)?;
            if j.managed.is_some() {
                return route::restore_native(self, &j, current.as_deref());
            }
            // A durable journal may precede installation, or restoration may precede journal removal.
            if current != j.previous {
                ensure!(
                    current.as_deref() == Some(j.installed.as_slice()),
                    "integration conflict; preserving current bytes"
                );
                match j.previous {
                    Some(ref bytes) => self.profile.write("codex-home/config.toml", bytes)?,
                    None => fs::remove_file(config)?,
                }
            }
            fs::remove_file(self.profile.path("runtime/integration.json")?)?;
            Ok(())
        })
    }
}
fn read_optional(path: &Path) -> Result<Option<Vec<u8>>> {
    match fs::read(path) {
        Ok(v) => Ok(Some(v)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

#[derive(Clone, Copy)]
pub enum Activity {
    Http,
    Browser,
}
#[derive(Default)]
struct Counts {
    http: usize,
    browser: usize,
    draining: bool,
}
#[derive(Clone, Default)]
pub struct DrainGate(Arc<Mutex<Counts>>);
pub struct ActivityGuard {
    gate: DrainGate,
    kind: Activity,
}
impl DrainGate {
    pub fn enter(&self, kind: Activity) -> Result<ActivityGuard> {
        let mut c = self.0.lock().unwrap();
        ensure!(!c.draining, "runtime draining");
        match kind {
            Activity::Http => c.http += 1,
            Activity::Browser => c.browser += 1,
        };
        Ok(ActivityGuard {
            gate: self.clone(),
            kind,
        })
    }
    pub fn snapshot(&self) -> Value {
        let c = self.0.lock().unwrap();
        json!({"activeHttp":c.http,"activeBrowser":c.browser,"draining":c.draining})
    }
    fn begin(&self) -> Result<()> {
        let mut c = self.0.lock().unwrap();
        ensure!(
            c.http == 0 && c.browser == 0,
            "busy: HTTP or browser work active"
        );
        c.draining = true;
        Ok(())
    }
    pub fn resume(&self) {
        self.0.lock().unwrap().draining = false;
    }
}
impl Drop for ActivityGuard {
    fn drop(&mut self) {
        let mut c = self.gate.0.lock().unwrap();
        match self.kind {
            Activity::Http => c.http -= 1,
            Activity::Browser => c.browser -= 1,
        }
    }
}
#[derive(Clone, Debug)]
pub struct Invocation {
    pub executable: PathBuf,
    pub args: Vec<String>,
}
impl Invocation {
    pub fn new(executable: impl Into<PathBuf>, args: Vec<String>) -> Result<Self> {
        let executable = executable.into();
        ensure!(
            executable.is_absolute() && executable.is_file(),
            "absolute existing executable required"
        );
        Ok(Self { executable, args })
    }
    pub fn packaged(resources: &Path, args: Vec<String>) -> Result<Self> {
        Self::new(resources.join("bin/codex-web-gpui-runtime"), args)
    }
}
pub struct ChildProcess {
    child: Child,
}
impl ChildProcess {
    fn spawn(profile: &Profile, inv: &Invocation) -> Result<Self> {
        let child = Command::new(&inv.executable)
            .args(&inv.args)
            .env_clear()
            .env("HOME", profile.root())
            .env("CODEX_HOME", profile.codex_home())
            .env("CODEX_CHATGPT_WEB_HOME", profile.root())
            .current_dir(profile.root())
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;
        Ok(Self { child })
    }
    pub fn pid(&self) -> u32 {
        self.child.id()
    }
    pub fn stop(&mut self) -> Result<()> {
        if self.child.try_wait()?.is_none() {
            self.child.kill()?;
        }
        self.child.wait()?;
        Ok(())
    }
}
impl Drop for ChildProcess {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub struct LogRecord {
    pub at: String,
    pub kind: String,
    pub message: String,
}
pub struct Supervisor {
    profile: Profile,
    child: Option<ChildProcess>,
    invocation: Option<Invocation>,
    restarts: VecDeque<Instant>,
    pub gate: DrainGate,
    pub records: VecDeque<LogRecord>,
}
impl Supervisor {
    pub fn new(profile: Profile) -> Self {
        Self {
            profile,
            child: None,
            invocation: None,
            restarts: VecDeque::new(),
            gate: DrainGate::default(),
            records: VecDeque::new(),
        }
    }
    fn record(&mut self, kind: &str, message: &str) {
        if self.records.len() == 256 {
            self.records.pop_front();
        }
        self.records.push_back(LogRecord {
            at: chrono::Utc::now().to_rfc3339(),
            kind: kind.into(),
            message: redact(message),
        });
        // Best effort diagnostics must not change lifecycle results.
        let _ = self
            .profile
            .write_json("runtime/operations.json", &self.records);
    }
    pub fn pid(&self) -> Option<u32> {
        self.child.as_ref().map(ChildProcess::pid)
    }
    pub fn start(&mut self, inv: Invocation) -> Result<()> {
        ensure!(self.child.is_none(), "owned child already running");
        self.gate.resume();
        self.child = Some(ChildProcess::spawn(&self.profile, &inv)?);
        self.invocation = Some(inv);
        self.record("start", "owned child spawned");
        Ok(())
    }
    pub fn poll(&mut self) -> Result<()> {
        if let Some(child) = self.child.as_mut() {
            if let Some(status) = child.child.try_wait()? {
                self.child = None;
                self.record("exit", &format!("owned child exited: {status}"));
                if !status.success() {
                    self.restart()?;
                }
            }
        }
        Ok(())
    }
    fn restart(&mut self) -> Result<()> {
        let now = Instant::now();
        while self
            .restarts
            .front()
            .is_some_and(|t| now.duration_since(*t) > Duration::from_secs(60))
        {
            self.restarts.pop_front();
        }
        if self.restarts.len() >= 5 {
            self.record("restart-limit", "five restarts in 60 seconds");
            bail!("restart budget exhausted");
        }
        self.restarts.push_back(now);
        if let Some(inv) = self.invocation.clone() {
            self.child = Some(ChildProcess::spawn(&self.profile, &inv)?);
            self.record("restart", "owned child restarted");
        }
        Ok(())
    }
    pub fn shutdown(&mut self) -> Result<()> {
        self.gate.begin()?;
        if let Some(child) = self.child.as_mut() {
            child.stop()?;
        }
        self.child = None;
        self.invocation = None;
        self.record("stop", "owned child stopped");
        Ok(())
    }
}
/// Conservative diagnostics: redact whole records carrying credential markers.
pub fn redact(value: &str) -> String {
    let lower = value.to_lowercase();
    if [
        "token",
        "secret",
        "password",
        "authorization",
        "api_key",
        "api-key",
        "credential",
        "bearer",
        "file:",
    ]
    .iter()
    .any(|k| lower.contains(k))
    {
        "[redacted]".into()
    } else {
        value.chars().take(4096).collect()
    }
}

pub struct RuntimeFacade {
    pub profile: Profile,
    pub supervisor: Supervisor,
    mcp: Option<mcp::OwnedMcp>,
}
impl RuntimeFacade {
    pub fn new(profile: Profile) -> Self {
        Self {
            supervisor: Supervisor::new(profile.clone()),
            profile,
            mcp: None,
        }
    }
    pub fn diagnostics(&self) -> Value {
        json!({"profile":self.profile.root(),"codexHome":self.profile.codex_home(),"browser":self.profile.browser(),"userData":self.profile.user_data(),"pid":self.supervisor.pid(),"activity":self.supervisor.gate.snapshot(),"records":self.supervisor.records,"service":{"supported":false,"installed":false}})
    }
    pub fn shutdown(&mut self) -> Result<()> {
        self.supervisor.gate.begin()?;
        if let Some(owned) = self.mcp.as_mut() {
            owned.stop()?;
        }
        self.mcp = None;
        self.supervisor.shutdown()
    }
    pub fn dispatch(&mut self, action: &str, data: Value) -> Result<Value> {
        match action {
            "diagnostics" => return Ok(self.diagnostics()),
            "config.read" => return self.profile.read_json("runtime/config.json"),
            "catalog.augment" => {
                let capabilities: cw_contracts::models::ChatGptWebAccountCapabilities =
                    serde_json::from_value(data["capabilities"].clone())?;
                return dynamic_catalog(
                    &data["native"],
                    &capabilities,
                    data["fullMode"].as_bool().unwrap_or(false),
                    data["contextWindow"].as_u64(),
                );
            }
            "config.write" => {
                validate_config(&data)?;
                self.profile.write_json("runtime/config.json", &data)?;
            }
            "integration.install" => IntegrationJournal::new(self.profile.clone()).install(
                data["document"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("document required"))?
                    .as_bytes(),
            )?,
            "integration.installNative" => IntegrationJournal::new(self.profile.clone())
                .install_native_route(
                    data["endpoint"]
                        .as_str()
                        .ok_or_else(|| anyhow::anyhow!("explicit bridge endpoint required"))?,
                )?,
            "integration.restore" => IntegrationJournal::new(self.profile.clone()).restore()?,
            "mcp.stop" => self.stop_mcp()?,
            "mcp.status" => return self.mcp_status(),
            "runtime.start" => {
                let executable = data["executable"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("explicit executable required"))?;
                let args: Vec<String> = serde_json::from_value(data["args"].clone())?;
                self.supervisor.start(Invocation::new(executable, args)?)?;
            }
            "runtime.stop" => self.shutdown()?,
            "runtime.poll" => self.supervisor.poll()?,
            _ => bail!("unsupported runtime action: {action}"),
        }
        Ok(json!({"ok":true}))
    }
}
fn validate_config(v: &Value) -> Result<()> {
    ensure!(v.is_object(), "configuration object required");
    ensure!(v["host"] == "127.0.0.1", "loopback host required");
    ensure!(
        v["port"].as_u64().is_some_and(|p| p > 0 && p <= 65535),
        "explicit port required"
    );
    ensure!(
        v["automaticAppName"]
            .as_str()
            .is_some_and(|s| !s.trim().is_empty())
            && v["manualAppName"]
                .as_str()
                .is_some_and(|s| !s.trim().is_empty())
            && v["automaticAppName"] != v["manualAppName"],
        "distinct explicit connector names required"
    );
    Ok(())
}

pub const TUNNEL_VERSION: &str = "0.0.16";
pub const TUNNEL_RELEASE_BASE: &str =
    "https://github.com/openai/tunnel-client/releases/download/v0.0.16";
/// Verify an explicitly supplied archive against the official pinned release manifest.
/// The caller must obtain the manifest from TUNNEL_RELEASE_BASE over authenticated HTTPS.
pub fn verify_tunnel_archive(asset: &str, bytes: &[u8], official_manifest: &str) -> Result<()> {
    use sha2::{Digest, Sha256};
    ensure!(
        ["darwin", "linux", "windows"]
            .iter()
            .any(|os| ["arm64", "amd64"]
                .iter()
                .any(|arch| asset == format!("tunnel-client-v{TUNNEL_VERSION}-{os}-{arch}.zip"))),
        "unrecognized pinned asset"
    );
    let hashes: Vec<_> = official_manifest
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let hash = fields.next()?;
            let name = fields.next()?.trim_start_matches('*');
            if name == asset { Some(hash) } else { None }
        })
        .collect();
    ensure!(hashes.len() == 1, "missing or ambiguous checksum entry");
    ensure!(
        hex::encode(Sha256::digest(bytes)) == hashes[0].to_ascii_lowercase(),
        "tunnel archive checksum mismatch"
    );
    Ok(())
}
#[derive(Serialize, Deserialize)]
pub struct TunnelConfig {
    pub tunnel_id: String,
    pub profile_dir: PathBuf,
    pub runtime_key_file: PathBuf,
    pub profile_name: String,
}
impl RuntimeFacade {
    pub fn configure_tunnel(&self, tunnel_id: &str, credential: &[u8]) -> Result<TunnelConfig> {
        ensure!(
            tunnel_id.starts_with("tunnel_")
                && tunnel_id.len() == 39
                && tunnel_id[7..]
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
            "invalid tunnel id"
        );
        ensure!(
            !credential.is_empty() && credential.len() <= 64 * 1024,
            "explicit fresh credentials required"
        );
        let instance = uuid::Uuid::new_v4().to_string();
        let relative = format!("tunnel/{instance}");
        let profile_dir = self.profile.directory(&relative)?;
        let key = format!("{relative}/runtime.key");
        self.profile.write(&key, credential)?;
        let config = TunnelConfig {
            tunnel_id: tunnel_id.into(),
            profile_dir,
            runtime_key_file: self.profile.path(&key)?,
            profile_name: format!("gpui-{instance}"),
        };
        self.profile
            .write_json(format!("{relative}/config.json"), &config)?;
        Ok(config)
    }
    /// Start only an explicit, already verified, application-owned tunnel executable.
    /// Archive verification and extraction are intentionally separate from execution.
    pub fn start_tunnel(
        &self,
        config: &TunnelConfig,
        binary: &Path,
        expected_binary_sha256: &str,
    ) -> Result<ChildProcess> {
        use sha2::{Digest, Sha256};
        ensure!(
            binary.starts_with(self.profile.root().join("tunnel")),
            "tunnel binary must be owned"
        );
        reject_symlinks(binary)?;
        ensure!(
            hex::encode(Sha256::digest(fs::read(binary)?)) == expected_binary_sha256,
            "binary checksum mismatch"
        );
        ensure!(
            config
                .profile_dir
                .starts_with(self.profile.root().join("tunnel"))
                && config.runtime_key_file.parent() == Some(config.profile_dir.as_path()),
            "foreign tunnel instance"
        );
        reject_symlinks(&config.profile_dir)?;
        reject_symlinks(&config.runtime_key_file)?;
        ensure!(
            fs::metadata(&config.runtime_key_file)?.permissions().mode() & 0o077 == 0,
            "credential permissions must be private"
        );
        let inv = Invocation::new(
            binary,
            vec![
                "run".into(),
                "--profile-dir".into(),
                config.profile_dir.to_string_lossy().into_owned(),
                "--profile".into(),
                config.profile_name.clone(),
            ],
        )?;
        ChildProcess::spawn(&self.profile, &inv)
    }
}
/// Fetch only the official checksum manifest for the pinned release; does not install or run.
pub async fn verify_official_tunnel_archive(asset: &str, bytes: &[u8]) -> Result<()> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;
    let mut response = client
        .get(format!("{TUNNEL_RELEASE_BASE}/SHA256SUMS.txt"))
        .send()
        .await?
        .error_for_status()?;
    let mut manifest = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        ensure!(
            manifest.len() + chunk.len() <= 1024 * 1024,
            "checksum manifest too large"
        );
        manifest.extend_from_slice(&chunk);
    }
    verify_tunnel_archive(asset, bytes, std::str::from_utf8(&manifest)?)
}

mod route;
pub use route::{compile_native_route, dynamic_catalog};
mod mcp;
pub use mcp::McpSetupResult;
mod desktop;
pub use desktop::{
    APPLICATION_ID, LaunchOnLogin, LoginItemBackend, LoginItemStatus, WindowBounds, WindowSnapshot,
    WindowState, WindowStateStore, WindowStateTracker, WorkArea, capture_window_state,
    normalize_window_state, should_start_hidden,
};
#[cfg(target_os = "macos")]
pub use desktop::{NativeLoginItem, autostart_status, set_autostart};
pub mod signin;
pub use signin::{
    OwnedLoginDestination, SignInCancellation, SignInPhase, SignInReceipt, SignInSession,
    SignInStatus, ValidatedLoginState, validate_capture_marker, validate_passkey_login_state,
};
