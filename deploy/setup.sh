#!/usr/bin/env bash
# Server bootstrap script — run once on a fresh Ubuntu 22.04 / Debian 12
# Usage: sudo bash setup.sh
set -euo pipefail

JULIA_VERSION="1.11.2"
TRADING_USER="trading"
TRADING_DIR="/opt/trading"
LOG_DIR="/var/log/trading"
JULIA_DIR="/usr/local/julia"

echo "=== ZetaField Trading System Setup ==="

# ── System dependencies ───────────────────────────────────────────────────────
apt-get update -qq
apt-get install -y --no-install-recommends \
    curl wget git build-essential pkg-config \
    libssl-dev libzmq3-dev \
    ca-certificates logrotate

# ── Create trading user ───────────────────────────────────────────────────────
if ! id "$TRADING_USER" &>/dev/null; then
    useradd --system --shell /bin/bash --home-dir "$TRADING_DIR" \
            --create-home "$TRADING_USER"
    echo "Created user: $TRADING_USER"
fi

# ── Julia ─────────────────────────────────────────────────────────────────────
if [[ ! -f "$JULIA_DIR/bin/julia" ]]; then
    ARCH=$(uname -m)
    [[ "$ARCH" == "x86_64" ]] && JULIA_ARCH="x86_64" || JULIA_ARCH="aarch64"
    JULIA_URL="https://julialang-s3.julialang.org/bin/linux/${JULIA_ARCH}/$(echo $JULIA_VERSION | cut -d. -f1-2)/julia-${JULIA_VERSION}-linux-${JULIA_ARCH}.tar.gz"

    wget -q "$JULIA_URL" -O /tmp/julia.tar.gz
    tar -xzf /tmp/julia.tar.gz -C /usr/local
    mv "/usr/local/julia-${JULIA_VERSION}" "$JULIA_DIR"
    rm /tmp/julia.tar.gz
    ln -sf "$JULIA_DIR/bin/julia" /usr/local/bin/julia
    echo "Julia $JULIA_VERSION installed"
fi

# ── Rust ──────────────────────────────────────────────────────────────────────
if ! command -v cargo &>/dev/null; then
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | \
        sh -s -- -y --default-toolchain stable --profile minimal
    source "$HOME/.cargo/env"
    echo "Rust installed"
fi

# ── Directories ───────────────────────────────────────────────────────────────
mkdir -p "$LOG_DIR" "$TRADING_DIR/deploy/scripts"
chown -R "$TRADING_USER:$TRADING_USER" "$LOG_DIR" "$TRADING_DIR"
chmod 750 "$LOG_DIR"

# ── Log rotation ──────────────────────────────────────────────────────────────
cat > /etc/logrotate.d/trading << 'EOF'
/var/log/trading/*.jsonl {
    daily
    rotate 90
    compress
    delaycompress
    missingok
    notifempty
    create 0640 trading trading
}
/var/log/trading/*.log {
    weekly
    rotate 52
    compress
    delaycompress
    missingok
    notifempty
    create 0640 trading trading
}
EOF

# ── systemd units ─────────────────────────────────────────────────────────────
cp "$TRADING_DIR/deploy/zeta.target"            /etc/systemd/system/
cp "$TRADING_DIR/deploy/zeta-engine.service"    /etc/systemd/system/
cp "$TRADING_DIR/deploy/zeta-executor.service"  /etc/systemd/system/
chmod +x "$TRADING_DIR/deploy/scripts/"*.sh

systemctl daemon-reload
systemctl enable zeta.target zeta-engine.service zeta-executor.service
echo "systemd units installed and enabled"

# ── Julia packages pre-compilation ───────────────────────────────────────────
echo "Pre-compiling Julia packages (this takes a few minutes on first run)..."
sudo -u "$TRADING_USER" julia --project="$TRADING_DIR/zeta" -e "
using Pkg
Pkg.instantiate()
Pkg.precompile()
println(\"Julia packages ready\")
"

# ── Rust build ────────────────────────────────────────────────────────────────
echo "Building Rust executor..."
cd "$TRADING_DIR/executor"
sudo -u "$TRADING_USER" cargo build --release
cp target/release/executor "$TRADING_DIR/executor/zeta-executor"
echo "Rust executor built"

echo ""
echo "=== Setup complete ==="
echo ""
echo "Next steps:"
echo "  1. Copy .env.example to /opt/trading/.env and fill in API keys"
echo "  2. systemctl start zeta.target"
echo "  3. journalctl -fu zeta-engine   # watch Julia logs"
echo "  4. journalctl -fu zeta-executor # watch Rust logs"
