# DEX and Simulation Module Audit Report

**Project:** longtail-mev-monitor
**Date:** 2026-02-02
**Auditor:** Claude Opus 4.5
**Scope:** `src/dex/` and `src/simulation/` modules

---

## Executive Summary

This audit examines the DEX interaction and simulation modules of the longtail-mev-monitor project. The codebase demonstrates a solid foundation for MEV monitoring with proper use of the Alloy library for Ethereum interactions. However, several issues were identified ranging from **critical math errors** to **performance concerns** and **missing features**.

### Severity Summary

| Severity | Count |
|----------|-------|
| Critical | 2 |
| High | 4 |
| Medium | 6 |
| Low | 5 |
| Informational | 4 |

---

## 1. Critical Issues

### 1.1 [CRITICAL] Uniswap V3 `get_amount_out` Uses Incorrect AMM Formula

**File:** `/home/ubuntu/Desktop/longtail-mev-monitor/src/dex/uniswap_v3.rs`
**Lines:** 845-863

**Description:**
The `get_amount_out` function for Uniswap V3 uses a constant product formula without fees, which is fundamentally incorrect for concentrated liquidity AMMs.

```rust
fn get_amount_out(&self, amount_in: U256, reserve_in: U256, reserve_out: U256) -> U256 {
    // V3 uses a different AMM formula based on concentrated liquidity
    // This is a simplified approximation for price estimation
    // For accurate quotes, use the Quoter contract
    if amount_in.is_zero() || reserve_in.is_zero() || reserve_out.is_zero() {
        return U256::ZERO;
    }

    // Simple constant product approximation (not accurate for V3)
    // Real V3 calculations require tick-by-tick simulation
    let numerator = amount_in * reserve_out;
    let denominator = reserve_in + amount_in;
    // ...
}
```

**Impact:**
- **Incorrect profit calculations** for V3 arbitrage opportunities
- **Missed MEV opportunities** due to underestimation
- **Failed transactions** due to overestimation
- The formula also **ignores fees entirely** (V3 has 0.01%, 0.05%, 0.3%, and 1% tiers)

**Recommendation:**
Implement proper V3 math with tick-based calculations:

```rust
/// V3 swap math implementation following Uniswap V3 whitepaper
pub fn get_amount_out_v3(
    &self,
    amount_in: U256,
    sqrt_price_x96: U256,
    liquidity: u128,
    fee_pips: u32,  // 100 = 0.01%, 500 = 0.05%, 3000 = 0.3%, 10000 = 1%
    zero_for_one: bool,
) -> U256 {
    // Apply fee
    let amount_in_after_fee = amount_in * U256::from(1_000_000 - fee_pips) / U256::from(1_000_000);

    // Calculate output using the concentrated liquidity formula
    // For accurate implementation, port logic from:
    // - AMMS-RS: https://github.com/darkforestry/amms-rs
    // - Or use the Quoter contract via eth_call

    // Simplified single-tick calculation (assumes no tick crossing):
    // delta_y = L * delta_sqrt_P  (for zero_for_one)
    // delta_x = L * delta_sqrt_P / (sqrt_P_old * sqrt_P_new)  (for one_for_zero)

    todo!("Implement proper V3 math or use Quoter contract")
}
```

Alternatively, always use the `QuoterV2` contract for V3 quotes (already defined in the codebase).

---

### 1.2 [CRITICAL] Missing Fee Application in V3 Reserves Interpretation

**File:** `/home/ubuntu/Desktop/longtail-mev-monitor/src/dex/uniswap_v3.rs`
**Lines:** 622-638

**Description:**
The `get_reserves` function returns V3 liquidity data in a V2-style format, losing critical information:

```rust
async fn get_reserves<T: Transport + Clone, P: Provider<T>>(
    &self,
    pool: &Address,
    provider: &P,
) -> DexResult<Reserves> {
    let slot0 = self.get_slot0(pool, provider).await?;
    let liquidity = self.get_liquidity(pool, provider).await?;

    // For V3, we return liquidity and sqrt_price as "reserves"
    Ok(Reserves {
        reserve0: U256::from(liquidity),
        reserve1: slot0.sqrt_price_x96,  // WRONG: This is not a reserve!
        block_timestamp_last: 0,
    })
}
```

