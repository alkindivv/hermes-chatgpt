//! Bounded content-free observations. HTTP replies are prepared, not delivery receipts.
use super::*;
use std::collections::VecDeque;
#[derive(Clone, Debug, Serialize)]
pub struct McpObservation {
    pub sequence: u64,
    pub event: String,
    pub tool: Option<String>,
    pub elapsed_ms: Option<u64>,
    pub call: Option<u64>,
    pub outcome: Option<String>,
    pub is_error: Option<bool>,
}
struct Entry {
    cancel: watch::Sender<Option<String>>,
    started: Instant,
    tool: String,
    ambiguous: bool,
    call: u64,
}
#[derive(Default)]
pub(super) struct Ledger {
    sequence: u64,
    events: VecDeque<McpObservation>,
    pending: HashMap<(String, String), Entry>,
}
impl Ledger {
    pub(super) fn clear_idle(&mut self) {
        self.pending.clear();
    }
    fn event(&mut self, event: &str, tool: Option<String>, elapsed_ms: Option<u64>) {
        self.sequence += 1;
        self.events.push_back(McpObservation {
            sequence: self.sequence,
            event: event.into(),
            tool,
            elapsed_ms,
            call: None,
            outcome: None,
            is_error: None,
        });
        if self.events.len() > 1024 {
            self.events.pop_front();
        }
    }
    pub(super) fn cancel(&mut self, scope: &str, id: &Value) {
        let key = (scope.into(), id.to_string());
        if let Some(entry) = self.pending.get(&key) {
            if !entry.ambiguous {
                entry
                    .cancel
                    .send_replace(Some("MCP request cancelled".into()));
                self.event("cancellation_observed", None, None);
                return;
            }
        }
        self.event("cancellation_uncorrelated", None, None);
    }
}
pub(super) struct Activity {
    inner: Arc<Inner>,
    key: Option<(String, String)>,
    pub cancellation: watch::Receiver<Option<String>>,
    _sender: watch::Sender<Option<String>>,
    finished: bool,
}
impl Activity {
    pub(super) fn begin(inner: Arc<Inner>, body: &Value) -> Self {
        let args = &body["params"]["arguments"];
        let scope = args["turn_token"]
            .as_str()
            .or_else(|| args["request_id"].as_str());
        let id = &body["id"];
        let (sender, cancellation) = watch::channel(None);
        let mut key = None;
        let valid = scope.is_some_and(|scope| {
            inner
                .turns
                .lock()
                .unwrap()
                .values()
                .any(|t| t.token == scope && validate_turn(t, None).is_ok())
        });
        let mut ledger = inner.mcp_ledger.lock().unwrap();
        let tool = body["params"]["name"]
            .as_str()
            .filter(|n| {
                super::mcp_tools(inner.execution_mode())
                    .iter()
                    .any(|t| t["name"] == *n)
            })
            .unwrap_or("unknown")
            .to_owned();
        if valid && (id.is_string() || id.is_number()) {
            let k = (scope.unwrap().into(), id.to_string());
            if let Some(entry) = ledger.pending.get_mut(&k) {
                entry.ambiguous = true;
                ledger.event("uncorrelated_duplicate_id", Some(tool), None);
            } else if ledger.pending.len() < 1024 {
                let call = ledger.sequence + 1;
                ledger.pending.insert(
                    k.clone(),
                    Entry {
                        cancel: sender.clone(),
                        started: Instant::now(),
                        tool: tool.clone(),
                        ambiguous: false,
                        call,
                    },
                );
                ledger.event("call_received", Some(tool), None);
                ledger.events.back_mut().unwrap().call = Some(call);
                key = Some(k);
            } else {
                ledger.event("uncorrelated_tracking_limit", Some(tool), None);
            }
        } else {
            ledger.event("uncorrelated_call", Some(tool), None);
        }
        drop(ledger);
        Self {
            inner,
            key,
            cancellation,
            _sender: sender,
            finished: false,
        }
    }
    pub(super) fn prepared(&mut self, result: &Result<Value, String>) {
        self.finished = true;
        let call = self.key.as_ref().and_then(|key| {
            self.inner
                .mcp_ledger
                .lock()
                .unwrap()
                .pending
                .get(key)
                .filter(|e| !e.ambiguous)
                .map(|e| e.call)
        });
        self.settle("reply_prepared");
        if call.is_none() {
            return;
        }
        if let Some(event) = self
            .inner
            .mcp_ledger
            .lock()
            .unwrap()
            .events
            .iter_mut()
            .rev()
            .find(|e| e.event == "reply_prepared" && e.call == call)
        {
            event.outcome = Some(
                if result.is_ok() {
                    "result"
                } else {
                    "protocol_error"
                }
                .into(),
            );
            event.is_error = result.as_ref().ok().map(|v| v["isError"] == true);
        }
    }
    fn settle(&mut self, event: &str) {
        if let Some(key) = self.key.take() {
            let mut ledger = self.inner.mcp_ledger.lock().unwrap();
            if ledger.pending.get(&key).is_some_and(|e| e.ambiguous) {
                return;
            }
            if let Some(entry) = ledger.pending.remove(&key) {
                if !entry.ambiguous {
                    ledger.event(
                        event,
                        Some(entry.tool),
                        Some(entry.started.elapsed().as_millis().min(u64::MAX as u128) as u64),
                    );
                    ledger.events.back_mut().unwrap().call = Some(entry.call);
                }
            }
        }
    }
}
impl Drop for Activity {
    fn drop(&mut self) {
        if !self.finished {
            self.settle("request_dropped");
        }
    }
}
impl BridgeHandle {
    pub fn mcp_observations(&self) -> Vec<McpObservation> {
        self.inner
            .mcp_ledger
            .lock()
            .unwrap()
            .events
            .iter()
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn observations_are_bounded_and_content_free() {
        let mut ledger = Ledger::default();
        for _ in 0..1030 {
            ledger.event("uncorrelated_call", Some("unknown".into()), None);
        }
        assert_eq!(ledger.events.len(), 1024);
        assert_eq!(ledger.events.front().unwrap().sequence, 7);
    }
}
