#!/usr/bin/env bash
# Run the dashboard end to end against a throwaway Herdr session.
#
#   scripts/e2e-sandbox.sh up     build, create fixtures, start the session and
#                                 open the dashboard in it
#   scripts/e2e-sandbox.sh down   stop the session and delete the sandbox
#
# Everything lives in a named Herdr session ($E2E_SESSION, default gh-wt-e2e)
# and a sandbox directory ($E2E_DIR, default $TMPDIR/herdr-gh-wt-e2e):
#
# - a stub `gh` that serves a fixed set of assigned issues for acme/app, so
#   no GitHub account or network is involved;
# - a ghq root holding acme/app, cloned from a local bare "origin" that also
#   has a remote-only branch (epic/demo) to pick as a base. Branches made
#   outside the plugin look like the issues' own: a worktree (login-timeout)
#   to link issue 1 to with l, a branch named after issue 2 (2-dark-mode),
#   and a branch GitHub links to issue 3 (search-accents) that is pushed after
#   the clone, so the plugin has to fetch it;
# - a config file with a short pi timeout. Set HERDR_GH_WT_PI_MODEL to try
#   real names from pi; without a usable model, pi gives up after 5 seconds
#   and the fallback names are used.
#
# Your own Herdr session is never touched: every Herdr call made here goes to
# the sandbox session's socket. Pull requests are not in the fixture, because
# `wt switch pr:N` needs the real GitHub.
set -euo pipefail

session=${E2E_SESSION:-gh-wt-e2e}
dir=${E2E_DIR:-${TMPDIR:-/tmp}/herdr-gh-wt-e2e}
marker=.herdr-gh-wt-e2e
repo_root=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)

die() {
  echo "e2e-sandbox: $*" >&2
  exit 1
}

# Single-quote a word for the pane's shell (POSIX shells and fish alike).
quote() {
  [[ $1 != *"'"* ]] || die "cannot quote a path containing a single quote: $1"
  printf "'%s'" "$1"
}

session_socket() {
  herdr session list --json |
    jq -r --arg name "$session" '.sessions[] | select(.name == $name and .running) | .socket_path'
}

write_fixtures() {
  local git=(git -c user.email=e2e@example.com -c user.name=e2e -c init.defaultBranch=main)

  "${git[@]}" init --quiet --bare "$dir/origin.git"
  "${git[@]}" clone --quiet "$dir/origin.git" "$dir/seed" 2>/dev/null
  "${git[@]}" -C "$dir/seed" commit --quiet --allow-empty -m "Initial commit"
  "${git[@]}" -C "$dir/seed" push --quiet origin main
  "${git[@]}" -C "$dir/seed" switch --quiet -c epic/demo
  "${git[@]}" -C "$dir/seed" commit --quiet --allow-empty -m "Epic work"
  "${git[@]}" -C "$dir/seed" push --quiet origin epic/demo

  mkdir -p "$dir/ghq/github.com/acme" "$dir/bin"
  local app="$dir/ghq/github.com/acme/app"
  "${git[@]}" clone --quiet "$dir/origin.git" "$app"
  "${git[@]}" -C "$app" worktree add --quiet -b login-timeout "$app.login-timeout"
  "${git[@]}" -C "$app" branch 2-dark-mode

  "${git[@]}" -C "$dir/seed" switch --quiet -c search-accents main
  "${git[@]}" -C "$dir/seed" commit --quiet --allow-empty -m "Accent folding"
  "${git[@]}" -C "$dir/seed" push --quiet origin search-accents
  rm -rf "$dir/seed"

  local issues='' n title linked
  for n in 1 2 3; do
    title=$(sed -n "${n}p" <<<$'Login times out\nAdd dark mode\nSearch ignores accents')
    linked=$([[ $n == 3 ]] && echo search-accents || true)
    issues+=${issues:+,}$(jq -cn --argjson n "$n" --arg title "$title" --arg linked "$linked" '{
      number: $n, title: $title, url: "https://github.com/acme/app/issues/\($n)",
      updatedAt: "2026-01-01T00:00:00Z", bodyText: "", author: {login: "e2e"},
      repository: {nameWithOwner: "acme/app"}, labels: {nodes: []},
      linkedBranches: {nodes: [
        $linked | select(. != "") | {ref: {name: ., repository: {nameWithOwner: "acme/app"}}}
      ]},
      closedByPullRequestsReferences: {nodes: []}
    }')
  done
  printf '{"data":{"viewer":{"login":"e2e"},"reviewRequests":{"nodes":[]},"assignedIssues":{"nodes":[%s]}}}\n' \
    "$issues" >"$dir/response.json"

  cat >"$dir/bin/gh" <<EOF
#!/bin/sh
# Stub gh for the e2e sandbox.
case "\$1 \$2" in
  "api graphql") cat $(quote "$dir/response.json") ;;
  "config get") echo https ;;
  "browse "*) echo "e2e sandbox: would open \$2 in the browser" ;;
  *) echo "e2e sandbox: unsupported gh call: \$*" >&2; exit 1 ;;
