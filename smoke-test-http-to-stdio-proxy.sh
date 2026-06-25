#!/usr/bin/env bash
# Smoke test for HTTP -> stdio proxying via docker-compose service.
#
# Verifies that:
# 1) filesystem-stdio-proxy accepts MCP stdio initialize
# 2) filesystem-stdio-proxy can proxy HTTP upstream from filesystem-proxy
# 3) tools/list returns a successful result

set -euo pipefail

COMPOSE_CMD="${COMPOSE_CMD:-docker-compose}"
HTTP_SERVICE="${HTTP_SERVICE:-filesystem-proxy}"
STDIO_SERVICE="${STDIO_SERVICE:-filesystem-stdio-proxy}"

if [[ -t 1 ]]; then
  GREEN='\033[0;32m'; RED='\033[0;31m'; YELLOW='\033[0;33m'; NC='\033[0m'
else
  GREEN=''; RED=''; YELLOW=''; NC=''
fi

PASS=0
FAIL=0

pass() { printf "  ${GREEN}OK${NC} %s\n" "$1"; ((++PASS)); }
fail() { printf "  ${RED}FAIL${NC} %s\n" "$1"; ((++FAIL)); }
info() { printf "  ${YELLOW}INFO${NC} %s\n" "$1"; }

printf "\nSmoke test (HTTP -> stdio): %s -> %s\n" "$HTTP_SERVICE" "$STDIO_SERVICE"

info "Sending initialize + tools/list over stdio"
OUT_FILE=$(mktemp)
trap 'rm -f "$OUT_FILE"' EXIT

if "$COMPOSE_CMD" run --rm --no-deps -T "$STDIO_SERVICE" >"$OUT_FILE" 2>&1 <<'EOF'
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"stdio-smoke","version":"1.0.0"}}}
{"jsonrpc":"2.0","method":"notifications/initialized"}
{"jsonrpc":"2.0","id":2,"method":"tools/list"}
EOF
then
  pass "stdio session completed"
else
  fail "stdio session exited with non-zero status"
fi

JSON_LINES=$(grep -E '^\{.*\}$' "$OUT_FILE" || true)

if printf '%s\n' "$JSON_LINES" | grep -q '"id":1.*"result"'; then
  pass "initialize returned result"
else
  fail "initialize result not found"
fi

if printf '%s\n' "$JSON_LINES" | grep -q '"id":2.*"result"'; then
  pass "tools/list returned result"
else
  fail "tools/list result not found"
fi

if printf '%s\n' "$JSON_LINES" | grep -q '"id":2.*"tools"'; then
  pass "tools payload present"
else
  fail "tools payload missing"
fi

if printf '%s\n' "$JSON_LINES" | grep -q '"error"'; then
  fail "error object found in JSON output"
else
  pass "no JSON-RPC errors"
fi

TOTAL=$((PASS + FAIL))
printf "\nTotal:  %d\n" "$TOTAL"
printf "Passed: %d\n" "$PASS"
printf "Failed: %d\n" "$FAIL"

if [[ $FAIL -eq 0 ]]; then
  printf "\n${GREEN}Smoke test passed.${NC}\n\n"
  exit 0
fi

printf "\n${RED}Smoke test failed. Output tail:${NC}\n" >&2
tail -n 60 "$OUT_FILE" >&2
exit 1