**Impact:**
- `reserve1` contains `sqrt_price_x96`, not an actual reserve value
- Any code using these "reserves" for V3 will produce incorrect results
- The `Reserves` struct is semantically wrong for V3 pools

**Recommendation:**
Create a V3-specific data structure:

```rust
#[derive(Debug, Clone)]
pub struct V3PoolState {
    pub sqrt_price_x96: U256,
    pub tick: i32,
    pub liquidity: u128,
    pub fee_pips: u32,
}

// Or calculate actual token reserves from liquidity + price range
pub fn calculate_v3_reserves(
    liquidity: u128,
    sqrt_price_x96: U256,
    tick_lower: i32,
    tick_upper: i32,
) -> (U256, U256) {
    // Convert liquidity to actual token amounts in the position range
    // Following Uniswap V3 math
}
```

---

## 2. High Severity Issues

### 2.1 [HIGH] Sandwich Simulation Does Not Account for Victim Transaction State Changes

**File:** `/home/ubuntu/Desktop/longtail-mev-monitor/src/simulation/eth_call.rs`
**Lines:** 313-412

**Description:**
The sandwich simulation runs frontrun and backrun as independent `eth_call` simulations, but the state changes from the frontrun do not persist to the backrun:

```rust
let frontrun_result = self
    .simulate_swap(frontrun.recipient, frontrun.recipient, frontrun_data, frontrun_value, block)
    .await?;

// ...victim transaction happens here (we can't simulate it directly)...

// Backrun simulation uses FRESH state, not post-frontrun state
let backrun_result = self
    .simulate_swap(adjusted_backrun.recipient, adjusted_backrun.recipient, backrun_data, backrun_value, block)
    .await?;
```

**Impact:**
- Sandwich profit calculations are **significantly inaccurate**
- Actual execution will differ from simulation
- May lead to unprofitable sandwich attempts

**Recommendation:**
Use `revm` for proper state persistence across simulations:

```rust
use revm::{db::CacheDB, Evm, Context};

pub async fn simulate_sandwich_with_revm(
    &self,
    frontrun: SwapParams,
    victim_tx: &Transaction,
    backrun: SwapParams,
) -> Result<SimulationResult, EthCallError> {
    // Fork state from RPC
    let mut cache_db = self.create_cache_db().await?;

    // 1. Simulate frontrun (commits state changes)
    let frontrun_result = self.execute_tx_on_db(&mut cache_db, &frontrun)?;

    // 2. Simulate victim tx (commits state changes)
    let victim_result = self.execute_tx_on_db(&mut cache_db, victim_tx)?;

    // 3. Simulate backrun (uses post-frontrun + post-victim state)
    let backrun_result = self.execute_tx_on_db(&mut cache_db, &backrun)?;

    // Calculate actual profit
    Ok(SimulationResult { ... })
}
```

---

### 2.2 [HIGH] Gas Estimation Uses Static Values, Not Actual Simulation

**File:** `/home/ubuntu/Desktop/longtail-mev-monitor/src/simulation/gas_estimator.rs`
**Lines:** 70-76, 309-328

**Description:**
Gas estimates are based on hardcoded values that don't reflect actual transaction complexity:

```rust
base_gas.insert(OpportunityType::PriceDiscrepancy, 300_000); // 2 swaps
base_gas.insert(OpportunityType::MultiHop, 450_000); // 3 swaps
base_gas.insert(OpportunityType::Sandwich, 400_000); // 2 swaps + overhead
```

The `adjust_gas_for_complexity` function adds 150k per additional swap, but this is also static:

```rust
let additional_gas = additional_swaps as u64 * 150_000;
let total_gas = base_gas + additional_gas;
total_gas + (total_gas / 10)  // 10% buffer
```

**Impact:**
- V3 swaps use significantly more gas than V2 (especially with tick crossings)
- Token-specific gas costs (rebasing tokens, fee-on-transfer) are ignored
- Incorrect profitability calculations may cause failed transactions

**Recommendation:**
Use `eth_estimateGas` or revm tracing for accurate estimates:

```rust
pub async fn estimate_gas_accurate(
    &self,
    tx: &TransactionRequest,
) -> Result<u64, SimulationError> {
    // Use eth_estimateGas for accurate estimation
    let estimate = self.provider
        .estimate_gas(tx)
        .await
        .map_err(|e| SimulationError::GasEstimationFailed(e.to_string()))?;

    // Add safety buffer
    Ok((estimate as f64 * 1.15) as u64)  // 15% buffer
}
```

