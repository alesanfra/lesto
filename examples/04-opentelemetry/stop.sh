#!/bin/sh
# Stop what start.sh started:
#
#   ./stop.sh           # the example (if still running) and the backends; collected data is kept
#   ./stop.sh --clean   # the same, and remove OpenObserve's volume: the next start is empty
#
# Jaeger keeps its traces in memory, so they are gone either way.
set -eu

cd "$(dirname "$0")"

case ${1:-} in
  "") down="docker compose down" ;;
  --clean) down="docker compose down -v" ;;
  *)
    echo "usage: $0 [--clean]" >&2
    exit 2
    ;;
esac

# Only this example's binary, whatever profile it was built with. SIGTERM: it flushes the
# telemetry still buffered before it exits.
if pkill -TERM -f '/target/[^/]+/opentelemetry-example' 2>/dev/null; then
  echo "stopping the example"
  for _ in $(seq 50); do
    pgrep -f '/target/[^/]+/opentelemetry-example' > /dev/null || break
    sleep 0.1
  done
fi

$down
