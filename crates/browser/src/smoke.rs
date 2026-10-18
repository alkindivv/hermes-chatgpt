//! Explicit source-native smoke input. Construction does not navigate, probe, or send.
use crate::{CapabilityInspection, Error, InteractionMode, Ownership, Result, TurnInput};
pub const SMOKE_PROMPT: &str = "Reply with exactly: CODEX WEB GPT READY";
pub const SMOKE_EXPECTED: &str = "CODEX WEB GPT READY";
/// Matches upstream smoke's generic Sol/high or Luna/low selection, without assuming GPT-6.
/// Primary must provide its idle owned target and run submit only after the explicit user action.
pub fn smoke_turn_input(
    inspection: &CapabilityInspection,
    ownership: Ownership,
) -> Result<TurnInput> {
    let (model, effort) = if inspection.capabilities.sol_available {
        ("chatgpt-web/high", "high")
    } else {
        ("chatgpt-web/gpt-5.6-luna", "low")
    };
    crate::selection::resolve_route(model, effort, inspection)?;
    Ok(TurnInput {
        connector_name: None,
        full_prompt: SMOKE_PROMPT.into(),
        model: model.into(),
        effort: effort.into(),
        attachments: vec![],
        interaction_mode: InteractionMode::Automatic,
        ownership,
        turn_token: None,
    })
}
pub fn validate_smoke_response(markdown: &str) -> Result<()> {
    if markdown.trim() != SMOKE_EXPECTED {
        return Err(Error::Dom(
            "smoke response did not match the exact upstream marker".into(),
        ));
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn smoke_requires_exact_source_marker() {
        assert_eq!(SMOKE_PROMPT, "Reply with exactly: CODEX WEB GPT READY");
        assert!(validate_smoke_response(" CODEX WEB GPT READY\n").is_ok());
        assert!(validate_smoke_response("CODEX WEB GPT READY extra").is_err());
    }
}