---

### 2.3 [HIGH] Pool Registry Not Thread-Safe for Updates During Iteration

**File:** `/home/ubuntu/Desktop/longtail-mev-monitor/src/dex/pool_registry.rs`
**Lines:** 161-172

**Description:**
While `DashMap` is used for thread-safe access, the indexing structures can become inconsistent:

```rust
pub struct PoolRegistry {
    pools: DashMap<Address, PoolInfo>,
    pairs: DashMap<(Address, Address), Vec<Address>>,
    token_pools: DashMap<Address, Vec<Address>>,
    pool_count: AtomicU64,
}
```

When `add_pool` is called, it updates multiple maps non-atomically:

```rust
pub fn add_pool(&self, pool: PoolInfo) {
    if self.pools.insert(address, pool).is_none() {
        self.pool_count.fetch_add(1, Ordering::Relaxed);  // (1)
    }
    self.pairs.entry((token0, token1)).or_default().push(address);  // (2)
    // ... more updates
}
```

**Impact:**
- Race conditions during pool discovery
- Inconsistent views between maps
- Potential duplicates in index vectors

**Recommendation:**
Use a lock or transaction-like pattern:

```rust
pub fn add_pool(&self, pool: PoolInfo) {
    let address = pool.address;

    // Use entry API with closure for atomicity within each map
    let is_new = self.pools.entry(address).or_insert_with(|| {
        // Only increment count and update indexes for new pools
        self.pool_count.fetch_add(1, Ordering::SeqCst);
        pool.clone()
    });

    // Consider using a single DashMap<Address, Arc<PoolInfo>>
    // with derived indexes computed on read, not write
}
```

---

### 2.4 [HIGH] eth_call Simulator Swap Encoding Hardcodes V2 Selectors

**File:** `/home/ubuntu/Desktop/longtail-mev-monitor/src/simulation/eth_call.rs`
**Lines:** 501-527

**Description:**
The `encode_swap_data` function only encodes for Uniswap V2:

```rust
pub fn encode_swap_data(&self, params: &SwapParams) -> Result<Bytes, EthCallError> {
    // Encode for Uniswap V2 style swapExactTokensForTokens
    let selector = [0x38, 0xed, 0x17, 0x39]; // swapExactTokensForTokens
    // ...
}
```

**Impact:**
- V3 swaps will be encoded incorrectly
- Simulations for V3 pools will fail or produce wrong results

**Recommendation:**
Add DEX type detection or pass encoding strategy:

```rust
pub fn encode_swap_data(
    &self,
    params: &SwapParams,
    dex_type: DexType,  // or detect from router address
) -> Result<Bytes, EthCallError> {
    match dex_type {
        DexType::UniswapV2 => self.encode_v2_swap_data(params),
        DexType::UniswapV3 => self.encode_v3_swap_data(params),
        DexType::Curve => self.encode_curve_swap_data(params),
        // ... etc
    }
}
```

---

## 3. Medium Severity Issues

### 3.1 [MEDIUM] Price Calculation Precision Loss

**File:** `/home/ubuntu/Desktop/longtail-mev-monitor/src/dex/uniswap_v2.rs`
**Lines:** 256-271

**Description:**
Price calculation converts U256 to f64, losing precision:

```rust
fn calculate_price(reserve0: U256, reserve1: U256) -> f64 {
    let r0 = reserve0.to_string().parse::<f64>().unwrap_or(0.0);
    let r1 = reserve1.to_string().parse::<f64>().unwrap_or(0.0);

    if r0 == 0.0 {
        0.0
    } else {
        r1 / r0
    }
}
```

**Impact:**
- f64 has ~15-17 significant digits; U256 can have 78 digits
- Price comparisons for arbitrage detection may be inaccurate
- Small arbitrage opportunities may be missed

**Recommendation:**
Use fixed-point arithmetic for price comparisons:

```rust
/// Calculate price ratio as a scaled U256 (e.g., 18 decimals)
fn calculate_price_scaled(reserve0: U256, reserve1: U256) -> U256 {
    if reserve0.is_zero() {
        return U256::ZERO;
    }
    // Scale by 1e18 for precision
    (reserve1 * U256::from(10).pow(U256::from(18))) / reserve0
}
```

---

