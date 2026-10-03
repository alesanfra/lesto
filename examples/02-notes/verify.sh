#!/usr/bin/env bash
# End to end with curl: CRUD, permissions, validation, docs and MCP against the notes example.
#
#   bash examples/02-notes/verify.sh                 # starts the example on $BASE, stops it after
#   BASE=http://127.0.0.1:9000 bash verify.sh        # against a server already running there
#
# The database is in memory, so a server started by hand must be fresh: the first check expects
# no notes. Needs bash and curl, nothing else.
set -euo pipefail

BASE=${BASE:-http://127.0.0.1:8765}
BOB="Authorization: Bearer bob-token"      # notes:write
ALICE="Authorization: Bearer alice-token"  # notes:write, notes:delete
JSON="Content-Type: application/json"

passed=0
server=""

cleanup() {
  if [[ -n $server ]]; then
    kill "$server" 2>/dev/null || true
    wait "$server" 2>/dev/null || true
  fi
}
trap cleanup EXIT

# ---- start the example unless something already answers on $BASE ----------------------------

if ! curl -fsS -o /dev/null "$BASE/openapi.json" 2>/dev/null; then
  port=${BASE##*:}
  root=$(cd "$(dirname "$0")/../.." && pwd)
  echo "building and starting the example on port $port..."
  (cd "$root" && cargo build -q -p notes)
  LESTO_PORT=$port LESTO_LOG=${LESTO_LOG:-off} "$root/target/debug/notes" >/dev/null &
  server=$!
  for _ in $(seq 50); do
    curl -fsS -o /dev/null "$BASE/openapi.json" 2>/dev/null && break
    sleep 0.1
  done
fi

# ---- helpers ---------------------------------------------------------------------------------

# check EXPECTED_STATUS DESCRIPTION [EXPECTED_BODY_FRAGMENT] -- CURL_ARGS...
# Prints the body on failure; the body of the last call is in $body.
check() {
  local expected=$1 what=$2 fragment=""
  shift 2
  if [[ $1 != -- ]]; then
    fragment=$1
    shift
  fi
  shift # --
  local out status
  out=$(curl -sS -w '\n%{http_code}' "$@")
  status=${out##*$'\n'}
  body=${out%$'\n'*}
  if [[ $status != "$expected" ]]; then
    echo "FAIL $what: status $status, expected $expected"
    echo "     $body"
    exit 1
  fi
  if [[ -n $fragment && $body != *"$fragment"* ]]; then
    echo "FAIL $what: body has no $fragment"
    echo "     $body"
    exit 1
  fi
  echo "ok   $status $what"
  passed=$((passed + 1))
}

# check_header DESCRIPTION HEADER_FRAGMENT -- CURL_ARGS...
check_header() {
  local what=$1 fragment=$2
  shift 3
  local headers
  headers=$(curl -sS -o /dev/null -D - "$@" | tr -d '\r')
  if [[ $headers != *"$fragment"* ]]; then
    echo "FAIL $what: no $fragment in"
    echo "$headers"
    exit 1
  fi
  echo "ok       $what"
  passed=$((passed + 1))
}

# ---- reading ---------------------------------------------------------------------------------

check 200 "list, empty at start" '[]' -- "$BASE/notes"
check 404 "a note that does not exist" '"status":404' -- "$BASE/notes/999"
check_header "errors are problem+json" "content-type: application/problem+json" -- "$BASE/notes/999"
check 422 "a path that is not a number" -- "$BASE/notes/abc"

# ---- writing ---------------------------------------------------------------------------------

check 401 "create without a token" -- -X POST "$BASE/notes" -H "$JSON" -d '{"text":"hello"}'
check 401 "create with an unknown token" -- -X POST "$BASE/notes" -H "$JSON" \
  -H "Authorization: Bearer nobody" -d '{"text":"hello"}'
check 422 "create with an empty text" '"errors"' -- -X POST "$BASE/notes" -H "$BOB" -H "$JSON" \
  -d '{"text":""}'
check 422 "create with a text over 280 characters" -- -X POST "$BASE/notes" -H "$BOB" -H "$JSON" \
  -d "{\"text\":\"$(printf 'x%.0s' $(seq 281))\"}"
check 422 "create with malformed JSON" -- -X POST "$BASE/notes" -H "$BOB" -H "$JSON" -d '{"text":'
check 201 "create as bob" '"author":"bob"' -- -X POST "$BASE/notes" -H "$BOB" -H "$JSON" \
  -d '{"text":"hello"}'
id=$(sed -E 's/.*"id":([0-9]+).*/\1/' <<<"$body")

check 200 "read it back" '"text":"hello"' -- "$BASE/notes/$id"
check 200 "its text as plain text" 'hello' -- "$BASE/notes/$id/text"
check 200 "listed" "\"id\":$id" -- "$BASE/notes"

check 200 "edit as the author" '"text":"hello, edited"' -- -X PATCH "$BASE/notes/$id" -H "$BOB" \
  -H "$JSON" -d '{"text":"hello, edited"}'
check 200 "an empty edit changes nothing" '"text":"hello, edited"' -- -X PATCH "$BASE/notes/$id" \
  -H "$BOB" -H "$JSON" -d '{}'
check 403 "edit as somebody else" -- -X PATCH "$BASE/notes/$id" -H "$ALICE" -H "$JSON" \
  -d '{"text":"mine now"}'
check 404 "edit a note that does not exist" -- -X PATCH "$BASE/notes/999" -H "$BOB" -H "$JSON" \
  -d '{"text":"x"}'

check 200 "the tidy prompt" '"messages"' -- "$BASE/prompts/tidy/$id?audience=the%20team"

# ---- deleting --------------------------------------------------------------------------------

check 403 "delete without notes:delete (bob)" -- -X DELETE "$BASE/notes/$id" -H "$BOB"
check 204 "delete as alice" -- -X DELETE "$BASE/notes/$id" -H "$ALICE"
check 404 "gone" -- "$BASE/notes/$id"
check 404 "delete twice" -- -X DELETE "$BASE/notes/$id" -H "$ALICE"

# ---- documentation ---------------------------------------------------------------------------

check 200 "OpenAPI document" '"openapi":"3.1' -- "$BASE/openapi.json"
check 200 "Scalar" '<html' -- "$BASE/docs"
check 200 "Swagger UI" '<html' -- "$BASE/swagger"
check 404 "an unknown route" -- "$BASE/nowhere"
check 405 "a method the route does not have" -- -X PUT "$BASE/notes"

# ---- MCP (2025-11-25, no session needed) -----------------------------------------------------

mcp() { # METHOD PARAMS
  printf '{"jsonrpc":"2.0","id":1,"method":"%s","params":%s}' "$1" "$2"
}
MCP_VERSION="MCP-Protocol-Version: 2025-11-25"

check 200 "MCP initialize" '"serverInfo"' -- -X POST "$BASE/mcp" -H "$JSON" \
  -d "$(mcp initialize '{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"verify","version":"0"}}')"
check 200 "MCP tools/list, no delete" '"create_note"' -- -X POST "$BASE/mcp" -H "$JSON" \
  -H "$MCP_VERSION" -d "$(mcp tools/list '{}')"
[[ $body != *delete* ]] || { echo "FAIL delete is exposed over MCP"; exit 1; }
check 200 "MCP create_note as bob" '"author\":\"bob' -- -X POST "$BASE/mcp" -H "$JSON" \
  -H "$MCP_VERSION" -H "$BOB" \
  -d "$(mcp tools/call '{"name":"create_note","arguments":{"text":"from an agent"}}')"
check 401 "MCP create_note without a token" -- -X POST "$BASE/mcp" -H "$JSON" -H "$MCP_VERSION" \
  -d "$(mcp tools/call '{"name":"create_note","arguments":{"text":"x"}}')"

echo "all $passed checks passed"
