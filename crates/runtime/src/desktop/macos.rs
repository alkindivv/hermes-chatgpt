//! Main-app ServiceManagement registration. Never creates a LaunchAgent plist.
use super::*;
use objc2::{
    msg_send,
    runtime::{AnyClass, AnyObject, Bool},
};
use std::ffi::{CStr, c_char};
#[link(name = "ServiceManagement", kind = "framework")]
unsafe extern "C" {}
#[link(name = "Foundation", kind = "framework")]
unsafe extern "C" {}
/// Must run from the signed NEW app bundle and its default isolated profile.
/// Constructing this value does not query or alter login registration.
pub struct NativeLoginItem {
    profile: Profile,
}
impl NativeLoginItem {
    pub fn for_profile(profile: Profile) -> Self {
        Self { profile }
    }
    /// Read-only launch-event detection; call while the app's open-application event is current.
    /// ServiceManagement main-app registration cannot supply arbitrary command-line arguments.
    pub fn was_launched_at_login(&self) -> Result<bool> {
        self.with_service(|_| {
            // SAFETY: NSAppleEventManager/NSAppleEventDescriptor SDK return signatures and
            // AERegistry keyAEPropData/keyAELaunchedAsLogInItem four-character constants.
            unsafe {
                let class = AnyClass::get(c"NSAppleEventManager")
                    .ok_or_else(|| anyhow::anyhow!("Apple event manager unavailable"))?;
                let manager: *mut AnyObject = msg_send![class, sharedAppleEventManager];
                let event: *mut AnyObject = msg_send![manager, currentAppleEvent];
                if event.is_null() {
                    return Ok(false);
                }
                let event_id: u32 = msg_send![event, eventID];
                if event_id != u32::from_be_bytes(*b"oapp") {
                    return Ok(false);
                }
                let property: *mut AnyObject =
                    msg_send![event,paramDescriptorForKeyword:u32::from_be_bytes(*b"prdt")];
                if property.is_null() {
                    return Ok(false);
                }
                let code: u32 = msg_send![property, enumCodeValue];
                Ok(code == u32::from_be_bytes(*b"lgit"))
            }
        })
    }
    fn with_service<T>(&self, run: impl FnOnce(*mut AnyObject) -> Result<T>) -> Result<T> {
        let home = std::env::var_os("HOME").ok_or_else(|| anyhow::anyhow!("HOME missing"))?;
        ensure!(
            self.profile.root() == PathBuf::from(home).join(".codex-web-gpui"),
            "main-app login requires default isolated GPUI profile"
        );
        objc2::rc::autoreleasepool(|_| {
            // SAFETY: Foundation/ServiceManagement classes and signatures are from the local SDK.
            // Every invocation revalidates NSBundle identity before accessing mainAppService.
            unsafe {
                let bundle_class = AnyClass::get(c"NSBundle")
                    .ok_or_else(|| anyhow::anyhow!("NSBundle unavailable"))?;
                let bundle: *mut AnyObject = msg_send![bundle_class, mainBundle];
                let identifier: *mut AnyObject = msg_send![bundle, bundleIdentifier];
                ensure!(
                    !identifier.is_null(),
                    "login item requires packaged GPUI app"
                );
                let identifier: *const c_char = msg_send![identifier, UTF8String];
                ensure!(
                    !identifier.is_null()
                        && CStr::from_ptr(identifier).to_bytes() == APPLICATION_ID.as_bytes(),
                    "foreign application bundle identity"
                );
                let class = AnyClass::get(c"SMAppService")
                    .ok_or_else(|| anyhow::anyhow!("macOS 13+ ServiceManagement required"))?;
                let service: *mut AnyObject = msg_send![class, mainAppService];
                ensure!(!service.is_null(), "main-app service unavailable");
                run(service)
            }
        })
    }
}
impl LoginItemBackend for NativeLoginItem {
    fn identity(&self) -> &str {
        APPLICATION_ID
    }
    fn status(&self) -> Result<LoginItemStatus> {
        self.with_service(|service| {
            // SAFETY: SMAppService.status returns NSInteger per the SDK declaration.
            let state: isize = unsafe { msg_send![service, status] };
            ensure!((0..=3).contains(&state), "unknown login item status");
            Ok(LoginItemStatus {
                supported: state != 3,
                enabled: state == 1,
                requires_approval: state == 2,
            })
        })
    }
    fn set_enabled(&self, enabled: bool) -> Result<()> {
        self.with_service(|service| {
            let mut error: *mut AnyObject = std::ptr::null_mut();
            // SAFETY: SDK methods return BOOL and take an autoreleasing NSError** out pointer.
            let accepted: Bool = unsafe {
                if enabled {
                    msg_send![service,registerAndReturnError:&mut error]
                } else {
                    msg_send![service,unregisterAndReturnError:&mut error]
                }
            };
            ensure!(
                accepted.as_bool(),
                "ServiceManagement rejected own login-item change"
            );
            Ok(())
        })
    }
}

/// Explicit UI action only. Uses the NEW main-app login item, not the app's legacy launchd adapter.
/// Custom profiles fail closed because ServiceManagement cannot preserve their argv.
pub fn set_autostart(enabled: bool, profile: &Profile) -> Result<LoginItemStatus> {
    LaunchOnLogin::new(
        profile.clone(),
        NativeLoginItem::for_profile(profile.clone()),
    )?
    .set_enabled(enabled)
}
/// Read the actual OS registration, never a saved desired preference.
pub fn autostart_status(profile: &Profile) -> Result<LoginItemStatus> {
    LaunchOnLogin::new(
        profile.clone(),
        NativeLoginItem::for_profile(profile.clone()),
    )?
    .status()
}
