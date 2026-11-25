#!/bin/sh
set -eu

: "${NEXDESK_HOST:?set the TURN realm}"
: "${NEXDESK_TURN_SECRET:?set the TURN shared secret}"
case "$NEXDESK_TURN_SECRET" in *[!A-Za-z0-9_-]*) exit 2 ;; esac
[ "${#NEXDESK_TURN_SECRET}" -ge 32 ] || exit 2

umask 077
config=$(mktemp /tmp/nexdesk-turn.XXXXXX)
cat /etc/nexdesk/turnserver.conf > "$config"
printf 'realm=%s\nstatic-auth-secret=%s\n' "$NEXDESK_HOST" "$NEXDESK_TURN_SECRET" >> "$config"
if [ -n "${NEXDESK_TURN_BIND_IP:-}" ]; then
    printf 'listening-ip=%s\nrelay-ip=%s\n' "$NEXDESK_TURN_BIND_IP" "$NEXDESK_TURN_BIND_IP" >> "$config"
fi
if [ -n "${NEXDESK_TURN_EXTERNAL_IP:-}" ]; then
    printf 'external-ip=%s\n' "$NEXDESK_TURN_EXTERNAL_IP" >> "$config"
fi
unset NEXDESK_TURN_SECRET
if [ "${NEXDESK_TURN_METRICS:-0}" = 1 ]; then
    exec turnserver -c "$config" --prometheus --prometheus-address 127.0.0.1
fi
exec turnserver -c "$config"
