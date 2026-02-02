//! Liquidity Event Detector
//!
//! Detects MEV opportunities arising from liquidity events:
//! - New pool creation (early arbitrage)
//! - Large liquidity removal (imbalance opportunities)
//! - Suspicious pools (potential honeypots)

use alloy::primitives::{Address, U256};
use async_trait::async_trait;
use chrono::{DateTime, Duration, Utc};
use dashmap::DashMap;
use std::collections::HashMap;

use super::{
    calculate_amount_out, estimate_gas_cost, generate_opportunity_id,
    u256_to_f64, Detector, DetectorConfig, DetectorContext, MonitorEvent, Opportunity,
    OpportunityType, PoolInfo, Priority, RegisteredPool, SwapStep,
};
use crate::error::Result;

/// Detector for liquidity-related MEV opportunities
pub struct LiquidityEventDetector {
    /// Name of this detector
    name: String,
    /// Track recently created pools
    recent_pools: DashMap<Address, PoolCreationInfo>,
    /// Track suspicious activity
    suspicious_pools: DashMap<Address, SuspiciousFlags>,
    /// Minimum liquidity for new pool to be interesting
    min_new_pool_liquidity: U256,
    /// Threshold for "large" liquidity removal (percentage)
    large_removal_threshold: f64,
    /// How long to track new pools
    new_pool_window: Duration,
}

impl LiquidityEventDetector {
    pub fn new() -> Self {
        Self {
            name: "liquidity_event".to_string(),
            recent_pools: DashMap::new(),
            suspicious_pools: DashMap::new(),
            min_new_pool_liquidity: U256::from(10_000_000_000_000_000_000u128), // 10 ETH equivalent
            large_removal_threshold: 0.1, // 10% of liquidity
            new_pool_window: Duration::hours(1),
        }
    }

    pub fn with_min_liquidity(mut self, liquidity: U256) -> Self {
        self.min_new_pool_liquidity = liquidity;
        self
    }

    pub fn with_removal_threshold(mut self, threshold: f64) -> Self {
        self.large_removal_threshold = threshold;
        self
    }

    /// Check if a pool was recently created
    fn is_recently_created(&self, pool: &Address) -> bool {
        if let Some(info) = self.recent_pools.get(pool) {
            let age = Utc::now().signed_duration_since(info.created_at);
            age < self.new_pool_window
        } else {
            false
        }
    }

    /// Analyze a new pool for arbitrage opportunity
    fn analyze_new_pool(
        &self,
        pool: Address,
        token0: Address,
        token1: Address,
        ctx: &DetectorContext,
    ) -> Option<NewPoolAnalysis> {
        // Get existing pools for this pair
        let existing_pools = ctx.pool_registry.get_pools_for_pair(token0, token1);

        if existing_pools.is_empty() {
            // No existing pools to arb against
            return None;
        }

        // Get the new pool's data
        let new_pool = ctx.pool_registry.get(&pool)?;

        // Check liquidity threshold
        let liquidity = new_pool.reserve0 + new_pool.reserve1;
        if liquidity < self.min_new_pool_liquidity {
            tracing::debug!(
                "New pool {} has insufficient liquidity: {}",
                pool,
                liquidity
            );
            return None;
        }

        // Calculate new pool price
        let new_price = if new_pool.reserve0.is_zero() {
            return None;
        } else {
            u256_to_f64(new_pool.reserve1) / u256_to_f64(new_pool.reserve0)
        };

        // Compare with existing pools
        let mut best_arb: Option<(RegisteredPool, f64)> = None;

        for existing in &existing_pools {
            if existing.address == pool {
                continue;
            }

            if existing.reserve0.is_zero() {
                continue;
            }

            let existing_price = u256_to_f64(existing.reserve1) / u256_to_f64(existing.reserve0);
            let spread = ((new_price - existing_price) / existing_price).abs();

            // New pools often have mispriced initial liquidity
            if spread > 0.01 {
                // > 1% spread
                match &best_arb {
                    Some((_, best_spread)) if spread > *best_spread => {
                        best_arb = Some((existing.clone(), spread));
                    }
                    None => {
                        best_arb = Some((existing.clone(), spread));
                    }
                    _ => {}
                }
            }
        }

        best_arb.map(|(existing, spread)| NewPoolAnalysis {
            new_pool: new_pool.clone(),
            existing_pool: existing,
            price_spread: spread,
        })
    }

