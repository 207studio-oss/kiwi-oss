#!/bin/sh
# Keep kiwi-live running on the box (no systemd/cron here). Idempotent: a second copy exits via flock.
# Started from ~/.profile and ~/.bashrc so any new shell after a box restart revives it.
HERE=$(cd "$(dirname "$0")" && pwd)
LOG="$HOME/.kiwi/state/kiwi-live.log"; mkdir -p "$HOME/.kiwi/state"
exec 9>"$HOME/.kiwi/state/kiwi-live.lock"
flock -n 9 || exit 0
# Webhook egress proxy for tailnet subscribers (tailscaled --outbound-http-proxy-listen); value lives in ~/.kiwi/webhook-proxy.
[ -n "$KIWI_WEBHOOK_PROXY" ] || KIWI_WEBHOOK_PROXY=$(cat "$HOME/.kiwi/webhook-proxy" 2>/dev/null); export KIWI_WEBHOOK_PROXY
while :; do
  [ "$(stat -c %s "$LOG" 2>/dev/null || echo 0)" -gt 5000000 ] && mv -f "$LOG" "$LOG.1"
  python3 "$HERE/kiwi_live.py" --root "$HOME/kiwi-main" --host 127.0.0.1 --port 8765 >>"$LOG" 2>&1
  echo "kiwi-live exited $?; restarting in 5s" >>"$LOG"; sleep 5
done
