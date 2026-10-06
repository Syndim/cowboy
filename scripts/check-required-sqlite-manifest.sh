#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
manifest="$script_dir/required-sqlite-tests.tsv"
runner="$script_dir/run-exact-test.sh"
package=cowboy-workflow-store

# These contract rows must stay present, exact, and aligned with tests that
# inspect the live SQLite file. A stale marker cannot pass merely by changing
# the required-test inventory without updating the schema contract.
while IFS=$'\t' read -r test_name marker; do
  recorded="$(awk -F '\t' -v package="$package" -v test_name="$test_name" '
    $1 == package && $2 == test_name { count++; marker = $3 }
    END { if (count == 1) print marker; else exit 1 }
  ' "$manifest")" || {
    echo "required SQLite schema test missing or duplicated: $test_name" >&2
    exit 1
  }
  if [ "$recorded" != "$marker" ]; then
    echo "required SQLite schema marker drift: $test_name: expected '$marker', found '$recorded'" >&2
    exit 1
  fi
  "$runner" "$package" "$test_name" "$marker"
done <<'SCHEMA_TESTS'
schema::tests::initializes_new_and_empty_files	EVIDENCE schema-init header=SQLite_format_3 user_version=2 wal=true foreign_keys=true max_connections=4
schema::tests::reopens_supported_schema_version	EVIDENCE schema-reopen user_version=2 tables=9
schema::tests::concurrent_first_connects_initialize_one_schema	EVIDENCE schema-concurrent connects=2 user_version=2 tables=9
schema::tests::upgrades_existing_v1_store_without_reclassifying_runs	EVIDENCE schema-upgrade from=1 to=2 legacy_run_preserved=true native_enrollment=empty
SCHEMA_TESTS

# Exercise the real exact-test runner, not a mock or a source-text substitute:
# stale versions/counts, a nonexistent test, a missing marker, and an ignored
# test must each fail even when cargo itself exits successfully.
expect_rejection() {
  local reason="$1" package="$2" name="$3" marker="$4" output
  if output="$("$runner" "$package" "$name" "$marker" 2>&1)"; then
    echo "exact-test runner accepted $reason" >&2
    exit 1
  fi
  if [[ "$output" != *"$reason"* ]]; then
    printf 'exact-test runner rejected for wrong reason (expected %s):\n%s\n' "$reason" "$output" >&2
    exit 1
  fi
}

expect_rejection 'required evidence marker was not emitted' "$package" \
  schema::tests::initializes_new_and_empty_files \
  'EVIDENCE schema-init header=SQLite_format_3 user_version=1 wal=true foreign_keys=true max_connections=4'
expect_rejection 'required evidence marker was not emitted' "$package" \
  schema::tests::reopens_supported_schema_version \
  'EVIDENCE schema-reopen user_version=2 tables=7'
expect_rejection 'expected exactly one listed test' "$package" \
  schema::tests::missing_exact_schema_test \
  'EVIDENCE schema-missing'
expect_rejection 'required evidence marker was not emitted' "$package" \
  schema::tests::reopens_supported_schema_version \
  'EVIDENCE schema-marker-never-emitted'
expect_rejection 'test result did not prove exactly 1 passed, 0 failed, and 0 ignored' cowboy \
  app::history::tests::hold_history_lock_helper \
  'EVIDENCE ignored-test-must-not-pass'

printf 'SQLITE_MANIFEST_REGRESSION_OK positive=4 negative=5\n'
