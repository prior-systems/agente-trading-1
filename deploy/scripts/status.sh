#!/usr/bin/env bash
# Quick status check for both services + OMS state
echo "=== ZetaField System Status ==="
echo ""

systemctl is-active zeta-engine   && echo "✓ Julia engine:   RUNNING" || echo "✗ Julia engine:   STOPPED"
systemctl is-active zeta-executor && echo "✓ Rust executor:  RUNNING" || echo "✗ Rust executor:  STOPPED"

echo ""
echo "--- Recent OMS events ---"
tail -5 /var/log/trading/oms.jsonl 2>/dev/null | python3 -c "
import sys, json
for line in sys.stdin:
    try:
        e = json.loads(line)
        print(f\"  {e.get('ts','?')[:19]}  {e.get('event_type','?')}\")
    except: pass
" || echo "  (no OMS log found)"

echo ""
echo "--- Last 3 Julia log lines ---"
journalctl -u zeta-engine --no-pager -n 3 --output=short-iso 2>/dev/null || true

echo ""
echo "--- Last 3 Executor log lines ---"
journalctl -u zeta-executor --no-pager -n 3 --output=short-iso 2>/dev/null || true
