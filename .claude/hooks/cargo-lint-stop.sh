#!/usr/bin/env bash
set -uo pipefail

REPO_ROOT=$(git rev-parse --show-toplevel 2>/dev/null || pwd)
cd "$REPO_ROOT"

# Concurrent subagent Stop events each launch this script as a separate
# process. Without serializing them, multiple `cargo clippy` runs race for
# the shared target directory. Block (not fail) on the lock so every run
# still gets checked, just one at a time.
LOCK_FILE="$REPO_ROOT/.claude/hooks/.cargo-lint-stop.lock"
exec 9>"$LOCK_FILE"
flock 9

json_escape() {
  local s=$1
  s=${s//\\/\\\\}
  s=${s//\"/\\\"}
  s=${s//$'\t'/\\t}
  s=${s//$'\r'/}
  s=${s//$'\n'/\\n}
  printf '%s' "$s"
}

# Per-file cargo fmt runs on each Edit/Write via the PostToolUse hook
# (.claude/settings.json). Running `cargo fmt --all` here reformats the
# entire repo on every Stop, which repeatedly reintroduced whitespace-only
# diffs into unrelated files during concurrent subagent runs (see
# .kiro/specs/federation-core/tasks.md Implementation Notes, task 4.1).

FAILURES=""

CLIPPY_OUTPUT=$(cargo clippy --all-targets --quiet -- -D warnings 2>&1)
CLIPPY_STATUS=$?
if [ "$CLIPPY_STATUS" -ne 0 ]; then
  TAIL=$(printf '%s\n' "$CLIPPY_OUTPUT" | tail -150)
  FAILURES="${FAILURES}cargo clippy failed (exit $CLIPPY_STATUS). Fix the lints before stopping.

$TAIL

"
fi

# Test execution is intentionally not run here: it's delegated to the
# autonomous agent runs (kiro-auto-implement / reviewer subagents), which
# run the full `cargo test` suite as part of task verification. Keeping it
# out of this hook avoids duplicating slow test runs on every Stop event.

if [ -n "$FAILURES" ]; then
  printf '{"decision":"block","reason":"%s"}\n' "$(json_escape "$FAILURES")"
else
  echo '{}'
fi
