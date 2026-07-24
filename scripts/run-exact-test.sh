#!/usr/bin/env bash
set -euo pipefail

if [ "$#" -ne 3 ]; then
  echo "usage: $0 <package> <fully-qualified-library-test> <evidence-marker>" >&2
  exit 2
fi

package="$1"
test_name="$2"
marker="$3"

if [ -z "$package" ] || [ -z "$test_name" ] || [ -z "$marker" ]; then
  echo "package, test name, and evidence marker must be non-empty" >&2
  exit 2
fi

if ! list_output="$(cargo test -p "$package" --lib -- "$test_name" --exact --list 2>&1)"; then
  printf '%s\n' "$list_output" >&2
  exit 1
fi
printf '%s\n' "$list_output"

listed_count="$(
  printf '%s\n' "$list_output" |
    awk -v expected="$test_name: test" '$0 == expected { count++ } END { print count + 0 }'
)"
if [ "$listed_count" -ne 1 ]; then
  echo "expected exactly one listed test named '$test_name', found $listed_count" >&2
  exit 1
fi

if ! test_output="$(cargo test -p "$package" --lib -- "$test_name" --exact --nocapture 2>&1)"; then
  printf '%s\n' "$test_output" >&2
  exit 1
fi
printf '%s\n' "$test_output"

result_count="$(
  printf '%s\n' "$test_output" |
    awk '
      /^test result:/ {
        summaries++
        if ($0 ~ /^test result: ok\. 1 passed; 0 failed; 0 ignored;/) {
          matching++
        }
      }
      END {
        if (summaries == 1 && matching == 1) print 1
        else print 0
      }
    '
)"
if [ "$result_count" -ne 1 ]; then
  echo "test result did not prove exactly 1 passed, 0 failed, and 0 ignored" >&2
  exit 1
fi

marker_count="$(
  printf '%s\n' "$test_output" |
    awk -v marker="$marker" '$0 == marker { count++ } END { print count + 0 }'
)"
if [ "$marker_count" -lt 1 ]; then
  echo "required evidence marker was not emitted: $marker" >&2
  exit 1
fi

printf 'EXACT_TEST_OK %s %s\n' "$package" "$test_name"
