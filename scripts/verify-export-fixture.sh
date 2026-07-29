#!/usr/bin/env bash
set -euo pipefail

ROOT="$(git rev-parse --show-toplevel)"
test "$ROOT" = "$(pwd)"
SMOKE="$ROOT/target/export-smoke-review"
test "$SMOKE" = "$ROOT/target/export-smoke-review"
rm -rf -- "$SMOKE"
mkdir -p "$SMOKE/workflows" "$SMOKE/state/events"

cat >"$SMOKE/workflows/export_smoke.lua" <<'LUA'
local start = step("start")
start.run = function(_ctx)
  return action.status { status = "success", body = "seed complete" }
end
return workflow("export-smoke", start)
LUA

cat >"$SMOKE/config.toml" <<EOF
state_dir = "$SMOKE/state"
workflow_store = "$SMOKE/state/data.db"
workflow_dirs = ["$SMOKE/workflows"]

[config_sets.default]
max_steps_per_run = 5
max_visits_per_step = 5
max_retries_per_run = 0
max_retries_per_step = 0

[[agents]]
name = "default"
command = "unused-agent"
args = []
EOF

cargo build -p cowboy
RUN_OUTPUT="$("$ROOT/target/debug/cowboy" --config "$SMOKE/config.toml" run --workflow export_smoke "export smoke request")"
RUN_ID="$(printf '%s\n' "$RUN_OUTPUT" | sed -n 's/^run=\([^ ]*\).*/\1/p' | head -n 1)"
test -n "$RUN_ID"

cat >"$SMOKE/state/events/$RUN_ID.json" <<EOF
[
  {
    "run_id": "$RUN_ID",
    "timestamp": "2026-01-02T03:04:05Z",
    "run_started_at": "2026-01-02T03:04:05Z",
    "kind": {
      "kind": "run_started",
      "workflow_name": "export-smoke",
      "current_step": "start",
      "request_topic": "Export smoke"
    }
  },
  {
    "run_id": "$RUN_ID",
    "timestamp": "2026-01-02T03:04:06Z",
    "run_started_at": "2026-01-02T03:04:05Z",
    "kind": {
      "kind": "agent_response",
      "step_id": "start",
      "content": "first response line\n"
    }
  },
  {
    "run_id": "$RUN_ID",
    "timestamp": "2026-01-02T03:04:07Z",
    "run_started_at": "2026-01-02T03:04:05Z",
    "kind": {
      "kind": "agent_response",
      "step_id": "start",
      "content": "second response line with BODY_ONLY_SEARCH_TOKEN"
    }
  },
  {
    "run_id": "$RUN_ID",
    "timestamp": "2026-01-02T03:04:08Z",
    "run_started_at": "2026-01-02T03:04:05Z",
    "kind": {
      "kind": "agent_tool_call",
      "step_id": "start",
      "tool_call_id": "tool-1",
      "title": "Inspect fixture",
      "tool_kind": "read",
      "status": "running"
    }
  },
  {
    "run_id": "$RUN_ID",
    "timestamp": "2026-01-02T03:04:09Z",
    "run_started_at": "2026-01-02T03:04:05Z",
    "kind": {
      "kind": "agent_tool_call_update",
      "step_id": "start",
      "tool_call_id": "tool-1",
      "title": "Inspect fixture",
      "status": "completed",
      "content": {
        "output": "TOOL_UPDATE_SEARCH_TOKEN"
      }
    }
  },
  {
    "run_id": "$RUN_ID",
    "timestamp": "2026-01-02T03:04:10Z",
    "run_started_at": "2026-01-02T03:04:05Z",
    "kind": {
      "kind": "run_completed"
    }
  }
]
EOF

EXPORT_OUTPUT="$(cd "$SMOKE" && "$ROOT/target/debug/cowboy" --config "$SMOKE/config.toml" export "$RUN_ID")"
HTML_PATH="$(printf '%s\n' "$EXPORT_OUTPUT" | sed -n 's/.*path=\(.*\)$/\1/p' | tail -n 1)"
test -f "$HTML_PATH"
test "$(dirname "$HTML_PATH")" = "$SMOKE"

printf 'RUN_ID=%q\nHTML_PATH=%q\nSMOKE=%q\n' "$RUN_ID" "$HTML_PATH" "$SMOKE" >"$SMOKE/result.env"
printf 'EXPORT_FIXTURE_OK run_id=%s html_path=%s\n' "$RUN_ID" "$HTML_PATH"
