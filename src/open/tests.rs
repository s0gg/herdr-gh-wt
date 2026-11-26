//! The opener's tests: pull requests, new issue branches and their base,
//! focus, and failures.

use std::path::Path;
use std::sync::{Arc, Mutex};

use super::world::*;
use super::*;
use crate::config::Config;
use crate::picker::BaseCandidate;
use crate::runner::fake::FakeRunner;

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
    let r = run_full(World::default(), &pr(), IssueBranch::Default, false);
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
    let r = run_full(world, &pr(), IssueBranch::Default, false);
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
    let r = run_with(
        World::default(),
        &issue(),
        IssueBranch::FromBase(&mut choose),
    );
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
    run_with(world, &issue(), IssueBranch::FromBase(&mut choose));
    assert_eq!(offered[0], candidate("origin/main", true, true));
}

#[test]
fn cancelling_the_base_choice_opens_nothing() {
    let r = run_with(
        World::default(),
        &issue(),
        IssueBranch::FromBase(&mut |_: &[BaseCandidate]| Ok(None)),
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
        IssueBranch::FromBase(
            &mut |_: &[BaseCandidate]| -> anyhow::Result<Option<String>> { panic!("not asked") },
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
    let r = run_with(world, &issue(), IssueBranch::FromBase(&mut choose));
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
