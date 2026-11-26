//! The opener's tests for issues and existing branches: linking a branch
//! the user picks, and offering branches that look like an issue's own.

use std::path::{Path, PathBuf};

use super::world::*;
use super::*;
use crate::picker::{BranchCandidate, Reason, SuggestedBranch};

/// A repository with a worktree made outside the plugin (`login-fix`)
/// and a branch without a worktree (`old-try`).
fn world_with_branches() -> World {
    let mut world = World::default();
    world
        .branches
        .extend(["login-fix".into(), "old-try".into()]);
    world
        .worktrees
        .push(("login-fix".into(), format!("{ROOT}.login-fix"), None));
    world.refs = [
        "refs/heads/old-try",
        "refs/heads/main",
        "refs/heads/login-fix",
        "refs/remotes/origin/main",
        "refs/remotes/origin/epic/x",
    ]
    .map(String::from)
    .to_vec();
    world
}

fn branch(name: &str, worktree: Option<&str>, open: bool, issue: Option<u64>) -> BranchCandidate {
    BranchCandidate {
        name: name.into(),
        worktree: worktree.map(PathBuf::from),
        open,
        issue,
    }
}

#[test]
fn links_a_chosen_branch_to_an_issue_and_opens_it() {
    let mut offered = Vec::new();
    let mut choose = |candidates: &[BranchCandidate]| {
        offered = candidates.to_vec();
        Ok(Some("login-fix".to_string()))
    };
    let r = run_with(
        world_with_branches(),
        &issue(),
        IssueBranch::Link(&mut choose),
    );
    assert_eq!(
        offered,
        vec![
            branch("login-fix", Some(&format!("{ROOT}.login-fix")), false, None),
            branch("old-try", None, false, None),
            branch("main", Some(ROOT), true, None),
        ],
        "worktrees first, then the other local branches, newest first"
    );
    assert_eq!(r.world.issue_map, vec![("login-fix".to_string(), 42)]);
    assert!(
        ran(&r, "wt switch login-fix --no-cd --format=json"),
        "{:#?}",
        r.lines
    );
    assert!(
        matches!(&r.outcome, Ok(Outcome::Opened { path, .. }) if *path == Path::new(&format!("{ROOT}.login-fix"))),
        "{:?}",
        r.outcome
    );
    assert!(
        !r.lines
            .iter()
            .any(|l| l.starts_with("pi ") && l.contains("branch name")),
        "no new branch name is generated"
    );
    assert!(
        r.log
            .iter()
            .any(|l| l.contains("login-fix") && l.contains("#42")),
        "{:?}",
        r.log
    );
}

#[test]
fn linking_moves_the_issue_off_its_previous_branch() {
    let mut world = world_with_branches();
    world.issue_map = vec![("old-try".into(), 42), ("main".into(), 7)];
    let mut offered = Vec::new();
    let mut choose = |candidates: &[BranchCandidate]| {
        offered = candidates.to_vec();
        Ok(Some("login-fix".to_string()))
    };
    let r = run_with(world, &issue(), IssueBranch::Link(&mut choose));
    assert!(r.outcome.is_ok(), "{:?}", r.outcome);
    assert_eq!(offered[1], branch("old-try", None, false, Some(42)));
    assert_eq!(offered[2].issue, Some(7));
    assert_eq!(
        r.world.issue_map,
        vec![("main".to_string(), 7), ("login-fix".to_string(), 42)]
    );
}

#[test]
fn linking_a_branch_of_another_issue_takes_it_over() {
    let mut world = world_with_branches();
    world.issue_map = vec![("login-fix".into(), 9)];
    let r = run_with(
        world,
        &issue(),
        IssueBranch::Link(&mut |_: &[BranchCandidate]| Ok(Some("login-fix".to_string()))),
    );
    assert!(r.outcome.is_ok(), "{:?}", r.outcome);
    assert_eq!(r.world.issue_map, vec![("login-fix".to_string(), 42)]);
    assert!(
        r.log
            .iter()
            .any(|l| l.contains("login-fix") && l.contains("issue #9")),
        "says which issue lost the branch: {:?}",
        r.log
    );
}

#[test]
fn linking_a_branch_open_in_herdr_focuses_it() {
    let mut world = world_with_branches();
    world.worktrees[1].2 = Some("w8".into());
    let r = run_with(
        world,
        &issue(),
        IssueBranch::Link(&mut |_: &[BranchCandidate]| Ok(Some("login-fix".to_string()))),
    );
    assert_eq!(
        r.outcome.as_ref().unwrap(),
        &Outcome::AlreadyOpen {
            workspace_id: "w8".into()
        }
    );
    assert_eq!(r.world.issue_map, vec![("login-fix".to_string(), 42)]);
    assert!(!ran(&r, "wt "), "{:#?}", r.lines);
}

