//! Plugin configuration: `config.toml` in the plugin's config directory,
//! overridden by `HERDR_GH_WT_*` environment variables.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

pub const ENV_CONFIG_PATH: &str = "HERDR_GH_WT_CONFIG";
pub const ENV_PI_MODEL: &str = "HERDR_GH_WT_PI_MODEL";
pub const ENV_PI_PROVIDER: &str = "HERDR_GH_WT_PI_PROVIDER";
pub const ENV_PI_THINKING: &str = "HERDR_GH_WT_PI_THINKING";
pub const ENV_PI_TIMEOUT: &str = "HERDR_GH_WT_PI_TIMEOUT";

/// Prompt for the Herdr workspace label. Placeholders: `{kind}`, `{ref}`,
/// `{repo}`, `{number}`, `{title}`, `{labels}`, `{body}`.
pub const DEFAULT_LABEL_PROMPT: &str = "\
You name terminal workspaces. Write a short label for a workspace in which I will work on the GitHub {kind} below.
Rules:
- Start with \"{ref}\" exactly.
- Follow it with a concise summary of the work, at most 30 characters, written in the same language as the title.
- Output the label only, on a single line, without quotes.

Repository: {repo}
Title: {title}
Labels: {labels}
Description:
{body}
";

/// Prompt for the branch name of a new issue worktree. Same placeholders as
/// [`DEFAULT_LABEL_PROMPT`].
pub const DEFAULT_BRANCH_PROMPT: &str = "\
Suggest a git branch name for working on the GitHub {kind} below.
Rules:
- 2 to 5 lowercase English words separated by hyphens (kebab-case).
- ASCII letters, digits and hyphens only. No slashes, no type prefix, no issue number.
- Output the branch name only, on a single line.

Repository: {repo}
Title: {title}
Labels: {labels}
Description:
{body}
";

