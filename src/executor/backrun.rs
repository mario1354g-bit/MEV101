//! Backrun execution strategy.
//!
//! This module implements the BackrunExecutor for capturing arbitrage
//! opportunities that arise after large trades cause price dislocations.

use alloy::primitives::{Address, B256, U256};
use alloy::providers::Provider as AlloyProvider;
use async_trait::async_trait;
use std::sync::Arc;
use tracing::{debug, error, info, instrument, warn};

use super::flashbots::FlashbotsBundle;
use super::tx_builder::{SwapParams, TxBuilder};
use super::{
    Config, ExecutionResult, Executor, ExecutorContext, Opportunity, OpportunityType,
    SimulationResult,
};
use crate::error::{ExecutionError, MevError};

/// Result type for backrun operations.
pub type Result<T> = std::result::Result<T, MevError>;

/// Backrun opportunity data extracted from opportunity.
#[derive(Debug, Clone)]
pub struct BackrunData {
    /// Target transaction hash to backrun
    pub target_tx_hash: B256,
    /// Token to trade (buy after target)
    pub token_in: Address,
    /// Token to receive
    pub token_out: Address,
    /// Amount to trade
    pub amount_in: U256,
    /// Expected amount out
    pub expected_amount_out: U256,
    /// Minimum amount out (with slippage)
    pub min_amount_out: U256,
    /// Pool to trade on
    pub pool: Address,
    /// Router address
    pub router: Address,
    /// DEX protocol
    pub protocol: String,
    /// Price impact of target tx in basis points
    pub target_price_impact_bps: u32,
    /// Direction: true if target tx raised price, we sell
    pub sell_after_target: bool,
}

/// Configuration for backrun execution.
#[derive(Debug, Clone)]
pub struct BackrunConfig {
    /// Minimum profit threshold in wei
    pub min_profit_wei: U256,
    /// Maximum slippage tolerance in basis points
    pub slippage_bps: u32,
    /// Minimum price impact of target tx to consider (bps)
    pub min_target_impact_bps: u32,
    /// Maximum blocks to wait for target tx inclusion
    pub max_blocks_wait: u8,
}

impl Default for BackrunConfig {
    fn default() -> Self {
        Self {
            min_profit_wei: U256::from(5_000_000_000_000_000u64), // 0.005 ETH
            slippage_bps: 100,                                     // 1%
            min_target_impact_bps: 50,                             // 0.5% price impact minimum
            max_blocks_wait: 2,
        }
    }
}

/// Backrun executor for price reversion opportunities.
///
/// This executor handles backrun opportunities by:
/// 1. Identifying large trades that cause price impact
/// 2. Building arb tx that captures price reversion after target
/// 3. Creating bundle: [target_tx_hash, backrun_tx]
/// 4. Submitting via Flashbots
pub struct BackrunExecutor {
    /// Executor name
    name: String,
    /// Backrun configuration
    config: BackrunConfig,
}

impl BackrunExecutor {
    /// Create a new backrun executor with default settings.
    pub fn new() -> Self {
        Self {
            name: "backrun".to_string(),
            config: BackrunConfig::default(),
        }
    }

    /// Create a backrun executor with custom configuration.
    pub fn with_config(config: BackrunConfig) -> Self {
        Self {
            name: "backrun".to_string(),
            config,
        }
    }

    /// Returns the name of this executor.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Check if this executor can handle the given opportunity.
    pub fn can_handle(&self, opp: &Opportunity) -> bool {
        matches!(opp.opportunity_type, OpportunityType::Backrun)
    }

