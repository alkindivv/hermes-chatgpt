use super::*;
use std::net::TcpListener;
fn profile() -> Result<(tempfile::TempDir, Profile)> {
    let t = tempfile::tempdir()?;
    let p = Profile::isolated(fs::canonicalize(t.path())?.join("owned"))?;
    Ok((t, p))
}
fn fixture(root: &Path, ports: (u16, u16)) -> Result<PathBuf> {
    let source = root.join("fixture-chrome.rs");
    let binary = root.join("fixture-chrome");
    let code=r#"use std::{env,fs,path::PathBuf,thread,time::Duration};fn main(){let a:Vec<String>=env::args().collect();assert!(a.iter().any(|v|v=="--remote-debugging-port=0"));assert!(!a.iter().any(|v|v=="--enable-automation"));let p=PathBuf::from(a.iter().find_map(|v|v.strip_prefix("--user-data-dir=")).unwrap());let offline=a.iter().any(|v|v=="--headless=new");let _=fs::remove_file(p.join("close-request"));if offline{assert!(!p.join("Default/Sessions").exists());assert!(a.iter().any(|v|v=="--restore-last-session"));assert!(a.iter().any(|v|v=="--disable-background-networking"));}else{fs::create_dir_all(p.join("Default/Sessions")).unwrap();assert!(a.iter().any(|v|v=="https://chatgpt.com/?temporary-chat=true"));}fs::write(p.join(if offline{"offline-args"}else{"normal-args"}),a.join("\n")).unwrap();fs::write(p.join("DevToolsActivePort"),format!("{}\n/devtools/browser/00000000-0000-4000-8000-000000000001\n",if offline{PORT2}else{PORT1})).unwrap();loop {if p.join("close-request").exists(){break;}thread::sleep(Duration::from_millis(5));}}"#.replace("PORT1",&ports.0.to_string()).replace("PORT2",&ports.1.to_string());
    fs::write(&source, code)?;
    ensure!(
        Command::new("rustc")
            .arg("--edition=2024")
            .arg(&source)
            .arg("-o")
            .arg(&binary)
            .status()?
            .success(),
        "fixture compilation failed"
    );
    Ok(binary)
}
fn mock(
    listener: TcpListener,
    login: Arc<Mutex<PathBuf>>,
    commands: Arc<Mutex<Vec<String>>>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let mut ws = tungstenite::accept(stream).unwrap();
        while let Ok(tungstenite::Message::Text(text)) = ws.read() {
            let call: Value = serde_json::from_str(&text).unwrap();
            let method = call["method"].as_str().unwrap();
            commands.lock().unwrap().push(method.into());
            if method == "Browser.close" {
                fs::write(login.lock().unwrap().join("close-request"), b"close").unwrap();
                break;
            }
            let result = match method {
                "Target.getTargets" => {
                    json!({"targetInfos":[{"type":"page","url":"about:blank","targetId":"fixture-page"}]})
                }
                "Target.attachToTarget" => json!({"sessionId":"fixture-session"}),
                "Page.navigate" => {
                    ws.send(tungstenite::Message::Text(json!({"method":"Fetch.requestPaused","sessionId":"fixture-session","params":{"requestId":"fixture-request"}}).to_string().into())).unwrap();
                    json!({"frameId":"fixture-frame"})
                }
                "Page.getFrameTree" => json!({"frameTree":{"frame":{"url":TEMPORARY_CHAT_URL}}}),
                "DOMStorage.getDOMStorageItems" => {
                    json!({"entries":[["fixture-storage","fixture-only-secret"]]})
                }
                "Storage.getCookies" => {
                    json!({"cookies":[{"name":"fixture-cookie","value":"fixture-only-secret","domain":".chatgpt.com","path":"/","expires":-1,"secure":true,"httpOnly":true},{"name":"excluded","value":"excluded-secret","domain":"accounts.google.com","path":"/","expires":-1,"secure":true,"httpOnly":true,"sameSite":"Lax"}]})
                }
                _ => json!({}),
            };
            ws.send(tungstenite::Message::Text(
                json!({"id":call["id"],"result":result}).to_string().into(),
            ))
            .unwrap();
        }
    })
}
#[test]
fn native_owned_login_offline_capture_and_transfer() -> Result<()> {
    let (_t, p) = profile()?;
    let normal = TcpListener::bind("127.0.0.1:0")?;
    let offline = TcpListener::bind("127.0.0.1:0")?;
    let ports = (normal.local_addr()?.port(), offline.local_addr()?.port());
    assert_ne!(ports.0, ports.1);
    let chrome = fixture(p.root(), ports)?;
    let mut session = SignInSession::start(p.clone(), chrome, Duration::from_secs(20))?;
    let login = session.login_profile()?;
    let location = Arc::new(Mutex::new(login.clone()));
    let commands = Arc::new(Mutex::new(Vec::new()));
    let normal_thread = mock(normal, location.clone(), commands.clone());
    let offline_thread = mock(offline, location, commands.clone());
    let until = Instant::now() + Duration::from_secs(5);
    while session.status()?.cdp_port.is_none() && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(session.status()?.cdp_port, Some(ports.0));
    let destination = OwnedLoginDestination::new(p.clone(), "browser/test-transfer.json".into())?;
    let (mut session, destination, outcome) = std::thread::spawn(move || {
        let outcome = session.continue_into(&destination);
        (session, destination, outcome)
    })
    .join()
    .unwrap();
    let receipt = outcome?;
    assert_eq!(receipt.cookie_count, 1);
    assert_eq!(receipt.local_storage_count, 1);
    assert!(!receipt.authenticated);
    assert_eq!(session.status()?.phase, SignInPhase::Completed);
    assert!(!login.exists());
    let transfer: Value = p.read_json("browser/test-transfer.json")?;
    validate_capture_marker(&transfer["marker"], chrono::Utc::now())?;
    assert_eq!(
        transfer["storageState"]["cookies"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(!serde_json::to_string(&receipt)?.contains("secret"));
    assert_eq!(
        fs::metadata(&receipt.transfer_path)?.permissions().mode() & 0o077,
        0
    );
    assert!(session.continue_into(&destination).is_err());
    let imported = destination.take(chrono::Utc::now())?;
    assert_eq!(imported.cookie_count(), 1);
    assert!(!receipt.transfer_path.exists());
    assert!(destination.take(chrono::Utc::now()).is_err());
    normal_thread.join().unwrap();
    offline_thread.join().unwrap();
    let commands = commands.lock().unwrap();
    let offline = commands
        .iter()
        .position(|v| v == "Network.emulateNetworkConditions")
        .unwrap();
    let cookies = commands
        .iter()
        .position(|v| v == "Storage.getCookies")
        .unwrap();
    assert!(offline < cookies);
    assert!(commands.iter().any(|v| v == "Fetch.fulfillRequest"));
    Ok(())
}
#[test]
fn cancellation_expiry_destination_isolation_and_external_child_survival() -> Result<()> {
    let (_t, p) = profile()?;
    let chrome = fixture(p.root(), (12345, 12346))?;
    let mut external = Command::new("/bin/sleep").arg("30").spawn()?;
    let result = (|| -> Result<()> {
        let mut session = SignInSession::start(p.clone(), chrome.clone(), Duration::from_secs(10))?;
        assert!(OwnedLoginDestination::new(p.clone(), "../escape.json".into()).is_err());
        assert!(OwnedLoginDestination::new(p.clone(), "codex-home/secret.json".into()).is_err());
        fs::write(p.path("browser/occupied.json")?, b"existing")?;
        let occupied = OwnedLoginDestination::new(p.clone(), "browser/occupied.json".into())?;
        assert!(session.continue_into(&occupied).is_err());
        assert_eq!(fs::read(p.path("browser/occupied.json")?)?, b"existing");
        let login = session.login_profile()?;
        let cancellation = session.cancellation_handle();
        std::thread::spawn(move || cancellation.cancel())
            .join()
            .unwrap()?;
        assert_eq!(session.status()?.phase, SignInPhase::Cancelled);
        session.cancel()?;
        assert!(!login.exists());
        assert!(external.try_wait()?.is_none());
        let mut timed = SignInSession::start(p.clone(), chrome, Duration::from_millis(50))?;
        let timed_profile = timed.login_profile()?;
        std::thread::sleep(Duration::from_millis(100));
        assert!(!timed_profile.exists());
        assert_eq!(timed.status()?.phase, SignInPhase::Expired);
        assert!(timed.child.lock().unwrap().child.try_wait()?.is_some());
        timed.cancel()?;
        Ok(())
    })();
    let _ = external.kill();
    let _ = external.wait();
    result
}
#[test]
fn marker_freshness_and_capture_schema() {
    let now = chrono::Utc::now();
    let mut marker = json!({"version":1,"captureComplete":true,"source":"isolated-normal-browser-profile","capturedAt":now.to_rfc3339()});
    assert!(validate_capture_marker(&marker, now).is_ok());
    marker["capturedAt"] = (now - chrono::Duration::minutes(12)).to_rfc3339().into();
    assert!(validate_capture_marker(&marker, now).is_err());
    marker["capturedAt"] = (now + chrono::Duration::seconds(61)).to_rfc3339().into();
    assert!(validate_capture_marker(&marker, now).is_err());
    marker["version"] = 2.into();
    assert!(validate_capture_marker(&marker, now).is_err());
}
