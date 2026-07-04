local context = require("utils/context.lua")

return function(roles, opts)
  opts = opts or {}
  local investigate = step(opts.id or "investigate", { role = roles.investigator })
  investigate.run = function(ctx)
    return action.agent {
      role = roles.investigator,
      prompt = [[Investigate this bug report before any fix planning:

]] .. context.request_context(ctx) .. context.previous_step_context(ctx, "Investigation feedback:") .. [[

Reproduce or otherwise ground the bug in the repository. Before returning "documented", create a bug-fix work folder at `docs/plans/<snake_case_bug_summary>/`; create `docs/plans` if it does not exist. Generate `<snake_case_bug_summary>` from the concise bug summary by lowercasing it, removing punctuation, and joining words with underscores. Write the Root Cause Analysis document to `docs/plans/<snake_case_bug_summary>/rca.md`.

The RCA document must contain these sections exactly:
- Bug behavior
- Root cause
- Reproduction steps
- Regression test
- Current failing result
- Fix constraints

Add one focused regression test that reproduces the bug and fails before the fix. Run the narrow command for that test and record the failing command/output in the RCA. Do not change product code. Return `work_dir` exactly as the written `docs/plans/<snake_case_bug_summary>` folder path. Return `rca_doc` exactly as `<work_dir>/rca.md`. Return `repro_test` as the test file path plus test name when available, e.g. `path/to/test.rs::test_name`. Include both the RCA doc and test file in `files`.

Return "documented" when the RCA and failing test are ready, "unclear" when more user context is required, or "blocked" if the investigation cannot proceed.]],
      output = {
        status = { "documented", "unclear", "blocked" },
        fields = { summary = "string", work_dir = "string", rca_doc = "string", repro_test = "string", files = "array", command = "string", failure = "string" },
      },
    }
  end
  return investigate
end
