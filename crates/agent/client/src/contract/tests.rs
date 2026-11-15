//! Shared backend contract tests.

use super::*;

#[test]
fn agent_access_round_trips_all_variants() {
    for access in [
        AgentAccess::Default,
        AgentAccess::DenyAll,
        AgentAccess::DenyEscalation {
            acp_mode: "read-only".to_string(),
        },
    ] {
        let json = serde_json::to_string(&access).unwrap();
        let parsed: AgentAccess = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, access);
    }
}

#[test]
fn agent_access_rejects_blank_escalation_mode() {
    let error = serde_json::from_str::<AgentAccess>(
        r#"{"mode":"deny_escalation","acp_mode":"  "}"#,
    )
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("deny_escalation acp_mode must not be blank"),
        "{error}"
    );
}

#[test]
fn agent_watchdog_options_apply_defaults() {
    let options: AgentWatchdogOptions = serde_json::from_str("{}").unwrap();
    assert_eq!(
        options,
        AgentWatchdogOptions {
            response_timeout_seconds: 100,
            cancel_timeout_seconds: 10,
            recovery_operation_timeout_seconds: 30,
        }
    );
}

#[test]
fn startup_readiness_rejects_invalid_paths() {
    let mut readiness = StartupReadiness {
        log_path: "~/logs/{{pid}}/agent.log".to_string(),
        contains: "ready".to_string(),
        timeout_seconds: 300,
    };
    assert!(readiness.validate().is_ok());

    readiness.log_path = "relative/{{pid}}/agent.log".to_string();
    assert!(readiness.validate().is_err());

    readiness.log_path = "~/logs/agent.log".to_string();
    assert!(readiness.validate().is_err());
}

#[test]
fn diagnostic_error_text_masks_secret_like_values() {
    assert_eq!(
        diagnostic_error_text("auth failed: api_key: \"sk-abc123\""),
        "auth failed: api_key: [redacted]"
    );
    assert_eq!(
        diagnostic_error_text("bearer eyJhbGciOiJIUzI1NiJ9.secret rejected"),
        "bearer [redacted] rejected"
    );
    assert_eq!(
        diagnostic_error_text("no secrets in this message"),
        "no secrets in this message"
    );
}

#[test]
fn diagnostic_error_text_truncates_agent_replies() {
    assert_eq!(
        diagnostic_error_text("bad reply; agent reply: {\"inner\":\"raw\"}"),
        "bad reply; agent reply: [redacted]"
    );
}
