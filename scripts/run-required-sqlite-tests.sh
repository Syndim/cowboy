#!/usr/bin/env bash
set -euo pipefail

script_dir="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
manifest="$script_dir/required-sqlite-tests.tsv"

if [ ! -f "$manifest" ]; then
  echo "required SQLite test manifest not found: $manifest" >&2
  exit 1
fi

if ! awk -F '\t' '
  NF != 3 || $1 == "" || $2 == "" || $3 == "" {
    printf "malformed manifest row %d\n", NR > "/dev/stderr"
    exit 1
  }
  {
    key = $1 "\t" $2
    if (seen[key]++) {
      printf "duplicate manifest test at row %d: %s %s\n", NR, $1, $2 > "/dev/stderr"
      exit 1
    }
  }
' "$manifest"; then
  exit 1
fi

while IFS=$'\t' read -r package test_name marker; do
  "$script_dir/run-exact-test.sh" "$package" "$test_name" "$marker"
done <"$manifest"
