//! Flashloan-assisted arbitrage execution strategy.
//!
//! This module implements the FlashloanExecutor for executing arbitrage
//! opportunities using borrowed capital from flashloan providers like
//! Balancer (0% fee) or Aave V3 (0.09% fee).

use alloy::primitives::{address, Address, Bytes, U256};
use alloy::providers::Provider as AlloyProvider;
use alloy::sol;
use alloy::sol_types::SolInterface;
use async_trait::async_trait;
use std::sync::Arc;
use tracing::{debug, error, info, instrument, warn};

use super::flashbots::FlashbotsBundle;
use super::tx_builder::TxBuilder;
use super::{
    ExecutionResult, Executor, ExecutorContext, Opportunity, OpportunityType,
    SimulationResult, SwapPath, SwapStep,
};
use crate::error::{ExecutionError, MevError};

/// Result type for flashloan operations.
pub type Result<T> = std::result::Result<T, MevError>;

// Solidity interface definitions for flashloan contracts
sol! {
    /// Balancer Vault flashloan interface
    interface IBalancerVault {
        function flashLoan(
            address recipient,
            address[] memory tokens,
            uint256[] memory amounts,
            bytes memory userData
        ) external;
    }

    /// Aave V3 Pool flashloan interface
    interface IAaveV3Pool {
        function flashLoanSimple(
            address receiverAddress,
            address asset,
            uint256 amount,
            bytes calldata params,
            uint16 referralCode
        ) external;

        function flashLoan(
            address receiverAddress,
            address[] calldata assets,
            uint256[] calldata amounts,
            uint256[] calldata interestRateModes,
            address onBehalfOf,
            bytes calldata params,
            uint16 referralCode
        ) external;
    }

    /// Our FlashloanArbitrage contract interface
    interface IFlashloanArbitrage {
        function executeArbitrage(
            address tokenBorrow,
            uint256 borrowAmount,
            address[] calldata swapPath,
            address[] calldata pools,
            bytes[] calldata swapData,
            uint256 minProfit
        ) external;

        function executeArbitrageWithCallback(
            address tokenBorrow,
            uint256 borrowAmount,
            bytes calldata arbitrageParams
        ) external;
    }
}

/// Flashloan provider types
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlashloanProvider {
    /// Balancer - 0% fee flashloans
    Balancer,
    /// Aave V3 - 0.09% fee flashloans
    AaveV3,
    /// Aave V2 - 0.09% fee flashloans (legacy)
    AaveV2,
    /// dYdX - 0% fee flashloans (requires 2 wei repay buffer)
    DyDx,
    /// Uniswap V3 - fee based on pool tier
    UniswapV3,
}

impl FlashloanProvider {
    /// Get the fee in basis points for this provider.
    pub fn fee_bps(&self) -> u32 {
        match self {
            FlashloanProvider::Balancer => 0,
            FlashloanProvider::DyDx => 0,
            FlashloanProvider::AaveV3 => 9,      // 0.09%
            FlashloanProvider::AaveV2 => 9,      // 0.09%
            FlashloanProvider::UniswapV3 => 30,  // Depends on pool, use 0.3% as default
        }
    }

    /// Get the contract address for this provider on the given chain.
    pub fn contract_address(&self, chain_id: u64) -> Option<Address> {
        match (self, chain_id) {
            // Ethereum Mainnet
            (FlashloanProvider::Balancer, 1) => {
                Some(address!("BA12222222228d8Ba445958a75a0704d566BF2C8"))
            }
            (FlashloanProvider::AaveV3, 1) => {
                Some(address!("87870Bca3F3fD6335C3F4ce8392D69350B4fA4E2"))
            }
            (FlashloanProvider::AaveV2, 1) => {
                Some(address!("7d2768dE32b0b80b7a3454c06BdAc94A69DDc7A9"))
            }

            // Arbitrum
            (FlashloanProvider::Balancer, 42161) => {
                Some(address!("BA12222222228d8Ba445958a75a0704d566BF2C8"))
            }
            (FlashloanProvider::AaveV3, 42161) => {
                Some(address!("794a61358D6845594F94dc1DB02A252b5b4814aD"))
            }

            // Polygon
            (FlashloanProvider::Balancer, 137) => {
                Some(address!("BA12222222228d8Ba445958a75a0704d566BF2C8"))
            }
            (FlashloanProvider::AaveV3, 137) => {
                Some(address!("794a61358D6845594F94dc1DB02A252b5b4814aD"))
            }

            // Base
            (FlashloanProvider::AaveV3, 8453) => {
                Some(address!("A238Dd80C259a72e81d7e4664a9801593F98d1c5"))
            }

            _ => None,
        }
    }
}