#[test]
fn linking_a_branch_without_a_worktree_creates_one() {
    let r = run_with(
        world_with_branches(),
        &issue(),
        IssueBranch::Link(&mut |_: &[BranchCandidate]| Ok(Some("old-try".to_string()))),
    );
    assert!(
        matches!(r.outcome, Ok(Outcome::Opened { .. })),
        "{:?}",
        r.outcome
    );
    assert!(ran(&r, "wt switch old-try --no-cd"), "{:#?}", r.lines);
    assert_eq!(r.world.issue_map, vec![("old-try".to_string(), 42)]);
}

#[test]
fn cancelling_the_link_choice_changes_nothing() {
    let mut world = world_with_branches();
    world.issue_map = vec![("old-try".into(), 42)];
    let r = run_with(
        world,
        &issue(),
        IssueBranch::Link(&mut |_: &[BranchCandidate]| Ok(None)),
    );
    assert_eq!(r.outcome.as_ref().unwrap(), &Outcome::Cancelled);
    assert_eq!(r.world.issue_map, vec![("old-try".to_string(), 42)]);
    assert!(!ran(&r, "wt "), "{:#?}", r.lines);
    assert!(!ran(&r, "pi "), "{:#?}", r.lines);
}

#[test]
fn pull_requests_are_never_linked() {
    let r = run_with(
        World::default(),
        &pr(),
        IssueBranch::Link(
            &mut |_: &[BranchCandidate]| -> anyhow::Result<Option<String>> { panic!("not asked") },
        ),
    );
    assert!(r.outcome.is_ok(), "{:?}", r.outcome);
    assert!(ran(&r, "wt switch pr:7 "), "{:#?}", r.lines);
    assert_eq!(r.world.issue_map, Vec::new());
}

/// Branches that look like issue #42's: one with its number, one of a
/// pull request that will close it, and one only on the remote that
/// GitHub links to it.
fn world_with_lookalikes() -> World {
    let mut world = World::default();
    world.branches.extend([
        "unrelated".into(),
        "issue-42-login".into(),
        "login-redesign".into(),
    ]);
    world.worktrees.push((
        "issue-42-login".into(),
        format!("{ROOT}.issue-42-login"),
        None,
    ));
    world.refs = [
        "refs/heads/unrelated",
        "refs/heads/issue-42-login",
        "refs/heads/login-redesign",
        "refs/heads/main",
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/gh-linked",
        "refs/remotes/origin/login-redesign",
        // Someone else's branch: only GitHub's links count on a remote.
        "refs/remotes/origin/42-other-approach",
        "refs/remotes/origin/main",
    ]
    .map(String::from)
    .to_vec();
    world
}

fn issue_with_hints() -> Item {
    Item {
        linked_branches: vec!["gh-linked".into()],
        closing_pull_requests: vec![crate::github::PullRef {
            number: 50,
            head_ref: "login-redesign".into(),
        }],
        ..issue()
    }
}

fn suggested(branch: BranchCandidate, remote: Option<&str>, reasons: &[Reason]) -> SuggestedBranch {
    SuggestedBranch {
        branch,
        remote: remote.map(String::from),
        reasons: reasons.to_vec(),
    }
}

#[test]
fn suggests_branches_that_look_like_the_issue_before_creating_one() {
    let mut offered = Vec::new();
    let mut choose = |suggestions: &[SuggestedBranch]| {
        offered = suggestions.to_vec();
        Ok(Some(BranchChoice::Existing("login-redesign".into())))
    };
    let r = run_with(
        world_with_lookalikes(),
        &issue_with_hints(),
        IssueBranch::Suggest(&mut choose),
    );
    assert_eq!(
        offered,
        vec![
            suggested(
                branch("login-redesign", None, false, None),
                None,
                &[Reason::ClosingPullRequest(50)]
            ),
            suggested(
                branch("gh-linked", None, false, None),
                Some("origin/gh-linked"),
                &[Reason::LinkedOnGitHub]
            ),
            suggested(
                branch(
                    "issue-42-login",
                    Some(&format!("{ROOT}.issue-42-login")),
                    false,
                    None
                ),
                None,
                &[Reason::MentionsNumber]
            ),
        ],
        "GitHub's links first, newest first; remote copies of local branches are left out"
    );
    assert!(
        ran(&r, "wt switch login-redesign --no-cd"),
        "{:#?}",
        r.lines
    );
    assert_eq!(r.world.issue_map, vec![("login-redesign".to_string(), 42)]);
    assert!(
        !ran(&r, "git fetch"),
        "every branch GitHub knows is here: {:#?}",
        r.lines
    );
    assert!(
        matches!(r.outcome, Ok(Outcome::Opened { .. })),
        "{:?}",
        r.outcome
    );
}