    /// Extract backrun data from opportunity.
    fn extract_backrun_data(&self, opp: &Opportunity) -> Result<BackrunData> {
        let target_tx = opp.target_tx.as_ref().ok_or_else(|| {
            MevError::Execution(ExecutionError::SubmissionFailed(
                "No target transaction for backrun".to_string(),
            ))
        })?;

        let metadata = &opp.metadata;

        // Extract amounts
        let amount_in = metadata
            .get("amount_in")
            .and_then(|v| v.as_str())
            .and_then(|s| U256::from_str_radix(s, 10).ok())
            .or_else(|| {
                opp.swap_path
                    .as_ref()
                    .map(|p| p.input_amount)
            })
            .ok_or_else(|| {
                MevError::Execution(ExecutionError::SubmissionFailed(
                    "Missing amount_in".to_string(),
                ))
            })?;

        let expected_amount_out = metadata
            .get("expected_amount_out")
            .and_then(|v| v.as_str())
            .and_then(|s| U256::from_str_radix(s, 10).ok())
            .or_else(|| {
                opp.swap_path
                    .as_ref()
                    .map(|p| p.expected_output)
            })
            .ok_or_else(|| {
                MevError::Execution(ExecutionError::SubmissionFailed(
                    "Missing expected_amount_out".to_string(),
                ))
            })?;

        let target_price_impact_bps = metadata
            .get("target_price_impact_bps")
            .and_then(|v| v.as_u64())
            .map(|v| v as u32)
            .unwrap_or(100); // Default 1%

        let sell_after_target = metadata
            .get("sell_after_target")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let router = metadata
            .get("router")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<Address>().ok())
            .unwrap_or(target_tx.to);

        let protocol = metadata
            .get("protocol")
            .and_then(|v| v.as_str())
            .unwrap_or("uniswap_v2")
            .to_string();

        // Get tokens from opportunity
        let token_in = opp.tokens.first().copied().ok_or_else(|| {
            MevError::Execution(ExecutionError::SubmissionFailed(
                "Missing token_in".to_string(),
            ))
        })?;

        let token_out = opp.tokens.get(1).copied().ok_or_else(|| {
            MevError::Execution(ExecutionError::SubmissionFailed(
                "Missing token_out".to_string(),
            ))
        })?;

        let pool = opp.pools.first().copied().unwrap_or(Address::ZERO);

        // Calculate min output with slippage
        let slippage = U256::from(self.config.slippage_bps);
        let min_amount_out = expected_amount_out * (U256::from(10000) - slippage) / U256::from(10000);

