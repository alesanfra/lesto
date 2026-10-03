#!/bin/sh
# Stop what start.sh started: the example (if still running) and the provider. The provider
# keeps nothing on disk, so there is nothing else to clean.
set -eu

cd "$(dirname "$0")"

if [ $# -gt 0 ]; then
  echo "usage: $0" >&2
  exit 2
fi

# Only this example's binary, whatever profile it was built with.
if pkill -TERM -f '/target/[^/]+/oidc-example' 2>/dev/null; then
  echo "stopping the example"
  for _ in $(seq 50); do
    pgrep -f '/target/[^/]+/oidc-example' > /dev/null || break
    sleep 0.1
  done
fi

docker compose down
