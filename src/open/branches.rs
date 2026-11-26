//! What the opener knows about a clone's branches: which issue each branch
//! is linked to, and which branches to offer as a base, as a branch to link,
//! or as an issue's possible own branch.

use std::path::Path;

use anyhow::{Context, Result};

use super::same_path;
use crate::github::Item;
use crate::herdr::WorktreeList;
use crate::picker::{BaseCandidate, BranchCandidate, Reason, SuggestedBranch};
use crate::runner::{Cmd, Io, Runner};

/// Git config variable (`branch.<name>.herdr-gh-wt-issue`) that links a
/// branch to an issue. Git drops it together with the branch.
pub const ISSUE_CONFIG_KEY: &str = "herdr-gh-wt-issue";

/// A local clone, at `root`.
pub(super) struct Repo<'a> {
    pub runner: &'a dyn Runner,
    pub root: &'a Path,
}

/// The branches a clone knows, each list most recently committed first.
struct Refs {
    local: Vec<String>,
    /// Remote-tracking branches (`origin/x`), leaving out the remotes' `HEAD`.
    remote: Vec<String>,
}

impl Refs {
    /// Whether `branch` is a local branch or a remote-tracking one.
    fn knows(&self, branch: &str) -> bool {
        self.local.iter().any(|b| b == branch)
            || self
                .remote
                .iter()
                .any(|r| remote_branch(r).is_some_and(|b| b == branch))
    }
}

