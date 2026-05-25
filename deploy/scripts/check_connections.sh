#!/usr/bin/env bash
# check_connections.sh — verifica credenciales y conectividad antes de arrancar
# Uso: bash deploy/scripts/check_connections.sh
#      bash deploy/scripts/check_connections.sh --live   (usa cuentas live en lugar de sandbox/demo)

set -uo pipefail

# ── Colores ───────────────────────────────────────────────────────────────────
GREEN='\033[0;32m'; RED='\033[0;31m'; YELLOW='\033[1;33m'
CYAN='\033[0;36m';  BOLD='\033[1m';   NC='\033[0m'

ok()   { echo -e "  ${GREEN}✓${NC}  $1"; }
fail() { echo -e "  ${RED}✗${NC}  $1"; ERRORS=$((ERRORS + 1)); }
info() { echo -e "  ${YELLOW}→${NC}  $1"; }
section() { echo -e "\n${CYAN}${BOLD}$1${NC}"; echo "  $(printf '%.0s─' {1..44})"; }

ERRORS=0
LIVE_MODE=false
[[ "${1:-}" == "--live" ]] && LIVE_MODE=true

# ── Cargar .env ───────────────────────────────────────────────────────────────
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
ENV_FILE="$ROOT/.env"

if [ -f "$ENV_FILE" ]; then
    set -a; source "$ENV_FILE"; set +a
    info "Cargado $ENV_FILE"
else
    info "No se encontró .env — usando variables de entorno existentes"
fi

# ── JSON helper (python3 o jq) ────────────────────────────────────────────────
json_get() {
    local json="$1" key="$2"
    if command -v jq &>/dev/null; then
        echo "$json" | jq -r ".$key // empty" 2>/dev/null
    else
        echo "$json" | python3 -c \
            "import sys, json; d=json.load(sys.stdin); print(d.get('$key',''))" 2>/dev/null
    fi
}

require_var() {
    local var="$1"
    if [ -z "${!var:-}" ]; then
        fail "$var no está definida"
        return 1
    fi
    return 0
}

# ─────────────────────────────────────────────────────────────────────────────

echo ""
echo -e "${BOLD}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
echo -e "${BOLD}  Zeta Field — verificación de conexiones         ${NC}"
echo -e "${BOLD}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
$LIVE_MODE && echo -e "  ${RED}⚠  MODO LIVE — usando cuentas reales${NC}"

# ── 1. Variables requeridas ───────────────────────────────────────────────────
section "1. Variables de entorno"

ALL_VARS=(
    DATABENTO_API_KEY ANTHROPIC_API_KEY
    TRADIER_TOKEN TRADIER_ACCOUNT_ID
    TRADOVATE_USERNAME TRADOVATE_PASSWORD TRADOVATE_CID TRADOVATE_SECRET
)
for v in "${ALL_VARS[@]}"; do
    if require_var "$v"; then
        # Mask value — show first 4 chars only
        val="${!v}"
        masked="${val:0:4}$(printf '*%.0s' {1..12})"
        ok "$v = $masked"
    fi
done

[ "$ERRORS" -gt 0 ] && echo -e "\n${RED}Faltan variables requeridas — corrige .env antes de continuar.${NC}" && exit 1

# ── 2. Tradier ────────────────────────────────────────────────────────────────
section "2. Tradier (opciones + acciones)"

if $LIVE_MODE; then
    TRADIER_BASE="https://api.tradier.com/v1"
    info "Usando cuenta LIVE"
else
    TRADIER_BASE="${TRADIER_SANDBOX:+https://sandbox.tradier.com/v1}"
    TRADIER_BASE="${TRADIER_BASE:-https://sandbox.tradier.com/v1}"
    info "Usando sandbox (TRADIER_SANDBOX=${TRADIER_SANDBOX:-true})"
fi

TRADIER_RESP=$(curl -sf \
    -H "Authorization: Bearer $TRADIER_TOKEN" \
    -H "Accept: application/json" \
    "$TRADIER_BASE/accounts/$TRADIER_ACCOUNT_ID/balances" 2>&1) && TRADIER_OK=true || TRADIER_OK=false