### 3.2 [MEDIUM] V3 Tick-to-Price Calculation Uses f64 Powers

**File:** `/home/ubuntu/Desktop/longtail-mev-monitor/src/dex/uniswap_v3.rs`
**Lines:** 488-499

**Description:**
The tick conversion functions use floating-point arithmetic:

```rust
pub fn tick_to_price(tick: i32) -> f64 {
    1.0001_f64.powi(tick)
}

pub fn price_to_tick(price: f64) -> i32 {
    (price.ln() / 1.0001_f64.ln()).round() as i32
}
```

**Impact:**
- Precision issues for extreme tick values (min tick: -887272, max tick: 887272)
- May cause incorrect tick calculations at boundaries

**Recommendation:**
Use the established Q64.96 fixed-point math from Uniswap V3 SDK:

```rust
// Use lookup tables or precise fixed-point math
// Reference: https://github.com/Uniswap/v3-core/blob/main/contracts/libraries/TickMath.sol

pub fn get_sqrt_ratio_at_tick(tick: i32) -> U256 {
    // Implement using bit manipulation and lookup tables
    // Similar to AMMS-RS implementation
}
```

---

### 3.3 [MEDIUM] ERC20 Balance Override Assumes Slot 0 Mapping

**File:** `/home/ubuntu/Desktop/longtail-mev-monitor/src/simulation/eth_call.rs`
**Lines:** 587-620

**Description:**
The balance override assumes all ERC20s store balances at slot 0:

```rust
pub fn create_balance_override(
    &self,
    token: Address,
    holder: Address,
    balance: U256,
) -> HashMap<Address, AccountOverride> {
    // For ERC20 tokens, the balance mapping is typically at slot 0
    let slot = calculate_balance_slot(holder, U256::ZERO);  // Assumes slot 0
    // ...
}
```

**Impact:**
- Many tokens use different storage layouts (OpenZeppelin, Vyper, proxies)
- USDC uses slot 9, DAI uses slot 2, etc.
- Simulations with overrides will fail for non-standard tokens

**Recommendation:**
Detect storage slot dynamically or maintain a mapping:

```rust
const KNOWN_BALANCE_SLOTS: &[(Address, U256)] = &[
    (USDC, U256::from(9)),
    (DAI, U256::from(2)),
    (WETH, U256::from(3)),
    // ...
];

pub fn get_balance_slot(token: Address) -> Option<U256> {
    KNOWN_BALANCE_SLOTS
        .iter()
        .find(|(addr, _)| *addr == token)
        .map(|(_, slot)| *slot)
}
```

---

### 3.4 [MEDIUM] Multi-Hop Simulation Doesn't Use Actual Output as Next Input

**File:** `/home/ubuntu/Desktop/longtail-mev-monitor/src/simulation/eth_call.rs`
**Lines:** 223-310

**Description:**
Each hop simulation is independent and doesn't carry forward the actual output:

```rust
for (i, swap) in swaps.iter().enumerate() {
    let mut swap_with_amount = swap.clone();
    swap_with_amount.amount_in = current_amount;  // Uses output from SEPARATE eth_call

    let result = self.simulate_swap(...).await?;

    current_amount = result.output_amount;  // Output decoded from return data
    // ...
}
```

**Impact:**
- State changes from hop 1 don't affect hop 2's reserves
- Slippage compounds incorrectly
- Large trades will have significantly wrong estimates

**Recommendation:**
Use revm with state persistence or simulate the entire multicall in one eth_call.

---

### 3.5 [MEDIUM] Priority Fee Estimation From History Is Expensive

**File:** `/home/ubuntu/Desktop/longtail-mev-monitor/src/simulation/gas_estimator.rs`
**Lines:** 331-379

**Description:**
The fallback priority fee estimation fetches multiple blocks and all their transaction receipts:

```rust
for i in 0..5 {
    if let Ok(Some(block)) = self.provider.get_block_by_number(...).await {
        for tx_hash in block.transactions.hashes() {
            if let Ok(Some(receipt)) = self.provider.get_transaction_receipt(tx_hash).await {
                // ...
            }
        }
    }
}
```

**Impact:**
- Potentially hundreds of RPC calls
- Significant latency in time-sensitive MEV scenarios
- May hit rate limits

**Recommendation:**
Use `eth_feeHistory` RPC method:

