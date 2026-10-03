#!/bin/sh
# Start the OpenID Connect provider and the example wired to it, in one command:
#
#   ./start.sh
#
# The provider (mock-oauth2-server) runs in Docker and stays up after the example stops
# (Ctrl-C); `./stop.sh` stops both. Every variable below can be overridden from the environment
# (`LESTO_PORT=9000 ./start.sh`). See README.md for what each one does.
set -eu

cd "$(dirname "$0")"

docker compose up -d

# The example reads the discovery document at startup and refuses to start without it.
export LESTO_OIDC_DISCOVERY_URL=${LESTO_OIDC_DISCOVERY_URL:-http://localhost:8089/default/.well-known/openid-configuration}
export LESTO_OIDC_AUDIENCES=${LESTO_OIDC_AUDIENCES:-api://notes}
export LESTO_OIDC_CLIENTS=${LESTO_OIDC_CLIENTS:-notes-cli}
export LESTO_PORT=${LESTO_PORT:-8765}

printf 'waiting for the provider'
ready=""
for _ in $(seq 60); do
  if curl -fsS -o /dev/null "$LESTO_OIDC_DISCOVERY_URL" 2>/dev/null; then
    ready=yes
    break
  fi
  printf .
  sleep 1
done
if [ -z "$ready" ]; then
  echo
  echo "the provider did not answer on $LESTO_OIDC_DISCOVERY_URL; see: docker compose logs oauth2" >&2
  exit 1
fi
echo " ready"

cat <<INFO

provider  ${LESTO_OIDC_DISCOVERY_URL%/.well-known/*}
API       http://127.0.0.1:$LESTO_PORT (docs at /docs)

Try:      curl -i 127.0.0.1:$LESTO_PORT/health
          curl -i 127.0.0.1:$LESTO_PORT/me          # 401: no token
Check:    BASE=http://127.0.0.1:$LESTO_PORT ./verify.sh   (in another terminal)
Stop:     Ctrl-C, then ./stop.sh

INFO

exec cargo run -q -p oidc-example
