#!/usr/bin/env bash
# Smoke test: verify stdio -> HTTP proxying for the filesystem MCP server.
#
# Usage:
#   ./smoke-test-stdio-to-http-proxy.sh [URL]
#   ./smoke-test-stdio-to-http-proxy.sh http://localhost:9090/mcp

set -eu

BASE_URL="${1:-http://localhost:9090/mcp}"

# ── Colours ───────────────────────────────────────────────────────
if [[ -t 1 ]]; then
  GREEN='\033[0;32m'; RED='\033[0;31m'; YELLOW='\033[0;33m'
  BOLD='\033[1m'; NC='\033[0m'
else
  GREEN=''; RED=''; YELLOW=''; BOLD=''; NC=''
fi

PASS=0; FAIL=0

pass() { printf "  ${GREEN}✓${NC} %s\n" "$1"; ((++PASS)); }
fail() { printf "  ${RED}✗${NC} %s\n" "$1"; ((++FAIL)); }
info() { printf "  ${YELLOW}→${NC} %s\n" "$1"; }

# ── Temp files ────────────────────────────────────────────────────

HEADER_FILE=$(mktemp)
BODY_FILE=$(mktemp)
trap 'rm -f "$HEADER_FILE" "$BODY_FILE"' EXIT

# ── Core helpers ──────────────────────────────────────────────────

# Extract the JSON payload from either:
#   • plain JSON:  {"jsonrpc":...}
#   • event-stream framed:  data: {"jsonrpc":...}
# Also strips any trailing \r left by \r\n line endings.
parse_response() {
  local raw="$1"
  # Servers may emit multiple event-stream data frames before the JSON-RPC object
  # (for example: "data: id: ...", "data: retry: ...", then JSON).
  # Scan the full payload and return the first complete JSON object.
  printf '%s' "$raw" | tr -d '\r' | awk '
    BEGIN {
      in_json = 0
      depth = 0
      in_string = 0
      escaped = 0
      buf = ""
    }
    {
      line = $0
      sub(/^data:[[:space:]]*/, "", line)

      for (i = 1; i <= length(line); i++) {
        c = substr(line, i, 1)

        if (!in_json) {
          if (c == "{") {
            in_json = 1
            depth = 1
            in_string = 0
            escaped = 0
            buf = "{"
          }
          continue
        }

        buf = buf c

        if (escaped) {
          escaped = 0
          continue
        }

        if (in_string && c == "\\") {
          escaped = 1
          continue
        }

        if (c == "\"") {
          in_string = !in_string
          continue
        }

        if (!in_string) {
          if (c == "{") {
            depth++
          } else if (c == "}") {
            depth--
            if (depth == 0) {
              print buf
              exit
            }
          }
        }
      }

      if (in_json) {
        buf = buf "\n"
      }
    }
  '
}

# Send a JSON-RPC POST; return parsed JSON.
# Uses --output FILE instead of $() to avoid command-substitution issues
# with streaming event-stream responses.
rpc() {
  local session_id="$1"
  local body="$2"

  local http_code
  http_code=$(curl -s --max-time 10 -X POST \
    -H "Content-Type: application/json" \
    -H "Accept: application/json, text/event-stream" \
    ${session_id:+-H "Mcp-Session-Id: $session_id"} \
    -d "$body" \
    --output "$BODY_FILE" \
    -w "%{http_code}" \
    "$BASE_URL") || { fail "curl failed (network error)"; return 1; }

  if [[ "$http_code" -ge 400 ]]; then
    fail "HTTP $http_code from server (body: $(head -c 120 "$BODY_FILE" | tr -d '\n'))"
    return 1
  fi

  parse_response "$(cat "$BODY_FILE")"
}

# Send a JSON-RPC notification (no id → server returns 202, no body).
notify() {
  local session_id="$1"
  local body="$2"
  curl -s --max-time 5 -X POST \
    -H "Content-Type: application/json" \
    -H "Accept: application/json, text/event-stream" \
    -H "Mcp-Session-Id: $session_id" \
    -d "$body" \
    -o /dev/null \
    -w "%{http_code}" \
    "$BASE_URL" || true
}

