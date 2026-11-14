use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use cowboy_agent_client::ModelInfo;
use cowboy_workflow_engine::{
    AgentAccess, AgentRuntimeConfig, AgentStartupReadiness, AgentWatchdogRuntimeConfig,
    RunnerLimitsConfig, RuntimeConfig,
};
use serde::{Deserialize, Serialize};

/// Configuration needed by the new workflow-first TUI shell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AppConfig {
    /// Directory for app/session state.
    pub state_dir: PathBuf,
    /// SQLite database that stores workflow runtime state.
    pub workflow_store: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_dir: Option<PathBuf>,
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
    /// Ambient environment variables forwarded to every workflow child process.
    #[serde(default = "default_allowed_env")]
    pub allowed_env: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allowed_cmds: Option<Vec<String>>,
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
    #[serde(default)]
    pub access: AgentAccess,
    #[serde(default)]
    pub allowed_env: Vec<String>,
    #[serde(default)]
    pub watchdog: AgentWatchdogConfig,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub startup_readiness: Option<AgentStartupReadiness>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AgentWatchdogConfig {
    pub response_timeout_seconds: u64,
    pub cancel_timeout_seconds: u64,
    pub recovery_operation_timeout_seconds: u64,
}

impl Default for AgentWatchdogConfig {
    fn default() -> Self {
        Self {
            response_timeout_seconds: 100,
            cancel_timeout_seconds: 10,
            recovery_operation_timeout_seconds: 30,
        }
    }
}

fn default_agent_command() -> String {
    "copilot".to_string()
}

fn default_agent_args() -> Vec<String> {
    vec!["--acp".to_string()]
}

fn default_allowed_env() -> Vec<String> {
    [
        "PATH",
        "PATHEXT",
        "SystemRoot",
        "USERPROFILE",
        "LOCALAPPDATA",
        "APPDATA",
        "TEMP",
        "TMP",
        "HOME",
    ]
    .map(str::to_string)
    .to_vec()
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self {
            name: "default".to_string(),
            command: default_agent_command(),
            args: default_agent_args(),
            model: None,
            access: AgentAccess::default(),
            allowed_env: Vec::new(),
            watchdog: AgentWatchdogConfig::default(),
            startup_readiness: None,
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
            workflow_store: state_dir.join("data.db"),
            state_dir,
            log_dir: None,
            config_sets: default_config_sets(),
            workflow_dirs: vec![config_root().join("workflows")],
            mouse_scroll_lines: default_mouse_scroll_lines(),
            allowed_env: default_allowed_env(),
            allowed_cmds: None,
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
    validate_allowed_env("allowed_env", &config.allowed_env)
        .with_context(|| format!("invalid environment config in {}", path.display()))?;
    validate_allowed_cmds(config.allowed_cmds.as_deref())
        .with_context(|| format!("invalid allowed_cmds in {}", path.display()))?;
    validate_config_sets(&config.config_sets)
        .with_context(|| format!("invalid config set in {}", path.display()))?;
    validate_mouse_scroll_lines(config.mouse_scroll_lines)
        .with_context(|| format!("invalid mouse_scroll_lines in {}", path.display()))?;
    validate_log_dir(config.log_dir.as_deref())
        .with_context(|| format!("invalid log_dir in {}", path.display()))?;
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
        if let AgentAccess::DenyEscalation { acp_mode } = &agent.access
            && acp_mode.trim().is_empty()
        {
            anyhow::bail!("agent {:?} access acp_mode must not be blank", agent.name);
        }
        if let Some(readiness) = &agent.startup_readiness {
            readiness
                .validate()
                .with_context(|| format!("agent {:?} invalid startup_readiness", agent.name))?;
        }
        validate_allowed_env(
            &format!("agent {:?} allowed_env", agent.name),
            &agent.allowed_env,
        )?;
        for (field, value) in [
            (
                "response_timeout_seconds",
                agent.watchdog.response_timeout_seconds,
            ),
            (
                "cancel_timeout_seconds",
                agent.watchdog.cancel_timeout_seconds,
            ),
            (
                "recovery_operation_timeout_seconds",
                agent.watchdog.recovery_operation_timeout_seconds,
            ),
        ] {
            if value == 0 {
                anyhow::bail!("agent {:?} {field} must be greater than zero", agent.name);
            }
        }
    }
    Ok(())
}

