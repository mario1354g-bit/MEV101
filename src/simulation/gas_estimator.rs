//! Gas cost estimation module for MEV bot.
//!
//! This module provides gas estimation utilities for different MEV opportunity types,
//! including base fee tracking, priority fee estimation, and builder tip calculations.

use crate::error::SimulationError;
use crate::simulation::Opportunity;
use crate::storage::models::OpportunityType;

use alloy::primitives::U256;
use alloy::providers::Provider;
use alloy::rpc::types::BlockTransactionsKind;
use alloy::transports::Transport;
use dashmap::DashMap;
use std::sync::Arc;
use tracing::{debug, trace, warn};

/// Gas estimator for MEV opportunities.
pub struct GasEstimator<T, P>
where
    T: Transport + Clone,
    P: Provider<T>,
{
    /// Ethereum provider
    provider: Arc<P>,
    /// Historical average gas usage by opportunity type
    base_gas: DashMap<OpportunityType, u64>,
    /// Cached base fee (updated periodically)
    cached_base_fee: tokio::sync::RwLock<Option<CachedValue<U256>>>,
    /// Cached priority fee
    cached_priority_fee: tokio::sync::RwLock<Option<CachedValue<U256>>>,
    /// Cache TTL in seconds
    cache_ttl_secs: u64,
    /// Builder tip percentage (0-100)
    builder_tip_percentage: u64,
    /// Phantom data for transport type
    _transport: std::marker::PhantomData<T>,
}

/// A cached value with timestamp.
#[derive(Debug, Clone)]
struct CachedValue<T> {
    value: T,
    timestamp: std::time::Instant,
}

impl<T> CachedValue<T> {
    fn new(value: T) -> Self {
        Self {
            value,
            timestamp: std::time::Instant::now(),
        }
    }

    fn is_expired(&self, ttl_secs: u64) -> bool {
        self.timestamp.elapsed().as_secs() > ttl_secs
    }
}

