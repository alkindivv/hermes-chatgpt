use anyhow::{Context as _, Result, ensure};
use crate::ui::{Launcher, UiEvent, now_ms};
use cw_browser::Cancellation;
use gpui::{Context, Window};
use std::{path::PathBuf, sync::{Arc, mpsc}, time::Duration};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginPhase { Opening, Waiting, Importing, SigningOut, Cancelling }
#[derive(Debug, PartialEq, Eq)]
pub enum LoginCommand { Continue, Cancel }
pub struct SessionJob {
    pub id: String,
    pub target: String,
    pub phase: LoginPhase,
    pub commands: Option<mpsc::Sender<LoginCommand>>,
    pub cancellation: Cancellation,
    pub process_cancellation: Option<cw_runtime::SignInCancellation>,
}
impl SessionJob {
    pub fn continue_login(&mut self) -> Result<()> {
        ensure!(self.phase == LoginPhase::Waiting && !self.cancellation.is_cancelled(), "The login is not waiting for confirmation");
        self.commands.as_ref().context("No passkey capture is waiting")?.send(LoginCommand::Continue)
            .map_err(|_| anyhow::anyhow!("The login worker has stopped"))?;
        self.phase = LoginPhase::Importing;
        Ok(())
    }
    pub fn cancel(&mut self) {
        self.cancellation.cancel();
        self.phase = LoginPhase::Cancelling;
        if let Some(sender) = &self.commands { let _ = sender.send(LoginCommand::Cancel); }
        if let Some(cancel) = self.process_cancellation.take() {
            std::thread::spawn(move || { let _ = cancel.cancel(); });
        }
    }
}
impl Drop for SessionJob { fn drop(&mut self) { self.cancel(); } }

impl Launcher {
    fn session_pages(&self) -> Result<(String, Vec<Arc<cw_browser::BrowserEngine>>)> {
        ensure!(!self.busy() && self.pending_turns.is_empty() && self.pending_compactions.is_empty(), "Finish active work before changing the browser session");
        ensure!(!self.manual(), "Zero Risk does not change or inspect browser sessions");
        let primary = self.tab().context("No owned browser is available")?;
        let mut pages = vec![primary.engine.clone().context("The browser is still initializing")?];
        for tab in self.tabs.iter().filter(|tab| tab.id != primary.id) {
            pages.push(tab.engine.clone().context("Wait for all browser tabs to initialize")?);
        }
        Ok((primary.id.clone(), pages))
    }

    pub(crate) fn start_logout(&mut self) -> Result<()> {
        let (target, pages) = self.session_pages()?;
        let guard = self.bridge.begin_session_change().map_err(anyhow::Error::msg)?;
        self.forget_session_evidence()?;
        let id = uuid::Uuid::new_v4().to_string();
        let cancellation = Cancellation::default();
        self.session_job = Some(SessionJob { id:id.clone(), target, phase:LoginPhase::SigningOut,
            commands:None, cancellation:cancellation.clone(), process_cancellation:None });
        self.operation = Some(self.text("logOut"));
        let sender = self.sender.clone();
        self.executor.spawn(async move {
            let result = replace_pages(&pages, None, cancellation).await;
            let _ = sender.send(UiEvent::SessionFinished { id, result, guard });
        });
        Ok(())
    }