    /// Analyze liquidity removal for imbalance opportunity
    fn analyze_liquidity_removal(
        &self,
        pool: &Address,
        amount0: U256,
        amount1: U256,
        ctx: &DetectorContext,
    ) -> Option<RemovalAnalysis> {
        let registered = ctx.pool_registry.get(pool)?;

        // Calculate removal percentage
        let removal0 = u256_to_f64(amount0) / u256_to_f64(registered.reserve0);
        let removal1 = u256_to_f64(amount1) / u256_to_f64(registered.reserve1);
        let avg_removal = (removal0 + removal1) / 2.0;

        if avg_removal < self.large_removal_threshold {
            return None;
        }

        // Calculate new reserves after removal
        let new_reserve0 = registered.reserve0.saturating_sub(amount0);
        let new_reserve1 = registered.reserve1.saturating_sub(amount1);

        if new_reserve0.is_zero() || new_reserve1.is_zero() {
            // Pool will be empty, not interesting
            return None;
        }

        // Calculate new price vs old price
        let old_price = u256_to_f64(registered.reserve1) / u256_to_f64(registered.reserve0);
        let new_price = u256_to_f64(new_reserve1) / u256_to_f64(new_reserve0);
        let price_change = (new_price - old_price) / old_price;

        // Check for imbalanced removal (one side more than other)
        let imbalance = (removal0 - removal1).abs();

        if imbalance < 0.02 && price_change.abs() < 0.01 {
            // Balanced removal, no opportunity
            return None;
        }

        // Find other pools for arbitrage
        let other_pools = ctx.pool_registry.get_pools_for_pair(
            registered.token0,
            registered.token1,
        );

        let arb_target = other_pools
            .iter()
            .filter(|p| p.address != *pool)
            .max_by(|a, b| {
                let liq_a = a.reserve0 + a.reserve1;
                let liq_b = b.reserve0 + b.reserve1;
                liq_a.cmp(&liq_b)
            })
            .cloned();

        Some(RemovalAnalysis {
            pool: registered.clone(),
            removal_percentage: avg_removal,
            price_change,
            imbalance,
            new_reserve0,
            new_reserve1,
            arb_target,
        })
    }

    /// Check for honeypot indicators
    fn check_honeypot(&self, pool: &Address, ctx: &DetectorContext) -> HoneypotCheck {
        let mut flags = HoneypotCheck::default();

        if let Some(pool_data) = ctx.pool_registry.get(pool) {
            // Check 1: Very low liquidity
            let total_liquidity = pool_data.reserve0 + pool_data.reserve1;
            if total_liquidity < U256::from(1_000_000_000_000_000_000u128) {
                flags.low_liquidity = true;
                flags.risk_score += 20;
            }

            // Check 2: Extreme price imbalance
            let price_ratio = if !pool_data.reserve0.is_zero() {
                u256_to_f64(pool_data.reserve1) / u256_to_f64(pool_data.reserve0)
            } else {
                f64::INFINITY
            };

            if !(0.000001..=1_000_000.0).contains(&price_ratio) {
                flags.extreme_price = true;
                flags.risk_score += 30;
            }

            // Check 3: Recent suspicious activity
            if let Some(suspicious) = self.suspicious_pools.get(pool) {
                if suspicious.failed_sells > 0 {
                    flags.failed_sells = true;
                    flags.risk_score += 50;
                }
                if suspicious.high_tax_detected {
                    flags.high_tax = true;
                    flags.risk_score += 40;
                }
            }
        }

        flags
    }

    /// Record suspicious activity for a pool
    pub fn record_suspicious_activity(&self, pool: Address, activity: SuspiciousActivity) {
        let mut entry = self.suspicious_pools.entry(pool).or_default();

        match activity {
            SuspiciousActivity::FailedSell => {
                entry.failed_sells += 1;
            }
            SuspiciousActivity::HighTax(rate) => {
                entry.high_tax_detected = true;
                entry.tax_rate = entry.tax_rate.max(rate);
            }
            SuspiciousActivity::OwnerMint => {
                entry.owner_mint_detected = true;
            }
        }

        entry.last_activity = Utc::now();
    }

