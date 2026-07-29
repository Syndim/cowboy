#!/usr/bin/env bash
set -euo pipefail

command -v python3 >/dev/null
command -v curl >/dev/null
command -v playwright-cli >/dev/null

ROOT="$(git rev-parse --show-toplevel)"
test "$ROOT" = "$(pwd)"
SMOKE="$ROOT/target/export-smoke-review"
RESULT_ENV="$SMOKE/result.env"
test -f "$RESULT_ENV"
source "$RESULT_ENV"
test "$SMOKE" = "$ROOT/target/export-smoke-review"
test "$HTML_PATH" = "$SMOKE/$(basename "$HTML_PATH")"
test -f "$HTML_PATH"

install_local_browser_libraries() {
  local browser
  browser="$(find "${HOME:?}/.cache/ms-playwright" -path '*/chrome-linux64/chrome' -type f | sort | tail -n 1)"
  test -n "$browser"
  if ! ldd "$browser" | rg -q 'not found'; then
    return
  fi

  command -v apt >/dev/null
  command -v dpkg-deb >/dev/null
  local package_dir="$SMOKE/browser-libs/packages"
  local library_root="$SMOKE/browser-libs/root"
  mkdir -p "$package_dir" "$library_root"
  (
    cd "$package_dir"
    apt download libnspr4 libnss3 libasound2t64
    for package in ./*.deb; do
      dpkg-deb -x "$package" "$library_root"
    done
  )
  export LD_LIBRARY_PATH="$library_root/usr/lib/x86_64-linux-gnu${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
  ! ldd "$browser" | rg -q 'not found'
}

install_local_browser_libraries

PORT=8766
URL="http://127.0.0.1:$PORT/$(basename "$HTML_PATH")"
SESSION="export-review"
CODE_FILE="$SMOKE/browser-check.js"
RESULT_FILE="$SMOKE/browser-result.txt"

python3 -m http.server "$PORT" --bind 127.0.0.1 --directory "$SMOKE" >"$SMOKE/http.log" 2>&1 &
SERVER_PID=$!
cleanup() {
  (cd "$SMOKE" && playwright-cli -s="$SESSION" close >/dev/null 2>&1) || true
  kill "$SERVER_PID" >/dev/null 2>&1 || true
}
trap cleanup EXIT

for _ in $(seq 1 50); do
  if curl --fail --silent "$URL" >/dev/null; then
    break
  fi
  sleep 0.1
done
curl --fail --silent "$URL" >/dev/null

cat >"$CODE_FILE" <<EOF
async page => {
  const requests = [];
  page.on("request", request => requests.push(request.url()));
  await page.goto("$URL");
  const initialRequests = requests.slice();
  const details = page.locator("details.card");
  const headers = await details.locator("summary").allTextContents();
  const bodies = await details.allTextContents();

  if (await details.count() !== 5) throw new Error("expected five cards");
  if (!headers[0].includes("Request") || !headers[1].includes("Run started") ||
      !headers[2].includes("Agent response") || !headers[3].includes("Inspect fixture") ||
      !headers[4].includes("Run completed")) throw new Error("unexpected card order");
  if (headers.filter(header => header.includes("Agent response")).length !== 1 ||
      !bodies[2].includes("first response line\\nsecond response line with BODY_ONLY_SEARCH_TOKEN")) {
    throw new Error("response chunks were not coalesced");
  }
  if (headers.filter(header => header.includes("Inspect fixture")).length !== 1 ||
      !bodies[3].includes("TOOL_UPDATE_SEARCH_TOKEN") || bodies[3].includes("running")) {
    throw new Error("tool update did not replace the running card");
  }
  if (await details.evaluateAll(nodes => nodes.some(node => node.open))) {
    throw new Error("cards were not initially collapsed");
  }

  await details.nth(0).locator("summary").click();
  if (!(await details.nth(0).evaluate(node => node.open))) throw new Error("card expansion failed");
  await page.locator("#expand-all").click();
  if (!(await details.evaluateAll(nodes => nodes.every(node => node.open)))) throw new Error("expand all failed");
  await page.locator("#collapse-all").click();
  if (await details.evaluateAll(nodes => nodes.some(node => node.open))) throw new Error("collapse all failed");

  const search = page.locator("#search");
  await search.fill("BODY_ONLY_SEARCH_TOKEN");
  if (await details.evaluateAll(nodes => nodes.filter(node => !node.hidden).length) !== 1 ||
      await details.evaluateAll(nodes => nodes.filter(node => !node.hidden && node.open).length) !== 1 ||
      await page.locator("#match-count").textContent() !== "1 match") {
    throw new Error("response body search failed");
  }
  await search.fill("");
  if (await details.evaluateAll(nodes => nodes.filter(node => !node.hidden).length) !== 5 ||
      await details.evaluateAll(nodes => nodes.some(node => node.open))) {
    throw new Error("clearing search did not restore collapsed cards");
  }
  await search.fill("TOOL_UPDATE_SEARCH_TOKEN");
  const visibleHeaders = await details.evaluateAll(nodes =>
    nodes.filter(node => !node.hidden).map(node => node.querySelector("summary").textContent));
  if (visibleHeaders.length !== 1 || !visibleHeaders[0].includes("Inspect fixture")) {
    throw new Error("tool output search failed");
  }

  requests.length = 0;
  await page.reload();
  const reloadRequests = requests.slice();
  const allRequests = initialRequests.concat(reloadRequests);
  const externalRequests = allRequests.filter(url =>
    url.startsWith("http://") || url.startsWith("https://")).filter(url => url !== "$URL");
  if (initialRequests.length !== 1 || initialRequests[0] !== "$URL") {
    throw new Error("initial load fetched resources beyond the HTML document: " + initialRequests.join(", "));
  }
  if (reloadRequests.length !== 1 || reloadRequests[0] !== "$URL") {
    throw new Error("reload fetched resources beyond the HTML document: " + reloadRequests.join(", "));
  }
  if (externalRequests.length !== 0) {
    throw new Error("external requests observed: " + externalRequests.join(", "));
  }
  return {
    cards: 5,
    response_matches: 1,
    tool_matches: 1,
    reload_requests: reloadRequests.length,
    external_requests: externalRequests.length
  };
}
EOF

(cd "$SMOKE" && playwright-cli -s="$SESSION" open --browser=chromium about:blank >/dev/null)
(cd "$SMOKE" && playwright-cli -s="$SESSION" run-code --filename="$CODE_FILE") | tee "$RESULT_FILE"
rg -q '"cards":5' "$RESULT_FILE"
rg -q '"response_matches":1' "$RESULT_FILE"
rg -q '"tool_matches":1' "$RESULT_FILE"
rg -q '"reload_requests":1' "$RESULT_FILE"
rg -q '"external_requests":0' "$RESULT_FILE"
printf 'EXPORT_BROWSER_OK cards=5 response_matches=1 tool_matches=1 reload_requests=1 external_requests=0\n'
