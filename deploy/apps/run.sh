#!/bin/sh
# tini forwards signals to the entire process group and reaps orphaned workers.
set -eu
haproxy -db -f /tmp/mmx-apps/haproxy.cfg &
front=$!
"$@" &
origin=$!
stop() {
    kill -TERM "$front" "$origin" 2>/dev/null || true
}
trap stop INT TERM EXIT
# BusyBox ash's wait -n exits when either owned service stops. Never leave a
# healthy-looking container running with only one half of its HTTP path alive.
status=0
# shellcheck disable=SC3045
wait -n || status=$?
stop
wait "$front" 2>/dev/null || true
wait "$origin" 2>/dev/null || true
trap - INT TERM EXIT
exit "$status"
