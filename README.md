# Longtail MEV Bot

High-performance Rust MEV bot implementing Paradigm's Artemis architecture for detecting and executing arbitrage, sandwich, and liquidation opportunities on Ethereum mainnet.

## Architecture

```
┌─────────────────────────────────────────────────────────────────────────┐
│                         Artemis Architecture                             │
├─────────────────────────────────────────────────────────────────────────┤
│                                                                          │
│   COLLECTORS              STRATEGIES              EXECUTORS              │
│   ───────────             ──────────              ─────────              │
│   ┌─────────┐            ┌──────────┐           ┌───────────┐           │
│   │ Mempool │───────────▶│ Sandwich │──────────▶│ Flashbots │           │
│   └─────────┘            └──────────┘           └───────────┘           │
│   ┌─────────┐            ┌──────────┐           ┌───────────┐           │
│   │  Block  │───────────▶│Arbitrage │──────────▶│  Direct   │           │
│   └─────────┘            └──────────┘           └───────────┘           │
│   ┌─────────┐            ┌──────────┐                                   │
│   │  Swap   │───────────▶│Liquidate │                                   │
│   │ Events  │            └──────────┘                                   │
│   └─────────┘                                                           │
│        │                       │                       │                │
│        └───────────────────────┼───────────────────────┘                │
│                                ▼                                        │
│                    ┌────────────────────┐                               │
│                    │   REVM Simulator   │                               │
│                    │  ┌──────────────┐  │                               │
│                    │  │  Warm Cache  │  │                               │
│                    │  │   Fork DB    │  │                               │
│                    │  │   Parallel   │  │                               │
│                    │  └──────────────┘  │                               │
│                    └────────────────────┘                               │
│                                                                          │
└─────────────────────────────────────────────────────────────────────────┘
```

## Features

### MEV Strategies
- **Sandwich Attacks** - Frontrun/backrun large swaps with REVM-simulated profit calculation
- **Cross-DEX Arbitrage** - Price discrepancy detection across Uniswap V2/V3, SushiSwap, 0x
- **Multi-hop Arbitrage** - Triangular and n-hop arbitrage path discovery
- **Liquidations** - Lending protocol liquidation opportunities (Aave, Compound)

### Simulation Engine
- **REVM Local Simulation** - Full EVM execution without RPC latency
- **Fork DB** - Lazy state loading from RPC with caching
- **Warm Cache** - Pre-loaded pool reserves for sub-millisecond simulation
- **Parallel Simulator** - Concurrent opportunity evaluation
- **Mempool Simulation** - Simulate pending transactions to predict state

### Execution
- **Flashbots Bundles** - MEV-protected submission to block builders
- **Flash Loans** - Balancer (0% fee) and Aave V3 (0.05% fee) integration
- **Direct Execution** - Standard transaction submission fallback
- **Gas Optimization** - Dynamic gas pricing with priority fee management

### Collectors
- **Mempool Monitor** - Pending transaction stream via WebSocket
- **Block Monitor** - New block notifications and state updates
- **Swap Event Collector** - Real-time DEX swap event parsing

### Infrastructure
- **Alloy** - Modern Rust Ethereum library (replaces ethers-rs)
- **SQLite** - Opportunity tracking and analytics
- **Axum Dashboard** - Real-time web UI with live stats
- **Multi-threaded Runtime** - Optimized tokio runtime for low latency

## Smart Contracts

Two Solidity contracts deployed on mainnet:

| Contract | Address | Purpose |
|----------|---------|---------|
| FlashloanArbitrage | `0xF53bEFDe7B7631BA5749499FB33Fd145372976bA` | Flash loan arbitrage execution |
| SandwichExecutor | `0xD39E9b7905fC6C947Bc8D2790d758EeD109EBCA3` | Sandwich attack execution |

## Quick Start

### 1. Clone & Build

```bash
git clone https://github.com/mario1354g-bit/longtail-mev-monitor.git
cd longtail-mev-monitor
cargo build --release
```

### 2. Configure

Copy and edit the config file:

```bash
cp .env.example .env
# Edit .env with your private key and RPC URLs
```

Key environment variables:
```bash
PRIVATE_KEY=your_private_key_here
ETH_RPC_URL=https://eth-mainnet.g.alchemy.com/v2/YOUR_KEY
ETH_WS_URL=wss://eth-mainnet.g.alchemy.com/v2/YOUR_KEY
```

Edit `config.toml` for detailed settings:

```toml
[monitoring]
artemis_mode = true      # Enable Artemis architecture
mempool_enabled = true   # Monitor pending transactions
min_profit_eth = 0.005   # Minimum profit threshold

[execution]
enabled = true
dry_run = true           # Set false for live execution
use_flashbots = true     # Submit via Flashbots relay

[simulation]
enabled = true
parallel = true
workers = 4
```

