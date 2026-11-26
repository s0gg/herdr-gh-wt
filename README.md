# herdr-gh-wt

A [Herdr](https://herdr.dev) plugin that shows a dashboard of the GitHub work
waiting for you, and opens any item as a
[worktrunk](https://github.com/max-sixty/worktrunk) worktree in its own Herdr
workspace.

- **Review requests**: open pull requests that request a review from you,
  either directly or through one of your teams.
- **Assigned issues**: open issues assigned to you.

Pick an item and press <kbd>Enter</kbd>. The plugin finds the local clone, has
worktrunk create or switch to the worktree, and opens it with
`herdr worktree open`. [pi](https://github.com/earendil-works/pi) names the
workspace.

## What happens when you open an item

1. **Find the repository.** The plugin looks up the local clone with
   `ghq list --exact <host>/<owner>/<repo>`. If there is no clone, it runs
   `ghq get` first, using SSH (`ghq get -p`) when `gh config get git_protocol`
   is `ssh`.
2. **Make sure the repository has a Herdr workspace.** If none exists, the
   plugin creates one labelled with the repository name, so the worktree
   workspace has a parent to group under.
3. **Stop if the item is already open.** If the item's branch is already open
   in Herdr, the plugin focuses that workspace (unless `focus = false`) and
   stops there.
4. **Switch to the worktree with worktrunk.** worktrunk's hooks run, and their
   output and any approval prompts show in the pane.
   - Pull requests: `wt switch pr:<number>`. worktrunk checks out the PR's
     branch, including branches from forks.
   - Issues: the first time, the plugin checks whether a branch already
     looks like the issue's own, and if one does, asks whether to use it or
     create a new branch. See
     [Branches that look like the issue's own](#branches-that-look-like-the-issues-own).
     For a new branch, pi suggests a name such as `fix-login-timeout`, and the
     plugin runs `wt switch --create <name>`. Either way, the plugin links the
     branch to the issue in the repository's git config
     (`branch.<name>.herdr-gh-wt-issue = <number>`). The next time you pick
     the same issue, it switches to the linked branch without asking. Git
     removes the link when the branch is deleted.
   - Issues opened with <kbd>b</kbd> instead of <kbd>Enter</kbd>: before the
     plugin creates the branch, it runs `git fetch --all` and asks you for the
     base (parent) branch, then runs
     `wt switch --create <name> --base <base>`. See
     [Choosing a base branch](#choosing-a-base-branch).
   - Issues opened with <kbd>l</kbd>: you pick an existing local branch, the
     plugin links it to the issue, and worktrunk switches to it. See
     [Linking an existing branch](#linking-an-existing-branch).
5. **Open the worktree in Herdr.** The plugin runs
   `herdr worktree open --label <label> --focus`. pi writes the label (for
   example `PR #101 Add dark mode toggle`). pi starts working on the label
   while worktrunk runs, so you don't wait for it afterwards.

By default, the focus moves to the item's workspace and the dashboard closes.
With `focus = false` in the `[open]` section of the config, the plugin opens
the item without moving the focus and goes back to the dashboard, so you can
open several items in a row. See [Configuration](#configuration).

If pi is unavailable or times out, the plugin falls back to `<PR|Issue> #N
<title>` for labels and `issue-<N>` for branch names, and says so in the pane.
If any step fails, the error stays on screen, and you can go back to the
dashboard.

## Requirements

- [Herdr](https://herdr.dev) ≥ 0.7.4
- [gh](https://cli.github.com), authenticated (`gh auth login`)
- [worktrunk](https://github.com/max-sixty/worktrunk) (`wt`) ≥ 0.60
- [pi](https://github.com/earendil-works/pi), with a model you can use
- [ghq](https://github.com/x-motemen/ghq)
- git
- A Rust toolchain (`cargo`) 1.88 or later to build the plugin

Platforms: Linux and macOS.

## Installation

From GitHub:

```bash
herdr plugin install s0gg/herdr-gh-wt
```

Herdr runs `cargo build --release` as part of the installation.

For local development, build the plugin and link the checkout instead:

```bash
cargo build --release
herdr plugin link "$PWD"
```

## Usage

Open the dashboard through the plugin action:

```bash
herdr plugin action invoke gh-wt.dashboard
```

To open it with a key, add a binding to `~/.config/herdr/config.toml`:

```toml
[[keys.command]]
key = "prefix+shift+i"
type = "plugin_action"
command = "gh-wt.dashboard"
description = "GitHub review requests & assigned issues"
```

Then run `herdr server reload-config`.

### Keys

| Key                         | Action                                          |
| --------------------------- | ----------------------------------------------- |
| `↑` `↓` / `k` `j`           | Move through both lists                         |
| `Tab` / `Shift+Tab`         | Switch between review requests and issues       |
| `g` / `G`                   | First / last item of the list                   |
| `Enter`                     | Open the item as a worktree workspace           |
| `b`                         | Open an issue in a new branch, starting from a base branch you choose |
| `l`                         | Link an issue to an existing branch you choose, and open it |
| `o`                         | Open the item in the browser (`gh browse`)      |
| `r`                         | Filter by repository                            |
| `/`                         | Filter by text (repository, number, title, author, labels, branch) |
| `R` / `F5`                  | Refresh                                         |
| `Esc`                       | Clear filters, or close the dashboard           |
| `q`                         | Close the dashboard                             |

### Choosing a base branch

By default, a new issue branch starts from the repository's default branch, as
with `wt switch --create`. To start it somewhere else, for example on top of an
epic branch, select the issue and press <kbd>b</kbd>.

The plugin fetches every remote, then lists the branches you can start from.
The default branch comes first, then local branches, then remote-tracking
branches that have no local copy. Within each group, the most recently
committed branches come first.

- Type to filter the list, use <kbd>↑</kbd>/<kbd>↓</kbd> to choose, and press
  <kbd>Enter</kbd> to create the branch from the selection.
- To use a base that isn't listed, type it and press <kbd>Enter</kbd> when
  nothing matches, or press <kbd>Alt</kbd>+<kbd>Enter</kbd> to use the typed
  text even when something matches. worktrunk accepts any base, including
  shortcuts such as `pr:123` and `^`.
- <kbd>Esc</kbd> cancels and takes you back to the dashboard.

If the issue already has a branch, the plugin opens that branch and says that
the base was not used.

### Branches that look like the issue's own

You may have started on an issue before opening it from the dashboard, for
example with `wt switch --create` or `gh issue develop`. So the first time you
open an issue with <kbd>Enter</kbd>, the plugin looks for branches that seem to
belong to it:

- Branches that GitHub links to the issue, in the "Development" section of the
  issue.
- Branches of open pull requests that will close the issue.
- Local branches with the issue number as a part of their name, split at `-`,
  `_` and `/`: `42-login`, `fix/issue-42` and `gh42` for issue 42, but not
  `142-login` or `v42`.

Branches linked to another issue are left out. When GitHub knows a branch that
the clone doesn't have, the plugin runs `git fetch --all` first. A branch that
is only on a remote can be offered, and worktrunk creates the local branch from
it.

If any branch qualifies, the plugin lists them, with GitHub's first and the
reason for each, followed by **Create a new branch**. Pick a branch to link it
to the issue and open it, or pick **Create a new branch** to go on as usual.
<kbd>Esc</kbd> cancels. When nothing qualifies, the plugin creates the new
branch without asking.

<kbd>b</kbd> always creates a new branch, so it doesn't look for these
branches.

### Linking an existing branch

To link any local branch to an issue, select the issue and press
<kbd>l</kbd>. Use it for a branch the plugin doesn't find by itself, or to
fix a wrong link.

The plugin lists the repository's local branches: branches with a worktree
first, then the others, each group with the most recently committed first.
Each branch shows whether it has a worktree, whether that worktree is open in
Herdr, and which issue it is linked to, if any.

- Type to filter the list, use <kbd>↑</kbd>/<kbd>↓</kbd> to choose, and press
  <kbd>Enter</kbd> to link the branch and open it.
- <kbd>Esc</kbd> cancels and takes you back to the dashboard.

An issue has one linked branch, and a branch belongs to one issue: linking
replaces the issue's previous link, and a branch linked to another issue moves
to this one. From then on, <kbd>Enter</kbd> opens the linked branch.

## Configuration

The plugin reads `config.toml` from its Herdr config directory:

```bash
config_dir=$(herdr plugin config-dir gh-wt)
${EDITOR:-vi} "$config_dir/config.toml"
```

Every key is optional:

```toml
[pi]
# Model passed to `pi --model`; leave it unset to use pi's default model.
model = "anthropic/claude-haiku-4-5"
# Passed to `pi --provider`.
# provider = "anthropic"
# Passed to `pi --thinking` (default "off"; set "" to leave it to pi).
thinking = "off"
# Seconds to wait for pi before using the fallback names (default 30).
timeout_secs = 30
# Extra arguments for every pi call.
extra_args = []
# Longest workspace label, in characters (default 40).
label_max_chars = 40
# Prompts. Placeholders: {kind} {ref} {repo} {number} {title} {labels} {body}
# label_prompt = "..."
# branch_prompt = "..."

[dashboard]
# How the dashboard opens: popup, overlay, zoomed, tab or split.
placement = "popup"
# Popup size, as terminal cells or a percentage.
width = "90%"
height = "90%"
# Maximum number of review requests and of issues to fetch (1 to 100).
limit = 100

[open]
# true: move the focus to the opened workspace and close the dashboard.
# false: open in the background and stay in the dashboard.
focus = true
```

Environment variables take precedence over the file:

| Variable                  | Overrides                                     |
| ------------------------- | --------------------------------------------- |
| `HERDR_GH_WT_PI_MODEL`    | `pi.model`                                    |
| `HERDR_GH_WT_PI_PROVIDER` | `pi.provider`                                 |
| `HERDR_GH_WT_PI_THINKING` | `pi.thinking`                                 |
| `HERDR_GH_WT_PI_TIMEOUT`  | `pi.timeout_secs`                             |
| `HERDR_GH_WT_CONFIG`      | The path of the config file itself            |

Herdr starts plugin commands from its server's environment, so set these
variables where the Herdr server starts, or use the config file.

The plugin calls pi without tools, sessions, extensions, skills or context
files, so a small, fast model is enough.

## Development

```bash
cargo test                   # unit and property-based (proptest) tests
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo deny check             # licenses, advisories, bans and sources (cargo-deny)
shellcheck scripts/*.sh
scripts/check-versions.sh    # Cargo.toml and herdr-plugin.toml versions match
```

The lints are configured in `Cargo.toml` (`[lints]`): clippy's pedantic group
is on, and unsafe code is forbidden.

CI (`.github/workflows/ci.yml`) runs these checks on every push to `main`
and on every pull request. It also runs the tests on macOS, checks that the
crate builds with the minimum supported Rust version (`rust-version` in
`Cargo.toml`), and audits the workflow itself with
[zizmor](https://github.com/zizmorcore/zizmor). When you bump the version,
change it in both `Cargo.toml` and `herdr-plugin.toml`.

### End-to-end sandbox

`scripts/e2e-sandbox.sh` runs the dashboard against a throwaway Herdr session
with a stub `gh`, so you can try the whole flow without a GitHub account and
without touching your own session:

```bash
scripts/e2e-sandbox.sh up      # prints how to read and drive the dashboard
scripts/e2e-sandbox.sh down    # stops the session and deletes the sandbox
```

The sandbox serves three assigned issues of a local `acme/app` repository.
Its origin has a remote-only `epic/demo` branch to pick as a base. Branches
made outside the plugin look like the issues' own: a `login-timeout` worktree
to link issue 1 to with <kbd>l</kbd>, a `2-dark-mode` branch named after issue
2, and a `search-accents` branch that GitHub links to issue 3, pushed after
the clone so that the plugin fetches it. Pull requests are not part of the
sandbox, because `wt switch pr:N` needs the real GitHub.

You can also run the dashboard outside a plugin pane with
`cargo run -- dashboard`, from inside Herdr.

Herdr caches the manifest when you link a plugin. After you edit
`herdr-plugin.toml`, relink the plugin:

```bash
herdr plugin unlink gh-wt && herdr plugin link "$PWD"
```

Source layout:

- `src/github.rs`: the GraphQL query for review requests and assigned issues
- `src/app.rs`: dashboard state and key handling
- `src/picker.rs`: state of the pickers for base branches and branches to link
- `src/ui.rs`: rendering, with ratatui
- `src/open.rs`: the open workflow (ghq, worktrunk, Herdr)
- `src/pi.rs`: prompts and pi calls
- `src/naming.rs`: cleans pi's answers into branch names and labels
- `src/config.rs`: `config.toml` and environment overrides
- `src/herdr.rs`: the Herdr CLI calls the plugin makes
- `src/runner.rs`: runs external commands; tests swap in a fake

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or
  <https://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or
  <https://opensource.org/licenses/MIT>)

at your option.

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in the work by you, as defined in the Apache-2.0
license, shall be dual licensed as above, without any additional terms or
conditions.