impl Repo<'_> {
    /// `git ARGS...` run inside the clone.
    fn git(&self, args: &[&str]) -> Cmd {
        Cmd::new("git", args.iter().copied()).cwd(self.root)
    }

    pub fn branch_exists(&self, branch: &str) -> bool {
        let reference = format!("refs/heads/{branch}");
        self.runner
            .run(&self.git(&["show-ref", "--verify", "--quiet", &reference]))
            .is_ok_and(|out| out.success)
    }

    /// Every (branch, issue number) link in the clone's git config.
    fn issue_links(&self) -> Result<Vec<(String, u64)>> {
        let pattern = format!("^branch\\..*\\.{ISSUE_CONFIG_KEY}$");
        let out = self
            .runner
            .run(&self.git(&["config", "--get-regexp", &pattern]))?;
        if !out.success {
            // git exits with 1 when nothing matches.
            return Ok(Vec::new());
        }
        let suffix = format!(".{ISSUE_CONFIG_KEY}");
        Ok(out
            .stdout
            .lines()
            .filter_map(|line| {
                let (key, value) = line.split_once(' ')?;
                let branch = key.strip_prefix("branch.")?.strip_suffix(&suffix)?;
                Some((branch.to_string(), value.trim().parse().ok()?))
            })
            .collect())
    }

    /// The existing branch linked to issue `number`, if any.
    pub fn linked_branch(&self, number: u64) -> Result<Option<String>> {
        Ok(self
            .issue_links()?
            .into_iter()
            .filter(|&(_, n)| n == number)
            .map(|(branch, _)| branch)
            .find(|branch| self.branch_exists(branch)))
    }

    /// Link a branch the plugin just created to its issue. A failure only
    /// costs the link, so it is reported and otherwise ignored.
    pub fn record_link(&self, branch: &str, number: u64, log: &mut dyn FnMut(&str)) {
        let recorded =
            self.runner
                .stdout(&self.git(&["config", &issue_key(branch), &number.to_string()]));
        if let Err(err) = recorded {
            log(&format!(
                "Could not record the issue on branch {branch}: {err:#}"
            ));
        }
    }

    /// Link `branch` to issue `number`, unlinking any other branch of that
    /// issue. A branch has one issue, so another issue's link is replaced.
    pub fn link(&self, branch: &str, number: u64, log: &mut dyn FnMut(&str)) -> Result<()> {
        for (other, linked) in self.issue_links()? {
            if other == branch && linked != number {
                log(&format!(
                    "Branch {branch} was linked to issue #{linked}; linking it to #{number} instead"
                ));
            } else if other != branch && linked == number {
                self.runner
                    .stdout(&self.git(&["config", "--unset-all", &issue_key(&other)]))
                    .with_context(|| format!("could not unlink branch {other}"))?;
                log(&format!("Unlinked branch {other}"));
            }
        }
        self.runner
            .stdout(&self.git(&["config", &issue_key(branch), &number.to_string()]))
            .with_context(|| format!("could not link branch {branch}"))?;
        log(&format!("Linked branch {branch} to issue #{number}"));
        Ok(())
    }

    /// `git fetch --all`, where it can ask for credentials. A failure is
    /// reported and otherwise ignored: the branches known locally remain.
    pub fn fetch_all(&self, log: &mut dyn FnMut(&str)) {
        let fetched = self
            .runner
            .run(&self.git(&["fetch", "--all", "--quiet"]).io(Io::Inherit));
        match fetched {
            Ok(out) if out.success => {}
            Ok(_) => log("git fetch failed; using the branches known locally"),
            Err(err) => log(&format!(
                "git fetch failed ({err:#}); using the branches known locally"
            )),
        }
    }

    fn refs(&self) -> Result<Refs> {
        let refs = self.runner.stdout(&self.git(&[
            "for-each-ref",
            "--sort=-committerdate",
            "--format=%(refname)",
            "refs/heads",
            "refs/remotes",
        ]))?;
        let mut local = Vec::new();
        let mut remote = Vec::new();
        for reference in refs.lines().map(str::trim) {
            if let Some(name) = reference.strip_prefix("refs/heads/") {
                local.push(name.to_string());
            } else if let Some(name) = reference.strip_prefix("refs/remotes/")
                && !name.ends_with("/HEAD")
            {
                remote.push(name.to_string());
            }
        }
        Ok(Refs { local, remote })
    }

    /// Local branch `name`, with its worktree in `list` and its issue link.
    fn candidate(name: &str, list: &WorktreeList, links: &[(String, u64)]) -> BranchCandidate {
        let worktree = list
            .worktrees
            .iter()
            .find(|w| w.branch.as_deref() == Some(name));
        BranchCandidate {
            name: name.to_string(),
            worktree: worktree.map(|w| w.path.clone()),
            open: worktree.is_some_and(|w| w.open_workspace_id.is_some()),
            issue: links
                .iter()
                .find(|(branch, _)| branch == name)
                .map(|&(_, n)| n),
        }
    }

    /// Local branches an issue can be linked to: branches checked out in a
    /// linked worktree first, then the others, each group most recently
    /// committed first.
    pub fn link_candidates(&self, list: &WorktreeList) -> Result<Vec<BranchCandidate>> {
        let links = self.issue_links()?;
        let (mut candidates, others): (Vec<_>, Vec<_>) = self
            .refs()?
            .local
            .iter()
            .map(|name| Self::candidate(name, list, &links))
            .partition(|c| {
                c.worktree
                    .as_deref()
                    .is_some_and(|p| !same_path(p, self.root))
            });
        candidates.extend(others);
        Ok(candidates)
    }

    /// Branches that look like issue `item`'s own and belong to no other
    /// issue: those GitHub links to it (fetched first when this clone lacks
    /// them), then those whose name has its number; each group most recently
    /// committed first. Branches only on a remote are offered when GitHub
    /// links them.
    pub fn suggestions(
        &self,
        list: &WorktreeList,
        item: &Item,
        log: &mut dyn FnMut(&str),
    ) -> Result<Vec<SuggestedBranch>> {
        let reasons = |name: &str, local: bool| {
            let mut reasons = Vec::new();
            if item.linked_branches.iter().any(|b| b == name) {
                reasons.push(Reason::LinkedOnGitHub);
            }
            reasons.extend(
                item.closing_pull_requests
                    .iter()
                    .filter(|pr| pr.head_ref == name)
                    .map(|pr| Reason::ClosingPullRequest(pr.number)),
            );
            if local && mentions_number(name, item.number) {
                reasons.push(Reason::MentionsNumber);
            }
            reasons
        };

        let mut refs = self.refs()?;
        let lacks_one_from_github = item
            .linked_branches
            .iter()
            .chain(item.closing_pull_requests.iter().map(|pr| &pr.head_ref))
            .any(|name| !refs.knows(name));
        if lacks_one_from_github {
            log("Fetching the branches GitHub links to the issue...");
            self.fetch_all(log);
            refs = self.refs()?;
        }
        let links = self.issue_links()?;

        let mut suggestions: Vec<SuggestedBranch> = refs
            .local
            .iter()
            .map(|name| SuggestedBranch {
                branch: Self::candidate(name, list, &links),
                remote: None,
                reasons: reasons(name, true),
            })
            .collect();
        for remote in &refs.remote {
            let Some(name) = remote_branch(remote) else {
                continue;
            };
            if suggestions.iter().any(|s| s.branch.name == name) {
                continue;
            }
            suggestions.push(SuggestedBranch {
                branch: Self::candidate(name, list, &links),
                remote: Some(remote.clone()),
                reasons: reasons(name, false),
            });
        }
        suggestions
            .retain(|s| !s.reasons.is_empty() && s.branch.issue.is_none_or(|n| n == item.number));
        suggestions.sort_by_key(|s| !s.reasons.iter().any(|r| r.is_from_github()));
        Ok(suggestions)
    }

    /// Local branches, then remote-tracking branches without a local
    /// counterpart, most recently committed first, with the default branch
    /// moved to the top.
    pub fn base_candidates(&self) -> Result<Vec<BaseCandidate>> {
        let Refs { local, mut remote } = self.refs()?;
        remote.retain(|name| {
            !remote_branch(name).is_some_and(|branch| local.iter().any(|l| l == branch))
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

        let head = self.runner.run(&self.git(&[
            "symbolic-ref",
            "--quiet",
            "--short",
            "refs/remotes/origin/HEAD",
        ]))?;
        if head.success {
            let head = head.stdout.trim();
            let local_default = remote_branch(head);
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
}

/// `origin/feature/x` -> `feature/x`.
fn remote_branch(remote_tracking: &str) -> Option<&str> {
    remote_tracking.split_once('/').map(|(_, branch)| branch)
}

/// The git config key that links `branch` to an issue.
fn issue_key(branch: &str) -> String {
    format!("branch.{branch}.{ISSUE_CONFIG_KEY}")
}

/// Whether issue `number` is one of the parts of a branch name, split at
/// `-`, `_` and `/`: the whole part, or after `issue` or `gh`, in any case.
fn mentions_number(branch: &str, number: u64) -> bool {
    let number = number.to_string();
    branch.split(['-', '_', '/']).any(|part| {
        let part = part.to_ascii_lowercase();
        part == number
            || ["issue", "gh"]
                .iter()
                .any(|prefix| part.strip_prefix(prefix) == Some(number.as_str()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn issue_numbers_are_matched_as_whole_parts_of_a_branch_name() {
        for name in [
            "42",
            "42-fix-login",
            "fix-login-42",
            "issue-42",
            "feature/42-login",
            "fix_42",
            "GH-42",
            "issue42",
            "Issue42-login",
            "gh42",
        ] {
            assert!(mentions_number(name, 42), "{name}");
        }
        for name in [
            "142-fix",
            "420",
            "fix-4-2",
            "release-4.2",
            "v42",
            "pr42",
            "main",
        ] {
            assert!(!mentions_number(name, 42), "{name}");
        }
    }

    #[test]
    fn remote_tracking_branches_lose_their_remote() {
        assert_eq!(remote_branch("origin/feature/x"), Some("feature/x"));
        assert_eq!(remote_branch("main"), None);
    }
}

#[cfg(test)]
mod properties {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn issue_numbers_match_as_parts_but_not_inside_other_numbers(
            number in 1u64..1_000_000,
            digit in 0u8..10,
            before in "[a-z]{0,8}",
            after in "[a-z]{0,8}",
            separator in prop::sample::select(vec!["-", "_", "/"]),
        ) {
            let part = |text: &str| -> String {
                [before.as_str(), text, after.as_str()]
                    .into_iter()
                    .filter(|p| !p.is_empty())
                    .collect::<Vec<_>>()
                    .join(separator)
            };
            let alone = part(&number.to_string());
            let prefixed = part(&format!("issue{number}"));
            let longer = part(&format!("{number}{digit}"));
            let led = part(&format!("{digit}{number}"));
            prop_assert!(mentions_number(&alone, number), "{}", alone);
            prop_assert!(mentions_number(&prefixed, number), "{}", prefixed);
            prop_assert!(!mentions_number(&longer, number), "{}", longer);
            prop_assert!(!mentions_number(&led, number), "{}", led);
        }
    }
}