```rust
pub async fn get_priority_fee_from_history(&self) -> Result<U256, SimulationError> {
    // eth_feeHistory returns historical gas data in one call
    let fee_history = self.provider
        .fee_history(10, BlockNumberOrTag::Latest, &[25.0, 50.0, 75.0])
        .await?;

    // Extract median priority fee from reward percentiles
    let median_rewards: Vec<_> = fee_history.reward
        .iter()
        .filter_map(|r| r.get(1))  // 50th percentile
        .collect();

    // Return median of medians
    Ok(calculate_median(&median_rewards))
}
```

---

### 3.6 [MEDIUM] V3 sqrtPriceX96 Conversion May Overflow

**File:** `/home/ubuntu/Desktop/longtail-mev-monitor/src/dex/uniswap_v3.rs`
**Lines:** 469-484

**Description:**
The price-to-sqrt conversion may overflow for high prices:

```rust
pub fn price_to_sqrt_price_x96(price: f64) -> U256 {
    let sqrt_price = price.sqrt();
    let q96: f64 = 2.0_f64.powi(96);
    let result = sqrt_price * q96;

    if result.is_infinite() || result.is_nan() {
        U256::MAX
    } else {
        U256::from(result as u128)  // u128 overflow for large prices
    }
}
```

**Impact:**
- Prices > ~3.4e38 will overflow u128
- Returning `U256::MAX` is incorrect behavior

**Recommendation:**
Use arbitrary precision math:

```rust
use ruint::Uint;

pub fn price_to_sqrt_price_x96_safe(price: U256, decimals_adjustment: i32) -> U256 {
    // Use fixed-point sqrt implementation
    let sqrt = isqrt(price);
    let q96 = U256::from(1u128) << 96;
    sqrt * q96 >> (decimals_adjustment / 2)
}
```

---

## 4. Low Severity Issues

### 4.1 [LOW] decode_path Doesn't Handle Empty Paths

**File:** `/home/ubuntu/Desktop/longtail-mev-monitor/src/dex/uniswap_v3.rs`
**Line:** 531

```rust
pub fn decode_path(path: &Bytes) -> DexResult<(Vec<Address>, Vec<u32>)> {
    if path.len() < 43 {  // Should also check for empty
        return Err(DexError::Decoding("Path too short".to_string()));
    }
    // ...
}
```

---

### 4.2 [LOW] Missing Input Validation in SwapParams

**File:** `/home/ubuntu/Desktop/longtail-mev-monitor/src/dex/mod.rs`
**Lines:** 112-130

**Description:**
`SwapParams::new` doesn't validate that `token_in != token_out`:

```rust
pub fn new(
    token_in: Address,
    token_out: Address,  // Could equal token_in
    // ...
) -> Self {
    // No validation
}
```

---

### 4.3 [LOW] Pool Discovery Uses Unbounded Loops

**File:** `/home/ubuntu/Desktop/longtail-mev-monitor/src/dex/pool_registry.rs`
**Lines:** 352-369

```rust
for i in 0..limit {
    match self.discover_v2_pool_at_index(...).await {
        Ok(_) => discovered += 1,
        Err(e) => warn!(...),  // Continues on error
    }
}
```

Consider adding rate limiting and proper backoff.

---

### 4.4 [LOW] Cached Values Use Instant::now() Which Can Be Inaccurate

**File:** `/home/ubuntu/Desktop/longtail-mev-monitor/src/simulation/gas_estimator.rs`
**Lines:** 42-58

Using block numbers instead of wall time would be more reliable for blockchain-related caching.

---

### 4.5 [LOW] Error Messages Expose Internal Details

**File:** Multiple files

Error messages like "Contract call failed: {e.to_string()}" may expose internal implementation details in production.

---

## 5. Informational

### 5.1 [INFO] No REVM Integration for Local Simulation

**Description:**
The codebase relies entirely on `eth_call` for simulation. According to the research in `01_rust_ethereum_libraries.md` and `05_rust_performance_optimization.md`, revm is the standard for competitive MEV bots.

**Current Pattern:**
```rust
let result = self.provider.call(&tx).block(block_id).await;
```

**Recommended Pattern (from research):**
```rust
use revm::{Evm, db::CacheDB};

let mut evm = Evm::builder()
    .with_db(&mut cache_db)
    .modify_tx_env(|tx| { ... })
    .build();

let result = evm.transact_commit();
```

