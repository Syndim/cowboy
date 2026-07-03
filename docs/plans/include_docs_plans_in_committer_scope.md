# Plan

Update the committer workflow definition in both workflow locations so the commit step commits every file related to an approved change, including planning documentation under `docs/plans`, rather than implying that only code files should be staged.

Keep the change scoped to workflow prompt/role wording. Do not change commit execution mechanics, git behavior, workflow graph transitions, or output schema.

# Changes

- Update `examples/workflows/roles/committer.lua` so the committer role explicitly treats code, tests, documentation, generated plan documents, and other request-related artifacts as eligible for the local commit.
- Update `examples/workflows/steps/commit.lua` so the commit prompt tells the agent to inspect the full diff and stage all request-related files, explicitly including `docs/plans/*.md` plan documents when they were created or updated for the change.
- Apply the same role wording change to `/home/yonsun/.config/cowboy/workflows/roles/committer.lua` so the user workflow copy stays in sync with the example workflow.
- Apply the same commit prompt change to `/home/yonsun/.config/cowboy/workflows/steps/commit.lua` so the user workflow copy commits plan documents too.
- Preserve the existing safety constraints: local commits only; never push, amend, rebase, or reset; return `committed` with hash/message or `blocked` when unsafe.

# Tests to be added/updated

- Add or update workflow-definition tests only if existing tests assert exact committer role or commit-step prompt text.
- If no exact prompt tests exist, do not add brittle string snapshot tests for this wording-only workflow prompt change.
- Verify both Lua workflow copies still compile/load through the existing workflow loader or chart tooling.

# How to verify

- Search both workflow locations for the old `stage only` wording and confirm it no longer appears in committer role or commit-step prompts.
- Search both workflow locations for `docs/plans` and confirm the commit-step prompt explicitly includes plan documents.
- Run the focused Lua workflow loader/chart verification that covers `examples/workflows/workflows/feature.lua` and `examples/workflows/workflows/bugfix.lua`.
- Run the same verification against `/home/yonsun/.config/cowboy/workflows/workflows/feature.lua` and `/home/yonsun/.config/cowboy/workflows/workflows/bugfix.lua` if the tooling accepts an external workflow root; otherwise manually inspect matching updated files.

# TODO

- [x] Update `examples/workflows/roles/committer.lua` to describe committing all request-related artifacts, not only code.
- [x] Update `examples/workflows/steps/commit.lua` to instruct staging all request-related files, explicitly including `docs/plans/*.md`.
- [x] Update `/home/yonsun/.config/cowboy/workflows/roles/committer.lua` with the same committer role wording.
- [x] Update `/home/yonsun/.config/cowboy/workflows/steps/commit.lua` with the same commit prompt wording.
- [x] Confirm the old `stage only` wording is gone from the committer role and commit step in both workflow roots.
- [x] Confirm `docs/plans` is mentioned in the commit prompt in both workflow roots.
- [x] Run focused workflow loading/chart verification for the changed workflow definitions.
