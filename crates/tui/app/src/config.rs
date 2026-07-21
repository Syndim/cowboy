use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use cowboy_agent_client::ModelInfo;
use cowboy_workflow_engine::{AgentRuntimeConfig, RunnerLimitsConfig, RuntimeConfig};
use serde::{Deserialize, Serialize};

/// Configuration needed by the new workflow-first TUI shell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AppConfig {
    /// Directory for app/session state.
    pub state_dir: PathBuf,
    /// Redb file that will back workflow run storage once the runner is wired in.
    pub workflow_store: PathBuf,
    /// Named workflow runner policies. The built-in `default` set is always present.
    ///
    /// ```toml
    /// [config_sets.default]
    /// max_steps_per_run = 100
    /// max_visits_per_step = 20
    /// max_retries_per_run = 200
    /// max_retries_per_step = 2
    ///
    /// [config_sets.careful]
    /// max_retries_per_run = 20
    /// max_retries_per_step = 4
    /// ```
    #[serde(
        default = "default_config_sets",
        deserialize_with = "deserialize_config_sets"
    )]
    pub config_sets: BTreeMap<String, ConfigSetConfig>,
    /// Additional workflow roots scanned for `.lua` workflows.
    #[serde(default)]
    pub workflow_dirs: Vec<PathBuf>,
    /// Transcript mouse-wheel visual rows scrolled per wheel detent.
    #[serde(default = "default_mouse_scroll_lines")]
    pub mouse_scroll_lines: u16,
    /// ACP-compatible agent commands used by workflow agent actions.
    #[serde(default = "default_agents")]
    pub agents: Vec<AgentConfig>,
}

/// Effective values for one named workflow runner policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ConfigSetConfig {
    pub max_steps_per_run: u32,
    pub max_visits_per_step: u32,
    pub max_retries_per_run: u32,
    pub max_retries_per_step: u32,
}

impl Default for ConfigSetConfig {
    fn default() -> Self {
        Self {
            max_steps_per_run: 100,
            max_visits_per_step: 20,
            max_retries_per_run: 200,
            max_retries_per_step: 2,
        }
    }
}

fn default_config_sets() -> BTreeMap<String, ConfigSetConfig> {
    BTreeMap::from([("default".to_string(), ConfigSetConfig::default())])
}

fn default_mouse_scroll_lines() -> u16 {
    3
}

fn deserialize_config_sets<'de, D>(
    deserializer: D,
) -> std::result::Result<BTreeMap<String, ConfigSetConfig>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let mut config_sets = BTreeMap::<String, ConfigSetConfig>::deserialize(deserializer)?;
    config_sets
        .entry("default".to_string())
        .or_insert_with(ConfigSetConfig::default);
    Ok(config_sets)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentConfig {
    pub name: String,
    #[serde(default = "default_agent_command")]
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub model: Option<ModelConfig>,
}

fn default_agent_command() -> String {
    "copilot".to_string()
}

fn default_agent_args() -> Vec<String> {
    vec!["--acp".to_string()]
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            name: "default".to_string(),
            command: default_agent_command(),
            args: default_agent_args(),
            model: None,
        }
    }
}

fn default_agents() -> Vec<AgentConfig> {
    vec![AgentConfig::default()]
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
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
            config_sets: default_config_sets(),
            workflow_dirs: vec![config_root().join("workflows")],
            mouse_scroll_lines: default_mouse_scroll_lines(),
            agents: default_agents(),
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
    validate_agents(&config.agents)
        .with_context(|| format!("invalid agent config in {}", path.display()))?;
    validate_config_sets(&config.config_sets)
        .with_context(|| format!("invalid config set in {}", path.display()))?;
    validate_mouse_scroll_lines(config.mouse_scroll_lines)
        .with_context(|| format!("invalid mouse_scroll_lines in {}", path.display()))?;
    config.expand_paths();
    Ok(config)
}

fn validate_mouse_scroll_lines(mouse_scroll_lines: u16) -> Result<()> {
    if mouse_scroll_lines == 0 {
        anyhow::bail!("mouse_scroll_lines must be greater than zero");
    }

    Ok(())
}