        Ok(BackrunData {
            target_tx_hash: target_tx.hash,
            token_in,
            token_out,
            amount_in,
            expected_amount_out,
            min_amount_out,
            pool,
            router,
            protocol,
            target_price_impact_bps,
            sell_after_target,
        })
    }

    /// Build backrun swap transaction.
    fn build_backrun_swap(&self, data: &BackrunData, recipient: Address) -> SwapParams {
        SwapParams::new(
            data.token_in,
            data.token_out,
            data.amount_in,
            data.min_amount_out,
            recipient,
            data.protocol.clone(),
        )
        .with_pool(data.pool)
    }

    /// Calculate expected profit from backrun.
    fn calculate_profit(&self, data: &BackrunData) -> U256 {
        // For backruns, profit is typically the arbitrage gain from price reversion
        // If we're selling after target raised price: profit = sell_out - buy_in
        // If we're buying after target lowered price: profit = expected_sell - buy_in

        // This is a simplified calculation; actual profit depends on the full arb path
        if data.expected_amount_out > data.amount_in {
            data.expected_amount_out - data.amount_in
        } else {
            U256::ZERO
        }
    }

    /// Validate that target transaction has sufficient price impact.
    fn validate_target_impact(&self, data: &BackrunData) -> Result<()> {
        if data.target_price_impact_bps < self.config.min_target_impact_bps {
            return Err(MevError::Execution(ExecutionError::SubmissionFailed(
                format!(
                    "Target price impact {}bps below minimum {}bps",
                    data.target_price_impact_bps, self.config.min_target_impact_bps
                ),
            )));
        }
        Ok(())
    }

    /// Validate profitability before execution.
    fn validate_profitability(
        &self,
        data: &BackrunData,
        gas_cost: U256,
        config: &Config,
    ) -> Result<()> {
        let gross_profit = self.calculate_profit(data);
        let net_profit = gross_profit.saturating_sub(gas_cost);
        let min_required = config.min_profit_wei.max(self.config.min_profit_wei);

        if net_profit < min_required {
            return Err(MevError::Execution(ExecutionError::NotProfitable));
        }

        Ok(())
    }

    /// Execute the backrun.
    async fn execute_backrun<P>(
        &self,
        opp: &Opportunity,
        ctx: &ExecutorContext<P>,
    ) -> Result<ExecutionResult>
    where
        P: AlloyProvider + Clone + Send + Sync + 'static,
    {
        let start = std::time::Instant::now();

        // Extract backrun data
        let backrun_data = self.extract_backrun_data(opp)?;

        // Validate target has sufficient impact
        self.validate_target_impact(&backrun_data)?;

        info!(
            target_tx = %backrun_data.target_tx_hash,
            amount_in = %backrun_data.amount_in,
            expected_profit = %self.calculate_profit(&backrun_data),
            target_impact_bps = backrun_data.target_price_impact_bps,
            "Executing backrun"
        );

        let tx_builder = TxBuilder::new(
            Arc::clone(&ctx.provider),
            Arc::clone(&ctx.signer),
            ctx.config.chain_id,
        );

        let executor_address = ctx.signer.address();

        // Build backrun transaction
        let backrun_params = self.build_backrun_swap(&backrun_data, executor_address);
        let backrun_tx = tx_builder.build_swap_tx(backrun_params).await?;

        // Estimate gas
        let gas_used = tx_builder.estimate_gas(&backrun_tx).await.unwrap_or(200_000);
        let gas_price = ctx
            .provider
            .get_gas_price()
            .await
            .map_err(|e| MevError::Execution(ExecutionError::SubmissionFailed(e.to_string())))?;

        let gas_cost = U256::from(gas_used) * U256::from(gas_price);

        // Validate profitability
        self.validate_profitability(&backrun_data, gas_cost, &ctx.config)?;

        // Sign transaction
        let signed_backrun = tx_builder.sign_tx(&backrun_tx).await?;

        // Get current block number
        let block_number = ctx
            .provider
            .get_block_number()
            .await
            .map_err(|e| MevError::Execution(ExecutionError::SubmissionFailed(e.to_string())))?;

        // Create bundle with backrun following target tx
        // The target tx is referenced by hash so our backrun executes after it
        let bundle = FlashbotsBundle::with_target_tx_hash(
            vec![signed_backrun],
            backrun_data.target_tx_hash,
            block_number + 1,
        )
        .with_revert_on_fail(ctx.config.revert_on_fail);

        // Simulate bundle
        let sim_result = ctx.flashbots.simulate_bundle(bundle.clone()).await?;

        if !sim_result.success {
            error!(
                error = ?sim_result.error,
                "Backrun bundle simulation failed"
            );
            return Ok(ExecutionResult {
                success: false,
                tx_hash: None,
                bundle_hash: None,
                block_number: None,
                actual_profit: None,
                gas_used: Some(sim_result.total_gas_used),
                gas_price: Some(sim_result.gas_price),
                error: sim_result.error,
                latency_ms: start.elapsed().as_millis() as u64,
            });
        }

        // Final profitability check
        let actual_gas_cost = U256::from(sim_result.total_gas_used) * sim_result.gas_price;
        let expected_profit = self.calculate_profit(&backrun_data);

        if expected_profit <= actual_gas_cost + ctx.config.min_profit_wei {
            warn!(
                expected_profit = %expected_profit,
                gas_cost = %actual_gas_cost,
                "Backrun not profitable after simulation"
            );
            return Ok(ExecutionResult {
                success: false,
                tx_hash: None,
                bundle_hash: None,
                block_number: None,
                actual_profit: None,
                gas_used: Some(sim_result.total_gas_used),
                gas_price: Some(sim_result.gas_price),
                error: Some("Not profitable after gas costs".to_string()),
                latency_ms: start.elapsed().as_millis() as u64,
            });
        }

        // Submit bundle for multiple target blocks
        let mut last_response = None;
        for block_offset in 0..ctx.config.target_blocks {
            let mut target_bundle = bundle.clone();
            target_bundle.block_number = block_number + 1 + block_offset as u64;

            match ctx.flashbots.send_bundle(target_bundle).await {
                Ok(response) => {
                    debug!(
                        bundle_hash = %response.bundle_hash,
                        target_block = block_number + 1 + block_offset as u64,
                        "Backrun bundle submitted"
                    );
                    last_response = Some(response);
                }
                Err(e) => {
                    warn!(error = %e, "Failed to submit bundle for block");
                }
            }
        }

        let response = last_response.ok_or_else(|| {
            MevError::Execution(ExecutionError::BundleFailed(
                "All bundle submissions failed".to_string(),
            ))
        })?;

        let actual_profit = expected_profit.saturating_sub(actual_gas_cost);

        info!(
            bundle_hash = %response.bundle_hash,
            target_tx = %backrun_data.target_tx_hash,
            profit = %actual_profit,
            latency_ms = start.elapsed().as_millis(),
            "Backrun execution complete"
        );

        Ok(ExecutionResult {
            success: true,
            tx_hash: None,
            bundle_hash: Some(response.bundle_hash),
            block_number: None,
            actual_profit: Some(actual_profit),
            gas_used: Some(sim_result.total_gas_used),
            gas_price: Some(sim_result.gas_price),
            error: None,
            latency_ms: start.elapsed().as_millis() as u64,
        })
    }
}