if $TRADIER_OK && echo "$TRADIER_RESP" | grep -q "balances"; then
    OBP=$(json_get "$(echo "$TRADIER_RESP" | python3 -c \
        "import sys,json; d=json.load(sys.stdin); print(json.dumps(d.get('balances',{})))" \
        2>/dev/null)" "option_buying_power")
    ok "Autenticado — account $TRADIER_ACCOUNT_ID"
    [ -n "$OBP" ] && ok "Option buying power: \$$OBP"
else
    fail "Tradier: respuesta inesperada"
    info "Preview: ${TRADIER_RESP:0:200}"
fi

# ── 3. Tradovate ──────────────────────────────────────────────────────────────
section "3. Tradovate (futuros)"

if $LIVE_MODE; then
    TRADOVATE_BASE="https://live.tradovate.com/v1"
    info "Usando cuenta LIVE"
else
    TRADOVATE_BASE="https://demo.tradovate.com/v1"
    info "Usando demo (TRADOVATE_DEMO=${TRADOVATE_DEMO:-true})"
fi

TRADOVATE_AUTH=$(curl -sf \
    -X POST "$TRADOVATE_BASE/auth/accesstokenrequest" \
    -H "Content-Type: application/json" \
    -d "{
        \"name\":\"$TRADOVATE_USERNAME\",
        \"password\":\"$TRADOVATE_PASSWORD\",
        \"appId\":\"ZetaField\",
        \"appVersion\":\"0.1.0\",
        \"cid\":$TRADOVATE_CID,
        \"sec\":\"$TRADOVATE_SECRET\",
        \"deviceId\":\"${TRADOVATE_DEVICE_ID:-$(python3 -c 'import uuid; print(uuid.uuid4())')}\"
    }" 2>&1) && TRAD_AUTH_OK=true || TRAD_AUTH_OK=false

if $TRAD_AUTH_OK; then
    USER_STATUS=$(json_get "$TRADOVATE_AUTH" "userStatus")
    ACCESS_TOKEN=$(json_get "$TRADOVATE_AUTH" "accessToken")

    if [ "$USER_STATUS" = "Active" ] && [ -n "$ACCESS_TOKEN" ]; then
        ok "Autenticado — userStatus: $USER_STATUS"

        # Fetch account list
        ACCOUNTS=$(curl -sf \
            -H "Authorization: Bearer $ACCESS_TOKEN" \
            "$TRADOVATE_BASE/account/list" 2>&1)
        ACCT_NAME=$(echo "$ACCOUNTS" | python3 -c \
            "import sys,json; a=json.load(sys.stdin); print(a[0]['name'] if a else '')" 2>/dev/null)
        ACCT_ID=$(echo "$ACCOUNTS" | python3 -c \
            "import sys,json; a=json.load(sys.stdin); print(a[0]['id'] if a else '')" 2>/dev/null)

        if [ -n "$ACCT_NAME" ]; then
            ok "Cuenta: $ACCT_NAME (id=$ACCT_ID)"
        else
            fail "No se encontraron cuentas en la respuesta"
        fi
    else
        fail "Tradovate auth: userStatus='$USER_STATUS'"
        info "Revisa TRADOVATE_CID y TRADOVATE_SECRET en la developer portal"
        info "Preview: ${TRADOVATE_AUTH:0:300}"
    fi
else
    fail "Tradovate: no se pudo conectar a $TRADOVATE_BASE"
    info "Preview: ${TRADOVATE_AUTH:0:200}"
fi

# ── 4. Anthropic ──────────────────────────────────────────────────────────────
section "4. Anthropic API (LLM agent)"

ANTHROPIC_RESP=$(curl -sf \
    -X POST "https://api.anthropic.com/v1/messages" \
    -H "x-api-key: $ANTHROPIC_API_KEY" \
    -H "anthropic-version: 2023-06-01" \
    -H "Content-Type: application/json" \
    -d '{
        "model": "claude-haiku-4-5-20251001",
        "max_tokens": 8,
        "messages": [{"role": "user", "content": "ping"}]
    }' 2>&1) && ANT_OK=true || ANT_OK=false