/// Configuration for flashloan execution.
#[derive(Debug, Clone)]
pub struct FlashloanConfig {
    /// Preferred flashloan provider (uses lowest fee by default)
    pub preferred_provider: Option<FlashloanProvider>,
    /// Address of the deployed FlashloanArbitrage contract
    pub arbitrage_contract: Address,
    /// Maximum flashloan fee tolerance in basis points
    pub max_fee_bps: u32,
    /// Minimum profit after fees and gas
    pub min_profit_wei: U256,
}

impl Default for FlashloanConfig {
    fn default() -> Self {
        Self {
            preferred_provider: Some(FlashloanProvider::Balancer), // 0% fee
            arbitrage_contract: Address::ZERO, // Must be set
            max_fee_bps: 10,                   // 0.1% max
            min_profit_wei: U256::from(10_000_000_000_000_000u64), // 0.01 ETH
        }
    }
}

/// Flashloan executor for large arbitrage opportunities.
///
/// For opportunities requiring more capital than available, this executor:
/// 1. Borrows capital via flashloan (Balancer 0% fee preferred)
/// 2. Executes arbitrage swaps
/// 3. Repays flashloan + fee
/// 4. Keeps profit
pub struct FlashloanExecutor {
    /// Executor name
    name: String,
    /// Flashloan configuration
    config: FlashloanConfig,
}

impl FlashloanExecutor {
    /// Create a new flashloan executor.
    pub fn new(arbitrage_contract: Address) -> Self {
        Self {
            name: "flashloan_arbitrage".to_string(),
            config: FlashloanConfig {
                arbitrage_contract,
                ..Default::default()
            },
        }
    }

    /// Create a flashloan executor with custom configuration.
    pub fn with_config(config: FlashloanConfig) -> Self {
        Self {
            name: "flashloan_arbitrage".to_string(),
            config,
        }
    }

    /// Returns the name of this executor.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Check if this executor can handle the given opportunity.
    pub fn can_handle(&self, opp: &Opportunity) -> bool {
        matches!(opp.opportunity_type, OpportunityType::Arbitrage)
    }

    /// Select the best flashloan provider for the given token and amount.
    fn select_provider(&self, _token: Address, chain_id: u64) -> Option<FlashloanProvider> {
        // Prefer provider with lowest fee that supports this chain
        let providers = [
            FlashloanProvider::Balancer, // 0% fee
            FlashloanProvider::DyDx,     // 0% fee
            FlashloanProvider::AaveV3,   // 0.09% fee
        ];

        // If preferred provider is set and available, use it
        if let Some(preferred) = self.config.preferred_provider {
            if preferred.contract_address(chain_id).is_some()
                && preferred.fee_bps() <= self.config.max_fee_bps
            {
                return Some(preferred);
            }
        }

        // Otherwise find the cheapest available provider
        providers
            .into_iter()
            .filter(|p| {
                p.contract_address(chain_id).is_some() && p.fee_bps() <= self.config.max_fee_bps
            })
            .min_by_key(|p| p.fee_bps())
    }

    /// Calculate the repayment amount including flashloan fee.
    fn calculate_repayment(&self, borrow_amount: U256, provider: FlashloanProvider) -> U256 {
        let fee = borrow_amount * U256::from(provider.fee_bps()) / U256::from(10000);
        borrow_amount + fee
    }