    pub(crate) fn start_passkey(&mut self) -> Result<()> {
        let (target, pages) = self.session_pages()?;
        ensure!(self.session_account.is_none(), "The owned browser is already signed in");
        let chrome = PathBuf::from("/Applications/Google Chrome.app/Contents/MacOS/Google Chrome");
        ensure!(chrome.is_file(), "Install Google Chrome in Applications before using passkey sign-in");
        let guard = self.bridge.begin_session_change().map_err(anyhow::Error::msg)?;
        self.forget_session_evidence()?;
        let id = uuid::Uuid::new_v4().to_string();
        let (commands, receiver) = mpsc::channel();
        let cancellation = Cancellation::default();
        self.session_job = Some(SessionJob { id:id.clone(), target, phase:LoginPhase::Opening,
            commands:Some(commands), cancellation:cancellation.clone(), process_cancellation:None });
        self.operation = Some(self.text("passkeyOpening"));
        let profile = self.profile.clone();
        let sender = self.sender.clone();
        self.executor.spawn(async move {
            let captured = tokio::task::spawn_blocking({
                let id = id.clone(); let sender = sender.clone(); let cancellation = cancellation.clone();
                move || capture_login(profile, chrome, id, receiver, cancellation, sender)
            }).await;
            let result = match captured {
                Ok(Ok(state)) if !cancellation.is_cancelled() => replace_pages(&pages, Some(&state), cancellation.clone()).await,
                Ok(Ok(_)) => Err(cw_browser::SessionFailure { message:"Passkey sign-in cancelled before session import".into(), cleanup_confirmed:true }),
                Ok(Err(error)) => Err(cw_browser::SessionFailure { message:cw_runtime::redact(&error.to_string()), cleanup_confirmed:true }),
                Err(_) => Err(cw_browser::SessionFailure { message:"The passkey worker stopped unexpectedly".into(), cleanup_confirmed:false }),
            };
            let _ = sender.send(UiEvent::SessionFinished { id, result, guard });
        });
        Ok(())
    }

    pub(crate) fn login_waiting(&mut self, id: String, cancellation: cw_runtime::SignInCancellation) {
        if let Some(job) = self.session_job.as_mut().filter(|job| job.id == id && !job.cancellation.is_cancelled() && job.phase == LoginPhase::Opening) {
            job.phase = LoginPhase::Waiting;
            job.process_cancellation = Some(cancellation);
            self.operation = Some(self.text("passkeyWaitingTitle"));
        } else {
            std::thread::spawn(move || { let _ = cancellation.cancel(); });
        }
    }

    pub(crate) fn finish_session_change(&mut self, id: String, result: std::result::Result<cw_browser::SessionReceipt, cw_browser::SessionFailure>, guard: cw_bridge::SessionChangeGuard, window: &mut Window, cx: &mut Context<Self>) {
        if self.session_job.as_ref().is_none_or(|job| job.id != id) {
            // Retain an unexplained completion instead of reopening admission on an unknown session.
            self.session_quarantine = Some(guard);
            self.error = Some("A session completion did not match its owner".into());
            return;
        }
        let job = self.session_job.as_ref().unwrap();
        if job.cancellation.is_cancelled() && result.as_ref().is_ok_and(|receipt| receipt.inspection.is_some()) {
            // Cancel may arrive after a successful worker reply was queued but before UI consumption.
            if let Some(engine) = self.tabs.iter().find(|tab| tab.id == job.target).and_then(|tab| tab.engine.clone()) {
                let sender = self.sender.clone();
                self.executor.spawn(async move {
                    let result = engine.replace_login_state(None, cw_browser::InteractionMode::Automatic, Cancellation::default()).await;
                    let _ = sender.send(UiEvent::SessionFinished { id, result, guard });
                });
            } else {
                self.session_quarantine = Some(guard);
                self.error = Some("Cancelled sign-in has no owned page for cleanup".into());
            }
            return;
        }
        let mut job = self.session_job.take().unwrap();
        job.commands = None;
        job.process_cancellation = None;
        self.operation = None;
        let target = job.target.clone();
        match result {
            Ok(receipt) => {
                // All views were parked before mutation. Retire retained task pages; never reuse their old epochs.
                for tab in &mut self.tabs { if tab.id != target || tab.owner_key.is_some() { tab.close_requested = true; } }
                if let Some(inspection) = receipt.inspection {
                    if let Err(error) = self.accept_capabilities(&target, inspection, window, cx) { self.error = Some(error.to_string()); }
                    else { self.log("info", "browser.passkey-imported", "The transferred session passed server and account verification in the owned browser"); }
                } else {
                    if let Err(error) = self.forget_session_evidence() { self.error = Some(error.to_string()); }
                    self.log("info", "browser.logout-verified", "The owned server session is confirmed signed out");
                }
                if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.id == target && !tab.close_requested) {
                    tab.visible = true;
                    tab.status = if tab.authenticated { "ready" } else { "signed-out" }.into();
                    self.selected_tab = Some(target);
                }
                let reminder = cw_contracts::state::next_session_refresh_reminder_at(now_ms() as f64);
                if let Ok(reminder) = reminder { let _ = self.persist(|state| state.session_refresh_reminder_at = Some(reminder)); }
                self.surface = cw_contracts::Surface::Browser;
            }
            Err(error) => {
                if !error.cleanup_confirmed {
                    for tab in &mut self.tabs { tab.quarantined = true; tab.status = "cleanup-unconfirmed".into(); }
                    self.session_quarantine = Some(guard);
                }
                self.log("error", "browser.session-change-failed", &error.message);
                self.error = Some(error.message);
            }
        }
    }
}

