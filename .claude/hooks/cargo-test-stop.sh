#!/usr/bin/env bash
set -uo pipefail

REPO_ROOT=$(git rev-parse --show-toplevel 2>/dev/null || pwd)
cd "$REPO_ROOT"

# Concurrent subagent Stop events each launch this script as a separate
# process. Without serializing them, multiple `cargo test` runs race for the
# shared Postgres instance's max_connections and fail with unrelated
# PoolTimedOut errors. Block (not fail) on the lock so every run still gets
# checked, just one at a time.
LOCK_FILE="$REPO_ROOT/.claude/hooks/.cargo-test-stop.lock"
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

# cargo fmt is auto-applied, never blocks the stop.
cargo fmt --all >/dev/null 2>&1

FAILURES=""

CLIPPY_OUTPUT=$(cargo clippy --all-targets --quiet -- -D warnings 2>&1)
CLIPPY_STATUS=$?
if [ "$CLIPPY_STATUS" -ne 0 ]; then
  TAIL=$(printf '%s\n' "$CLIPPY_OUTPUT" | tail -150)
  FAILURES="${FAILURES}cargo clippy failed (exit $CLIPPY_STATUS). Fix the lints before stopping.

$TAIL

"
fi

TEST_OUTPUT=$(cargo test --release --quiet 2>&1)
TEST_STATUS=$?
if [ "$TEST_STATUS" -ne 0 ]; then
  TAIL=$(printf '%s\n' "$TEST_OUTPUT" | tail -150)
  FAILURES="${FAILURES}cargo test failed (exit $TEST_STATUS). Fix the failing tests before stopping.

$TAIL

"
fi

if [ -n "$FAILURES" ]; then
  printf '{"decision":"block","reason":"%s"}\n' "$(json_escape "$FAILURES")"
else
  echo '{}'
fi
