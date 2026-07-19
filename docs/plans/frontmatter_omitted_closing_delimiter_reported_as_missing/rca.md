# RCA: Frontmatter with an omitted closing delimiter is reported as MissingFrontmatter

## Bug behavior

Agent step replies frequently contain a well-formed opening YAML frontmatter
delimiter (`---`), a complete and valid set of scalar/list fields (including
`status`), and a Markdown body — but omit the **closing** `---` delimiter. When
this happens:

- `parse_frontmatter_output` fails with `Error::MissingFrontmatter`.
- The executor logs `agent step: failed to parse frontmatter output` and the
  failure is classified as recoverable, triggering a retry on the reused
  session.
- The retry nudge tells the agent its response "could not be parsed as a
  workflow result (agent response is missing YAML frontmatter)" and asks it to
  ensure the response *begins* with a valid frontmatter block. Because the
  response already begins with a valid opening delimiter and complete fields,
  the model re-emits substantially the same output, which fails identically.
- After the retry budget is exhausted the run fails and the already-completed,
  otherwise-valid work (status, fields, body) cannot be serialized into a
  `StepRecord`.

Sanitized evidence (redacted; run IDs, steps and structure preserved, no
credentials/personal data): the diagnostics bundle records **44** parse
failures across **11** runs and **20** logs, on steps `plan`, `review`,
`review_plan`, `review_rca`, `implement`, `investigate`, and `commit`. The
recurring error line is:

```
ERROR cowboy_workflow_agent::executor: agent step: failed to parse frontmatter
output run_id=<redacted> step=<step> reply=<reply>
```

Many `reply=` values begin with a lone `---` line (opening delimiter present),
which is the fingerprint of this defect rather than a truly missing frontmatter
block.

## Root cause

`split_frontmatter` in `crates/workflow/agent/src/frontmatter.rs` requires
**both** delimiters and collapses two distinct failure modes into one error:

```rust
fn split_frontmatter(raw: &str) -> Result<(&str, &str)> {
    let Some(open_start) = find_frontmatter_open(raw) else {
        return Err(Error::MissingFrontmatter); // no opening delimiter
    };
    let frontmatter = &raw[open_start..];
    let after_open = &frontmatter[3..];
    let after_open = after_open.trim_start_matches(['\r', '\n']);
    let Some(close_start) = after_open.find("\n---") else {
        return Err(Error::MissingFrontmatter); // no CLOSING delimiter -> same error
    };
    // ...
}
```

When the closing delimiter is absent, the second `else` branch returns
`Error::MissingFrontmatter` — the same error used when there is no opening
delimiter at all. Consequences:

1. **No recovery for an omitted closing delimiter.** A reply that carries a
   valid opening delimiter plus complete, parseable fields and body is rejected
   outright. The lenient/fallback parser (`parse_lenient_frontmatter`) is only
   reached *after* `split_frontmatter` succeeds, so it never runs for this case.

2. **Failure classification is too coarse for retry nudges and diagnostics.**
   `Error` (in `crates/workflow/agent/src/error.rs`) has a single
   `MissingFrontmatter` variant and no distinct variants for "opening delimiter
   missing", "closing delimiter missing", "YAML not a mapping", or "schema"
   (missing `status`) problems in a way that lets the nudge target the actual
   defect. The reason string is derived from `last_error.to_string()`
   (`crates/workflow/engine/src/runner.rs`) and passed to
   `build_retry_nudge` (`crates/workflow/agent/src/prompt.rs`), so a
   missing-closing-delimiter reply produces the misleading nudge "missing YAML
   frontmatter" and instructs the model to make the response *begin* with
   frontmatter — advice that does not describe the real problem, so the retry
   reproduces the same output.

The defect is therefore a parsing + error-classification gap, not a transport or
schema problem.

## Reproduction steps

1. Build/test the agent crate from the repository root.
2. Feed the parser a representative reply that has an opening `---`, complete and
   valid fields (including `status`), a Markdown body, and **no** closing `---`
   (mirrors the sanitized `plan`/`review`/`commit` evidence):

   ```text
   ---
   status: ready
   summary: Strict stdlib PNG CRC/structure validation plan
   files:
     - scripts/verify.sh
     - tests/verify_crc_corrupt_frame.sh

   ## Plan

   1. Add a strict CRC check to the stdlib PNG fallback parser.
   2. Add a focused regression test for CRC-corrupted frames.
   ```