    /// Build opportunity for new pool arbitrage
    fn build_new_pool_opportunity(
        &self,
        analysis: &NewPoolAnalysis,
        gas_price: U256,
        min_profit: U256,
    ) -> Option<Opportunity> {
        // Determine direction: buy from cheaper, sell to more expensive
        let new_price = u256_to_f64(analysis.new_pool.reserve1)
            / u256_to_f64(analysis.new_pool.reserve0);
        let existing_price = u256_to_f64(analysis.existing_pool.reserve1)
            / u256_to_f64(analysis.existing_pool.reserve0);

        let (buy_pool, sell_pool) = if new_price < existing_price {
            (&analysis.new_pool, &analysis.existing_pool)
        } else {
            (&analysis.existing_pool, &analysis.new_pool)
        };

        // Calculate optimal trade size
        let buy_reserve = u256_to_f64(buy_pool.reserve0);
        let optimal_size = buy_reserve * 0.01; // Start with 1% of reserves

        let amount_in = super::f64_to_u256(optimal_size);

        // Calculate expected output
        let intermediate = calculate_amount_out(
            amount_in,
            buy_pool.reserve0,
            buy_pool.reserve1,
            buy_pool.fee_bps,
        );

        let final_amount = calculate_amount_out(
            intermediate,
            sell_pool.reserve1,
            sell_pool.reserve0,
            sell_pool.fee_bps,
        );

        if final_amount <= amount_in {
            return None;
        }

        let gross_profit = final_amount - amount_in;
        let gas_cost = estimate_gas_cost(2, 150_000, gas_price);

        if gross_profit <= gas_cost {
            return None;
        }

        let net_profit = gross_profit - gas_cost;
        if net_profit < min_profit {
            return None;
        }

        let tokens = vec![buy_pool.token0, buy_pool.token1];

        let swap_path = vec![
            SwapStep {
                pool: buy_pool.address,
                dex: buy_pool.dex.clone(),
                token_in: buy_pool.token0,
                token_out: buy_pool.token1,
                amount_in,
                min_amount_out: intermediate * U256::from(99) / U256::from(100),
            },
            SwapStep {
                pool: sell_pool.address,
                dex: sell_pool.dex.clone(),
                token_in: sell_pool.token1,
                token_out: sell_pool.token0,
                amount_in: intermediate,
                min_amount_out: amount_in, // At minimum get back input
            },
        ];

        let pools = vec![
            PoolInfo {
                address: buy_pool.address,
                dex: buy_pool.dex.clone(),
                token0: buy_pool.token0,
                token1: buy_pool.token1,
                reserve0: buy_pool.reserve0,
                reserve1: buy_pool.reserve1,
                fee_bps: buy_pool.fee_bps,
            },
            PoolInfo {
                address: sell_pool.address,
                dex: sell_pool.dex.clone(),
                token0: sell_pool.token0,
                token1: sell_pool.token1,
                reserve0: sell_pool.reserve0,
                reserve1: sell_pool.reserve1,
                fee_bps: sell_pool.fee_bps,
            },
        ];

        let priority = if net_profit > U256::from(1_000_000_000_000_000_000u128) {
            Priority::Critical
        } else if net_profit > U256::from(100_000_000_000_000_000u128) {
            Priority::High
        } else {
            Priority::Medium
        };

        let mut metadata = HashMap::new();
        metadata.insert("type".to_string(), "new_pool_arb".to_string());
        metadata.insert("spread".to_string(), format!("{:.4}", analysis.price_spread));
        metadata.insert("new_pool".to_string(), format!("{:?}", analysis.new_pool.address));

        Some(Opportunity {
            id: generate_opportunity_id(OpportunityType::NewPoolArbitrage, &tokens),
            opportunity_type: OpportunityType::NewPoolArbitrage,
            priority,
            estimated_profit: gross_profit,
            estimated_gas_cost: gas_cost,
            net_profit,
            tokens,
            pools,
            swap_path,
            target_tx: None,
            deadline_block: None,
            detected_at: Utc::now(),
            confidence: 0.7, // New pools are inherently riskier
            metadata,
        })
    }