fn validate_config_sets(config_sets: &BTreeMap<String, ConfigSetConfig>) -> Result<()> {
    for (name, config_set) in config_sets {
        if name.trim().is_empty() {
            anyhow::bail!("config set name must not be empty");
        }
        if config_set.max_steps_per_run == 0 {
            anyhow::bail!("config set {name:?} max_steps_per_run must be greater than zero");
        }
        if config_set.max_visits_per_step == 0 {
            anyhow::bail!("config set {name:?} max_visits_per_step must be greater than zero");
        }
    }

    Ok(())
}

fn validate_agents(agents: &[AgentConfig]) -> Result<()> {
    use std::collections::BTreeSet;

    let mut names = BTreeSet::new();
    for agent in agents {
        if agent.name.trim().is_empty() {
            anyhow::bail!("agent name must not be empty");
        }
        if !names.insert(agent.name.as_str()) {
            anyhow::bail!("agent names must be unique: {:?}", agent.name);
        }
    }
    Ok(())
}

impl AppConfig {
    pub fn runtime_config(&self, cwd: PathBuf) -> RuntimeConfig {
        let mut config_sets = self
            .config_sets
            .iter()
            .map(|(name, config_set)| {
                (
                    name.clone(),
                    RunnerLimitsConfig {
                        max_steps_per_run: config_set.max_steps_per_run,
                        max_visits_per_step: config_set.max_visits_per_step,
                        max_retries_per_run: config_set.max_retries_per_run,
                        max_retries_per_step: config_set.max_retries_per_step,
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();
        config_sets
            .entry("default".to_string())
            .or_insert_with(RunnerLimitsConfig::default);

        RuntimeConfig::new(
            cwd,
            self.state_dir.clone(),
            self.workflow_store.clone(),
            self.workflow_dirs.clone(),
            self.agents
                .iter()
                .map(|agent| {
                    AgentRuntimeConfig::new(
                        agent.name.clone(),
                        agent.command.clone(),
                        agent.args.clone(),
                        agent.model.clone().map(|model| ModelInfo {
                            id: model.id,
                            provider: model.provider,
                        }),
                    )
                })
                .collect(),
            config_sets,
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
    fn missing_config_uses_default_config_set() {
        let dir = tempfile::tempdir().unwrap();
        let config = load_config(&dir.path().join("missing.toml")).unwrap();
        assert_eq!(config.config_sets.len(), 1);
        assert_eq!(
            config.config_sets["default"],
            ConfigSetConfig {
                max_steps_per_run: 100,
                max_visits_per_step: 20,
                max_retries_per_run: 200,
                max_retries_per_step: 2,
            }
        );
        assert_eq!(config.agents.len(), 1);
        assert_eq!(config.agents[0].name, "default");
        assert_eq!(config.agents[0].command, "copilot");
        assert_eq!(config.mouse_scroll_lines, 3);
    }

    #[test]
    fn explicit_mouse_scroll_lines_parses() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "mouse_scroll_lines = 5\n").unwrap();

        let config = load_config(&path).unwrap();

        assert_eq!(config.mouse_scroll_lines, 5);
    }

    #[test]
    fn mouse_scroll_lines_zero_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "mouse_scroll_lines = 0\n").unwrap();

        let err = load_config(&path).unwrap_err();

        assert!(
            format!("{err:#}").contains("mouse_scroll_lines must be greater than zero"),
            "{err:#}"
        );
    }

    #[test]
    fn unknown_top_level_field_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "unknown_top_level = 1\n").unwrap();

        let err = load_config(&path).unwrap_err();

        assert!(
            format!("{err:#}").contains("unknown field `unknown_top_level`"),
            "{err:#}"
        );
    }

    #[test]
    fn documented_config_sets_parse_with_independent_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(
            &path,
            r#"
[config_sets.default]
max_steps_per_run = 100
max_visits_per_step = 20
max_retries_per_run = 200
max_retries_per_step = 2

[config_sets.careful]
# Omitted step/visit fields inherit 100 and 20.
max_retries_per_run = 20
max_retries_per_step = 4
"#,
        )
        .unwrap();