#[test]
fn a_suggested_remote_branch_is_checked_out_by_worktrunk() {
    let r = run_with(
        world_with_lookalikes(),
        &issue_with_hints(),
        IssueBranch::Suggest(&mut |_: &[SuggestedBranch]| {
            Ok(Some(BranchChoice::Existing("gh-linked".into())))
        }),
    );
    assert!(r.outcome.is_ok(), "{:?}", r.outcome);
    assert!(ran(&r, "wt switch gh-linked --no-cd"), "{:#?}", r.lines);
    assert_eq!(r.world.issue_map, vec![("gh-linked".to_string(), 42)]);
}

#[test]
fn a_new_branch_can_be_chosen_over_the_suggestions() {
    let r = run_with(
        world_with_lookalikes(),
        &issue_with_hints(),
        IssueBranch::Suggest(&mut |_: &[SuggestedBranch]| Ok(Some(BranchChoice::New))),
    );
    assert!(r.outcome.is_ok(), "{:?}", r.outcome);
    assert!(
        ran(&r, "wt switch --create fix-login-timeout "),
        "{:#?}",
        r.lines
    );
    assert_eq!(
        r.world.issue_map,
        vec![("fix-login-timeout".to_string(), 42)]
    );
}

#[test]
fn cancelling_the_suggestions_opens_nothing() {
    let r = run_with(
        world_with_lookalikes(),
        &issue_with_hints(),
        IssueBranch::Suggest(&mut |_: &[SuggestedBranch]| Ok(None)),
    );
    assert_eq!(r.outcome.as_ref().unwrap(), &Outcome::Cancelled);
    assert!(!ran(&r, "wt "), "{:#?}", r.lines);
    assert!(!ran(&r, "pi "), "{:#?}", r.lines);
    assert_eq!(r.world.issue_map, Vec::new());
}

fn never_asked(_: &[SuggestedBranch]) -> anyhow::Result<Option<BranchChoice>> {
    panic!("not asked")
}

#[test]
fn nothing_is_asked_without_lookalikes() {
    let r = run_with(
        World::default(),
        &issue(),
        IssueBranch::Suggest(&mut never_asked),
    );
    assert!(r.outcome.is_ok(), "{:?}", r.outcome);
    assert!(
        ran(&r, "wt switch --create fix-login-timeout "),
        "{:#?}",
        r.lines
    );
}

#[test]
fn nothing_is_asked_for_a_linked_issue_or_a_pull_request() {
    let mut world = world_with_lookalikes();
    world.issue_map.push(("unrelated".into(), 42));
    let r = run_with(
        world,
        &issue_with_hints(),
        IssueBranch::Suggest(&mut never_asked),
    );
    assert!(ran(&r, "wt switch unrelated --no-cd"), "{:#?}", r.lines);

    let r = run_with(
        world_with_lookalikes(),
        &pr(),
        IssueBranch::Suggest(&mut never_asked),
    );
    assert!(ran(&r, "wt switch pr:7 "), "{:#?}", r.lines);
}

#[test]
fn branches_of_other_issues_are_not_suggested() {
    let mut world = world_with_lookalikes();
    world.issue_map.push(("issue-42-login".into(), 7));
    let r = run_with(world, &issue(), IssueBranch::Suggest(&mut never_asked));
    assert!(
        ran(&r, "wt switch --create fix-login-timeout "),
        "{:#?}",
        r.lines
    );
}

#[test]
fn fetches_a_branch_github_links_but_the_clone_lacks() {
    let world = World {
        fetched_refs: vec!["refs/remotes/origin/gh-new".into()],
        ..World::default()
    };
    let item = Item {
        linked_branches: vec!["gh-new".into()],
        ..issue()
    };
    let mut offered = Vec::new();
    let mut choose = |suggestions: &[SuggestedBranch]| {
        offered = suggestions.to_vec();
        Ok(None)
    };
    let r = run_with(world, &item, IssueBranch::Suggest(&mut choose));
    assert!(ran(&r, "git fetch --all --quiet"), "{:#?}", r.lines);
    assert_eq!(
        offered,
        vec![suggested(
            branch("gh-new", None, false, None),
            Some("origin/gh-new"),
            &[Reason::LinkedOnGitHub]
        )]
    );
}

#[test]
fn a_branch_github_links_that_cannot_be_fetched_is_skipped() {
    let item = Item {
        linked_branches: vec!["gh-deleted".into()],
        ..issue()
    };
    let r = run_with(
        World::default(),
        &item,
        IssueBranch::Suggest(&mut never_asked),
    );
    assert!(r.outcome.is_ok(), "{:?}", r.outcome);
    assert!(
        ran(&r, "wt switch --create fix-login-timeout "),
        "{:#?}",
        r.lines
    );
}
