#!/usr/bin/env bash
set -euo pipefail

tmp_root="$(mktemp -d "$PWD/.verify-sqlite-store.XXXXXX")"
pid_a=""
pid_b=""
lock_holder_pid=""
cleanup() {
  for pid in "$pid_a" "$pid_b" "$lock_holder_pid"; do
    if [ -n "$pid" ] && ps -p "$pid" >/dev/null 2>&1; then
      kill "$pid"
      wait "$pid" 2>/dev/null || true
    fi
  done
  if [ -d "$tmp_root" ]; then
    find "$tmp_root" -depth -delete
  fi
}
trap cleanup EXIT

just test-apps
store_cli="$PWD/target/debug/test-apps/store-cli"
engine_cli="$PWD/target/debug/test-apps/engine-cli"
test -x "$store_cli"
test -x "$engine_cli"

db="$tmp_root/store/data.db"
mkdir -p "$(dirname "$db")"
"$store_cli" "$db" save-run run-smoke workflow source-hash start \
  >"$tmp_root/save-run.out"
rg -q '^saved run run-smoke$' "$tmp_root/save-run.out"
"$store_cli" "$db" load-run run-smoke >"$tmp_root/load-run.json"
rg -q '"id": "run-smoke"' "$tmp_root/load-run.json"
"$store_cli" "$db" list-runs >"$tmp_root/list-runs.json"
rg -q '"run_id": "run-smoke"' "$tmp_root/list-runs.json"
step_hash="$("$store_cli" "$db" put-step record-1 start success)"
test -n "$step_hash"
"$store_cli" "$db" get-step "$step_hash" >"$tmp_root/get-step.json"
rg -q '"id": "record-1"' "$tmp_root/get-step.json"
turn_hash="$("$store_cli" "$db" append-turn run-smoke record-1 turn-1 hello)"
test -n "$turn_hash"
python3 - "$db" <<'PY'
import pathlib
import sys
assert pathlib.Path(sys.argv[1]).read_bytes()[:16] == b"SQLite format 3\x00"
PY
"$store_cli" "$db" delete-run run-smoke >"$tmp_root/delete-run.out"
if "$store_cli" "$db" load-run run-smoke >"$tmp_root/deleted-run.out" 2>&1; then
  echo "deleted run remained loadable" >&2
  exit 1
fi
"$store_cli" "$db" get-step "$step_hash" >"$tmp_root/get-step-after-delete.json"
rg -q '"id": "record-1"' "$tmp_root/get-step-after-delete.json"

overlap_workflows="$tmp_root/overlap-workflows"
barrier_dir="$tmp_root/overlap-barrier"
barrier_script="$tmp_root/overlap-barrier.sh"
mkdir -p "$overlap_workflows" "$barrier_dir"
cat >"$barrier_script" <<'SH'
#!/bin/sh
set -eu
barrier_dir="$1"
run_id="$2"
: >"$barrier_dir/$run_id.ready"
while [ ! -f "$barrier_dir/release" ]; do
  sleep 0.02
done
SH
chmod +x "$barrier_script"
cat >"$overlap_workflows/00-overlap.lua" <<LUA
local hold = step("hold")
hold.run = function(ctx)
  return action.command {
    program = "$barrier_script",
    args = { tostring(ctx.request), tostring(ctx.run_id) },
    success_status = "success",
    failure_status = "failed",
    timeout_ms = 15000,
  }
end
return workflow("00-overlap", hold)
LUA

engine_state="$tmp_root/engine-state"
common_env=(
  env
  COWBOY_ENGINE_STATE="$engine_state"
  COWBOY_ENGINE_WORKFLOWS="$overlap_workflows"
  COWBOY_ENGINE_SELECTOR=deterministic
)
"${common_env[@]}" "$engine_cli" run "$barrier_dir" \
  >"$tmp_root/parallel-a.out" 2>&1 &
pid_a=$!
"${common_env[@]}" "$engine_cli" run "$barrier_dir" \
  >"$tmp_root/parallel-b.out" 2>&1 &
pid_b=$!

deadline=$((SECONDS + 15))
while [ "$(find "$barrier_dir" -maxdepth 1 -type f -name 'run-*.ready' | wc -l)" -ne 2 ]; do
  ps -p "$pid_a" >/dev/null
  ps -p "$pid_b" >/dev/null
  if [ "$SECONDS" -ge "$deadline" ]; then
    echo "engine-cli overlap barrier was not reached by both processes" >&2
    exit 1
  fi
  sleep 0.05
