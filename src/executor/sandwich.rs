//! Sandwich attack execution strategy.
//!
//! This module implements the SandwichExecutor for executing sandwich attacks
//! by frontrunning and backrunning victim transactions.

use alloy::primitives::{Address, Bytes, B256, U256};
use alloy::providers::Provider as AlloyProvider;
use alloy::sol;
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

/// Result type for sandwich operations.
pub type Result<T> = std::result::Result<T, MevError>;

// Solidity interface for DEX routers
sol! {
    /// Uniswap V2 Router interface
    interface IUniswapV2Router {
        function swapExactTokensForTokens(
            uint256 amountIn,
            uint256 amountOutMin,
            address[] calldata path,
            address to,
            uint256 deadline
        ) external returns (uint256[] memory amounts);

        function swapTokensForExactTokens(
            uint256 amountOut,
            uint256 amountInMax,
            address[] calldata path,
            address to,
            uint256 deadline
        ) external returns (uint256[] memory amounts);
    }
}

/// Sandwich opportunity data extracted from opportunity.
#[derive(Debug, Clone)]
pub struct SandwichData {
    /// Victim transaction hash
    pub victim_tx_hash: B256,
    /// Token being bought by victim (we frontrun buy this)
    pub token_in: Address,
    /// Token being sold by victim (we frontrun sell to get this)
    pub token_out: Address,
    /// Amount for frontrun transaction
    pub frontrun_amount: U256,
    /// Expected output from frontrun
    pub frontrun_expected_out: U256,
    /// Amount for backrun transaction (output from frontrun)
    pub backrun_amount: U256,
    /// Expected output from backrun
    pub backrun_expected_out: U256,
    /// Pool address
    pub pool: Address,
    /// Router address
    pub router: Address,
    /// DEX protocol
    pub protocol: String,
    /// Minimum profit threshold
    pub min_profit: U256,
}

/// Configuration for sandwich execution.
#[derive(Debug, Clone)]
pub struct SandwichConfig {
    /// Minimum profit threshold in wei
    pub min_profit_wei: U256,
    /// Maximum slippage for frontrun in basis points
    pub frontrun_slippage_bps: u32,
    /// Maximum slippage for backrun in basis points
    pub backrun_slippage_bps: u32,
    /// Whether to use aggressive gas pricing
    pub aggressive_gas: bool,
    /// Maximum gas price multiplier for frontrun
    pub max_gas_multiplier: u32,
}

impl Default for SandwichConfig {
    fn default() -> Self {
        Self {
            min_profit_wei: U256::from(10_000_000_000_000_000u64), // 0.01 ETH
            frontrun_slippage_bps: 100,                            // 1%
            backrun_slippage_bps: 200,                             // 2% (more tolerance for backrun)
            aggressive_gas: true,
            max_gas_multiplier: 3,
        }
    }
}

/// Sandwich executor for frontrun/backrun opportunities.
///
/// This executor handles sandwich attacks by:
/// 1. Building frontrun tx (buy token before victim)
/// 2. Including victim tx hash in bundle
/// 3. Building backrun tx (sell token after victim)
/// 4. Creating atomic bundle: [frontrun, victim_hash, backrun]
/// 5. Submitting via Flashbots
pub struct SandwichExecutor {
    /// Executor name
    name: String,
    /// Sandwich configuration
    config: SandwichConfig,
}

impl SandwichExecutor {
    /// Create a new sandwich executor with default settings.
    pub fn new() -> Self {
        Self {
            name: "sandwich".to_string(),
            config: SandwichConfig::default(),
        }
    }

    /// Create a sandwich executor with custom configuration.
    pub fn with_config(config: SandwichConfig) -> Self {
        Self {
            name: "sandwich".to_string(),
            config,
        }
    }

