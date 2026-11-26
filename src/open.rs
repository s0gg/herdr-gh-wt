//! Opening a pull request or issue as a worktrunk worktree in a Herdr
//! worktree workspace.
//!
//! 1. Find the local clone with `ghq` (cloning it with `ghq get` if needed,
//!    over SSH when that is gh's git protocol).
//! 2. Make sure Herdr has a workspace for the repository.
//! 3. If the item's branch is already open in Herdr, focus that workspace.
//! 4. Otherwise let worktrunk switch to (or create) the worktree — pull
//!    requests via `wt switch pr:N`, issues via a branch named by pi and,
//!    optionally, started from a base branch the user picks — and open it
//!    with `herdr worktree open`, labelled by pi.

use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::base_picker::BaseCandidate;
use crate::config::PiConfig;
use crate::github::{Item, Kind};
use crate::herdr::{Herdr, WorktreeList};
use crate::naming::unique_branch;
use crate::pi;
use crate::runner::{Cmd, Io, Runner};

/// Git config variable (`branch.<name>.herdr-gh-wt-issue`) that records which
/// issue a branch was created for. Git drops it together with the branch.
pub const ISSUE_CONFIG_KEY: &str = "herdr-gh-wt-issue";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The item was already open in `workspace_id`, which got the focus
    /// when [`Opener::focus`] is set.
    AlreadyOpen { workspace_id: String },
    /// The worktree was opened as a new workspace.
    Opened { path: PathBuf, label: String },
    /// The user backed out (for example of choosing a base branch).
    Cancelled,
}

/// Asks the user for the base of a new issue branch among the candidates;
/// `None` cancels.
pub type ChooseBase<'a> = &'a mut dyn FnMut(&[BaseCandidate]) -> Result<Option<String>>;

pub struct Opener {
    pub runner: Arc<dyn Runner + Send + Sync>,
    pub herdr: Herdr,
    pub pi: PiConfig,
    /// Move the focus to the item's workspace.
    pub focus: bool,
}

impl Opener {
    /// Open `item`, reporting progress through `log`.
    pub fn open(&self, item: &Item, log: &mut dyn FnMut(&str)) -> Result<Outcome> {
        self.open_with(item, None, log)
    }

    /// Like [`Opener::open`]; when a new issue branch is created,
    /// `choose_base` picks the branch it starts from.
    pub fn open_with(
        &self,
        item: &Item,
        choose_base: Option<ChooseBase>,
        log: &mut dyn FnMut(&str),
    ) -> Result<Outcome> {
        let runner = &*self.runner;
        let repo_path = self.locate_repository(item, log)?;
        let list = self.herdr.worktree_list(runner, &repo_path)?;
        let root = list.source.repo_root.clone();
        let root_workspace = if let Some(id) = list.source.source_workspace_id.clone() {
            id
        } else {
            let label = repository_label(&list);
            log(&format!("Creating Herdr workspace \"{label}\""));
            self.herdr.workspace_create(runner, &root, &label)?
        };

        // The branch the item lives on, when it is known up front.
        let known_branch = match item.kind {
            Kind::PullRequest => item.head_ref.clone(),
            Kind::Issue => self.recorded_issue_branch(&root, item.number)?,
        };
        if let (Kind::Issue, Some(branch), Some(_)) = (item.kind, &known_branch, &choose_base) {
            log(&format!(
                "Issue #{} already has branch {branch}; a base branch only applies to new branches",
                item.number
            ));
        }
        if let Some(id) = known_branch
            .as_deref()
            .and_then(|b| open_workspace_of_branch(&list, b))
        {
            return self.already_open(&id, log);
        }

        let creates_issue_branch = item.kind == Kind::Issue && known_branch.is_none();
        let target = match (item.kind, known_branch) {
            (Kind::PullRequest, _) => vec![format!("pr:{}", item.number)],
            (Kind::Issue, Some(branch)) => vec![branch],
            (Kind::Issue, None) => {
                let base = match choose_base {
                    Some(choose) => match self.ask_base(&root, choose, log)? {
                        Some(base) => Some(base),
                        None => return Ok(Outcome::Cancelled),
                    },
                    None => None,
                };
                let mut target = vec![
                    "--create".to_string(),
                    self.new_issue_branch(&root, item, log),
                ];
                if let Some(base) = base {
                    target.extend(["--base".to_string(), base]);
                }
                target
            }
        };

        // Name the workspace while worktrunk works; hooks can take a while.
        let label = self.spawn_label(item);
        log("Switching to the worktree with worktrunk...");
        let switched = self.switch(&root, &target)?;

        if creates_issue_branch {
            self.record_issue_branch(&root, &switched.branch, item.number, log);
        }

        let list = self.herdr.worktree_list(runner, &root)?;
        if let Some(id) = list
            .worktrees
            .iter()
            .find(|w| same_path(&w.path, &switched.path))
            .and_then(|w| w.open_workspace_id.clone())
        {
            return self.already_open(&id, log);
        }
        if same_path(&switched.path, &root) {
            let id = list.source.source_workspace_id.unwrap_or(root_workspace);
            return self.already_open(&id, log);
        }

        let label = match label.recv() {
            Ok(Ok(label)) => label,
            Ok(Err(err)) => {
                log(&format!(
                    "pi could not name the workspace ({err:#}); using the title instead"
                ));
                pi::fallback_label(item, self.pi.label_max_chars)
            }
            Err(_) => pi::fallback_label(item, self.pi.label_max_chars),
        };
        log(&format!(
            "Opening {} as \"{label}\"",
            switched.path.display()
        ));
        self.herdr
            .worktree_open(runner, &root, &switched.path, &label, self.focus)?;
        Ok(Outcome::Opened {
            path: switched.path,
            label,
        })
    }

