//! Direct arbitrage execution strategy.
//!
//! This module implements the ArbitrageExecutor for executing cross-DEX
//! arbitrage opportunities by buying low on one venue and selling high on another.

use alloy::primitives::{Address, Bytes, B256, U256};
use alloy::providers::Provider as AlloyProvider;
use async_trait::async_trait;
use std::sync::Arc;
use tracing::{debug, error, info, instrument, warn};

use super::flashbots::{FlashbotsBundle, FlashbotsClient};
use super::tx_builder::{SwapParams, TxBuilder};
use super::{
    Config, ExecutionResult, Executor, ExecutorContext, Opportunity, OpportunityType,
    SimulationResult, SwapPath, SwapStep,
};
use crate::error::{ExecutionError, MevError};

/// Result type for arbitrage operations.
pub type Result<T> = std::result::Result<T, MevError>;

/// Arbitrage executor for cross-DEX price discrepancy opportunities.
///
/// This executor handles direct arbitrage by:
/// 1. Building swap transactions (buy low, sell high)
/// 2. Signing transactions
/// 3. Creating Flashbots bundles
/// 4. Submitting to relay
/// 5. Monitoring inclusion
pub struct ArbitrageExecutor {
    /// Executor name
    name: String,
    /// Minimum profit threshold in wei (after gas)
    min_profit_wei: U256,
    /// Maximum slippage tolerance in basis points
    max_slippage_bps: u32,
    /// Whether to use private mempool submission
    use_private_mempool: bool,
}

impl ArbitrageExecutor {
    /// Create a new arbitrage executor with default settings.
    pub fn new() -> Self {
        Self {
            name: "arbitrage".to_string(),
            min_profit_wei: U256::from(10_000_000_000_000_000u64), // 0.01 ETH
            max_slippage_bps: 50,                                   // 0.5%
            use_private_mempool: true,
        }
    }

    /// Create an arbitrage executor with custom settings.
    pub fn with_settings(
        min_profit_wei: U256,
        max_slippage_bps: u32,
        use_private_mempool: bool,
    ) -> Self {
        Self {
            name: "arbitrage".to_string(),
            min_profit_wei,
            max_slippage_bps,
            use_private_mempool,
        }
    }

    /// Calculate minimum output with slippage protection.
    fn calculate_min_output(&self, expected_output: U256) -> U256 {
        let slippage_amount = expected_output * U256::from(self.max_slippage_bps) / U256::from(10000);
        expected_output.saturating_sub(slippage_amount)
    }

    /// Validate that the opportunity meets minimum profitability requirements.
    fn validate_profitability(
        &self,
        estimated_profit: U256,
        gas_cost: U256,
        config: &Config,
    ) -> Result<()> {
        let net_profit = estimated_profit.saturating_sub(gas_cost);
        let min_required = config.min_profit_wei.max(self.min_profit_wei);

        if net_profit < min_required {
            return Err(MevError::Execution(ExecutionError::NotProfitable));
        }

        Ok(())
    }

    /// Build swap transactions for the arbitrage path.
    async fn build_swap_transactions<P>(
        &self,
        swap_path: &SwapPath,
        tx_builder: &TxBuilder<P>,
        recipient: Address,
    ) -> Result<Vec<Bytes>>
    where
        P: AlloyProvider + Clone + Send + Sync + 'static,
    {
        let mut signed_txs = Vec::new();

        for (i, step) in swap_path.steps.iter().enumerate() {
            let amount_in = if i == 0 {
                swap_path.input_amount
            } else {
                // Use expected output from previous step
                swap_path.steps[i - 1].amount_out
            };

            // Calculate minimum output for this step
            let amount_out_min = if i == swap_path.steps.len() - 1 {
                // Final step: use path's min_output
                swap_path.min_output
            } else {
                // Intermediate step: allow small slippage
                self.calculate_min_output(step.amount_out)
            };

            let params = SwapParams::new(
                step.token_in,
                step.token_out,
                amount_in,
                amount_out_min,
                recipient,
                step.protocol.clone(),
            )
            .with_pool(step.pool)
            .with_fee(step.fee.unwrap_or(3000));

            let signed_tx = tx_builder.build_and_sign_swap(params).await?;
            signed_txs.push(signed_tx);
        }

        Ok(signed_txs)
    }

