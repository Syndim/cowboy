local roles = {
  planner = require("roles/planner.lua"),
  implementer = require("roles/implementer.lua"),
  tester = require("roles/tester.lua"),
  reviewer = require("roles/reviewer.lua"),
  committer = require("roles/committer.lua"),
}

local plan = require("steps/plan.lua")(roles, { kind = "feature" })
local clarify = require("steps/clarify.lua")("clarify")
local clarify_answer = require("steps/clarify.lua")("clarify_answer")
local review_plan = require("steps/review_plan.lua")(roles)
local confirm_plan = require("steps/confirm_plan.lua")("confirm_plan")
local confirm_plan_answer = require("steps/confirm_plan.lua")("confirm_plan_answer")
local implement = require("steps/implement.lua")(roles, { kind = "feature" })
local test = require("steps/test.lua")(roles, { kind = "feature" })
local review = require("steps/review_implementation.lua")(roles)
local confirm_result = require("steps/confirm_result.lua")("confirm_result")
local confirm_result_answer = require("steps/confirm_result.lua")("confirm_result_answer")
local review_result_feedback = require("steps/review_result_feedback.lua")(roles)
local revise = require("steps/revise.lua")(roles)
local commit = require("steps/commit.lua")(roles)
local done = require("steps/done.lua")("feature implemented, tested, reviewed, and committed")
local blocked = require("steps/blocked.lua")("feature workflow blocked")
local blocked_answer = require("steps/blocked.lua")("feature workflow blocked", "blocked_answer")
local triage_blocked = require("steps/triage_blocked.lua")("triage_blocked")

plan:on("ready", review_plan)
plan:on("unclear", clarify)
clarify:on("answered", clarify_answer)
clarify_answer:on("clarified", plan)
review_plan:on("approved", confirm_plan)
review_plan:on("changes_requested", plan)
confirm_plan:on("answered", confirm_plan_answer)
confirm_plan_answer:on("confirmed", implement)
confirm_plan_answer:on("changes_requested", plan)
implement:on("implemented", test)
implement:on("blocked", blocked)
test:on("passed", review)
test:on("failed", revise)
test:on("blocked", blocked)
review:on("approved", confirm_result)
review:on("changes_requested", revise)
review:on("replan_requested", plan)
confirm_result:on("answered", confirm_result_answer)
confirm_result_answer:on("confirmed", commit)
confirm_result_answer:on("changes_requested", review_result_feedback)
review_result_feedback:on("changes_requested", revise)
review_result_feedback:on("replan_requested", plan)
revise:on("implemented", test)
revise:on("blocked", blocked)
commit:on("committed", done)
commit:on("blocked", blocked)
blocked:on("answered", blocked_answer)
blocked_answer:on("triaged", triage_blocked)
triage_blocked:on("plan", plan)
triage_blocked:on("implement", implement)
triage_blocked:on("revise", revise)

return workflow("feature", plan, {
  description = "Plan, review, confirm, implement, test, review, confirm, and commit feature work",
})