    /// Build opportunity for liquidity removal
    fn build_removal_opportunity(
        &self,
        analysis: &RemovalAnalysis,
        gas_price: U256,
        min_profit: U256,
    ) -> Option<Opportunity> {
        let arb_target = analysis.arb_target.as_ref()?;

        // Calculate price after removal
        let post_removal_price = u256_to_f64(analysis.new_reserve1)
            / u256_to_f64(analysis.new_reserve0);
        let target_price = u256_to_f64(arb_target.reserve1)
            / u256_to_f64(arb_target.reserve0);

        let spread = ((post_removal_price - target_price) / target_price).abs();

        if spread < 0.003 {
            // Less than 0.3% spread
            return None;
        }

        // Determine direction
        let (buy_pool, sell_pool, buy_reserve0, buy_reserve1) = if post_removal_price < target_price {
            // Buy from affected pool (cheaper after removal)
            (
                &analysis.pool,
                arb_target,
                analysis.new_reserve0,
                analysis.new_reserve1,
            )
        } else {
            // Buy from target, sell to affected pool
            (
                arb_target,
                &analysis.pool,
                arb_target.reserve0,
                arb_target.reserve1,
            )
        };

        let optimal_size = u256_to_f64(buy_reserve0) * 0.02; // 2% of reserves
        let amount_in = super::f64_to_u256(optimal_size);

        let intermediate = calculate_amount_out(amount_in, buy_reserve0, buy_reserve1, buy_pool.fee_bps);

        let (sell_reserve0, sell_reserve1) = if sell_pool.address == analysis.pool.address {
            (analysis.new_reserve0, analysis.new_reserve1)
        } else {
            (sell_pool.reserve0, sell_pool.reserve1)
        };

        let final_amount = calculate_amount_out(
            intermediate,
            sell_reserve1,
            sell_reserve0,
            sell_pool.fee_bps,
        );

        if final_amount <= amount_in {
            return None;
        }

        let gross_profit = final_amount - amount_in;
        let gas_cost = estimate_gas_cost(2, 150_000, gas_price);

        if gross_profit <= gas_cost {
            return None;
        }

        let net_profit = gross_profit - gas_cost;
        if net_profit < min_profit {
            return None;
        }

        let tokens = vec![buy_pool.token0, buy_pool.token1];

        let swap_path = vec![
            SwapStep {
                pool: buy_pool.address,
                dex: buy_pool.dex.clone(),
                token_in: buy_pool.token0,
                token_out: buy_pool.token1,
                amount_in,
                min_amount_out: intermediate * U256::from(99) / U256::from(100),
            },
            SwapStep {
                pool: sell_pool.address,
                dex: sell_pool.dex.clone(),
                token_in: sell_pool.token1,
                token_out: sell_pool.token0,
                amount_in: intermediate,
                min_amount_out: amount_in,
            },
        ];

        let pools = vec![
            PoolInfo {
                address: buy_pool.address,
                dex: buy_pool.dex.clone(),
                token0: buy_pool.token0,
                token1: buy_pool.token1,
                reserve0: buy_reserve0,
                reserve1: buy_reserve1,
                fee_bps: buy_pool.fee_bps,
            },
            PoolInfo {
                address: sell_pool.address,
                dex: sell_pool.dex.clone(),
                token0: sell_pool.token0,
                token1: sell_pool.token1,
                reserve0: sell_reserve0,
                reserve1: sell_reserve1,
                fee_bps: sell_pool.fee_bps,
            },
        ];

        let priority = if net_profit > U256::from(1_000_000_000_000_000_000u128) {
            Priority::Critical
        } else if net_profit > U256::from(100_000_000_000_000_000u128) {
            Priority::High
        } else {
            Priority::Medium
        };

        let mut metadata = HashMap::new();
        metadata.insert("type".to_string(), "liquidity_imbalance".to_string());
        metadata.insert("removal_pct".to_string(), format!("{:.2}", analysis.removal_percentage * 100.0));
        metadata.insert("price_change".to_string(), format!("{:.4}", analysis.price_change));
        metadata.insert("imbalance".to_string(), format!("{:.4}", analysis.imbalance));

        Some(Opportunity {
            id: generate_opportunity_id(OpportunityType::LiquidityImbalance, &tokens),
            opportunity_type: OpportunityType::LiquidityImbalance,
            priority,
            estimated_profit: gross_profit,
            estimated_gas_cost: gas_cost,
            net_profit,
            tokens,
            pools,
            swap_path,
            target_tx: None,
            deadline_block: None,
            detected_at: Utc::now(),
            confidence: 0.75,
            metadata,
        })
    }