    /// Encode flashloan calldata for Balancer.
    fn encode_balancer_flashloan(
        &self,
        token: Address,
        amount: U256,
        callback_data: Bytes,
    ) -> Bytes {
        let tokens = vec![token];
        let amounts = vec![amount];

        let call = IBalancerVault::flashLoanCall {
            recipient: self.config.arbitrage_contract,
            tokens,
            amounts,
            userData: callback_data,
        };

        Bytes::from(IBalancerVault::IBalancerVaultCalls::flashLoan(call).abi_encode())
    }

    /// Encode flashloan calldata for Aave V3.
    fn encode_aave_flashloan(
        &self,
        token: Address,
        amount: U256,
        callback_data: Bytes,
    ) -> Bytes {
        let call = IAaveV3Pool::flashLoanSimpleCall {
            receiverAddress: self.config.arbitrage_contract,
            asset: token,
            amount,
            params: callback_data,
            referralCode: 0,
        };

        Bytes::from(IAaveV3Pool::IAaveV3PoolCalls::flashLoanSimple(call).abi_encode())
    }

    /// Encode arbitrage callback data.
    fn encode_arbitrage_params(
        &self,
        swap_path: &SwapPath,
        min_profit: U256,
    ) -> Bytes {
        let tokens: Vec<Address> = swap_path
            .steps
            .iter()
            .map(|s| s.token_in)
            .chain(std::iter::once(
                swap_path.steps.last().map(|s| s.token_out).unwrap_or(Address::ZERO),
            ))
            .collect();

        let pools: Vec<Address> = swap_path.steps.iter().map(|s| s.pool).collect();

        // Encode swap data for each step
        let swap_data: Vec<Bytes> = swap_path
            .steps
            .iter()
            .map(|step| self.encode_swap_step(step))
            .collect();

        let call = IFlashloanArbitrage::executeArbitrageCall {
            tokenBorrow: swap_path.steps.first().map(|s| s.token_in).unwrap_or(Address::ZERO),
            borrowAmount: swap_path.input_amount,
            swapPath: tokens,
            pools,
            swapData: swap_data,
            minProfit: min_profit,
        };

        Bytes::from(
            IFlashloanArbitrage::IFlashloanArbitrageCalls::executeArbitrage(call).abi_encode(),
        )
    }

    /// Encode a single swap step.
    fn encode_swap_step(&self, step: &SwapStep) -> Bytes {
        // Encode protocol-specific swap data
        // This is a simplified version; production would use RouterEncoder
        let mut data = Vec::new();

        // Protocol identifier (1 byte)
        let protocol_id = match step.protocol.as_str() {
            "uniswap_v2" => 0u8,
            "uniswap_v3" => 1u8,
            "sushiswap" => 2u8,
            "curve" => 3u8,
            _ => 255u8,
        };
        data.push(protocol_id);

        // Fee tier for V3 (4 bytes)
        data.extend_from_slice(&step.fee.unwrap_or(3000).to_be_bytes());

        Bytes::from(data)
    }

