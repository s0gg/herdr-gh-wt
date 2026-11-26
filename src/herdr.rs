//! The subset of the Herdr CLI this plugin drives.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

use crate::runner::{Cmd, Runner};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct WorktreeList {
    pub source: Source,
    #[serde(default)]
    pub worktrees: Vec<Worktree>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Source {
    pub repo_root: PathBuf,
    #[serde(default)]
    pub repo_name: String,
    /// The workspace showing the repository's main checkout, if any.
    pub source_workspace_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Worktree {
    pub branch: Option<String>,
    pub path: PathBuf,
    /// The Herdr workspace this checkout is open in, if any.
    pub open_workspace_id: Option<String>,
}

/// How the dashboard pane is opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PaneOpen {
    pub plugin_id: String,
    pub entrypoint: String,
    pub placement: String,
    pub width: Option<String>,
    pub height: Option<String>,
}

pub struct Herdr {
    bin: String,
}

impl Herdr {
    pub fn new(bin: impl Into<String>) -> Self {
        Herdr { bin: bin.into() }
    }

    /// The running Herdr binary (`$HERDR_BIN_PATH`), else `herdr` on PATH.
    pub fn from_env(env: &dyn Fn(&str) -> Option<String>) -> Self {
        Herdr::new(env("HERDR_BIN_PATH").filter(|b| !b.is_empty()).unwrap_or_else(|| "herdr".to_string()))
    }

    fn cmd<I, S>(&self, args: I) -> Cmd
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Cmd::new(self.bin.clone(), args)
    }

    pub fn worktree_list(&self, runner: &dyn Runner, cwd: &Path) -> Result<WorktreeList> {
        let json = runner.stdout(&self.cmd(["worktree", "list", "--cwd", &path_str(cwd), "--json"]))?;
        let response: Envelope<WorktreeList> =
            serde_json::from_str(&json).context("unexpected `herdr worktree list` output")?;
        Ok(response.result)
    }

    /// Create a workspace without focusing it and return its id.
    pub fn workspace_create(&self, runner: &dyn Runner, cwd: &Path, label: &str) -> Result<String> {
        let json = runner.stdout(&self.cmd([
            "workspace",
            "create",
            "--cwd",
            &path_str(cwd),
            "--label",
            label,
            "--no-focus",
        ]))?;
        let response: Envelope<Created> =
            serde_json::from_str(&json).context("unexpected `herdr workspace create` output")?;
        Ok(response.result.workspace.workspace_id)
    }

    pub fn workspace_focus(&self, runner: &dyn Runner, workspace_id: &str) -> Result<()> {
        runner.stdout(&self.cmd(["workspace", "focus", workspace_id]))?;
        Ok(())
    }

    /// Open an existing checkout of `repo_root` as a focused worktree workspace.
    pub fn worktree_open(&self, runner: &dyn Runner, repo_root: &Path, path: &Path, label: &str) -> Result<()> {
        runner.stdout(&self.cmd([
            "worktree",
            "open",
            "--cwd",
            &path_str(repo_root),
            "--path",
            &path_str(path),
            "--label",
            label,
            "--focus",
            "--json",
        ]))?;
        Ok(())
    }

    pub fn plugin_pane_open(&self, runner: &dyn Runner, pane: &PaneOpen) -> Result<()> {
        let mut args: Vec<String> = [
            "plugin",
            "pane",
            "open",
            "--plugin",
            &pane.plugin_id,
            "--entrypoint",
            &pane.entrypoint,
            "--placement",
            &pane.placement,
            "--focus",
        ]
        .map(String::from)
        .to_vec();
        // Sizes only apply to popups.
        if pane.placement == "popup" {
            for (flag, value) in [("--width", &pane.width), ("--height", &pane.height)] {
                if let Some(value) = value {
                    args.push(flag.to_string());
                    args.push(value.clone());
                }
            }
        }
        runner.stdout(&self.cmd(args))?;
        Ok(())
    }
}