    /// Submit bundle to Flashbots and monitor for inclusion.
    async fn submit_and_monitor<P>(
        &self,
        bundle: FlashbotsBundle,
        flashbots: &FlashbotsClient,
        _provider: &P,
        target_blocks: u8,
    ) -> Result<(B256, Option<u64>)>
    where
        P: AlloyProvider + Clone + Send + Sync + 'static,
    {
        let mut last_bundle_hash = B256::ZERO;

        for block_offset in 0..target_blocks {
            let mut current_bundle = bundle.clone();
            current_bundle.block_number += block_offset as u64;

            info!(
                block_number = current_bundle.block_number,
                "Submitting arbitrage bundle"
            );

            let response = flashbots.send_bundle(current_bundle.clone()).await?;
            last_bundle_hash = response.bundle_hash;

            debug!(
                bundle_hash = %last_bundle_hash,
                "Bundle submitted, checking inclusion"
            );

            // Wait briefly and check bundle stats
            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

            match flashbots.get_bundle_stats(last_bundle_hash).await {
                Ok(stats) => {
                    if stats.is_sent_to_miners {
                        info!(
                            bundle_hash = %last_bundle_hash,
                            "Bundle sent to block builders"
                        );
                    }
                }
                Err(e) => {
                    warn!(error = %e, "Failed to get bundle stats");
                }
            }
        }

        Ok((last_bundle_hash, None))
    }

