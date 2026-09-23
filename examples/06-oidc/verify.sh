#!/bin/sh
# End to end against mock-oauth2-server: get tokens with the client-credentials grant, call the
# example, check 200 / 401 / 403. Needs `docker compose up -d` and the example running on
# $BASE (default http://127.0.0.1:8765) with the variables of README.md.
set -eu

BASE=${BASE:-http://127.0.0.1:8765}
TOKEN_URL=${TOKEN_URL:-http://localhost:8089/default/token}

token() {
  curl -fsS -X POST "$TOKEN_URL" \
    -d grant_type=client_credentials -d client_id=notes-cli -d client_secret=secret \
    -d "scope=$1" | tr -d '\n' | sed -E 's/.*"access_token" *: *"([^"]+)".*/\1/'
}

check() { # expected-status description curl-args...
  expected=$1; what=$2; shift 2
  status=$(curl -s -o /dev/null -w '%{http_code}' "$@")
  if [ "$status" = "$expected" ]; then
    echo "ok   $status $what"
  else
    echo "FAIL $status $what (expected $expected)"; exit 1
  fi
}

reader=$(token notes:read)
admin=$(token admin)

check 200 "public route, no token"          "$BASE/health"
check 401 "Jwt argument, no token"          "$BASE/me"
check 401 "Jwt argument, forged token"      -H "Authorization: Bearer $reader.x" "$BASE/me"
check 200 "Jwt argument, reader token"      -H "Authorization: Bearer $reader" "$BASE/me"
check 401 "protected app, no token"         "$BASE/admin/stats"
check 403 "protected app, reader token"     -H "Authorization: Bearer $reader" "$BASE/admin/stats"
check 200 "protected app, admin token"      -H "Authorization: Bearer $admin" "$BASE/admin/stats"
curl -fsS -H "Authorization: Bearer $admin" "$BASE/me"; echo