    fn already_open(&self, workspace_id: &str, log: &mut dyn FnMut(&str)) -> Result<Outcome> {
        if self.focus {
            log("Already open in Herdr; focusing it");
            self.herdr.workspace_focus(&*self.runner, workspace_id)?;
        } else {
            log("Already open in Herdr");
        }
        Ok(Outcome::AlreadyOpen {
            workspace_id: workspace_id.to_string(),
        })
    }

    /// The local clone of the item's repository, cloned with `ghq get` first
    /// when there is none.
    fn locate_repository(&self, item: &Item, log: &mut dyn FnMut(&str)) -> Result<PathBuf> {
        let spec = format!("{}/{}", item.host, item.repo);
        if let Some(path) = self.ghq_find(&spec)? {
            return Ok(path);
        }
        log(&format!("Cloning {spec} with ghq..."));
        // Clone the way gh is set up to talk to git: `-p` makes ghq use SSH.
        let mut args = vec!["get".to_string()];
        if self.gh_git_protocol(&item.host).as_deref() == Some("ssh") {
            args.push("-p".to_string());
        }
        args.push(spec.clone());
        let out = self.runner.run(&Cmd::new("ghq", args).io(Io::Inherit))?;
        if !out.success {
            bail!("`ghq get {spec}` failed");
        }
        self.ghq_find(&spec)?
            .with_context(|| format!("{spec} is not under the ghq root even after `ghq get`"))
    }

    fn gh_git_protocol(&self, host: &str) -> Option<String> {
        let cmd = Cmd::new("gh", ["config", "get", "git_protocol", "--host", host]);
        self.runner.stdout(&cmd).ok().map(|p| p.trim().to_string())
    }