    /// Clean up old tracked pools
    pub fn cleanup(&self) {
        let now = Utc::now();
        let cutoff = now - self.new_pool_window;

        // Remove old pool creation records
        self.recent_pools.retain(|_, info| info.created_at > cutoff);

        // Remove stale suspicious pool records (keep for 24 hours)
        let suspicious_cutoff = now - Duration::hours(24);
        self.suspicious_pools
            .retain(|_, flags| flags.last_activity > suspicious_cutoff);
    }
}

impl Default for LiquidityEventDetector {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Detector for LiquidityEventDetector {
    fn name(&self) -> &str {
        &self.name
    }

    fn is_enabled(&self, config: &DetectorConfig) -> bool {
        config.enable_liquidity_events
    }

    async fn detect(
        &self,
        event: &MonitorEvent,
        ctx: &DetectorContext,
    ) -> Result<Vec<Opportunity>> {
        let mut opportunities = Vec::new();
        let gas_price = ctx.get_gas_price().await;
        let min_profit = ctx.config.min_profit_wei;

        match event {
            MonitorEvent::PoolCreated {
                pool,
                token0,
                token1,
                fee,
                ..
            } => {
                tracing::info!(
                    "New pool detected: {} for {:?}/{:?} (fee: {})",
                    pool,
                    token0,
                    token1,
                    fee
                );

                // Record the new pool
                self.recent_pools.insert(
                    *pool,
                    PoolCreationInfo {
                        address: *pool,
                        token0: *token0,
                        token1: *token1,
                        created_at: Utc::now(),
                        initial_reserves: None,
                    },
                );

                // Look for arbitrage opportunities
                if let Some(analysis) = self.analyze_new_pool(*pool, *token0, *token1, ctx) {
                    // Check for honeypot
                    let honeypot_check = self.check_honeypot(pool, ctx);

                    if honeypot_check.risk_score < 50 {
                        if let Some(opp) = self.build_new_pool_opportunity(&analysis, gas_price, min_profit) {
                            tracing::info!(
                                "New pool arbitrage opportunity: spread {:.2}%, profit {} wei",
                                analysis.price_spread * 100.0,
                                opp.net_profit
                            );
                            opportunities.push(opp);
                        }
                    } else {
                        tracing::warn!(
                            "Pool {} flagged as potential honeypot (risk score: {})",
                            pool,
                            honeypot_check.risk_score
                        );
                    }
                }
            }

            MonitorEvent::LiquidityAdded {
                pool,
                amount0,
                amount1,
                ..
            } => {
                // Update initial reserves if this is a recently created pool
                if let Some(mut info) = self.recent_pools.get_mut(pool) {
                    if info.initial_reserves.is_none() {
                        info.initial_reserves = Some((*amount0, *amount1));
                    }
                }

                // After liquidity is added to a new pool, check for arb
                if self.is_recently_created(pool) {
                    if let Some(registered) = ctx.pool_registry.get(pool) {
                        if let Some(analysis) = self.analyze_new_pool(
                            *pool,
                            registered.token0,
                            registered.token1,
                            ctx,
                        ) {
                            let honeypot_check = self.check_honeypot(pool, ctx);

                            if honeypot_check.risk_score < 50 {
                                if let Some(opp) = self.build_new_pool_opportunity(
                                    &analysis,
                                    gas_price,
                                    min_profit,
                                ) {
                                    opportunities.push(opp);
                                }
                            }
                        }
                    }
                }
            }

            MonitorEvent::LiquidityRemoved {
                pool,
                amount0,
                amount1,
                ..
            } => {
                // Analyze for imbalance opportunity
                if let Some(analysis) = self.analyze_liquidity_removal(pool, *amount0, *amount1, ctx) {
                    tracing::info!(
                        "Large liquidity removal from {}: {:.1}% removed, {:.2}% price change",
                        pool,
                        analysis.removal_percentage * 100.0,
                        analysis.price_change * 100.0
                    );

                    // Check for honeypot
                    let honeypot_check = self.check_honeypot(pool, ctx);

                    if honeypot_check.risk_score < 30 {
                        if let Some(opp) = self.build_removal_opportunity(&analysis, gas_price, min_profit) {
                            opportunities.push(opp);
                        }
                    }
                }
            }

            _ => {}
        }

        // Periodic cleanup - using a simple heuristic based on pool count
        // Clean up when we have many recent pools tracked
        if self.recent_pools.len() > 100 {
            self.cleanup();
        }

        Ok(opportunities)
    }
}

/// Information about a newly created pool
#[derive(Debug, Clone)]
struct PoolCreationInfo {
    #[allow(dead_code)] // Reserved for future use: pool address tracking
    address: Address,
    #[allow(dead_code)] // Reserved for future use: token pair validation
    token0: Address,
    #[allow(dead_code)] // Reserved for future use: token pair validation
    token1: Address,
    created_at: DateTime<Utc>,
    initial_reserves: Option<(U256, U256)>,
}

/// Analysis result for a new pool
struct NewPoolAnalysis {
    new_pool: RegisteredPool,
    existing_pool: RegisteredPool,
    price_spread: f64,
}

/// Analysis result for liquidity removal
struct RemovalAnalysis {
    pool: RegisteredPool,
    removal_percentage: f64,
    price_change: f64,
    imbalance: f64,
    new_reserve0: U256,
    new_reserve1: U256,
    arb_target: Option<RegisteredPool>,
}

/// Flags for suspicious pool activity
#[derive(Debug, Clone, Default)]
struct SuspiciousFlags {
    failed_sells: u32,
    high_tax_detected: bool,
    tax_rate: f64,
    owner_mint_detected: bool,
    last_activity: DateTime<Utc>,
}

/// Types of suspicious activity
pub enum SuspiciousActivity {
    FailedSell,
    HighTax(f64),
    OwnerMint,
}

/// Result of honeypot check
#[derive(Debug, Default)]
struct HoneypotCheck {
    low_liquidity: bool,
    extreme_price: bool,
    failed_sells: bool,
    high_tax: bool,
    risk_score: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_honeypot_check() {
        let detector = LiquidityEventDetector::new();

        // Record some suspicious activity
        let pool = Address::repeat_byte(0x01);
        detector.record_suspicious_activity(pool, SuspiciousActivity::FailedSell);
        detector.record_suspicious_activity(pool, SuspiciousActivity::HighTax(0.5));

        // Check the flags
        let flags = detector.suspicious_pools.get(&pool).unwrap();
        assert_eq!(flags.failed_sells, 1);
        assert!(flags.high_tax_detected);
        assert_eq!(flags.tax_rate, 0.5);
    }

    #[test]
    fn test_recent_pool_tracking() {
        let detector = LiquidityEventDetector::new();

        let pool = Address::repeat_byte(0x01);
        let token0 = Address::repeat_byte(0x02);
        let token1 = Address::repeat_byte(0x03);

        detector.recent_pools.insert(
            pool,
            PoolCreationInfo {
                address: pool,
                token0,
                token1,
                created_at: Utc::now(),
                initial_reserves: None,
            },
        );

        assert!(detector.is_recently_created(&pool));
        assert!(!detector.is_recently_created(&Address::repeat_byte(0xFF)));
    }

    #[test]
    fn test_cleanup() {
        let detector = LiquidityEventDetector::new();

        let pool = Address::repeat_byte(0x01);

        // Add an old pool creation record
        detector.recent_pools.insert(
            pool,
            PoolCreationInfo {
                address: pool,
                token0: Address::ZERO,
                token1: Address::ZERO,
                created_at: Utc::now() - Duration::hours(2), // 2 hours ago
                initial_reserves: None,
            },
        );

        // Cleanup should remove it (window is 1 hour)
        detector.cleanup();

        assert!(!detector.recent_pools.contains_key(&pool));
    }
}
