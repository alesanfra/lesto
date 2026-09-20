#!/bin/sh
# Throughput and latency percentiles over a real socket, as a check on `benches/overhead.rs`:
# that one measures the framework with no networking, this one measures what a client sees.
#
# Needs `oha` (cargo install oha). Usage: sh scripts/bench-http.sh [seconds] [connections]
set -eu

duration=${1:-10}
connections=${2:-50}
port=${LESTO_BENCH_PORT:-8791}

command -v oha >/dev/null 2>&1 || { echo "oha is not installed: cargo install oha" >&2; exit 1; }

cargo build --release -p hello
LESTO_PORT="$port" ./target/release/hello &
server=$!
trap 'kill "$server" 2>/dev/null || true' EXIT INT TERM

# Wait for the port to answer before measuring anything.
i=0
while [ "$i" -lt 100 ]; do
    if curl -fsS "http://127.0.0.1:$port/hello" >/dev/null 2>&1; then break; fi
    i=$((i + 1))
    sleep 0.1
done

echo "== GET /hello =="
oha -z "${duration}s" -c "$connections" --no-tui "http://127.0.0.1:$port/hello"
echo "== GET /missing (404) =="
oha -z "${duration}s" -c "$connections" --no-tui "http://127.0.0.1:$port/missing"
