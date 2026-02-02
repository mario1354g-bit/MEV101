#!/bin/bash
# =============================================================================
# RETH MEV NODE STARTUP SCRIPT
# =============================================================================
#
# Prerequisites:
#   1. Install reth: curl -L https://reth.rs/install.sh | bash
#   2. NVMe SSD with 1TB+ space
#   3. 32GB+ RAM recommended
#   4. Good internet connection
#
# Usage:
#   chmod +x start-reth-mev.sh
#   ./start-reth-mev.sh
#
# =============================================================================

set -e

# Configuration
DATADIR="${RETH_DATADIR:-$HOME/.local/share/reth/mainnet}"
CONFIG_FILE="$(dirname "$0")/reth-mev-config.toml"
LOG_LEVEL="${LOG_LEVEL:-info}"
AUTHRPC_JWTSECRET="${JWT_SECRET:-$HOME/.local/share/reth/jwt.hex}"

# Colors for output
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
NC='\033[0m' # No Color

echo -e "${GREEN}=============================================${NC}"
echo -e "${GREEN}  RETH MEV NODE - Starting...${NC}"
echo -e "${GREEN}=============================================${NC}"

# Check if reth is installed
if ! command -v reth &> /dev/null; then
    echo -e "${RED}Error: reth not found. Install with:${NC}"
    echo "curl -L https://reth.rs/install.sh | bash"
    exit 1
fi

# Show reth version
echo -e "${YELLOW}Reth version:${NC}"
reth --version

# Create JWT secret if not exists
if [ ! -f "$AUTHRPC_JWTSECRET" ]; then
    echo -e "${YELLOW}Creating JWT secret...${NC}"
    mkdir -p "$(dirname "$AUTHRPC_JWTSECRET")"
    openssl rand -hex 32 > "$AUTHRPC_JWTSECRET"
fi

# Check disk space
AVAILABLE_SPACE=$(df -BG "$HOME" | tail -1 | awk '{print $4}' | tr -d 'G')
if [ "$AVAILABLE_SPACE" -lt 500 ]; then
    echo -e "${RED}Warning: Less than 500GB available. Reth needs ~1TB for full sync.${NC}"
fi

# Check RAM
TOTAL_RAM=$(free -g | awk '/^Mem:/{print $2}')
if [ "$TOTAL_RAM" -lt 16 ]; then
    echo -e "${RED}Warning: Less than 16GB RAM. 32GB+ recommended for MEV.${NC}"
fi

echo -e "${YELLOW}Data directory:${NC} $DATADIR"
echo -e "${YELLOW}Config file:${NC} $CONFIG_FILE"
echo -e "${YELLOW}Log level:${NC} $LOG_LEVEL"
echo ""

# Build the command
RETH_CMD="reth node"

# Add config file if exists
if [ -f "$CONFIG_FILE" ]; then
    RETH_CMD="$RETH_CMD --config $CONFIG_FILE"
else
    echo -e "${YELLOW}Warning: Config file not found, using defaults${NC}"
fi

# Core settings
RETH_CMD="$RETH_CMD \
    --datadir $DATADIR \
    --log.file.directory $DATADIR/logs \
    --authrpc.jwtsecret $AUTHRPC_JWTSECRET \
    --authrpc.addr 0.0.0.0 \
    --authrpc.port 8551"

# HTTP RPC (for standard RPC calls)
RETH_CMD="$RETH_CMD \
    --http \
    --http.addr 0.0.0.0 \
    --http.port 8545 \
    --http.api eth,net,web3,txpool,debug,trace,reth"

# WebSocket (for subscriptions - critical for MEV)
RETH_CMD="$RETH_CMD \
    --ws \
    --ws.addr 0.0.0.0 \
    --ws.port 8546 \
    --ws.api eth,net,web3,txpool,debug,trace,reth"

# Transaction pool settings (MEV optimized)
RETH_CMD="$RETH_CMD \
    --txpool.pending-max 20000 \
    --txpool.basefee-max 20000 \
    --txpool.queued-max 20000"

# Metrics (optional but useful)
RETH_CMD="$RETH_CMD \
    --metrics 0.0.0.0:9001"

# Log level
RETH_CMD="$RETH_CMD -vvv"

echo -e "${GREEN}Starting reth with MEV configuration...${NC}"
echo ""
echo -e "${YELLOW}RPC Endpoints:${NC}"
echo "  HTTP:      http://localhost:8545"
echo "  WebSocket: ws://localhost:8546"
echo "  IPC:       /tmp/reth.ipc"
echo "  Auth RPC:  http://localhost:8551 (for consensus client)"
echo "  Metrics:   http://localhost:9001/metrics"
echo ""
echo -e "${YELLOW}Useful commands:${NC}"
echo "  Check sync:    curl -s localhost:8545 -X POST -H 'Content-Type: application/json' -d '{\"jsonrpc\":\"2.0\",\"method\":\"eth_syncing\",\"params\":[],\"id\":1}'"
echo "  Pending txs:   curl -s localhost:8545 -X POST -H 'Content-Type: application/json' -d '{\"jsonrpc\":\"2.0\",\"method\":\"txpool_status\",\"params\":[],\"id\":1}'"
echo "  Latest block:  curl -s localhost:8545 -X POST -H 'Content-Type: application/json' -d '{\"jsonrpc\":\"2.0\",\"method\":\"eth_blockNumber\",\"params\":[],\"id\":1}'"
echo ""
echo -e "${GREEN}=============================================${NC}"
echo ""

# Run reth
exec $RETH_CMD