    fn ghq_find(&self, spec: &str) -> Result<Option<PathBuf>> {
        let out = self
            .runner
            .stdout(&Cmd::new("ghq", ["list", "--full-path", "--exact", spec]))?;
        Ok(out
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty())
            .map(PathBuf::from))
    }

    fn branch_exists(&self, root: &Path, branch: &str) -> bool {
        let reference = format!("refs/heads/{branch}");
        self.runner
            .run(&git(root, &["show-ref", "--verify", "--quiet", &reference]))
            .is_ok_and(|out| out.success)
    }

    /// The existing branch previously created for issue `number`, if any.
    fn recorded_issue_branch(&self, root: &Path, number: u64) -> Result<Option<String>> {
        let pattern = format!("^branch\\..*\\.{ISSUE_CONFIG_KEY}$");
        let out = self
            .runner
            .run(&git(root, &["config", "--get-regexp", &pattern]))?;
        if !out.success {
            // git exits with 1 when nothing matches.
            return Ok(None);
        }
        let suffix = format!(".{ISSUE_CONFIG_KEY}");
        let number = number.to_string();
        Ok(out
            .stdout
            .lines()
            .filter_map(|line| line.split_once(' '))
            .filter(|(_, value)| value.trim() == number)
            .filter_map(|(key, _)| key.strip_prefix("branch.")?.strip_suffix(&suffix))
            .find(|branch| self.branch_exists(root, branch))
            .map(str::to_string))
    }

    fn record_issue_branch(
        &self,
        root: &Path,
        branch: &str,
        number: u64,
        log: &mut dyn FnMut(&str),
    ) {
        let key = format!("branch.{branch}.{ISSUE_CONFIG_KEY}");
        let recorded = self
            .runner
            .stdout(&git(root, &["config", &key, &number.to_string()]));
        if let Err(err) = recorded {
            log(&format!(
                "Could not record the issue on branch {branch}: {err:#}"
            ));
        }
    }

    /// Refresh the remote-tracking branches and let `choose` pick a base.
    fn ask_base(
        &self,
        root: &Path,
        choose: ChooseBase,
        log: &mut dyn FnMut(&str),
    ) -> Result<Option<String>> {
        log("Fetching remote branches...");
        let fetched = self
            .runner
            .run(&git(root, &["fetch", "--all", "--quiet"]).io(Io::Inherit));
        match fetched {
            Ok(out) if out.success => {}
            Ok(_) => log("git fetch failed; offering the branches known locally"),
            Err(err) => log(&format!(
                "git fetch failed ({err:#}); offering the branches known locally"
            )),
        }
        let candidates = self.base_candidates(root)?;
        choose(&candidates)
    }

    /// Local branches, then remote-tracking branches without a local
    /// counterpart, most recently committed first, with the default branch
    /// moved to the top.
    fn base_candidates(&self, root: &Path) -> Result<Vec<BaseCandidate>> {
        let refs = self.runner.stdout(&git(
            root,
            &[
                "for-each-ref",
                "--sort=-committerdate",
                "--format=%(refname)",
                "refs/heads",
                "refs/remotes",
            ],
        ))?;
        let mut local = Vec::new();
        let mut remote = Vec::new();
        for reference in refs.lines().map(str::trim) {
            if let Some(name) = reference.strip_prefix("refs/heads/") {
                local.push(name);
            } else if let Some(name) = reference.strip_prefix("refs/remotes/")
                && !name.ends_with("/HEAD")
            {
                remote.push(name);
            }
        }
        remote.retain(|name| {
            !name
                .split_once('/')
                .is_some_and(|(_, branch)| local.contains(&branch))
        });

        let candidate = |name: &str, remote| BaseCandidate {
            name: name.to_string(),
            remote,
            is_default: false,
        };
        let mut candidates: Vec<BaseCandidate> = local
            .iter()
            .map(|name| candidate(name, false))
            .chain(remote.iter().map(|name| candidate(name, true)))
            .collect();

        let head = self.runner.run(&git(
            root,
            &[
                "symbolic-ref",
                "--quiet",
                "--short",
                "refs/remotes/origin/HEAD",
            ],
        ))?;
        if head.success {
            let head = head.stdout.trim();
            let local_default = head.split_once('/').map(|(_, branch)| branch);
            let default = candidates
                .iter()
                .position(|c| !c.remote && Some(c.name.as_str()) == local_default)
                .or_else(|| candidates.iter().position(|c| c.remote && c.name == head));
            if let Some(index) = default {
                let mut default = candidates.remove(index);
                default.is_default = true;
                candidates.insert(0, default);
            }
        }
        Ok(candidates)
    }

    /// A fresh branch name for an issue: pi's slug, made unique.
    fn new_issue_branch(&self, root: &Path, item: &Item, log: &mut dyn FnMut(&str)) -> String {
        log("Asking pi for a branch name...");
        let slug = pi::generate_branch(&*self.runner, &self.pi, item).unwrap_or_else(|err| {
            let fallback = format!("issue-{}", item.number);
            log(&format!(
                "pi could not name the branch ({err:#}); using {fallback}"
            ));
            fallback
        });
        unique_branch(&slug, |branch| self.branch_exists(root, branch))
    }

    /// Generate the workspace label on a background thread. The thread is
    /// detached so a failing worktrunk run never waits for pi.
    fn spawn_label(&self, item: &Item) -> mpsc::Receiver<Result<String>> {
        let (tx, rx) = mpsc::channel();
        let runner = Arc::clone(&self.runner);
        let config = self.pi.clone();
        let item = item.clone();
        std::thread::spawn(move || {
            let _ = tx.send(pi::generate_label(&*runner, &config, &item));
        });
        rx
    }

    fn switch(&self, root: &Path, target: &[String]) -> Result<Switched> {
        let mut args = vec!["switch".to_string()];
        args.extend(target.iter().cloned());
        args.extend(["--no-cd".to_string(), "--format=json".to_string()]);
        let cmd = Cmd::new("wt", args).cwd(root).io(Io::CaptureStdout);
        let stdout = self
            .runner
            .stdout(&cmd)
            .context("worktrunk could not switch to the worktree")?;
        parse_switched(&stdout)
    }
}