async fn replace_pages(pages: &[Arc<cw_browser::BrowserEngine>], state: Option<&cw_runtime::ValidatedLoginState>, cancellation: Cancellation) -> std::result::Result<cw_browser::SessionReceipt, cw_browser::SessionFailure> {
    for page in pages.iter().skip(1) {
        if page.park_for_session_change(cancellation.clone()).await.is_err() {
            return Err(cw_browser::SessionFailure { message:"An owned browser tab could not be parked for session replacement".into(), cleanup_confirmed:false });
        }
    }
    pages[0].replace_login_state(state, cw_browser::InteractionMode::Automatic, cancellation).await
}

fn capture_login(profile: cw_runtime::Profile, chrome: PathBuf, id: String, commands: mpsc::Receiver<LoginCommand>, cancellation: Cancellation, sender: mpsc::Sender<UiEvent>) -> Result<cw_runtime::ValidatedLoginState> {
    ensure!(!cancellation.is_cancelled(), "Passkey sign-in cancelled");
    let mut login = cw_runtime::SignInSession::start(profile.clone(), chrome, Duration::from_secs(600))?;
    let destination = cw_runtime::OwnedLoginDestination::new(profile, format!("browser/login-transfer-{id}.json").into())?;
    let _ = sender.send(UiEvent::LoginWaiting { id, cancellation:login.cancellation_handle() });
    let outcome = (|| {
        loop {
            ensure!(!cancellation.is_cancelled(), "Passkey sign-in cancelled");
            ensure!(login.status()?.phase == cw_runtime::SignInPhase::Waiting, "The disposable login browser closed or expired");
            match commands.recv_timeout(Duration::from_millis(150)) {
                Ok(LoginCommand::Continue) => break,
                Ok(LoginCommand::Cancel) | Err(mpsc::RecvTimeoutError::Disconnected) => anyhow::bail!("Passkey sign-in cancelled"),
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
        login.continue_into(&destination)?;
        // Consume the sensitive envelope once, even if cancellation arrived after capture committed it.
        let state = destination.take(chrono::Utc::now())?;
        ensure!(!cancellation.is_cancelled(), "Passkey sign-in cancelled before session import");
        Ok(state)
    })();
    let cleanup = login.cancel();
    match (outcome, cleanup) {
        (Ok(state), Ok(())) => Ok(state),
        (Err(error), Ok(())) => Err(error),
        (_, Err(_)) => anyhow::bail!("The disposable login browser could not be cleaned up"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn waiting() -> (SessionJob, mpsc::Receiver<LoginCommand>) {
        let (tx, rx) = mpsc::channel();
        (SessionJob { id:"fixture".into(), target:"owned".into(), phase:LoginPhase::Waiting,
            commands:Some(tx), cancellation:Cancellation::default(), process_cancellation:None }, rx)
    }
    #[test]
    fn explicit_continue_is_dispatched_once_and_disables_repeat_capture() {
        let (mut job, rx) = waiting();
        job.continue_login().unwrap();
        assert_eq!(rx.try_recv().unwrap(), LoginCommand::Continue);
        assert_eq!(job.phase, LoginPhase::Importing);
        assert!(job.continue_login().is_err());
        assert!(rx.try_recv().is_err());
    }
    #[test]
    fn cancelled_or_closed_login_cannot_start_a_later_capture() {
        let (mut job, rx) = waiting();
        let signal = job.cancellation.clone();
        job.cancel();
        assert!(signal.is_cancelled());
        assert_eq!(rx.try_recv().unwrap(), LoginCommand::Cancel);
        assert!(job.continue_login().is_err());
        assert!(rx.try_recv().is_err());
    }
    #[test]
    fn disconnected_worker_does_not_become_importing() {
        let (mut job, rx) = waiting();
        drop(rx);
        assert!(job.continue_login().is_err());
        assert_eq!(job.phase, LoginPhase::Waiting);
    }
}