**Benefits of REVM:**
- 10-100x faster than RPC-based simulation
- State persistence between simulations
- Accurate gas metering
- State diff inspection
- Bundle simulation with proper state propagation

---

### 5.2 [INFO] Missing DEX Support

The codebase only supports Uniswap V2/V3 and SushiSwap. For comprehensive MEV monitoring, consider adding:

- **Curve** - Large TVL, different math (StableSwap invariant)
- **Balancer** - Weighted pools, different formula
- **0x/Aggregators** - For routing optimization
- **Bancor v3** - Single-sided liquidity

Reference libraries for implementations:
- **AMMS-RS**: https://github.com/darkforestry/amms-rs
- **CFMMS-RS**: https://github.com/0xKitsune/cfmms-rs

---

### 5.3 [INFO] Consider Event-Based Pool State Sync

**Current Approach:** Polls `getReserves()` on-demand

**Recommended Approach:** Subscribe to `Sync` events for V2 and `Swap` events for V3:

```rust
// V2 Sync event
event Sync(uint112 reserve0, uint112 reserve1);

// V3 Swap event
event Swap(
    address sender,
    address recipient,
    int256 amount0,
    int256 amount1,
    uint160 sqrtPriceX96,
    uint128 liquidity,
    int24 tick
);
```

This enables real-time pool state tracking without polling.

---

### 5.4 [INFO] Type System Could Be More Expressive

Consider using newtypes for different numeric types:

```rust
// Instead of raw U256 everywhere:
pub struct TokenAmount(U256);
pub struct SqrtPriceX96(U256);
pub struct WeiAmount(U256);
pub struct Tick(i32);

// Prevents mixing up different numeric meanings
```

---

## 6. Performance Recommendations

### 6.1 Use Parallel Pool State Fetching

```rust
use futures::future::join_all;

pub async fn fetch_reserves_batch(
    pools: &[Address],
    provider: &impl Provider,
) -> Vec<Reserves> {
    let futures: Vec<_> = pools
        .iter()
        .map(|pool| async {
            let pair = IUniswapV2Pair::new(*pool, provider);
            pair.getReserves().call().await
        })
        .collect();

    join_all(futures).await
}
```

### 6.2 Implement Multicall for Batch Queries

```rust
use alloy::sol;

sol! {
    interface IMulticall3 {
        struct Call3 {
            address target;
            bool allowFailure;
            bytes callData;
        }

        function aggregate3(Call3[] calldata calls)
            external payable returns (Result[] memory);
    }
}
```

### 6.3 Pre-compute Common Values

```rust
lazy_static! {
    // Pre-compute tick-to-sqrt-price lookup table
    static ref TICK_TO_SQRT_PRICE: HashMap<i32, U256> = {
        let mut map = HashMap::new();
        for tick in (MIN_TICK..=MAX_TICK).step_by(60) {
            map.insert(tick, compute_sqrt_price_at_tick(tick));
        }
        map
    };
}
```

---

## 7. Summary of Recommended Actions

### Immediate (Critical/High):
1. Fix V3 `get_amount_out` to use proper concentrated liquidity math or Quoter
2. Integrate revm for accurate multi-transaction simulation
3. Fix sandwich simulation to use persistent state
4. Add proper gas estimation via `eth_estimateGas`

### Short-term (Medium):
5. Fix price calculation precision with fixed-point math
6. Implement proper V3 tick math without floating point
7. Add storage slot detection for balance overrides
8. Optimize priority fee estimation with `eth_feeHistory`

### Long-term (Enhancements):
9. Add support for Curve, Balancer, and other DEXes
10. Implement event-based pool state synchronization
11. Add REVM-based simulation engine
12. Implement proper bundle simulation with Flashbots

---

## 8. References

- [AMMS-RS](https://github.com/darkforestry/amms-rs) - Reference AMM implementations
- [CFMMS-RS](https://github.com/0xKitsune/cfmms-rs) - CFMM simulation library
- [REVM Documentation](https://bluealloy.github.io/revm/)
- [Uniswap V3 Whitepaper](https://uniswap.org/whitepaper-v3.pdf)
- [Uniswap V3 TickMath](https://github.com/Uniswap/v3-core/blob/main/contracts/libraries/TickMath.sol)
- Research documents at `/home/ubuntu/Desktop/research/`
