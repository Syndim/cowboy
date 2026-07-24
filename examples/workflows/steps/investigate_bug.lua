local context = require("utils/context.lua")

return function(roles, opts)
  opts = opts or {}
  local investigate = step(opts.id or "investigate", { role = roles.investigator })
  investigate.run = function(ctx)
    local prompt, errors = context.build_agent_prompt(ctx, {
      objective = "Investigate this bug report before any fix planning.",
      heading = "Investigation feedback:",
      include_step = true,
      include_status = true,
      fields = { "clarification", "user_feedback", "summary", "feedback", "work_dir", "rca_doc", "repro_test", "files", "command", "failure", "failures" },
      include_body = true,
      guidance = { "preserve_user_feedback" },
      instructions = [[Reproduce or otherwise ground the bug in the repository. Before returning "documented", create a bug-fix work folder at `docs/plans/<snake_case_bug_summary>/`; create `docs/plans` if it does not exist. Generate `<snake_case_bug_summary>` from the concise bug summary by lowercasing it, removing punctuation, and joining words with underscores. Write the Root Cause Analysis document to `docs/plans/<snake_case_bug_summary>/rca.md`.

The RCA document must contain these sections exactly:
- Bug behavior
- Root cause
- Root cause evidence
- Reproduction steps
- Regression test
- Current failing result
- Fix constraints

The Root cause evidence section must prove the root cause is correct. Provide a concrete, step-by-step walkthrough of how the bug happens, tracing the flow from trigger to observed failure. Prefer an example flow reconstructed from real log output: quote the relevant log lines (redacted as needed) and, for each step, explain what the line shows and how it advances toward the defect. When logs are unavailable, ground the walkthrough in specific source locations (file, function, and line) that carry the flow instead. Do not assert the root cause without this traceable evidence.

The RCA document must not include sensitive user data. Redact, generalize, or omit secrets, credentials, personal data, private paths, and proprietary customer content while preserving enough technical detail to reproduce the issue.

Add one focused regression test that reproduces the bug and fails before the fix. Run the narrow command for that test and record the failing command/output in the RCA. Do not change product code. Return `work_dir` exactly as the written `docs/plans/<snake_case_bug_summary>` folder path. Return `rca_doc` exactly as `<work_dir>/rca.md`. Return `repro_test` as the test file path plus test name when available, e.g. `path/to/test.rs::test_name`. Include both the RCA doc and test file in `files`.

Return "documented" when the RCA and failing test are ready, "unclear" when more user context is required, or "blocked" if the investigation cannot proceed.]],
    })
    if not prompt then return context.invalid_context_action(ctx, "unclear", errors) end
    return action.agent {
      role = roles.investigator,
      prompt = prompt,
      output = {
        status = { "documented", "unclear", "blocked" },
        fields = { summary = "string", user_feedback = "array", work_dir = "string", rca_doc = "string", repro_test = "string", files = "array", command = "string", failure = "string" },
      },
    }
  end
  return investigate
end
