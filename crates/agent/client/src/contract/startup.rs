//! Startup readiness probe for spawned agent backends.
//!
//! Backends that write a readiness marker to a log file can be gated on it
//! before the first prompt is sent. Shared here so every backend transport
//! interprets identical configuration.

use std::path::{Component, Path};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StartupReadiness {
    pub log_path: String,
    pub contains: String,
    #[serde(default = "default_timeout_seconds")]
    pub timeout_seconds: u64,
}

fn default_timeout_seconds() -> u64 {
    300
}

impl StartupReadiness {
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.log_path.trim().is_empty(),
            "startup readiness log_path must not be blank"
        );
        anyhow::ensure!(
            !self.contains.trim().is_empty() && !self.contains.contains(['\n', '\r']),
            "startup readiness contains must be a nonblank single-line marker"
        );
        anyhow::ensure!(
            (1..=300).contains(&self.timeout_seconds),
            "startup readiness timeout_seconds must be between 1 and 300"
        );
        anyhow::ensure!(
            self.log_path.matches("{pid}").count() == 1,
            "startup readiness log_path must contain exactly one {{pid}}"
        );
        anyhow::ensure!(
            !self.log_path.contains("**"),
            "startup readiness log_path does not support recursive wildcards"
        );
        anyhow::ensure!(
            !self.log_path.contains(['?', '[', ']']),
            "startup readiness log_path supports only * wildcards"
        );
        let components = Path::new(&self.log_path).components().collect::<Vec<_>>();
        let index = components
            .iter()
            .position(|part| part.as_os_str().to_string_lossy().contains("{pid}"))
            .unwrap();
        anyhow::ensure!(
            index + 1 < components.len(),
            "startup readiness {{pid}} must identify a directory before the log file"
        );
        anyhow::ensure!(
            components[..index]
                .iter()
                .all(|part| !part.as_os_str().to_string_lossy().contains('*')),
            "startup readiness wildcards must be inside or after the {{pid}} directory"
        );
        anyhow::ensure!(
            components
                .iter()
                .all(|part| !matches!(part, Component::ParentDir)),
            "startup readiness log_path must not contain parent components"
        );
        anyhow::ensure!(
            Path::new(&self.log_path).is_absolute()
                || self.log_path.starts_with("~/")
                || self.log_path.starts_with("~\\"),
            "startup readiness log_path must be absolute or start with ~/"
        );
        Ok(())
    }
}
