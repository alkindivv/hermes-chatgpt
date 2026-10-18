use gpui::{AssetSource, SharedString};
use std::{borrow::Cow, sync::LazyLock};

pub struct Assets;
impl AssetSource for Assets {
    fn load(&self, path: &str) -> anyhow::Result<Option<Cow<'static, [u8]>>> {
        Ok(match path {
            "icons/activity.svg" => Some(Cow::Borrowed(include_bytes!(
                "../../../assets/icons/activity.svg"
            ))),
            "icons/alert.svg" => Some(Cow::Borrowed(include_bytes!(
                "../../../assets/icons/alert.svg"
            ))),
            "icons/back.svg" => Some(Cow::Borrowed(include_bytes!(
                "../../../assets/icons/back.svg"
            ))),
            "icons/brand.svg" => Some(Cow::Borrowed(include_bytes!(
                "../../../assets/icons/brand.svg"
            ))),
            "icons/browser.svg" => Some(Cow::Borrowed(include_bytes!(
                "../../../assets/icons/browser.svg"
            ))),
            "icons/check.svg" => Some(Cow::Borrowed(include_bytes!(
                "../../../assets/icons/check.svg"
            ))),
            "icons/chevron.svg" => Some(Cow::Borrowed(include_bytes!(
                "../../../assets/icons/chevron.svg"
            ))),
            "icons/close.svg" => Some(Cow::Borrowed(include_bytes!(
                "../../../assets/icons/close.svg"
            ))),
            "icons/expand.svg" => Some(Cow::Borrowed(include_bytes!(
                "../../../assets/icons/expand.svg"
            ))),
            "icons/external.svg" => Some(Cow::Borrowed(include_bytes!(
                "../../../assets/icons/external.svg"
            ))),
            "icons/forward.svg" => Some(Cow::Borrowed(include_bytes!(
                "../../../assets/icons/forward.svg"
            ))),
            "icons/github.svg" => Some(Cow::Borrowed(include_bytes!(
                "../../../assets/icons/github.svg"
            ))),
            "icons/globe.svg" => Some(Cow::Borrowed(include_bytes!(
                "../../../assets/icons/globe.svg"
            ))),
            "icons/info.svg" => Some(Cow::Borrowed(include_bytes!(
                "../../../assets/icons/info.svg"
            ))),
            "icons/logs.svg" => Some(Cow::Borrowed(include_bytes!(
                "../../../assets/icons/logs.svg"
            ))),
            "icons/mcp.svg" => Some(Cow::Borrowed(include_bytes!(
                "../../../assets/icons/mcp.svg"
            ))),
            "icons/minus.svg" => Some(Cow::Borrowed(include_bytes!(
                "../../../assets/icons/minus.svg"
            ))),
            "icons/pause.svg" => Some(Cow::Borrowed(include_bytes!(
                "../../../assets/icons/pause.svg"
            ))),
            "icons/plus.svg" => Some(Cow::Borrowed(include_bytes!(
                "../../../assets/icons/plus.svg"
            ))),
            "icons/reload.svg" => Some(Cow::Borrowed(include_bytes!(
                "../../../assets/icons/reload.svg"
            ))),
            "icons/settings.svg" => Some(Cow::Borrowed(include_bytes!(
                "../../../assets/icons/settings.svg"
            ))),
            "icons/setup.svg" => Some(Cow::Borrowed(include_bytes!(
                "../../../assets/icons/setup.svg"
            ))),
            "icons/sidebar.svg" => Some(Cow::Borrowed(include_bytes!(
                "../../../assets/icons/sidebar.svg"
            ))),
            "icons/update.svg" => Some(Cow::Borrowed(include_bytes!(
                "../../../assets/icons/update.svg"
            ))),
            "icons/x.svg" => Some(Cow::Borrowed(include_bytes!("../../../assets/icons/x.svg"))),
            _ => None,
        })
    }
    fn list(&self, _path: &str) -> anyhow::Result<Vec<SharedString>> {
        Ok(Vec::new())
    }
}
pub static COPY: LazyLock<serde_json::Value> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../../../assets/copy.json")).expect("checked upstream copy")
});
pub static LANGUAGES: LazyLock<serde_json::Value> = LazyLock::new(|| {
    serde_json::from_str(include_str!("../../../assets/languages.json"))
        .expect("checked upstream languages")
});
pub fn text(language: &str, key: &str) -> String {
    COPY.get(language)
        .or_else(|| COPY.get("en"))
        .and_then(|c| c.get(key))
        .and_then(|v| v.as_str())
        .unwrap_or(key)
        .to_owned()
}
