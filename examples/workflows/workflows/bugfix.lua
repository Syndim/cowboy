local roles = {
  investigator = require("roles/investigator.lua"),
  planner = require("roles/planner.lua"),
  implementer = require("roles/implementer.lua"),
  tester = require("roles/tester.lua"),
  reviewer = require("roles/reviewer.lua"),
  blocker_reviewer = require("roles/blocker_reviewer.lua"),
  committer = require("roles/committer.lua"),
}

local investigate = require("steps/investigate_bug.lua")(roles)
local confirm_rca = require("steps/confirm_rca.lua")("confirm_rca")
local confirm_rca_input = require("steps/confirm_rca.lua")("confirm_rca_input")
local review_rca = require("steps/review_rca.lua")(roles)
local plan = require("steps/plan.lua")(roles, { kind = "bug fix" })
local clarify = require("steps/clarify.lua")("clarify")
local clarify_input = require("steps/clarify.lua")("clarify_input")
local review_plan = require("steps/review_plan.lua")(roles)
local confirm_plan = require("steps/confirm_plan.lua")("confirm_plan")
local confirm_plan_input = require("steps/confirm_plan.lua")("confirm_plan_input")
local implement = require("steps/implement.lua")(roles, { kind = "bug fix" })
local test = require("steps/test.lua")(roles, { kind = "bug fix" })
local review = require("steps/review_implementation.lua")(roles)
local confirm_result = require("steps/confirm_result.lua")("confirm_result")
local confirm_result_input = require("steps/confirm_result.lua")("confirm_result_input")
local review_result_feedback = require("steps/review_result_feedback.lua")(roles)
local revise = require("steps/revise.lua")(roles)
local commit = require("steps/commit.lua")(roles)
local done = require("steps/done.lua")("bug fix implemented, tested, reviewed, and committed")
local capture_blocker = require("steps/capture_blocker.lua")("capture_blocker")
local review_blocker = require("steps/review_blocker.lua")(roles)
local blocked = require("steps/blocked.lua")("bug fix workflow blocked")
local blocked_input = require("steps/blocked.lua")("bug fix workflow blocked", "blocked_input")
local triage_blocked = require("steps/triage_blocked.lua")({
  id = "triage_blocked",
  retry_steps = { "investigate", "plan", "implement", "test", "revise", "commit" },
})

investigate:on("documented", review_rca)
investigate:on("unclear", clarify)
investigate:on("blocked", capture_blocker)
review_rca:on("approved", confirm_rca)
review_rca:on("changes_requested", investigate)
confirm_rca:on("provided", confirm_rca_input)
confirm_rca_input:on("confirmed", plan)
confirm_rca_input:on("changes_requested", investigate)
plan:on("ready", review_plan)
plan:on("unclear", clarify)
clarify:on("provided", clarify_input)
clarify_input:on("clarified", investigate)
review_plan:on("approved", confirm_plan)
review_plan:on("changes_requested", plan)
confirm_plan:on("provided", confirm_plan_input)
confirm_plan_input:on("confirmed", implement)
confirm_plan_input:on("changes_requested", plan)
implement:on("implemented", test)
implement:on("blocked", capture_blocker)
test:on("passed", review)
test:on("failed", revise)
test:on("blocked", capture_blocker)
review:on("approved", confirm_result)
review:on("changes_requested", revise)
review:on("replan_requested", plan)
confirm_result:on("provided", confirm_result_input)
confirm_result_input:on("confirmed", commit)
confirm_result_input:on("changes_requested", review_result_feedback)
review_result_feedback:on("changes_requested", revise)
review_result_feedback:on("replan_requested", plan)
revise:on("implemented", test)
revise:on("blocked", capture_blocker)
commit:on("committed", done)
commit:on("blocked", capture_blocker)
capture_blocker:on("captured", review_blocker)
review_blocker:on("recoverable", triage_blocked)
review_blocker:on("user_required", blocked)
blocked:on("provided", blocked_input)
blocked_input:on("triaged", triage_blocked)
triage_blocked:on("investigate", investigate)
triage_blocked:on("plan", plan)
triage_blocked:on("implement", implement)
triage_blocked:on("test", test)
triage_blocked:on("revise", revise)
triage_blocked:on("commit", commit)

return workflow("bugfix", investigate, {
  description = "Investigate, review RCA, confirm RCA, plan, review, confirm, implement, test, review, confirm, and commit bug fixes",
})
