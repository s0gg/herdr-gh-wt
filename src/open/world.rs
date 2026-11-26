//! A simulated machine for the opener's tests: a ghq root, a Herdr session,
//! a git repository and worktrunk, answering the commands the opener runs.

use std::sync::{Arc, Mutex};

use super::{ISSUE_CONFIG_KEY, IssueBranch, Opener, Outcome};
use crate::config::Config;
use crate::github::{Item, Kind};
use crate::herdr::Herdr;
use crate::runner::fake::FakeRunner;
use crate::runner::{Cmd, Io, Output};

pub const ROOT: &str = "/src/github.com/acme/app";

/// A simulated machine: a ghq root, a Herdr session, a git repository and
/// worktrunk, answering the commands the opener runs.
#[derive(Clone)]
#[allow(clippy::struct_excessive_bools)] // independent switches of a test fixture
pub struct World {
    pub cloned: bool,
    pub workspace: bool,
    /// (branch, path, open workspace id)
    pub worktrees: Vec<(String, String, Option<String>)>,
    /// local branches
    pub branches: Vec<String>,
    /// (branch, issue number) from git config
    pub issue_map: Vec<(String, u64)>,
    pub pi_label: Result<String, String>,
    pub pi_branch: Result<String, String>,
    pub wt_fails: bool,
    /// `gh config get git_protocol`
    pub git_protocol: String,
    /// `git for-each-ref` output, newest first
    pub refs: Vec<String>,
    /// `git symbolic-ref refs/remotes/origin/HEAD`
    pub origin_head: Option<String>,
    pub fetch_fails: bool,
    /// refs that `git fetch` brings in
    pub fetched_refs: Vec<String>,
}

impl Default for World {
    fn default() -> Self {
        World {
            cloned: true,
            workspace: true,
            worktrees: vec![("main".into(), ROOT.into(), Some("w1".into()))],
            branches: vec!["main".into()],
            issue_map: vec![],
            pi_label: Ok("PR #7 Add column".into()),
            pi_branch: Ok("fix-login-timeout".into()),
            wt_fails: false,
            git_protocol: "https".into(),
            refs: [
                "refs/heads/feature/login",
                "refs/remotes/origin/epic/x",
                "refs/heads/main",
                "refs/remotes/origin/HEAD",
                "refs/remotes/origin/main",
            ]
            .map(String::from)
            .to_vec(),
            origin_head: Some("origin/main".into()),
            fetch_fails: false,
            fetched_refs: vec![],
        }
    }
}

pub fn worktree_path(branch: &str) -> String {
    if branch == "main" {
        ROOT.into()
    } else {
        format!("{ROOT}.{}", branch.replace('/', "-"))
    }
}

