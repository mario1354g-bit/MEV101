# Long-Tail MEV Monitor

High-performance Rust MEV bot for detecting and executing arbitrage opportunities across decentralized exchanges. Monitors 57 trading pairs across Uniswap V2, SushiSwap, ShibaSwap, and Fraxswap with real-time profit estimation and flash loan execution.

## Live Dashboard

![Dashboard](https://img.shields.io/badge/Dashboard-Live-brightgreen)

Real-time web dashboard at `http://localhost:8080` showing:
- Active arbitrage opportunities with profit estimates (USD/ETH)
- Type classification (ARB, SANDWICH, LIQUIDATION, BACKRUN)
- Spread percentages and trading routes
- Simulation status (Pending/Verified/Failed)
- Top pairs and best spreads charts

## Features

### Detection
- **57 Trading Pairs** across 4 DEXes (UniV2, Sushi, Shiba, Frax)
- **Real-time Price Monitoring** via Alchemy WebSocket
- **Cross-DEX Arbitrage** detection with spread calculation
- **False Positive Filtering** for bad data and extreme spreads
- **High Priority Alerts** for >0.5% spread opportunities

### Execution
- **Flash Loan Support**: Balancer (0% fee), Aave V3 (0.05% fee)
- **Flashbots Integration** for MEV protection
- **Local Simulation** via REVM before execution
- **Gas Optimization** with dynamic pricing

### Infrastructure
- **Alchemy RPC** integration (HTTP + WebSocket)
- **Reth Node** support for local simulation
- **SQLite Storage** for opportunity tracking
- **Axum Dashboard** with 5-second auto-refresh

## Quick Start

### 1. Clone & Build

```bash
git clone https://github.com/mario1354g-bit/longtail-mev-monitor.git
cd longtail-mev-monitor
cargo build --release
```

### 2. Configure

Edit `config.toml`:

```toml
[ethereum]
http_rpc_url = "https://eth-mainnet.g.alchemy.com/v2/YOUR_KEY"
ws_rpc_url = "wss://eth-mainnet.g.alchemy.com/v2/YOUR_KEY"
chain_id = 1

[execution]
enabled = true
dry_run = false  # Set true for monitoring only
min_profit_eth = 0.01

[dashboard]
enabled = true
host = "127.0.0.1"
port = 8080
```

### 3. Set Private Key

```bash
export PRIVATE_KEY="your_private_key_here"
```

### 4. Run

```bash
./target/release/longtail-mev-monitor
```

### 5. View Dashboard

Open http://127.0.0.1:8080

## Architecture

```
┌────────────────────────────────────────────────────────────────┐
│                    MEV Monitor Architecture                     │
├────────────────────────────────────────────────────────────────┤
│                                                                 │
│  ┌─────────────┐    ┌─────────────┐    ┌─────────────┐        │
│  │   Alchemy   │───▶│  DEX Price  │───▶│ Opportunity │        │
│  │  WebSocket  │    │   Scanner   │    │  Detector   │        │
│  └─────────────┘    └─────────────┘    └─────────────┘        │
│         │                                      │               │
│         ▼                                      ▼               │
│  ┌─────────────┐    ┌─────────────┐    ┌─────────────┐        │
│  │   Block     │    │    REVM     │◀───│  Flashloan  │        │
│  │  Monitor    │    │  Simulator  │    │  Executor   │        │
│  └─────────────┘    └─────────────┘    └─────────────┘        │
│                            │                   │               │
│                            ▼                   ▼               │
│                     ┌─────────────┐    ┌─────────────┐        │
│                     │  Dashboard  │    │  Flashbots  │        │
│                     │   (Axum)    │    │   Bundle    │        │
│                     └─────────────┘    └─────────────┘        │
│                                                                 │
└────────────────────────────────────────────────────────────────┘
```

## DEX Coverage

| DEX | Router | Pairs |
|-----|--------|-------|
| Uniswap V2 | 0x7a250d5630B4cF539739dF2C5dAcb4c659F2488D | 57 |
| SushiSwap | 0xd9e1cE17f2641f24aE83637ab66a2cca9C378B9F | 57 |
| ShibaSwap | 0x03f7724180AA6b939894B5Ca4314783B0b36b329 | 57 |
| Fraxswap | 0xC14d550632db8592D1243Edc8B95b0Ad06703867 | 57 |

## Profit Calculation

Dashboard estimates profit assuming 100 ETH flash loan:

```
Spread: 0.9%
Flash Loan: 100 ETH (Balancer, 0% fee)
Gross Profit: 0.9 ETH
Gas Cost: ~0.02 ETH
Net Profit: ~0.88 ETH (~$2,200)
```

## Flash Loan Execution

Deploy the FlashloanArbitrage contract:

```bash
cd contracts
forge create FlashloanArbitrage --rpc-url $RPC_URL --private-key $PRIVATE_KEY
```

Add to config:

```toml
[flashloan]
enabled = true
arbitrage_contract = "0xYOUR_CONTRACT_ADDRESS"
preferred_provider = "balancer"  # 0% fee
```

## Local Simulation (Reth)

For accurate simulation before execution:

```bash
# Start reth with MEV config
./start-reth-mev.sh

# Or manually
reth node --http --http.api eth,net,web3,debug,trace
```

## Configuration Reference

| Setting | Default | Description |
|---------|---------|-------------|
| `ethereum.http_rpc_url` | - | Alchemy HTTP endpoint |
| `ethereum.ws_rpc_url` | - | Alchemy WebSocket endpoint |
| `execution.enabled` | `true` | Enable execution |
| `execution.dry_run` | `false` | Simulate only |
| `execution.min_profit_eth` | `0.01` | Min profit threshold |
| `dashboard.port` | `8080` | Dashboard port |

## Safety

**Start with `dry_run = true` to monitor without executing.**

- Opportunities shown are estimates until simulated
- Gas costs vary with network congestion
- Competition from other MEV bots is fierce
- Never risk more than you can afford to lose

## Requirements

- Rust 1.70+
- Alchemy API key (paid tier for WebSocket)
- ~0.1 ETH for gas (execution)
- Optional: Reth node for local simulation

## License

MIT
