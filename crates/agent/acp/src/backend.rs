//! Named presets for launching common ACP coding-agent backends.
//!
//! Both the `acp-chat` test app (`--backend NAME`) and the live integration
//! tests resolve their command/args/model/provider defaults from this single
//! table, so adding a backend only touches one place.

use cowboy_agent_client::ModelInfo;

use crate::transport::{StdioConfig, TransportConfig};

/// How to launch a named ACP backend plus its default model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackendPreset {
    /// Short backend id used by `--backend` and env prefixes (e.g. `copilot`).
    pub name: &'static str,
    /// Executable to spawn.
    pub command: &'static str,
    /// Arguments that put the executable into ACP-server mode.
    pub args: &'static [&'static str],
    /// Default model id.
    pub model: &'static str,
    /// Default provider for the model.
    pub provider: &'static str,
}

impl BackendPreset {
    /// GitHub Copilot CLI (`copilot --acp`).
    pub const COPILOT: BackendPreset = BackendPreset {
        name: "copilot",
        command: "copilot",
        args: &["--acp"],
        model: "claude-sonnet-4.5",
        provider: "anthropic",
    };

    /// Oh My Pi CLI (`omp acp`).
    pub const OMP: BackendPreset = BackendPreset {
        name: "omp",
        command: "omp",
        args: &["acp"],
        model: "claude-sonnet-4.5",
        provider: "github-copilot",
    };

    /// Look up a preset by case-insensitive name.
    pub fn lookup(name: &str) -> Option<&'static BackendPreset> {
        PRESETS
            .iter()
            .find(|preset| preset.name.eq_ignore_ascii_case(name))
    }

    /// Comma-separated known preset names, for help and error text.
    pub fn known_names() -> String {
        PRESETS
            .iter()
            .map(|preset| preset.name)
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// Default args as owned strings.
    pub fn owned_args(&self) -> Vec<String> {
        self.args.iter().map(|arg| (*arg).to_string()).collect()
    }

    /// Stdio transport launching this backend.
    pub fn stdio_transport(&self) -> TransportConfig {
        TransportConfig::Stdio(StdioConfig {
            command: self.command.to_string(),
            args: self.owned_args(),
            env: vec![],
        })
    }

    /// Default model descriptor for this backend.
    pub fn model_info(&self) -> ModelInfo {
        ModelInfo {
            id: self.model.to_string(),
            provider: Some(self.provider.to_string()),
        }
    }
}

/// All built-in backend presets.
pub const PRESETS: &[BackendPreset] = &[BackendPreset::COPILOT, BackendPreset::OMP];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_is_case_insensitive() {
        assert_eq!(BackendPreset::lookup("OMP"), Some(&BackendPreset::OMP));
        assert_eq!(
            BackendPreset::lookup("copilot"),
            Some(&BackendPreset::COPILOT)
        );
        assert_eq!(BackendPreset::lookup("nope"), None);
    }

    #[test]
    fn known_names_lists_every_preset() {
        assert_eq!(BackendPreset::known_names(), "copilot, omp");
    }
}
