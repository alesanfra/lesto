#!/bin/sh
# Start a backend and the example wired to it, in one command:
#
#   ./start.sh                 # OpenObserve: traces, logs and metrics, UI on :5080
#   ./start.sh jaeger          # Jaeger: traces only, UI on :16686
#
# The backend runs in Docker and stays up after the example stops (Ctrl-C), so a restart keeps
# what it collected; `./stop.sh` stops it, `./stop.sh --clean` removes its data too. Every
# variable below can be overridden from the environment (`LESTO_PORT=9000 ./start.sh`). See
# README.md for what each one does.
set -eu

cd "$(dirname "$0")"
backend=${1:-openobserve}

wait_for() { # URL NAME
  printf 'waiting for %s' "$2"
  for _ in $(seq 60); do
    if curl -fsS -o /dev/null "$1" 2>/dev/null; then
      echo " ready"
      return
    fi
    printf .
    sleep 1
  done
  echo
  echo "$2 did not answer on $1; see: docker compose logs $backend" >&2
  exit 1
}

case $backend in
  openobserve)
    docker compose up -d openobserve
    wait_for http://localhost:5080/healthz OpenObserve
    export OTEL_EXPORTER_OTLP_ENDPOINT=${OTEL_EXPORTER_OTLP_ENDPOINT:-http://localhost:5080/api/default}
    # base64("root@example.com:Complexpass#123"), the demo credentials of compose.yaml
    export OTEL_EXPORTER_OTLP_HEADERS=${OTEL_EXPORTER_OTLP_HEADERS:-Authorization=Basic cm9vdEBleGFtcGxlLmNvbTpDb21wbGV4cGFzcyMxMjM=}
    # Metrics every 10 s instead of 60, so they show up while you look.
    export OTEL_METRIC_EXPORT_INTERVAL=${OTEL_METRIC_EXPORT_INTERVAL:-10000}
    ui="http://localhost:5080 (root@example.com / Complexpass#123)"
    ;;
  jaeger)
    docker compose up -d jaeger
    wait_for http://localhost:16686 Jaeger
    export OTEL_EXPORTER_OTLP_ENDPOINT=${OTEL_EXPORTER_OTLP_ENDPOINT:-http://localhost:4318}
    # Jaeger answers 404 on /v1/logs and /v1/metrics: export traces only.
    export OTEL_LOGS_EXPORTER=none
    export OTEL_METRICS_EXPORTER=none
    ui="http://localhost:16686"
    ;;
  *)
    echo "usage: $0 [openobserve|jaeger]" >&2
    exit 2
    ;;
esac

export OTEL_SERVICE_NAME=${OTEL_SERVICE_NAME:-lesto-demo}
export LESTO_PORT=${LESTO_PORT:-8000}

cat <<EOF

backend   $backend, UI at $ui
service   $OTEL_SERVICE_NAME
API       http://localhost:$LESTO_PORT (docs at /docs)

Try:      curl localhost:$LESTO_PORT/notes
          curl localhost:$LESTO_PORT/boom
EOF
if [ "$backend" = openobserve ]; then
  echo "Check:    ./verify.sh   (in another terminal)"
fi
echo "Stop:     Ctrl-C, then ./stop.sh (--clean to drop the data)"
echo

exec cargo run -q -p opentelemetry-example