esac
EOF
  chmod +x "$dir/bin/gh"

  printf '[pi]\ntimeout_secs = 5\n' >"$dir/config.toml"
}

up() {
  local cmd
  for cmd in herdr git wt jq cargo; do
    command -v "$cmd" >/dev/null || die "$cmd is not on PATH"
  done
  [[ ! -e $dir ]] || die "$dir already exists; run '$0 down' first"
  [[ -z $(session_socket) ]] || die "session $session is already running; run '$0 down' first"

  echo "Building the dashboard..."
  cargo build --quiet --manifest-path "$repo_root/Cargo.toml"
  local bin="$repo_root/target/debug/herdr-gh-wt"

  mkdir -p "$dir"
  touch "$dir/$marker"
  write_fixtures

  echo "Starting Herdr session $session..."
  # Detach the server from the Herdr session this script may run in.
  env -u HERDR_ENV -u HERDR_SOCKET_PATH -u HERDR_WORKSPACE_ID -u HERDR_TAB_ID -u HERDR_PANE_ID \
    nohup herdr --session "$session" server >"$dir/server.log" 2>&1 &
  local socket='' _
  for _ in $(seq 50); do
    socket=$(session_socket)
    [[ -n $socket ]] && break
    sleep 0.2
  done
  [[ -n $socket ]] || die "session $session did not start; see $dir/server.log"
  export HERDR_SOCKET_PATH=$socket

  local pane
  pane=$(herdr workspace create --cwd "$dir" --label e2e --focus | jq -r '.result.root_pane.pane_id')
  [[ -n $pane && $pane != null ]] || die "could not create the e2e workspace"
  herdr pane run "$pane" "env PATH=$(quote "$dir/bin"):\"\$PATH\" GHQ_ROOT=$(quote "$dir/ghq") HERDR_GH_WT_CONFIG=$(quote "$dir/config.toml") $(quote "$bin") dashboard" >/dev/null

  cat <<EOF

The sandbox is up. Talk to it with:

  export HERDR_SOCKET_PATH=$socket

  herdr pane read $pane --source visible     # see the dashboard
  herdr pane send-keys $pane enter           # open the selected issue
  herdr pane send-keys $pane b               # ... from a base branch you pick
  herdr pane send-keys $pane l               # ... on an existing branch you link
  herdr workspace list                       # workspaces the dashboard opened

Or look at it yourself: herdr session attach $session

Tear it down with: $0 down
EOF
}

down() {
  if [[ -n $(session_socket) ]]; then
    herdr session stop "$session" >/dev/null
  fi
  if herdr session list --json | jq -e --arg name "$session" '.sessions[] | select(.name == $name)' >/dev/null; then
    herdr session delete "$session" >/dev/null
  fi
  if [[ -e $dir ]]; then
    # Only ever delete a directory this script created.
    [[ -e $dir/$marker ]] || die "$dir was not created by this script; not deleting it"
    rm -rf "$dir"
  fi
  echo "The sandbox is gone."
}

case ${1:-} in
  up) up ;;
  down) down ;;
  *)
    echo "usage: $0 up|down" >&2
    exit 2
    ;;
esac