3. Observe that `parse_frontmatter_output` returns `Err(Error::MissingFrontmatter)`
   even though every field and the body are valid and present.

This is captured as the automated regression test below.

## Regression test

- **Test file path:** `crates/workflow/agent/src/frontmatter.rs`
- **Test name:** `frontmatter::tests::recovers_frontmatter_with_omitted_closing_delimiter`
- **Command:**

  ```bash
  cargo test -p cowboy-workflow-agent --lib \
    frontmatter::tests::recovers_frontmatter_with_omitted_closing_delimiter
  ```

- **Expected result before the fix:** the test **fails**. `parse_frontmatter_output`
  returns `Err(Error::MissingFrontmatter)`, so the `.expect(...)` on the parse
  result panics. After a conservative recovery fix, the test is expected to pass
  (status `ready`, `summary` and `files` fields preserved, body starting with
  `## Plan`).

The test asserts the desired recovered behavior, so it is red before the product
fix and green after it, without weakening any existing assertion.

## Current failing result

Running the narrow command above against the current (unfixed) product code:

```
running 1 test
test frontmatter::tests::recovers_frontmatter_with_omitted_closing_delimiter ... FAILED

failures:

---- frontmatter::tests::recovers_frontmatter_with_omitted_closing_delimiter stdout ----

thread 'frontmatter::tests::recovers_frontmatter_with_omitted_closing_delimiter'
panicked at crates/workflow/agent/src/frontmatter.rs:
frontmatter with an omitted closing delimiter should be recovered: MissingFrontmatter

failures:
    frontmatter::tests::recovers_frontmatter_with_omitted_closing_delimiter

test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 24 filtered out
```

The panic value `MissingFrontmatter` confirms the root cause: the closing-delimiter
branch of `split_frontmatter` rejects an otherwise-complete result.

## Fix constraints

- **Tests/docs only during investigation.** No product code was changed to
  produce this RCA and the failing regression test; the fix is a separate step.
- **Conservative recovery only.** Recovery for an omitted closing delimiter must
  require an unambiguous opening delimiter plus fields that parse as a valid YAML
  mapping with the required `status`. Do not accept plain prose, and do not
  accept malformed or ambiguous YAML as frontmatter. Preserve the existing strict
  parse path for well-formed, fully-delimited blocks and for genuinely
  frontmatter-less prose (the existing `rejects_missing_frontmatter` behavior for
  plain text must remain).
- **Deterministic body boundary.** With no closing delimiter, the boundary
  between the YAML mapping and the Markdown body must be chosen conservatively
  and deterministically (e.g. the first line that can no longer belong to the
  mapping, such as a Markdown heading or a blank-line-separated body), avoiding
  swallowing body content into YAML or vice versa.
- **Accurate failure classification.** Distinguish and report opening-delimiter
  vs closing-delimiter vs YAML-parse vs schema (missing/typed `status`) failures
  precisely enough that `build_retry_nudge` can give a targeted nudge (e.g. "add
  the closing `---`") and that diagnostics/logs are actionable. Update the
  `Error` enum and its `recoverable()` mapping accordingly; all such
  frontmatter/parse failures should remain recoverable.
- **Preserve `user_feedback` exactly.** When a `user_feedback` field is present
  in the frontmatter, it must be carried through to the output fields verbatim.
  It is cumulative raw user direction; recovery/parsing must not rewrite, merge,
  reorder, or append agent- or reviewer-generated feedback to it.
- **Review sibling paths.** Apply the same reasoning to related parser, error,
  and prompt paths: `crates/workflow/agent/src/frontmatter.rs`,
  `crates/workflow/agent/src/error.rs`,
  `crates/workflow/agent/src/prompt.rs` (`build_retry_nudge` reason wiring), and
  the reason plumbing in `crates/workflow/engine/src/runner.rs` /
  `crates/workflow/agent/src/executor.rs`.
- **Validation for the fix (later).** Add regression tests derived from
  representative review/plan/commit logs — long block scalars, Markdown
  headings/body, prose preamble, valid closed blocks, and negative
  ambiguous/malformed cases — then run the focused tests and
  `cargo test --workspace`.
- **Delivery.** Commit locally only; do not push or open a PR.
