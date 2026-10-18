#[cfg(target_os = "macos")]
fn main() -> anyhow::Result<()> {
    use cef::build_util::mac::{BundleInfo, build_bundle};
    let app_crate = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let workspace = app_crate.join("../..").canonicalize()?;
    // CEF invokes cargo without -p; select this package, not the workspace defaults.
    std::env::set_current_dir(app_crate)?;
    let out = workspace.join("dist");
    std::fs::create_dir_all(&out)?;
    let info = BundleInfo::new(
        "codex-web-gpui",
        "dev.alkindi.codex-web-gpui",
        "Codex Web GPUI",
        "en",
        semver::Version::new(0, 1, 0),
    );
    let app = build_bundle(&out, "codex-web-gpui", info)
        .map_err(|e| anyhow::anyhow!("CEF bundle failed: {e:?}"))?;
    let resources = app.join("Contents/Resources");
    std::fs::create_dir_all(&resources)?;
    for name in ["UPSTREAM-LICENSE", "GPUI-LICENSE"] {
        std::fs::copy(workspace.join("assets").join(name), resources.join(name))?;
    }
    std::fs::copy(workspace.join("upstream.lock.json"), resources.join("upstream.lock.json"))?;
    println!("{}", app.display());
    Ok(())
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("This bundle target supports macOS only.");
}
