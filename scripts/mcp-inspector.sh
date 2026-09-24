#!/bin/sh
# The MCP endpoint of examples/02-notes checked with the MCP Inspector's CLI: a second client
# next to rmcp (tests/mcp_rmcp.rs), built on the TypeScript SDK, which is stricter about tool
# schemas (it silently drops a tool whose schema it rejects). In both protocol eras:
#
#   - tools/list --strict: the four tools, no portability warning;
#   - tools/call create_note with a token: the note, as structured content;
#   - tools/call update_note with an empty text: a tool error (the 422);
#   - tools/call create_note without a token: the 401 reaches the client, which asks for OAuth.
#
# Needs Node 22.19+ (npx) and jq. Usage: sh scripts/mcp-inspector.sh
# Environment: CARGO_PROFILE (default dev), LESTO_PORT (default 8765),
# MCP_INSPECTOR_VERSION (default below; bump it on purpose, like the docs assets).
set -eu

profile=${CARGO_PROFILE:-dev}
port=${LESTO_PORT:-8765}
version=${MCP_INSPECTOR_VERSION:-2.8.0}
url="http://127.0.0.1:$port/mcp"
work=$(mktemp -d)

case $profile in
    dev) dir=debug ;;
    *) dir=$profile ;;
esac

cargo build -q --profile "$profile" -p notes
LESTO_PORT=$port "target/$dir/notes" >"$work/server.log" 2>&1 &
server=$!
trap 'kill $server 2>/dev/null || true; rm -rf "$work"' EXIT INT TERM

tries=0
until curl -sf "http://127.0.0.1:$port/notes" >/dev/null; do
    tries=$((tries + 1))
    if [ $tries -gt 60 ]; then
        echo "notes did not start on port $port:" >&2
        cat "$work/server.log" >&2
        exit 1
    fi
    sleep 1
done

fail() {
    echo "FAIL ($era): $1" >&2
    shift
    for file in "$@"; do
        echo "--- $file" >&2
        cat "$file" >&2
    done
    exit 1
}

# inspector <name> <args..>: stdout to $work/<name>.out, stderr (npm noise removed) to .err.
inspector() {
    name=$1
    shift
    status=0
    npx -y "@modelcontextprotocol/inspector@$version" --cli "$url" --protocol-era "$era" \
        --format json "$@" >"$work/$name.out" 2>"$work/$name.raw" || status=$?
    grep -v '^npm warn' "$work/$name.raw" >"$work/$name.err" || true
    return $status
}

for era in legacy modern; do
    inspector list --method tools/list --strict ||
        fail "tools/list exited with an error" "$work/list.out" "$work/list.err"
    if grep -qi 'warning\|error' "$work/list.err"; then
        fail "tools/list --strict reported problems" "$work/list.err"
    fi
    names=$(head -n 1 "$work/list.out" | jq -r '[.result.tools[].name] | join(",")')
    [ "$names" = "list_notes,get_note,create_note,update_note" ] ||
        fail "unexpected tools: $names" "$work/list.out"

    inspector create --header "Authorization: Bearer bob-token" \
        --method tools/call --tool-name create_note --tool-arg "text=from the $era inspector" ||
        fail "create_note failed" "$work/create.out" "$work/create.err"
    author=$(head -n 1 "$work/create.out" | jq -r '.result.structuredContent.author')
    [ "$author" = "bob" ] || fail "create_note returned author '$author'" "$work/create.out"

    inspector invalid --header "Authorization: Bearer bob-token" \
        --method tools/call --tool-name update_note --tool-args-json '{"id": 1, "text": ""}' || true
    is_error=$(head -n 1 "$work/invalid.out" | jq -r '.result.isError')
    [ "$is_error" = "true" ] || fail "update_note with an empty text is not a tool error" "$work/invalid.out"
    grep -q '422' "$work/invalid.out" || fail "the tool error does not carry the 422" "$work/invalid.out"

    # --stored-auth-only: fail instead of opening a browser for the OAuth flow.
    inspector anonymous --stored-auth-only \
        --method tools/call --tool-name create_note --tool-arg text=anonymous || true
    # The CLI reports this error on stderr.
    code=$(cat "$work/anonymous.out" "$work/anonymous.err" | grep '^{' | head -n 1 |
        jq -r '.error.code // empty')
    [ "$code" = "auth_required" ] ||
        fail "a call without a token did not ask for authentication" "$work/anonymous.out" "$work/anonymous.err"

    echo "ok ($era): 4 tools, strict schemas, call, tool error, 401"
done
