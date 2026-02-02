//! Opportunity Detection Modules for MEV Bot
//!
//! This module provides various detectors for identifying MEV opportunities:
//! - Price discrepancy (cross-DEX arbitrage)
//! - Multi-hop arbitrage (triangular, etc.)
//! - Sandwich opportunities
//! - Liquidity events (new pools, large removals)

pub mod liquidity_event;
pub mod multihop;
pub mod price_discrepancy;
pub mod sandwich_detector;

use alloy::primitives::{Address, Bytes, U256};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

pub use liquidity_event::LiquidityEventDetector;
pub use multihop::MultihopDetector;
pub use price_discrepancy::PriceDiscrepancyDetector;
pub use sandwich_detector::SandwichDetector;

use crate::error::Result;

/// Type of MEV opportunity detected
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OpportunityType {
    /// Cross-DEX arbitrage (buy low, sell high)
    Arbitrage,
    /// Multi-hop arbitrage (triangular, etc.)
    MultihopArbitrage,
    /// Sandwich attack opportunity
    Sandwich,
    /// Just-in-time liquidity
    JitLiquidity,
    /// Liquidation opportunity
    Liquidation,
    /// New pool arbitrage
    NewPoolArbitrage,
    /// Liquidity imbalance from removal
    LiquidityImbalance,
}

/// Priority level for opportunity execution
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Priority {
    Low = 1,
    Medium = 2,
    High = 3,
    Critical = 4,
}

/// Represents a detected MEV opportunity
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Opportunity {
    /// Unique identifier
    pub id: String,
    /// Type of opportunity
    pub opportunity_type: OpportunityType,
    /// Priority level
    pub priority: Priority,
    /// Estimated profit in Wei
    pub estimated_profit: U256,
    /// Estimated gas cost in Wei
    pub estimated_gas_cost: U256,
    /// Net profit after gas
    pub net_profit: U256,
    /// Tokens involved in the opportunity
    pub tokens: Vec<Address>,
    /// Pools/DEXes involved
    pub pools: Vec<PoolInfo>,
    /// Swap path for execution
    pub swap_path: Vec<SwapStep>,
    /// Target transaction (for sandwich)
    pub target_tx: Option<TargetTransaction>,
    /// Block deadline (opportunity expires after this block)
    pub deadline_block: Option<u64>,
    /// Timestamp when detected
    pub detected_at: DateTime<Utc>,
    /// Confidence score (0.0 - 1.0)
    pub confidence: f64,
    /// Additional metadata
    pub metadata: HashMap<String, String>,
}

/// Information about a pool involved in the opportunity
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolInfo {
    /// Pool address
    pub address: Address,
    /// DEX name (e.g., "uniswap_v2", "sushiswap")
    pub dex: String,
    /// Token0 address
    pub token0: Address,
    /// Token1 address
    pub token1: Address,
    /// Current reserves or liquidity
    pub reserve0: U256,
    pub reserve1: U256,
    /// Fee in basis points
    pub fee_bps: u32,
}

/// A single swap step in an arbitrage path
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwapStep {
    /// Pool to use
    pub pool: Address,
    /// DEX identifier
    pub dex: String,
    /// Token to sell
    pub token_in: Address,
    /// Token to receive
    pub token_out: Address,
    /// Amount to swap (0 means use output from previous step)
    pub amount_in: U256,
    /// Minimum amount out (slippage protection)
    pub min_amount_out: U256,
}

/// Target transaction for sandwich attacks
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetTransaction {
    /// Transaction hash
    pub hash: Bytes,
    /// Sender address
    pub from: Address,
    /// Contract being called
    pub to: Address,
    /// Value being sent
    pub value: U256,
    /// Gas price
    pub gas_price: U256,
    /// Decoded swap parameters
    pub swap_params: SwapParams,
}

/// Decoded swap parameters from a pending transaction
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwapParams {
    /// Token being sold
    pub token_in: Address,
    /// Token being bought
    pub token_out: Address,
    /// Amount being swapped
    pub amount_in: U256,
    /// Minimum amount out (slippage)
    pub min_amount_out: U256,
    /// Recipient address
    pub recipient: Address,
    /// Deadline timestamp
    pub deadline: U256,
}

