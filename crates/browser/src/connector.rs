use crate::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConnectorReceipt {
    pub name: String,
    pub document_id: String,
    pub composer_cleared: bool,
}

impl BrowserEngine {
    /// Proves exact selection and then removes the owned draft; never submits a message.
    pub async fn verify_connector(
        &self,
        name: &str,
        mode: InteractionMode,
        cancel: Cancellation,
    ) -> Result<ConnectorReceipt> {
        if mode == InteractionMode::Manual {
            return Err(Error::ManualDomForbidden);
        }
        validate_name(name)?;
        let mut state = tokio::select! { biased; _=cancel.cancelled()=>return Err(Error::Cancelled), state=self.state.lock()=>state };
        if state.cleanup_unconfirmed {
            return Err(Error::CleanupUnconfirmed);
        }
        self.wait_ready_inner(Duration::from_secs(30), &cancel)
            .await?;
        let before = self.connector_snapshot(name, &cancel).await?;
        before.require_available(Some(name), &before.document_id)?;
        let result = self
            .select_connector(name, &before.document_id, &cancel, &mut state)
            .await;
        if result.is_err() && !state.cleanup_unconfirmed {
            return result.map(|_| unreachable!());
        }
        // Verification explicitly clears a matching selection, including a reused matching pill.
        state.cleanup_unconfirmed = true;
        if self
            .clear_connector_draft(name, &before.document_id, &Cancellation::default())
            .await
            .is_err()
        {
            return Err(Error::CleanupUnconfirmed);
        }
        state.cleanup_unconfirmed = false;
        result?;
        Ok(ConnectorReceipt {
            name: name.into(),
            document_id: before.document_id,
            composer_cleared: true,
        })
    }

    pub(crate) async fn connector_snapshot(
        &self,
        name: &str,
        cancel: &Cancellation,
    ) -> Result<ConnectorSnapshot> {
        serde_json::from_value(
            self.page("connectorSnapshot", json!({"name":name}), cancel)
                .await?,
        )
        .map_err(|_| Error::Dom("Connector observation was incomplete".into()))
    }

    async fn connector_key(
        &self,
        key: &str,
        code: &str,
        virtual_key: u32,
        modifiers: u32,
        cancel: &Cancellation,
    ) -> Result<()> {
        for kind in ["keyDown", "keyUp"] {
            self.client.request("Input.dispatchKeyEvent", json!({"type":kind,"key":key,"code":code,
                "windowsVirtualKeyCode":virtual_key,"nativeVirtualKeyCode":virtual_key,"modifiers":modifiers}),
                Duration::from_secs(5), cancel).await?;
        }
        Ok(())
    }

    pub(crate) async fn clear_connector_draft(
        &self,
        name: &str,
        document: &str,
        cancel: &Cancellation,
    ) -> Result<()> {
        let observed = self.connector_snapshot(name, cancel).await?;
        observed.require_document(document)?;
        self.connector_key("Escape", "Escape", 27, 0, cancel)
            .await?;
        self.page("connectorFocus", json!({"document_id":document}), cancel)
            .await?;
        self.connector_key("a", "KeyA", 65, 4, cancel).await?;
        self.connector_key("Backspace", "Backspace", 8, 0, cancel)
            .await?;
        pause(Duration::from_millis(150), cancel).await?;
        let cleared = self.connector_snapshot(name, cancel).await?;
        cleared.require_document(document)?;
        if !cleared.text.trim().is_empty() || !cleared.selected.is_empty() {
            return Err(Error::Dom(
                "Connector cleanup did not leave an empty composer".into(),
            ));
        }
        Ok(())
    }

