use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use cowboy_workflow_engine::{AgentRuntimeConfig, RunnerLimitsConfig, RuntimeConfig};
use serde::{Deserialize, Serialize};

/// Configuration needed by the new workflow-first TUI shell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    /// Directory for app/session state.
    pub state_dir: PathBuf,
    /// Redb file that will back workflow run storage once the runner is wired in.
    pub workflow_store: PathBuf,
    /// Maximum workflow actions handled in one run.
    pub max_steps_per_run: u32,
    /// Maximum visits to one workflow step in a single run.
    pub max_visits_per_step: u32,
    /// Additional workflow roots scanned for `.lua` workflows.
    #[serde(default)]
    pub workflow_dirs: Vec<PathBuf>,
    /// ACP-compatible agent command used by workflow agent actions.
    pub agent: AgentConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentConfig {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub model: ModelConfig,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            command: "copilot".to_string(),
            args: vec!["--acp".to_string()],
            model: ModelConfig::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelConfig {
    pub id: String,
    pub provider: Option<String>,
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            id: "claude-sonnet-4.5".to_string(),
            provider: Some("anthropic".to_string()),
        }
    }
}

impl Default for AppConfig {
    fn default() -> Self {
        let state_dir = state_root();
        Self {
            workflow_store: state_dir.join("workflow.redb"),
            state_dir,
            max_steps_per_run: 100,
            max_visits_per_step: 20,
            workflow_dirs: vec![config_root().join("workflows")],
            agent: AgentConfig::default(),
        }
    }
}

/// Default config path: `~/.config/cowboy/config.toml` on every platform
/// (honoring `XDG_CONFIG_HOME`).
pub fn default_config_path() -> PathBuf {
    config_root().join("config.toml")
}

/// `~/.config/cowboy` on every platform (honoring `XDG_CONFIG_HOME`).
fn config_root() -> PathBuf {
    xdg_base("XDG_CONFIG_HOME", &[".config"]).join("cowboy")
}

/// `~/.local/state/cowboy` on every platform (honoring `XDG_STATE_HOME`).
fn state_root() -> PathBuf {
    xdg_base("XDG_STATE_HOME", &[".local", "state"]).join("cowboy")
}

/// Resolve an XDG base dir: `env_var` when set, else `$HOME` + `default_segments`.
fn xdg_base(env_var: &str, default_segments: &[&str]) -> PathBuf {
    if let Some(dir) = std::env::var_os(env_var).filter(|value| !value.is_empty()) {
        return PathBuf::from(dir);
    }
    let mut base = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    for segment in default_segments {
        base.push(segment);
    }
    base
}

/// Load a TOML config if it exists; otherwise return conservative defaults.
pub fn load_config(path: &Path) -> Result<AppConfig> {
    if !path.exists() {
        return Ok(AppConfig::default());
    }
    let raw = fs::read_to_string(path)
        .with_context(|| format!("failed to read config {}", path.display()))?;
    let mut config: AppConfig = toml::from_str(&raw)
        .with_context(|| format!("failed to parse config {}", path.display()))?;
    config.expand_paths();
    Ok(config)
}

impl AppConfig {
    pub fn runtime_config(&self, cwd: PathBuf) -> RuntimeConfig {
        RuntimeConfig::new(
            cwd,
            self.state_dir.clone(),
            self.workflow_store.clone(),
            self.workflow_dirs.clone(),
            AgentRuntimeConfig::new(
                self.agent.command.clone(),
                self.agent.args.clone(),
                self.agent.model.id.clone(),
                self.agent.model.provider.clone(),
            ),
            RunnerLimitsConfig {
                max_steps_per_run: self.max_steps_per_run,
                max_visits_per_step: self.max_visits_per_step,
            },
        )
    }

    fn expand_paths(&mut self) {
        self.state_dir = expand_tilde(&self.state_dir);
        self.workflow_store = expand_tilde(&self.workflow_store);
        self.workflow_dirs = self
            .workflow_dirs
            .iter()
            .map(|path| expand_tilde(path))
            .collect();
    }
}

fn expand_tilde(path: &Path) -> PathBuf {
    let raw = path.to_string_lossy();
    if raw == "~" {
        return dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        return dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(rest);
    }
    path.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_config_uses_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let config = load_config(&dir.path().join("missing.toml")).unwrap();
        assert_eq!(config.max_steps_per_run, 100);
        assert_eq!(config.max_visits_per_step, 20);
    }

    #[test]
    fn parses_config_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(
            &path,
            r#"
state_dir = "/tmp/cowboy-state"
workflow_store = "/tmp/cowboy-state/workflow.redb"
max_steps_per_run = 7
max_visits_per_step = 3
"#,
        )
        .unwrap();

        let config = load_config(&path).unwrap();
        assert_eq!(config.max_steps_per_run, 7);
        assert_eq!(config.max_visits_per_step, 3);
    }

    #[test]
    fn defaults_use_config_root_for_config_and_user_workflows() {
        let config_path = default_config_path();
        assert!(config_path.ends_with("cowboy/config.toml"));
        // Never the macOS "Application Support" location.
        assert!(
            !config_path
                .to_string_lossy()
                .contains("Application Support")
        );

        let defaults = AppConfig::default();
        let user_workflows = config_path.parent().unwrap().join("workflows");
        assert!(defaults.workflow_dirs.contains(&user_workflows));
        assert!(
            !defaults
                .state_dir
                .to_string_lossy()
                .contains("Application Support")
        );
    }
}
