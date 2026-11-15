//! Turn watchdog policy shared between Cowboy and agent backends.

use serde::{Deserialize, Serialize};

/// Watchdog timeouts applied to one backend prompt turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AgentWatchdogOptions {
    pub response_timeout_seconds: u64,
    pub cancel_timeout_seconds: u64,
    pub recovery_operation_timeout_seconds: u64,
}

impl Default for AgentWatchdogOptions {
    fn default() -> Self {
        Self {
            response_timeout_seconds: 100,
            cancel_timeout_seconds: 10,
            recovery_operation_timeout_seconds: 30,
        }
    }
}
