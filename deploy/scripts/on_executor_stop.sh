#!/usr/bin/env bash
# Called by systemd ExecStopPost when zeta-executor dies or is stopped.
# Logs the event and sends an alert.
set -euo pipefail

LOG="/var/log/trading/system.log"
TS=$(date -u +"%Y-%m-%dT%H:%M:%SZ")
EXIT_CODE="${EXIT_CODE:-unknown}"

echo "$TS executor stopped (exit_code=$EXIT_CODE)" >> "$LOG"

# Alert via Telegram if bot token is configured
if [[ -n "${TELEGRAM_BOT_TOKEN:-}" && -n "${TELEGRAM_CHAT_ID:-}" ]]; then
    MSG="⚠️ zeta-executor stopped at $TS (exit=$EXIT_CODE). Check open positions manually."
    curl -s -X POST "https://api.telegram.org/bot${TELEGRAM_BOT_TOKEN}/sendMessage" \
        -d chat_id="$TELEGRAM_CHAT_ID" \
        -d text="$MSG" \
        > /dev/null || true
fi

# Write shutdown event to OMS log so replay knows system went down
OMS_LOG="${OMS_LOG_PATH:-/var/log/trading/oms.jsonl}"
echo "{\"event_type\":\"SystemStop\",\"ts\":\"$TS\",\"reason\":\"systemd ExecStopPost exit_code=$EXIT_CODE\"}" >> "$OMS_LOG" || true
