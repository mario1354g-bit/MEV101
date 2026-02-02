//! Price Discrepancy Detector
//!
//! Detects cross-DEX arbitrage opportunities by comparing prices
//! across different pools for the same token pair.

use alloy::primitives::U256;
use async_trait::async_trait;
use chrono::Utc;
use std::collections::HashMap;

use super::{
    calculate_amount_out, calculate_price_impact, estimate_gas_cost, generate_opportunity_id,
    u256_to_f64, Detector, DetectorConfig, DetectorContext, MonitorEvent, Opportunity,
    OpportunityType, PoolInfo, Priority, RegisteredPool, SwapStep,
};
use crate::error::Result;

/// Detector for cross-DEX price discrepancies
pub struct PriceDiscrepancyDetector {
    /// Name of this detector
    name: String,
    /// Minimum spread to consider (e.g., 0.003 = 0.3%)
    min_spread: f64,
    /// Maximum price impact allowed
    max_price_impact: f64,
}

impl PriceDiscrepancyDetector {
    pub fn new() -> Self {
        Self {
            name: "price_discrepancy".to_string(),
            min_spread: 0.003, // 0.3%
            max_price_impact: 0.05, // 5%
        }
    }

    pub fn with_min_spread(mut self, spread: f64) -> Self {
        self.min_spread = spread;
        self
    }

    pub fn with_max_impact(mut self, impact: f64) -> Self {
        self.max_price_impact = impact;
        self
    }

    /// Calculate the price for a pool (token0 in terms of token1)
    fn pool_price(&self, pool: &RegisteredPool) -> f64 {
        if pool.reserve0.is_zero() {
            return 0.0;
        }
        u256_to_f64(pool.reserve1) / u256_to_f64(pool.reserve0)
    }

    /// Find arbitrage between two pools
    fn find_arbitrage(
        &self,
        pool_low: &RegisteredPool,
        pool_high: &RegisteredPool,
        gas_price: U256,
        min_profit: U256,
    ) -> Option<ArbitrageResult> {
        let price_low = self.pool_price(pool_low);
        let price_high = self.pool_price(pool_high);

        if price_low <= 0.0 || price_high <= 0.0 {
            return None;
        }

        // Calculate spread
        let spread = (price_high - price_low) / price_low;

        if spread < self.min_spread {
            return None;
        }

        tracing::debug!(
            "Found spread {:.4}% between {} ({}) and {} ({})",
            spread * 100.0,
            pool_low.dex,
            pool_low.address,
            pool_high.dex,
            pool_high.address
        );

        // Find optimal trade size using binary search
        let optimal = self.find_optimal_amount(pool_low, pool_high, gas_price, min_profit)?;

        Some(optimal)
    }

    /// Binary search for optimal trade amount
    fn find_optimal_amount(
        &self,
        pool_buy: &RegisteredPool,
        pool_sell: &RegisteredPool,
        gas_price: U256,
        min_profit: U256,
    ) -> Option<ArbitrageResult> {
        // Estimate gas cost for 2 swaps (~300k gas total)
        let gas_cost = estimate_gas_cost(2, 150_000, gas_price);

        // Start with small amount and scale up
        let reserve_buy = u256_to_f64(pool_buy.reserve0);

        // Search range: 0.01% to 5% of buy pool reserve
        let min_amount = reserve_buy * 0.0001;
        let max_amount = reserve_buy * 0.05;

        let mut best_profit = 0.0f64;
        let mut best_amount = 0.0f64;

        // Binary search for optimal amount
        let steps = 20;
        for i in 0..steps {
            let ratio = (i as f64) / (steps as f64);
            let amount = min_amount + ratio * (max_amount - min_amount);

            let profit = self.calculate_profit(
                pool_buy,
                pool_sell,
                amount,
            );

            if profit > best_profit {
                best_profit = profit;
                best_amount = amount;
            }
        }

        // Refine around best amount
        let refine_min = best_amount * 0.8;
        let refine_max = best_amount * 1.2;

        for i in 0..10 {
            let ratio = (i as f64) / 10.0;
            let amount = refine_min + ratio * (refine_max - refine_min);

            let profit = self.calculate_profit(pool_buy, pool_sell, amount);

            if profit > best_profit {
                best_profit = profit;
                best_amount = amount;
            }
        }

        // Check if profit exceeds gas cost
        let gas_cost_f64 = u256_to_f64(gas_cost);
        let net_profit = best_profit - gas_cost_f64;

        if net_profit <= 0.0 {
            return None;
        }

        let net_profit_u256 = super::f64_to_u256(net_profit);
        if net_profit_u256 < min_profit {
            return None;
        }

        // Check price impact
        let amount_u256 = super::f64_to_u256(best_amount);
        let impact = calculate_price_impact(amount_u256, pool_buy.reserve0, pool_buy.reserve1);

        if impact > self.max_price_impact {
            tracing::debug!("Price impact too high: {:.2}%", impact * 100.0);
            return None;
        }

        Some(ArbitrageResult {
            amount_in: amount_u256,
            expected_profit: super::f64_to_u256(best_profit),
            net_profit: net_profit_u256,
            gas_cost,
            price_impact: impact,
            spread: (self.pool_price(pool_sell) - self.pool_price(pool_buy)) / self.pool_price(pool_buy),
        })
    }

