#!/bin/sh
set -eu

case "$PORT" in
    ''|0*|*[!0-9]*) echo 'PORT must be an integer from 1024 to 65535' >&2; exit 1 ;;
esac
if [ "${#PORT}" -gt 5 ] || [ "$PORT" -lt 1024 ] || [ "$PORT" -gt 65535 ]; then
    echo 'PORT must be an integer from 1024 to 65535' >&2
    exit 1
fi
for hostname in "$WALLET_HOST" "$EXPLORER_HOST"; do
    case "$hostname" in
        ''|*[!a-zA-Z0-9.-]*|.*|*.|*..*)
            echo 'WALLET_HOST and EXPLORER_HOST must be individual DNS hostnames' >&2
            exit 1 ;;
    esac
done
WALLET_HOST=$(printf '%s' "$WALLET_HOST" | tr '[:upper:]' '[:lower:]')
EXPLORER_HOST=$(printf '%s' "$EXPLORER_HOST" | tr '[:upper:]' '[:lower:]')
if [ "$WALLET_HOST" = "$EXPLORER_HOST" ]; then
    echo 'WALLET_HOST and EXPLORER_HOST must be different' >&2
    exit 1
fi
export PORT WALLET_HOST EXPLORER_HOST

mkdir -p /tmp/mmx-apps
# Limit substitution to our settings; preserve nginx's $uri, $status, etc.
# shellcheck disable=SC2016
envsubst '${PORT} ${WALLET_HOST} ${EXPLORER_HOST}' \
    < /etc/nginx/mmx/nginx.conf.template > /tmp/mmx-apps/nginx.conf
exec "$@"