fn validate_allowed_env(field: &str, allowed_env: &[String]) -> Result<()> {
    use std::collections::BTreeSet;

    let mut names = BTreeSet::new();
    for name in allowed_env {
        if name.trim().is_empty() {
            anyhow::bail!("{field} entries must not be empty");
        }
        if name.contains('=') {
            anyhow::bail!("{field} entry {name:?} must not contain '='");
        }
        if name.contains('\0') {
            anyhow::bail!("{field} entry must not contain NUL");
        }
        if !names.insert(name.as_str()) {
            anyhow::bail!("{field} contains duplicate entry {name:?}");
        }
    }
    Ok(())
}

fn validate_log_dir(log_dir: Option<&Path>) -> Result<()> {
    if log_dir.is_some_and(|path| path.as_os_str().is_empty()) {
        anyhow::bail!("log_dir must not be blank");
    }

    Ok(())
}

fn validate_allowed_cmds(allowed_cmds: Option<&[String]>) -> Result<()> {
    use std::collections::BTreeSet;

    let mut programs = BTreeSet::new();
    for program in allowed_cmds.into_iter().flatten() {
        if program.trim().is_empty() {
            anyhow::bail!("allowed_cmds entries must not be blank");
        }
        if program.contains('\0') {
            anyhow::bail!("allowed_cmds entry must not contain NUL");
        }
        if !programs.insert(program.as_str()) {
            anyhow::bail!("allowed_cmds contains duplicate entry {program:?}");
        }
    }
    Ok(())
}