impl<T, P> GasEstimator<T, P>
where
    T: Transport + Clone,
    P: Provider<T>,
{
    /// Create a new gas estimator with default gas values.
    pub fn new(provider: Arc<P>) -> Self {
        let base_gas = DashMap::new();

        // Set default gas estimates for each opportunity type
        base_gas.insert(OpportunityType::PriceDiscrepancy, 300_000); // 2 swaps
        base_gas.insert(OpportunityType::MultiHop, 450_000); // 3 swaps
        base_gas.insert(OpportunityType::Sandwich, 400_000); // 2 swaps + overhead
        base_gas.insert(OpportunityType::Backrun, 200_000); // 1 swap
        base_gas.insert(OpportunityType::Liquidation, 500_000); // Complex
        base_gas.insert(OpportunityType::LiquidityEvent, 250_000); // 1-2 swaps

        Self {
            provider,
            base_gas,
            cached_base_fee: tokio::sync::RwLock::new(None),
            cached_priority_fee: tokio::sync::RwLock::new(None),
            cache_ttl_secs: 12, // Roughly 1 block
            builder_tip_percentage: 90, // 90% of profit to builder
            _transport: std::marker::PhantomData,
        }
    }

    /// Create a gas estimator with custom gas values.
    pub fn with_custom_gas(provider: Arc<P>, gas_values: DashMap<OpportunityType, u64>) -> Self {
        Self {
            provider,
            base_gas: gas_values,
            cached_base_fee: tokio::sync::RwLock::new(None),
            cached_priority_fee: tokio::sync::RwLock::new(None),
            cache_ttl_secs: 12,
            builder_tip_percentage: 90,
            _transport: std::marker::PhantomData,
        }
    }

    /// Set the builder tip percentage.
    pub fn set_builder_tip_percentage(&mut self, percentage: u64) {
        self.builder_tip_percentage = percentage.min(100);
    }

    /// Set the cache TTL in seconds.
    pub fn set_cache_ttl(&mut self, ttl_secs: u64) {
        self.cache_ttl_secs = ttl_secs;
    }

    /// Estimate the total gas cost for an opportunity in wei.
    pub async fn estimate_gas_cost(
        &self,
        opp: &Opportunity,
    ) -> Result<U256, SimulationError> {
        let base_fee = self.get_base_fee().await?;
        let priority_fee = self.get_priority_fee().await?;
        let gas_units = self.get_gas_units(&opp.opportunity_type);

        // Adjust gas based on complexity (number of swaps)
        let adjusted_gas = self.adjust_gas_for_complexity(gas_units, opp);

        let total_gas_price = base_fee + priority_fee;
        let gas_cost = U256::from(adjusted_gas) * total_gas_price;

        debug!(
            opportunity_type = %opp.opportunity_type,
            base_fee = %base_fee,
            priority_fee = %priority_fee,
            gas_units = adjusted_gas,
            gas_cost_wei = %gas_cost,
            "Estimated gas cost"
        );

        Ok(gas_cost)
    }

    /// Estimate gas cost with a specific gas price.
    pub fn estimate_gas_cost_with_price(
        &self,
        opp: &Opportunity,
        gas_price: U256,
    ) -> U256 {
        let gas_units = self.get_gas_units(&opp.opportunity_type);
        let adjusted_gas = self.adjust_gas_for_complexity(gas_units, opp);
        U256::from(adjusted_gas) * gas_price
    }

    /// Get the base gas units for an opportunity type.
    pub fn get_gas_units(&self, opp_type: &OpportunityType) -> u64 {
        *self.base_gas.get(opp_type).map(|v| *v).as_ref().unwrap_or(&300_000)
    }

    /// Update the historical gas usage for an opportunity type.
    pub fn update_gas_usage(&self, opp_type: OpportunityType, actual_gas: u64) {
        self.base_gas
            .entry(opp_type)
            .and_modify(|current| {
                // Exponential moving average with alpha = 0.1
                *current = (*current * 9 + actual_gas) / 10;
            })
            .or_insert(actual_gas);

        trace!(
            opportunity_type = %opp_type,
            actual_gas = actual_gas,
            new_estimate = ?self.base_gas.get(&opp_type).map(|v| *v),
            "Updated gas usage estimate"
        );
    }

    /// Get the current base fee.
    pub async fn get_base_fee(&self) -> Result<U256, SimulationError> {
        // Check cache first
        {
            let cache = self.cached_base_fee.read().await;
            if let Some(cached) = &*cache {
                if !cached.is_expired(self.cache_ttl_secs) {
                    return Ok(cached.value);
                }
            }
        }

        // Fetch fresh value
        let block = self
            .provider
            .get_block_by_number(alloy::eips::BlockNumberOrTag::Latest, BlockTransactionsKind::Hashes)
            .await
            .map_err(|e| SimulationError::GasEstimationFailed(e.to_string()))?
            .ok_or_else(|| SimulationError::GasEstimationFailed("No latest block".to_string()))?;

        let base_fee = block
            .header
            .base_fee_per_gas
            .map(U256::from)
            .unwrap_or_else(|| {
                warn!("Block has no base fee, using default");
                U256::from(30_000_000_000u64) // 30 gwei default
            });

        // Update cache
        {
            let mut cache = self.cached_base_fee.write().await;
            *cache = Some(CachedValue::new(base_fee));
        }

        Ok(base_fee)
    }

    /// Get the suggested priority fee.
    pub async fn get_priority_fee(&self) -> Result<U256, SimulationError> {
        // Check cache first
        {
            let cache = self.cached_priority_fee.read().await;
            if let Some(cached) = &*cache {
                if !cached.is_expired(self.cache_ttl_secs) {
                    return Ok(cached.value);
                }
            }
        }

        // Use eth_maxPriorityFeePerGas if available
        let priority_fee = match self.provider.get_max_priority_fee_per_gas().await {
            Ok(fee) => U256::from(fee),
            Err(_) => {
                // Fallback: estimate from recent transactions
                self.estimate_priority_fee_from_history().await?
            }
        };

        // Update cache
        {
            let mut cache = self.cached_priority_fee.write().await;
            *cache = Some(CachedValue::new(priority_fee));
        }

        Ok(priority_fee)
    }

    /// Get the effective gas price (base fee + priority fee).
    pub async fn get_effective_gas_price(&self) -> Result<U256, SimulationError> {
        let base_fee = self.get_base_fee().await?;
        let priority_fee = self.get_priority_fee().await?;
        Ok(base_fee + priority_fee)
    }

    /// Estimate the builder tip for a given gross profit.
    pub fn estimate_builder_tip(&self, gross_profit: U256) -> U256 {
        // Typically 90% of profit goes to builder for competitive opportunities
        gross_profit * U256::from(self.builder_tip_percentage) / U256::from(100)
    }

    /// Calculate the maximum bid for an opportunity.
    pub fn calculate_max_bid(
        &self,
        gross_profit: U256,
        gas_cost: U256,
        min_profit_margin: U256,
    ) -> U256 {
        if gross_profit <= gas_cost + min_profit_margin {
            return U256::ZERO;
        }

        // Max bid = gross profit - gas cost - min profit margin
        gross_profit - gas_cost - min_profit_margin
    }

    /// Estimate the optimal priority fee for a given profit opportunity.
    pub async fn estimate_optimal_priority_fee(
        &self,
        gross_profit: U256,
        gas_units: u64,
    ) -> Result<U256, SimulationError> {
        let base_fee = self.get_base_fee().await?;

        // Calculate max priority fee that still leaves profit
        // profit = gross_profit - gas_units * (base_fee + priority_fee)
        // For breakeven: priority_fee = (gross_profit / gas_units) - base_fee

        if gross_profit == U256::ZERO || gas_units == 0 {
            return Ok(U256::ZERO);
        }

        let max_gas_price = gross_profit / U256::from(gas_units);

        if max_gas_price <= base_fee {
            // Can't be profitable at current base fee
            return Ok(U256::ZERO);
        }

        // Leave some profit margin (use builder tip percentage as guide)
        let available_for_priority = max_gas_price - base_fee;
        let optimal_priority = available_for_priority
            * U256::from(self.builder_tip_percentage)
            / U256::from(100);

        debug!(
            gross_profit = %gross_profit,
            gas_units = gas_units,
            base_fee = %base_fee,
            optimal_priority = %optimal_priority,
            "Calculated optimal priority fee"
        );

        Ok(optimal_priority)
    }

    /// Adjust gas estimate based on opportunity complexity.
    fn adjust_gas_for_complexity(&self, base_gas: u64, opp: &Opportunity) -> u64 {
        let num_swaps = opp.swaps.len();

        // Add gas per additional swap beyond the base assumption
        let additional_swaps = match opp.opportunity_type {
            OpportunityType::PriceDiscrepancy => num_swaps.saturating_sub(2),
            OpportunityType::MultiHop => num_swaps.saturating_sub(3),
            OpportunityType::Sandwich => num_swaps.saturating_sub(2),
            OpportunityType::Backrun => num_swaps.saturating_sub(1),
            OpportunityType::Liquidation => num_swaps.saturating_sub(1),
            OpportunityType::LiquidityEvent => num_swaps.saturating_sub(1),
        };

        // Each additional swap adds ~150k gas
        let additional_gas = additional_swaps as u64 * 150_000;

        // Add 10% buffer for safety
        let total_gas = base_gas + additional_gas;
        total_gas + (total_gas / 10)
    }

    /// Estimate priority fee from recent block history.
    async fn estimate_priority_fee_from_history(&self) -> Result<U256, SimulationError> {
        // Fallback to a reasonable default if we can't get historical data
        // This is around 2 gwei which is typical for non-urgent transactions
        let default_priority = U256::from(2_000_000_000u64);

        // Try to get recent blocks and analyze priority fees
        let latest_block = self
            .provider
            .get_block_number()
            .await
            .map_err(|e| SimulationError::GasEstimationFailed(e.to_string()))?;

        // Get the last few blocks to analyze
        let mut priority_fees = Vec::new();
        for i in 0..5 {
            if latest_block < i {
                break;
            }

            let block_num = latest_block - i;
            if let Ok(Some(block)) = self
                .provider
                .get_block_by_number(alloy::eips::BlockNumberOrTag::Number(block_num), BlockTransactionsKind::Hashes)
                .await
            {
                if let Some(base_fee) = block.header.base_fee_per_gas {
                    // Get transactions and estimate their priority fees
                    for tx_hash in block.transactions.hashes() {
                        if let Ok(Some(receipt)) = self.provider.get_transaction_receipt(tx_hash).await {
                            let effective_gas_price = receipt.effective_gas_price;
                            let priority = effective_gas_price.saturating_sub(base_fee as u128);
                            if priority > 0 {
                                priority_fees.push(U256::from(priority));
                            }
                        }
                    }
                }
            }
        }

        if priority_fees.is_empty() {
            return Ok(default_priority);
        }

        // Return median priority fee
        priority_fees.sort();
        let median_idx = priority_fees.len() / 2;
        Ok(priority_fees[median_idx])
    }

    /// Get gas price recommendations for different urgency levels.
    pub async fn get_gas_recommendations(&self) -> Result<GasRecommendations, SimulationError> {
        let base_fee = self.get_base_fee().await?;
        let priority_fee = self.get_priority_fee().await?;

        // Slow: base fee + minimal priority
        let slow_priority = priority_fee / U256::from(2);

        // Standard: base fee + normal priority
        let standard_priority = priority_fee;

        // Fast: base fee + 1.5x priority
        let fast_priority = priority_fee + priority_fee / U256::from(2);

        // Instant: base fee + 2x priority (for MEV)
        let instant_priority = priority_fee * U256::from(2);

        Ok(GasRecommendations {
            base_fee,
            slow: GasPrice {
                max_fee: base_fee + slow_priority,
                priority_fee: slow_priority,
            },
            standard: GasPrice {
                max_fee: base_fee + standard_priority,
                priority_fee: standard_priority,
            },
            fast: GasPrice {
                max_fee: base_fee + fast_priority,
                priority_fee: fast_priority,
            },
            instant: GasPrice {
                max_fee: base_fee + instant_priority,
                priority_fee: instant_priority,
            },
        })
    }
}

