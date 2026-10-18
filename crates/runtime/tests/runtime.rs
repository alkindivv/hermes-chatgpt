use cw_runtime::*;
use serde_json::json;
#[test]
fn isolation_and_exact_restore() -> anyhow::Result<()> {
    let t = tempfile::tempdir()?;
    let p = Profile::isolated(std::fs::canonicalize(t.path())?.join("owned"))?;
    assert!(p.path("../escape").is_err());
    let original = b"\xef\xbb\xbf# keep\r\nmodel = 'prior'\r\n";
    std::fs::write(p.codex_home().join("config.toml"), original)?;
    let j = IntegrationJournal::new(p.clone());
    j.install(b"model = 'new'\n")?;
    std::fs::write(p.codex_home().join("config.toml"), b"conflict")?;
    assert!(j.restore().is_err());
    std::fs::write(p.codex_home().join("config.toml"), b"model = 'new'\n")?;
    j.restore()?;
    assert_eq!(std::fs::read(p.codex_home().join("config.toml"))?, original);
    p.write_json("state.json", &json!({"ok":true}))?;
    assert_eq!(
        p.read_json::<serde_json::Value>("state.json")?,
        json!({"ok":true})
    );
    Ok(())
}
#[test]
fn symlink_rejected() -> anyhow::Result<()> {
    let t = tempfile::tempdir()?;
    let p = Profile::isolated(std::fs::canonicalize(t.path())?.join("owned"))?;
    std::os::unix::fs::symlink(t.path(), p.root().join("escape"))?;
    assert!(p.write_json("escape/x", &json!(1)).is_err());
    Ok(())
}
#[test]
fn busy_shutdown_and_owned_process() -> anyhow::Result<()> {
    let t = tempfile::tempdir()?;
    let p = Profile::isolated(std::fs::canonicalize(t.path())?.join("owned"))?;
    let mut s = Supervisor::new(p);
    s.start(Invocation::new("/bin/sleep", vec!["30".into()])?)?;
    let guard = s.gate.enter(Activity::Browser)?;
    assert!(s.shutdown().is_err());
    drop(guard);
    s.shutdown()?;
    assert!(s.pid().is_none());
    Ok(())
}
#[test]
fn drains_both_output_streams() -> anyhow::Result<()> {
    let t = tempfile::tempdir()?;
    let p = Profile::isolated(std::fs::canonicalize(t.path())?.join("owned"))?;
    let mut s = Supervisor::new(p);
    s.start(Invocation::new(
        std::env::current_exe()?,
        vec![
            "--exact".into(),
            "output_child".into(),
            "--nocapture".into(),
        ],
    )?)?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while s.pid().is_some() && std::time::Instant::now() < deadline {
        s.poll()?;
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(s.pid().is_none());
    Ok(())
}
#[test]
fn output_child() {
    if std::env::var_os("CODEX_HOME").is_none() {
        return;
    }
    for _ in 0..3000 {
        println!("{}", "x".repeat(100));
        eprintln!("{}", "y".repeat(100));
    }
}
#[test]
fn permissions_tunnel_checksum_and_redaction() -> anyhow::Result<()> {
    use sha2::{Digest, Sha256};
    use std::os::unix::fs::PermissionsExt;
    let t = tempfile::tempdir()?;
    let p = Profile::isolated(std::fs::canonicalize(t.path())?.join("owned"))?;
    let facade = RuntimeFacade::new(p);
    let c =
        facade.configure_tunnel("tunnel_0123456789abcdef0123456789abcdef", b"fresh-test-key")?;
    assert_eq!(
        std::fs::metadata(c.runtime_key_file)?.permissions().mode() & 0o777,
        0o600
    );
    let asset = "tunnel-client-v0.0.16-darwin-arm64.zip";
    let manifest = format!("{}  {}", hex::encode(Sha256::digest(b"archive")), asset);
    verify_tunnel_archive(asset, b"archive", &manifest)?;
    assert!(verify_tunnel_archive(asset, b"tampered", &manifest).is_err());
    assert_eq!(redact("Authorization: Bearer secret"), "[redacted]");
    Ok(())
}
#[test]
fn both_drain_counters_and_missing_prior() -> anyhow::Result<()> {
    let t = tempfile::tempdir()?;
    let p = Profile::isolated(std::fs::canonicalize(t.path())?.join("owned"))?;
    let journal = IntegrationJournal::new(p.clone());
    journal.install(b"model = 'test'\n")?;
    journal.restore()?;
    assert!(!p.codex_home().join("config.toml").exists());
    let mut supervisor = Supervisor::new(p);
    let a = supervisor.gate.enter(Activity::Http)?;
    let b = supervisor.gate.enter(Activity::Browser)?;
    assert_eq!(supervisor.gate.snapshot()["activeHttp"], 1);
    drop(a);
    assert!(supervisor.shutdown().is_err());
    drop(b);
    supervisor.shutdown()?;
    assert!(supervisor.gate.enter(Activity::Http).is_err());
    Ok(())
}
#[test]
fn restart_budget_and_non_owned_survival() -> anyhow::Result<()> {
    let t = tempfile::tempdir()?;
    let p = Profile::isolated(std::fs::canonicalize(t.path())?.join("owned"))?;
    let mut other = std::process::Command::new("/bin/sleep").arg("30").spawn()?;
    let result = (|| -> anyhow::Result<()> {
        let mut s = Supervisor::new(p);
        s.start(Invocation::new("/usr/bin/false", vec![])?)?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        let mut exhausted = false;
        while std::time::Instant::now() < deadline {
            if s.poll().is_err() {
                exhausted = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(exhausted);
        assert!(s.pid().is_none());
        s.shutdown()?;
        assert!(other.try_wait()?.is_none());
        Ok(())
    })();
    let _ = other.kill();
    let _ = other.wait();
    result
}
#[test]
fn rejects_existing_route_and_root_symlink() -> anyhow::Result<()> {
    let t = tempfile::tempdir()?;
    let base = std::fs::canonicalize(t.path())?;
    let p = Profile::isolated(base.join("owned"))?;
    let bytes = b"openai_base_url = 'http://127.0.0.1:9999/v1'\n";
    std::fs::write(p.codex_home().join("config.toml"), bytes)?;
    assert!(
        IntegrationJournal::new(p.clone())
            .install(b"model = 'new'\n")
            .is_err()
    );
    assert_eq!(std::fs::read(p.codex_home().join("config.toml"))?, bytes);
    std::os::unix::fs::symlink(p.root(), base.join("alias"))?;
    assert!(Profile::isolated(base.join("alias")).is_err());
    Ok(())
}
