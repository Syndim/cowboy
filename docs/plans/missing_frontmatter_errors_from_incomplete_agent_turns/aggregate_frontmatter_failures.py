#!/usr/bin/env python3
"""Aggregate + classify the sanitized genuine executor frontmatter-parse-failure
emissions into a frequency-by-shape summary.

Input:  genuine_emissions.sanitized.json — a JSON array of sanitized emission
        records, one per genuine `agent step: failed to parse frontmatter output`
        emission (matched from the diagnostic logs by exact source-location
        prefix `cowboy_workflow_agent::executor:
        crates/workflow/agent/src/executor.rs:<line>`, so occurrences of the
        phrase inside agent tool payloads are excluded). Each record carries only
        sanitized/structural fields: run handle (8 chars), step, reply length,
        whether a `---` frontmatter delimiter or a ```yaml/```yml fence was
        present, any matched generic backend notice, and a sanitized reply string.

Usage:
    python3 aggregate_frontmatter_failures.py genuine_emissions.sanitized.json \
      > missing_frontmatter_frequency.json

Classification rules (deterministic, applied in order):
  1. rendered_reply_len == 0 or sanitized reply is whitespace-only -> "empty_or_whitespace"
  2. has_frontmatter_delim is true  -> "has_opening_delim" (parser-recoverable candidate)
  3. has_yaml_fence is true         -> "yaml_code_fence"   (parser-recoverable candidate)
  4. matched_backend_notice == "anthropic_stall"      -> "backend_stall_notice"
  5. matched_backend_notice == "openai_stream_closed" -> "backend_stream_closed_notice"
  6. otherwise (nonempty, no delimiter, no notice)    -> "incomplete_preamble_or_prose"

Rules 2 and 3 are the ONLY shapes a parser-side recovery could fix; the output
reports how many records fell into them so the "zero parser-recoverable
frontmatter" claim is checkable, not asserted.
"""
import json
import sys
from collections import Counter


def classify(rec):
    if rec["rendered_reply_len"] == 0 or rec.get("rendered_reply_sanitized", "").strip() == "":
        return "empty_or_whitespace"
    if rec.get("has_frontmatter_delim"):
        return "has_opening_delim"
    if rec.get("has_yaml_fence"):
        return "yaml_code_fence"
    notice = rec.get("matched_backend_notice")
    if notice == "anthropic_stall":
        return "backend_stall_notice"
    if notice == "openai_stream_closed":
        return "backend_stream_closed_notice"
    return "incomplete_preamble_or_prose"


def main():
    records = json.load(open(sys.argv[1]))
    by_shape = Counter()
    by_run_step = Counter()
    runs = set()
    parser_recoverable = 0
    for rec in records:
        shape = classify(rec)
        by_shape[shape] += 1
        by_run_step[(rec["run"], rec["step"], shape)] += 1
        runs.add(rec["run"])
        if shape in ("has_opening_delim", "yaml_code_fence"):
            parser_recoverable += 1

    out = {
        "note": "Frequency-by-shape of genuine executor `failed to parse frontmatter output` emissions. Regenerated deterministically from genuine_emissions.sanitized.json by aggregate_frontmatter_failures.py.",
        "distinct_runs": len(runs),
        "total_genuine_parse_failures": sum(by_shape.values()),
        "parser_recoverable_frontmatter_count": parser_recoverable,
        "by_shape": dict(by_shape.most_common()),
        "by_run_step_shape": [
            {"run": r, "step": s, "shape": t, "count": c}
            for (r, s, t), c in sorted(by_run_step.items())
        ],
    }
    json.dump(out, sys.stdout, indent=2)
    sys.stdout.write("\n")


if __name__ == "__main__":
    main()
