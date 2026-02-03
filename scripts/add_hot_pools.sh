#!/bin/bash
# Add hot pools to FlashloanArbitrage contract as approved routers
#
# Usage: ./scripts/add_hot_pools.sh
#
# Requires:
# - PRIVATE_KEY env var set
# - RPC_URL env var set (or uses Alchemy default)

set -e

# Contract address
FLASHLOAN_CONTRACT="0xF53bEFDe7B7631BA5749499FB33Fd145372976bA"

# RPC URL (use env or default to Alchemy)
RPC_URL="${RPC_URL:-https://eth-mainnet.g.alchemy.com/v2/4Lkxgt7ISPaJFuHh-_3vn}"

# Check private key
if [ -z "$PRIVATE_KEY" ]; then
    echo "Error: PRIVATE_KEY env var not set"
    echo "Run: source .env && ./scripts/add_hot_pools.sh"
    exit 1
fi

echo "Adding hot pools to FlashloanArbitrage contract..."
echo "Contract: $FLASHLOAN_CONTRACT"
echo ""

# Function to add router
add_router() {
    local addr=$1
    local name=$2
    echo "Adding $name ($addr)..."
    cast send $FLASHLOAN_CONTRACT \
        "setApprovedRouter(address,bool)" \
        $addr true \
        --rpc-url $RPC_URL \
        --private-key $PRIVATE_KEY \
        --gas-limit 100000
    echo "  Done"
}

# ============= UNISWAP V3 POOLS =============
echo "=== Uniswap V3 Pools ==="

# USDC/WETH
add_router "0x88e6A0c2dDD26FEEb64F039a2c41296FcB3f5640" "UniV3 USDC/WETH 0.05%"
add_router "0x8ad599c3A0ff1De082011EFDDc58f1908eb6e6D8" "UniV3 USDC/WETH 0.3%"

# WBTC/WETH
add_router "0x4585FE77225b41b697C938B018E2Ac67Ac5a20c0" "UniV3 WBTC/WETH 0.05%"
add_router "0xCBCdF9626bC03E24f779434178A73a0B4bad62eD" "UniV3 WBTC/WETH 0.3%"

# WBTC/USDT
add_router "0x9Db9e0e53058C89e5B94e29621a205198648425B" "UniV3 WBTC/USDT 0.3%"

# WETH/USDT
add_router "0x11b815efB8f581194ae79006d24E0d814B7697F6" "UniV3 WETH/USDT 0.05%"
add_router "0x4e68Ccd3E89f51C3074ca5072bbAC773960dFa36" "UniV3 WETH/USDT 0.3%"

# AAVE
add_router "0x5aB53EE1d50eeF2C1DD3d5402789cd27bB52c1bB" "UniV3 AAVE/WETH 0.3%"

# ============= UNISWAP V3 ROUTERS =============
echo ""
echo "=== Uniswap V3 Routers ==="
add_router "0xE592427A0AEce92De3Edee1F18E0157C05861564" "UniV3 SwapRouter"
add_router "0x68b3465833fb72A70ecDF485E0e4C7bD8665Fc45" "UniV3 SwapRouter02"

# ============= UNISWAP V2 ROUTERS =============
echo ""
echo "=== Uniswap V2 Routers ==="
add_router "0x7a250d5630B4cF539739dF2C5dAcb4c659F2488D" "UniV2 Router"

# ============= SUSHISWAP =============
echo ""
echo "=== SushiSwap ==="
add_router "0xd9e1cE17f2641f24aE83637ab66a2cca9C378B9F" "SushiSwap Router"

# ============= CURVE =============
echo ""
echo "=== Curve ==="
add_router "0xbEbc44782C7dB0a1A60Cb6fe97d0b483032FF1C7" "Curve 3pool"

# ============= RING V2 =============
echo ""
echo "=== Ring V2 ==="
add_router "0x147D15e009a63Ebed5196EA029679329204f98fd" "Ring V2 fwWETH/fwUSDT"

# ============= AAVE POOLS =============
echo ""
echo "=== Aave V3 Pools ==="
cast send $FLASHLOAN_CONTRACT \
    "setApprovedAavePool(address,bool)" \
    "0x87870Bca3F3fD6335C3F4ce8392D69350B4fA4E2" true \
    --rpc-url $RPC_URL \
    --private-key $PRIVATE_KEY \
    --gas-limit 100000
echo "Added Aave V3 Pool"

echo ""
echo "=== All hot pools added successfully! ==="
