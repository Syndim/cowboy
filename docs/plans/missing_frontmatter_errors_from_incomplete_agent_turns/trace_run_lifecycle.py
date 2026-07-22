#!/usr/bin/env python3
"""Emit an EVIDENCE-PRESERVING step lifecycle for one run from the frozen
source-log manifest, so the worked-example retry accounting is reproducible.

Design goals (no synthesis):
  * Every emitted line carries the generalized source basename and 1-based line
    number, plus the sanitized fields actually parsed from that log line.
  * ALL `agent step: starting <step>` dispatches are emitted (every step, not
    just the target), so intervening different-step visits are visible.
  * A same-step re-dispatch is labeled `retry` ONLY when the immediately preceding
    lifecycle event for the run was a parse failure of that SAME step (no
    different-step start intervened). Otherwise it is a `visit-start`. This mirrors
    the runner: `retry_step` re-dispatches the current step after a recoverable
    failure and increments `step_retries_used` (runner.rs:221-231); a new visit
    happens only after the workflow routed back via a different step.
  * Runtime run-error / loaded-run / resume lines are PARSED for their real fields
    (error reason, exhausted step, counters, status, current_step). Nothing is
    hard-coded; if a field is absent the script prints `<unparsed>` for it.

Redaction: log path -> basename; run_id/session_id -> `<redacted>`; a reply that
is an exact backend notice is shown verbatim, other prose truncated to 42 chars.

Usage:
    python3 trace_run_lifecycle.py \
      --manifest source_log_manifest.json --logs-dir "<state_dir>/logs" \
      --run <8hex-handle> \
      > run_lifecycle.txt
"""
import argparse
import hashlib
import json
import os
import re
import sys

ANTHROPIC_STALL = "Anthropic stream stalled while waiting for the next event"
OPENAI_STREAM_CLOSED = (
    "OpenAI responses stream closed before a terminal response event was received"
)


def reply_shape(reply):
    if ANTHROPIC_STALL in reply:
        return ANTHROPIC_STALL, "stall-notice"
    if OPENAI_STREAM_CLOSED in reply:
        return OPENAI_STREAM_CLOSED, "stream-close-notice"
    if reply.strip() == "":
        return "<empty>", "empty"
    return reply[:42] + "… <prose redacted>", "prose/preamble (ambiguous)"


def sha256(path):
    with open(path, "rb") as fh:
        return hashlib.sha256(fh.read()).hexdigest()