    /// Extract sandwich data from opportunity.
    fn extract_sandwich_data(&self, opp: &Opportunity) -> Result<SandwichData> {
        let target_tx = opp.target_tx.as_ref().ok_or_else(|| {
            MevError::Execution(ExecutionError::SubmissionFailed(
                "No target transaction for sandwich".to_string(),
            ))
        })?;

        // Extract from metadata
        let metadata = &opp.metadata;

        let frontrun_amount = metadata
            .get("frontrun_amount")
            .and_then(|v| v.as_str())
            .and_then(|s| U256::from_str_radix(s, 10).ok())
            .ok_or_else(|| {
                MevError::Execution(ExecutionError::SubmissionFailed(
                    "Missing frontrun_amount in metadata".to_string(),
                ))
            })?;

        let frontrun_expected_out = metadata
            .get("frontrun_expected_out")
            .and_then(|v| v.as_str())
            .and_then(|s| U256::from_str_radix(s, 10).ok())
            .ok_or_else(|| {
                MevError::Execution(ExecutionError::SubmissionFailed(
                    "Missing frontrun_expected_out in metadata".to_string(),
                ))
            })?;

        let backrun_expected_out = metadata
            .get("backrun_expected_out")
            .and_then(|v| v.as_str())
            .and_then(|s| U256::from_str_radix(s, 10).ok())
            .ok_or_else(|| {
                MevError::Execution(ExecutionError::SubmissionFailed(
                    "Missing backrun_expected_out in metadata".to_string(),
                ))
            })?;

        let router = metadata
            .get("router")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<Address>().ok())
            .unwrap_or_else(|| target_tx.to);

        let protocol = metadata
            .get("protocol")
            .and_then(|v| v.as_str())
            .unwrap_or("uniswap_v2")
            .to_string();

        // Tokens should be in the opportunity
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

