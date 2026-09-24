#!/bin/sh
# The MCP endpoint of examples/02-notes checked with the MCP Inspector's CLI: a second client
# next to rmcp (tests/mcp_rmcp.rs), built on the TypeScript SDK, which is stricter about tool
# schemas (it silently drops a tool whose schema it rejects). In both protocol eras:
#
#   - tools/list --strict: the four tools, no portability warning;
#   - tools/call create_note with a token: the note, as structured content;
#   - resources/templates/list and resources/read: the note's text, read through its URI;
#   - prompts/list and prompts/get: the tidy_note prompt, with the note's text in it;
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
    echo "  FAIL  $1" >&2
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

pass() {
    echo "  ok    $1"
}

for era in legacy modern; do
    case $era in
        legacy) echo "MCP 2025-11-25 (legacy, opens with initialize):" ;;
        modern) echo "MCP 2026-07-28 (stateless):" ;;
    esac
    inspector list --method tools/list --strict ||
        fail "tools/list exited with an error" "$work/list.out" "$work/list.err"
    if grep -qi 'warning\|error' "$work/list.err"; then
        fail "tools/list --strict reported problems" "$work/list.err"
    fi
    names=$(head -n 1 "$work/list.out" | jq -r '[.result.tools[].name] | join(",")')
    [ "$names" = "list_notes,get_note,create_note,update_note" ] ||
        fail "unexpected tools: $names" "$work/list.out"
    pass "tools/list shows the 4 tools, no --strict warning"

    inspector create --header "Authorization: Bearer bob-token" \
        --method tools/call --tool-name create_note --tool-arg "text=from the $era inspector" ||
        fail "create_note failed" "$work/create.out" "$work/create.err"
    author=$(head -n 1 "$work/create.out" | jq -r '.result.structuredContent.author')
    [ "$author" = "bob" ] || fail "create_note returned author '$author'" "$work/create.out"
    id=$(head -n 1 "$work/create.out" | jq -r '.result.structuredContent.id')
    pass "create_note with bob's token creates the note"

    inspector templates --method resources/templates/list ||
        fail "resources/templates/list failed" "$work/templates.out" "$work/templates.err"
    template=$(head -n 1 "$work/templates.out" | jq -r '.result.resourceTemplates[0].uriTemplate')
    [ "$template" = "lesto://notes/notes/{id}/text" ] ||
        fail "unexpected resource template: $template" "$work/templates.out"
    inspector read --method resources/read --uri "lesto://notes/notes/$id/text" ||
        fail "resources/read failed" "$work/read.out" "$work/read.err"
    text=$(head -n 1 "$work/read.out" | jq -r '.result.contents[0].text')
    [ "$text" = "from the $era inspector" ] || fail "resources/read returned '$text'" "$work/read.out"
    pass "resources/read of the note_text template returns the note's text"

    inspector prompts --method prompts/list ||
        fail "prompts/list failed" "$work/prompts.out" "$work/prompts.err"
    prompt=$(head -n 1 "$work/prompts.out" | jq -r '[.result.prompts[].name] | join(",")')
    [ "$prompt" = "tidy_note" ] || fail "unexpected prompts: $prompt" "$work/prompts.out"
    inspector prompt --method prompts/get --prompt-name tidy_note \
        --prompt-args "id=$id" "audience=the team" ||
        fail "prompts/get failed" "$work/prompt.out" "$work/prompt.err"
    head -n 1 "$work/prompt.out" | jq -r '.result.messages[0].content.text' | grep -q "from the $era inspector" ||
        fail "prompts/get does not carry the note" "$work/prompt.out"
    pass "prompts/get tidy_note returns a message with the note in it"

    inspector invalid --header "Authorization: Bearer bob-token" \
        --method tools/call --tool-name update_note --tool-args-json '{"id": 1, "text": ""}' || true
    is_error=$(head -n 1 "$work/invalid.out" | jq -r '.result.isError')
    [ "$is_error" = "true" ] || fail "update_note with an empty text is not a tool error" "$work/invalid.out"
    grep -q '422' "$work/invalid.out" || fail "the tool error does not carry the 422" "$work/invalid.out"
    pass "update_note with an empty text is refused: tool error with the validation problem"

    # --stored-auth-only: fail instead of opening a browser for the OAuth flow.
    inspector anonymous --stored-auth-only \
        --method tools/call --tool-name create_note --tool-arg text=anonymous || true
    # The CLI reports this error on stderr.
    code=$(cat "$work/anonymous.out" "$work/anonymous.err" | grep '^{' | head -n 1 |
        jq -r '.error.code // empty')
    [ "$code" = "auth_required" ] ||
        fail "a call without a token did not ask for authentication" "$work/anonymous.out" "$work/anonymous.err"
    pass "create_note without a token is refused (as it should): the client is asked to log in"
done
echo "All checks passed."

