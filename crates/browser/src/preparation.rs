//! One owned editor transaction, shared by ordinary turns and checkpoints.
use crate::*;

pub(super) struct PreparedSubmission {
    pub baseline: Snapshot,
    connector_path: Option<String>,
}

impl BrowserEngine {
    pub(super) async fn prepare_submission(
        &self,
        input: &TurnInput,
        cancellation: &Cancellation,
        state: &mut State,
    ) -> Result<PreparedSubmission> {
        self.wait_ready_inner(Duration::from_secs(30), cancellation)
            .await?;
        let inspection = self
            .inspect_capabilities_inner(CapabilityOptions::default(), cancellation)
            .await?;
        let route = selection::resolve_route(&input.model, &input.effort, &inspection)?;
        self.select_route(&route, cancellation).await?;
        let baseline = self
            .snapshot(input.turn_token.as_deref(), cancellation)
            .await?;
        if baseline.running {
            return Err(Error::Dom("generation already running".into()));
        }
        let name = input.connector_name.as_deref().unwrap_or("");
        let before = self.connector_snapshot(name, cancellation).await?;
        before.require_available(input.connector_name.as_deref(), &baseline.document_id)?;
        let mut prepared = PreparedSubmission {
            baseline,
            connector_path: None,
        };
        let result: Result<()> = async {
            if let Some(name) = &input.connector_name {
                self.select_connector(name, &prepared.baseline.document_id, cancellation, state)
                    .await?;
                let selected = self.connector_snapshot(name, cancellation).await?;
                selected.require_document(&prepared.baseline.document_id)?;
                prepared.connector_path = Some(selected.selected_path(name)?);
            }
            self.page(
                "focus",
                json!({"document_id":prepared.baseline.document_id}),
                cancellation,
            )
            .await?;
            // This flag stays set across all awaits, including when the whole future is dropped.
            state.cleanup_unconfirmed = true;
            self.client
                .request(
                    "Input.insertText",
                    json!({"text":input.full_prompt}),
                    Duration::from_secs(10),
                    cancellation,
                )
                .await?;
            self.page("verifyPrompt", json!(input.full_prompt), cancellation)
                .await?;
            self.attach(&input.attachments, cancellation).await?;
            Ok(())
        }
        .await;
        if let Err(error) = result {
            self.rollback_preparation(input, &prepared.baseline, state)
                .await?;
            return Err(error);
        }
        Ok(prepared)
    }

    pub(super) async fn rollback_preparation(
        &self,
        input: &TurnInput,
        baseline: &Snapshot,
        state: &mut State,
    ) -> Result<()> {
        if !state.cleanup_unconfirmed {
            return Ok(());
        }
        // Caller cancellation must never cancel rollback. A timeout/drop leaves the flag armed.
        let cleanup = Cancellation::default();
        let result = tokio::time::timeout(Duration::from_secs(10), async {
            if !input.attachments.is_empty() {
                let mut names = Vec::new();
                for attachment in &input.attachments {
                    let path = tokio::fs::canonicalize(&attachment.path)
                        .await
                        .unwrap_or_else(|_| attachment.path.clone());
                    if let Some(name) = path.file_name() {
                        names.push(name.to_string_lossy().into_owned());
                    }
                }
                if self
                    .page(
                        "clearAttachments",
                        json!({"document_id":baseline.document_id,"names":names}),
                        &cleanup,
                    )
                    .await?
                    != true
                {
                    return Err(Error::CleanupUnconfirmed);
                }
            }
            self.clear_connector_draft(
                input.connector_name.as_deref().unwrap_or(""),
                &baseline.document_id,
                &cleanup,
            )
            .await?;
            let final_state = self
                .connector_snapshot(input.connector_name.as_deref().unwrap_or(""), &cleanup)
                .await?;
            final_state.require_available(None, &baseline.document_id)?;
            Ok(())
        })
        .await;
        if !matches!(result, Ok(Ok(()))) {
            return Err(Error::CleanupUnconfirmed);
        }
        state.cleanup_unconfirmed = false;
        Ok(())
    }

    pub(super) async fn send_prepared(
        &self,
        input: &TurnInput,
        prepared: &PreparedSubmission,
        cancel: &Cancellation,
    ) -> Result<()> {
        self.page(
            "send",
            json!({
                "document_id":prepared.baseline.document_id,
                "full_prompt":input.full_prompt,
                "connector_name":input.connector_name,
                "connector_path":prepared.connector_path,
            }),
            cancel,
        )
        .await?;
        Ok(())
    }
}
