set -u
cd "$(git rev-parse --show-toplevel)"

# Pre-state snapshots (proves the run leaves the working tree and foreign stash intact).
status_before="$(git status --porcelain)"
stash_before="$(git stash list)"

# Scope gate: exactly the three fix files under crates/tui/app, never transcript.rs.
echo "=== tracked file scope (crates/tui/app) ==="
git status --porcelain -- crates/tui/app

# Isolated read-only baseline at committed HEAD; stable $BASE in this one shell.
# Fallback-only EXIT trap: removes the worktree only if explicit cleanup did not.
BASE="$(mktemp -d)/cowboy-baseline"
removed=0
trap '[ "$removed" = 1 ] || git worktree remove --force "$BASE" >/dev/null 2>&1 || true' EXIT
git worktree add --detach "$BASE" HEAD >/dev/null

# Same app:: suite on the working tree and on the clean baseline (one lib target
# => exactly one `test result:` summary each).
wt_out="$(mktemp)"; base_out="$(mktemp)"
cargo test -p cowboy --lib app:: >"$wt_out" 2>&1; wt_status=$?
cargo test -p cowboy --manifest-path "$BASE/Cargo.toml" --lib app:: >"$base_out" 2>&1; base_status=$?

extract() { grep -E '^test .+ \.\.\. FAILED$' "$1" | sed -E 's/^test (.+) \.\.\. FAILED$/\1/' | sort -u; }

# Suite-completion oracle: prove each run actually REACHED libtest and its
# summary/exit status is consistent with its failure set. Rejects compile /
# manifest / environment failures that never produce a `test result:` line.
suite_complete() {  # <output-file> <exit-status> <label>
    local f="$1" st="$2" label="$3" n
    n="$(grep -cE '^test result:' "$f")"
    if [ "$n" != 1 ]; then
        echo "SUITE INCOMPLETE ($label): expected exactly one 'test result:' summary, got $n (suite never reached libtest)"
        return 1
    fi
    if [ -z "$(extract "$f")" ]; then
        { grep -qE '^test result: ok\.' "$f" && [ "$st" = 0 ]; } \
            || { echo "SUITE STATUS MISMATCH ($label): empty failure set but summary/exit not clean ok/0 (exit=$st)"; return 1; }
    else
        { grep -qE '^test result: FAILED\.' "$f" && [ "$st" = 101 ]; } \
            || { echo "SUITE STATUS MISMATCH ($label): non-empty failure set but summary/exit not FAILED/101 (exit=$st)"; return 1; }
    fi
    return 0
}

# Placement oracle: the four required tests must be present and `... ok` in the
# working-tree run (a missing / ignored / filtered-out / failed one is rejected).
check_placement() {  # <working-tree-output-file>
    local f="$1" t prc=0
    for t in input_end moved_single_line_position wrapped_input_end moved_wrapped_input_position; do
        grep -qE "^test app::tests::draw_places_cursor_at_${t} \.\.\. ok\$" "$f" \
            || { echo "PLACEMENT MISSING/NOT-OK: app::tests::draw_places_cursor_at_${t}"; prc=1; }
    done
    return $prc
}

# Failing-test sets and the BIDIRECTIONAL difference (exact-equality oracle).
wt_fail="$(extract "$wt_out")"; base_fail="$(extract "$base_out")"
new_fail="$(comm -23 <(printf '%s\n' "$wt_fail") <(printf '%s\n' "$base_fail") | sed '/^$/d')"   # in working tree, not baseline
gone_fail="$(comm -13 <(printf '%s\n' "$wt_fail") <(printf '%s\n' "$base_fail") | sed '/^$/d')"  # in baseline, not working tree

# EXPLICIT verdict-affecting cleanup BEFORE printing the result.
cleanup_rc=0
git worktree remove --force "$BASE" >/dev/null 2>&1 && removed=1 || cleanup_rc=1
git worktree list | grep -qF "$BASE" && cleanup_rc=1   # $BASE must be absent afterward
status_after="$(git status --porcelain)"
stash_after="$(git stash list)"

echo "=== working-tree failures ==="; printf '%s\n' "$wt_fail"
echo "=== baseline failures ===";     printf '%s\n' "$base_fail"
echo "=== working-only failures (regressions) ==="; printf '%s\n' "$new_fail"
echo "=== baseline-only failures (disappeared) ==="; printf '%s\n' "$gone_fail"
echo "=== required placement tests (working tree) ==="
grep -E '^test app::tests::draw_places_cursor_at_.+ \.\.\. (ok|FAILED|ignored)$' "$wt_out" || echo "(none printed)"
echo "wt_status=$wt_status base_status=$base_status cleanup_rc=$cleanup_rc"

rc=0
suite_complete "$wt_out"   "$wt_status"   working-tree || rc=1
suite_complete "$base_out" "$base_status" baseline     || rc=1
check_placement "$wt_out" || rc=1
[ -n "$new_fail" ]  && { echo "REGRESSION: new app:: failure(s) introduced"; rc=1; }
[ -n "$gone_fail" ] && { echo "SET MISMATCH: baseline failure(s) absent from working tree"; rc=1; }
[ "$cleanup_rc" != 0 ]                  && { echo "CLEANUP FAILED: baseline worktree not removed"; rc=1; }
[ "$status_before" != "$status_after" ] && { echo "WORKTREE STATE CHANGED"; rc=1; }
[ "$stash_before"  != "$stash_after"  ] && { echo "STASH MUTATED"; rc=1; }
[ "$rc" = 0 ] && echo "PASS: both suites completed; failure sets identical; four placement tests ok; cleanup verified; stash unchanged"
exit $rc