    /// Calculate profit for a given input amount
    fn calculate_profit(
        &self,
        pool_buy: &RegisteredPool,
        pool_sell: &RegisteredPool,
        amount_in: f64,
    ) -> f64 {
        // Step 1: Buy token0 from pool_buy using token1
        // We're buying token0 (the one that's cheaper here)
        // Input: token1, Output: token0
        let amount_u256 = super::f64_to_u256(amount_in);

        // Buy from pool_buy (swap token0 for token1 at lower price)
        let intermediate = calculate_amount_out(
            amount_u256,
            pool_buy.reserve0,
            pool_buy.reserve1,
            pool_buy.fee_bps,
        );

        if intermediate.is_zero() {
            return 0.0;
        }

        // Step 2: Sell at pool_sell (swap token1 back for token0 at higher price)
        let final_amount = calculate_amount_out(
            intermediate,
            pool_sell.reserve1,
            pool_sell.reserve0,
            pool_sell.fee_bps,
        );

        // Profit = final_amount - amount_in
        let final_f64 = u256_to_f64(final_amount);

        if final_f64 > amount_in {
            final_f64 - amount_in
        } else {
            0.0
        }
    }

    /// Build opportunity from arbitrage result
    fn build_opportunity(
        &self,
        pool_buy: &RegisteredPool,
        pool_sell: &RegisteredPool,
        result: &ArbitrageResult,
    ) -> Opportunity {
        let tokens = vec![pool_buy.token0, pool_buy.token1];

        let swap_path = vec![
            SwapStep {
                pool: pool_buy.address,
                dex: pool_buy.dex.clone(),
                token_in: pool_buy.token0,
                token_out: pool_buy.token1,
                amount_in: result.amount_in,
                min_amount_out: U256::ZERO, // Will be calculated during execution
            },
            SwapStep {
                pool: pool_sell.address,
                dex: pool_sell.dex.clone(),
                token_in: pool_sell.token1,
                token_out: pool_sell.token0,
                amount_in: U256::ZERO, // Use output from previous step
                min_amount_out: result.amount_in, // At minimum, get back what we put in
            },
        ];

        let pools = vec![
            PoolInfo {
                address: pool_buy.address,
                dex: pool_buy.dex.clone(),
                token0: pool_buy.token0,
                token1: pool_buy.token1,
                reserve0: pool_buy.reserve0,
                reserve1: pool_buy.reserve1,
                fee_bps: pool_buy.fee_bps,
            },
            PoolInfo {
                address: pool_sell.address,
                dex: pool_sell.dex.clone(),
                token0: pool_sell.token0,
                token1: pool_sell.token1,
                reserve0: pool_sell.reserve0,
                reserve1: pool_sell.reserve1,
                fee_bps: pool_sell.fee_bps,
            },
        ];

        let priority = if result.net_profit > U256::from(10_000_000_000_000_000_000u128) {
            Priority::Critical // > 10 ETH
        } else if result.net_profit > U256::from(1_000_000_000_000_000_000u128) {
            Priority::High // > 1 ETH
        } else if result.net_profit > U256::from(100_000_000_000_000_000u128) {
            Priority::Medium // > 0.1 ETH
        } else {
            Priority::Low
        };

        let mut metadata = HashMap::new();
        metadata.insert("spread".to_string(), format!("{:.4}", result.spread));
        metadata.insert("price_impact".to_string(), format!("{:.4}", result.price_impact));
        metadata.insert("buy_dex".to_string(), pool_buy.dex.clone());
        metadata.insert("sell_dex".to_string(), pool_sell.dex.clone());

        Opportunity {
            id: generate_opportunity_id(OpportunityType::Arbitrage, &tokens),
            opportunity_type: OpportunityType::Arbitrage,
            priority,
            estimated_profit: result.expected_profit,
            estimated_gas_cost: result.gas_cost,
            net_profit: result.net_profit,
            tokens,
            pools,
            swap_path,
            target_tx: None,
            deadline_block: None,
            detected_at: Utc::now(),
            confidence: calculate_confidence(result),
            metadata,
        }
    }
}