done
ps -p "$pid_a" >/dev/null
ps -p "$pid_b" >/dev/null
mapfile -t overlap_runs < <(
  find "$barrier_dir" -maxdepth 1 -type f -name 'run-*.ready' -printf '%f\n' \
    | sed 's/\.ready$//' | sort
)
test "${#overlap_runs[@]}" -eq 2
test "${overlap_runs[0]}" != "${overlap_runs[1]}"
"${common_env[@]}" "$engine_cli" runs >"$tmp_root/overlap-runs.out" 2>&1
rg -q "${overlap_runs[0]}" "$tmp_root/overlap-runs.out"
rg -q "${overlap_runs[1]}" "$tmp_root/overlap-runs.out"
ps -p "$pid_a" >/dev/null
ps -p "$pid_b" >/dev/null
touch "$barrier_dir/release"
wait "$pid_a"
wait "$pid_b"
pid_a=""
pid_b=""
rg -q "run=${overlap_runs[0]} .* status=Completed" \
  "$tmp_root/parallel-a.out" "$tmp_root/parallel-b.out"
rg -q "run=${overlap_runs[1]} .* status=Completed" \
  "$tmp_root/parallel-a.out" "$tmp_root/parallel-b.out"
if rg -qi 'database already open|cannot acquire lock' \
  "$tmp_root/parallel-a.out" "$tmp_root/parallel-b.out"; then
  echo "independent runs hit an exclusive database-open failure" >&2
  exit 1
fi
python3 - "$engine_state/data.db" <<'PY'
import sqlite3
import sys

with sqlite3.connect(sys.argv[1]) as connection:
    result = connection.execute("PRAGMA integrity_check").fetchone()
assert result == ("ok",), result
PY

locked_run="${overlap_runs[0]}"
lock_path="$engine_state/data.db.locks/$locked_run.lock"
lock_ready="$tmp_root/cross-process-lock.ready"
lock_release="$tmp_root/cross-process-lock.release"
python3 - "$lock_path" "$lock_ready" "$lock_release" <<'PY' &
import fcntl
import pathlib
import sys
import time

lock_path = pathlib.Path(sys.argv[1])
ready_path = pathlib.Path(sys.argv[2])
release_path = pathlib.Path(sys.argv[3])
lock_path.parent.mkdir(parents=True, exist_ok=True)
with lock_path.open("a+b") as handle:
    fcntl.flock(handle.fileno(), fcntl.LOCK_EX)
    ready_path.write_text("ready", encoding="utf-8")
    while not release_path.exists():
        time.sleep(0.02)
PY
lock_holder_pid=$!
deadline=$((SECONDS + 10))
while [ ! -f "$lock_ready" ]; do
  ps -p "$lock_holder_pid" >/dev/null
  if [ "$SECONDS" -ge "$deadline" ]; then
    echo "cross-process sidecar lock holder did not become ready" >&2
    exit 1
  fi
  sleep 0.05
done
if "${common_env[@]}" "$engine_cli" step "$locked_run" \
  >"$tmp_root/cross-process-lock.out" 2>&1; then
  echo "second process advanced a run whose sidecar lock was held" >&2
  exit 1
fi
rg -q "run $locked_run is already active in another Cowboy instance" \
  "$tmp_root/cross-process-lock.out"
touch "$lock_release"
wait "$lock_holder_pid"
lock_holder_pid=""

bash scripts/run-required-sqlite-tests.sh

legacy_file="$tmp_root/legacy.redb"
config_file="$tmp_root/legacy-config.toml"
printf 'legacy-redb-placeholder\n' >"$legacy_file"
before_hash="$(sha256sum "$legacy_file" | cut -d' ' -f1)"
printf 'state_dir = "%s"\nworkflow_store = "%s"\n' \
  "$tmp_root/legacy-state" "$legacy_file" >"$config_file"
if cargo run -p cowboy --quiet -- --config "$config_file" runs \
  >"$tmp_root/legacy-open.out" 2>&1; then
  echo "non-SQLite store unexpectedly opened" >&2
  exit 1
fi
rg -qi 'not a SQLite database|non-SQLite|SQLite store path' \
  "$tmp_root/legacy-open.out"
after_hash="$(sha256sum "$legacy_file" | cut -d' ' -f1)"
test "$before_hash" = "$after_hash"

cargo test -p cowboy-workflow-core
cargo test -p cowboy-workflow-store
cargo test -p cowboy-workflow-agent
cargo test -p cowboy-workflow-actions
cargo test -p cowboy-workflow-engine
cargo test -p cowboy
cargo clippy -p cowboy-workflow-core -p cowboy-workflow-store \
  -p cowboy-workflow-agent -p cowboy-workflow-actions \
  -p cowboy-workflow-engine -p cowboy --all-targets -- -D warnings