    /// Execute a simple two-hop arbitrage (buy low, sell high).
    async fn execute_simple_arb<P>(
        &self,
        opp: &Opportunity,
        ctx: &ExecutorContext<P>,
    ) -> Result<ExecutionResult>
    where
        P: AlloyProvider + Clone + Send + Sync + 'static,
    {
        let start = std::time::Instant::now();

        let swap_path = opp.swap_path.as_ref().ok_or_else(|| {
            MevError::Execution(ExecutionError::SubmissionFailed(
                "No swap path provided for arbitrage".to_string(),
            ))
        })?;

        // Validate profitability before execution
        self.validate_profitability(
            opp.estimated_profit_wei,
            opp.estimated_gas_wei,
            &ctx.config,
        )?;

        let tx_builder = TxBuilder::new(
            Arc::clone(&ctx.provider),
            Arc::clone(&ctx.signer),
            ctx.config.chain_id,
        );

        // Build and sign swap transactions
        let signed_txs = self
            .build_swap_transactions(swap_path, &tx_builder, ctx.signer.address())
            .await?;

        info!(
            tx_count = signed_txs.len(),
            estimated_profit = %opp.estimated_profit_wei,
            "Built arbitrage transactions"
        );

        // Get current block number
        let block_number = ctx
            .provider
            .get_block_number()
            .await
            .map_err(|e| MevError::Execution(ExecutionError::SubmissionFailed(e.to_string())))?;

        // Create Flashbots bundle
        let bundle = FlashbotsBundle::new(signed_txs, block_number + 1)
            .with_revert_on_fail(ctx.config.revert_on_fail);

        // Simulate bundle first
        let sim_result = ctx.flashbots.simulate_bundle(bundle.clone()).await?;

        if !sim_result.success {
            error!(
                error = ?sim_result.error,
                "Bundle simulation failed"
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

        // Verify profitability from simulation
        let gas_cost = U256::from(sim_result.total_gas_used) * sim_result.gas_price;
        if opp.estimated_profit_wei <= gas_cost + ctx.config.min_profit_wei {
            warn!(
                estimated_profit = %opp.estimated_profit_wei,
                gas_cost = %gas_cost,
                "Opportunity no longer profitable after simulation"
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

        // Submit bundle
        let (bundle_hash, included_block) = self
            .submit_and_monitor(
                bundle,
                &ctx.flashbots,
                &*ctx.provider,
                ctx.config.target_blocks,
            )
            .await?;

        let actual_profit = opp.estimated_profit_wei.saturating_sub(gas_cost);

        info!(
            bundle_hash = %bundle_hash,
            profit = %actual_profit,
            latency_ms = start.elapsed().as_millis(),
            "Arbitrage execution complete"
        );

        Ok(ExecutionResult {
            success: true,
            tx_hash: None,
            bundle_hash: Some(bundle_hash),
            block_number: included_block,
            actual_profit: Some(actual_profit),
            gas_used: Some(sim_result.total_gas_used),
            gas_price: Some(sim_result.gas_price),
            error: None,
            latency_ms: start.elapsed().as_millis() as u64,
        })
    }
}

impl Default for ArbitrageExecutor {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl<P> Executor<P> for ArbitrageExecutor
where
    P: AlloyProvider + Clone + Send + Sync + 'static,
{
    fn name(&self) -> &str {
        &self.name
    }

    fn can_handle(&self, opp: &Opportunity) -> bool {
        matches!(opp.opportunity_type, OpportunityType::Arbitrage)
    }

    #[instrument(skip(self, ctx), fields(executor = %self.name, opportunity_id = %opp.id))]
    async fn execute(&self, opp: &Opportunity, ctx: &ExecutorContext<P>) -> Result<ExecutionResult> {
        info!(
            opportunity_type = ?opp.opportunity_type,
            estimated_profit = %opp.estimated_profit_wei,
            "Executing arbitrage opportunity"
        );

        // Validate we have a swap path
        if opp.swap_path.is_none() {
            return Err(MevError::Execution(ExecutionError::SubmissionFailed(
                "No swap path provided".to_string(),
            )));
        }

        // Check Flashbots requirement
        if !ctx.config.use_flashbots && self.use_private_mempool {
            return Err(MevError::Execution(ExecutionError::SubmissionFailed(
                "Arbitrage requires Flashbots for MEV protection".to_string(),
            )));
        }

        self.execute_simple_arb(opp, ctx).await
    }

    #[instrument(skip(self, ctx), fields(executor = %self.name))]
    async fn simulate(&self, opp: &Opportunity, ctx: &ExecutorContext<P>) -> Result<SimulationResult> {
        let swap_path = opp.swap_path.as_ref().ok_or_else(|| {
            MevError::Execution(ExecutionError::SubmissionFailed(
                "No swap path provided for arbitrage".to_string(),
            ))
        })?;

        let tx_builder = TxBuilder::new(
            Arc::clone(&ctx.provider),
            Arc::clone(&ctx.signer),
            ctx.config.chain_id,
        );

        // Build transactions for simulation
        let signed_txs = self
            .build_swap_transactions(swap_path, &tx_builder, ctx.signer.address())
            .await?;

        // Get current block number
        let block_number = ctx
            .provider
            .get_block_number()
            .await
            .map_err(|e| MevError::Execution(ExecutionError::SubmissionFailed(e.to_string())))?;

        // Create bundle for simulation
        let bundle = FlashbotsBundle::new(signed_txs, block_number + 1);

        // Simulate via Flashbots
        let sim_response = ctx.flashbots.simulate_bundle(bundle).await?;

        // Calculate profitability
        let gas_cost = U256::from(sim_response.total_gas_used) * sim_response.gas_price;
        let profit = if opp.estimated_profit_wei > gas_cost {
            opp.estimated_profit_wei - gas_cost
        } else {
            U256::ZERO
        };
        let is_profitable = profit >= ctx.config.min_profit_wei;

        debug!(
            simulated_gas = sim_response.total_gas_used,
            gas_cost = %gas_cost,
            profit = %profit,
            is_profitable = is_profitable,
            "Arbitrage simulation complete"
        );

        Ok(SimulationResult {
            success: sim_response.success,
            profit,
            gas_used: sim_response.total_gas_used,
            state_changes: Vec::new(),
            logs: Vec::new(),
            error: sim_response.error,
            is_profitable,
        })
    }
}

/// Builder for creating complex arbitrage paths.
pub struct ArbitragePathBuilder {
    steps: Vec<SwapStep>,
    input_amount: U256,
}

impl ArbitragePathBuilder {
    /// Create a new path builder with the input amount.
    pub fn new(input_amount: U256) -> Self {
        Self {
            steps: Vec::new(),
            input_amount,
        }
    }

    /// Add a swap step to the path.
    pub fn add_step(
        mut self,
        pool: Address,
        token_in: Address,
        token_out: Address,
        protocol: &str,
        expected_out: U256,
        fee: Option<u32>,
    ) -> Self {
        self.steps.push(SwapStep {
            pool,
            token_in,
            token_out,
            protocol: protocol.to_string(),
            fee,
            amount_out: expected_out,
        });
        self
    }

    /// Build the swap path with slippage tolerance.
    pub fn build(self, slippage_bps: u32) -> SwapPath {
        let expected_output = self
            .steps
            .last()
            .map(|s| s.amount_out)
            .unwrap_or(U256::ZERO);

        let slippage_amount = expected_output * U256::from(slippage_bps) / U256::from(10000);
        let min_output = expected_output.saturating_sub(slippage_amount);

        SwapPath {
            steps: self.steps,
            input_amount: self.input_amount,
            expected_output,
            min_output,
        }
    }
}

/// Calculate optimal arbitrage amount for Uniswap V2-style pools.
///
/// Given two pools with reserves, calculates the optimal trade amount
/// that maximizes profit from the price discrepancy.
pub fn calculate_optimal_amount_v2(
    reserve_a_in: U256,
    reserve_a_out: U256,
    reserve_b_in: U256,
    reserve_b_out: U256,
    fee_bps: u32,
) -> U256 {
    // Optimal amount formula for V2-style AMMs with fees
    // This is a simplified version; production would use more precise math

    let fee_factor = U256::from(10000 - fee_bps);
    let numerator = reserve_a_in * reserve_b_out * fee_factor * fee_factor;
    let denominator = reserve_a_out * reserve_b_in * U256::from(10000) * U256::from(10000);

    if numerator <= denominator {
        return U256::ZERO; // No arbitrage opportunity
    }

    // Calculate square root of ratio
    let ratio = numerator / denominator;
    let sqrt_ratio = sqrt_u256(ratio);

    // Optimal input = (sqrt(ratio) - 1) * reserve_a_in / (sqrt(ratio) + fee_factor/10000)
    let one = U256::from(1);
    if sqrt_ratio <= one {
        return U256::ZERO;
    }

    let numerator_final = (sqrt_ratio - one) * reserve_a_in;
    let denominator_final = sqrt_ratio + fee_factor / U256::from(10000);

    numerator_final / denominator_final
}

/// Integer square root for U256.
fn sqrt_u256(n: U256) -> U256 {
    if n.is_zero() {
        return U256::ZERO;
    }

    let mut x = n;
    let mut y = (x + U256::from(1)) / U256::from(2);

    while y < x {
        x = y;
        y = (x + n / x) / U256::from(2);
    }

    x
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_arbitrage_executor_creation() {
        let executor = ArbitrageExecutor::new();
        assert_eq!(executor.name(), "arbitrage");
        assert_eq!(executor.max_slippage_bps, 50);
    }

    #[test]
    fn test_calculate_min_output() {
        let executor = ArbitrageExecutor::with_settings(
            U256::ZERO,
            100, // 1% slippage
            true,
        );

        let expected = U256::from(1000);
        let min_output = executor.calculate_min_output(expected);

        // 1% of 1000 = 10, so min should be 990
        assert_eq!(min_output, U256::from(990));
    }

    #[test]
    fn test_path_builder() {
        let path = ArbitragePathBuilder::new(U256::from(1_000_000_000_000_000_000u128))
            .add_step(
                Address::ZERO,
                Address::ZERO,
                Address::repeat_byte(1),
                "uniswap_v2",
                U256::from(2000_000_000u64),
                None,
            )
            .add_step(
                Address::repeat_byte(2),
                Address::repeat_byte(1),
                Address::ZERO,
                "sushiswap",
                U256::from(1_050_000_000_000_000_000u128),
                None,
            )
            .build(50); // 0.5% slippage

        assert_eq!(path.steps.len(), 2);
        assert_eq!(path.input_amount, U256::from(1_000_000_000_000_000_000u128));
        assert!(path.min_output < path.expected_output);
    }

    #[test]
    fn test_sqrt_u256() {
        assert_eq!(sqrt_u256(U256::ZERO), U256::ZERO);
        assert_eq!(sqrt_u256(U256::from(1)), U256::from(1));
        assert_eq!(sqrt_u256(U256::from(4)), U256::from(2));
        assert_eq!(sqrt_u256(U256::from(100)), U256::from(10));
        assert_eq!(sqrt_u256(U256::from(101)), U256::from(10)); // Floor
    }

    #[test]
    fn test_can_handle() {
        let executor = ArbitrageExecutor::new();

        let arb_opp = Opportunity {
            id: "test".to_string(),
            opportunity_type: OpportunityType::Arbitrage,
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

        let sandwich_opp = Opportunity {
            opportunity_type: OpportunityType::Sandwich,
            ..arb_opp.clone()
        };

        assert!(executor.can_handle(&arb_opp));
        assert!(!executor.can_handle(&sandwich_opp));
    }
}