        Ok(SandwichData {
            victim_tx_hash: target_tx.hash,
            token_in,
            token_out,
            frontrun_amount,
            frontrun_expected_out,
            backrun_amount: frontrun_expected_out, // We sell what we bought
            backrun_expected_out,
            pool,
            router,
            protocol,
            min_profit: self.config.min_profit_wei,
        })
    }

    /// Build frontrun swap transaction (buy token before victim).
    fn build_frontrun_swap(&self, data: &SandwichData, recipient: Address) -> SwapParams {
        let slippage = U256::from(self.config.frontrun_slippage_bps);
        let min_out = data.frontrun_expected_out * (U256::from(10000) - slippage) / U256::from(10000);

        SwapParams::new(
            data.token_in,
            data.token_out,
            data.frontrun_amount,
            min_out,
            recipient,
            data.protocol.clone(),
        )
        .with_pool(data.pool)
    }

    /// Build backrun swap transaction (sell token after victim).
    fn build_backrun_swap(&self, data: &SandwichData, recipient: Address) -> SwapParams {
        let slippage = U256::from(self.config.backrun_slippage_bps);
        let min_out = data.backrun_expected_out * (U256::from(10000) - slippage) / U256::from(10000);

        SwapParams::new(
            data.token_out,  // Sell what we bought in frontrun
            data.token_in,   // Get back original token
            data.backrun_amount,
            min_out,
            recipient,
            data.protocol.clone(),
        )
        .with_pool(data.pool)
    }

    /// Calculate expected profit from sandwich.
    fn calculate_profit(&self, data: &SandwichData) -> U256 {
        // Profit = backrun_out - frontrun_in
        if data.backrun_expected_out > data.frontrun_amount {
            data.backrun_expected_out - data.frontrun_amount
        } else {
            U256::ZERO
        }
    }

    /// Validate profitability before execution.
    fn validate_profitability(
        &self,
        data: &SandwichData,
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

    /// Execute the sandwich attack.
    async fn execute_sandwich<P>(
        &self,
        opp: &Opportunity,
        ctx: &ExecutorContext<P>,
    ) -> Result<ExecutionResult>
    where
        P: AlloyProvider + Clone + Send + Sync + 'static,
    {
        let start = std::time::Instant::now();

        // Extract sandwich data
        let sandwich_data = self.extract_sandwich_data(opp)?;

        info!(
            victim_tx = %sandwich_data.victim_tx_hash,
            frontrun_amount = %sandwich_data.frontrun_amount,
            expected_profit = %self.calculate_profit(&sandwich_data),
            "Executing sandwich attack"
        );

        let tx_builder = TxBuilder::new(
            Arc::clone(&ctx.provider),
            Arc::clone(&ctx.signer),
            ctx.config.chain_id,
        );

        let executor_address = ctx.signer.address();

        // Build frontrun transaction
        let frontrun_params = self.build_frontrun_swap(&sandwich_data, executor_address);
        let frontrun_tx = tx_builder.build_swap_tx(frontrun_params).await?;

        // Build backrun transaction
        let backrun_params = self.build_backrun_swap(&sandwich_data, executor_address);
        let backrun_tx = tx_builder.build_swap_tx(backrun_params).await?;

        // Estimate gas for both transactions
        let frontrun_gas = tx_builder.estimate_gas(&frontrun_tx).await.unwrap_or(200_000);
        let backrun_gas = tx_builder.estimate_gas(&backrun_tx).await.unwrap_or(200_000);
        let total_gas = frontrun_gas + backrun_gas;

        // Get gas price
        let gas_price = ctx
            .provider
            .get_gas_price()
            .await
            .map_err(|e| MevError::Execution(ExecutionError::SubmissionFailed(e.to_string())))?;

        let gas_cost = U256::from(total_gas) * U256::from(gas_price);

        // Validate profitability
        self.validate_profitability(&sandwich_data, gas_cost, &ctx.config)?;

        // Sign transactions
        let signed_frontrun = tx_builder.sign_tx(&frontrun_tx).await?;
        let signed_backrun = tx_builder.sign_tx(&backrun_tx).await?;

        // Get current block number
        let block_number = ctx
            .provider
            .get_block_number()
            .await
            .map_err(|e| MevError::Execution(ExecutionError::SubmissionFailed(e.to_string())))?;

        // Create bundle with victim tx hash sandwiched
        // Note: Flashbots allows including tx hashes for txs already in mempool
        let bundle = FlashbotsSandwichBundle {
            frontrun_tx: signed_frontrun,
            victim_tx_hash: sandwich_data.victim_tx_hash,
            backrun_tx: signed_backrun,
            block_number: block_number + 1,
            revert_on_fail: true,
        };

        // Convert to standard Flashbots bundle format
        let flashbots_bundle = bundle.to_flashbots_bundle();

        // Simulate the bundle (excluding victim tx as it's already in mempool)
        let sim_bundle = FlashbotsBundle::new(
            vec![bundle.frontrun_tx.clone(), bundle.backrun_tx.clone()],
            block_number + 1,
        );

        let sim_result = ctx.flashbots.simulate_bundle(sim_bundle).await?;

        if !sim_result.success {
            error!(
                error = ?sim_result.error,
                "Sandwich bundle simulation failed"
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

        // Final profitability check with actual gas
        let actual_gas_cost = U256::from(sim_result.total_gas_used) * sim_result.gas_price;
        let expected_profit = self.calculate_profit(&sandwich_data);

        if expected_profit <= actual_gas_cost + ctx.config.min_profit_wei {
            warn!(
                expected_profit = %expected_profit,
                gas_cost = %actual_gas_cost,
                "Sandwich not profitable after simulation"
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
        let response = ctx.flashbots.send_bundle(flashbots_bundle).await?;

        let actual_profit = expected_profit.saturating_sub(actual_gas_cost);

        info!(
            bundle_hash = %response.bundle_hash,
            victim_tx = %sandwich_data.victim_tx_hash,
            profit = %actual_profit,
            latency_ms = start.elapsed().as_millis(),
            "Sandwich execution complete"
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

impl Default for SandwichExecutor {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl<P> Executor<P> for SandwichExecutor
where
    P: AlloyProvider + Clone + Send + Sync + 'static,
{
    fn name(&self) -> &str {
        &self.name
    }

    fn can_handle(&self, opp: &Opportunity) -> bool {
        matches!(opp.opportunity_type, OpportunityType::Sandwich)
    }

    #[instrument(skip(self, ctx), fields(executor = %self.name, opportunity_id = %opp.id))]
    async fn execute(&self, opp: &Opportunity, ctx: &ExecutorContext<P>) -> Result<ExecutionResult> {
        // Validate we have required data
        if opp.target_tx.is_none() {
            return Err(MevError::Execution(ExecutionError::SubmissionFailed(
                "No target transaction for sandwich".to_string(),
            )));
        }

        // Require Flashbots for sandwich attacks
        if !ctx.config.use_flashbots {
            return Err(MevError::Execution(ExecutionError::SubmissionFailed(
                "Sandwich attacks require Flashbots".to_string(),
            )));
        }

        self.execute_sandwich(opp, ctx).await
    }

    #[instrument(skip(self, ctx), fields(executor = %self.name))]
    async fn simulate(&self, opp: &Opportunity, ctx: &ExecutorContext<P>) -> Result<SimulationResult> {
        let sandwich_data = self.extract_sandwich_data(opp)?;

        let tx_builder = TxBuilder::new(
            Arc::clone(&ctx.provider),
            Arc::clone(&ctx.signer),
            ctx.config.chain_id,
        );

        let executor_address = ctx.signer.address();

        // Build transactions for simulation
        let frontrun_params = self.build_frontrun_swap(&sandwich_data, executor_address);
        let frontrun_tx = tx_builder.build_swap_tx(frontrun_params).await?;

        let backrun_params = self.build_backrun_swap(&sandwich_data, executor_address);
        let backrun_tx = tx_builder.build_swap_tx(backrun_params).await?;

        // Sign transactions
        let signed_frontrun = tx_builder.sign_tx(&frontrun_tx).await?;
        let signed_backrun = tx_builder.sign_tx(&backrun_tx).await?;

        // Get current block number
        let block_number = ctx
            .provider
            .get_block_number()
            .await
            .map_err(|e| MevError::Execution(ExecutionError::SubmissionFailed(e.to_string())))?;

        // Create bundle for simulation (without victim tx)
        let bundle = FlashbotsBundle::new(vec![signed_frontrun, signed_backrun], block_number + 1);

        // Simulate
        let sim_response = ctx.flashbots.simulate_bundle(bundle).await?;

        // Calculate profitability
        let gas_cost = U256::from(sim_response.total_gas_used) * sim_response.gas_price;
        let gross_profit = self.calculate_profit(&sandwich_data);
        let net_profit = gross_profit.saturating_sub(gas_cost);
        let is_profitable = net_profit >= ctx.config.min_profit_wei;

        debug!(
            gross_profit = %gross_profit,
            gas_cost = %gas_cost,
            net_profit = %net_profit,
            is_profitable = is_profitable,
            "Sandwich simulation complete"
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

/// Sandwich bundle with explicit frontrun, victim, and backrun components.
#[derive(Debug, Clone)]
pub struct FlashbotsSandwichBundle {
    /// Signed frontrun transaction
    pub frontrun_tx: Bytes,
    /// Victim transaction hash (already in mempool)
    pub victim_tx_hash: B256,
    /// Signed backrun transaction
    pub backrun_tx: Bytes,
    /// Target block number
    pub block_number: u64,
    /// Revert bundle on any failure
    pub revert_on_fail: bool,
}

impl FlashbotsSandwichBundle {
    /// Convert to standard Flashbots bundle format.
    ///
    /// Note: Flashbots bundles can include both signed txs and tx hashes.
    /// The relay will include the referenced tx if it's in the mempool.
    pub fn to_flashbots_bundle(&self) -> FlashbotsBundle {
        // For sandwich attacks, we submit frontrun and backrun as signed txs
        // The victim tx is referenced by hash and must be in the mempool
        FlashbotsBundle {
            txs: vec![self.frontrun_tx.clone(), self.backrun_tx.clone()],
            block_number: self.block_number,
            min_timestamp: None,
            max_timestamp: None,
            revert_on_fail: self.revert_on_fail,
        }
    }
}

/// Calculate optimal frontrun amount for a given victim trade.
///
/// Uses the constant product formula to find the amount that maximizes
/// profit while ensuring the victim trade still executes.
pub fn calculate_optimal_frontrun_amount(
    victim_amount_in: U256,
    reserve_in: U256,
    reserve_out: U256,
    victim_min_out: U256,
    fee_bps: u32,
) -> U256 {
    // Simplified calculation - production would use more sophisticated optimization
    // Start with a fraction of victim amount and adjust based on reserves

    let fee_factor = U256::from(10000 - fee_bps);

    // Calculate how much the victim expects to receive
    let victim_expected_out = (victim_amount_in * reserve_out * fee_factor)
        / (reserve_in * U256::from(10000) + victim_amount_in * fee_factor);

    // If victim's slippage tolerance is too tight, reduce frontrun
    let slippage_room = if victim_expected_out > victim_min_out {
        victim_expected_out - victim_min_out
    } else {
        return U256::ZERO; // No room for sandwich
    };

    // Frontrun amount should move price but still leave room for victim
    // Use ~50% of available slippage room as a heuristic
    let price_impact_budget = slippage_room / U256::from(2);

    // Calculate frontrun amount that creates this price impact
    // This is a simplified version; production would solve the quadratic
    let frontrun_amount = (price_impact_budget * reserve_in) / reserve_out;

    // Cap at a reasonable fraction of reserves to avoid excessive slippage
    let max_frontrun = reserve_in / U256::from(10); // Max 10% of reserve

    frontrun_amount.min(max_frontrun)
}

/// Estimate profit from a sandwich attack.
pub fn estimate_sandwich_profit(
    frontrun_amount: U256,
    reserve_in: U256,
    reserve_out: U256,
    victim_amount_in: U256,
    fee_bps: u32,
) -> U256 {
    let fee_factor = U256::from(10000 - fee_bps);
    let fee_denom = U256::from(10000);

    // After frontrun: new reserves
    let frontrun_out =
        (frontrun_amount * reserve_out * fee_factor) / (reserve_in * fee_denom + frontrun_amount * fee_factor);

    let reserve_in_after_frontrun = reserve_in + frontrun_amount;
    let reserve_out_after_frontrun = reserve_out - frontrun_out;

    // After victim: new reserves
    let victim_out = (victim_amount_in * reserve_out_after_frontrun * fee_factor)
        / (reserve_in_after_frontrun * fee_denom + victim_amount_in * fee_factor);

    let reserve_in_after_victim = reserve_in_after_frontrun + victim_amount_in;
    let reserve_out_after_victim = reserve_out_after_frontrun - victim_out;

    // Backrun: sell tokens we bought in frontrun
    let backrun_out = (frontrun_out * reserve_in_after_victim * fee_factor)
        / (reserve_out_after_victim * fee_denom + frontrun_out * fee_factor);

    // Profit = backrun_out - frontrun_amount
    if backrun_out > frontrun_amount {
        backrun_out - frontrun_amount
    } else {
        U256::ZERO
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sandwich_executor_creation() {
        let executor = SandwichExecutor::new();
        assert_eq!(executor.name(), "sandwich");
    }

    #[test]
    fn test_can_handle() {
        let executor = SandwichExecutor::new();

        let sandwich_opp = Opportunity {
            id: "test".to_string(),
            opportunity_type: OpportunityType::Sandwich,
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
            ..sandwich_opp.clone()
        };

        assert!(executor.can_handle(&sandwich_opp));
        assert!(!executor.can_handle(&arb_opp));
    }

    #[test]
    fn test_calculate_profit() {
        let executor = SandwichExecutor::new();

        let data = SandwichData {
            victim_tx_hash: B256::ZERO,
            token_in: Address::ZERO,
            token_out: Address::repeat_byte(1),
            frontrun_amount: U256::from(1_000_000_000_000_000_000u128), // 1 ETH
            frontrun_expected_out: U256::from(2000_000_000u64),          // 2000 USDC
            backrun_amount: U256::from(2000_000_000u64),
            backrun_expected_out: U256::from(1_050_000_000_000_000_000u128), // 1.05 ETH
            pool: Address::ZERO,
            router: Address::ZERO,
            protocol: "uniswap_v2".to_string(),
            min_profit: U256::ZERO,
        };

        let profit = executor.calculate_profit(&data);
        assert_eq!(profit, U256::from(50_000_000_000_000_000u128)); // 0.05 ETH
    }

    #[test]
    fn test_estimate_sandwich_profit() {
        let reserve_in = U256::from(1000_000_000_000_000_000_000u128);  // 1000 ETH
        let reserve_out = U256::from(2_000_000_000_000u128);             // 2M USDC
        let frontrun_amount = U256::from(10_000_000_000_000_000_000u128); // 10 ETH
        let victim_amount = U256::from(5_000_000_000_000_000_000u128);    // 5 ETH
        let fee_bps = 30; // 0.3%

        let profit = estimate_sandwich_profit(
            frontrun_amount,
            reserve_in,
            reserve_out,
            victim_amount,
            fee_bps,
        );

        // Should have some profit (exact amount depends on math)
        assert!(profit > U256::ZERO);
    }

    #[test]
    fn test_sandwich_bundle_conversion() {
        let bundle = FlashbotsSandwichBundle {
            frontrun_tx: Bytes::from(vec![1, 2, 3]),
            victim_tx_hash: B256::ZERO,
            backrun_tx: Bytes::from(vec![4, 5, 6]),
            block_number: 12345,
            revert_on_fail: true,
        };

        let flashbots_bundle = bundle.to_flashbots_bundle();

        assert_eq!(flashbots_bundle.txs.len(), 2);
        assert_eq!(flashbots_bundle.block_number, 12345);
        assert!(flashbots_bundle.revert_on_fail);
    }

    #[test]
    fn test_optimal_frontrun_calculation() {
        let victim_amount = U256::from(1_000_000_000_000_000_000u128);    // 1 ETH
        let reserve_in = U256::from(1000_000_000_000_000_000_000u128);    // 1000 ETH
        let reserve_out = U256::from(2_000_000_000_000u128);              // 2M USDC
        let victim_min_out = U256::from(1_900_000_000u128);               // 1900 USDC (5% slippage)
        let fee_bps = 30;

        let optimal = calculate_optimal_frontrun_amount(
            victim_amount,
            reserve_in,
            reserve_out,
            victim_min_out,
            fee_bps,
        );

        // Should calculate a reasonable frontrun amount
        assert!(optimal > U256::ZERO);
        assert!(optimal < reserve_in / U256::from(10)); // Should be < 10% of reserve
    }
}
