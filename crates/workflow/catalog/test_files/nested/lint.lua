local linter = role("linter", "Run linters and report findings")

local lint = step("lint", { role = linter })
lint.run = function(ctx)
  return action.status { status = "success", body = "no lint errors" }
end

-- A workflow in a subdirectory gets the id "nested/lint".
return workflow("lint", lint, { description = "Runs project linters" })