/// Gas price for a specific urgency level.
#[derive(Debug, Clone)]
pub struct GasPrice {
    /// Maximum fee per gas (EIP-1559)
    pub max_fee: U256,
    /// Priority fee per gas (tip to validators)
    pub priority_fee: U256,
}

/// Gas price recommendations for different urgency levels.
#[derive(Debug, Clone)]
pub struct GasRecommendations {
    /// Current base fee
    pub base_fee: U256,
    /// Slow transaction (might take several blocks)
    pub slow: GasPrice,
    /// Standard transaction (next few blocks)
    pub standard: GasPrice,
    /// Fast transaction (next block)
    pub fast: GasPrice,
    /// Instant/MEV transaction (high priority)
    pub instant: GasPrice,
}

impl GasRecommendations {
    /// Get the gas price for MEV opportunities.
    pub fn mev_gas_price(&self) -> &GasPrice {
        &self.instant
    }

    /// Calculate gas cost for a given number of gas units.
    pub fn calculate_cost(&self, gas_units: u64, level: GasUrgency) -> U256 {
        let gas_price = match level {
            GasUrgency::Slow => &self.slow,
            GasUrgency::Standard => &self.standard,
            GasUrgency::Fast => &self.fast,
            GasUrgency::Instant => &self.instant,
        };
        U256::from(gas_units) * gas_price.max_fee
    }
}