# Fail + print message if the JSON response contains an "error" key.
assert_no_error() {
  local json="$1" label="$2"
  if printf '%s' "$json" | grep -q '"error"'; then
    local msg
    msg=$(printf '%s' "$json" | grep -o '"message":"[^"]*"' | head -1 | cut -d'"' -f4 || true)
    fail "$label: ${msg:-<unknown error>}"
    return 1
  fi
  return 0
}

# ── Step 1: initialize ────────────────────────────────────────────
printf "\n${BOLD}1. Initialize${NC}\n"

HTTP_INIT=$(curl -s --max-time 10 -X POST \
  -H "Content-Type: application/json" \
  -H "Accept: application/json, text/event-stream" \
  -D "$HEADER_FILE" \
  -d '{
    "jsonrpc": "2.0",
    "id": 1,
    "method": "initialize",
    "params": {
      "protocolVersion": "2024-11-05",
      "capabilities": {},
      "clientInfo": { "name": "smoke-test", "version": "1.0.0" }
    }
  }' \
  --output "$BODY_FILE" \
  -w "%{http_code}" \
  "$BASE_URL") || { fail "curl failed — is the proxy running at $BASE_URL?"; exit 1; }

SESSION_ID=$(grep -i '^Mcp-Session-Id:' "$HEADER_FILE" | tr -d '\r' | awk '{print $2}' || true)
CONTENT_TYPE=$(grep -i '^content-type:' "$HEADER_FILE" | tr -d '\r' | awk '{print $2}' || true)

if [[ -z "$SESSION_ID" ]]; then
  fail "No Mcp-Session-Id header (HTTP $HTTP_INIT) — is the proxy running?"
  info "Headers:" && cat "$HEADER_FILE"
  exit 1
fi
pass "Session established (id: $SESSION_ID)"
info "HTTP $HTTP_INIT, Content-Type: ${CONTENT_TYPE:-unknown}"

RAW_INIT=$(cat "$BODY_FILE")
info "Raw body (first 120 chars): $(printf '%s' "$RAW_INIT" | head -c 120 | tr -d '\n')"

INIT_JSON=$(parse_response "$RAW_INIT")

SERVER_NAME=$(printf '%s' "$INIT_JSON" | grep -o '"name":"[^"]*"' | head -1 | cut -d'"' -f4 || true)
PROTOCOL=$(printf '%s' "$INIT_JSON" | grep -o '"protocolVersion":"[^"]*"' | head -1 | cut -d'"' -f4 || true)
[[ -n "$SERVER_NAME" ]] && info "Server: $SERVER_NAME, protocol: ${PROTOCOL:-unknown}"

if printf '%s' "$INIT_JSON" | grep -q '"result"'; then
  pass "Initialize returned a result object"
else
  fail "Initialize response missing 'result'"
  info "Parsed INIT_JSON: $(printf '%s' "$INIT_JSON" | head -c 200)"
fi

# ── Step 2: notifications/initialized ────────────────────────────
printf "\n${BOLD}2. Confirm handshake${NC}\n"

HTTP_STATUS=$(notify "$SESSION_ID" '{"jsonrpc":"2.0","method":"notifications/initialized"}')
if [[ "$HTTP_STATUS" == "202" ]]; then
  pass "Handshake confirmed (HTTP 202)"
else
  fail "Expected HTTP 202 from notifications/initialized, got $HTTP_STATUS"
fi

# ── Step 3: tools/list ────────────────────────────────────────────
printf "\n${BOLD}3. List tools${NC}\n"

TOOLS_JSON=$(rpc "$SESSION_ID" '{"jsonrpc":"2.0","id":2,"method":"tools/list"}') || true
assert_no_error "$TOOLS_JSON" "tools/list" || true

for tool in list_directory read_file get_file_info directory_tree list_allowed_directories; do
  if printf '%s' "$TOOLS_JSON" | grep -q "\"$tool\""; then
    pass "Tool available: $tool"
  else
    fail "Tool missing:   $tool"
  fi
done