if $ANT_OK && echo "$ANTHROPIC_RESP" | grep -q "content"; then
    MODEL=$(json_get "$ANTHROPIC_RESP" "model")
    ok "Autenticado — model: $MODEL"
else
    HTTP_STATUS=$(echo "$ANTHROPIC_RESP" | grep -o '"type":"[^"]*"' | head -1)
    fail "Anthropic: respuesta inesperada ($HTTP_STATUS)"
    info "Preview: ${ANTHROPIC_RESP:0:200}"
fi

# ── 5. ThetaData ──────────────────────────────────────────────────────────────
section "5. ThetaData (option chain + Greeks)"
# ThetaData v3 runs as a local terminal at 127.0.0.1:25503 — no auth in HTTP requests

THETA_RESP=$(curl -sf --max-time 5 \
    "http://127.0.0.1:25503/v3/bulk_snapshot/option/greeks?root=SPY&exp=0&use_csv=false" \
    -H "Accept: application/json" 2>&1) && THETA_OK=true || THETA_OK=false

if $THETA_OK && echo "$THETA_RESP" | grep -qE "header|response"; then
    ok "Theta Terminal corriendo y respondiendo"
elif echo "$THETA_RESP" | grep -qi "refused\|connect"; then
    fail "ThetaData: Theta Terminal no está corriendo en 127.0.0.1:25503"
    info "Inicia el Theta Terminal antes de arrancar el sistema"
else
    fail "ThetaData: respuesta inesperada del terminal local"
    info "Preview: ${THETA_RESP:0:200}"
fi

# ── 6. Databento ──────────────────────────────────────────────────────────────
section "6. Databento (CME futures MBO)"

DATABENTO_RESP=$(curl -sf \
    "https://hist.databento.com/v0/metadata.list_schemas?dataset=GLBX.MDP3" \
    -u "$DATABENTO_API_KEY:" 2>&1) && DB_OK=true || DB_OK=false

if $DB_OK && echo "$DATABENTO_RESP" | grep -q "mbo\|mbp\|trades"; then
    ok "Databento conectado — GLBX.MDP3 accesible"
    info "Schemas: $(echo "$DATABENTO_RESP" | python3 -c \
        "import sys,json; d=json.load(sys.stdin); print(', '.join(d[:5]))" 2>/dev/null)"
elif echo "$DATABENTO_RESP" | grep -qi "unauthorized\|forbidden"; then
    fail "Databento: API key inválida"
else
    fail "Databento: respuesta inesperada"
    info "Preview: ${DATABENTO_RESP:0:200}"
fi

# ── 7. ZMQ socket path ────────────────────────────────────────────────────────
section "7. ZeroMQ IPC"

ZMQ_EP="${ZMQ_ENDPOINT:-ipc:///tmp/zeta.sock}"
ZMQ_PATH="${ZMQ_EP#ipc://}"

if [ -d "$(dirname "$ZMQ_PATH")" ]; then
    ok "Socket path disponible: $ZMQ_PATH"
    if [ -S "$ZMQ_PATH" ]; then
        info "Socket ya existe — executor posiblemente corriendo"
    fi
else
    fail "Directorio no existe: $(dirname "$ZMQ_PATH")"
fi

# ── Resumen ───────────────────────────────────────────────────────────────────
echo ""
echo -e "${BOLD}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
if [ "$ERRORS" -eq 0 ]; then
    echo -e "  ${GREEN}${BOLD}✓ Todo OK — listo para arrancar${NC}"
    echo ""
    echo "  Para iniciar el sistema:"
    echo "    sudo systemctl start zeta.target"
    echo "  O en desarrollo:"
    echo "    cd zeta && julia --project=. src/server.jl &"
    echo "    cd executor && RUST_LOG=info cargo run"
else
    echo -e "  ${RED}${BOLD}✗ $ERRORS error(s) — corrige antes de arrancar${NC}"
fi
echo -e "${BOLD}━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━${NC}"
echo ""

exit "$ERRORS"
