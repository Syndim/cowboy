#!/usr/bin/env python3
"""Extract sanitized genuine executor frontmatter-parse-failure emissions into
`genuine_emissions.sanitized.json`, from a FROZEN source-log manifest.

This is the log -> sanitized-fixture transform. To keep the output reproducible
even though `<state_dir>/logs` is a live, growing directory, this script does NOT
glob the directory. Instead it consumes `source_log_manifest.json` (a frozen,
ordered list of the exact log basenames + SHA-256 that were used for the
investigation), verifies each file's SHA-256 before reading, and processes the
files in manifest order. If any listed file is missing or its hash does not match,
the script exits non-zero rather than silently drifting.

It matches ONLY genuine executor emissions by exact source-location prefix,
computes every classification field deterministically from the TRACING-RENDERED
`reply=` field (nothing is investigator-asserted), and redacts the reply prose.

IMPORTANT provenance note: the `reply=` value is the tracing-rendered form of the
agent reply, not the original raw bytes. `tracing` renders the string field inline
on one line, so embedded newlines appear as the literal two-character escape
"\\n". All parsing below operates on this rendered field, and lengths reported are
rendered-field lengths, not original byte lengths.

Usage:
    python3 extract_genuine_emissions.py \
      --manifest source_log_manifest.json \
      --logs-dir "<state_dir>/logs" \
      > genuine_emissions.sanitized.json

`--logs-dir` supplies the real directory the manifest basenames resolve against
(kept out of the committed artifacts so no absolute path is stored). The manifest
itself records only generalized basenames and hashes.

Matching rule (genuine emissions only): a log line containing the exact prefix
    cowboy_workflow_agent::executor: crates/workflow/agent/src/executor.rs:<line>: \
    agent step: failed to parse frontmatter output run_id=<id> step=<step> reply=<reply>
Occurrences of the phrase inside agent tool payloads (which carry a different
module/target prefix, e.g. cowboy_agent_acp::messages) do NOT match.

Computed fields (from the tracing-rendered `reply=` field):
  - rendered_reply_len:    character length of the tracing-rendered reply field.
  - has_frontmatter_delim: rendered reply has a `---` line delimiter, i.e. `---`
    at the start or preceded by a rendered newline escape (`\\n---`).
  - has_yaml_fence:        rendered reply contains a ```yaml or ```yml fence.
  - matched_backend_notice: exact-literal match of a known generic backend notice
    ("anthropic_stall" / "openai_stream_closed"), else null. Exact-literal
    presence only; it attributes the LITERAL, not provider origin.

Redaction rules (rendered_reply_sanitized):
  - empty/whitespace rendered reply -> "".
  - exact backend notice            -> the generic notice string (no user content).
  - otherwise (nonempty prose/preamble with no delimiter and no known notice)
    -> "<sanitized nonempty reply: no frontmatter delimiter, no known backend
       notice>" (the proprietary reply prose is dropped; only its structural
       signature is retained).

Run handles are reduced to the first 8 hex chars; no full run UUID is emitted.
"""
import argparse
import hashlib
import json
import os
import re
import sys

PREFIX = re.compile(
    r"cowboy_workflow_agent::executor: crates/workflow/agent/src/executor\.rs:\d+: "
    r"agent step: failed to parse frontmatter output "
    r"run_id=(\S+) step=(\S+) reply=(.*)$"
)
ANTHROPIC_STALL = "Anthropic stream stalled while waiting for the next event"
OPENAI_STREAM_CLOSED = (
    "OpenAI responses stream closed before a terminal response event was received"
)


def matched_notice(rendered_reply):
    if ANTHROPIC_STALL in rendered_reply:
        return "anthropic_stall"
    if OPENAI_STREAM_CLOSED in rendered_reply:
        return "openai_stream_closed"
    return None


def has_delim(rendered_reply):
    # The rendered reply shows newlines as the literal escape "\n"; a `---`
    # delimiter is therefore either at the very start or preceded by that escape.
    return (
        bool(re.search(r"(^|\\n)---(\\n|$)", rendered_reply))
        or rendered_reply.strip() == "---"
    )


def sanitized_reply(rendered_reply, notice):
    if rendered_reply.strip() == "":
        return ""
    if notice == "anthropic_stall":
        return ANTHROPIC_STALL
    if notice == "openai_stream_closed":
        return OPENAI_STREAM_CLOSED
    return (
        "<sanitized nonempty reply: no frontmatter delimiter, no known backend notice>"
    )


def sha256(path):
    with open(path, "rb") as fh:
        return hashlib.sha256(fh.read()).hexdigest()


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--manifest", required=True)
    ap.add_argument("--logs-dir", required=True)
    args = ap.parse_args()

    manifest = json.load(open(args.manifest))
    records = []
    for entry in manifest["files"]:
        path = os.path.join(args.logs_dir, entry["basename"])
        if not os.path.isfile(path):
            sys.exit(f"manifest file missing: {entry['basename']}")
        actual = sha256(path)
        if actual != entry["sha256"]:
            sys.exit(
                f"manifest hash mismatch for {entry['basename']}: "
                f"expected {entry['sha256']}, got {actual}"
            )
        with open(path, errors="replace") as fh:
            for line in fh:
                m = PREFIX.search(line.rstrip("\n"))
                if not m:
                    continue
                run, step, rendered_reply = m.groups()
                notice = matched_notice(rendered_reply)
                records.append(
                    {
                        "run": run.removeprefix("run-")[:8],
                        "step": step,
                        "rendered_reply_len": len(rendered_reply),
                        "has_frontmatter_delim": has_delim(rendered_reply),
                        "has_yaml_fence": (
                            "```yaml" in rendered_reply or "```yml" in rendered_reply
                        ),
                        "matched_backend_notice": notice,
                        "rendered_reply_sanitized": sanitized_reply(
                            rendered_reply, notice
                        ),
                    }
                )

    records.sort(
        key=lambda r: (
            r["run"],
            r["step"],
            r["rendered_reply_sanitized"],
            r["rendered_reply_len"],
        )
    )
    json.dump(records, sys.stdout, indent=2)
    sys.stdout.write("\n")


if __name__ == "__main__":
    main()