def ts(line):
    return line[:24]


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--manifest", required=True)
    ap.add_argument("--logs-dir", required=True)
    ap.add_argument("--run", required=True, help="8-hex run handle, e.g. ca3f4e0a")
    args = ap.parse_args()
    manifest = json.load(open(args.manifest))
    run = args.run

    start_re = re.compile(rf"agent step: starting run_id=run-{run}\S* step=(?P<step>\S+)")
    reply_re = re.compile(
        rf"agent step: (?P<variant>initial|correction) reply run_id=run-{run}\S* "
        rf"step=(?P<step>\S+) session_id=\S+ stop_reason=(?P<stop>\w+) "
        rf"reply_chars=(?P<chars>\d+)"
    )
    fail_re = re.compile(
        rf"executor\.rs:610: agent step: failed to parse frontmatter output "
        rf"run_id=run-{run}\S* step=(?P<step>\S+) reply=(?P<reply>.*)$"
    )
    runerr_re = re.compile(
        rf"workflow events collected before run error run_id=run-{run}\S*"
        rf"(?:\s+event_count=(?P<ec>\d+))?[^\n]*?error=(?P<error>.*)$"
    )
    loaded_re = re.compile(
        rf"loaded workflow run run_id=run-{run}\S* status=(?P<status>\w+)"
        rf"[^\n]*?current_step=(?P<cur>\S+)(?:\s+steps_executed=(?P<se>\d+))?"
    )
    resume_re = re.compile(
        rf"resuming non-terminal run; re-executing the retained current step "
        rf"run_id=run-{run}\S* status=(?P<status>\w+)[^\n]*?current_step=(?P<cur>\S+)"
    )
    exhaust_reason_re = re.compile(
        r"exhausted retry budget for step \\?\"?(?P<step>[a-z_]+)\\?\"?: "
        r"(?P<used>\d+)/(?P<max>\d+) retries used"
    )

    # 1) Collect ordered events with provenance (basename, line number).
    events = []
    for entry in manifest["files"]:
        path = os.path.join(args.logs_dir, entry["basename"])
        if not os.path.isfile(path):
            sys.exit(f"manifest file missing: {entry['basename']}")
        if sha256(path) != entry["sha256"]:
            sys.exit(f"manifest hash mismatch for {entry['basename']}")
        base = entry["basename"]
        with open(path, errors="replace") as fh:
            for lineno, line in enumerate(fh, 1):
                line = line.rstrip("\n")
                for kind, rx in (
                    ("start", start_re),
                    ("reply", reply_re),
                    ("fail", fail_re),
                    ("runerr", runerr_re),
                    ("loaded", loaded_re),
                    ("resume", resume_re),
                ):
                    m = rx.search(line)
                    if m:
                        events.append((kind, base, lineno, ts(line), m.groupdict()))
                        break

    # 2) Emit with retry classification derived from adjacency of same-step events.
    print(f"# Evidence-preserving step lifecycle for run <redacted> ({run})")
    print("# Every row: [ts] SRC=<basename>:<lineno> — parsed sanitized fields.")
    print("# StepRetrying records live in the mutable events/<run>.json, not these")
    print("# frozen tracing logs; retry consumption is derived from a same-step")
    print("# re-dispatch immediately after a same-step parse failure (runner.rs:221-231).")

    prev_kind = None
    prev_step = None
    step_retries = {}  # step -> durable retry count within the current visit chain
    visit_no = {}      # step -> visit counter

    for kind, base, lineno, t, g in events:
        src = f"{base}:{lineno}"
        if kind == "start":
            step = g["step"]
            is_retry = prev_kind == "fail" and prev_step == step
            if is_retry:
                step_retries[step] = step_retries.get(step, 0) + 1
                label = (
                    f"retry (step={step}, step_retries_used={step_retries[step]}, "
                    f"proof: immediately follows a same-step parse failure with no "
                    f"intervening different-step start)"
                )
            else:
                visit_no[step] = visit_no.get(step, 0) + 1
                # A brand-new visit chain resets the per-visit-chain retry count.
                step_retries[step] = 0
                label = f"visit-start (step={step}, visit {visit_no[step]})"
            print(f"\n[{t}] SRC={src} START: {label}")
            prev_kind, prev_step = "start", step
        elif kind == "reply":
            print(
                f"[{t}] SRC={src} reply: step={g['step']} {g['variant']} "
                f"stop_reason={g['stop']} reply_chars={g['chars']}"
            )
            prev_kind, prev_step = "reply", g["step"]
        elif kind == "fail":
            shown, shape = reply_shape(g["reply"])
            print(
                f"[{t}] SRC={src} PARSE-FAIL (MissingFrontmatter): step={g['step']} "
                f"shape={shape}; reply={shown}"
            )
            prev_kind, prev_step = "fail", g["step"]
        elif kind == "runerr":
            reason = g.get("error") or "<unparsed>"
            em = exhaust_reason_re.search(reason)
            if em:
                detail = (
                    f"kind=retry-exhaustion step={em.group('step')} "
                    f"counters={em.group('used')}/{em.group('max')}"
                )
            else:
                detail = "kind=<not-a-retry-exhaustion>"
            ec = g.get("ec") or "<unparsed>"
            print(
                f"[{t}] SRC={src} RUN-ERROR: event_count={ec}; {detail}; "
                f"reason={reason[:120]}…"
            )
            prev_kind = "runerr"
        elif kind == "loaded":
            print(
                f"[{t}] SRC={src} LOADED-RUN: status={g.get('status','<unparsed>')} "
                f"current_step={g.get('cur','<unparsed>')} "
                f"steps_executed={g.get('se') or '<unparsed>'}"
            )
            prev_kind = "loaded"
        elif kind == "resume":
            print(
                f"[{t}] SRC={src} RESUME: status={g.get('status','<unparsed>')} "
                f"current_step={g.get('cur','<unparsed>')} "
                f"(separate invocation re-executing the retained current step)"
            )
            # A resume restarts visit accounting for the retained step.
            prev_kind, prev_step = "resume", None


if __name__ == "__main__":
    main()
