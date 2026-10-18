//! Owned-frame upload processing receipts, following upstream attachment-upload.ts.
use crate::{Cancellation, CdpClient, Error, Result};
use base64::Engine;
use serde_json::{Value, json};
use std::collections::{HashMap, HashSet};
use std::time::Duration;
use tokio::sync::broadcast;
pub(crate) struct Uploads {
    events: broadcast::Receiver<Value>,
    frame: String,
    names: HashSet<String>,
    requests: HashMap<String, (String, bool)>,
    completed: HashSet<String>,
}
impl Uploads {
    pub async fn start(
        client: &CdpClient,
        names: &[String],
        cancel: &Cancellation,
    ) -> Result<Self> {
        let events = client.events();
        let tree = client
            .request(
                "Page.getFrameTree",
                json!({}),
                Duration::from_secs(5),
                cancel,
            )
            .await?;
        let frame = tree["frameTree"]["frame"]["id"]
            .as_str()
            .ok_or_else(|| Error::Dom("missing upload frame".into()))?
            .into();
        if names.iter().collect::<HashSet<_>>().len() != names.len() {
            return Err(Error::Dom("duplicate attachment names".into()));
        }
        client
            .request("Network.enable", json!({}), Duration::from_secs(5), cancel)
            .await?;
        Ok(Self {
            events,
            frame,
            names: names.iter().cloned().collect(),
            requests: HashMap::new(),
            completed: HashSet::new(),
        })
    }
    pub fn complete(&self) -> bool {
        self.names == self.completed
    }
    pub async fn poll(&mut self, client: &CdpClient, cancel: &Cancellation) -> Result<()> {
        loop {
            let event = match self.events.try_recv() {
                Ok(v) => v,
                Err(broadcast::error::TryRecvError::Empty) => break,
                Err(_) => return Err(Error::Dom("upload network observation lost".into())),
            };
            let p = &event["params"];
            let id = p["requestId"].as_str().unwrap_or("");
            match event["method"].as_str().unwrap_or("") {
                "Network.requestWillBeSent"
                    if p["frameId"] == self.frame && p["request"]["method"] == "POST" =>
                {
                    let request = &p["request"];
                    let Ok(url) = url::Url::parse(request["url"].as_str().unwrap_or("")) else {
                        continue;
                    };
                    if url.origin().ascii_serialization() != "https://chatgpt.com" {
                        continue;
                    }
                    let creation = url.path() == "/backend-api/files";
                    let processing = url.path() == "/backend-api/files/process_upload_stream"
                        || (url
                            .path()
                            .starts_with("/backend-api/files/upload_reservations/")
                            && url.path().ends_with("/claim_and_finish"));
                    if !creation && !processing {
                        continue;
                    }
                    let Ok(body) =
                        serde_json::from_str::<Value>(request["postData"].as_str().unwrap_or(""))
                    else {
                        continue;
                    };
                    if let Some(name) = body["file_name"]
                        .as_str()
                        .filter(|s| self.names.contains(*s))
                    {
                        self.requests.insert(id.into(), (name.into(), creation));
                    }
                }
                "Network.responseReceived" if self.requests.contains_key(id) => {
                    let status = p["response"]["status"].as_u64().unwrap_or(0);
                    if !(200..300).contains(&status) {
                        return Err(Error::Dom(format!("attachment HTTP {status}")));
                    }
                }
                "Network.loadingFailed" if self.requests.contains_key(id) => {
                    return Err(Error::Dom("attachment connection failed".into()));
                }
                "Network.loadingFinished" => {
                    let Some((name, creation)) = self.requests.remove(id) else {
                        continue;
                    };
                    if creation {
                        continue;
                    }
                    let body = client
                        .request(
                            "Network.getResponseBody",
                            json!({"requestId":id}),
                            Duration::from_secs(5),
                            cancel,
                        )
                        .await?;
                    let text = body["body"].as_str().unwrap_or("");
                    let decoded = if body["base64Encoded"] == true {
                        String::from_utf8(
                            base64::engine::general_purpose::STANDARD
                                .decode(text)
                                .map_err(|e| Error::Dom(e.to_string()))?,
                        )
                        .map_err(|e| Error::Dom(e.to_string()))?
                    } else {
                        text.into()
                    };
                    if !processing_completed(&decoded)? {
                        return Err(Error::Dom(
                            "upload ended without processing completion".into(),
                        ));
                    }
                    self.completed.insert(name);
                }
                _ => {}
            }
        }
        Ok(())
    }
}
fn processing_completed(text: &str) -> Result<bool> {
    let mut complete = false;
    for line in text.lines() {
        let Ok(envelope) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        let data = envelope
            .get("data")
            .filter(|v| v.is_object())
            .unwrap_or(&envelope);
        let event = data["event"]
            .as_str()
            .or(envelope["event"].as_str())
            .unwrap_or("");
        if [".error", ".failed", ".cancelled", ".unknown"]
            .iter()
            .any(|s| event.ends_with(s))
        {
            return Err(Error::Dom(format!("upload {event}")));
        }
        if event == "file.processing.completed" {
            complete = true
        }
    }
    Ok(complete)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn receipts() {
        assert!(processing_completed("{\"event\":\"file.processing.completed\"}").unwrap());
        assert!(!processing_completed("{\"id\":\"created\"}").unwrap());
        assert!(processing_completed("{\"data\":{\"event\":\"file.processing.failed\"}}").is_err());
    }
}
