local M = {}

M.key = "implementation"

M.instructions = [[Implement the approved work and any later change requests for the same responsibility.

Maintain the approved plan's stable `TODO-NN` checklist and exactly one complete implementer evidence record per checked TODO. Preserve unchanged evidence records with semantic deep equality, replace affected records with current observations, and map every executed command to its one-based procedure step. Never claim `implemented` while a required TODO is unchecked, unproven, mismatched, duplicated, or not run.

Preserve `user_feedback` exactly when present; it is cumulative raw user direction and must not contain agent- or reviewer-generated feedback. Preserve `Goal`, `Validation`, `Work dir`, `Plan doc`, `Validation doc`, `RCA doc`, and `Repro test` values exactly when present. If a repro test is named, do not edit it; fix product code instead. Return `blocked` only when the work cannot proceed.]]

M.output = {
  status = { "implemented", "blocked" },
  fields = {
    summary = "string",
    user_feedback = "array",
    goal = "string",
    validation = "string",
    work_dir = "string",
    plan_doc = "string",
    validation_doc = "string",
    rca_doc = "string",
    repro_test = "string",
    files = "array",
    implementation_commands = "array",
    implementation_evidence = "array",
  },
}

return M