fn path_str(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[derive(Deserialize)]
struct Envelope<T> {
    result: T,
}

#[derive(Deserialize)]
struct Created {
    workspace: CreatedWorkspace,
}

#[derive(Deserialize)]
struct CreatedWorkspace {
    workspace_id: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::Output;
    use crate::runner::fake::FakeRunner;

    const LIST: &str = r#"{"id":"1","result":{
      "source":{"repo_key":"/r/app/.git","repo_name":"app","repo_root":"/r/app","source_checkout_path":"/r/app","source_workspace_id":"w1"},
      "worktrees":[
        {"branch":"main","is_bare":false,"is_detached":false,"is_linked_worktree":false,"is_prunable":false,"label":"app","open_workspace_id":"w1","path":"/r/app"},
        {"branch":"feat/x","is_linked_worktree":true,"path":"/r/app.feat-x"},
        {"branch":null,"is_detached":true,"path":"/r/app.detached"}
      ]}}"#;

    #[test]
    fn from_env_prefers_herdr_bin_path() {
        let herdr = Herdr::from_env(&|k| (k == "HERDR_BIN_PATH").then(|| "/opt/herdr".to_string()));
        assert_eq!(herdr.bin, "/opt/herdr");
        assert_eq!(Herdr::from_env(&|_| None).bin, "herdr");
    }

    #[test]
    fn worktree_list_parses_source_and_worktrees() {
        let runner = FakeRunner::new(|_| Ok(Output::ok(LIST)));
        let list = Herdr::new("herdr").worktree_list(&runner, Path::new("/r/app")).unwrap();
        assert_eq!(runner.lines(), ["herdr worktree list --cwd /r/app --json"]);
        assert_eq!(list.source.repo_root, PathBuf::from("/r/app"));
        assert_eq!(list.source.repo_name, "app");
        assert_eq!(list.source.source_workspace_id.as_deref(), Some("w1"));
        assert_eq!(list.worktrees.len(), 3);
        assert_eq!(list.worktrees[0].open_workspace_id.as_deref(), Some("w1"));
        assert_eq!(list.worktrees[1].branch.as_deref(), Some("feat/x"));
        assert_eq!(list.worktrees[1].open_workspace_id, None);
        assert_eq!(list.worktrees[2].branch, None);
    }

    #[test]
    fn worktree_list_without_a_workspace() {
        let json = r#"{"result":{"source":{"repo_root":"/r/app","repo_name":"app"},"worktrees":[]}}"#;
        let runner = FakeRunner::new(move |_| Ok(Output::ok(json)));
        let list = Herdr::new("herdr").worktree_list(&runner, Path::new("/r/app")).unwrap();
        assert_eq!(list.source.source_workspace_id, None);
    }

    #[test]
    fn worktree_list_reports_failures() {
        let runner = FakeRunner::new(|_| Ok(Output::failed("not a git repository")));
        let err = Herdr::new("herdr").worktree_list(&runner, Path::new("/x")).unwrap_err();
        assert!(format!("{err:#}").contains("not a git repository"), "{err:#}");
    }

    #[test]
    fn workspace_create_returns_the_new_id() {
        let runner = FakeRunner::new(|_| {
            Ok(Output::ok(r#"{"result":{"workspace":{"workspace_id":"w9"},"tab":{"tab_id":"w9:t1"}}}"#))
        });
        let id = Herdr::new("herdr").workspace_create(&runner, Path::new("/r/app"), "app").unwrap();
        assert_eq!(id, "w9");
        assert_eq!(runner.lines(), ["herdr workspace create --cwd /r/app --label app --no-focus"]);
    }

    #[test]
    fn workspace_focus_and_worktree_open_commands() {
        let runner = FakeRunner::new(|_| Ok(Output::ok("{}")));
        let herdr = Herdr::new("/bin/herdr");
        herdr.workspace_focus(&runner, "w3").unwrap();
        herdr
            .worktree_open(&runner, Path::new("/r/app"), Path::new("/r/app.fix"), "Issue #1 Fix")
            .unwrap();
        let calls = runner.calls();
        assert_eq!(calls[0].line(), "/bin/herdr workspace focus w3");
        assert_eq!(
            calls[1].args,
            ["worktree", "open", "--cwd", "/r/app", "--path", "/r/app.fix", "--label", "Issue #1 Fix", "--focus", "--json"]
        );
    }

    #[test]
    fn plugin_pane_open_passes_placement_and_size() {
        let runner = FakeRunner::new(|_| Ok(Output::ok("{}")));
        let herdr = Herdr::new("herdr");
        let mut pane = PaneOpen {
            plugin_id: "gh-wt".into(),
            entrypoint: "dashboard".into(),
            placement: "popup".into(),
            width: Some("90%".into()),
            height: None,
        };
        herdr.plugin_pane_open(&runner, &pane).unwrap();
        pane.placement = "zoomed".into();
        herdr.plugin_pane_open(&runner, &pane).unwrap();
        assert_eq!(
            runner.lines(),
            [
                "herdr plugin pane open --plugin gh-wt --entrypoint dashboard --placement popup --focus --width 90%",
                "herdr plugin pane open --plugin gh-wt --entrypoint dashboard --placement zoomed --focus",
            ]
        );
    }
}
