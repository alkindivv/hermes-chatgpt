//! Launcher decisions derived from upstream App.tsx.

pub fn initial_surface(manual: bool, core_ready: bool, catalog_ready: bool) -> &'static str {
    if manual && !core_ready {
        "mcp"
    } else if core_ready && (manual || catalog_ready) {
        "browser"
    } else {
        "setup"
    }
}

pub fn can_finish_welcome(github_opened: bool, x_opened: bool, busy: bool) -> bool {
    github_opened && x_opened && !busy
}

pub fn manual_deadline_ms(compaction: bool, copied_at_ms: u64) -> u64 {
    copied_at_ms.saturating_add(if compaction { 120_000 } else { 60_000 })
}

pub fn session_reminder_due(authenticated: bool, reminder: Option<&str>, now: u64) -> bool {
    authenticated && reminder
        .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
        .is_some_and(|value| i128::from(value.timestamp_millis()) <= i128::from(now))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_manual_setup_routes_to_mcp_without_claiming_catalog_ready() {
        assert_eq!(initial_surface(true, false, false), "mcp");
        assert_eq!(initial_surface(true, true, false), "browser");
    }

    #[test]
    fn automatic_route_requires_both_core_and_verified_catalog() {
        assert_eq!(initial_surface(false, false, false), "setup");
        assert_eq!(initial_surface(false, true, false), "setup");
        assert_eq!(initial_surface(false, true, true), "browser");
    }

    #[test]
    fn welcome_requires_both_social_links_and_no_pending_write() {
        assert!(!can_finish_welcome(false, false, false));
        assert!(!can_finish_welcome(true, false, false));
        assert!(!can_finish_welcome(true, true, true));
        assert!(can_finish_welcome(true, true, false));
    }

    #[test]
    fn copy_restarts_manual_handoff_deadline_for_the_correct_turn_kind() {
        assert_eq!(manual_deadline_ms(false, 10_000), 70_000);
        assert_eq!(manual_deadline_ms(true, 10_000), 130_000);
        assert_eq!(manual_deadline_ms(false, 35_000), 95_000);
    }

    #[test]
    fn session_reminder_requires_verified_session_and_a_due_valid_deadline() {
        let at = "2026-10-18T12:00:00Z";
        let now = chrono::DateTime::parse_from_rfc3339(at).unwrap().timestamp_millis() as u64;
        assert!(!session_reminder_due(true, Some(at), now - 1));
        assert!(session_reminder_due(true, Some(at), now));
        assert!(session_reminder_due(true, Some(at), now + 1));
        assert!(!session_reminder_due(false, Some(at), now + 1));
        assert!(!session_reminder_due(true, None, now));
        assert!(!session_reminder_due(true, Some("invalid"), now));
    }
}