    pub(crate) async fn select_connector(
        &self,
        name: &str,
        document: &str,
        cancel: &Cancellation,
        state: &mut State,
    ) -> Result<()> {
        validate_name(name)?;
        let before = self.connector_snapshot(name, cancel).await?;
        before.require_available(Some(name), document)?;
        if before.exact_selected(name)? {
            return Ok(());
        }
        for attempt in 0..3 {
            // Arm before the first owned edit. Dropping the future cannot release a dirty page.
            state.cleanup_unconfirmed = true;
            self.clear_connector_draft(name, document, cancel).await?;
            self.page("connectorFocus", json!({"document_id":document}), cancel)
                .await?;
            for letter in "@codex".chars() {
                self.client
                    .request(
                        "Input.insertText",
                        json!({"text":letter.to_string()}),
                        Duration::from_secs(5),
                        cancel,
                    )
                    .await?;
                pause(Duration::from_millis(25), cancel).await?;
            }
            let deadline = tokio::time::Instant::now() + Duration::from_millis(2500);
            let mut observed = loop {
                let observed = self.connector_snapshot(name, cancel).await?;
                observed.require_document(document)?;
                if observed.text.trim() != "@codex" {
                    return Err(Error::Dom(
                        "The composer did not retain the complete connector mention".into(),
                    ));
                }
                if observed.exact_row(name)?.is_some_and(|row| !row.disabled) {
                    break Some(observed);
                }
                if tokio::time::Instant::now() >= deadline {
                    break None;
                }
                pause(Duration::from_millis(100), cancel).await?;
            };
            if let Some(mut observed) = observed.take() {
                let rows = observed.rows.len();
                for _ in 0..rows {
                    if observed
                        .exact_row(name)?
                        .is_some_and(|row| row.highlighted && !row.disabled)
                    {
                        break;
                    }
                    self.connector_key("ArrowDown", "ArrowDown", 40, 0, cancel)
                        .await?;
                    observed = self.connector_snapshot(name, cancel).await?;
                    observed.require_document(document)?;
                    observed.exact_row(name)?;
                }
                // Refresh immediately before activation, even if the initial row was highlighted.
                observed = self.connector_snapshot(name, cancel).await?;
                observed.require_document(document)?;
                if observed.text.trim() != "@codex"
                    || !observed.selected.is_empty()
                    || !observed
                        .exact_row(name)?
                        .is_some_and(|row| row.highlighted && !row.disabled)
                {
                    return Err(Error::Dom(
                        "The exact connector could not take keyboard selection".into(),
                    ));
                }
                self.connector_key("Enter", "Enter", 13, 0, cancel).await?;
                let deadline = tokio::time::Instant::now() + Duration::from_millis(2500);
                loop {
                    let selected = self.connector_snapshot(name, cancel).await?;
                    selected.require_document(document)?;
                    if selected.exact_selected(name)? && selected.text.trim().is_empty() {
                        return Ok(());
                    }
                    if !selected.selected.is_empty() || tokio::time::Instant::now() >= deadline {
                        return Err(Error::Dom(
                            "The exact connector pill was not confirmed".into(),
                        ));
                    }
                    pause(Duration::from_millis(100), cancel).await?;
                }
            }
            if attempt == 2 {
                return Err(Error::Dom(format!(
                    "No exact connector named {name:?} was found after three mention attempts"
                )));
            }
        }
        unreachable!("bounded connector attempt loop")
    }
}

#[derive(Deserialize)]
pub(crate) struct ConnectorSnapshot {
    pub document_id: String,
    pub text: String,
    selected: Vec<SelectedConnector>,
    rows: Vec<MentionRow>,
    pub has_attachments: bool,
    #[serde(default)]
    running: bool,
}
#[derive(Deserialize)]
struct SelectedConnector {
    name: String,
    path: String,
}
#[derive(Deserialize)]
struct MentionRow {
    name: String,
    highlighted: bool,
    disabled: bool,
}
impl ConnectorSnapshot {
    pub(super) fn require_document(&self, expected: &str) -> Result<()> {
        if self.document_id.is_empty() || self.document_id != expected || self.running {
            return Err(Error::Dom(
                "Connector document changed or a generation started".into(),
            ));
        }
        Ok(())
    }
    pub(super) fn require_available(&self, name: Option<&str>, document: &str) -> Result<()> {
        self.require_document(document)?;
        if !self.text.trim().is_empty() || self.has_attachments {
            return Err(Error::Dom(
                "The owned composer already contains a user draft or attachments".into(),
            ));
        }
        if let Some(name) = name {
            validate_name(name)?;
        }
        if !self.selected.is_empty()
            && !name.is_some_and(|name| self.exact_selected(name).unwrap_or(false))
        {
            return Err(Error::Dom(
                "A different or ambiguous connector is already selected".into(),
            ));
        }
        Ok(())
    }
    pub(super) fn selected_path(&self, name: &str) -> Result<String> {
        if !self.exact_selected(name)? {
            return Err(Error::Dom(
                "The exact connector pill was not confirmed".into(),
            ));
        }
        Ok(self.selected[0].path.clone())
    }
    fn exact_row(&self, name: &str) -> Result<Option<&MentionRow>> {
        let mut matches = self.rows.iter().filter(|row| row.name == name);
        let first = matches.next();
        if matches.next().is_some() {
            return Err(Error::Dom(
                "Connector menu exposed duplicate exact names".into(),
            ));
        }
        Ok(first)
    }
    fn exact_selected(&self, name: &str) -> Result<bool> {
        if self.selected.len() > 1 {
            return Err(Error::Dom(
                "Composer has ambiguous connector selections".into(),
            ));
        }
        Ok(self.selected.first().is_some_and(|selected| {
            selected.name == name
                && (selected.path.starts_with("app://") || selected.path.starts_with("plugin:"))
        }))
    }
}
fn validate_name(name: &str) -> Result<()> {
    if name.trim() != name
        || name.is_empty()
        || name.chars().count() > 128
        || name.chars().any(char::is_control)
    {
        return Err(Error::Dom(
            "Connector name must be one nonempty line of at most 128 characters".into(),
        ));
    }
    Ok(())
}
async fn pause(duration: Duration, cancel: &Cancellation) -> Result<()> {
    tokio::select! { biased; _=cancel.cancelled()=>Err(Error::Cancelled), _=tokio::time::sleep(duration)=>Ok(()) }
}