### 3. Run

```bash
# Monitoring mode (dry run)
./target/release/longtail-mev-monitor

# With dashboard
./target/release/longtail-mev-monitor --features dashboard
```

### 4. Dashboard

Open http://127.0.0.1:8080 for real-time monitoring:
- Live opportunity feed
- Profit/loss tracking
- Gas statistics
- Pool activity heatmap

## Project Structure

```
src/
├── artemis/           # Artemis engine orchestration
│   ├── collector.rs   # Collector trait
│   ├── strategy.rs    # Strategy trait
│   ├── executor.rs    # Executor trait
│   └── engine.rs      # Event processing pipeline
├── collectors/        # Data collection
│   ├── mempool.rs     # Pending tx monitoring
│   ├── block.rs       # Block notifications
│   └── swap_events.rs # DEX swap parsing
├── strategies/        # MEV detection
│   ├── sandwich.rs    # Sandwich attack logic
│   ├── arbitrage.rs   # Cross-DEX arbitrage
│   └── liquidation.rs # Liquidation detection
├── simulation/        # Transaction simulation
│   ├── revm_simulator.rs  # REVM execution
│   ├── fork_db.rs         # State forking
│   ├── warm_cache.rs      # Reserve caching
│   ├── swap_simulator.rs  # Swap simulation
│   └── parallel.rs        # Concurrent simulation
├── executors/         # Transaction execution
│   ├── flashbots.rs   # Flashbots bundle submission
│   └── direct.rs      # Direct tx submission
├── detectors/         # Opportunity detection
│   ├── sandwich_detector.rs
│   ├── price_discrepancy.rs
│   ├── multihop.rs
│   └── liquidity_event.rs
├── dex/               # DEX integrations
│   ├── uniswap_v2.rs
│   ├── uniswap_v3.rs
│   └── pool_registry.rs
└── storage/           # Database layer
contracts/
├── FlashloanArbitrage.sol  # Flash loan execution
└── SandwichExecutor.sol    # Sandwich execution
```

## DEX Support

| DEX | Type | Router/Factory |
|-----|------|----------------|
| Uniswap V2 | AMM | `0x7a250d5630B4cF539739dF2C5dAcb4c659F2488D` |
| Uniswap V3 | Concentrated | `0xE592427A0AEce92De3Edee1F18E0157C05861564` |
| SushiSwap | AMM | `0xd9e1cE17f2641f24aE83637ab66a2cca9C378B9F` |
| 0x | Aggregator | `0xDef1C0ded9bec7F1a1670819833240f027b25EfF` |

## Configuration Reference

### Monitoring

| Setting | Default | Description |
|---------|---------|-------------|
| `artemis_mode` | `true` | Use Artemis architecture |
| `mempool_enabled` | `true` | Monitor pending transactions |
| `block_enabled` | `true` | Monitor new blocks |
| `min_profit_eth` | `0.005` | Minimum profit threshold |
| `max_gas_price_gwei` | `500` | Maximum gas price |

### Execution

| Setting | Default | Description |
|---------|---------|-------------|
| `enabled` | `true` | Enable execution |
| `dry_run` | `true` | Simulate only |
| `use_flashbots` | `true` | Use Flashbots relay |
| `slippage_tolerance` | `0.5` | Slippage % |
| `max_priority_fee_gwei` | `3` | Max tip |

### Simulation

| Setting | Default | Description |
|---------|---------|-------------|
| `enabled` | `true` | Enable simulation |
| `parallel` | `true` | Parallel simulation |
| `workers` | `4` | Simulation threads |
| `timeout_ms` | `5000` | Simulation timeout |

## Local Development

### Run with Reth Node

For lowest latency simulation:

```bash
# Start reth with MEV configuration
./start-reth-mev.sh

# Or manually
reth node \
  --http --http.api eth,net,web3,debug,trace \
  --ws --ws.api eth,net,web3,debug,trace
```

### Deploy Contracts

```bash
cd contracts
forge create FlashloanArbitrage \
  --rpc-url $ETH_RPC_URL \
  --private-key $PRIVATE_KEY

forge create SandwichExecutor \
  --rpc-url $ETH_RPC_URL \
  --private-key $PRIVATE_KEY
```

## Safety

**Start with `dry_run = true` to monitor without executing.**

- All opportunities are simulated before execution
- Flashbots provides MEV protection (no failed tx on-chain)
- Gas costs and slippage are factored into profit calculations
- Competition from other MEV searchers is intense
- Never risk more than you can afford to lose

## Requirements

- Rust 1.75+
- Alchemy/Infura API key (paid tier for WebSocket + mempool)
- ETH for gas (~0.1 ETH minimum)
- Optional: Local reth/geth node for simulation

## License

MIT