pub fn simulate(world: &Mutex<World>, cmd: &Cmd) -> Output {
    let mut w = world.lock().unwrap();
    let args: Vec<&str> = cmd.args.iter().map(String::as_str).collect();
    match (cmd.program.as_str(), args.as_slice()) {
        ("ghq", ["list", "--full-path", "--exact", "github.com/acme/app"]) => {
            Output::ok(if w.cloned {
                format!("{ROOT}\n")
            } else {
                String::new()
            })
        }
        ("ghq", ["get", rest @ ..]) => {
            assert_eq!(cmd.io, Io::Inherit, "ghq get shows its progress");
            assert_eq!(rest.last(), Some(&"github.com/acme/app"));
            w.cloned = true;
            Output::ok("")
        }
        ("gh", ["config", "get", "git_protocol", "--host", "github.com"]) => {
            Output::ok(format!("{}\n", w.git_protocol))
        }
        ("herdr", ["worktree", "list", "--cwd", cwd, "--json"]) => {
            assert_eq!(*cwd, ROOT);
            let worktrees: Vec<_> = w
                .worktrees
                .iter()
                .map(|(b, p, ws)| serde_json::json!({"branch": b, "path": p, "open_workspace_id": ws}))
                .collect();
            let source_ws = if w.workspace {
                serde_json::json!("w1")
            } else {
                serde_json::Value::Null
            };
            Output::ok(
                serde_json::json!({"result": {
                    "source": {"repo_root": ROOT, "repo_name": "app", "source_workspace_id": source_ws},
                    "worktrees": worktrees
                }})
                .to_string(),
            )
        }
        (
            "herdr",
            [
                "workspace",
                "create",
                "--cwd",
                ROOT,
                "--label",
                "app",
                "--no-focus",
            ],
        ) => {
            w.workspace = true;
            Output::ok(r#"{"result":{"workspace":{"workspace_id":"w1"}}}"#)
        }
        ("herdr", ["workspace", "focus", _] | ["worktree", "open", ..]) => Output::ok("{}"),
        ("git", ["config", "--get-regexp", _]) => {
            if w.issue_map.is_empty() {
                Output {
                    success: false,
                    ..Output::default()
                }
            } else {
                Output::ok(
                    w.issue_map
                        .iter()
                        .map(|(b, n)| format!("branch.{b}.{ISSUE_CONFIG_KEY} {n}"))
                        .collect::<Vec<_>>()
                        .join("\n"),
                )
            }
        }
        ("git", ["config", "--unset-all", key]) => {
            let branch = issue_key_branch(key);
            let before = w.issue_map.len();
            w.issue_map.retain(|(b, _)| b != branch);
            Output {
                // git exits with 5 when there is nothing to unset.
                success: w.issue_map.len() < before,
                ..Output::default()
            }
        }
        ("git", ["config", key, value]) => {
            let branch = issue_key_branch(key).to_string();
            let number = value.parse().unwrap();
            match w.issue_map.iter_mut().find(|(b, _)| *b == branch) {
                Some(entry) => entry.1 = number,
                None => w.issue_map.push((branch, number)),
            }
            Output::ok("")
        }
        ("git", ["fetch", "--all", "--quiet"]) => {
            assert_eq!(cmd.io, Io::Inherit, "fetch can prompt for credentials");
            if w.fetch_fails {
                Output::failed("could not read from remote")
            } else {
                let fetched = std::mem::take(&mut w.fetched_refs);
                w.refs.extend(fetched);
                Output::ok("")
            }
        }
        (
            "git",
            [
                "for-each-ref",
                "--sort=-committerdate",
                "--format=%(refname)",
                "refs/heads",
                "refs/remotes",
            ],
        ) => Output::ok(w.refs.join("\n")),
        (
            "git",
            [
                "symbolic-ref",
                "--quiet",
                "--short",
                "refs/remotes/origin/HEAD",
            ],
        ) => match &w.origin_head {
            Some(head) => Output::ok(format!("{head}\n")),
            None => Output {
                success: false,
                ..Output::default()
            },
        },
        ("git", ["show-ref", "--verify", "--quiet", r]) => {
            let branch = r.strip_prefix("refs/heads/").unwrap();
            Output {
                success: w.branches.iter().any(|b| b == branch),
                ..Output::default()
            }
        }
        ("wt", ["switch", rest @ ..]) => {
            assert_eq!(
                cmd.io,
                Io::CaptureStdout,
                "wt hooks and prompts reach the terminal"
            );
            assert_eq!(&rest[rest.len() - 2..], ["--no-cd", "--format=json"]);
            if w.wt_fails {
                return Output::failed("hook failed");
            }
            let (branch, action) = match rest {
                ["--create", b, ..] => (b.to_string(), "created"),
                [r, ..] if r.starts_with("pr:") => ("feat/x".to_string(), "created"),
                [b, ..] => (b.to_string(), "existing"),
                [] => unreachable!(),
            };
            let path = worktree_path(&branch);
            if !w.worktrees.iter().any(|(b, _, _)| *b == branch) {
                w.worktrees.push((branch.clone(), path.clone(), None));
            }
            if !w.branches.contains(&branch) {
                w.branches.push(branch.clone());
            }
            Output::ok(
                serde_json::json!({"action": action, "branch": branch, "path": path}).to_string(),
            )
        }
        ("pi", _) => {
            let prompt = cmd.args.last().unwrap();
            let answer = if prompt.contains("branch name") {
                &w.pi_branch
            } else {
                &w.pi_label
            };
            match answer {
                Ok(text) => Output::ok(format!("{text}\n")),
                Err(e) => Output::failed(e.clone()),
            }
        }
        _ => panic!("unexpected command: {}", cmd.line()),
    }
}

/// `branch.<name>.herdr-gh-wt-issue` -> `<name>`
fn issue_key_branch(key: &str) -> &str {
    key.strip_prefix("branch.")
        .and_then(|k| k.strip_suffix(&format!(".{ISSUE_CONFIG_KEY}")))
        .expect("unexpected git config key")
}

pub fn pr() -> Item {
    Item {
        kind: Kind::PullRequest,
        host: "github.com".into(),
        repo: "acme/app".into(),
        number: 7,
        title: "Add a column".into(),
        url: "https://github.com/acme/app/pull/7".into(),
        author: Some("alice".into()),
        updated_at: String::new(),
        labels: vec![],
        body: String::new(),
        is_draft: false,
        head_ref: Some("feat/x".into()),
        reviewers: vec![],
        linked_branches: vec![],
        closing_pull_requests: vec![],
    }
}

pub fn issue() -> Item {
    Item {
        kind: Kind::Issue,
        number: 42,
        title: "Login times out".into(),
        url: "https://github.com/acme/app/issues/42".into(),
        head_ref: None,
        ..pr()
    }
}

pub struct Run {
    pub outcome: anyhow::Result<Outcome>,
    pub lines: Vec<String>,
    pub log: Vec<String>,
    pub world: World,
}

pub fn run(world: World, item: &Item) -> Run {
    run_with(world, item, IssueBranch::Default)
}

pub fn run_with(world: World, item: &Item, how: IssueBranch) -> Run {
    run_full(world, item, how, true)
}

pub fn run_full(world: World, item: &Item, how: IssueBranch, focus: bool) -> Run {
    let world = Arc::new(Mutex::new(world));
    let shared = world.clone();
    let runner = Arc::new(FakeRunner::new(move |cmd| Ok(simulate(&shared, cmd))));
    let opener = Opener {
        runner: runner.clone(),
        herdr: Herdr::new("herdr"),
        pi: Config::default().pi,
        focus,
    };
    let mut log = Vec::new();
    let outcome = opener.open_with(item, how, &mut |line| log.push(line.to_string()));
    let world = world.lock().unwrap().clone();
    Run {
        outcome,
        lines: runner.lines(),
        log,
        world,
    }
}

pub fn ran(run: &Run, prefix: &str) -> bool {
    run.lines.iter().any(|l| l.starts_with(prefix))
}
