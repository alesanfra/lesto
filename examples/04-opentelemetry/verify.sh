#!/bin/sh
# End-to-end check of the OpenObserve option: send a few requests to the example and ask
# OpenObserve for the spans and the log records it received.
#
#   docker compose up -d openobserve
#   cargo run -p opentelemetry-example        # in another terminal, see README.md
#   ./verify.sh
set -eu

API=${API:-http://localhost:8000}
O2=${O2:-http://localhost:5080}
AUTH=${AUTH:-cm9vdEBleGFtcGxlLmNvbTpDb21wbGV4cGFzcyMxMjM=}   # root@example.com:Complexpass#123
SERVICE=${OTEL_SERVICE_NAME:-lesto-demo}

echo "sending requests to $API"
curl -fsS "$API/hello" > /dev/null
curl -fsS -X POST "$API/notes" -H 'content-type: application/json' -d '{"text":"from verify.sh"}' > /dev/null
curl -fsS "$API/notes" > /dev/null
curl -sS -o /dev/null "$API/boom"
curl -sS -o /dev/null "$API/broken"

# The batch exporter flushes on a schedule; give it a moment, and OpenObserve a moment to index.
echo "waiting for the spans and logs to be exported and indexed"
sleep 40

# The last 15 minutes for this service, newest first.
now=$(date +%s)
from=$(( (now - 900) * 1000000 ))
to=$(( (now + 60) * 1000000 ))
search() {   # search <traces|logs> <sql>
  query=$(printf '{"query":{"sql":"%s","start_time":%s,"end_time":%s,"from":0,"size":20}}' "$2" "$from" "$to")
  curl -fsS -X POST "$O2/api/default/_search?type=$1" \
    -H "Authorization: Basic $AUTH" \
    -H 'content-type: application/json' \
    -d "$query"
  echo
}

echo "--- spans"
search traces "SELECT operation_name, span_status, http_response_status_code, db_system_name, duration FROM default WHERE service_name = '$SERVICE' ORDER BY start_time DESC"

# Every log record carries the trace and span it happened in.
echo "--- logs"
search logs "SELECT body, severity, trace_id, span_id FROM default WHERE service_name = '$SERVICE' ORDER BY _timestamp DESC"