impl Default for BackrunExecutor {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl<P> Executor<P> for BackrunExecutor
where
    P: AlloyProvider + Clone + Send + Sync + 'static,
{
    fn name(&self) -> &str {
        &self.name
    }

    fn can_handle(&self, opp: &Opportunity) -> bool {
        matches!(opp.opportunity_type, OpportunityType::Backrun)
    }

    #[instrument(skip(self, ctx), fields(executor = %self.name, opportunity_id = %opp.id))]
    async fn execute(&self, opp: &Opportunity, ctx: &ExecutorContext<P>) -> Result<ExecutionResult> {
        // Validate we have required data
        if opp.target_tx.is_none() {
            return Err(MevError::Execution(ExecutionError::SubmissionFailed(
                "No target transaction for backrun".to_string(),
            )));
        }

        // Recommend Flashbots for backruns
        if !ctx.config.use_flashbots {
            warn!("Backrun without Flashbots may be frontrun by others");
        }

        self.execute_backrun(opp, ctx).await
    }

    #[instrument(skip(self, ctx), fields(executor = %self.name))]
    async fn simulate(&self, opp: &Opportunity, ctx: &ExecutorContext<P>) -> Result<SimulationResult> {
        let backrun_data = self.extract_backrun_data(opp)?;

        // Validate target impact
        self.validate_target_impact(&backrun_data)?;

        let tx_builder = TxBuilder::new(
            Arc::clone(&ctx.provider),
            Arc::clone(&ctx.signer),
            ctx.config.chain_id,
        );

        let executor_address = ctx.signer.address();

        // Build transaction for simulation
        let backrun_params = self.build_backrun_swap(&backrun_data, executor_address);
        let backrun_tx = tx_builder.build_swap_tx(backrun_params).await?;

        // Sign transaction
        let signed_backrun = tx_builder.sign_tx(&backrun_tx).await?;

        // Get current block number
        let block_number = ctx
            .provider
            .get_block_number()
            .await
            .map_err(|e| MevError::Execution(ExecutionError::SubmissionFailed(e.to_string())))?;

        // Create bundle for simulation
        let bundle = FlashbotsBundle::new(vec![signed_backrun], block_number + 1);

        // Simulate
        let sim_response = ctx.flashbots.simulate_bundle(bundle).await?;

        // Calculate profitability
        let gas_cost = U256::from(sim_response.total_gas_used) * sim_response.gas_price;
        let gross_profit = self.calculate_profit(&backrun_data);
        let net_profit = gross_profit.saturating_sub(gas_cost);
        let is_profitable = net_profit >= ctx.config.min_profit_wei;

        debug!(
            gross_profit = %gross_profit,
            gas_cost = %gas_cost,
            net_profit = %net_profit,
            is_profitable = is_profitable,
            "Backrun simulation complete"
        );

        Ok(SimulationResult {
            success: sim_response.success,
            profit: net_profit,
            gas_used: sim_response.total_gas_used,
            state_changes: Vec::new(),
            logs: Vec::new(),
            error: sim_response.error,
            is_profitable,
        })
    }
}

/// Analyze a transaction to determine if it's a good backrun target.
pub fn analyze_backrun_potential(
    tx_value: U256,
    pool_reserves: (U256, U256),
    is_buy: bool,
) -> BackrunAnalysis {
    let (reserve_in, reserve_out) = if is_buy {
        pool_reserves
    } else {
        (pool_reserves.1, pool_reserves.0)
    };

    // Calculate price impact
    let price_before = reserve_out * U256::from(10000) / reserve_in;
    let new_reserve_in = reserve_in + tx_value;
    let output = tx_value * reserve_out / new_reserve_in;
    let new_reserve_out = reserve_out - output;
    let price_after = new_reserve_out * U256::from(10000) / new_reserve_in;

    let price_impact_bps = if price_before > price_after {
        ((price_before - price_after) * U256::from(10000) / price_before).to::<u32>()
    } else {
        0
    };

    // Calculate optimal backrun amount
    // After the target tx, price will be temporarily skewed
    // We want to trade in the opposite direction to capture reversion
    let optimal_backrun_amount = calculate_optimal_backrun_amount(
        new_reserve_in,
        new_reserve_out,
        reserve_in,
        reserve_out,
    );

    BackrunAnalysis {
        price_impact_bps,
        optimal_amount: optimal_backrun_amount,
        is_worthwhile: price_impact_bps >= 50, // At least 0.5% impact
    }
}

/// Analysis result for backrun potential.
#[derive(Debug, Clone)]
pub struct BackrunAnalysis {
    /// Price impact of target transaction in basis points
    pub price_impact_bps: u32,
    /// Optimal amount to backrun with
    pub optimal_amount: U256,
    /// Whether this is worth executing
    pub is_worthwhile: bool,
}

/// Calculate optimal backrun amount to capture price reversion.
fn calculate_optimal_backrun_amount(
    reserve_in_after: U256,
    reserve_out_after: U256,
    reserve_in_before: U256,
    reserve_out_before: U256,
) -> U256 {
    // The optimal backrun amount is the one that returns the pool
    // to its original price (or close to it)

    // For constant product: k = x * y
    let _k_after = reserve_in_after * reserve_out_after;
    let _k_before = reserve_in_before * reserve_out_before;

    // We want to find amount that brings price back
    // This is a simplified calculation; production would be more precise
    let price_ratio_before = reserve_out_before * U256::from(1_000_000) / reserve_in_before;
    let price_ratio_after = reserve_out_after * U256::from(1_000_000) / reserve_in_after;

    if price_ratio_after >= price_ratio_before {
        // Price went up (less token_out per token_in), we should sell token_out
        // Amount to sell = sqrt(k / new_price) - current_reserve_out
        // Simplified: use fraction of the difference
        (reserve_out_before - reserve_out_after) / U256::from(2)
    } else {
        // Price went down, we should buy token_out
        (reserve_in_after - reserve_in_before) / U256::from(2)
    }
}

/// Estimate profit from backrunning a specific transaction.
pub fn estimate_backrun_profit(
    target_amount: U256,
    backrun_amount: U256,
    reserve_in: U256,
    reserve_out: U256,
    fee_bps: u32,
) -> U256 {
    let fee_factor = U256::from(10000 - fee_bps);
    let fee_denom = U256::from(10000);

    // State after target tx
    let target_out = (target_amount * reserve_out * fee_factor)
        / (reserve_in * fee_denom + target_amount * fee_factor);

    let reserve_in_after = reserve_in + target_amount;
    let reserve_out_after = reserve_out - target_out;

    // Backrun trade (opposite direction - sell token_out to get token_in)
    // Return the output as a proxy for potential profit
    (backrun_amount * reserve_in_after * fee_factor)
        / (reserve_out_after * fee_denom + backrun_amount * fee_factor)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_backrun_executor_creation() {
        let executor = BackrunExecutor::new();
        assert_eq!(executor.name(), "backrun");
        assert_eq!(executor.config.slippage_bps, 100);
    }

    #[test]
    fn test_can_handle() {
        let executor = BackrunExecutor::new();

        let backrun_opp = Opportunity {
            id: "test".to_string(),
            opportunity_type: OpportunityType::Backrun,
            block_number: 0,
            timestamp: 0,
            tokens: vec![],
            pools: vec![],
            estimated_profit_wei: U256::ZERO,
            estimated_gas_wei: U256::ZERO,
            swap_path: None,
            target_tx: None,
            metadata: serde_json::Value::Null,
        };

        let arb_opp = Opportunity {
            opportunity_type: OpportunityType::Arbitrage,
            ..backrun_opp.clone()
        };

        assert!(executor.can_handle(&backrun_opp));
        assert!(!executor.can_handle(&arb_opp));
    }

    #[test]
    fn test_analyze_backrun_potential() {
        let tx_value = U256::from(10_000_000_000_000_000_000u128); // 10 ETH
        let reserves = (
            U256::from(1000_000_000_000_000_000_000u128), // 1000 ETH
            U256::from(2_000_000_000_000u128),            // 2M USDC
        );

        let analysis = analyze_backrun_potential(tx_value, reserves, true);

        // 10 ETH into 1000 ETH pool should cause ~1% price impact
        assert!(analysis.price_impact_bps > 0);
        assert!(analysis.price_impact_bps < 500); // Should be < 5%
    }

    #[test]
    fn test_validate_target_impact() {
        let executor = BackrunExecutor::with_config(BackrunConfig {
            min_target_impact_bps: 50,
            ..Default::default()
        });

        let mut data = BackrunData {
            target_tx_hash: B256::ZERO,
            token_in: Address::ZERO,
            token_out: Address::repeat_byte(1),
            amount_in: U256::ZERO,
            expected_amount_out: U256::ZERO,
            min_amount_out: U256::ZERO,
            pool: Address::ZERO,
            router: Address::ZERO,
            protocol: "uniswap_v2".to_string(),
            target_price_impact_bps: 100, // 1%
            sell_after_target: false,
        };

        // Should pass with 1% impact (> 0.5% minimum)
        assert!(executor.validate_target_impact(&data).is_ok());

        // Should fail with 0.3% impact (< 0.5% minimum)
        data.target_price_impact_bps = 30;
        assert!(executor.validate_target_impact(&data).is_err());
    }

    #[test]
    fn test_calculate_profit() {
        let executor = BackrunExecutor::new();

        let data = BackrunData {
            target_tx_hash: B256::ZERO,
            token_in: Address::ZERO,
            token_out: Address::repeat_byte(1),
            amount_in: U256::from(1_000_000_000_000_000_000u128),    // 1 ETH
            expected_amount_out: U256::from(1_050_000_000_000_000_000u128), // 1.05 ETH worth
            min_amount_out: U256::ZERO,
            pool: Address::ZERO,
            router: Address::ZERO,
            protocol: "uniswap_v2".to_string(),
            target_price_impact_bps: 100,
            sell_after_target: false,
        };

        let profit = executor.calculate_profit(&data);
        assert_eq!(profit, U256::from(50_000_000_000_000_000u128)); // 0.05 ETH
    }

    #[test]
    fn test_estimate_backrun_profit() {
        let target_amount = U256::from(10_000_000_000_000_000_000u128); // 10 ETH
        let backrun_amount = U256::from(5_000_000_000_000_000_000u128);  // 5 ETH worth of token_out
        let reserve_in = U256::from(1000_000_000_000_000_000_000u128);  // 1000 ETH
        let reserve_out = U256::from(2_000_000_000_000u128);            // 2M USDC
        let fee_bps = 30;

        let profit = estimate_backrun_profit(
            target_amount,
            backrun_amount,
            reserve_in,
            reserve_out,
            fee_bps,
        );

        // Should have some output
        assert!(profit > U256::ZERO);
    }
}
