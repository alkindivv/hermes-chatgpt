//! Explicit, disposable Chrome sign-in. No personal-profile discovery or credential logging.
use crate::*;
use std::sync::{
    Condvar,
    atomic::{AtomicBool, Ordering},
};
mod cdp;
mod state;
pub use state::{ValidatedLoginState, validate_passkey_login_state};
pub const TEMPORARY_CHAT_URL: &str = "https://chatgpt.com/?temporary-chat=true";
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub enum SignInPhase {
    Waiting,
    Capturing,
    Completed,
    Cancelled,
    Expired,
    Failed,
}
#[derive(Clone, Debug, Serialize)]
pub struct SignInStatus {
    pub id: uuid::Uuid,
    pub pid: u32,
    pub phase: SignInPhase,
    pub cdp_port: Option<u16>,
}
#[derive(Clone)]
pub struct OwnedLoginDestination {
    profile: Profile,
    relative: PathBuf,
}
impl OwnedLoginDestination {
    /// Explicit owned JSON destination beneath browser/. Existing files are never replaced.
    pub fn new(profile: Profile, relative: PathBuf) -> Result<Self> {
        ensure!(
            relative.starts_with("browser")
                && relative.components().count() > 1
                && relative.extension().is_some_and(|v| v == "json"),
            "login destination must be an owned browser JSON file"
        );
        profile.path(&relative)?;
        Ok(Self { profile, relative })
    }
    /// Revalidate and consume one owned transfer. Returns sensitive data without logging it.
    pub fn take(&self, now: chrono::DateTime<chrono::Utc>) -> Result<ValidatedLoginState> {
        let path = self.profile.path(&self.relative)?;
        let lock = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(
                self.profile
                    .path(self.relative.with_extension("json.lock"))?,
            )?;
        fs2::FileExt::lock_exclusive(&lock)?;
        let metadata = fs::metadata(&path)?;
        ensure!(
            metadata.is_file() && metadata.len() <= 16 * 1024 * 1024 + 64 * 1024,
            "invalid owned transfer size"
        );
        let value: Value = serde_json::from_slice(&fs::read(&path)?)
            .map_err(|_| anyhow::anyhow!("invalid owned transfer JSON"))?;
        validate_capture_marker(&value["marker"], now)?;
        let state = validate_passkey_login_state(&value["storageState"])?;
        fs::remove_file(path)?;
        Ok(state)
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct SignInReceipt {
    pub transfer_path: PathBuf,
    pub captured_at: String,
    pub cookie_count: usize,
    pub local_storage_count: usize,
    pub authenticated: bool,
}
/// Cloneable UI cancellation capability scoped to exactly this disposable login child/profile.
#[derive(Clone)]
pub struct SignInCancellation {
    profile: Profile,
    relative: String,
    child: Arc<Mutex<ChildProcess>>,
    requested: Arc<AtomicBool>,
    wake: Arc<(Mutex<bool>, Condvar)>,
}
impl SignInCancellation {
    pub fn cancel(&self) -> Result<()> {
        self.requested.store(true, Ordering::Release);
        let (lock, condvar) = &*self.wake;
        *lock.lock().unwrap() = true;
        condvar.notify_all();
        self.child.lock().unwrap().stop()?;
        let path = self.profile.path(&self.relative)?;
        if path.exists() {
            fs::remove_dir_all(path)?;
        }
        Ok(())
    }
}
/// Owns exactly the disposable Chrome child. Drop/cancel stop it and remove only its UUID directory.
pub struct SignInSession {
    profile: Profile,
    id: uuid::Uuid,
    relative: String,
    chrome: PathBuf,
    child: Arc<Mutex<ChildProcess>>,
    deadline: Instant,
    expired: Arc<AtomicBool>,
    cancelled: Arc<AtomicBool>,
    wake: Arc<(Mutex<bool>, Condvar)>,
    watchdog: Option<std::thread::JoinHandle<()>>,
    phase: SignInPhase,
}
impl SignInSession {
    /// Caller must invoke only for an explicit sign-in action. No browser is launched on construction elsewhere.
    pub fn start(profile: Profile, chrome: PathBuf, timeout: Duration) -> Result<Self> {
        ensure!(
            !timeout.is_zero() && timeout <= Duration::from_secs(600),
            "sign-in timeout must be within 1 ns..10 minutes"
        );
        ensure!(
            chrome.is_absolute() && chrome.is_file(),
            "explicit absolute Chrome executable required"
        );
        let id = uuid::Uuid::new_v4();
        let relative = format!("signin/{id}");
        profile.directory(format!("{relative}/chrome"))?;
        let invocation = invocation(&profile, &relative, &chrome, false)?;
        let child = match ChildProcess::spawn(&profile, &invocation) {
            Ok(c) => Arc::new(Mutex::new(c)),
            Err(_) => {
                let _ = fs::remove_dir_all(profile.path(&relative)?);
                bail!("dedicated login process failed to start");
            }
        };
        let expired = Arc::new(AtomicBool::new(false));
        let wake = Arc::new((Mutex::new(false), Condvar::new()));
        let (clock, signal, owned) = (expired.clone(), wake.clone(), child.clone());
        let temporary_profile = profile.clone();
        let temporary_relative = relative.clone();
        let watchdog = std::thread::spawn(move || {
            let (lock, condvar) = &*signal;
            let done = lock.lock().unwrap();
            let (done, _) = condvar
                .wait_timeout_while(done, timeout, |done| !*done)
                .unwrap();
            if !*done {
                clock.store(true, Ordering::Release);
                let _ = owned.lock().unwrap().stop();
                if let Ok(path) = temporary_profile.path(&temporary_relative) {
                    let _ = fs::remove_dir_all(path);
                }
            }
        });
        Ok(Self {
            profile,
            id,
            relative,
            chrome,
            child,
            deadline: Instant::now() + timeout,
            expired,
            cancelled: Arc::new(AtomicBool::new(false)),
            wake,
            watchdog: Some(watchdog),
            phase: SignInPhase::Waiting,
        })
    }
    pub fn cancellation_handle(&self) -> SignInCancellation {
        SignInCancellation {
            profile: self.profile.clone(),
            relative: self.relative.clone(),
            child: self.child.clone(),
            requested: self.cancelled.clone(),
            wake: self.wake.clone(),
        }
    }
    pub fn status(&mut self) -> Result<SignInStatus> {
        if self.phase != SignInPhase::Completed {
            if self.expired.load(Ordering::Acquire) {
                self.phase = SignInPhase::Expired;
            } else if self.cancelled.load(Ordering::Acquire) {
                self.phase = SignInPhase::Cancelled;
            }
        }
        let pid;
        {
            let mut child = self.child.lock().unwrap();
            pid = child.pid();
            if matches!(self.phase, SignInPhase::Waiting | SignInPhase::Capturing)
                && child.child.try_wait()?.is_some()
            {
                self.phase = SignInPhase::Failed;
            }
        }
        if matches!(
            self.phase,
            SignInPhase::Failed | SignInPhase::Expired | SignInPhase::Cancelled
        ) {
            self.finish_watchdog();
            self.cleanup()?;
        }
        Ok(SignInStatus {
            id: self.id,
            pid,
            phase: self.phase.clone(),
            cdp_port: self.endpoint().ok().map(|v| v.0),
        })
    }
    pub fn login_profile(&self) -> Result<PathBuf> {
        self.profile.path(format!("{}/chrome", self.relative))
    }
    fn endpoint(&self) -> Result<(u16, String)> {
        let path = self
            .profile
            .path(format!("{}/chrome/DevToolsActivePort", self.relative))?;
        ensure!(
            fs::metadata(&path)?.len() <= 1024,
            "invalid owned CDP endpoint file"
        );
        let bytes = fs::read_to_string(path)?;
        let mut lines = bytes.lines();
        let port = lines
            .next()
            .and_then(|v| v.parse::<u16>().ok())
            .filter(|v| *v > 0)
            .ok_or_else(|| anyhow::anyhow!("CDP port not ready"))?;
        let path = lines
            .next()
            .ok_or_else(|| anyhow::anyhow!("CDP browser not ready"))?;
        let id = path
            .strip_prefix("/devtools/browser/")
            .ok_or_else(|| anyhow::anyhow!("invalid CDP browser path"))?;
        uuid::Uuid::parse_str(id).map_err(|_| anyhow::anyhow!("invalid CDP browser ID"))?;
        ensure!(lines.next().is_none(), "invalid CDP endpoint file");
        Ok((port, path.into()))
    }
    fn active(&self) -> Result<()> {
        ensure!(
            !self.expired.load(Ordering::Acquire)
                && !self.cancelled.load(Ordering::Acquire)
                && Instant::now() < self.deadline,
            "sign-in deadline expired"
        );
        ensure!(
            self.child.lock().unwrap().child.try_wait()?.is_none(),
            "dedicated login process closed before Continue"
        );
        Ok(())
    }
    fn connect(&self) -> Result<cdp::Cdp> {
        loop {
            self.active()?;
            if let Ok((port, path)) = self.endpoint() {
                return cdp::Cdp::open(port, &path, self.deadline);
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    fn stop_gracefully(&self, cdp: &mut cdp::Cdp) -> Result<()> {
        cdp.close_browser();
        let until = (Instant::now() + Duration::from_secs(5)).min(self.deadline);
        loop {
            if self.child.lock().unwrap().child.try_wait()?.is_some() {
                break;
            }
            if Instant::now() >= until {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        self.child.lock().unwrap().stop()
    }
    fn capture(&mut self) -> Result<ValidatedLoginState> {
        let mut browser = self.connect()?;
        let first_port = self.endpoint()?.0;
        self.stop_gracefully(&mut browser)?;
        drop(browser);
        // Delete only disposable tab-session records before offline cookie restoration.
        let default = self
            .profile
            .path(format!("{}/chrome/Default", self.relative))?;
        let sessions = default.join("Sessions");
        crate::reject_symlinks(&sessions)?;
        if sessions.exists() {
            fs::remove_dir_all(sessions)?;
        }
        for name in [
            "Current Session",
            "Current Tabs",
            "Last Session",
            "Last Tabs",
        ] {
            let path = default.join(name);
            crate::reject_symlinks(&path)?;
            match fs::remove_file(path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(e.into()),
            }
        }
        fs::remove_file(
            self.profile
                .path(format!("{}/chrome/DevToolsActivePort", self.relative))?,
        )?;
        ensure!(
            !self.expired.load(Ordering::Acquire) && !self.cancelled.load(Ordering::Acquire),
            "sign-in deadline expired or cancelled"
        );
        let invocation = invocation(&self.profile, &self.relative, &self.chrome, true)?;
        {
            let mut owned = self.child.lock().unwrap();
            ensure!(
                !self.expired.load(Ordering::Acquire) && !self.cancelled.load(Ordering::Acquire),
                "sign-in deadline expired or cancelled"
            );
            *owned = ChildProcess::spawn(&self.profile, &invocation)
                .map_err(|_| anyhow::anyhow!("offline capture process failed to start"))?;
        }
        let mut browser = self.connect()?;
        ensure!(
            self.endpoint()?.0 != first_port,
            "offline capture reused original CDP port"
        );
        let raw = browser.capture()?;
        let state = validate_passkey_login_state(&raw)?;
        self.stop_gracefully(&mut browser)?;
        Ok(state)
    }
    /// Blocking bounded CDP workflow: move this owned handle to a worker/spawn_blocking task.
    pub fn continue_into(&mut self, destination: &OwnedLoginDestination) -> Result<SignInReceipt> {
        ensure!(
            self.phase == SignInPhase::Waiting,
            "sign-in Continue is not available"
        );
        ensure!(
            destination.profile.root() == self.profile.root(),
            "foreign sign-in destination profile"
        );
        let target = self.profile.path(&destination.relative)?;
        ensure!(!target.exists(), "login destination already occupied");
        self.phase = SignInPhase::Capturing;
        let result = (|| -> Result<SignInReceipt> {
            let state = self.capture()?;
            ensure!(
                !self.expired.load(Ordering::Acquire)
                    && !self.cancelled.load(Ordering::Acquire)
                    && Instant::now() < self.deadline,
                "sign-in deadline expired"
            );
            self.cleanup()?;
            ensure!(
                !self.expired.load(Ordering::Acquire)
                    && !self.cancelled.load(Ordering::Acquire)
                    && Instant::now() < self.deadline,
                "sign-in deadline expired"
            );
            self.finish_watchdog();
            let now = chrono::Utc::now();
            let marker = json!({"version":1,"captureComplete":true,"source":"isolated-normal-browser-profile","capturedAt":now.to_rfc3339()});
            validate_capture_marker(&marker, now)?;
            // One atomic envelope prevents a state/marker partial commit. No authenticated marker is created.
            let parent = destination.relative.parent().unwrap();
            self.profile.directory(parent)?;
            let lock_path = destination.relative.with_extension("json.lock");
            let lock = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(false)
                .mode(0o600)
                .open(self.profile.path(lock_path)?)?;
            fs2::FileExt::lock_exclusive(&lock)?;
            ensure!(
                !self.profile.path(&destination.relative)?.exists(),
                "login destination conflict"
            );
            self.profile.write_json(
                &destination.relative,
                &json!({"marker":marker,"storageState":state.storage_state()}),
            )?;
            Ok(SignInReceipt {
                transfer_path: target,
                captured_at: now.to_rfc3339(),
                cookie_count: state.cookie_count(),
                local_storage_count: state.local_storage_count(),
                authenticated: false,
            })
        })();
        self.finish_watchdog();
        let cleanup = self.cleanup();
        match result {
            Ok(receipt) => {
                cleanup?;
                self.phase = SignInPhase::Completed;
                Ok(receipt)
            }
            Err(error) => {
                self.phase = if self.expired.load(Ordering::Acquire) {
                    SignInPhase::Expired
                } else if self.cancelled.load(Ordering::Acquire) {
                    SignInPhase::Cancelled
                } else {
                    SignInPhase::Failed
                };
                cleanup?;
                Err(error)
            }
        }
    }
    fn finish_watchdog(&mut self) {
        let (lock, condvar) = &*self.wake;
        *lock.lock().unwrap() = true;
        condvar.notify_all();
        if let Some(thread) = self.watchdog.take() {
            let _ = thread.join();
        }
    }
    fn cleanup(&self) -> Result<()> {
        self.child.lock().unwrap().stop()?;
        let path = self.profile.path(&self.relative)?;
        if path.exists() {
            fs::remove_dir_all(path)?;
        }
        Ok(())
    }
    pub fn cancel(&mut self) -> Result<()> {
        if self.phase != SignInPhase::Completed {
            self.cancellation_handle().cancel()?;
        }
        self.finish_watchdog();
        self.cleanup()?;
        if !matches!(
            self.phase,
            SignInPhase::Completed | SignInPhase::Expired | SignInPhase::Failed
        ) {
            self.phase = SignInPhase::Cancelled;
        }
        Ok(())
    }
}
impl Drop for SignInSession {
    fn drop(&mut self) {
        self.finish_watchdog();
        let _ = self.cleanup();
    }
}
fn invocation(
    profile: &Profile,
    relative: &str,
    chrome: &Path,
    offline: bool,
) -> Result<Invocation> {
    let mut args = vec![
        format!(
            "--user-data-dir={}",
            profile.path(format!("{relative}/chrome"))?.display()
        ),
        "--disable-background-mode".into(),
        "--no-first-run".into(),
        "--no-default-browser-check".into(),
        "--remote-debugging-address=127.0.0.1".into(),
        "--remote-debugging-port=0".into(),
    ];
    if offline {
        args.extend([
            "--headless=new".into(),
            "--disable-background-networking".into(),
            "--restore-last-session".into(),
            "about:blank".into(),
        ]);
    } else {
        args.extend(["--new-window".into(), TEMPORARY_CHAT_URL.into()]);
    }
    Invocation::new(chrome, args)
}
pub fn validate_capture_marker(marker: &Value, now: chrono::DateTime<chrono::Utc>) -> Result<()> {
    ensure!(
        serde_json::to_vec(marker)?.len() <= 64 * 1024,
        "capture marker too large"
    );
    ensure!(
        marker["version"] == 1
            && marker["captureComplete"] == true
            && marker["source"] == "isolated-normal-browser-profile",
        "invalid sign-in capture evidence"
    );
    let captured = marker["capturedAt"]
        .as_str()
        .and_then(|v| chrono::DateTime::parse_from_rfc3339(v).ok())
        .ok_or_else(|| anyhow::anyhow!("invalid capture timestamp"))?;
    ensure!(
        captured >= now - chrono::Duration::minutes(11)
            && captured <= now + chrono::Duration::seconds(60),
        "stale or future capture evidence"
    );
    Ok(())
}
#[cfg(test)]
mod tests;
