//! Naming worktrees and workspaces with the `pi` coding agent.

use std::time::Duration;

use anyhow::{Result, anyhow};

use crate::config::PiConfig;
use crate::github::{Item, Kind};
use crate::naming::{sanitize_branch_slug, sanitize_label};
use crate::runner::{Cmd, Runner};

/// Longest issue/PR description passed to the model, in characters.
pub const MAX_BODY_CHARS: usize = 1500;

/// Fill the `{kind}`, `{ref}`, `{repo}`, `{number}`, `{title}`, `{labels}`
/// and `{body}` placeholders of `template` from `item`.
pub fn render_prompt(template: &str, item: &Item) -> String {
    let value = |name: &str| -> Option<String> {
        Some(match name {
            "kind" => match item.kind {
                Kind::PullRequest => "pull request".to_string(),
                Kind::Issue => "issue".to_string(),
            },
            "ref" => item.reference(),
            "repo" => item.repo.clone(),
            "number" => item.number.to_string(),
            "title" => item.title.clone(),
            "labels" if item.labels.is_empty() => "(none)".to_string(),
            "labels" => item.labels.join(", "),
            "body" if item.body.trim().is_empty() => "(empty)".to_string(),
            "body" => truncate(item.body.trim(), MAX_BODY_CHARS),
            _ => return None,
        })
    };

    // Single pass, so placeholder-like text inside a title or body is kept verbatim.
    let mut out = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        match after
            .find('}')
            .and_then(|end| value(&after[..end]).map(|v| (end, v)))
        {
            Some((end, v)) => {
                out.push_str(&v);
                rest = &after[end + 1..];
            }
            None => {
                out.push('{');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

fn truncate(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut out: String = text.chars().take(max_chars).collect();
    out.push('…');
    out
}

/// The non-interactive pi invocation that answers `prompt`.
pub fn command(config: &PiConfig, prompt: &str) -> Cmd {
    let mut args: Vec<String> = [
        "-p",
        "--no-session",
        "--no-tools",
        "--no-extensions",
        "--no-skills",
        "--no-prompt-templates",
        "--no-context-files",
        "--no-mcp",
    ]
    .map(String::from)
    .to_vec();
    for (flag, value) in [
        ("--provider", &config.provider),
        ("--model", &config.model),
        ("--thinking", &config.thinking),
    ] {
        if let Some(value) = value {
            args.push(flag.to_string());
            args.push(value.clone());
        }
    }
    args.extend(config.extra_args.iter().cloned());
    args.push("--".to_string());
    args.push(prompt.to_string());

    // Run outside any repository so project-local pi settings don't apply.
    Cmd::new("pi", args)
        .cwd(std::env::temp_dir())
        .timeout(Duration::from_secs(config.timeout_secs))
}

fn ask(runner: &dyn Runner, config: &PiConfig, template: &str, item: &Item) -> Result<String> {
    let prompt = render_prompt(template, item);
    let out = runner.run(&command(config, &prompt))?;
    if !out.success {
        let detail = out.stderr.trim();
        anyhow::bail!(
            "pi failed: {}",
            if detail.is_empty() {
                "no error output"
            } else {
                detail
            }
        );
    }
    Ok(out.stdout)
}

/// Ask pi for a workspace label for `item`.
pub fn generate_label(runner: &dyn Runner, config: &PiConfig, item: &Item) -> Result<String> {
    let answer = ask(runner, config, &config.label_prompt, item)?;
    sanitize_label(&answer, config.label_max_chars)
        .ok_or_else(|| anyhow!("pi returned no usable label"))
}

/// Ask pi for a branch slug for `item`.
pub fn generate_branch(runner: &dyn Runner, config: &PiConfig, item: &Item) -> Result<String> {
    let answer = ask(runner, config, &config.branch_prompt, item)?;
    sanitize_branch_slug(&answer)
        .ok_or_else(|| anyhow!("pi returned no usable branch name: {:?}", answer.trim()))
}

/// The label used when pi is unavailable: `PR #12 <title>`.
pub fn fallback_label(item: &Item, max_chars: usize) -> String {
    let label = format!("{} {}", item.reference(), item.title);
    sanitize_label(&label, max_chars).unwrap_or_else(|| item.reference())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::runner::Output;
    use crate::runner::fake::FakeRunner;

    pub(crate) fn issue() -> Item {
        Item {
            kind: Kind::Issue,
            host: "github.com".into(),
            repo: "acme/app".into(),
            number: 42,
            title: "Login times out".into(),
            url: "https://github.com/acme/app/issues/42".into(),
            author: Some("bob".into()),
            updated_at: "2026-11-25T00:00:00Z".into(),
            labels: vec!["bug".into(), "auth".into()],
            body: "Steps to reproduce".into(),
            is_draft: false,
            head_ref: None,
            reviewers: vec![],
        }
    }

    fn pr() -> Item {
        Item {
            kind: Kind::PullRequest,
            number: 7,
            head_ref: Some("feat/x".into()),
            labels: vec![],
            body: String::new(),
            ..issue()
        }
    }

    fn pi_config() -> PiConfig {
        Config::default().pi
    }

    #[test]
    fn renders_every_placeholder() {
        let out = render_prompt(
            "{kind}|{ref}|{repo}|{number}|{title}|{labels}|{body}",
            &issue(),
        );
        assert_eq!(
            out,
            "issue|Issue #42|acme/app|42|Login times out|bug, auth|Steps to reproduce"
        );
        let out = render_prompt("{kind}|{ref}|{labels}|{body}", &pr());
        assert_eq!(out, "pull request|PR #7|(none)|(empty)");
    }

    #[test]
    fn long_bodies_are_truncated() {
        let item = Item {
            body: "x".repeat(MAX_BODY_CHARS + 100),
            ..issue()
        };
        let out = render_prompt("{body}", &item);
        assert_eq!(out.chars().count(), MAX_BODY_CHARS + 1);
        assert!(out.ends_with('…'));
    }

    #[test]
    fn command_is_non_interactive_and_isolated() {
        let cmd = command(&pi_config(), "hello");
        assert_eq!(cmd.program, "pi");
        for flag in [
            "-p",
            "--no-session",
            "--no-tools",
            "--no-extensions",
            "--no-skills",
            "--no-context-files",
        ] {
            assert!(
                cmd.args.iter().any(|a| a == flag),
                "missing {flag} in {:?}",
                cmd.args
            );
        }
        assert!(
            !cmd.args.iter().any(|a| a == "--model"),
            "model is left to pi by default"
        );
        assert_eq!(cmd.args[cmd.args.len() - 2..], ["--", "hello"]);
        assert_eq!(cmd.timeout, Some(Duration::from_secs(30)));
    }

    #[test]
    fn command_passes_model_provider_thinking_and_extra_args() {
        let config = PiConfig {
            model: Some("anthropic/claude-haiku-4-5".into()),
            provider: Some("anthropic".into()),
            thinking: Some("low".into()),
            extra_args: vec!["--offline".into()],
            timeout_secs: 9,
            ..pi_config()
        };
        let cmd = command(&config, "hi");
        let line = cmd.line();
        assert!(
            line.contains("--model anthropic/claude-haiku-4-5"),
            "{line}"
        );
        assert!(line.contains("--provider anthropic"), "{line}");
        assert!(line.contains("--thinking low"), "{line}");
        assert!(line.contains("--offline -- hi"), "{line}");
        assert_eq!(cmd.timeout, Some(Duration::from_secs(9)));
    }

    #[test]
    fn generate_label_cleans_up_the_answer() {
        let runner = FakeRunner::new(|_| Ok(Output::ok("\n\"Issue #42 Fix login timeout\"\n")));
        let label = generate_label(&runner, &pi_config(), &issue()).unwrap();
        assert_eq!(label, "Issue #42 Fix login timeout");
        let prompt = runner.calls()[0].args.last().unwrap().clone();
        assert!(prompt.contains("Login times out"), "{prompt}");
    }

    #[test]
    fn generate_label_respects_the_length_limit() {
        let runner = FakeRunner::new(|_| Ok(Output::ok("abcdefghijklmnopqrstuvwxyz")));
        let config = PiConfig {
            label_max_chars: 10,
            ..pi_config()
        };
        assert_eq!(
            generate_label(&runner, &config, &issue()).unwrap(),
            "abcdefghi…"
        );
    }

    #[test]
    fn generate_branch_returns_a_slug() {
        let runner = FakeRunner::new(|_| Ok(Output::ok("`Fix-Login-Timeout`\n")));
        assert_eq!(
            generate_branch(&runner, &pi_config(), &issue()).unwrap(),
            "fix-login-timeout"
        );
        let prompt = runner.calls()[0].args.last().unwrap().clone();
        assert!(prompt.contains("branch name"), "{prompt}");
    }

    #[test]
    fn empty_answers_and_failures_are_errors() {
        let blank = FakeRunner::new(|_| Ok(Output::ok("  \n")));
        assert!(generate_branch(&blank, &pi_config(), &issue()).is_err());
        assert!(generate_label(&blank, &pi_config(), &issue()).is_err());

        let failing = FakeRunner::new(|_| Ok(Output::failed("Error: No API key\n")));
        let err = generate_label(&failing, &pi_config(), &issue()).unwrap_err();
        assert_eq!(format!("{err:#}"), "pi failed: Error: No API key");
    }

    #[test]
    fn fallback_label_uses_reference_and_title() {
        assert_eq!(fallback_label(&issue(), 40), "Issue #42 Login times out");
        assert_eq!(fallback_label(&pr(), 12), "PR #7 Login…");
    }
}