# ── Step 4: list_allowed_directories ─────────────────────────────
printf "\n${BOLD}4. Verify allowed directories${NC}\n"

ALLOWED_JSON=$(rpc "$SESSION_ID" \
  '{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"list_allowed_directories","arguments":{}}}') || true
assert_no_error "$ALLOWED_JSON" "list_allowed_directories" || true

if printf '%s' "$ALLOWED_JSON" | grep -q '/workspace/src'; then
  pass "/workspace/src is an allowed directory"
else
  fail "/workspace/src not in allowed directories"
  info "Response (first 200): $(printf '%s' "$ALLOWED_JSON" | head -c 200)"
fi

# ── Step 5: list_directory ────────────────────────────────────────
printf "\n${BOLD}5. List /workspace/src${NC}\n"

LIST_JSON=$(rpc "$SESSION_ID" \
  '{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"list_directory","arguments":{"path":"/workspace/src"}}}') || true
assert_no_error "$LIST_JSON" "list_directory" || true

for f in main.rs cli.rs proxy_server.rs config_loader.rs http_client.rs; do
  if printf '%s' "$LIST_JSON" | grep -q "$f"; then
    pass "Found: $f"
  else
    fail "Not found: $f"
  fi
done

# ── Step 6: read_file ─────────────────────────────────────────────
printf "\n${BOLD}6. Read /workspace/src/main.rs${NC}\n"

READ_JSON=$(rpc "$SESSION_ID" \
  '{"jsonrpc":"2.0","id":5,"method":"tools/call","params":{"name":"read_file","arguments":{"path":"/workspace/src/main.rs"}}}') || true
assert_no_error "$READ_JSON" "read_file" || true

if printf '%s' "$READ_JSON" | grep -q 'fn main'; then
  pass "main.rs readable, contains 'fn main'"
else
  fail "main.rs read failed or unexpected content"
fi
if printf '%s' "$READ_JSON" | grep -q 'tokio'; then
  pass "main.rs contains expected Rust content ('tokio')"
else
  fail "main.rs missing expected Rust content"
fi

# ── Step 7: get_file_info ─────────────────────────────────────────
printf "\n${BOLD}7. Get file info for /workspace/src/cli.rs${NC}\n"

INFO_JSON=$(rpc "$SESSION_ID" \
  '{"jsonrpc":"2.0","id":6,"method":"tools/call","params":{"name":"get_file_info","arguments":{"path":"/workspace/src/cli.rs"}}}') || true
assert_no_error "$INFO_JSON" "get_file_info" || true

for field in size type; do
  if printf '%s' "$INFO_JSON" | grep -q "$field"; then
    pass "File info contains '$field'"
  else
    fail "File info missing '$field'"
  fi
done

# ── Step 8: reject path outside workspace ────────────────────────
printf "\n${BOLD}8. Verify path traversal is blocked${NC}\n"

ESCAPE_JSON=$(rpc "$SESSION_ID" \
  '{"jsonrpc":"2.0","id":7,"method":"tools/call","params":{"name":"read_file","arguments":{"path":"/etc/passwd"}}}') || true

if printf '%s' "$ESCAPE_JSON" | grep -qi 'isError.*true\|not allowed\|outside\|denied\|access'; then
  pass "Access to /etc/passwd is correctly denied"
else
  fail "Access to /etc/passwd was NOT denied (security issue)"
  info "Response: $(printf '%s' "$ESCAPE_JSON" | head -c 200)"
fi

# ── Summary ───────────────────────────────────────────────────────
TOTAL=$((PASS + FAIL))
printf "\n────────────────────────────────\n"
printf "  Total:  %d\n"          "$TOTAL"
printf "  ${GREEN}Passed: %d${NC}\n" "$PASS"
printf "  ${RED}Failed: %d${NC}\n"  "$FAIL"
printf "────────────────────────────────\n"

if [[ $FAIL -eq 0 ]]; then
  printf "\n${GREEN}${BOLD}All checks passed.${NC}\n\n"
  exit 0
else
  printf "\n${RED}${BOLD}%d check(s) failed.${NC}\n\n" "$FAIL"
  exit 1
fi
