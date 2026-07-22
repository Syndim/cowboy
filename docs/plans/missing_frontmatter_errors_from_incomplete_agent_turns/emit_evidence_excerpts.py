#!/usr/bin/env python3
"""Emit sanitized, CORRELATED log excerpts proving the missing-frontmatter root
cause, from the frozen source-log manifest.

Correlation model (defensible, code-grounded):
  In `crates/workflow/agent/src/executor.rs`, a single step turn logs, in this
  order within one process/thread and one log file:
    1. `ACP prompt turn completed ... session_id=<S> ... stop_reason=... activity=...`
       (from the ACP client, inside run_prompt_turn);
    2. `agent step: initial reply ...  session_id=<S> stop_reason=... reply_chars=<N>`
       (executor.rs:508) — SAME session_id S, immediately after the turn; or the
       `correction reply` variant on a retry;
    3. on parse failure, `agent step: failed to parse frontmatter output
       run_id=<R> step=<T> reply=<...>` (executor.rs:610) — SAME run_id/step,
       parsing exactly the reply whose length is <N>.
  So each parse-failure line is joined to the NEAREST PRECEDING executor
  reply-debug line with the same run_id+step (giving session_id S and reply_chars
  N), and that reply-debug line is joined to the NEAREST PRECEDING ACP completion
  line carrying session_id S. This is a session_id + file-adjacency join, not a
  bare same-file sample. Any parse-failure that cannot be correlated (no matching
  preceding reply-debug / ACP line) is still emitted, explicitly flagged
  `correlation=none`.

Redaction: absolute log path -> basename; run_id/session_id -> `<redacted>`; a
reply that is an exact backend notice is shown verbatim (no user content), any
other prose is truncated to 40 chars + `… <prose redacted>`.

Usage:
    python3 emit_evidence_excerpts.py \
      --manifest source_log_manifest.json --logs-dir "<state_dir>/logs" \
      > evidence_excerpts.txt

Deterministic: same manifest + same (hash-verified) logs -> identical output.
"""
import argparse
import hashlib
import json
import os
import re
import sys

FAIL = re.compile(
    r"cowboy_workflow_agent::executor: crates/workflow/agent/src/executor\.rs:\d+: "
    r"agent step: failed to parse frontmatter output "
    r"run_id=(?P<run>\S+) step=(?P<step>\S+) reply=(?P<reply>.*)$"
)
REPLY = re.compile(
    r"agent step: (?P<variant>initial|correction) reply "
    r"run_id=(?P<run>\S+) step=(?P<step>\S+) session_id=(?P<sid>\S+) "
    r"stop_reason=(?P<stop>\w+) reply_chars=(?P<chars>\d+)"
)
ACP = re.compile(
    r'ACP prompt turn completed session_id="?(?P<sid>[0-9a-f-]+)"? '
    r"id=(?P<id>\d+) stop_reason=(?P<stop>\w+) activity=(?P<act>\w+) "
    r"trailing_text=(?P<tt>\w+)"
)
ANTHROPIC_STALL = "Anthropic stream stalled while waiting for the next event"
OPENAI_STREAM_CLOSED = (
    "OpenAI responses stream closed before a terminal response event was received"
)


def redact_reply(reply):
    if ANTHROPIC_STALL in reply:
        return ANTHROPIC_STALL
    if OPENAI_STREAM_CLOSED in reply:
        return OPENAI_STREAM_CLOSED
    if reply.strip() == "":
        return "<empty>"
    return reply[:40] + "… <prose redacted>"


def sha256(path):
    with open(path, "rb") as fh:
        return hashlib.sha256(fh.read()).hexdigest()


def scan(path):
    """Return an ordered list of (kind, dict) events for the correlated types."""
    events = []
    with open(path, errors="replace") as fh:
        for line in fh:
            line = line.rstrip("\n")
            m = ACP.search(line)
            if m:
                events.append(("acp", m.groupdict()))
                continue
            m = REPLY.search(line)
            if m:
                events.append(("reply", m.groupdict()))
                continue
            m = FAIL.search(line)
            if m:
                events.append(("fail", m.groupdict()))
    return events


def correlate(events):
    """For each parse-failure, join the nearest preceding reply-debug (same
    run+step) and the nearest preceding ACP completion (same session_id)."""
    out = []
    for i, (kind, ev) in enumerate(events):
        if kind != "fail":
            continue
        reply_ev = None
        for j in range(i - 1, -1, -1):
            k, e = events[j]
            if k == "reply" and e["run"] == ev["run"] and e["step"] == ev["step"]:
                reply_ev = e
                reply_idx = j
                break
        acp_ev = None
        if reply_ev is not None:
            for j in range(reply_idx - 1, -1, -1):
                k, e = events[j]
                if k == "acp" and e["sid"] == reply_ev["sid"]:
                    acp_ev = e
                    break
        out.append((ev, reply_ev, acp_ev))
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--manifest", required=True)
    ap.add_argument("--logs-dir", required=True)
    args = ap.parse_args()
    manifest = json.load(open(args.manifest))

    print("# Sanitized CORRELATED log excerpts: missing-frontmatter root cause")
    print("# Join: parse-failure -> nearest preceding executor reply-debug")
    print("#       (same run_id+step) -> nearest preceding ACP completion (same")
    print("#       session_id). run_id/session_id redacted; reply prose redacted.")
    for entry in manifest["files"]:
        path = os.path.join(args.logs_dir, entry["basename"])
        if not os.path.isfile(path):
            sys.exit(f"manifest file missing: {entry['basename']}")
        if sha256(path) != entry["sha256"]:
            sys.exit(f"manifest hash mismatch for {entry['basename']}")

        correlated = correlate(scan(path))
        if not correlated:
            continue
        print(f"\n## {entry['basename']}  (genuine emissions: {entry['genuine_emissions']})")
        for n, (fail, reply, acp) in enumerate(correlated, 1):
            print(f"\n- correlated turn {n} (step={fail['step']}):")
            if acp is not None:
                print(
                    f"    ACP prompt turn completed session_id=<redacted> id={acp['id']} "
                    f"stop_reason={acp['stop']} activity={acp['act']} trailing_text={acp['tt']}"
                )
            else:
                print("    ACP prompt turn completed: correlation=none")
            if reply is not None:
                print(
                    f"    agent step: {reply['variant']} reply run_id=<redacted> "
                    f"step={reply['step']} session_id=<redacted> "
                    f"stop_reason={reply['stop']} reply_chars={reply['chars']}"
                )
            else:
                print("    agent step: reply-debug: correlation=none")
            print(
                f"    executor.rs:610: agent step: failed to parse frontmatter output "
                f"run_id=<redacted> step={fail['step']} reply={redact_reply(fail['reply'])}"
            )


if __name__ == "__main__":
    main()
