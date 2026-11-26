//! herdr-gh-wt: a Herdr plugin that shows GitHub pull requests awaiting your
//! review and issues assigned to you, and opens any of them as a worktrunk
//! worktree in a Herdr worktree workspace.

pub mod app;
pub mod config;
pub mod github;
pub mod herdr;
pub mod naming;
pub mod open;
pub mod pi;
pub mod picker;
pub mod runner;
pub mod ui;