/// Event types that can trigger detection
#[derive(Debug, Clone)]
pub enum MonitorEvent {
    /// New block received
    NewBlock {
        number: u64,
        timestamp: u64,
        base_fee: U256,
    },
    /// Price update on a pool
    PriceUpdate {
        pool: Address,
        dex: String,
        token0: Address,
        token1: Address,
        reserve0: U256,
        reserve1: U256,
        price: f64,
    },
    /// Pending transaction in mempool
    PendingTransaction {
        hash: Bytes,
        from: Address,
        to: Address,
        value: U256,
        input: Bytes,
        gas_price: U256,
        gas_limit: u64,
    },
    /// Swap executed on-chain
    SwapExecuted {
        pool: Address,
        sender: Address,
        amount0_in: U256,
        amount1_in: U256,
        amount0_out: U256,
        amount1_out: U256,
        tx_hash: Bytes,
    },
    /// Liquidity added to pool
    LiquidityAdded {
        pool: Address,
        provider: Address,
        amount0: U256,
        amount1: U256,
        liquidity: U256,
    },
    /// Liquidity removed from pool
    LiquidityRemoved {
        pool: Address,
        provider: Address,
        amount0: U256,
        amount1: U256,
        liquidity: U256,
    },
    /// New pool created
    PoolCreated {
        factory: Address,
        pool: Address,
        token0: Address,
        token1: Address,
        fee: u32,
    },
}

/// Pool registry for tracking known pools
pub struct PoolRegistry {
    /// Pools indexed by address
    pools: DashMap<Address, RegisteredPool>,
    /// Pools indexed by token pair
    pairs: DashMap<(Address, Address), Vec<Address>>,
}

/// A registered pool with cached data
#[derive(Debug, Clone)]
pub struct RegisteredPool {
    pub address: Address,
    pub dex: String,
    pub token0: Address,
    pub token1: Address,
    pub reserve0: U256,
    pub reserve1: U256,
    pub fee_bps: u32,
    pub last_updated: DateTime<Utc>,
    /// Block number when reserves were last updated (for staleness check)
    pub last_block: u64,
}

impl PoolRegistry {
    pub fn new() -> Self {
        Self {
            pools: DashMap::new(),
            pairs: DashMap::new(),
        }
    }

    /// Register a new pool
    pub fn register(&self, pool: RegisteredPool) {
        let address = pool.address;
        let token0 = pool.token0;
        let token1 = pool.token1;

        self.pools.insert(address, pool);

        // Index by both orderings of the pair
        self.pairs
            .entry((token0, token1))
            .or_default()
            .push(address);
        if token0 != token1 {
            self.pairs
                .entry((token1, token0))
                .or_default()
                .push(address);
        }
    }

    /// Get pool by address
    pub fn get(&self, address: &Address) -> Option<RegisteredPool> {
        self.pools.get(address).map(|p| p.clone())
    }