impl AppConfig {
    pub fn resolved_log_dir(&self) -> PathBuf {
        self.log_dir
            .clone()
            .unwrap_or_else(|| self.state_dir.join("logs"))
    }

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
            self.allowed_env.clone(),
            self.agents
                .iter()
                .map(|agent| {
                    let mut runtime = AgentRuntimeConfig::new(
                        agent.name.clone(),
                        agent.command.clone(),
                        agent.args.clone(),
                        agent.model.clone().map(|model| ModelInfo {
                            id: model.id,
                            provider: model.provider,
                        }),
                    );
                    runtime.access = agent.access.clone();
                    runtime.watchdog = AgentWatchdogRuntimeConfig {
                        response_timeout_seconds: agent.watchdog.response_timeout_seconds,
                        cancel_timeout_seconds: agent.watchdog.cancel_timeout_seconds,
                        recovery_operation_timeout_seconds: agent
                            .watchdog
                            .recovery_operation_timeout_seconds,
                    };
                    runtime.allowed_env = agent.allowed_env.clone();
                    runtime.startup_readiness = agent.startup_readiness.clone();
                    runtime
                })
                .collect(),
            config_sets,
        )
        .with_allowed_cmds(self.allowed_cmds.clone())
    }

    fn expand_paths(&mut self) {
        self.state_dir = expand_tilde(&self.state_dir);
        self.workflow_store = expand_tilde(&self.workflow_store);
        self.log_dir = self.log_dir.as_deref().map(expand_tilde);
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
    use std::process::Command;

    #[test]
    fn log_dir_omitted_uses_state_logs_without_serializing() {
        let config = AppConfig::default();

        assert_eq!(config.log_dir, None);
        assert_eq!(config.resolved_log_dir(), config.state_dir.join("logs"));
        assert!(
            toml::to_string(&config)
                .unwrap()
                .lines()
                .all(|line| !line.starts_with("log_dir"))
        );
        assert!(
            serde_json::to_value(&config)
                .unwrap()
                .get("log_dir")
                .is_none()
        );
    }

    #[test]
    fn log_dir_explicitly_configures_independent_logs_and_serializes() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let log_dir = dir.path().join("logs");
        fs::write(
            &path,
            format!(
                "state_dir = {}\nworkflow_store = {}\nlog_dir = {}\n",
                serde_json::to_string(&dir.path().join("state").to_string_lossy()).unwrap(),
                serde_json::to_string(&dir.path().join("state/data.db").to_string_lossy()).unwrap(),
                serde_json::to_string(&log_dir.to_string_lossy()).unwrap(),
            ),
        )
        .unwrap();

        let config = load_config(&path).unwrap();
        assert_eq!(config.log_dir, Some(log_dir.clone()));
        assert_eq!(config.resolved_log_dir(), log_dir);
        assert!(toml::to_string(&config).unwrap().contains("log_dir"));
        assert!(
            serde_json::to_value(&config)
                .unwrap()
                .get("log_dir")
                .is_some()
        );
        assert!(
            serde_json::to_value(config.runtime_config(dir.path().to_path_buf()))
                .unwrap()
                .get("log_dir")
                .is_none()
        );
    }

    #[test]
    fn log_dir_expands_tilde_when_loaded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "log_dir = '~/cowboy-logs'\n").unwrap();

        let config = load_config(&path).unwrap();
        assert_eq!(
            config.log_dir,
            Some(
                dirs::home_dir()
                    .unwrap_or_else(|| PathBuf::from("."))
                    .join("cowboy-logs")
            )
        );
    }

    #[test]
    fn log_dir_empty_value_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "log_dir = ''\n").unwrap();

        let error = load_config(&path).unwrap_err();

        assert!(
            error
                .root_cause()
                .to_string()
                .contains("log_dir must not be blank")
        );
    }

    #[test]
    fn log_dir_missing_from_existing_config_is_backward_compatible() {
        let config: AppConfig = toml::from_str(
            "state_dir = 'state'\nworkflow_store = 'state/data.db'\nworkflow_dirs = []\n",
        )
        .unwrap();

        assert_eq!(config.log_dir, None);
        assert_eq!(config.resolved_log_dir(), PathBuf::from("state/logs"));
    }

    #[test]
    fn allowed_cmds_missing_config_defaults_to_unrestricted() {
        let dir = tempfile::tempdir().unwrap();
        for config in [
            AppConfig::default(),
            load_config(&dir.path().join("missing.toml")).unwrap(),
        ] {
            assert_eq!(config.allowed_cmds, None);
            assert_eq!(
                config.runtime_config(dir.path().to_path_buf()).allowed_cmds,
                None
            );
        }
    }

    #[test]
    fn allowed_cmds_omitted_empty_and_nonempty_reach_runtime_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let programs = vec![
            "git".to_string(),
            "Git".to_string(),
            " git ".to_string(),
            "./git".to_string(),
            "/usr/bin/git".to_string(),
            "tool=name".to_string(),
            "~/bin/tool".to_string(),
        ];
        for (raw, expected) in [
            ("", None),
            ("allowed_cmds = []\n", Some(Vec::new())),
            (
                "allowed_cmds = ['git', 'Git', ' git ', './git', '/usr/bin/git', 'tool=name', '~/bin/tool']\n",
                Some(programs),
            ),
        ] {
            fs::write(
                &path,
                format!("{raw}[[agents]]\nname = 'default'\ncommand = 'unlisted-agent'\nargs = ['--acp']\n"),
            )
            .unwrap();
            let config = load_config(&path).unwrap();
            assert_eq!(config.allowed_cmds, expected);
            let runtime = config.runtime_config(dir.path().to_path_buf());
            assert_eq!(runtime.allowed_cmds, expected);
            assert_eq!(runtime.agents[0].command, "unlisted-agent");
            assert_eq!(runtime.agents[0].args, ["--acp"]);
            assert_eq!(config.allowed_cmds, expected);
        }
    }

    #[test]
    fn allowed_cmds_roundtrip_preserves_omitted_empty_and_nonempty() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        for allowed_cmds in [
            None,
            Some(Vec::new()),
            Some(vec![
                "git".to_string(),
                "Git".to_string(),
                " git ".to_string(),
            ]),
        ] {
            let config = AppConfig {
                allowed_cmds: allowed_cmds.clone(),
                ..AppConfig::default()
            };
            let serialized = toml::to_string(&config).unwrap();
            let value: toml::Value = toml::from_str(&serialized).unwrap();
            assert_eq!(value.get("allowed_cmds").is_some(), allowed_cmds.is_some());
            fs::write(&path, serialized).unwrap();
            let loaded = load_config(&path).unwrap();
            assert_eq!(loaded.allowed_cmds, allowed_cmds);
            assert_eq!(
                loaded.runtime_config(dir.path().to_path_buf()).allowed_cmds,
                allowed_cmds
            );

            let json = serde_json::to_value(&config).unwrap();
            assert_eq!(json.get("allowed_cmds").is_some(), allowed_cmds.is_some());
            let decoded: AppConfig = serde_json::from_value(json).unwrap();
            assert_eq!(decoded, config);
        }
    }

    #[test]
    fn allowed_cmds_invalid_types_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        for value in [
            "'git'",
            "42",
            "1.5",
            "false",
            "{}",
            "{ program = 'git' }",
            "[42]",
            "['git', false]",
            "[['git']]",
            "[{ program = 'git' }]",
        ] {
            fs::write(&path, format!("allowed_cmds = {value}\n")).unwrap();
            let error = load_config(&path).unwrap_err();
            assert!(
                format!("{error:#}").contains("allowed_cmds"),
                "{value}: {error:#}"
            );
        }
    }

    #[test]
    fn allowed_cmds_invalid_entries_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        for (value, expected) in [
            (r#"[""]"#, "blank"),
            (r#"["   "]"#, "blank"),
            (r#"["\t\r\n"]"#, "blank"),
            (r#"["\u2003"]"#, "blank"),
            (r#"["\u0000"]"#, "NUL"),
            (r#"["git\u0000"]"#, "NUL"),
            (r#"["\u0000git"]"#, "NUL"),
            (r#"["gi\u0000t"]"#, "NUL"),
            (r#"["git", "git"]"#, "duplicate"),
            (r#"["git", "other", "git"]"#, "duplicate"),
            (r#"[" git ", " git "]"#, "duplicate"),
            (r#"["./git", "./git"]"#, "duplicate"),
        ] {
            fs::write(&path, format!("allowed_cmds = {value}\n")).unwrap();
            let error = load_config(&path).unwrap_err();
            let message = format!("{error:#}");
            assert!(message.contains("allowed_cmds"), "{value}: {message}");
            assert!(message.contains(expected), "{value}: {message}");
        }
    }

    #[test]
    fn configurable_allowed_env_parses_and_reaches_runtime() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(
            &path,
            r#"
allowed_env = ["COWBOY_TEST_GLOBAL", "PATH"]

[[agents]]
name = "default"
allowed_env = ["COWBOY_TEST_DEFAULT"]
command = "copilot"

[[agents]]
name = "planner"
command = "copilot"
allowed_env = ["COWBOY_TEST_PLANNER"]
[agents.model]
id = "planner-model"
provider = "configured-provider"
"#,
        )
        .unwrap();

        let config = load_config(&path).unwrap();
        assert_eq!(config.allowed_env, ["COWBOY_TEST_GLOBAL", "PATH"]);
        assert_eq!(config.agents[0].allowed_env, ["COWBOY_TEST_DEFAULT"]);
        assert_eq!(config.agents[1].allowed_env, ["COWBOY_TEST_PLANNER"]);

        let runtime = config.runtime_config(dir.path().to_path_buf());
        assert_eq!(runtime.allowed_env, ["COWBOY_TEST_GLOBAL", "PATH"]);
        assert_eq!(runtime.agents[0].allowed_env, ["COWBOY_TEST_DEFAULT"]);
        assert_eq!(runtime.agents[1].allowed_env, ["COWBOY_TEST_PLANNER"]);
    }

    #[test]
    fn named_agent_access_parses_and_reaches_runtime_independently_of_model() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(
            &path,
            r#"[[agents]]
name = "default"

[[agents]]
name = "reviewer"
access = { mode = "deny_escalation", acp_mode = "read-only" }
[agents.model]
id = "review-model"
provider = "configured-provider"

[[agents]]
name = "implementer"
access = "deny_all"
"#,
        )
        .unwrap();

        let config = load_config(&path).unwrap();
        let runtime = config.runtime_config(dir.path().to_path_buf());
        let expected = [
            AgentAccess::Default,
            AgentAccess::DenyEscalation {
                acp_mode: "read-only".to_string(),
            },
            AgentAccess::DenyAll,
        ];
        for (index, access) in expected.into_iter().enumerate() {
            assert_eq!(config.agents[index].access, access);
            assert_eq!(runtime.agents[index].access, access);
        }
        assert_eq!(runtime.agents[1].model.as_ref().unwrap().id, "review-model");
    }

    #[test]
    fn omitted_access_and_explicit_default_have_the_same_runtime_policy() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(
            &path,
            "[[agents]]\nname = \"default\"\n[[agents]]\nname = \"second\"\naccess = \"default\"\n",
        )
        .unwrap();

        let config = load_config(&path).unwrap();
        let runtime = config.runtime_config(dir.path().to_path_buf());
        for agent in &config.agents {
            assert_eq!(agent.access, AgentAccess::Default);
        }
        for agent in &runtime.agents {
            assert_eq!(agent.access, AgentAccess::Default);
        }
    }

    #[test]
    fn blank_access_acp_mode_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        for mode in ["", "  "] {
            fs::write(
                &path,
                format!(
                    "[[agents]]\nname = \"reviewer\"\naccess = {{ mode = \"deny_escalation\", acp_mode = {mode:?} }}\n"
                ),
            )
            .unwrap();
            let error = load_config(&path).unwrap_err();
            assert!(format!("{error:#}").contains("acp_mode"), "{error:#}");
        }
    }

    #[test]
    fn obsolete_agent_policy_keys_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        for old_key in [
            "allow_tools = false",
            "permission_requests = \"deny\"",
            "required_acp_mode = \"read-only\"",
        ] {
            fs::write(
                &path,
                format!("[[agents]]\nname = \"default\"\n{old_key}\n"),
            )
            .unwrap();
            let error = load_config(&path).unwrap_err();
            assert!(format!("{error:#}").contains("unknown field"), "{error:#}");
        }
    }

    #[test]
    fn malformed_agent_access_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        for value in [
            "\"deny\"",
            "{ mode = \"deny_escalation\" }",
            "{ mode = \"deny_escalation\", acp_mode = 42 }",
            "{ mode = \"deny_escalation\", acp_mode = \"read-only\", extra = true }",
            "{ mode = \"unknown\", acp_mode = \"read-only\" }",
        ] {
            fs::write(
                &path,
                format!("[[agents]]\nname = \"default\"\naccess = {value}\n"),
            )
            .unwrap();
            assert!(load_config(&path).is_err(), "accepted access = {value}");
        }
    }

    #[test]
    fn allowed_env_omission_preserves_platform_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(
            &path,
            "[[agents]]\nname = \"default\"\ncommand = \"copilot\"\n",
        )
        .unwrap();

        let config = load_config(&path).unwrap();
        assert_eq!(config.allowed_env, default_allowed_env());
        assert!(config.agents[0].allowed_env.is_empty());

        let runtime = config.runtime_config(dir.path().to_path_buf());
        assert_eq!(runtime.allowed_env, default_allowed_env());
        assert!(runtime.agents[0].allowed_env.is_empty());
    }

    #[test]
    fn explicit_empty_allowed_env_is_preserved() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(
            &path,
            "allowed_env = []\n[[agents]]\nname = \"default\"\nallowed_env = []\n",
        )
        .unwrap();

        let config = load_config(&path).unwrap();
        assert!(config.allowed_env.is_empty());
        assert!(config.agents[0].allowed_env.is_empty());

        let runtime = config.runtime_config(dir.path().to_path_buf());
        assert!(runtime.allowed_env.is_empty());
        assert!(runtime.agents[0].allowed_env.is_empty());
    }

    #[test]
    fn rejects_invalid_allowed_env() {
        let dir = tempfile::tempdir().unwrap();
        let cases = [
            (
                "global-empty.toml",
                "allowed_env = [\"\"]\n",
                "allowed_env entries must not be empty",
            ),
            (
                "global-equals.toml",
                "allowed_env = [\"BAD=NAME\"]\n",
                "allowed_env entry \"BAD=NAME\" must not contain '='",
            ),
            (
                "global-duplicate.toml",
                "allowed_env = [\"PATH\", \"PATH\"]\n",
                "allowed_env contains duplicate entry \"PATH\"",
            ),
            (
                "agent-empty.toml",
                "[[agents]]\nname = \"default\"\nallowed_env = [\"   \"]\n",
                "agent \"default\" allowed_env entries must not be empty",
            ),
            (
                "agent-duplicate.toml",
                "[[agents]]\nname = \"default\"\nallowed_env = [\"TOKEN\", \"TOKEN\"]\n",
                "agent \"default\" allowed_env contains duplicate entry \"TOKEN\"",
            ),
        ];

        for (name, raw, expected) in cases {
            let path = dir.path().join(name);
            fs::write(&path, raw).unwrap();
            let error = load_config(&path).unwrap_err();
            assert!(format!("{error:#}").contains(expected), "{error:#}");
        }

        let error = validate_allowed_env("allowed_env", &["BAD\0NAME".to_string()]).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("allowed_env entry must not contain NUL")
        );
    }

    #[test]
    fn runtime_config_serialization_contains_environment_names_not_values() {
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "config::tests::runtime_config_serialization_environment_probe",
                "--ignored",
                "--nocapture",
            ])
            .env("COWBOY_TEST_GLOBAL", "global-secret-marker")
            .env("COWBOY_TEST_ROLE", "role-secret-marker")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "serialization probe failed:\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    #[ignore]
    fn runtime_config_serialization_environment_probe() {
        let config = AppConfig {
            allowed_env: vec!["COWBOY_TEST_GLOBAL".to_string()],
            agents: vec![AgentConfig {
                allowed_env: vec!["COWBOY_TEST_ROLE".to_string()],
                ..AgentConfig::default()
            }],
            ..AppConfig::default()
        };
        let app_json = serde_json::to_string(&config).unwrap();
        let runtime_json =
            serde_json::to_string(&config.runtime_config(PathBuf::from("."))).unwrap();

        for serialized in [&app_json, &runtime_json] {
            assert!(serialized.contains("COWBOY_TEST_GLOBAL"));
            assert!(serialized.contains("COWBOY_TEST_ROLE"));
            assert!(!serialized.contains("global-secret-marker"));
            assert!(!serialized.contains("role-secret-marker"));
        }
    }

    #[test]
    fn missing_config_uses_sqlite_store_default() {
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
        assert_eq!(config.agents[0].watchdog, AgentWatchdogConfig::default());
        assert_eq!(config.mouse_scroll_lines, 3);
        assert_eq!(config.workflow_store, config.state_dir.join("data.db"));
        println!("EVIDENCE config-default workflow_store=data.db");
    }

    #[tokio::test]
    async fn non_sqlite_store_file_is_rejected_without_modification() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("legacy.redb");
        let before = b"legacy-redb-placeholder\n".to_vec();
        fs::write(&path, &before).unwrap();
        let config = AppConfig {
            state_dir: dir.path().join("state"),
            workflow_store: path.clone(),
            ..AppConfig::default()
        };

        let result = cowboy_workflow_engine::WorkflowRuntime::new(
            config.runtime_config(dir.path().to_path_buf()),
        )
        .await;
        let Err(error) = result else {
            panic!("non-SQLite workflow store unexpectedly opened")
        };

        assert!(error.to_string().contains("not a SQLite database"));
        assert_eq!(fs::read(path).unwrap(), before);
        println!("EVIDENCE clean-cutover rejected=true bytes_unchanged=true");
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
    fn shipped_demo_config_defines_workflow_agents() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../..")
            .join("demo-config.toml");

        let config = load_config(&path).unwrap();
        let assert_agent = |name: &str, expected_args: &[&str]| {
            let agent = config
                .agents
                .iter()
                .find(|agent| agent.name == name)
                .unwrap_or_else(|| panic!("demo config should define the {name} agent"));

            assert_eq!(agent.command, "omp");
            assert_eq!(agent.args, expected_args);
        };

        assert_agent("default", &["--thinking=auto", "acp"]);
        assert_agent(
            "planner",
            &[
                "--model=github-copilot/claude-opus-4.8",
                "--thinking=xhigh",
                "acp",
            ],
        );
        assert_agent(
            "reviewer",
            &[
                "--model=github-copilot/gpt-5.6-sol",
                "--thinking=high",
                "acp",
            ],
        );
        assert_agent(
            "implementer",
            &[
                "--model=github-copilot/claude-opus-4.8",
                "--thinking=medium",
                "acp",
            ],
        );
        assert_agent(
            "tester",
            &[
                "--model=github-copilot/claude-sonnet-5",
                "--thinking=medium",
                "acp",
            ],
        );
        assert_agent(
            "committer",
            &[
                "--model=github-copilot/claude-haiku-4.5",
                "--thinking=low",
                "acp",
            ],
        );
    }

    const WATCHDOG_CONTRACT_START: &str = "<!-- cowboy-agent-watchdog-contract:start -->";
    const WATCHDOG_CONTRACT_END: &str = "<!-- cowboy-agent-watchdog-contract:end -->";

    fn expected_watchdog_contract() -> String {
        let defaults = AgentWatchdogConfig::default();
        format!(
            "```toml\n[agents.watchdog]\nresponse_timeout_seconds = {}\ncancel_timeout_seconds = {}\nrecovery_operation_timeout_seconds = {}\n```\n\nParsed ACP activity resets the inactivity deadline. An in-flight ACP tool call\nrestarts the inactivity watchdog instead of triggering recovery, and that\nrestart is unbounded because deciding a tool call is stuck and aborting it is\nthe agent's responsibility rather than Cowboy's. Recovery first sends exactly\none `session/cancel` and, when the agent acknowledges it with any stop reason,\nincluding `end_turn`, sends `\"Continue\"` on the same session. If cancellation\nfails or times out, Cowboy kills the recorded PID, waits for exit, restarts the\nagent with `--resume=<session-id>`, initializes ACP, and sends `\"Continue\"`.\nThe recovery-operation timeout separately bounds termination, restart,\ninitialization, and continuation dispatch.\nA recovery whose continuation produces no session output at all is a backend that\nwill not resume, not a slow turn: after three consecutive such recoveries the\nturn fails instead of repeating forever.\nThis ACP recovery does not consume workflow retry budgets. All values must be\ngreater than zero, and Cowboy must be restarted after watchdog configuration\nchanges.",
            defaults.response_timeout_seconds,
            defaults.cancel_timeout_seconds,
            defaults.recovery_operation_timeout_seconds,
        )
    }

    fn extract_watchdog_contract(content: &str) -> std::result::Result<&str, String> {
        let starts = content
            .match_indices(WATCHDOG_CONTRACT_START)
            .collect::<Vec<_>>();
        let ends = content
            .match_indices(WATCHDOG_CONTRACT_END)
            .collect::<Vec<_>>();
        if starts.len() != 1 || ends.len() != 1 {
            return Err(format!(
                "expected one watchdog contract block, found {} starts and {} ends",
                starts.len(),
                ends.len()
            ));
        }
        let body_start = starts[0].0 + WATCHDOG_CONTRACT_START.len();
        let body_end = ends[0].0;
        if body_start > body_end {
            return Err("watchdog contract end precedes start".to_string());
        }
        Ok(content[body_start..body_end].trim())
    }

    fn validate_watchdog_document(content: &str) -> std::result::Result<(), String> {
        let contract = extract_watchdog_contract(content)?;
        if contract != expected_watchdog_contract() {
            return Err(
                "watchdog contract differs from code defaults or recovery order".to_string(),
            );
        }
        let outside = content.replacen(
            &format!("{WATCHDOG_CONTRACT_START}\n{contract}\n{WATCHDOG_CONTRACT_END}"),
            "",
            1,
        );
        for field in [
            "response_timeout_seconds",
            "cancel_timeout_seconds",
            "recovery_operation_timeout_seconds",
        ] {
            if outside.lines().any(|line| {
                line.trim_start()
                    .strip_prefix(field)
                    .is_some_and(|rest| rest.trim_start().starts_with('='))
            }) {
                return Err(format!(
                    "watchdog field assignment appears outside authoritative block: {field}"
                ));
            }
        }
        Ok(())
    }

    #[test]
    fn documented_agent_watchdog_contract_is_unique_and_exact() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../..");
        let demo = load_config(&root.join("demo-config.toml")).unwrap();
        assert_eq!(
            demo.agents
                .iter()
                .find(|agent| agent.name == "default")
                .unwrap()
                .watchdog,
            AgentWatchdogConfig::default()
        );

        for relative in ["README.md", "docs/architecture.md", "docs/module-map.md"] {
            let content = fs::read_to_string(root.join(relative)).unwrap();
            validate_watchdog_document(&content)
                .unwrap_or_else(|error| panic!("{relative}: {error}"));
        }

        let contract = expected_watchdog_contract();
        let valid = format!("{WATCHDOG_CONTRACT_START}\n{contract}\n{WATCHDOG_CONTRACT_END}");
        for invalid in [
            valid.replace(
                "response_timeout_seconds = 100",
                "response_timeout_seconds = 99",
            ),
            valid.replace(
                "Recovery first sends exactly\none `session/cancel`",
                "Recovery first restarts the agent, then sends\none `session/cancel`",
            ),
            format!("{valid}\n{valid}"),
            contract.clone(),
            format!("{valid}\nresponse_timeout_seconds = 100"),
        ] {
            assert!(validate_watchdog_document(&invalid).is_err());
        }
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
[agents.watchdog]
response_timeout_seconds = 7
cancel_timeout_seconds = 8
recovery_operation_timeout_seconds = 9

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
        assert_eq!(runtime.agents[0].watchdog.response_timeout_seconds, 7);
        assert_eq!(runtime.agents[0].watchdog.cancel_timeout_seconds, 8);
        assert_eq!(
            runtime.agents[0]
                .watchdog
                .recovery_operation_timeout_seconds,
            9
        );
        assert_eq!(runtime.agents[1].name, "reviewer");
        assert_eq!(runtime.agents[1].command, "copilot");
        assert_eq!(runtime.agents[1].model.as_ref().unwrap().id, "gpt-5.5-1m");
    }

    #[test]
    fn legacy_agent_config_omits_startup_readiness() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "[[agents]]\nname = 'default'\n").unwrap();

        let config = load_config(&path).unwrap();
        assert!(AgentConfig::default().startup_readiness.is_none());
        assert!(config.agents[0].startup_readiness.is_none());
        assert!(
            !toml::to_string(&config)
                .unwrap()
                .contains("startup_readiness")
        );
        let app_json = serde_json::to_value(&config).unwrap();
        assert!(app_json["agents"][0].get("startup_readiness").is_none());
        let decoded: AppConfig = serde_json::from_value(app_json).unwrap();
        assert_eq!(decoded, config);

        let runtime = config.runtime_config(dir.path().to_path_buf());
        assert!(runtime.agents[0].startup_readiness.is_none());
        assert!(
            serde_json::to_value(&runtime).unwrap()["agents"][0]
                .get("startup_readiness")
                .is_none()
        );
    }

    #[test]
    fn startup_readiness_parses_and_forwards_without_expanding_tilde() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        for (log_path, timeout) in [
            ("~/.local/agency/logs/session_*_{pid}/process-*.log", None),
            ("~/logs/session_{pid}_suffix/process.log", Some(1)),
            ("~/logs/{pid}/literal/*/process-*.log", Some(300)),
            ("~/logs/session_*_{pid}_suffix/process.log", Some(30)),
        ] {
            let timeout_field = timeout
                .map(|seconds| format!("timeout_seconds = {seconds}\n"))
                .unwrap_or_default();
            fs::write(
                &path,
                format!(
                    "[[agents]]\nname = 'default'\n[agents.startup_readiness]\nlog_path = '{log_path}'\ncontains = 'ACP server started, waiting for requests'\n{timeout_field}\n[[agents]]\nname = 'other'\n"
                ),
            )
            .unwrap();

            let config = load_config(&path).unwrap();
            let expected = AgentStartupReadiness {
                log_path: log_path.to_string(),
                contains: "ACP server started, waiting for requests".to_string(),
                timeout_seconds: timeout.unwrap_or(300),
            };
            assert_eq!(config.agents[0].startup_readiness, Some(expected.clone()));
            assert!(config.agents[1].startup_readiness.is_none());
            let runtime = config.runtime_config(dir.path().to_path_buf());
            assert_eq!(runtime.agents[0].startup_readiness, Some(expected));
            assert!(runtime.agents[1].startup_readiness.is_none());
            assert_eq!(
                config,
                toml::from_str::<AppConfig>(&toml::to_string(&config).unwrap()).unwrap()
            );
            assert_eq!(
                config,
                serde_json::from_str::<AppConfig>(&serde_json::to_string(&config).unwrap())
                    .unwrap()
            );
        }
    }

    #[test]
    fn rejects_invalid_agent_startup_readiness() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        for (log_path, contains, timeout) in [
            ("", "ready", 300),
            ("   ", "ready", 300),
            ("~/logs/{pid}/process.log", "", 300),
            ("~/logs/{pid}/process.log", " \t ", 300),
            ("~/logs/{pid}/process.log", "ready", 0),
            ("~/logs/{pid}/process.log", "ready", 301),
            ("~/logs/session/process.log", "ready", 300),
            ("~/logs/{pid}_{pid}/process.log", "ready", 300),
            ("~/logs/{pid}/{pid}/process.log", "ready", 300),
            ("~/logs/*/session_{pid}/process.log", "ready", 300),
            ("~/logs/prefix*/session_{pid}/process.log", "ready", 300),
            ("~/logs/session_**_{pid}/process.log", "ready", 300),
            ("~/logs/{pid}/**/process.log", "ready", 300),
            ("~/logs/{pid}/process-?.log", "ready", 300),
            ("~/logs/{pid}/process-[ab].log", "ready", 300),
        ] {
            fs::write(
                &path,
                format!(
                    "[[agents]]\nname = 'default'\n[agents.startup_readiness]\nlog_path = '{log_path}'\ncontains = '{contains}'\ntimeout_seconds = {timeout}\n"
                ),
            )
            .unwrap();

            let error = load_config(&path).unwrap_err();
            assert!(
                format!("{error:#}").contains("invalid startup_readiness"),
                "{error:#}"
            );
        }
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
    fn rejects_zero_agent_watchdog_fields() {
        let dir = tempfile::tempdir().unwrap();
        for field in [
            "response_timeout_seconds",
            "cancel_timeout_seconds",
            "recovery_operation_timeout_seconds",
        ] {
            let path = dir.path().join(format!("{field}.toml"));
            fs::write(
                &path,
                format!("[[agents]]\nname = \"default\"\n[agents.watchdog]\n{field} = 0\n"),
            )
            .unwrap();

            let err = load_config(&path).unwrap_err();
            assert!(
                format!("{err:#}").contains(&format!("{field} must be greater than zero")),
                "{err:#}"
            );
        }
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
    fn rejects_legacy_deny_tools_agent_field() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        fs::write(&path, "[[agents]]\nname = \"default\"\ndeny_tools = true\n").unwrap();

        let err = load_config(&path).unwrap_err();
        assert!(err.to_string().contains("failed to parse config"));
        assert!(format!("{err:#}").contains("unknown field `deny_tools`"));
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
