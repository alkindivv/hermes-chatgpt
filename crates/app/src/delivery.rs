use cw_bridge::BrowserEvent;
use cw_browser::{Cancellation, Progress, TurnResult};
use std::future::Future;
use tokio::sync::mpsc;

#[derive(Debug)]
pub struct DeliveryError {
    pub message: String,
    pub cleanup_confirmed: bool,
}

impl From<mpsc::error::SendError<BrowserEvent>> for DeliveryError {
    fn from(error: mpsc::error::SendError<BrowserEvent>) -> Self {
        // The browser future has already returned before terminal events are sent.
        Self { message: error.to_string(), cleanup_confirmed: true }
    }
}

/// Connect the browser's mutable previews and terminal result to one Responses turn.
pub async fn relay_browser_output<F>(
    execution: F,
    mut progress: mpsc::UnboundedReceiver<Progress>,
    events: &mpsc::Sender<BrowserEvent>,
    cancellation: &Cancellation,
    mut notify: impl FnMut(Progress),
) -> Result<(), DeliveryError>
where
    F: Future<Output = anyhow::Result<TurnResult>>,
{
    tokio::pin!(execution);
    let mut open = true;
    let outcome = loop {
        tokio::select! {
            result = &mut execution => break result,
            event = progress.recv(), if open => {
                if let Some(event) = event {
                    // These are replaceable previews, not committed Markdown deltas.
                    notify(event);
                } else { open = false; }
            }
        }
    };
    while let Ok(event) = progress.try_recv() {
        notify(event);
    }
    let cleanup_confirmed = !outcome.as_ref().err().is_some_and(|error| {
        matches!(error.downcast_ref::<cw_browser::Error>(), Some(cw_browser::Error::CleanupUnconfirmed))
    });
    if !cleanup_confirmed {
        let message = "The owned browser could not confirm cleanup; its tab remains unavailable".to_owned();
        let _ = events.send(BrowserEvent::Failed {
            code: "browser_cleanup_unconfirmed".into(), message: message.clone(),
        }).await;
        return Err(DeliveryError { message, cleanup_confirmed: false });
    }
    if cancellation.is_cancelled() {
        let _ = events.send(BrowserEvent::Cancelled).await;
        return Err(DeliveryError { message: "Browser turn cancelled".into(), cleanup_confirmed: true });
    }
    match outcome {
        Ok(TurnResult::Completed { text, .. }) => {
            if !text.is_empty() {
                events.send(BrowserEvent::TextDelta(text)).await?;
            }
            events.send(BrowserEvent::Completed { output: vec![], usage: None }).await?;
            Ok(())
        }
        result => {
            let error = match result {
                Err(error) => error.to_string(),
                _ => "Automatic turn unexpectedly returned a manual handoff".into(),
            };
            let event = BrowserEvent::Failed { code: "browser_execution_failed".into(), message: error.clone() };
            let _ = events.send(event).await;
            Err(DeliveryError { message: error, cleanup_confirmed: true })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cw_browser::Ownership;

    #[tokio::test]
    async fn revised_preview_never_leaks_into_final_markdown() {
        let owner = Ownership { task_id: "fixture".into(), epoch: 1 };
        let (preview_tx, preview_rx) = mpsc::unbounded_channel();
        let (events, mut received) = mpsc::channel(16);
        let execution = async {
            preview_tx.send(Progress::Assistant { ownership: owner.clone(), turn_id: "a".into(), text: "Draft answer".into(), html: "<p>Draft answer</p>".into() }).unwrap();
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            preview_tx.send(Progress::Assistant { ownership: owner.clone(), turn_id: "a".into(), text: "Final answer".into(), html: "<p><strong>Final</strong> answer</p>".into() }).unwrap();
            Ok(TurnResult::Completed { ownership: owner, user_turn_id: "u".into(), assistant_turn_id: "a".into(), text: "**Final** answer".into(), html: "<p><strong>Final</strong> answer</p>".into() })
        };
        let mut previews = 0;
        relay_browser_output(execution, preview_rx, &events, &Cancellation::default(), |_| previews += 1).await.unwrap();
        drop(events);
        let mut answer = String::new();
        let mut completed = 0;
        while let Some(event) = received.recv().await {
            match event {
                BrowserEvent::TextDelta(text) => answer.push_str(&text),
                BrowserEvent::Completed { .. } => completed += 1,
                _ => panic!("unexpected terminal event"),
            }
        }
        assert_eq!(previews, 2);
        assert_eq!(answer, "**Final** answer");
        assert_eq!(completed, 1);
    }

    #[tokio::test]
    async fn cancelled_delivery_never_marks_the_turn_completed() {
        let cancellation = Cancellation::default();
        cancellation.cancel();
        let (_tx, rx) = mpsc::unbounded_channel();
        let (events, mut received) = mpsc::channel(4);
        assert!(relay_browser_output(async { Err(anyhow::anyhow!("cancelled")) }, rx, &events, &cancellation, |_| {}).await.is_err());
        drop(events);
        assert!(matches!(received.recv().await, Some(BrowserEvent::Cancelled)));
        assert!(received.recv().await.is_none());
    }

    #[tokio::test]
    async fn unconfirmed_cleanup_is_reported_as_failure_even_after_cancellation() {
        let cancellation = Cancellation::default();
        cancellation.cancel();
        let (_tx, rx) = mpsc::unbounded_channel();
        let (events, mut received) = mpsc::channel(4);
        let result = relay_browser_output(
            async { Err(anyhow::Error::new(cw_browser::Error::CleanupUnconfirmed)) },
            rx, &events, &cancellation, |_| {},
        ).await;
        assert!(result.is_err());
        assert!(matches!(received.recv().await, Some(BrowserEvent::Failed { code, .. }) if code == "browser_cleanup_unconfirmed"));
    }

    #[tokio::test]
    async fn cancellation_suppresses_an_undelivered_terminal_answer() {
        let cancellation = Cancellation::default();
        cancellation.cancel();
        let (_tx, rx) = mpsc::unbounded_channel();
        let (events, mut received) = mpsc::channel(4);
        let result = relay_browser_output(
            async { Ok(TurnResult::Completed {
                ownership: Ownership { task_id: "fixture".into(), epoch: 1 },
                user_turn_id: "u".into(), assistant_turn_id: "a".into(),
                text: "Undelivered answer".into(), html: "<p>Undelivered answer</p>".into(),
            }) }, rx, &events, &cancellation, |_| {},
        ).await;
        drop(events);
        assert!(result.is_err());
        assert!(matches!(received.recv().await, Some(BrowserEvent::Cancelled)));
        assert!(received.recv().await.is_none());
    }
}
