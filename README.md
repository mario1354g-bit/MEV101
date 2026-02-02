# Long-Tail MEV Monitor & Executor

A Rust-based MEV (Maximal Extractable Value) bot focused on long-tail opportunities: obscure token pairs, multi-hop arbitrage, sandwich attacks, backruns, and liquidations.

## Features

- **Price Discrepancy Detection**: Cross-DEX arbitrage on Uniswap V2/V3, SushiSwap
- **Multi-Hop Arbitrage**: 3-4 hop paths using Bellman-Ford negative cycle detection
- **Sandwich Attacks**: Mempool monitoring with optimal frontrun calculation
- **Backrunning**: Capture price reversion after large swaps
- **Liquidations**: Monitor Aave V3, Compound V3, Euler, Morpho
- **Flashloan Support**: Balancer (0% fee), Aave V3 (0.09% fee)
- **Flashbots Integration**: Bundle submission for MEV protection

## Requirements

- Rust 1.70+
- Erigon/Geth full node with WebSocket enabled
- ~0.1 ETH for gas (testing only initially)

## Quick Start

### 1. Configure Environment

```bash
cd /home/ubuntu/Desktop/longtail-mev-monitor

# Edit .env with your settings
cat > .env << 'EOF'
PRIVATE_KEY=your_private_key_here
ETHEREUM_HTTP_RPC=http://localhost:8545
ETHEREUM_WS_RPC=ws://localhost:8546
FLASHBOTS_RELAY_URL=https://relay.flashbots.net
EOF
```

### 2. Configure Settings

Edit `config.toml`:

```toml
[ethereum]
http_rpc = "http://localhost:8545"  # Your Erigon node
ws_rpc = "ws://localhost:8546"
chain_id = 1

[execution]
enabled = false          # Start with monitoring only!
dry_run = true          # Simulate but don't submit
min_profit_eth = 0.005  # Minimum profit threshold
```

### 3. Build & Run

```bash
# Build release binary
cargo build --release

# Run the bot
./target/release/longtail-mev-monitor
```

### 4. View Dashboard

Open http://localhost:3000 in your browser.

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                     MEV Bot Architecture                     │
├─────────────────────────────────────────────────────────────┤
│  Monitors           │  Detectors         │  Executors        │
│  ─────────          │  ─────────         │  ─────────        │
│  • Block            │  • Price Disc.     │  • Arbitrage      │
│  • Mempool          │  • Multi-hop       │  • Flashloan      │
│  • Price            │  • Sandwich        │  • Sandwich       │
│  • Liquidity        │  • Liquidity       │  • Backrun        │
│  • Liquidation      │                    │  • Liquidation    │
├─────────────────────────────────────────────────────────────┤
│  Simulation Engine  │  Storage (SQLite)  │  Dashboard (Axum) │
└─────────────────────────────────────────────────────────────┘
```

## Smart Contracts

Before live execution, deploy the contracts:

```bash
# Contracts are in /contracts/
# - FlashloanArbitrage.sol
# - SandwichExecutor.sol

# Deploy using Foundry or Hardhat
forge create contracts/FlashloanArbitrage.sol:FlashloanArbitrage --rpc-url $RPC
```

## Safety

**CRITICAL: Start with monitoring only!**

1. Set `execution.enabled = false` initially
2. Collect data for 1-2 weeks
3. Analyze opportunities in the dashboard
4. Only enable execution after understanding the landscape

**Never risk more than you can afford to lose.**

## Configuration Reference

| Setting | Default | Description |
|---------|---------|-------------|
| `execution.enabled` | `false` | Enable live execution |
| `execution.dry_run` | `true` | Simulate without submitting |
| `execution.min_profit_eth` | `0.005` | Minimum profit threshold |
| `monitoring.min_spread` | `0.003` | 0.3% minimum spread |
| `monitoring.max_hops` | `4` | Maximum arbitrage hops |

## Realistic Expectations

- **90%+ of detected opportunities won't be capturable**
- **Competition exists even on long-tail pairs**
- **Most profit goes to builders (90%+) via Flashbots**
- **This is a research tool first, execution tool second**

## License

MIT