    /// Get all pools for a token pair
    pub fn get_pools_for_pair(&self, token0: Address, token1: Address) -> Vec<RegisteredPool> {
        self.pairs
            .get(&(token0, token1))
            .map(|addrs| {
                addrs
                    .iter()
                    .filter_map(|a| self.pools.get(a).map(|p| p.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Update pool reserves atomically with block number check to prevent stale updates
    pub fn update_reserves(&self, address: Address, reserve0: U256, reserve1: U256) {
        self.update_reserves_at_block(address, reserve0, reserve1, 0);
    }

    /// Update pool reserves with block number for staleness protection
    /// Only updates if block_number >= pool's last_block (prevents reorg issues)
    pub fn update_reserves_at_block(&self, address: Address, reserve0: U256, reserve1: U256, block_number: u64) {
        if let Some(mut pool) = self.pools.get_mut(&address) {
            // Only update if this is newer data (or block is 0 for backward compat)
            if block_number == 0 || block_number >= pool.last_block {
                pool.reserve0 = reserve0;
                pool.reserve1 = reserve1;
                pool.last_updated = Utc::now();
                if block_number > 0 {
                    pool.last_block = block_number;
                }
            }
        }
    }

    /// Get all registered pools
    pub fn all_pools(&self) -> Vec<RegisteredPool> {
        self.pools.iter().map(|p| p.clone()).collect()
    }

    /// Get all unique tokens
    pub fn all_tokens(&self) -> Vec<Address> {
        let mut tokens: Vec<Address> = self
            .pools
            .iter()
            .flat_map(|p| vec![p.token0, p.token1])
            .collect();
        tokens.sort();
        tokens.dedup();
        tokens
    }
}

impl Default for PoolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Price cache for quick lookups
pub struct PriceCache {
    /// Token prices in USD (or ETH)
    prices: DashMap<Address, f64>,
    /// Last update time per token
    last_updated: DashMap<Address, DateTime<Utc>>,
}

impl PriceCache {
    pub fn new() -> Self {
        Self {
            prices: DashMap::new(),
            last_updated: DashMap::new(),
        }
    }

    /// Update price for a token
    pub fn update(&self, token: Address, price: f64) {
        self.prices.insert(token, price);
        self.last_updated.insert(token, Utc::now());
    }

    /// Get price for a token
    pub fn get(&self, token: &Address) -> Option<f64> {
        self.prices.get(token).map(|p| *p)
    }

    /// Get price if fresh (within max_age seconds)
    pub fn get_fresh(&self, token: &Address, max_age_secs: i64) -> Option<f64> {
        let updated = self.last_updated.get(token)?;
        let age = Utc::now().signed_duration_since(*updated);
        if age.num_seconds() <= max_age_secs {
            self.prices.get(token).map(|p| *p)
        } else {
            None
        }
    }
}

impl Default for PriceCache {
    fn default() -> Self {
        Self::new()
    }
}

/// Configuration for detectors
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetectorConfig {
    /// Minimum profit threshold in Wei
    pub min_profit_wei: U256,
    /// Minimum profit threshold in USD
    pub min_profit_usd: f64,
    /// Price discrepancy threshold (0.003 = 0.3%)
    pub price_discrepancy_threshold: f64,
    /// Maximum hops for multi-hop arbitrage
    pub max_hops: usize,
    /// Minimum sandwich target size in USD
    pub min_sandwich_target_usd: f64,
    /// Maximum sandwich target size in USD
    pub max_sandwich_target_usd: f64,
    /// Gas price multiplier for cost estimation
    pub gas_price_multiplier: f64,
    /// Base gas for swap (approximately)
    pub base_swap_gas: u64,
    /// Whether to detect sandwich opportunities
    pub enable_sandwich: bool,
    /// Whether to detect arbitrage opportunities
    pub enable_arbitrage: bool,
    /// Whether to detect multi-hop arbitrage
    pub enable_multihop: bool,
    /// Whether to detect liquidity events
    pub enable_liquidity_events: bool,
}

impl Default for DetectorConfig {
    fn default() -> Self {
        Self {
            min_profit_wei: U256::from(1_000_000_000_000_000u64), // 0.001 ETH
            min_profit_usd: 1.0,
            price_discrepancy_threshold: 0.003, // 0.3%
            max_hops: 4,
            min_sandwich_target_usd: 500.0,
            max_sandwich_target_usd: 50_000.0,
            gas_price_multiplier: 1.2,
            base_swap_gas: 150_000,
            enable_sandwich: true,
            enable_arbitrage: true,
            enable_multihop: true,
            enable_liquidity_events: true,
        }
    }
}

/// Context provided to detectors
pub struct DetectorContext {
    /// Registry of known pools
    pub pool_registry: Arc<PoolRegistry>,
    /// Price cache
    pub price_cache: Arc<PriceCache>,
    /// Detector configuration
    pub config: Arc<DetectorConfig>,
    /// Current gas price
    pub gas_price: Arc<RwLock<U256>>,
    /// Current block number
    pub current_block: Arc<RwLock<u64>>,
}

impl DetectorContext {
    pub fn new(
        pool_registry: Arc<PoolRegistry>,
        price_cache: Arc<PriceCache>,
        config: Arc<DetectorConfig>,
    ) -> Self {
        Self {
            pool_registry,
            price_cache,
            config,
            gas_price: Arc::new(RwLock::new(U256::from(30_000_000_000u64))), // 30 gwei default
            current_block: Arc::new(RwLock::new(0)),
        }
    }

    pub async fn update_gas_price(&self, price: U256) {
        let mut gp = self.gas_price.write().await;
        *gp = price;
    }

    pub async fn update_block(&self, block: u64) {
        let mut b = self.current_block.write().await;
        *b = block;
    }

    pub async fn get_gas_price(&self) -> U256 {
        *self.gas_price.read().await
    }

    pub async fn get_current_block(&self) -> u64 {
        *self.current_block.read().await
    }
}

/// Detector trait for opportunity detection
#[async_trait]
pub trait Detector: Send + Sync {
    /// Name of the detector
    fn name(&self) -> &str;

    /// Detect opportunities from an event
    async fn detect(
        &self,
        event: &MonitorEvent,
        ctx: &DetectorContext,
    ) -> Result<Vec<Opportunity>>;

    /// Check if detector is enabled
    fn is_enabled(&self, config: &DetectorConfig) -> bool;
}

/// Manager for running multiple detectors
pub struct DetectorManager {
    /// Registered detectors
    detectors: Vec<Box<dyn Detector>>,
    /// Shared context
    ctx: Arc<DetectorContext>,
    /// Detected opportunities (recent)
    recent_opportunities: Arc<RwLock<Vec<Opportunity>>>,
    /// Max recent opportunities to keep
    max_recent: usize,
}

impl DetectorManager {
    pub fn new(ctx: Arc<DetectorContext>) -> Self {
        Self {
            detectors: Vec::new(),
            ctx,
            recent_opportunities: Arc::new(RwLock::new(Vec::new())),
            max_recent: 1000,
        }
    }

    /// Add a detector
    pub fn add_detector(&mut self, detector: Box<dyn Detector>) {
        tracing::info!("Adding detector: {}", detector.name());
        self.detectors.push(detector);
    }

    /// Create manager with all default detectors
    pub fn with_default_detectors(ctx: Arc<DetectorContext>) -> Self {
        let mut manager = Self::new(ctx);

        // Add all detectors
        manager.add_detector(Box::new(PriceDiscrepancyDetector::new()));
        manager.add_detector(Box::new(MultihopDetector::new()));
        manager.add_detector(Box::new(SandwichDetector::new()));
        manager.add_detector(Box::new(LiquidityEventDetector::new()));

        manager
    }

    /// Process an event through all detectors
    pub async fn process_event(&self, event: &MonitorEvent) -> Result<Vec<Opportunity>> {
        let mut all_opportunities = Vec::new();

        for detector in &self.detectors {
            if !detector.is_enabled(&self.ctx.config) {
                continue;
            }

            match detector.detect(event, &self.ctx).await {
                Ok(opportunities) => {
                    if !opportunities.is_empty() {
                        tracing::info!(
                            "Detector {} found {} opportunities",
                            detector.name(),
                            opportunities.len()
                        );
                        all_opportunities.extend(opportunities);
                    }
                }
                Err(e) => {
                    tracing::warn!("Detector {} failed: {}", detector.name(), e);
                }
            }
        }

        // Store recent opportunities
        if !all_opportunities.is_empty() {
            let mut recent = self.recent_opportunities.write().await;
            recent.extend(all_opportunities.clone());
            // Trim to max size
            if recent.len() > self.max_recent {
                let drain_count = recent.len() - self.max_recent;
                recent.drain(0..drain_count);
            }
        }

        // Sort by estimated profit descending
        all_opportunities.sort_by(|a, b| b.estimated_profit.cmp(&a.estimated_profit));

        Ok(all_opportunities)
    }

    /// Get recent opportunities
    pub async fn get_recent_opportunities(&self) -> Vec<Opportunity> {
        self.recent_opportunities.read().await.clone()
    }

    /// Clear recent opportunities
    pub async fn clear_recent(&self) {
        self.recent_opportunities.write().await.clear();
    }
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Calculate price impact for a swap
/// Returns the price impact as a decimal (0.01 = 1%)
pub fn calculate_price_impact(amount_in: U256, reserve_in: U256, reserve_out: U256) -> f64 {
    if reserve_in.is_zero() || reserve_out.is_zero() {
        return 1.0; // 100% impact for empty pools
    }

    // For CPMM: price_impact = amount_in / (reserve_in + amount_in)
    let amount_f64 = u256_to_f64(amount_in);
    let reserve_f64 = u256_to_f64(reserve_in);

    amount_f64 / (reserve_f64 + amount_f64)
}

/// Calculate optimal frontrun amount for sandwich attack
/// Formula: Vf* = sqrt(Vv * L * (1-fee)) - L*(1-fee)
/// Where: Vv = victim amount, L = liquidity, fee = swap fee
pub fn calculate_optimal_frontrun(
    victim_amount: U256,
    reserve_in: U256,
    _reserve_out: U256,
    fee_bps: u32,
) -> U256 {
    // fee_bps: 30 = 0.3%, so fee = 0.003
    let fee = (fee_bps as f64) / 10_000.0;
    let one_minus_fee = 1.0 - fee;

    let vv = u256_to_f64(victim_amount);
    let reserve_in_f64 = u256_to_f64(reserve_in);

    // Use geometric mean of reserves as liquidity proxy
    let l = reserve_in_f64;

    // Vf* = sqrt(Vv * L * (1-fee)) - L*(1-fee)
    let sqrt_term = (vv * l * one_minus_fee).sqrt();
    let linear_term = l * one_minus_fee;

    if sqrt_term > linear_term {
        let optimal = sqrt_term - linear_term;
        // Cap at reasonable percentage of reserves
        let max_frontrun = reserve_in_f64 * 0.1; // Max 10% of reserves
        f64_to_u256(optimal.min(max_frontrun))
    } else {
        U256::ZERO
    }
}

/// Estimate sandwich profit using CPMM model
/// Returns (profit, frontrun_output, backrun_output)
pub fn estimate_sandwich_profit(
    victim_amount: U256,
    frontrun_amount: U256,
    reserve_in: U256,
    reserve_out: U256,
    fee_bps: u32,
) -> (U256, U256, U256) {
    let fee = (fee_bps as f64) / 10_000.0;
    let one_minus_fee = 1.0 - fee;

    let r_in = u256_to_f64(reserve_in);
    let r_out = u256_to_f64(reserve_out);
    let v_f = u256_to_f64(frontrun_amount);
    let v_v = u256_to_f64(victim_amount);

    // Step 1: Frontrun swap (we buy token_out)
    // amount_out = r_out * v_f * (1-fee) / (r_in + v_f * (1-fee))
    let frontrun_out = r_out * v_f * one_minus_fee / (r_in + v_f * one_minus_fee);

    // New reserves after frontrun
    let r_in_1 = r_in + v_f;
    let r_out_1 = r_out - frontrun_out;

    // Step 2: Victim swap (they also buy token_out, pushing price up more)
    let victim_out = r_out_1 * v_v * one_minus_fee / (r_in_1 + v_v * one_minus_fee);

    // New reserves after victim
    let r_in_2 = r_in_1 + v_v;
    let r_out_2 = r_out_1 - victim_out;

    // Step 3: Backrun swap (we sell token_out back to token_in)
    // We sell all the token_out we got from frontrun
    let backrun_out = r_in_2 * frontrun_out * one_minus_fee / (r_out_2 + frontrun_out * one_minus_fee);

    // Profit = backrun_out - frontrun_amount
    let profit = if backrun_out > v_f {
        backrun_out - v_f
    } else {
        0.0
    };

    (
        f64_to_u256(profit),
        f64_to_u256(frontrun_out),
        f64_to_u256(backrun_out),
    )
}

/// Calculate output amount for a CPMM swap
/// fee_bps: fee in basis points (e.g., 30 = 0.30%)
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

    // Fee sanity check (max 10% = 1000 bps to catch malformed data)
    if fee_bps > 1000 {
        tracing::warn!("Unusually high fee: {} bps, clamping to 1000", fee_bps);
        return U256::ZERO;
    }

    // amount_out = reserve_out * amount_in * (1 - fee) / (reserve_in + amount_in * (1 - fee))
    let fee = (fee_bps as f64) / 10_000.0;
    let one_minus_fee = 1.0 - fee;

    let a_in = u256_to_f64(amount_in);
    let r_in = u256_to_f64(reserve_in);
    let r_out = u256_to_f64(reserve_out);

    let amount_out = r_out * a_in * one_minus_fee / (r_in + a_in * one_minus_fee);

    f64_to_u256(amount_out)
}

/// Calculate input amount needed to get desired output (CPMM)
pub fn calculate_amount_in(
    amount_out: U256,
    reserve_in: U256,
    reserve_out: U256,
    fee_bps: u32,
) -> U256 {
    if amount_out.is_zero() || reserve_in.is_zero() || reserve_out.is_zero() {
        return U256::ZERO;
    }

    if amount_out >= reserve_out {
        return U256::MAX; // Cannot get more than reserves
    }

    let fee = (fee_bps as f64) / 10_000.0;
    let one_minus_fee = 1.0 - fee;

    let a_out = u256_to_f64(amount_out);
    let r_in = u256_to_f64(reserve_in);
    let r_out = u256_to_f64(reserve_out);

    // amount_in = reserve_in * amount_out / ((reserve_out - amount_out) * (1 - fee))
    let amount_in = r_in * a_out / ((r_out - a_out) * one_minus_fee);

    f64_to_u256(amount_in)
}

/// Calculate price from reserves
pub fn calculate_price(reserve0: U256, reserve1: U256, decimals0: u8, decimals1: u8) -> f64 {
    if reserve0.is_zero() || reserve1.is_zero() {
        return 0.0;
    }

    let r0 = u256_to_f64(reserve0);
    let r1 = u256_to_f64(reserve1);

    // Price of token0 in terms of token1
    let decimal_adjustment = 10f64.powi((decimals1 as i32) - (decimals0 as i32));
    (r1 / r0) * decimal_adjustment
}

/// Estimate gas cost for a multi-swap transaction
pub fn estimate_gas_cost(num_swaps: usize, base_gas: u64, gas_price: U256) -> U256 {
    // Base gas per swap is ~150k, with some overhead
    let total_gas = base_gas + (num_swaps as u64 - 1) * 100_000;
    U256::from(total_gas) * gas_price
}

/// Generate a unique opportunity ID
pub fn generate_opportunity_id(opp_type: OpportunityType, tokens: &[Address]) -> String {
    use std::time::{SystemTime, UNIX_EPOCH};

    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock is before Unix epoch")
        .as_nanos();

    let token_str: String = tokens
        .iter()
        .take(2)
        .map(|t| format!("{:x}", t).chars().take(8).collect::<String>())
        .collect::<Vec<_>>()
        .join("-");

    format!("{:?}-{}-{}", opp_type, token_str, timestamp)
}

// ============================================================================
// Utility Functions
// ============================================================================

/// Convert U256 to f64 with proper handling of large values
/// For MEV calculations, we typically work with wei amounts that fit in u128
/// For values > u128::MAX, uses logarithmic scaling to preserve relative precision
pub fn u256_to_f64(val: U256) -> f64 {
    if val.is_zero() {
        return 0.0;
    }

    // For values that fit in u128, use direct conversion (most common case)
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
    if val.is_infinite() {
        return U256::MAX;
    }

    // Handle very large numbers - use u128::MAX as practical limit for most MEV operations
    if val >= 2.0f64.powi(128) {
        // For extremely large values, try to preserve some precision
        if val >= 2.0f64.powi(256) {
            return U256::MAX;
        }
        // Scale down, convert, then scale back up
        let exp = (val.log2() - 64.0).floor() as i32;
        let mantissa = (val / 2f64.powi(exp)) as u128;
        return U256::from(mantissa) << exp as usize;
    }

    // Safe conversion for values that fit in u128
    U256::from(val as u128)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_price_impact() {
        let amount = U256::from(1_000_000_000_000_000_000u64); // 1 ETH
        let reserve = U256::from(100_000_000_000_000_000_000u128); // 100 ETH

        let impact = calculate_price_impact(amount, reserve, reserve);
        assert!(impact > 0.009 && impact < 0.011); // ~1% impact
    }

    #[test]
    fn test_amount_out() {
        let amount_in = U256::from(1_000_000_000_000_000_000u64); // 1 ETH
        let reserve_in = U256::from(100_000_000_000_000_000_000u128); // 100 ETH
        let reserve_out = U256::from(100_000_000_000_000_000_000u128); // 100 tokens

        let amount_out = calculate_amount_out(amount_in, reserve_in, reserve_out, 30);
        // Should be slightly less than 1 due to fees and price impact
        assert!(amount_out < U256::from(1_000_000_000_000_000_000u64));
        assert!(amount_out > U256::from(900_000_000_000_000_000u64));
    }

    #[test]
    fn test_u256_f64_conversion() {
        let original = U256::from(1_000_000_000_000_000_000u64);
        let f64_val = u256_to_f64(original);
        let back = f64_to_u256(f64_val);

        // Should be approximately equal (some precision loss expected)
        let diff = if original > back {
            original - back
        } else {
            back - original
        };
        assert!(diff < U256::from(1000u64)); // Less than 1000 wei difference
    }
}