/// `git ARGS...` run inside `root`.
fn git(root: &Path, args: &[&str]) -> Cmd {
    Cmd::new("git", args.iter().copied()).cwd(root)
}

#[derive(Debug, Deserialize)]
struct Switched {
    #[serde(default)]
    branch: String,
    path: PathBuf,
}

/// worktrunk prints one JSON object; tolerate stray lines around it.
fn parse_switched(stdout: &str) -> Result<Switched> {
    serde_json::from_str(stdout.trim())
        .ok()
        .or_else(|| {
            stdout
                .lines()
                .rev()
                .find_map(|line| serde_json::from_str(line.trim()).ok())
        })
        .with_context(|| format!("unexpected worktrunk output: {}", stdout.trim()))
}

fn open_workspace_of_branch(list: &WorktreeList, branch: &str) -> Option<String> {
    list.worktrees
        .iter()
        .find(|w| w.branch.as_deref() == Some(branch))
        .and_then(|w| w.open_workspace_id.clone())
}

/// The label for a new repository workspace: the repository name.
fn repository_label(list: &WorktreeList) -> String {
    let name = list.source.repo_name.trim_end_matches(".git");
    if !name.is_empty() {
        return name.to_string();
    }
    list.source.repo_root.file_name().map_or_else(
        || list.source.repo_root.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}

fn same_path(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::base_picker::BaseCandidate;
    use crate::config::Config;
    use crate::runner::Output;
    use crate::runner::fake::FakeRunner;
    use std::sync::Mutex;

    const ROOT: &str = "/src/github.com/acme/app";

    /// A simulated machine: a ghq root, a Herdr session, a git repository and
    /// worktrunk, answering the commands the opener runs.
    #[derive(Clone)]
    #[allow(clippy::struct_excessive_bools)] // independent switches of a test fixture
    struct World {
        cloned: bool,
        workspace: bool,
        /// (branch, path, open workspace id)
        worktrees: Vec<(String, String, Option<String>)>,
        /// local branches
        branches: Vec<String>,
        /// (branch, issue number) from git config
        issue_map: Vec<(String, u64)>,
        pi_label: Result<String, String>,
        pi_branch: Result<String, String>,
        wt_fails: bool,
        /// `gh config get git_protocol`
        git_protocol: String,
        /// `git for-each-ref` output, newest first
        refs: Vec<String>,
        /// `git symbolic-ref refs/remotes/origin/HEAD`
        origin_head: Option<String>,
        fetch_fails: bool,
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
            }
        }
    }

    fn worktree_path(branch: &str) -> String {
        if branch == "main" {
            ROOT.into()
        } else {
            format!("{ROOT}.{}", branch.replace('/', "-"))
        }
    }

    fn simulate(world: &Mutex<World>, cmd: &Cmd) -> Output {
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
            ("git", ["config", key, value]) => {
                let branch = key
                    .strip_prefix("branch.")
                    .and_then(|k| k.strip_suffix(&format!(".{ISSUE_CONFIG_KEY}")))
                    .expect("unexpected git config key");
                w.issue_map
                    .push((branch.to_string(), value.parse().unwrap()));
                Output::ok("")
            }
            ("git", ["fetch", "--all", "--quiet"]) => {
                assert_eq!(cmd.io, Io::Inherit, "fetch can prompt for credentials");
                if w.fetch_fails {
                    Output::failed("could not read from remote")
                } else {
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
                    serde_json::json!({"action": action, "branch": branch, "path": path})
                        .to_string(),
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

    fn pr() -> Item {
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
        }
    }

    fn issue() -> Item {
        Item {
            kind: Kind::Issue,
            number: 42,
            title: "Login times out".into(),
            url: "https://github.com/acme/app/issues/42".into(),
            head_ref: None,
            ..pr()
        }
    }

    struct Run {
        outcome: anyhow::Result<Outcome>,
        lines: Vec<String>,
        log: Vec<String>,
        world: World,
    }

    fn run(world: World, item: &Item) -> Run {
        run_with(world, item, None)
    }

    fn run_with(world: World, item: &Item, choose_base: Option<ChooseBase>) -> Run {
        run_full(world, item, choose_base, true)
    }

    fn run_full(world: World, item: &Item, choose_base: Option<ChooseBase>, focus: bool) -> Run {
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
        let outcome = opener.open_with(item, choose_base, &mut |line| log.push(line.to_string()));
        let world = world.lock().unwrap().clone();
        Run {
            outcome,
            lines: runner.lines(),
            log,
            world,
        }
    }

    fn ran(run: &Run, prefix: &str) -> bool {
        run.lines.iter().any(|l| l.starts_with(prefix))
    }

    #[test]
    fn opens_a_pull_request_in_a_new_worktree_workspace() {
        let r = run(World::default(), &pr());
        assert_eq!(
            r.outcome.as_ref().unwrap().clone(),
            Outcome::Opened {
                path: format!("{ROOT}.feat-x").into(),
                label: "PR #7 Add column".into()
            }
        );
        assert!(
            ran(&r, "wt switch pr:7 --no-cd --format=json"),
            "{:#?}",
            r.lines
        );
        assert_eq!(
            r.lines.last().unwrap(),
            &format!(
                "herdr worktree open --cwd {ROOT} --path {ROOT}.feat-x --label PR #7 Add column --focus --json"
            )
        );
        assert!(
            !ran(&r, "herdr workspace create"),
            "the repository workspace already exists"
        );
    }

    #[test]
    fn wt_runs_inside_the_repository() {
        let world = Arc::new(Mutex::new(World::default()));
        let shared = world.clone();
        let runner = Arc::new(FakeRunner::new(move |cmd| Ok(simulate(&shared, cmd))));
        Opener {
            runner: runner.clone(),
            herdr: Herdr::new("herdr"),
            pi: Config::default().pi,
            focus: true,
        }
        .open(&pr(), &mut |_| {})
        .unwrap();
        let wt = runner
            .calls()
            .into_iter()
            .find(|c| c.program == "wt")
            .unwrap();
        assert_eq!(wt.cwd.as_deref(), Some(Path::new(ROOT)));
    }

    #[test]
    fn focuses_a_pull_request_that_is_already_open() {
        let mut world = World::default();
        world
            .worktrees
            .push(("feat/x".into(), format!("{ROOT}.feat-x"), Some("w5".into())));
        let r = run(world, &pr());
        assert_eq!(
            r.outcome.as_ref().unwrap().clone(),
            Outcome::AlreadyOpen {
                workspace_id: "w5".into()
            }
        );
        assert_eq!(r.lines.last().unwrap(), "herdr workspace focus w5");
        assert!(!ran(&r, "wt "), "{:#?}", r.lines);
        assert!(
            !ran(&r, "pi "),
            "no label is generated for an open workspace"
        );
    }

    #[test]
    fn opens_in_the_background_when_focus_is_off() {
        let r = run_full(World::default(), &pr(), None, false);
        assert!(
            matches!(r.outcome, Ok(Outcome::Opened { .. })),
            "{:?}",
            r.outcome
        );
        let open = r.lines.last().unwrap();
        assert!(open.starts_with("herdr worktree open"), "{open}");
        assert!(open.ends_with("--no-focus --json"), "{open}");
    }

    #[test]
    fn leaves_an_open_workspace_alone_when_focus_is_off() {
        let mut world = World::default();
        world
            .worktrees
            .push(("feat/x".into(), format!("{ROOT}.feat-x"), Some("w5".into())));
        let r = run_full(world, &pr(), None, false);
        assert_eq!(
            r.outcome.as_ref().unwrap(),
            &Outcome::AlreadyOpen {
                workspace_id: "w5".into()
            }
        );
        assert!(!ran(&r, "herdr workspace focus"), "{:#?}", r.lines);
    }

    #[test]
    fn opens_an_existing_worktree_that_is_not_open_in_herdr() {
        let mut world = World::default();
        world
            .worktrees
            .push(("feat/x".into(), format!("{ROOT}.feat-x"), None));
        let r = run(world, &pr());
        assert!(matches!(
            r.outcome.as_ref().unwrap().clone(),
            Outcome::Opened { .. }
        ));
        assert!(ran(&r, "herdr worktree open"));
    }

    #[test]
    fn clones_a_missing_repository_with_ghq() {
        let world = World {
            cloned: false,
            ..World::default()
        };
        let r = run(world, &pr());
        assert!(r.outcome.is_ok(), "{:?}", r.outcome);
        assert!(ran(&r, "ghq get github.com/acme/app"), "{:#?}", r.lines);
        assert!(r.log.iter().any(|l| l.contains("Cloning")), "{:?}", r.log);
    }

    #[test]
    fn clones_over_ssh_when_gh_uses_ssh() {
        let world = World {
            cloned: false,
            git_protocol: "ssh".into(),
            ..World::default()
        };
        let r = run(world, &pr());
        assert!(r.outcome.is_ok(), "{:?}", r.outcome);
        assert!(ran(&r, "ghq get -p github.com/acme/app"), "{:#?}", r.lines);
    }

    #[test]
    fn creates_the_repository_workspace_when_missing() {
        let world = World {
            workspace: false,
            ..World::default()
        };
        let r = run(world, &pr());
        assert!(r.outcome.is_ok(), "{:?}", r.outcome);
        let create = r
            .lines
            .iter()
            .position(|l| l.starts_with("herdr workspace create"))
            .expect("workspace created");
        let open = r
            .lines
            .iter()
            .position(|l| l.starts_with("herdr worktree open"))
            .unwrap();
        assert!(create < open);
    }

    #[test]
    fn creates_an_issue_branch_named_by_pi_and_remembers_it() {
        let world = World {
            pi_label: Ok("Issue #42 Fix login".into()),
            ..World::default()
        };
        let r = run(world, &issue());
        assert_eq!(
            r.outcome.as_ref().unwrap().clone(),
            Outcome::Opened {
                path: format!("{ROOT}.fix-login-timeout").into(),
                label: "Issue #42 Fix login".into()
            }
        );
        assert!(
            ran(
                &r,
                "wt switch --create fix-login-timeout --no-cd --format=json"
            ),
            "{:#?}",
            r.lines
        );
        assert_eq!(
            r.world.issue_map,
            vec![("fix-login-timeout".to_string(), 42)]
        );
    }

    #[test]
    fn issue_branch_names_do_not_clobber_existing_branches() {
        let mut world = World::default();
        world.branches.push("fix-login-timeout".into());
        let r = run(world, &issue());
        assert!(r.outcome.is_ok(), "{:?}", r.outcome);
        assert!(
            ran(&r, "wt switch --create fix-login-timeout-2 "),
            "{:#?}",
            r.lines
        );
    }

    #[test]
    fn reuses_the_branch_recorded_for_an_issue() {
        let mut world = World::default();
        world.branches.push("login-fix".into());
        world.issue_map.push(("other".into(), 1));
        world.issue_map.push(("login-fix".into(), 42));
        let r = run(world, &issue());
        assert!(r.outcome.is_ok(), "{:?}", r.outcome);
        assert!(
            ran(&r, "wt switch login-fix --no-cd --format=json"),
            "{:#?}",
            r.lines
        );
        assert!(
            !r.lines
                .iter()
                .any(|l| l.starts_with("pi ") && l.contains("branch name")),
            "no new branch name is generated"
        );
    }

    #[test]
    fn ignores_a_recorded_branch_that_no_longer_exists() {
        let mut world = World::default();
        world.issue_map.push(("gone".into(), 42));
        let r = run(world, &issue());
        assert!(r.outcome.is_ok(), "{:?}", r.outcome);
        assert!(
            ran(&r, "wt switch --create fix-login-timeout "),
            "{:#?}",
            r.lines
        );
    }

    #[test]
    fn focuses_an_issue_worktree_that_is_already_open() {
        let mut world = World::default();
        world.branches.push("login-fix".into());
        world.issue_map.push(("login-fix".into(), 42));
        world.worktrees.push((
            "login-fix".into(),
            format!("{ROOT}.login-fix"),
            Some("w8".into()),
        ));
        let r = run(world, &issue());
        assert_eq!(
            r.outcome.as_ref().unwrap().clone(),
            Outcome::AlreadyOpen {
                workspace_id: "w8".into()
            }
        );
        assert!(!ran(&r, "wt "));
        assert!(!ran(&r, "pi "));
    }

    fn candidate(name: &str, remote: bool, is_default: bool) -> BaseCandidate {
        BaseCandidate {
            name: name.into(),
            remote,
            is_default,
        }
    }

    #[test]
    fn creates_an_issue_branch_from_a_chosen_base() {
        let mut offered = Vec::new();
        let mut choose = |candidates: &[BaseCandidate]| {
            offered = candidates.to_vec();
            Ok(Some("origin/epic/x".to_string()))
        };
        let r = run_with(World::default(), &issue(), Some(&mut choose));
        assert!(
            matches!(r.outcome, Ok(Outcome::Opened { .. })),
            "{:?}",
            r.outcome
        );
        assert!(
            ran(
                &r,
                "wt switch --create fix-login-timeout --base origin/epic/x --no-cd --format=json"
            ),
            "{:#?}",
            r.lines
        );
        assert_eq!(
            offered,
            vec![
                candidate("main", false, true),
                candidate("feature/login", false, false),
                candidate("origin/epic/x", true, false),
            ],
            "default first; origin/HEAD and remote copies of local branches are left out"
        );
        let fetch = r
            .lines
            .iter()
            .position(|l| l.starts_with("git fetch"))
            .expect("fetched");
        let list = r
            .lines
            .iter()
            .position(|l| l.starts_with("git for-each-ref"))
            .unwrap();
        assert!(fetch < list);
        assert_eq!(
            r.world.issue_map,
            vec![("fix-login-timeout".to_string(), 42)]
        );
    }

    #[test]
    fn a_remote_only_default_branch_is_offered_first() {
        let world = World {
            refs: vec![
                "refs/heads/feature/login".into(),
                "refs/remotes/origin/main".into(),
            ],
            ..World::default()
        };
        let mut offered = Vec::new();
        let mut choose = |candidates: &[BaseCandidate]| {
            offered = candidates.to_vec();
            Ok(None)
        };
        run_with(world, &issue(), Some(&mut choose));
        assert_eq!(offered[0], candidate("origin/main", true, true));
    }

    #[test]
    fn cancelling_the_base_choice_opens_nothing() {
        let r = run_with(
            World::default(),
            &issue(),
            Some(&mut |_: &[BaseCandidate]| Ok(None)),
        );
        assert_eq!(r.outcome.as_ref().unwrap(), &Outcome::Cancelled);
        assert!(!ran(&r, "wt "), "{:#?}", r.lines);
        assert!(!ran(&r, "pi "), "{:#?}", r.lines);
    }

    #[test]
    fn the_base_choice_is_skipped_for_an_issue_that_has_a_branch() {
        let mut world = World::default();
        world.branches.push("login-fix".into());
        world.issue_map.push(("login-fix".into(), 42));
        let r = run_with(
            world,
            &issue(),
            Some(
                &mut |_: &[BaseCandidate]| -> anyhow::Result<Option<String>> {
                    panic!("not asked")
                },
            ),
        );
        assert!(
            matches!(r.outcome, Ok(Outcome::Opened { .. })),
            "{:?}",
            r.outcome
        );
        assert!(ran(&r, "wt switch login-fix --no-cd"), "{:#?}", r.lines);
        assert!(
            r.log
                .iter()
                .any(|l| l.contains("login-fix") && l.contains("base")),
            "{:?}",
            r.log
        );
    }

    #[test]
    fn a_failed_fetch_still_offers_the_known_branches() {
        let world = World {
            fetch_fails: true,
            ..World::default()
        };
        let mut asked = false;
        let mut choose = |_: &[BaseCandidate]| {
            asked = true;
            Ok(Some("main".to_string()))
        };
        let r = run_with(world, &issue(), Some(&mut choose));
        assert!(asked);
        assert!(r.outcome.is_ok(), "{:?}", r.outcome);
        assert!(r.log.iter().any(|l| l.contains("fetch")), "{:?}", r.log);
    }

    #[test]
    fn falls_back_to_a_generated_label_when_pi_fails() {
        let world = World {
            pi_label: Err("No API key".into()),
            ..World::default()
        };
        let r = run(world, &pr());
        assert_eq!(
            r.outcome.as_ref().unwrap().clone(),
            Outcome::Opened {
                path: format!("{ROOT}.feat-x").into(),
                label: "PR #7 Add a column".into()
            }
        );
        assert!(
            r.log.iter().any(|l| l.contains("No API key")),
            "{:?}",
            r.log
        );
    }

    #[test]
    fn falls_back_to_an_issue_branch_when_pi_fails() {
        let world = World {
            pi_branch: Err("offline".into()),
            ..World::default()
        };
        let r = run(world, &issue());
        assert!(r.outcome.is_ok(), "{:?}", r.outcome);
        assert!(ran(&r, "wt switch --create issue-42 "), "{:#?}", r.lines);
        assert!(r.log.iter().any(|l| l.contains("offline")), "{:?}", r.log);
    }

    #[test]
    fn stops_when_worktrunk_fails() {
        let world = World {
            wt_fails: true,
            ..World::default()
        };
        let r = run(world, &pr());
        let err = r.outcome.as_ref().unwrap_err();
        assert!(format!("{err:#}").contains("hook failed"), "{err:#}");
        assert!(!ran(&r, "herdr worktree open"));
    }

    #[test]
    fn focuses_the_repository_workspace_for_its_main_checkout() {
        // An issue recorded against the branch checked out in the main
        // checkout: worktrunk lands on the repository root itself, which is
        // the repository workspace rather than a linked worktree.
        let mut world = World::default();
        world.worktrees[0].2 = None;
        world.issue_map.push(("main".into(), 42));
        let r = run(world, &issue());
        assert_eq!(
            r.outcome.as_ref().unwrap().clone(),
            Outcome::AlreadyOpen {
                workspace_id: "w1".into()
            }
        );
        assert!(!ran(&r, "herdr worktree open"), "{:#?}", r.lines);
    }
}

#[cfg(test)]
mod properties {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn worktrunk_output_never_panics(stdout in any::<String>()) {
            let _ = parse_switched(&stdout);
        }

        #[test]
        fn worktrunk_json_is_found_among_other_lines(
            noise in prop::collection::vec("[^{\n]*", 0..3),
            branch in any::<String>(),
            path in "/[^\0]*",
        ) {
            let json = serde_json::json!({"action": "created", "branch": branch, "path": path});
            let stdout = format!("{}\n{json}\n", noise.join("\n"));
            let switched = parse_switched(&stdout).unwrap();
            prop_assert_eq!(switched.branch, branch);
            prop_assert_eq!(switched.path, PathBuf::from(path));
        }
    }
}