        let config = load_config(&path).unwrap();
        assert_eq!(config.config_sets.len(), 2);
        assert_eq!(
            config.config_sets["careful"],
            ConfigSetConfig {
                max_retries_per_run: 20,
                max_retries_per_step: 4,
                ..ConfigSetConfig::default()
            }
        );
    }

    #[test]
    fn partial_default_override_and_custom_only_sets_retain_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let default_path = dir.path().join("default.toml");
        fs::write(
            &default_path,
            "[config_sets.default]\nmax_steps_per_run = 7\n",
        )
        .unwrap();
        let config = load_config(&default_path).unwrap();
        assert_eq!(config.config_sets["default"].max_steps_per_run, 7);
        assert_eq!(config.config_sets["default"].max_visits_per_step, 20);

        let custom_path = dir.path().join("custom.toml");
        fs::write(
            &custom_path,
            "[config_sets.fast]\nmax_retries_per_step = 0\n",
        )
        .unwrap();
        let config = load_config(&custom_path).unwrap();
        assert_eq!(config.config_sets["default"], ConfigSetConfig::default());
        assert_eq!(config.config_sets["fast"].max_steps_per_run, 100);
        assert_eq!(config.config_sets["fast"].max_retries_per_run, 200);
        assert_eq!(config.config_sets["fast"].max_retries_per_step, 0);
    }

    #[test]
    fn runtime_conversion_preserves_all_named_sets() {
        let config = AppConfig {
            config_sets: BTreeMap::from([
                ("default".to_string(), ConfigSetConfig::default()),
                (
                    "careful".to_string(),
                    ConfigSetConfig {
                        max_steps_per_run: 9,
                        max_visits_per_step: 8,
                        max_retries_per_run: 7,
                        max_retries_per_step: 6,
                    },
                ),
            ]),
            ..AppConfig::default()
        };

        let runtime = config.runtime_config(PathBuf::from("."));
        assert_eq!(runtime.config_sets.len(), 2);
        assert_eq!(runtime.config_sets["careful"].max_steps_per_run, 9);
        assert_eq!(runtime.config_sets["careful"].max_visits_per_step, 8);
        assert_eq!(runtime.config_sets["careful"].max_retries_per_run, 7);
        assert_eq!(runtime.config_sets["careful"].max_retries_per_step, 6);
    }

    #[test]
    fn runtime_config_does_not_include_mouse_scroll_lines() {
        let slow_mouse = AppConfig {
            mouse_scroll_lines: 1,
            ..AppConfig::default()
        };
        let fast_mouse = AppConfig {
            mouse_scroll_lines: 9,
            ..slow_mouse.clone()
        };

        let slow_runtime = slow_mouse.runtime_config(PathBuf::from("."));
        let fast_runtime = fast_mouse.runtime_config(PathBuf::from("."));

        assert_eq!(
            serde_json::to_value(&slow_runtime).unwrap(),
            serde_json::to_value(&fast_runtime).unwrap()
        );
    }

    #[test]
    fn config_set_validation_rejects_names_fields_and_nonpositive_execution_limits() {
        let dir = tempfile::tempdir().unwrap();
        let cases = [
            (
                "blank.toml",
                "[config_sets.\"   \"]\nmax_retries_per_step = 0\n",
                "config set name must not be empty",
            ),
            (
                "unknown.toml",
                "[config_sets.default]\nunknown = 1\n",
                "unknown field `unknown`",
            ),
            (
                "steps.toml",
                "[config_sets.default]\nmax_steps_per_run = 0\n",
                "max_steps_per_run must be greater than zero",
            ),
            (
                "visits.toml",
                "[config_sets.default]\nmax_visits_per_step = 0\n",
                "max_visits_per_step must be greater than zero",
            ),
        ];

        for (name, raw, expected) in cases {
            let path = dir.path().join(name);
            fs::write(&path, raw).unwrap();
            let err = load_config(&path).unwrap_err();
            assert!(format!("{err:#}").contains(expected), "{err:#}");
        }
    }

    #[test]
    fn removed_top_level_limits_are_rejected_with_config_sets_guidance() {
        let dir = tempfile::tempdir().unwrap();
        for field in [
            "max_steps_per_run",
            "max_visits_per_step",
            "max_retries_per_step",
        ] {
            let path = dir.path().join(format!("{field}.toml"));
            fs::write(&path, format!("{field} = 1\n")).unwrap();
            let err = load_config(&path).unwrap_err();
            let message = format!("{err:#}");
            assert!(message.contains(&format!("unknown field `{field}`")));
            assert!(message.contains("config_sets"));
        }
    }

    #[test]
    fn shipped_demo_config_matches_config_set_contract() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../..")
            .join("demo-config.toml");

        let config = load_config(&path).unwrap();

        assert_eq!(config.config_sets["default"], ConfigSetConfig::default());
        assert_eq!(config.config_sets["careful"].max_steps_per_run, 100);
        assert_eq!(config.config_sets["careful"].max_visits_per_step, 20);
        assert_eq!(config.config_sets["careful"].max_retries_per_run, 20);
        assert_eq!(config.config_sets["careful"].max_retries_per_step, 4);
        assert_eq!(config.mouse_scroll_lines, 3);
    }

    #[test]
    fn parses_named_agents_and_runtime_conversion_preserves_them() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(
            &path,
            r#"
[[agents]]
name = "default"
command = "copilot"
args = ["--acp"]
[agents.model]
id = "opus-4.8-1m"
provider = "github-copilot"

[[agents]]
name = "reviewer"
command = "copilot"
args = ["--acp"]
model = { id = "gpt-5.5-1m", provider = "github-copilot" }
"#,
        )
        .unwrap();

        let config = load_config(&path).unwrap();
        assert_eq!(config.agents.len(), 2);
        assert_eq!(config.agents[0].name, "default");
        assert_eq!(config.agents[0].model.as_ref().unwrap().id, "opus-4.8-1m");
        assert_eq!(
            config.agents[0].model.as_ref().unwrap().provider.as_deref(),
            Some("github-copilot")
        );
        assert_eq!(config.agents[1].name, "reviewer");
        assert_eq!(config.agents[1].model.as_ref().unwrap().id, "gpt-5.5-1m");
        assert_eq!(
            config.agents[1].model.as_ref().unwrap().provider.as_deref(),
            Some("github-copilot")
        );

        let runtime = config.runtime_config(dir.path().to_path_buf());
        assert_eq!(runtime.agents.len(), 2);
        assert_eq!(runtime.agents[0].name, "default");
        assert_eq!(runtime.agents[0].model.as_ref().unwrap().id, "opus-4.8-1m");
        assert_eq!(runtime.agents[1].name, "reviewer");
        assert_eq!(runtime.agents[1].command, "copilot");
        assert_eq!(runtime.agents[1].model.as_ref().unwrap().id, "gpt-5.5-1m");
    }

    #[test]
    fn agent_model_is_optional_and_runtime_preserves_absence() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(
            &path,
            r#"
[[agents]]
name = "default"
command = "copilot"
args = ["--acp", "--model=claude-opus-4.8", "--context=long_context"]
"#,
        )
        .unwrap();

        let config = load_config(&path).unwrap();

        assert!(config.agents[0].model.is_none());
        let runtime = config.runtime_config(dir.path().to_path_buf());
        assert!(runtime.agents[0].model.is_none());
    }

    #[test]
    fn rejects_agent_entry_missing_name() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(
            &path,
            r#"
[[agents]]
command = "copilot"
args = ["--acp"]
"#,
        )
        .unwrap();

        let err = load_config(&path).unwrap_err();

        assert!(err.to_string().contains("failed to parse config"));
        assert!(format!("{err:#}").contains("missing field `name`"));
    }

    #[test]
    fn rejects_blank_agent_name() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(
            &path,
            r#"
[[agents]]
name = "   "
command = "copilot"
"#,
        )
        .unwrap();

        let err = load_config(&path).unwrap_err();

        assert!(err.to_string().contains("invalid agent config"));
        assert!(format!("{err:#}").contains("agent name must not be empty"));
    }

    #[test]
    fn rejects_duplicate_agent_names() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(
            &path,
            r#"
[[agents]]
name = "default"
command = "copilot"

[[agents]]
name = "default"
command = "other"
"#,
        )
        .unwrap();

        let err = load_config(&path).unwrap_err();

        assert!(err.to_string().contains("invalid agent config"));
        assert!(format!("{err:#}").contains("agent names must be unique"));
    }

    #[test]
    fn rejects_legacy_agent_table() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(
            &path,
            r#"
[agent]
command = "copilot"
args = ["--acp"]
"#,
        )
        .unwrap();

        let err = load_config(&path).unwrap_err();

        assert!(err.to_string().contains("failed to parse config"));
        assert!(format!("{err:#}").contains("unknown field `agent`"));
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