const PLACEMENTS: [&str; 5] = ["popup", "overlay", "zoomed", "tab", "split"];

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawConfig {
    pi: RawPi,
    dashboard: RawDashboard,
    open: RawOpen,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawOpen {
    focus: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawPi {
    model: Option<String>,
    provider: Option<String>,
    thinking: Option<String>,
    timeout_secs: Option<u64>,
    extra_args: Option<Vec<String>>,
    label_prompt: Option<String>,
    branch_prompt: Option<String>,
    label_max_chars: Option<usize>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct RawDashboard {
    placement: Option<String>,
    width: Option<String>,
    height: Option<String>,
    limit: Option<u32>,
}

/// `Some("")` in the file explicitly clears a value.
fn non_empty(value: Option<String>, default: Option<String>) -> Option<String> {
    match value {
        Some(v) if v.trim().is_empty() => None,
        Some(v) => Some(v),
        None => default,
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub pi: PiConfig,
    pub dashboard: DashboardConfig,
    pub open: OpenConfig,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PiConfig {
    /// Passed to `pi --model`; `None` uses pi's own default model.
    pub model: Option<String>,
    /// Passed to `pi --provider`.
    pub provider: Option<String>,
    /// Passed to `pi --thinking`.
    pub thinking: Option<String>,
    /// Give up on pi after this many seconds and fall back to a generated name.
    pub timeout_secs: u64,
    /// Extra arguments appended to every pi invocation.
    pub extra_args: Vec<String>,
    pub label_prompt: String,
    pub branch_prompt: String,
    pub label_max_chars: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct OpenConfig {
    /// Move the focus to the opened workspace and close the dashboard. When
    /// false, open in the background and stay in the dashboard.
    pub focus: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DashboardConfig {
    pub placement: String,
    pub width: Option<String>,
    pub height: Option<String>,
    /// Maximum number of pull requests / issues fetched per list.
    pub limit: u32,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            pi: PiConfig {
                model: None,
                provider: None,
                thinking: Some("off".to_string()),
                timeout_secs: 30,
                extra_args: Vec::new(),
                label_prompt: DEFAULT_LABEL_PROMPT.to_string(),
                branch_prompt: DEFAULT_BRANCH_PROMPT.to_string(),
                label_max_chars: 40,
            },
            dashboard: DashboardConfig {
                placement: "popup".to_string(),
                width: Some("90%".to_string()),
                height: Some("90%".to_string()),
                limit: 100,
            },
            open: OpenConfig { focus: true },
        }
    }
}

impl Config {
    pub fn from_toml_str(text: &str) -> Result<Self> {
        let raw: RawConfig = toml::from_str(text)?;
        let d = Config::default();
        let placement = raw.dashboard.placement.unwrap_or(d.dashboard.placement);
        if !PLACEMENTS.contains(&placement.as_str()) {
            bail!(
                "unsupported dashboard.placement {placement:?} (expected one of: {})",
                PLACEMENTS.join(", ")
            );
        }
        Ok(Config {
            pi: PiConfig {
                model: non_empty(raw.pi.model, d.pi.model),
                provider: non_empty(raw.pi.provider, d.pi.provider),
                thinking: non_empty(raw.pi.thinking, d.pi.thinking),
                timeout_secs: raw.pi.timeout_secs.unwrap_or(d.pi.timeout_secs),
                extra_args: raw.pi.extra_args.unwrap_or(d.pi.extra_args),
                label_prompt: raw.pi.label_prompt.unwrap_or(d.pi.label_prompt),
                branch_prompt: raw.pi.branch_prompt.unwrap_or(d.pi.branch_prompt),
                label_max_chars: raw.pi.label_max_chars.unwrap_or(d.pi.label_max_chars),
            },
            dashboard: DashboardConfig {
                placement,
                width: non_empty(raw.dashboard.width, d.dashboard.width),
                height: non_empty(raw.dashboard.height, d.dashboard.height),
                limit: raw.dashboard.limit.unwrap_or(d.dashboard.limit),
            },
            open: OpenConfig {
                focus: raw.open.focus.unwrap_or(d.open.focus),
            },
        })
    }

    /// Apply `HERDR_GH_WT_*` overrides read through `env`.
    pub fn apply_env(&mut self, env: &dyn Fn(&str) -> Option<String>) -> Result<()> {
        let get = |key: &str| env(key).filter(|v| !v.trim().is_empty());
        if let Some(model) = get(ENV_PI_MODEL) {
            self.pi.model = Some(model);
        }
        if let Some(provider) = get(ENV_PI_PROVIDER) {
            self.pi.provider = Some(provider);
        }
        if let Some(thinking) = get(ENV_PI_THINKING) {
            self.pi.thinking = Some(thinking);
        }
        if let Some(timeout) = get(ENV_PI_TIMEOUT) {
            self.pi.timeout_secs = timeout.trim().parse().with_context(|| {
                format!("{ENV_PI_TIMEOUT} must be a number of seconds, got {timeout:?}")
            })?;
        }
        Ok(())
    }

    /// Where the config file lives: `$HERDR_GH_WT_CONFIG`, else
    /// `$HERDR_PLUGIN_CONFIG_DIR/config.toml`.
    pub fn path(env: &dyn Fn(&str) -> Option<String>) -> Option<PathBuf> {
        let get = |key: &str| env(key).filter(|v| !v.is_empty());
        get(ENV_CONFIG_PATH).map(PathBuf::from).or_else(|| {
            get("HERDR_PLUGIN_CONFIG_DIR").map(|dir| Path::new(&dir).join("config.toml"))
        })
    }

    /// Read the config file (missing file = defaults) and apply env overrides.
    pub fn load(env: &dyn Fn(&str) -> Option<String>) -> Result<Self> {
        let mut config = match Self::path(env) {
            Some(path) if path.exists() => Self::read(&path)?,
            _ => Config::default(),
        };
        config.apply_env(env)?;
        Ok(config)
    }

    fn read(path: &Path) -> Result<Self> {
        let text =
            std::fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
        Self::from_toml_str(&text).with_context(|| format!("invalid config in {}", path.display()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env_from(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let map: HashMap<String, String> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |key| map.get(key).cloned()
    }

    #[test]
    fn defaults_leave_model_to_pi_and_open_a_large_popup() {
        let config = Config::default();
        assert_eq!(config.pi.model, None);
        assert_eq!(config.pi.provider, None);
        assert_eq!(config.pi.thinking.as_deref(), Some("off"));
        assert_eq!(config.pi.timeout_secs, 30);
        assert!(config.pi.label_prompt.contains("{title}"));
        assert!(config.pi.branch_prompt.contains("{title}"));
        assert_eq!(config.dashboard.placement, "popup");
        assert_eq!(config.dashboard.width.as_deref(), Some("90%"));
        assert_eq!(config.dashboard.height.as_deref(), Some("90%"));
        assert_eq!(config.dashboard.limit, 100);
    }

    #[test]
    fn opening_moves_the_focus_by_default() {
        assert!(Config::default().open.focus);
    }

    #[test]
    fn focus_can_be_turned_off() {
        let config = Config::from_toml_str("[open]\nfocus = false\n").unwrap();
        assert!(!config.open.focus);
        assert!(
            Config::from_toml_str("[open]\nfocuss = false\n").is_err(),
            "typos are reported"
        );
    }

    #[test]
    fn empty_toml_is_the_default() {
        assert_eq!(Config::from_toml_str("").unwrap(), Config::default());
    }

    #[test]
    fn toml_overrides_individual_keys() {
        let config = Config::from_toml_str(
            r#"
            [pi]
            model = "anthropic/claude-haiku-4-5"
            provider = "anthropic"
            thinking = "low"
            timeout_secs = 10
            extra_args = ["--offline"]
            label_prompt = "label {title}"
            label_max_chars = 24

            [dashboard]
            placement = "zoomed"
            width = "120"
            limit = 30
            "#,
        )
        .unwrap();
        assert_eq!(
            config.pi.model.as_deref(),
            Some("anthropic/claude-haiku-4-5")
        );
        assert_eq!(config.pi.provider.as_deref(), Some("anthropic"));
        assert_eq!(config.pi.thinking.as_deref(), Some("low"));
        assert_eq!(config.pi.timeout_secs, 10);
        assert_eq!(config.pi.extra_args, vec!["--offline".to_string()]);
        assert_eq!(config.pi.label_prompt, "label {title}");
        assert_eq!(config.pi.branch_prompt, Config::default().pi.branch_prompt);
        assert_eq!(config.pi.label_max_chars, 24);
        assert_eq!(config.dashboard.placement, "zoomed");
        assert_eq!(config.dashboard.width.as_deref(), Some("120"));
        assert_eq!(config.dashboard.height.as_deref(), Some("90%"));
        assert_eq!(config.dashboard.limit, 30);
    }

    #[test]
    fn empty_strings_mean_unset() {
        let config = Config::from_toml_str("[pi]\nmodel = \"\"\nthinking = \"\"\n").unwrap();
        assert_eq!(config.pi.model, None);
        assert_eq!(config.pi.thinking, None);
    }

    #[test]
    fn unknown_keys_are_rejected() {
        let err = Config::from_toml_str("[pi]\nmodle = \"x\"\n").unwrap_err();
        assert!(format!("{err:#}").contains("modle"), "{err:#}");
    }

    #[test]
    fn invalid_placement_is_rejected() {
        let err = Config::from_toml_str("[dashboard]\nplacement = \"floating\"\n").unwrap_err();
        assert!(format!("{err:#}").contains("floating"), "{err:#}");
    }

    #[test]
    fn env_overrides_win_over_the_file() {
        let mut config = Config::from_toml_str("[pi]\nmodel = \"from-file\"\n").unwrap();
        config
            .apply_env(&env_from(&[
                (ENV_PI_MODEL, "openai/gpt-5-mini"),
                (ENV_PI_PROVIDER, "openai"),
                (ENV_PI_THINKING, "minimal"),
                (ENV_PI_TIMEOUT, "5"),
            ]))
            .unwrap();
        assert_eq!(config.pi.model.as_deref(), Some("openai/gpt-5-mini"));
        assert_eq!(config.pi.provider.as_deref(), Some("openai"));
        assert_eq!(config.pi.thinking.as_deref(), Some("minimal"));
        assert_eq!(config.pi.timeout_secs, 5);
    }

    #[test]
    fn empty_env_values_are_ignored() {
        let mut config = Config::from_toml_str("[pi]\nmodel = \"from-file\"\n").unwrap();
        config.apply_env(&env_from(&[(ENV_PI_MODEL, "")])).unwrap();
        assert_eq!(config.pi.model.as_deref(), Some("from-file"));
    }

    #[test]
    fn invalid_env_timeout_is_an_error() {
        let mut config = Config::default();
        let err = config
            .apply_env(&env_from(&[(ENV_PI_TIMEOUT, "soon")]))
            .unwrap_err();
        assert!(format!("{err:#}").contains(ENV_PI_TIMEOUT), "{err:#}");
    }

    #[test]
    fn path_prefers_explicit_env_then_plugin_config_dir() {
        assert_eq!(
            Config::path(&env_from(&[
                (ENV_CONFIG_PATH, "/x/c.toml"),
                ("HERDR_PLUGIN_CONFIG_DIR", "/cfg")
            ])),
            Some(PathBuf::from("/x/c.toml"))
        );
        assert_eq!(
            Config::path(&env_from(&[("HERDR_PLUGIN_CONFIG_DIR", "/cfg")])),
            Some(PathBuf::from("/cfg/config.toml"))
        );
        assert_eq!(Config::path(&env_from(&[])), None);
    }

    #[test]
    fn load_reads_the_file_and_applies_env() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("config.toml"),
            "[pi]\nmodel = \"file\"\nprovider = \"p\"\n",
        )
        .unwrap();
        let dir_str = dir.path().to_str().unwrap();
        let config = Config::load(&env_from(&[
            ("HERDR_PLUGIN_CONFIG_DIR", dir_str),
            (ENV_PI_MODEL, "env"),
        ]))
        .unwrap();
        assert_eq!(config.pi.model.as_deref(), Some("env"));
        assert_eq!(config.pi.provider.as_deref(), Some("p"));
    }

    #[test]
    fn load_without_a_file_uses_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::load(&env_from(&[(
            "HERDR_PLUGIN_CONFIG_DIR",
            dir.path().to_str().unwrap(),
        )]))
        .unwrap();
        assert_eq!(config, Config::default());
    }

    #[test]
    fn load_reports_the_file_on_parse_errors() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("config.toml"), "[pi\n").unwrap();
        let err = Config::load(&env_from(&[(
            "HERDR_PLUGIN_CONFIG_DIR",
            dir.path().to_str().unwrap(),
        )]))
        .unwrap_err();
        assert!(format!("{err:#}").contains("config.toml"), "{err:#}");
    }
}
