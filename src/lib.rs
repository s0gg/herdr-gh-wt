//! herdr-gh-wt: a Herdr plugin that shows GitHub pull requests awaiting your
//! review and issues assigned to you, and opens any of them as a worktrunk
//! worktree in a Herdr worktree workspace.

pub mod naming;
pub mod config;
pub mod runner;
pub mod github;
pub mod pi;
pub mod herdr;
pub mod open;
