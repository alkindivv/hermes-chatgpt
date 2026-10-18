//! CEF-owned child process entrypoint, adapted from gpui-cef under MIT/Apache-2.0.
#[cfg(target_os = "macos")]
fn main() {
    use cef::{App, api_hash, args::Args, execute_process, library_loader, sys};
    let args = Args::new();
    let _sandbox = {
        let mut sandbox = cef::sandbox::Sandbox::new();
        sandbox.initialize(args.as_main_args());
        sandbox
    };
    let _loader = {
        let loader = library_loader::LibraryLoader::new(
            &std::env::current_exe().expect("helper executable path"),
            true,
        );
        assert!(loader.load(), "cannot load the bundled CEF framework");
        loader
    };
    let _ = api_hash(sys::CEF_API_VERSION_LAST, 0);
    let code = execute_process(
        Some(args.as_main_args()),
        None::<&mut App>,
        std::ptr::null_mut(),
    );
    std::process::exit(code);
}

#[cfg(not(target_os = "macos"))]
fn main() {
    eprintln!("This CEF helper supports macOS only.");
}
