# Security and Code Audit: Monitors and Detectors Modules

**Audit Date:** 2026-02-02
**Auditor:** Claude Opus 4.5
**Scope:** src/monitors/ and src/detectors/ directories

---

## Executive Summary

This audit reviews the monitors and detectors modules of the longtail-mev-monitor Rust MEV project. The codebase demonstrates solid MEV fundamentals but contains several critical bugs, missing error handling, and deviations from production-grade MEV bot best practices as documented in the research materials.

**Severity Summary:**
- Critical: 4
- High: 8
- Medium: 12
- Low: 15
- Informational: 10

---

## Table of Contents

1. [Critical Issues](#1-critical-issues)
2. [High Severity Issues](#2-high-severity-issues)
3. [Medium Severity Issues](#3-medium-severity-issues)
4. [Low Severity Issues](#4-low-severity-issues)
5. [Missing Features](#5-missing-features)
6. [Performance Improvements](#6-performance-improvements)
7. [Best Practice Deviations](#7-best-practice-deviations)

---

## 1. Critical Issues

### 1.1 Unsafe Raw Pointer Usage in Monitor Start Methods

**Files:**
- `src/monitors/mempool_monitor.rs:645-649`
- `src/monitors/price_monitor.rs:598-602`
- `src/monitors/liquidity_monitor.rs:700-704`
- `src/monitors/block_monitor.rs:279-287`
- `src/monitors/liquidation_monitor.rs:789-793`

**Issue:** All monitor `start()` methods use unsafe raw pointer casting to create a self-reference for spawned tasks. This is undefined behavior if the monitor is dropped while the task is still running.

**Current Code:**
```rust
let self_ref = unsafe { &*(self as *const MempoolMonitor) };

tokio::spawn(async move {
    self_ref.run_monitoring_loop(tx, stop_rx).await;
});
```

**Impact:** Memory safety violation, potential use-after-free, crashes in production.

**Suggested Fix:**
```rust
// Use Arc wrapper pattern as already provided in the codebase
// The ArcMempoolMonitor wrapper should be the primary interface

// In the Monitor trait, change signature or use Arc<Self>
pub async fn start(self: Arc<Self>, tx: mpsc::Sender<MonitorEvent>) -> Result<()> {
    let monitor = Arc::clone(&self);

    if monitor.running.swap(true, Ordering::Relaxed) {
        return Err(MevError::Provider(ProviderError::SubscriptionError(
            "Monitor is already running".to_string(),
        )));
    }

    let (stop_tx, stop_rx) = mpsc::channel(1);
    {
        let mut guard = monitor.stop_tx.write().await;
        *guard = Some(stop_tx);
    }

    tokio::spawn(async move {
        monitor.run_monitoring_loop(tx, stop_rx).await;
    });

    Ok(())
}
```

---

### 1.2 U256 to f64 Conversion Precision Loss

**File:** `src/detectors/mod.rs:777-785`

**Issue:** The `u256_to_f64` function has severe precision loss for values larger than 2^53, which is common for token amounts with 18+ decimals.

**Current Code:**
```rust
pub fn u256_to_f64(val: U256) -> f64 {
    let bytes = val.to_be_bytes::<32>();
    let mut result = 0.0f64;
    for (i, &byte) in bytes.iter().enumerate() {
        result += (byte as f64) * 256f64.powi(31 - i as i32);
    }
    result
}
```

**Impact:** Incorrect profit calculations, missed opportunities, potential losses due to wrong trade sizes.

**Suggested Fix:**
```rust
/// Convert U256 to f64 with proper handling of large values
/// For MEV calculations, we typically work with wei amounts that fit in u128
pub fn u256_to_f64(val: U256) -> f64 {
    // For values that fit in u128, use direct conversion
    if val <= U256::from(u128::MAX) {
        let low: u128 = val.to::<u128>();
        return low as f64;
    }

    // For larger values, use logarithmic scaling to preserve relative precision
    // This is acceptable for profit comparisons but not for exact amounts
    let bits = 256 - val.leading_zeros();
    let shift = bits.saturating_sub(53); // f64 mantissa is 53 bits
    let shifted = val >> shift;
    let base: f64 = shifted.to::<u128>() as f64;
    base * 2f64.powi(shift as i32)
}

/// Convert f64 to U256 with overflow protection
pub fn f64_to_u256(val: f64) -> U256 {
    if val <= 0.0 || val.is_nan() {
        return U256::ZERO;
    }
    if val.is_infinite() || val >= 2.0f64.powi(128) {
        // Use u128::MAX as practical limit
        return U256::from(u128::MAX);
    }
    U256::from(val as u128)
}
```

---

### 1.3 Missing Slippage Protection in Sandwich Detector

**File:** `src/detectors/sandwich_detector.rs:458-469`

**Issue:** The backrun swap's `min_amount_out` is set to the original input amount, which doesn't account for frontrun costs and may result in unprofitable executions.

**Current Code:**
```rust
SwapStep {
    pool: analysis.pool.address,
    dex: analysis.pool.dex.clone(),
    token_in: decoded.token_out,
    token_out: decoded.token_in,
    amount_in: analysis.frontrun_output,
    min_amount_out: analysis.frontrun_amount, // At minimum get back input
},
```

**Impact:** Sandwich attacks may execute at a loss due to MEV competition or price movements.

**Suggested Fix:**
```rust
SwapStep {
    pool: analysis.pool.address,
    dex: analysis.pool.dex.clone(),
    token_in: decoded.token_out,
    token_out: decoded.token_in,
    amount_in: analysis.frontrun_output,
    // Must cover input + gas costs + minimum profit margin
    min_amount_out: analysis.frontrun_amount + analysis.gas_cost + ctx.config.min_profit_wei,
},
```

---

### 1.4 Race Condition in Pool Registry Updates

**File:** `src/detectors/mod.rs:294-300`

**Issue:** The `update_reserves` method doesn't handle concurrent updates atomically, which can lead to inconsistent state during high-frequency price updates.

**Current Code:**
```rust
pub fn update_reserves(&self, address: Address, reserve0: U256, reserve1: U256) {
    if let Some(mut pool) = self.pools.get_mut(&address) {
        pool.reserve0 = reserve0;
        pool.reserve1 = reserve1;
        pool.last_updated = Utc::now();
    }
}
```

**Impact:** Arbitrage calculations may use inconsistent reserve values, leading to failed transactions.

**Suggested Fix:**
```rust
pub fn update_reserves(&self, address: Address, reserve0: U256, reserve1: U256, block_number: u64) {
    if let Some(mut pool) = self.pools.get_mut(&address) {
        // Only update if this is newer data
        if block_number >= pool.last_block {
            pool.reserve0 = reserve0;
            pool.reserve1 = reserve1;
            pool.last_updated = Utc::now();
            pool.last_block = block_number;
        }
    }
}

// Add to RegisteredPool struct:
pub struct RegisteredPool {
    // ... existing fields ...
    pub last_block: u64,
}
```

---

## 2. High Severity Issues

### 2.1 Missing Transaction Simulation Before Execution

**Files:** All detector files

**Issue:** Per the research (03_rust_mev_implementations.md), production MEV bots must simulate transactions using REVM before submission. The detectors calculate theoretical profits but don't validate with simulation.

**Impact:** Transactions may revert on-chain, wasting gas and potentially revealing strategy to competitors.

**Suggested Fix:**
Add a simulation step to the DetectorContext:

```rust
// In src/detectors/mod.rs

use revm::{db::CacheDB, Evm, primitives::{TxEnv, ExecutionResult}};

impl DetectorContext {
    /// Simulate a swap path to verify profitability
    pub async fn simulate_swap_path(
        &self,
        swap_path: &[SwapStep],
        fork_block: u64,
    ) -> Result<SimulationResult> {
        let db = self.create_fork_db(fork_block).await?;
        let mut evm = Evm::builder().with_db(db).build();

        let mut total_gas = 0u64;
        let mut final_output = U256::ZERO;

        for step in swap_path {
            let tx_env = self.build_swap_tx(step)?;
            evm.env.tx = tx_env;

            match evm.transact_commit()? {
                ExecutionResult::Success { gas_used, output, .. } => {
                    total_gas += gas_used;
                    final_output = self.decode_swap_output(&output)?;
                }
                ExecutionResult::Revert { output, .. } => {
                    return Ok(SimulationResult::Reverted(output.to_vec()));
                }
                ExecutionResult::Halt { reason, .. } => {
                    return Ok(SimulationResult::Halted(format!("{:?}", reason)));
                }
            }
        }

        Ok(SimulationResult::Success {
            gas_used: total_gas,
            output: final_output,
        })
    }
}
```

---

### 2.2 Incorrect V3 Path Decoding

**File:** `src/monitors/mempool_monitor.rs:389-444`

**Issue:** The V3 exactInput path decoding has an off-by-one error in the loop logic that can miss the last token in multi-hop paths.

**Current Code:**
```rust
while offset + 20 <= path_bytes.len() {
    path.push(Address::from_slice(&path_bytes[offset..offset + 20]));
    offset += 23; // 20 (address) + 3 (fee)
    if offset > path_bytes.len() && offset - 3 + 20 <= path_bytes.len() {
        // Last token - this logic is confusing and incorrect
        path.push(Address::from_slice(
            &path_bytes[offset - 3..offset - 3 + 20],
        ));
        break;
    }
}
```

**Suggested Fix:**
```rust
fn decode_v3_path(&self, path_bytes: &[u8]) -> Option<Vec<Address>> {
    // V3 path format: token0 (20) + fee (3) + token1 (20) + fee (3) + ... + tokenN (20)
    // Total length for N tokens = 20 + (N-1) * 23

    if path_bytes.len() < 20 {
        return None;
    }

    let mut path = Vec::new();
    let mut offset = 0;

    // First token
    path.push(Address::from_slice(&path_bytes[offset..offset + 20]));
    offset += 20;

    // Remaining tokens (each preceded by 3-byte fee)
    while offset + 23 <= path_bytes.len() {
        offset += 3; // Skip fee
        path.push(Address::from_slice(&path_bytes[offset..offset + 20]));
        offset += 20;
    }

    // Check for final token without trailing fee
    if offset + 3 <= path_bytes.len() && path_bytes.len() - offset >= 3 {
        // There's a fee but maybe partial token - skip
    }

    if path.len() >= 2 {
        Some(path)
    } else {
        None
    }
}
```

---

### 2.3 Unbounded Memory Growth in Seen Transactions Set

**File:** `src/monitors/mempool_monitor.rs:484-493`

**Issue:** The `mark_seen` method clears the entire set when reaching `max_seen_txs`, causing a burst of duplicate processing for recently seen transactions.

**Current Code:**
```rust
async fn mark_seen(&self, hash: TxHash) -> bool {
    let mut seen = self.seen_txs.write().await;

    // Clear if too large
    if seen.len() >= self.max_seen_txs {
        seen.clear();
    }

    !seen.insert(hash)
}
```

**Suggested Fix:**
```rust
use std::collections::VecDeque;

// Change the field type
seen_txs: RwLock<(HashSet<TxHash>, VecDeque<TxHash>)>,

async fn mark_seen(&self, hash: TxHash) -> bool {
    let mut seen = self.seen_txs.write().await;
    let (set, queue) = &mut *seen;

    // Check if already seen
    if set.contains(&hash) {
        return true;
    }

    // Evict oldest entries if at capacity
    while set.len() >= self.max_seen_txs {
        if let Some(old_hash) = queue.pop_front() {
            set.remove(&old_hash);
        } else {
            break;
        }
    }

    // Insert new hash
    set.insert(hash);
    queue.push_back(hash);

    false
}
```

---

### 2.4 Missing Gas Price Validation in Liquidation Monitor

**File:** `src/monitors/liquidation_monitor.rs:509-522`

**Issue:** The `estimate_profit` function uses hardcoded bonus percentages and doesn't account for current gas prices, leading to potentially unprofitable liquidation recommendations.

**Current Code:**
```rust
fn estimate_profit(&self, protocol: LendingProtocol, debt_usd: f64) -> f64 {
    let bonus_pct = match protocol {
        LendingProtocol::AaveV3 => 0.05,
        LendingProtocol::CompoundV3 => 0.08,
        // ...
    };
    let max_liquidation = debt_usd * 0.5;
    max_liquidation * bonus_pct
}
```

**Suggested Fix:**
```rust
fn estimate_profit(
    &self,
    protocol: LendingProtocol,
    debt_usd: f64,
    gas_price_gwei: f64,
    eth_price_usd: f64,
) -> f64 {
    let bonus_pct = match protocol {
        LendingProtocol::AaveV3 => 0.05,
        LendingProtocol::CompoundV3 => 0.08,
        LendingProtocol::Euler => 0.10,
        LendingProtocol::Morpho => 0.05,
    };

    // Estimate gas cost
    let gas_units = 400_000u64; // Liquidation + flash loan
    let gas_cost_eth = (gas_units as f64) * gas_price_gwei * 1e-9;
    let gas_cost_usd = gas_cost_eth * eth_price_usd;

    let max_liquidation = debt_usd * 0.5;
    let gross_profit = max_liquidation * bonus_pct;

    // Return net profit
    (gross_profit - gas_cost_usd).max(0.0)
}
```

---

### 2.5 Bellman-Ford Algorithm Not Properly Detecting All Negative Cycles

**File:** `src/detectors/multihop.rs:137-203`

**Issue:** The Bellman-Ford implementation limits iterations to `min(n, max_hops)` instead of `n-1`, which may miss valid arbitrage cycles.

**Current Code:**
```rust
for _ in 0..n.min(self.max_hops) {
    // ... relaxation
}
```

**Suggested Fix:**
```rust
// Run full Bellman-Ford, then filter by hop count
for _ in 0..n - 1 {
    let mut updated = false;
    for token in &tokens {
        if let Some(edges) = graph.edges_from(*token) {
            for (idx, edge) in edges.iter().enumerate() {
                // ... relaxation logic
            }
        }
    }
    if !updated {
        break; // Early termination if no updates
    }
}

// When reconstructing cycle, validate hop count
fn reconstruct_cycle(...) -> Option<Cycle> {
    let cycle = /* reconstruction logic */;

    // Filter by max_hops constraint
    if cycle.edges.len() > self.max_hops {
        return None;
    }

    Some(cycle)
}
```

---

### 2.6 Missing Deadline Validation in Sandwich Detector

**File:** `src/detectors/sandwich_detector.rs:582-644`

**Issue:** The detector doesn't check if the victim transaction's deadline has already passed or is about to expire.

**Suggested Fix:**
```rust
async fn detect(&self, event: &MonitorEvent, ctx: &DetectorContext) -> Result<Vec<Opportunity>> {
    // ... existing code ...

    let decoded = match self.decode_swap_params(to, input, *value) {
        Some(d) => d,
        None => return Ok(opportunities),
    };

    // Check deadline validity
    let current_timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    let deadline_secs = decoded.deadline.to::<u64>();

    // Need at least 2 blocks worth of time (24 seconds on mainnet)
    if deadline_secs < current_timestamp + 24 {
        tracing::debug!("Transaction deadline too close: {}", deadline_secs);
        return Ok(opportunities);
    }

    // ... rest of detection logic
}
```

---

### 2.7 Price Monitor V3 Price Calculation Overflow Risk

**File:** `src/monitors/price_monitor.rs:302-314`

**Issue:** The V3 price calculation can overflow for extreme `sqrtPriceX96` values.

**Current Code:**
```rust
fn calculate_v3_price(&self, sqrt_price_x96: U256, decimals0: u8, decimals1: u8) -> f64 {
    let sqrt_price = sqrt_price_x96.to::<u128>() as f64;  // Can overflow!
    let two_96 = 2f64.powi(96);
    let price = (sqrt_price / two_96).powi(2);
    // ...
}
```

**Suggested Fix:**
```rust
fn calculate_v3_price(&self, sqrt_price_x96: U256, decimals0: u8, decimals1: u8) -> f64 {
    // sqrtPriceX96 can exceed u128::MAX for extreme prices
    // Use logarithmic calculation for safety

    if sqrt_price_x96.is_zero() {
        return 0.0;
    }

    // Calculate bits and scale appropriately
    let bits = 256 - sqrt_price_x96.leading_zeros();

    if bits <= 128 {
        let sqrt_price = sqrt_price_x96.to::<u128>() as f64;
        let two_96 = 2f64.powi(96);
        let price = (sqrt_price / two_96).powi(2);
        let decimal_adjustment = 10f64.powi(decimals0 as i32 - decimals1 as i32);
        price * decimal_adjustment
    } else {
        // For very large sqrtPrice, use log-space calculation
        let log_sqrt = (bits as f64) * 2f64.ln() +
            (sqrt_price_x96 >> (bits - 53)).to::<u64>() as f64 / 2f64.powi(53) * 2f64.ln();
        let log_price = 2.0 * (log_sqrt - 96.0 * 2f64.ln());
        let decimal_adjustment = (decimals0 as f64 - decimals1 as f64) * 10f64.ln();
        (log_price + decimal_adjustment).exp()
    }
}
```

---

### 2.8 Missing Block Reorg Handling

**Files:** All monitors

**Issue:** The monitors don't handle blockchain reorganizations, which can cause duplicate events or missed updates.

**Suggested Fix:**
```rust
// Add to BlockMonitor
async fn handle_reorg(&self, new_block: &BlockInfo, event_tx: &mpsc::Sender<MonitorEvent>) {
    let prev_block = self.last_block.load(Ordering::Relaxed);

    // Check for reorg (new block's parent doesn't match our last block)
    if prev_block > 0 && new_block.number <= prev_block {
        tracing::warn!(
            "{}: Potential reorg detected. Current: {}, New: {}",
            self.name,
            prev_block,
            new_block.number
        );

        // Emit reorg event
        if let Err(e) = event_tx.send(MonitorEvent::ChainReorg {
            old_head: prev_block,
            new_head: new_block.number,
            depth: prev_block - new_block.number + 1,
        }).await {
            tracing::error!("Failed to send reorg event: {:?}", e);
        }
    }
}

// Add to MonitorEvent enum
pub enum MonitorEvent {
    // ... existing variants ...
    ChainReorg {
        old_head: u64,
        new_head: u64,
        depth: u64,
    },
}
```

---

## 3. Medium Severity Issues

### 3.1 Inefficient Pool Pair Indexing

**File:** `src/detectors/mod.rs:262-273`

**Issue:** Pools are indexed by both (token0, token1) and (token1, token0), doubling storage and causing duplicate entries in queries.

**Suggested Fix:**
```rust
pub fn register(&self, pool: RegisteredPool) {
    let address = pool.address;
    let (token0, token1) = if pool.token0 < pool.token1 {
        (pool.token0, pool.token1)
    } else {
        (pool.token1, pool.token0)
    };

    self.pools.insert(address, pool);

    // Single canonical ordering
    self.pairs.entry((token0, token1)).or_default().push(address);
}

pub fn get_pools_for_pair(&self, token_a: Address, token_b: Address) -> Vec<RegisteredPool> {
    // Normalize ordering
    let (token0, token1) = if token_a < token_b {
        (token_a, token_b)
    } else {
        (token_b, token_a)
    };

    self.pairs
        .get(&(token0, token1))
        .map(|addrs| /* ... */)
        .unwrap_or_default()
}
```

---

### 3.2 Missing Rate Limiting in Mempool Monitor

**File:** `src/monitors/mempool_monitor.rs:121-122`

**Issue:** The `rate_limit` configuration option is defined but never implemented.

**Suggested Fix:**
```rust
async fn run_monitoring_loop(&self, event_tx: mpsc::Sender<MonitorEvent>, mut stop_rx: mpsc::Receiver<()>) {
    // ... setup code ...

    let rate_limiter = self.config.rate_limit.map(|limit| {
        tokio::time::interval(Duration::from_millis(1000 / limit as u64))
    });

    loop {
        // Apply rate limiting if configured
        if let Some(ref mut limiter) = rate_limiter {
            limiter.tick().await;
        }

        tokio::select! {
            // ... existing select branches
        }
    }
}
```

---

### 3.3 Hardcoded Protocol Addresses

**Files:**
- `src/monitors/mempool_monitor.rs:29-47`
- `src/monitors/liquidity_monitor.rs:93-112`
- `src/monitors/liquidation_monitor.rs:125-146`

**Issue:** All protocol addresses are hardcoded for Ethereum mainnet, preventing multi-chain deployment.

**Suggested Fix:**
```rust
#[derive(Debug, Clone)]
pub struct NetworkConfig {
    pub chain_id: u64,
    pub routers: HashMap<String, Address>,
    pub factories: HashMap<String, Address>,
    pub lending_protocols: HashMap<String, Address>,
    pub multicall: Address,
}

impl NetworkConfig {
    pub fn mainnet() -> Self {
        Self {
            chain_id: 1,
            routers: [
                ("uniswap_v2", address!("7a250d5630B4cF539739dF2C5dAcb4c659F2488D")),
                // ...
            ].into_iter().collect(),
            // ...
        }
    }

    pub fn arbitrum() -> Self { /* ... */ }
    pub fn optimism() -> Self { /* ... */ }
}
```

---

### 3.4 Missing Honeypot Token Detection

**File:** `src/detectors/liquidity_event.rs:212-249`

**Issue:** The honeypot check only examines pool characteristics but doesn't verify token contract behavior (transfer taxes, blacklists, etc.).

**Suggested Fix:**
```rust
async fn check_token_honeypot(&self, token: Address, provider: &impl Provider) -> TokenCheck {
    let mut check = TokenCheck::default();

    // Simulate a buy and sell to detect taxes
    let test_amount = U256::from(1_000_000_000_000_000_000u128); // 1 token

    // Check for transfer restrictions
    let code = provider.get_code(token, None).await.unwrap_or_default();

    // Known malicious patterns
    if code.contains(&[0x70, 0x61, 0x75, 0x73, 0x65, 0x64]) { // "paused"
        check.has_pause_function = true;
        check.risk_score += 20;
    }

    // Check for owner-only functions that could rug
    if code.contains(&[0x8d, 0xa5, 0xcb, 0x5b]) { // renounceOwnership selector
        check.ownership_renounced = false;
        check.risk_score += 10;
    }

    check
}
```

---

### 3.5 Inefficient DFS in Multi-hop Detector

**File:** `src/detectors/multihop.rs:206-293`

**Issue:** The DFS creates many cloned vectors on each recursive call, causing excessive allocations.

**Suggested Fix:**
```rust
fn dfs_find_cycles(
    &self,
    graph: &PriceGraph,
    start: Address,
    current: Address,
    path: &mut Vec<Address>,
    edges: &mut Vec<GraphEdge>,
    total_weight: f64,
    best_cycle: &mut Option<Cycle>,
    best_profit: &mut f64,
    visited: &mut HashSet<Address>,
) {
    if path.len() >= self.max_hops {
        return;
    }

    if let Some(graph_edges) = graph.edges_from(current) {
        for edge in graph_edges {
            let new_weight = total_weight + edge.weight;

            if edge.to == start && path.len() >= 2 {
                let profit_ratio = (-new_weight).exp();
                if profit_ratio > self.min_profit_ratio && profit_ratio > *best_profit {
                    *best_profit = profit_ratio;

                    let mut cycle_path = path.clone();
                    cycle_path.push(current);
                    cycle_path.push(start);

                    let mut cycle_edges = edges.clone();
                    cycle_edges.push(edge.clone());

                    *best_cycle = Some(Cycle {
                        path: cycle_path,
                        edges: cycle_edges,
                        profit_ratio,
                    });
                }
                continue;
            }

            if !visited.contains(&edge.to) && !path.contains(&edge.to) {
                path.push(current);
                edges.push(edge.clone());
                visited.insert(edge.to);

                self.dfs_find_cycles(
                    graph, start, edge.to, path, edges,
                    new_weight, best_cycle, best_profit, visited,
                );

                visited.remove(&edge.to);
                edges.pop();
                path.pop();
            }
        }
    }
}
```

---

### 3.6 Missing Error Context in Monitor Errors

**Files:** All monitor files

**Issue:** Error messages don't include sufficient context for debugging.

**Suggested Fix:**
```rust
// Instead of:
Err(MevError::Provider(ProviderError::WebSocketError(e.to_string())))

// Use:
Err(MevError::Provider(ProviderError::WebSocketError(
    format!(
        "Failed to connect to {} at {}: {}",
        self.name,
        self.ws_url,
        e
    )
)))
```

---

### 3.7 Statistics Not Thread-Safe for High-Frequency Updates

**File:** `src/monitors/mempool_monitor.rs:97-109`

**Issue:** Stats are updated with a write lock for every transaction, causing contention.

**Suggested Fix:**
```rust
use std::sync::atomic::AtomicU64;

#[derive(Debug, Default)]
pub struct MempoolStats {
    pub total_seen: AtomicU64,
    pub router_txs: AtomicU64,
    pub decoded_swaps: AtomicU64,
    pub failed_decodes: AtomicU64,
    pub filtered_out: AtomicU64,
}

impl MempoolStats {
    pub fn increment_total_seen(&self) {
        self.total_seen.fetch_add(1, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> MempoolStatsSnapshot {
        MempoolStatsSnapshot {
            total_seen: self.total_seen.load(Ordering::Relaxed),
            // ...
        }
    }
}
```

---

### 3.8 Price Cache Doesn't Handle Stale Data

**File:** `src/detectors/mod.rs:334-363`

**Issue:** `get()` returns prices regardless of age, only `get_fresh()` checks staleness.

**Suggested Fix:**
```rust
impl PriceCache {
    /// Get price, returning None if older than default max age (60 seconds)
    pub fn get(&self, token: &Address) -> Option<f64> {
        self.get_fresh(token, 60)
    }

    /// Get price regardless of age (use with caution)
    pub fn get_unchecked(&self, token: &Address) -> Option<f64> {
        self.prices.get(token).map(|p| *p)
    }
}
```

---

### 3.9 Missing Validation for Pool Fee Values

**File:** `src/detectors/mod.rs:681-702`

**Issue:** The `calculate_amount_out` function accepts any `fee_bps` value without validation.

**Suggested Fix:**
```rust
pub fn calculate_amount_out(
    amount_in: U256,
    reserve_in: U256,
    reserve_out: U256,
    fee_bps: u32,
) -> U256 {
    // Validate inputs
    if amount_in.is_zero() || reserve_in.is_zero() || reserve_out.is_zero() {
        return U256::ZERO;
    }

    // Fee sanity check (max 10% = 1000 bps)
    if fee_bps > 1000 {
        tracing::warn!("Unusually high fee: {} bps", fee_bps);
        return U256::ZERO;
    }

    // ... calculation
}
```

---

### 3.10 Liquidation Monitor Missing Position Discovery

**File:** `src/monitors/liquidation_monitor.rs`

**Issue:** The monitor only tracks explicitly registered positions. Per research, production liquidation bots actively scan for positions via events and The Graph.

**Suggested Addition:**
```rust
impl LiquidationMonitor {
    /// Discover positions from Borrow events
    pub async fn discover_positions_from_events(
        &self,
        provider: &impl Provider,
        from_block: u64,
    ) -> Result<Vec<(Address, LendingProtocol)>> {
        // Query Aave Borrow events
        let borrow_filter = Filter::new()
            .address(protocols::AAVE_V3_POOL)
            .event("Borrow(address,address,address,uint256,uint8,uint256,uint16)")
            .from_block(from_block);

        let logs = provider.get_logs(&borrow_filter).await?;

        let positions: Vec<_> = logs
            .iter()
            .filter_map(|log| {
                let user = Address::from_slice(&log.topics[2][12..32]);
                Some((user, LendingProtocol::AaveV3))
            })
            .collect();

        Ok(positions)
    }
}
```

---

### 3.11 Block Monitor Missing MEV-Boost Block Detection

**File:** `src/monitors/block_monitor.rs`

**Issue:** The monitor doesn't identify blocks built by MEV-Boost builders, which is relevant for strategy adjustment.

**Suggested Addition:**
```rust
impl BlockInfo {
    pub fee_recipient: Option<Address>,
    pub extra_data: Option<Bytes>,
    pub is_mev_boost: bool,
}

// Known builder addresses
const KNOWN_BUILDERS: &[Address] = &[
    address!("..."), // Flashbots
    address!("..."), // bloXroute
    // ...
];

fn process_header(header: &Header, received_at: Instant) -> BlockInfo {
    let fee_recipient = header.miner; // or beneficiary
    let is_mev_boost = KNOWN_BUILDERS.contains(&fee_recipient);

    BlockInfo {
        // ... existing fields ...
        fee_recipient: Some(fee_recipient),
        is_mev_boost,
    }
}
```

---

### 3.12 Missing Multicall Batch Size Limits

**Files:**
- `src/monitors/price_monitor.rs:365-397`
- `src/monitors/liquidation_monitor.rs:525-731`

**Issue:** Multicall batches can grow unbounded, potentially hitting gas limits or RPC size limits.

**Suggested Fix:**
```rust
const MAX_MULTICALL_BATCH: usize = 50;

async fn poll_pools(&self, provider: &RootProvider<Http<Client>>, event_tx: &mpsc::Sender<MonitorEvent>) -> Result<()> {
    let pools = self.pools.read().await;
    let pool_list: Vec<_> = pools.iter().collect();

    // Process in batches
    for chunk in pool_list.chunks(MAX_MULTICALL_BATCH) {
        self.poll_pool_batch(provider, chunk, event_tx).await?;
    }

    Ok(())
}
```

---

## 4. Low Severity Issues

### 4.1 Unused Configuration Fields

**File:** `src/monitors/mempool_monitor.rs:118-122`

- `min_value` is defined but never checked

### 4.2 Magic Numbers

**Files:** Multiple

- Gas estimates (150_000, 300_000, 400_000) should be constants
- Fee percentages should be configurable

### 4.3 Missing Documentation

**Files:** All detector files

- Public functions lack documentation
- Complex algorithms need explanations

### 4.4 Inconsistent Error Handling

- Some functions return `Option`, others `Result`
- Error types should be unified

### 4.5 Missing Cleanup in Stop Methods

**Files:** All monitors

- WebSocket connections not explicitly closed
- Pending futures not cancelled

### 4.6 Debug Logging in Production Code

**File:** `src/detectors/price_discrepancy.rs:77-85`

```rust
tracing::debug!(
    "Found spread {:.4}% between {} ({}) and {} ({})",
    // ...
);
```

Should use trace level for high-frequency events.

### 4.7 String Allocations in Hot Paths

**File:** `src/detectors/mod.rs:754-770`

`generate_opportunity_id` allocates strings on every call.

### 4.8 Missing Input Sanitization

**File:** `src/monitors/mempool_monitor.rs:221-256`

Input data bounds not fully validated before slice operations.

### 4.9 Potential Integer Overflow

**File:** `src/monitors/block_monitor.rs:137`

```rust
Some((now - block_timestamp) * 1000)  // Can overflow for very old blocks
```

### 4.10 Missing Pool Type Discrimination

**File:** `src/detectors/price_discrepancy.rs`

V2 and V3 pools use same price calculation but have different characteristics.

### 4.11 Confidence Score Not Calibrated

**Files:** All detectors

Confidence scores are arbitrary and not validated against actual outcomes.

### 4.12 Missing Transaction Gas Limit Validation

**File:** `src/detectors/sandwich_detector.rs`

Victim transaction gas limit not checked for execution feasibility.

### 4.13 Duplicate Router Entries Possible

**File:** `src/detectors/sandwich_detector.rs:90-94`

```rust
pub fn add_router(&mut self, router: Address) {
    if !self.known_routers.contains(&router) {  // Good
        self.known_routers.push(router);
    }
}
```

But initial list construction doesn't deduplicate.

### 4.14 Missing Timestamp Validation

**File:** `src/monitors/block_monitor.rs:126-138`

Negative timestamps or far-future timestamps not handled.

### 4.15 Inefficient Token Deduplication

**File:** `src/detectors/mod.rs:308-317`

```rust
pub fn all_tokens(&self) -> Vec<Address> {
    let mut tokens: Vec<Address> = self.pools
        .iter()
        .flat_map(|p| vec![p.token0, p.token1])  // Allocates vec per pool
        .collect();
    tokens.sort();
    tokens.dedup();
    tokens
}
```

Should use HashSet directly.

---

## 5. Missing Features

### 5.1 Flashbots/MEV-Share Integration

Per research (02_mev_fundamentals.md), production MEV bots should submit bundles via private relays.

```rust
// Required addition
pub struct FlashbotsExecutor {
    middleware: FlashbotsMiddleware<Provider<Ws>, LocalWallet>,
    signer: LocalWallet,
}

impl FlashbotsExecutor {
    pub async fn submit_bundle(&self, opportunity: &Opportunity, target_block: u64) -> Result<BundleHash> {
        let mut bundle = BundleRequest::new();

        for step in &opportunity.swap_path {
            let tx = self.build_swap_transaction(step)?;
            bundle = bundle.push_transaction(tx);
        }

        bundle = bundle.set_block(target_block);

        let pending = self.middleware.send_bundle(&bundle).await?;
        Ok(pending.bundle_hash)
    }
}
```

### 5.2 EVM Simulation with REVM

Per research (03_rust_mev_implementations.md), all production bots simulate before execution.

### 5.3 JIT Liquidity Detection

The detectors module mentions JIT but doesn't implement it.

```rust
pub struct JitLiquidityDetector {
    min_swap_size_usd: f64,
    target_fee_capture_ratio: f64,
}

impl JitLiquidityDetector {
    async fn analyze_pending_swap(
        &self,
        swap: &PendingSwap,
        pool: &UniswapV3Pool,
    ) -> Option<JitOpportunity> {
        // Calculate optimal tick range
        // Estimate fee capture
        // Build mint/burn bundle
    }
}
```

### 5.4 Cross-Chain Arbitrage

No support for bridged liquidity opportunities.

### 5.5 Historical Data Analysis/Backtesting

No backtesting framework as mentioned in research (rbuilder backtest mode).

### 5.6 Metrics and Telemetry

Missing Prometheus metrics for:
- Opportunities detected/executed
- Latency histograms
- Gas price tracking
- Profit/loss tracking

### 5.7 Position Management for Liquidations

The liquidation monitor detects but doesn't track position changes over time.

### 5.8 Adaptive Gas Pricing

No EIP-1559 gas price optimization based on historical data.

### 5.9 MEV-Share Refund Handling

No support for MEV-Share's redistribution mechanism.

### 5.10 Salmonella/Honeypot Token Simulation

Per rusty-sando research, simulation should detect malicious tokens.

---

## 6. Performance Improvements

### 6.1 Use Concurrent Processing for Multi-Pool Detection

**Current:** Sequential pool comparison
**Improved:**
```rust
use rayon::prelude::*;

let opportunities: Vec<Opportunity> = pool_pairs
    .par_iter()
    .filter_map(|(pool_a, pool_b)| {
        self.find_arbitrage(pool_a, pool_b, gas_price, min_profit)
    })
    .collect();
```

### 6.2 Cache ABI Decoding Results

**Issue:** Repeated ABI decoding for same selectors
**Solution:** Use lazy_static for decoded selectors

### 6.3 Use Zero-Copy Parsing for Transaction Data

**Current:** Multiple slice copies
**Improved:** Use `bytes::Bytes` with proper lifecycle management

### 6.4 Pool State Subscription Instead of Polling

**Current:** HTTP polling every second
**Improved:** WebSocket subscription to Sync events

### 6.5 Batch RPC Calls

**Current:** Individual `get_transaction_by_hash` calls
**Improved:** Use JSON-RPC batching

### 6.6 Pre-compute Exchange Rate Logarithms

**Current:** Computing `ln()` on every graph edge creation
**Improved:** Cache computed values with pool state

---

## 7. Best Practice Deviations

### 7.1 Not Following Artemis Architecture

Per research (03_rust_mev_implementations.md), the Artemis framework uses a clear Collector -> Strategy -> Executor pipeline. This codebase mixes concerns.

### 7.2 Missing Bundle Simulation

All research materials emphasize simulating bundles before submission.

### 7.3 No Gas Bidding Strategy

MEV bots need sophisticated gas bidding, not fixed multipliers.

### 7.4 Missing State Fork for Simulation

Per REVM patterns in research, should fork state at specific blocks for accurate simulation.

### 7.5 No Private Mempool Integration

Production bots use Blocknative, BloxRoute, or MEV-Share for better mempool visibility.

### 7.6 Missing Nonce Management

No nonce tracking for bundle transactions.

### 7.7 No Bundle Timing Optimization

Should target specific block builders and submission windows.

---

## Conclusion

The codebase provides a solid foundation for MEV detection but requires significant improvements before production deployment:

1. **Immediate:** Fix unsafe pointer usage and precision loss issues
2. **Short-term:** Add simulation, proper error handling, and thread safety
3. **Medium-term:** Implement missing features (Flashbots, REVM simulation)
4. **Long-term:** Refactor to Artemis-style architecture

The detection logic for arbitrage, sandwich, and liquidation opportunities aligns with MEV fundamentals from the research, but execution infrastructure is incomplete.
