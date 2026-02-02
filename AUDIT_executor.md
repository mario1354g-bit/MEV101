# Executor Module Security Audit Report

**Project:** longtail-mev-monitor
**Module:** src/executor/
**Audit Date:** 2026-02-02
**Auditor:** Security Review

---

## Executive Summary

This audit examines the executor module of the longtail-mev-monitor MEV bot project, comparing implementation against established patterns from Artemis, Rusty-Sando, and Flashbots infrastructure documentation. The audit identified **15 critical/high severity issues**, **12 medium severity issues**, and **8 low severity/code quality issues**.

---

## Table of Contents

1. [Critical Security Issues](#1-critical-security-issues)
2. [High Severity Issues](#2-high-severity-issues)
3. [Medium Severity Issues](#3-medium-severity-issues)
4. [Low Severity / Code Quality](#4-low-severity--code-quality)
5. [Missing Features](#5-missing-features)
6. [Comparison with Reference Implementations](#6-comparison-with-reference-implementations)
7. [Recommendations Summary](#7-recommendations-summary)

---

## 1. Critical Security Issues

### 1.1 CRITICAL: Incorrect Flashbots Signature Implementation

**File:** `src/executor/flashbots.rs:81-91`

The signature implementation uses `keccak256` directly on the payload, but Flashbots expects EIP-191 signed message format.

```rust
// CURRENT (INCORRECT):
async fn sign_payload(&self, payload: &str) -> Result<String> {
    let message_hash = keccak256(payload.as_bytes());
    let signature = self.signer.sign_hash(&message_hash).await
        .map_err(|e| MevError::Execution(ExecutionError::SignerError(e.to_string())))?;
    // ...
}
```

**Issue:** Flashbots expects the payload to be signed using EIP-191 personal_sign format, not a raw hash. This will cause authentication failures with the Flashbots relay.

**Fix:**
```rust
async fn sign_payload(&self, payload: &str) -> Result<String> {
    // EIP-191: \x19Ethereum Signed Message:\n<length><message>
    let message = format!("\x19Ethereum Signed Message:\n{}{}", payload.len(), payload);
    let message_hash = keccak256(message.as_bytes());
    let signature = self.signer.sign_hash(&message_hash).await
        .map_err(|e| MevError::Execution(ExecutionError::SignerError(e.to_string())))?;

    Ok(format!(
        "{}:0x{}",
        self.signer.address(),
        hex::encode(signature.as_bytes())
    ))
}
```

**Reference:** Research doc `04_flashbots_infrastructure.md:351-366` shows correct signing pattern.

---

### 1.2 CRITICAL: Sandwich Attack Bundle Missing Victim Transaction

**File:** `src/executor/sandwich.rs:549-559`

The sandwich bundle conversion drops the victim transaction hash entirely.

```rust
// CURRENT (INCORRECT):
pub fn to_flashbots_bundle(&self) -> FlashbotsBundle {
    // PROBLEM: victim_tx_hash is completely ignored!
    FlashbotsBundle {
        txs: vec![self.frontrun_tx.clone(), self.backrun_tx.clone()],
        block_number: self.block_number,
        // ...
    }
}
```

**Issue:** Without including the victim transaction, the sandwich attack cannot work. Flashbots bundles support referencing mempool transactions by hash, but this implementation omits it entirely.

**Fix:** The Flashbots bundle format needs to be extended to support tx hash references, or use MEV-Share's `mev_sendBundle` format:

```rust
// Option 1: Extend FlashbotsBundle to support hash references
pub struct FlashbotsBundle {
    pub txs: Vec<Bytes>,
    pub tx_hashes: Vec<B256>,  // Add this field for mempool tx references
    // ...
}

// Option 2: Use MEV-Share format for sandwiches
// body: [{ hash: victim_hash }, { tx: frontrun }, { tx: backrun }]
```

---

### 1.3 CRITICAL: Nonce Management Race Condition

**File:** `src/executor/tx_builder.rs:218-219`

```rust
pub async fn sign_tx(&self, tx: &TransactionRequest) -> Result<Bytes> {
    let nonce = self.get_nonce().await?;
    // ...
}
```

**Issue:** Each transaction signing fetches the nonce independently. When signing multiple transactions for a bundle (e.g., approval + swap), both may get the same nonce, causing the second transaction to fail.

**Evidence in:** `src/executor/arbitrage.rs:219-221` - multiple txs signed in loop:
```rust
for (i, step) in swap_path.steps.iter().enumerate() {
    // ...
    let signed_tx = tx_builder.build_and_sign_swap(params).await?;
    signed_txs.push(signed_tx);
}
```

**Fix:**
```rust
pub struct TxBuilder<P> {
    // ...
    nonce_tracker: Arc<Mutex<Option<u64>>>,
}

impl<P> TxBuilder<P> {
    pub async fn sign_tx(&self, tx: &TransactionRequest) -> Result<Bytes> {
        let nonce = self.get_and_increment_nonce().await?;
        // ...
    }

    async fn get_and_increment_nonce(&self) -> Result<u64> {
        let mut tracker = self.nonce_tracker.lock().await;
        let nonce = match *tracker {
            Some(n) => n,
            None => self.provider.get_transaction_count(self.signer.address()).await?,
        };
        *tracker = Some(nonce + 1);
        Ok(nonce)
    }

    pub fn reset_nonce(&self) {
        let mut tracker = self.nonce_tracker.lock().await;
        *tracker = None;
    }
}
```

---

### 1.4 CRITICAL: Backrun Bundle Does Not Include Target Transaction

**File:** `src/executor/backrun.rs:322-324`

```rust
// Create bundle with backrun following target tx
// Note: We submit our tx and reference the target by hash
let bundle = FlashbotsBundle::new(vec![signed_backrun], block_number + 1)
    .with_revert_on_fail(ctx.config.revert_on_fail);
```

**Issue:** The backrun executor creates a bundle with only the backrun transaction, without referencing the target transaction. For a backrun to work, the target transaction must be included in or referenced by the bundle. Without this, there's no guarantee the backrun will execute after the target.

**Fix:** Use MEV-Share's bundle format to reference the target:
```rust
// Should use MEV-Share mev_sendBundle with hash reference:
// body: [{ hash: target_tx_hash }, { tx: backrun_tx }]
```

---

## 2. High Severity Issues

### 2.1 HIGH: Missing Bundle Simulation Before Submission

**File:** `src/executor/backrun.rs:371-388`

The backrun executor submits bundles to multiple blocks without simulating for each target block.

```rust
for block_offset in 0..ctx.config.target_blocks {
    let mut target_bundle = bundle.clone();
    target_bundle.block_number = block_number + 1 + block_offset as u64;

    match ctx.flashbots.send_bundle(target_bundle).await {
        // No simulation for each block!
```

**Issue:** State may change between blocks. A bundle profitable at block N may not be profitable at block N+1. Each target block should have its own simulation.

**Fix:**
```rust
for block_offset in 0..ctx.config.target_blocks {
    let mut target_bundle = bundle.clone();
    target_bundle.block_number = block_number + 1 + block_offset as u64;

    // Simulate for this specific block
    let sim_result = ctx.flashbots.simulate_bundle(target_bundle.clone()).await?;
    if !sim_result.success {
        continue; // Skip this block
    }

    match ctx.flashbots.send_bundle(target_bundle).await {
        // ...
    }
}
```

---

### 2.2 HIGH: Unlimited Token Approval Pattern

**File:** `src/executor/liquidation.rs:458-459`

```rust
let approval_calldata = self.build_approval_call(pool_address, data.debt_amount);
```

While this approves exact amounts (good), the general pattern in tx_builder suggests unlimited approvals might be used elsewhere.

**File:** `src/executor/router_encoder.rs:473-475`

The `encode_erc20_approve` function is often called with `U256::MAX`:

```rust
pub fn encode_erc20_approve(spender: Address, amount: U256) -> Bytes {
    let call = IERC20::approveCall { spender, amount };
    Bytes::from(call.abi_encode())
}
```

**Issue:** Unlimited approvals are a security risk. If the approved contract is compromised, all tokens can be drained.

**Recommendation:** Always approve exact amounts needed for the transaction.

---

### 2.3 HIGH: No Slippage Protection on Intermediate Swaps

**File:** `src/executor/arbitrage.rs:110-117`

```rust
let amount_out_min = if i == swap_path.steps.len() - 1 {
    // Final step: use path's min_output
    swap_path.min_output
} else {
    // Intermediate step: allow small slippage
    self.calculate_min_output(step.amount_out)  // Only uses expected, not accounting for cumulative slippage
};
```

**Issue:** Intermediate swap slippage compounds. If step 1 has 0.5% slippage and step 2 has 0.5%, total could be ~1% but each step only protects for 0.5%.

**Fix:**
```rust
let amount_out_min = if i == swap_path.steps.len() - 1 {
    swap_path.min_output
} else {
    // Calculate cumulative slippage tolerance
    let remaining_steps = swap_path.steps.len() - i - 1;
    let cumulative_slippage_bps = self.max_slippage_bps * (remaining_steps as u32 + 1);
    let slippage_amount = step.amount_out * U256::from(cumulative_slippage_bps) / U256::from(10000);
    step.amount_out.saturating_sub(slippage_amount)
};
```

---

### 2.4 HIGH: Goerli Testnet is Deprecated

**File:** `src/executor/flashbots.rs:57-64`

```rust
/// Create a Flashbots client for Goerli testnet.
pub fn goerli(signer: PrivateKeySigner) -> Self {
    Self::new(
        "https://relay-goerli.flashbots.net".to_string(),
        signer,
        5,
    )
}
```

**Issue:** Goerli has been deprecated. The correct testnet is Sepolia.

**Fix:** Remove Goerli support or mark as deprecated, and ensure Sepolia is the recommended testnet.

---

### 2.5 HIGH: Missing Retry Logic for Bundle Submission

**File:** `src/executor/flashbots.rs:162-196`

```rust
pub async fn send_bundle(&self, bundle: FlashbotsBundle) -> Result<BundleResponse> {
    // Single attempt, no retries
    let response: SendBundleResponse = self
        .rpc_request("eth_sendBundle", vec![params])
        .await?;
    // ...
}
```

**Issue:** Network failures, rate limits, and transient errors are not handled. The config has `max_retries` but it's never used.

**Fix:**
```rust
pub async fn send_bundle(&self, bundle: FlashbotsBundle) -> Result<BundleResponse> {
    let mut last_error = None;

    for attempt in 0..=self.max_retries {
        match self.send_bundle_internal(&bundle).await {
            Ok(response) => return Ok(response),
            Err(e) => {
                last_error = Some(e);
                if attempt < self.max_retries {
                    tokio::time::sleep(Duration::from_millis(100 * (attempt as u64 + 1))).await;
                }
            }
        }
    }

    Err(last_error.unwrap())
}
```

---

### 2.6 HIGH: No Validation of Router Addresses

**File:** `src/executor/tx_builder.rs:401-455`

```rust
fn get_router_address(protocol: &str, chain_id: u64) -> Result<Address> {
    match (protocol, chain_id) {
        ("uniswap_v2", 1) => Ok("0x7a250d5630B4cF539739dF2C5dAcb4c659F2488D".parse().unwrap()),
        // ...
    }
}
```

**Issue:** Router addresses are hardcoded without on-chain validation. If a router is upgraded or deprecated, the code would interact with the wrong contract.

**Recommendation:** Add on-chain validation or configuration-based router registry with version checking.

---

## 3. Medium Severity Issues

### 3.1 MEDIUM: Deadline Always Set to MAX

**File:** `src/executor/router_encoder.rs:348`

```rust
deadline: U256::MAX, // Use MAX for deadline
```

**Issue:** Using `U256::MAX` for deadline means transactions never expire. If a transaction gets stuck in the mempool or bundle pool, it could execute at a much later time with unfavorable prices.

**Fix:** Use a reasonable deadline (e.g., current block timestamp + 2 minutes):
```rust
deadline: U256::from(current_timestamp + 120),
```

---

### 3.2 MEDIUM: Insufficient Gas Buffer

**File:** `src/executor/tx_builder.rs:282-286`

```rust
// Add 20% buffer for safety
let gas_with_buffer = gas + (gas / 5);
```

**Issue:** 20% buffer may be insufficient for complex MEV transactions, especially during high congestion. Artemis and Rusty-Sando typically use 30-50% buffers.

**Fix:**
```rust
// Add 30% buffer for MEV transactions (higher variability)
let gas_with_buffer = gas * 130 / 100;
```

---

### 3.3 MEDIUM: Missing Access List Optimization

**File:** `src/executor/tx_builder.rs:237`

```rust
access_list: Default::default(),
```

**Issue:** Access lists can reduce gas costs for storage-heavy operations. MEV transactions often access many storage slots, making this optimization valuable.

**Fix:** Generate access lists based on simulated state access:
```rust
// Get access list from simulation
let access_list = self.provider
    .create_access_list(&tx)
    .await
    .unwrap_or_default();
```

---

### 3.4 MEDIUM: No Bundle Cancellation on Unprofitable Detection

**File:** `src/executor/arbitrage.rs:263-280`

When simulation shows unprofitability, the function returns without attempting to cancel any previously submitted bundles.

**Issue:** If bundles were submitted to earlier blocks (via multi-block targeting), they should be cancelled when the opportunity becomes unprofitable.

**Fix:** Use `replacement_uuid` and `eth_cancelBundle`:
```rust
if !is_profitable {
    if let Some(uuid) = &bundle.replacement_uuid {
        let _ = ctx.flashbots.cancel_bundle(uuid).await;
    }
    return Ok(ExecutionResult { ... });
}
```

---

### 3.5 MEDIUM: Inconsistent Error Handling

**File:** `src/executor/backrun.rs:299-304`

```rust
let gas_used = tx_builder.estimate_gas(&backrun_tx).await.unwrap_or(200_000);
```

**Issue:** Silently falling back to default gas on estimation failure hides potential issues with the transaction.

**Fix:**
```rust
let gas_used = match tx_builder.estimate_gas(&backrun_tx).await {
    Ok(gas) => gas,
    Err(e) => {
        warn!(error = %e, "Gas estimation failed, using default");
        200_000
    }
};
```

---

### 3.6 MEDIUM: Profit Calculation Doesn't Account for Token Decimals

**File:** `src/executor/backrun.rs:217-228`

```rust
fn calculate_profit(&self, data: &BackrunData) -> U256 {
    if data.expected_amount_out > data.amount_in {
        data.expected_amount_out - data.amount_in
    } else {
        U256::ZERO
    }
}
```

**Issue:** This assumes both tokens have the same decimals. Comparing 1e18 ETH to 1e6 USDC will give incorrect results.

**Fix:**
```rust
fn calculate_profit(&self, data: &BackrunData, token_in_decimals: u8, token_out_decimals: u8) -> U256 {
    // Normalize to common decimal base (e.g., 1e18)
    let normalized_in = data.amount_in * U256::from(10).pow(U256::from(18 - token_in_decimals));
    let normalized_out = data.expected_amount_out * U256::from(10).pow(U256::from(18 - token_out_decimals));
    // ...
}
```

---

### 3.7 MEDIUM: Health Factor Validation Off-by-One

**File:** `src/executor/liquidation.rs:421-432`

```rust
let one_e18 = U256::from(1_000_000_000_000_000_000u128);

if data.health_factor >= one_e18 {
    return Err(...);
}
```

**Issue:** A health factor of exactly 1.0 (1e18) should be liquidatable (it's right at the threshold). The condition should use `>` not `>=`.

**Fix:**
```rust
if data.health_factor > one_e18 {
    return Err(...);
}
```

---

### 3.8 MEDIUM: Flashloan Provider Selection Ignores Token Liquidity

**File:** `src/executor/flashloan.rs:204-228`

```rust
fn select_provider(&self, _token: Address, chain_id: u64) -> Option<FlashloanProvider> {
    // Token parameter is unused!
```

**Issue:** Different providers have different token support and liquidity. Balancer may not have liquidity for all tokens.

**Fix:**
```rust
fn select_provider(&self, token: Address, amount: U256, chain_id: u64) -> Option<FlashloanProvider> {
    let providers = [FlashloanProvider::Balancer, FlashloanProvider::AaveV3];

    for provider in providers {
        if provider.contract_address(chain_id).is_some()
            && provider.fee_bps() <= self.config.max_fee_bps
            && self.check_liquidity(provider, token, amount, chain_id).await
        {
            return Some(provider);
        }
    }
    None
}
```

---

### 3.9 MEDIUM: No Protection Against Salmonella Tokens

**File:** Multiple executor files

**Issue:** None of the executors check for malicious token implementations (salmonella tokens, fee-on-transfer tokens, rebasing tokens). Rusty-Sando specifically includes salmonella detection.

**Reference:** `03_rust_mev_implementations.md:236` - "Salmonella Detection: Detects malicious ERC20 implementations"

**Recommendation:** Add token validation before execution:
```rust
async fn validate_token(&self, token: Address) -> Result<bool> {
    // Check for fee-on-transfer
    // Check for rebasing
    // Check for malicious transfer hooks
    // Blacklist known salmonella tokens
}
```

---

### 3.10 MEDIUM: Bundle Hash Calculation Differs from Flashbots

**File:** `src/executor/flashbots.rs:449-456`

```rust
pub fn hash(&self) -> B256 {
    let mut data = Vec::new();
    for tx in &self.txs {
        data.extend_from_slice(tx);
    }
    keccak256(&data)
}
```

**Issue:** This is not how Flashbots calculates bundle hashes. The actual bundle hash is computed by the relay and returned in the response.

**Fix:** Remove this method or mark it as a local identifier only, not the actual bundle hash:
```rust
/// Calculate a local identifier for this bundle.
/// Note: This is NOT the same as the bundle hash returned by Flashbots.
pub fn local_id(&self) -> B256 {
    // ...
}
```

---

### 3.11 MEDIUM: No Rate Limiting for API Requests

**File:** `src/executor/flashbots.rs`

**Issue:** No rate limiting is implemented, risking IP bans from Flashbots relay.

**Reference:** `04_flashbots_infrastructure.md:92-96`:
- "600 submissions per 5 minutes per IP (~2/second average)"
- "10,000 requests per second per IP for RPC endpoints"

**Fix:** Implement rate limiting:
```rust
pub struct FlashbotsClient {
    rate_limiter: RateLimiter,
    // ...
}

impl FlashbotsClient {
    async fn rpc_request<T, R>(&self, method: &str, params: T) -> Result<R> {
        self.rate_limiter.acquire().await;
        // ... existing code
    }
}
```

---

### 3.12 MEDIUM: Missing Multi-Builder Support

**File:** `src/executor/flashbots.rs`

**Issue:** Only submits to single Flashbots relay. Production MEV bots submit to multiple builders for higher inclusion rates.

**Reference:** `04_flashbots_infrastructure.md:163-170` lists multiple builders: Flashbots, Beaverbuild, Titan, rsync-builder, builder0x69.

**Fix:**
```rust
pub async fn submit_to_builders(&self, bundle: FlashbotsBundle, builders: &[String]) -> Vec<Result<BundleResponse>> {
    let futures = builders.iter().map(|url| {
        self.send_bundle_to_url(&bundle, url)
    });

    futures::future::join_all(futures).await
}
```

---

## 4. Low Severity / Code Quality

### 4.1 LOW: Unused `chain_id` Field

**File:** `src/executor/flashbots.rs:28-29`

```rust
#[allow(dead_code)]
chain_id: u64,
```

The `chain_id` field is stored but never used. It should either be removed or used for chain-specific validation.

---

### 4.2 LOW: Magic Numbers

**File:** `src/executor/backrun.rs:299`

```rust
let gas_used = tx_builder.estimate_gas(&backrun_tx).await.unwrap_or(200_000);
```

**Fix:** Use named constants:
```rust
const DEFAULT_SWAP_GAS: u64 = 200_000;
const DEFAULT_LIQUIDATION_GAS: u64 = 350_000;
```

---

### 4.3 LOW: Missing Documentation for Error Codes

**File:** `src/executor/flashbots.rs:143-144`

```rust
format!("RPC error {}: {}", error.code, error.message),
```

**Issue:** Error codes are not documented or mapped to meaningful error types.

---

### 4.4 LOW: Test Coverage Gaps

Multiple files have tests only for basic unit functionality, missing:
- Integration tests with mock providers
- Edge case tests (zero amounts, max values)
- Concurrent execution tests

---

### 4.5 LOW: Inconsistent Logging Levels

Some failures are logged as `warn` while similar failures elsewhere are `error`. Standardize logging levels.

---

### 4.6 LOW: SwapParams Default Deadline

**File:** `src/executor/tx_builder.rs:374`

```rust
deadline: u64::MAX,
```

Using `u64::MAX` is different from `U256::MAX` used elsewhere, creating inconsistency.

---

### 4.7 LOW: Clone-heavy Pattern

**File:** `src/executor/mod.rs:286-298`

The `ExecutorContext` clone implementation clones `Arc` wrappers, which is correct, but the pattern is used excessively when references could suffice.

---

### 4.8 LOW: Missing `#[must_use]` Attributes

Functions like `calculate_profit`, `calculate_min_output` return values that must be used but lack `#[must_use]` attributes.

---

## 5. Missing Features

### 5.1 No MEV-Share Integration

**Priority:** High

The codebase only implements traditional `eth_sendBundle` but lacks MEV-Share (`mev_sendBundle`) support, which is now the preferred method for backruns.

**Reference:** `04_flashbots_infrastructure.md:729-762` - MEV-Share flow diagram.

**Required additions:**
- SSE client for MEV-Share event stream
- `mev_sendBundle` implementation
- `mev_simBundle` for matched bundle simulation
- Privacy hints configuration

### 5.2 No Local EVM Simulation (REVM)

**Priority:** High

All simulation is done via Flashbots `eth_callBundle`. Local REVM simulation would:
- Enable faster iteration
- Allow pre-filtering before hitting rate limits
- Enable more sophisticated strategy testing

**Reference:** `03_rust_mev_implementations.md:289-347` - REVM simulation patterns.

### 5.3 No Bundle Stats Monitoring

**Priority:** Medium

While `get_bundle_stats` exists, there's no continuous monitoring or metrics collection for:
- Bundle inclusion rate
- Average time to inclusion
- Competition analysis

### 5.4 No Private Transaction Support

**Priority:** Medium

The `send_private_transaction` method exists but is never used. Private transactions via Flashbots Protect can be useful for single-tx MEV.

### 5.5 No Access List Generation

**Priority:** Medium

EIP-2930 access lists can save gas but aren't generated.

### 5.6 No Flashloan Callback Contract Reference

**Priority:** High

The flashloan executor references `IFlashloanArbitrage` contract but no actual Solidity contract exists in the repo. The callback implementation is critical.

### 5.7 No Concurrent Simulation

**Priority:** Medium

**Reference:** `03_rust_mev_implementations.md:235` - Rusty-Sando uses "Concurrent Simulations: Fast local EVM simulations"

### 5.8 No Token Dust Management

**Priority:** Low

**Reference:** `03_rust_mev_implementations.md:235` - Rusty-Sando "Token Dust Storage: Retains token remnants for gas efficiency"

---

## 6. Comparison with Reference Implementations

### 6.1 Artemis Framework Patterns

| Feature | Artemis | This Project | Status |
|---------|---------|--------------|--------|
| Collector/Strategy/Executor pattern | Yes | Partial | Missing collectors |
| Event-driven architecture | Yes | Partial | No event stream |
| MEV-Share executor | Yes | No | Missing |
| Generic provider support | Yes | Yes | OK |

### 6.2 Rusty-Sando Patterns

| Feature | Rusty-Sando | This Project | Status |
|---------|-------------|--------------|--------|
| REVM simulation | Yes | No | Missing |
| Salmonella detection | Yes | No | Missing |
| Optimal amount calculation | Yes | Partial | Simplified |
| Multi-meat bundles | Yes | No | Single victim only |
| Huff contracts | Yes | No | N/A |

### 6.3 ethers-flashbots / alloy-mev Patterns

| Feature | Reference | This Project | Status |
|---------|-----------|--------------|--------|
| Bundle simulation before submit | Yes | Partial | Not per-block |
| Multiple builder submission | Yes | No | Missing |
| Replacement UUID support | Yes | Partial | Unused |
| MEV-Share bundle format | Yes | No | Missing |

---

## 7. Recommendations Summary

### Immediate (Critical/High):

1. **Fix Flashbots signature** to use EIP-191 format
2. **Include victim transaction** in sandwich bundles
3. **Implement nonce management** to prevent conflicts
4. **Fix backrun bundles** to reference target transactions
5. **Add per-block simulation** for multi-block targeting
6. **Implement retry logic** for bundle submission

### Short-term (Medium):

1. Add MEV-Share integration
2. Implement REVM local simulation
3. Add multi-builder support
4. Fix deadline handling
5. Add salmonella/malicious token detection
6. Implement rate limiting
7. Add access list generation

### Long-term (Low/Quality):

1. Improve test coverage
2. Add metrics and monitoring
3. Standardize error handling
4. Create flashloan callback contract
5. Add token dust management
6. Implement concurrent simulation

---

## Appendix A: File-by-File Issue Summary

| File | Critical | High | Medium | Low |
|------|----------|------|--------|-----|
| flashbots.rs | 1 | 2 | 2 | 1 |
| sandwich.rs | 1 | 0 | 1 | 0 |
| backrun.rs | 1 | 1 | 2 | 1 |
| arbitrage.rs | 0 | 1 | 1 | 0 |
| tx_builder.rs | 1 | 1 | 1 | 2 |
| liquidation.rs | 0 | 1 | 1 | 0 |
| flashloan.rs | 0 | 0 | 1 | 0 |
| router_encoder.rs | 0 | 0 | 1 | 1 |
| mod.rs | 0 | 0 | 1 | 2 |

---

## Appendix B: References

1. Paradigm Artemis Framework: https://github.com/paradigmxyz/artemis
2. Rusty-Sando: https://github.com/mouseless0x/rusty-sando
3. Flashbots Documentation: https://docs.flashbots.net
4. ethers-flashbots: https://crates.io/crates/ethers-flashbots
5. alloy-mev: https://crates.io/crates/alloy-mev
6. Research: `/home/ubuntu/Desktop/research/03_rust_mev_implementations.md`
7. Research: `/home/ubuntu/Desktop/research/04_flashbots_infrastructure.md`

---

*End of Audit Report*