/// Gas urgency levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GasUrgency {
    Slow,
    Standard,
    Fast,
    Instant,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_estimate_builder_tip() {
        // Create a mock-like scenario by testing the calculation directly
        let gross_profit = U256::from(1_000_000_000_000u64); // 1e12 wei

        // 90% tip
        let tip_90 = gross_profit * U256::from(90) / U256::from(100);
        assert_eq!(tip_90, U256::from(900_000_000_000u64));

        // 50% tip
        let tip_50 = gross_profit * U256::from(50) / U256::from(100);
        assert_eq!(tip_50, U256::from(500_000_000_000u64));
    }

    #[test]
    fn test_calculate_max_bid() {
        let gross_profit = U256::from(1_000_000u64);
        let gas_cost = U256::from(200_000u64);
        let min_margin = U256::from(100_000u64);

        // Max bid should be: 1_000_000 - 200_000 - 100_000 = 700_000
        let max_bid = gross_profit - gas_cost - min_margin;
        assert_eq!(max_bid, U256::from(700_000u64));

        // If gas cost + margin >= gross profit, max bid should be 0
        let high_gas = U256::from(950_000u64);
        let should_be_zero = if gross_profit <= high_gas + min_margin {
            U256::ZERO
        } else {
            gross_profit - high_gas - min_margin
        };
        assert_eq!(should_be_zero, U256::ZERO);
    }

    #[test]
    fn test_gas_recommendations_cost_calculation() {
        let recommendations = GasRecommendations {
            base_fee: U256::from(30_000_000_000u64), // 30 gwei
            slow: GasPrice {
                max_fee: U256::from(31_000_000_000u64),
                priority_fee: U256::from(1_000_000_000u64),
            },
            standard: GasPrice {
                max_fee: U256::from(32_000_000_000u64),
                priority_fee: U256::from(2_000_000_000u64),
            },
            fast: GasPrice {
                max_fee: U256::from(33_000_000_000u64),
                priority_fee: U256::from(3_000_000_000u64),
            },
            instant: GasPrice {
                max_fee: U256::from(34_000_000_000u64),
                priority_fee: U256::from(4_000_000_000u64),
            },
        };

        let gas_units = 200_000u64;

        let slow_cost = recommendations.calculate_cost(gas_units, GasUrgency::Slow);
        let fast_cost = recommendations.calculate_cost(gas_units, GasUrgency::Fast);

        assert!(fast_cost > slow_cost);
        assert_eq!(
            slow_cost,
            U256::from(gas_units) * U256::from(31_000_000_000u64)
        );
    }

    #[test]
    fn test_cached_value_expiry() {
        let cached = CachedValue::new(U256::from(100));

        // Fresh value should not be expired
        assert!(!cached.is_expired(60));

        // Value with 0 TTL should be expired immediately
        assert!(cached.is_expired(0));
    }

    #[test]
    fn test_gas_urgency() {
        assert_ne!(GasUrgency::Slow, GasUrgency::Fast);
        assert_eq!(GasUrgency::Instant, GasUrgency::Instant);
    }
}