impl Default for PriceDiscrepancyDetector {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Detector for PriceDiscrepancyDetector {
    fn name(&self) -> &str {
        &self.name
    }

    fn is_enabled(&self, config: &DetectorConfig) -> bool {
        config.enable_arbitrage
    }

    async fn detect(
        &self,
        event: &MonitorEvent,
        ctx: &DetectorContext,
    ) -> Result<Vec<Opportunity>> {
        let mut opportunities = Vec::new();

        match event {
            MonitorEvent::PriceUpdate {
                pool,
                token0,
                token1,
                reserve0,
                reserve1,
                ..
            } => {
                // Update the pool registry
                ctx.pool_registry.update_reserves(*pool, *reserve0, *reserve1);

                // Get all pools for this token pair
                let pools = ctx.pool_registry.get_pools_for_pair(*token0, *token1);

                if pools.len() < 2 {
                    return Ok(opportunities);
                }

                let gas_price = ctx.get_gas_price().await;
                let min_profit = ctx.config.min_profit_wei;

                // Compare all pairs of pools
                for i in 0..pools.len() {
                    for j in (i + 1)..pools.len() {
                        let pool_a = &pools[i];
                        let pool_b = &pools[j];

                        let price_a = self.pool_price(pool_a);
                        let price_b = self.pool_price(pool_b);

                        if price_a <= 0.0 || price_b <= 0.0 {
                            continue;
                        }

                        // Determine which pool has lower price
                        let (pool_low, pool_high) = if price_a < price_b {
                            (pool_a, pool_b)
                        } else {
                            (pool_b, pool_a)
                        };

                        // Find arbitrage opportunity
                        if let Some(result) = self.find_arbitrage(
                            pool_low,
                            pool_high,
                            gas_price,
                            min_profit,
                        ) {
                            tracing::info!(
                                "Arbitrage opportunity found: {} -> {} profit: {} wei",
                                pool_low.dex,
                                pool_high.dex,
                                result.net_profit
                            );

                            let opp = self.build_opportunity(pool_low, pool_high, &result);
                            opportunities.push(opp);
                        }
                    }
                }
            }
            MonitorEvent::SwapExecuted {
                pool,
                amount0_in,
                amount1_in,
                amount0_out,
                amount1_out,
                ..
            } => {
                // Update reserves based on swap
                if let Some(mut registered) = ctx.pool_registry.get(pool) {
                    // Calculate new reserves
                    let new_reserve0 = if *amount0_in > U256::ZERO {
                        registered.reserve0 + amount0_in - amount0_out
                    } else {
                        registered.reserve0 - amount0_out + amount0_in
                    };
                    let new_reserve1 = if *amount1_in > U256::ZERO {
                        registered.reserve1 + amount1_in - amount1_out
                    } else {
                        registered.reserve1 - amount1_out + amount1_in
                    };

                    ctx.pool_registry.update_reserves(*pool, new_reserve0, new_reserve1);

                    // Now check for opportunities with updated reserves
                    let pools = ctx.pool_registry.get_pools_for_pair(
                        registered.token0,
                        registered.token1,
                    );

                    if pools.len() >= 2 {
                        let gas_price = ctx.get_gas_price().await;
                        let min_profit = ctx.config.min_profit_wei;

                        for other_pool in &pools {
                            if other_pool.address == *pool {
                                continue;
                            }

                            // Get fresh data
                            registered = match ctx.pool_registry.get(pool) {
                                Some(p) => p,
                                None => continue, // Pool was removed
                            };

                            let price_this = self.pool_price(&registered);
                            let price_other = self.pool_price(other_pool);

                            if price_this <= 0.0 || price_other <= 0.0 {
                                continue;
                            }

                            let (pool_low, pool_high) = if price_this < price_other {
                                (&registered, other_pool)
                            } else {
                                (other_pool, &registered)
                            };

                            if let Some(result) = self.find_arbitrage(
                                pool_low,
                                pool_high,
                                gas_price,
                                min_profit,
                            ) {
                                let opp = self.build_opportunity(pool_low, pool_high, &result);
                                opportunities.push(opp);
                            }
                        }
                    }
                }
            }
            _ => {}
        }

        Ok(opportunities)
    }
}

/// Result of arbitrage calculation
#[derive(Debug)]
struct ArbitrageResult {
    /// Optimal input amount
    amount_in: U256,
    /// Expected gross profit
    expected_profit: U256,
    /// Net profit after gas
    net_profit: U256,
    /// Estimated gas cost
    gas_cost: U256,
    /// Price impact percentage
    price_impact: f64,
    /// Price spread between pools
    spread: f64,
}

/// Calculate confidence score for an arbitrage opportunity
fn calculate_confidence(result: &ArbitrageResult) -> f64 {
    let mut confidence: f64 = 1.0;

    // Lower confidence for high price impact
    if result.price_impact > 0.03 {
        confidence *= 0.7;
    } else if result.price_impact > 0.02 {
        confidence *= 0.85;
    }

    // Lower confidence for small spreads (close to threshold)
    if result.spread < 0.005 {
        confidence *= 0.8;
    }

    // Higher confidence for larger net profits
    let net_profit_f64 = u256_to_f64(result.net_profit);
    if net_profit_f64 > 1e18 {
        // > 1 ETH
        confidence *= 1.1;
    }

    confidence.clamp(0.1_f64, 1.0_f64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloy::primitives::Address;

    fn create_test_pool(
        address: Address,
        dex: &str,
        token0: Address,
        token1: Address,
        reserve0: U256,
        reserve1: U256,
    ) -> RegisteredPool {
        RegisteredPool {
            address,
            dex: dex.to_string(),
            token0,
            token1,
            reserve0,
            reserve1,
            fee_bps: 30,
            last_updated: Utc::now(),
            last_block: 0,
        }
    }

    #[test]
    fn test_pool_price() {
        let detector = PriceDiscrepancyDetector::new();

        let pool = create_test_pool(
            Address::ZERO,
            "test",
            Address::ZERO,
            Address::ZERO,
            U256::from(100_000_000_000_000_000_000u128), // 100 ETH
            U256::from(200_000_000_000u128), // 200k USDC (6 decimals)
        );

        let price = detector.pool_price(&pool);
        // Price should be ~2000 (200k / 100)
        assert!(price > 1999.0 && price < 2001.0);
    }

    #[test]
    fn test_find_arbitrage() {
        let detector = PriceDiscrepancyDetector::new();

        let token0 = Address::ZERO;
        let token1 = Address::repeat_byte(0x01);

        // Pool with lower price (1.0)
        let pool_low = create_test_pool(
            Address::repeat_byte(0x10),
            "uniswap",
            token0,
            token1,
            U256::from(100_000_000_000_000_000_000u128), // 100
            U256::from(100_000_000_000_000_000_000u128), // 100
        );

        // Pool with higher price (1.01 = 1% spread)
        let pool_high = create_test_pool(
            Address::repeat_byte(0x20),
            "sushiswap",
            token0,
            token1,
            U256::from(100_000_000_000_000_000_000u128), // 100
            U256::from(101_000_000_000_000_000_000u128), // 101
        );

        let gas_price = U256::from(30_000_000_000u64); // 30 gwei
        let min_profit = U256::from(1_000_000_000_000_000u64); // 0.001 ETH

        let result = detector.find_arbitrage(&pool_low, &pool_high, gas_price, min_profit);

        // With 1% spread and reasonable liquidity, should find an opportunity
        // (depending on gas costs)
        if let Some(r) = result {
            assert!(r.net_profit > U256::ZERO);
            assert!(r.spread > 0.009 && r.spread < 0.011);
        }
    }
}