    /// Build and execute flashloan arbitrage.
    async fn execute_flashloan_arb<P>(
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
                "No swap path provided".to_string(),
            ))
        })?;

        // Get the token to borrow (first token in path)
        let borrow_token = swap_path
            .steps
            .first()
            .map(|s| s.token_in)
            .ok_or_else(|| {
                MevError::Execution(ExecutionError::SubmissionFailed(
                    "Empty swap path".to_string(),
                ))
            })?;

        let borrow_amount = swap_path.input_amount;

        // Select flashloan provider
        let provider = self
            .select_provider(borrow_token, ctx.config.chain_id)
            .ok_or_else(|| {
                MevError::Execution(ExecutionError::SubmissionFailed(
                    "No flashloan provider available".to_string(),
                ))
            })?;

        let provider_address = provider
            .contract_address(ctx.config.chain_id)
            .ok_or_else(|| {
                MevError::Execution(ExecutionError::SubmissionFailed(
                    "Flashloan provider not deployed on this chain".to_string(),
                ))
            })?;

        info!(
            provider = ?provider,
            borrow_token = %borrow_token,
            borrow_amount = %borrow_amount,
            "Selected flashloan provider"
        );

        // Calculate repayment and verify profitability
        let repayment = self.calculate_repayment(borrow_amount, provider);
        let flashloan_fee = repayment - borrow_amount;

        // Verify we can still profit after flashloan fee
        if opp.estimated_profit_wei <= flashloan_fee + opp.estimated_gas_wei + self.config.min_profit_wei {
            warn!(
                estimated_profit = %opp.estimated_profit_wei,
                flashloan_fee = %flashloan_fee,
                gas_cost = %opp.estimated_gas_wei,
                "Opportunity not profitable after flashloan fee"
            );
            return Ok(ExecutionResult {
                success: false,
                tx_hash: None,
                bundle_hash: None,
                block_number: None,
                actual_profit: None,
                gas_used: None,
                gas_price: None,
                error: Some("Not profitable after flashloan fee".to_string()),
                latency_ms: start.elapsed().as_millis() as u64,
            });
        }

        // Encode arbitrage callback data
        let min_profit = opp.estimated_profit_wei
            .saturating_sub(flashloan_fee)
            .saturating_sub(opp.estimated_gas_wei)
            * U256::from(90) / U256::from(100); // 10% slippage buffer

        let callback_data = self.encode_arbitrage_params(swap_path, min_profit);

        // Encode flashloan call
        let flashloan_calldata = match provider {
            FlashloanProvider::Balancer => {
                self.encode_balancer_flashloan(borrow_token, borrow_amount, callback_data)
            }
            FlashloanProvider::AaveV3 | FlashloanProvider::AaveV2 => {
                self.encode_aave_flashloan(borrow_token, borrow_amount, callback_data)
            }
            _ => {
                return Err(MevError::Execution(ExecutionError::SubmissionFailed(
                    "Unsupported flashloan provider".to_string(),
                )));
            }
        };

        // Build transaction
        let tx_builder = TxBuilder::new(
            Arc::clone(&ctx.provider),
            Arc::clone(&ctx.signer),
            ctx.config.chain_id,
        );

        let tx = tx_builder
            .build_raw_tx(provider_address, U256::ZERO, flashloan_calldata, None)
            .await?;

        let signed_tx = tx_builder.sign_tx(&tx).await?;

        // Get current block number
        let block_number = ctx
            .provider
            .get_block_number()
            .await
            .map_err(|e| MevError::Execution(ExecutionError::SubmissionFailed(e.to_string())))?;

        // Create Flashbots bundle
        let bundle = FlashbotsBundle::new(vec![signed_tx], block_number + 1)
            .with_revert_on_fail(true);

        // Simulate bundle
        let sim_result = ctx.flashbots.simulate_bundle(bundle.clone()).await?;

        if !sim_result.success {
            error!(
                error = ?sim_result.error,
                "Flashloan bundle simulation failed"
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

        // Calculate actual gas cost
        let gas_cost = U256::from(sim_result.total_gas_used) * sim_result.gas_price;
        let total_cost = gas_cost + flashloan_fee;

        // Final profitability check
        if opp.estimated_profit_wei <= total_cost + ctx.config.min_profit_wei {
            warn!(
                estimated_profit = %opp.estimated_profit_wei,
                total_cost = %total_cost,
                "Opportunity not profitable after all costs"
            );
            return Ok(ExecutionResult {
                success: false,
                tx_hash: None,
                bundle_hash: None,
                block_number: None,
                actual_profit: None,
                gas_used: Some(sim_result.total_gas_used),
                gas_price: Some(sim_result.gas_price),
                error: Some("Not profitable after all costs".to_string()),
                latency_ms: start.elapsed().as_millis() as u64,
            });
        }

        // Submit bundle
        let response = ctx.flashbots.send_bundle(bundle).await?;

        let actual_profit = opp.estimated_profit_wei.saturating_sub(total_cost);

        info!(
            bundle_hash = %response.bundle_hash,
            profit = %actual_profit,
            flashloan_fee = %flashloan_fee,
            gas_cost = %gas_cost,
            latency_ms = start.elapsed().as_millis(),
            "Flashloan arbitrage execution complete"
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

impl Default for FlashloanExecutor {
    fn default() -> Self {
        Self::new(Address::ZERO)
    }
}

#[async_trait]
impl<P> Executor<P> for FlashloanExecutor
where
    P: AlloyProvider + Clone + Send + Sync + 'static,
{
    fn name(&self) -> &str {
        &self.name
    }

    fn can_handle(&self, opp: &Opportunity) -> bool {
        // Handle arbitrage opportunities that require flashloan
        // (determined by metadata flag or amount exceeds available balance)
        matches!(opp.opportunity_type, OpportunityType::Arbitrage)
            && opp.metadata.get("use_flashloan").and_then(|v| v.as_bool()).unwrap_or(false)
    }

    #[instrument(skip(self, ctx), fields(executor = %self.name, opportunity_id = %opp.id))]
    async fn execute(&self, opp: &Opportunity, ctx: &ExecutorContext<P>) -> Result<ExecutionResult> {
        // Validate arbitrage contract is set
        if self.config.arbitrage_contract == Address::ZERO {
            return Err(MevError::Execution(ExecutionError::SubmissionFailed(
                "Flashloan arbitrage contract address not configured".to_string(),
            )));
        }

        info!(
            contract = %self.config.arbitrage_contract,
            estimated_profit = %opp.estimated_profit_wei,
            "Executing flashloan arbitrage"
        );

        self.execute_flashloan_arb(opp, ctx).await
    }

    #[instrument(skip(self, ctx), fields(executor = %self.name))]
    async fn simulate(&self, opp: &Opportunity, ctx: &ExecutorContext<P>) -> Result<SimulationResult> {
        let swap_path = opp.swap_path.as_ref().ok_or_else(|| {
            MevError::Execution(ExecutionError::SubmissionFailed(
                "No swap path provided".to_string(),
            ))
        })?;

        let borrow_token = swap_path
            .steps
            .first()
            .map(|s| s.token_in)
            .ok_or_else(|| {
                MevError::Execution(ExecutionError::SubmissionFailed(
                    "Empty swap path".to_string(),
                ))
            })?;

        // Select provider and calculate fees
        let provider = self
            .select_provider(borrow_token, ctx.config.chain_id)
            .ok_or_else(|| {
                MevError::Execution(ExecutionError::SubmissionFailed(
                    "No flashloan provider available".to_string(),
                ))
            })?;

        let repayment = self.calculate_repayment(swap_path.input_amount, provider);
        let flashloan_fee = repayment - swap_path.input_amount;

        // Estimate gas (flashloan arb is typically 350k-500k gas)
        let estimated_gas = 450_000u64;
        let gas_price = ctx
            .provider
            .get_gas_price()
            .await
            .map_err(|e| MevError::Execution(ExecutionError::SubmissionFailed(e.to_string())))?;

        let gas_cost = U256::from(estimated_gas) * U256::from(gas_price);
        let total_cost = gas_cost + flashloan_fee;

        let profit = if opp.estimated_profit_wei > total_cost {
            opp.estimated_profit_wei - total_cost
        } else {
            U256::ZERO
        };

        let is_profitable = profit >= ctx.config.min_profit_wei;

        debug!(
            flashloan_fee = %flashloan_fee,
            gas_cost = %gas_cost,
            profit = %profit,
            is_profitable = is_profitable,
            "Flashloan arbitrage simulation complete"
        );

        Ok(SimulationResult {
            success: true,
            profit,
            gas_used: estimated_gas,
            state_changes: Vec::new(),
            logs: Vec::new(),
            error: None,
            is_profitable,
        })
    }
}

/// Determine if flashloan is needed for an opportunity.
pub fn should_use_flashloan(
    required_amount: U256,
    available_balance: U256,
    estimated_profit: U256,
    min_profit_multiplier: u32,
) -> bool {
    // Use flashloan if:
    // 1. Required amount exceeds available balance
    // 2. Profit is large enough to justify flashloan overhead
    required_amount > available_balance
        && estimated_profit >= required_amount / U256::from(min_profit_multiplier)
}

/// Calculate maximum borrowable amount for a given profit target.
pub fn calculate_max_borrow(
    expected_profit_rate_bps: u32,
    flashloan_fee_bps: u32,
    target_profit: U256,
    gas_cost: U256,
) -> U256 {
    // profit = borrow * profit_rate - borrow * fee - gas
    // target + gas = borrow * (profit_rate - fee)
    // borrow = (target + gas) / (profit_rate - fee)

    if expected_profit_rate_bps <= flashloan_fee_bps {
        return U256::ZERO;
    }

    let rate_diff = U256::from(expected_profit_rate_bps - flashloan_fee_bps);
    let numerator = (target_profit + gas_cost) * U256::from(10000);

    numerator / rate_diff
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_flashloan_provider_fees() {
        assert_eq!(FlashloanProvider::Balancer.fee_bps(), 0);
        assert_eq!(FlashloanProvider::AaveV3.fee_bps(), 9);
        assert_eq!(FlashloanProvider::DyDx.fee_bps(), 0);
    }

    #[test]
    fn test_flashloan_provider_addresses() {
        // Mainnet Balancer
        let balancer = FlashloanProvider::Balancer.contract_address(1);
        assert!(balancer.is_some());
        assert_eq!(
            balancer.unwrap(),
            address!("BA12222222228d8Ba445958a75a0704d566BF2C8")
        );

        // Unknown chain
        let unknown = FlashloanProvider::Balancer.contract_address(99999);
        assert!(unknown.is_none());
    }

    #[test]
    fn test_calculate_repayment() {
        let executor = FlashloanExecutor::new(Address::ZERO);
        let borrow = U256::from(1_000_000_000_000_000_000u128); // 1 ETH

        // Balancer (0% fee)
        let repay_balancer = executor.calculate_repayment(borrow, FlashloanProvider::Balancer);
        assert_eq!(repay_balancer, borrow);

        // Aave V3 (0.09% fee)
        let repay_aave = executor.calculate_repayment(borrow, FlashloanProvider::AaveV3);
        let expected_fee = borrow * U256::from(9) / U256::from(10000);
        assert_eq!(repay_aave, borrow + expected_fee);
    }

    #[test]
    fn test_should_use_flashloan() {
        let required = U256::from(10_000_000_000_000_000_000u128); // 10 ETH
        let available = U256::from(1_000_000_000_000_000_000u128); // 1 ETH
        let profit = U256::from(100_000_000_000_000_000u128);      // 0.1 ETH

        // Should use flashloan: need more than available and profit is good
        assert!(should_use_flashloan(required, available, profit, 100));

        // Should not use flashloan: have enough balance
        let available_plenty = U256::from(20_000_000_000_000_000_000u128);
        assert!(!should_use_flashloan(required, available_plenty, profit, 100));

        // Should not use flashloan: profit too small
        let small_profit = U256::from(1_000_000_000_000_000u128); // 0.001 ETH
        assert!(!should_use_flashloan(required, available, small_profit, 100));
    }

    #[test]
    fn test_calculate_max_borrow() {
        // 1% profit rate, 0.1% flashloan fee, want 0.1 ETH profit, 0.01 ETH gas
        let max_borrow = calculate_max_borrow(
            100, // 1%
            10,  // 0.1%
            U256::from(100_000_000_000_000_000u128), // 0.1 ETH
            U256::from(10_000_000_000_000_000u128),  // 0.01 ETH
        );

        // (0.1 + 0.01) * 10000 / (100 - 10) = 1.1 * 10000 / 90 = 122.22 ETH
        assert!(max_borrow > U256::from(100_000_000_000_000_000_000u128));
    }

    #[test]
    fn test_executor_can_handle() {
        let executor = FlashloanExecutor::new(Address::ZERO);

        // Should handle arbitrage with flashloan flag
        let mut opp = Opportunity {
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
            metadata: serde_json::json!({"use_flashloan": true}),
        };

        assert!(executor.can_handle(&opp));

        // Should not handle without flashloan flag
        opp.metadata = serde_json::json!({"use_flashloan": false});
        assert!(!executor.can_handle(&opp));

        // Should not handle sandwich
        opp.opportunity_type = OpportunityType::Sandwich;
        opp.metadata = serde_json::json!({"use_flashloan": true});
        assert!(!executor.can_handle(&opp));
    }
}
