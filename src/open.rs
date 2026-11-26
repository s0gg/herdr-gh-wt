//! Opening a pull request or issue as a worktrunk worktree in a Herdr
//! worktree workspace.
//!
//! 1. Find the local clone with `ghq` (cloning it with `ghq get` if needed,
//!    over SSH when that is gh's git protocol).
//! 2. Make sure Herdr has a workspace for the repository.
//! 3. If the item's branch is already open in Herdr, focus that workspace.
//! 4. Otherwise let worktrunk switch to (or create) the worktree — pull
//!    requests via `wt switch pr:N`, issues via their linked branch or a new
//!    branch named by pi and, optionally, started from a base branch the user
//!    picks — and open it with `herdr worktree open`, labelled by pi.
//!
//! An issue is linked to its branch in the repository's git config. The
//! plugin links the branches it creates, and the user can link an existing
//! branch instead.

use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};

use anyhow::{Context, Result, bail};
use serde::Deserialize;

use crate::config::PiConfig;
use crate::github::{Item, Kind};
use crate::herdr::{Herdr, WorktreeList};
use crate::naming::unique_branch;
use crate::pi;
use crate::picker::{BaseCandidate, BranchCandidate, SuggestedBranch};
use crate::runner::{Cmd, Io, Runner};

mod branches;
#[cfg(test)]
mod link_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod world;

pub use branches::ISSUE_CONFIG_KEY;
use branches::Repo;

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

/// Asks the user which local branch to link to an issue; `None` cancels.
pub type ChooseBranch<'a> = &'a mut dyn FnMut(&[BranchCandidate]) -> Result<Option<String>>;

/// What the user wants for an issue that has no linked branch yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BranchChoice {
    /// A new branch.
    New,
    /// This existing branch, linked to the issue.
    Existing(String),
}

/// Asks the user whether to create a new branch for an issue or to use one
/// of the branches that look like its own; `None` cancels.
pub type ChooseSuggested<'a> =
    &'a mut dyn FnMut(&[SuggestedBranch]) -> Result<Option<BranchChoice>>;

/// How an issue's branch is settled. Pull requests always use their own
/// branch.
pub enum IssueBranch<'a> {
    /// The issue's linked branch; without one, a new branch from the
    /// repository's default branch.
    Default,
    /// Like `Default`, but when branches look like the issue's own (GitHub
    /// links them to it, or their name has its number), the function decides
    /// between them and a new branch first.
    Suggest(ChooseSuggested<'a>),
    /// Like `Default`, but a new branch starts from a base that the function
    /// picks.
    FromBase(ChooseBase<'a>),
    /// Link the issue to an existing local branch that the function picks,
    /// in place of any branch it was linked to, and open that branch.
    Link(ChooseBranch<'a>),
}

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
        self.open_with(item, IssueBranch::Default, log)
    }

    /// Like [`Opener::open`], settling an issue's branch as `how` says.
    pub fn open_with(
        &self,
        item: &Item,
        mut how: IssueBranch,
        log: &mut dyn FnMut(&str),
    ) -> Result<Outcome> {
        let runner = &*self.runner;
        let repo_path = self.locate_repository(item, log)?;
        let list = self.herdr.worktree_list(runner, &repo_path)?;
        let root = list.source.repo_root.clone();
        let repo = Repo {
            runner,
            root: &root,
        };
        let root_workspace = if let Some(id) = list.source.source_workspace_id.clone() {
            id
        } else {
            let label = repository_label(&list);
            log(&format!("Creating Herdr workspace \"{label}\""));
            self.herdr.workspace_create(runner, &root, &label)?
        };

        // The branch the item lives on, when it is known up front.
        let known_branch = match (item.kind, &mut how) {
            (Kind::PullRequest, _) => item.head_ref.clone(),
            (Kind::Issue, IssueBranch::Link(choose)) => {
                let candidates = repo.link_candidates(&list)?;
                let Some(branch) = choose(&candidates)? else {
                    return Ok(Outcome::Cancelled);
                };
                repo.link(&branch, item.number, log)?;
                Some(branch)
            }
            (Kind::Issue, IssueBranch::Suggest(choose)) => match repo.linked_branch(item.number)? {
                Some(branch) => Some(branch),
                None => match ask_suggested(&repo, &list, item, choose, log)? {
                    None => return Ok(Outcome::Cancelled),
                    Some(BranchChoice::New) => None,
                    Some(BranchChoice::Existing(branch)) => {
                        repo.link(&branch, item.number, log)?;
                        Some(branch)
                    }
                },
            },
            (Kind::Issue, _) => repo.linked_branch(item.number)?,
        };
        if let (Kind::Issue, Some(branch), IssueBranch::FromBase(_)) =
            (item.kind, &known_branch, &how)
        {
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
                let base = match how {
                    IssueBranch::FromBase(choose) => match ask_base(&repo, choose, log)? {
                        Some(base) => Some(base),
                        None => return Ok(Outcome::Cancelled),
                    },
                    IssueBranch::Default | IssueBranch::Suggest(_) | IssueBranch::Link(_) => None,
                };
                let mut target = vec![
                    "--create".to_string(),
                    self.new_issue_branch(&repo, item, log),
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
            repo.record_link(&switched.branch, item.number, log);
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

    /// A fresh branch name for an issue: pi's slug, made unique.
    fn new_issue_branch(&self, repo: &Repo, item: &Item, log: &mut dyn FnMut(&str)) -> String {
        log("Asking pi for a branch name...");
        let slug = pi::generate_branch(&*self.runner, &self.pi, item).unwrap_or_else(|err| {
            let fallback = format!("issue-{}", item.number);
            log(&format!(
                "pi could not name the branch ({err:#}); using {fallback}"
            ));
            fallback
        });
        unique_branch(&slug, |branch| repo.branch_exists(branch))
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

/// Refresh the remote-tracking branches and let `choose` pick a base.
fn ask_base(repo: &Repo, choose: ChooseBase, log: &mut dyn FnMut(&str)) -> Result<Option<String>> {
    log("Fetching remote branches...");
    repo.fetch_all(log);
    choose(&repo.base_candidates()?)
}

/// When branches look like issue `item`'s own, let `choose` decide between
/// them and a new branch; a new branch when none do.
fn ask_suggested(
    repo: &Repo,
    list: &WorktreeList,
    item: &Item,
    choose: ChooseSuggested,
    log: &mut dyn FnMut(&str),
) -> Result<Option<BranchChoice>> {
    let suggestions = repo.suggestions(list, item, log)?;
    if suggestions.is_empty() {
        return Ok(Some(BranchChoice::New));
    }
    choose(&suggestions)
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
