//! Liquidation execution strategy.
//!
//! This module implements the LiquidationExecutor for executing liquidation
//! opportunities in lending protocols like Aave, Compound, and Morpho.

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
    SimulationResult,
};
use crate::error::{ExecutionError, MevError};

/// Result type for liquidation operations.
pub type Result<T> = std::result::Result<T, MevError>;

// Solidity interface definitions for lending protocols
sol! {
    /// Aave V2 Lending Pool liquidation interface
    interface IAaveV2LendingPool {
        function liquidationCall(
            address collateralAsset,
            address debtAsset,
            address user,
            uint256 debtToCover,
            bool receiveAToken
        ) external;
    }

    /// Aave V3 Pool liquidation interface
    interface IAaveV3Pool {
        function liquidationCall(
            address collateralAsset,
            address debtAsset,
            address user,
            uint256 debtToCover,
            bool receiveAToken
        ) external;
    }

    /// Compound V2 cToken liquidation interface
    interface ICompoundCToken {
        function liquidateBorrow(
            address borrower,
            uint256 repayAmount,
            address cTokenCollateral
        ) external returns (uint256);
    }

    /// Compound V3 Comet liquidation interface
    interface ICompoundComet {
        function absorb(address absorber, address[] calldata accounts) external;
        function buyCollateral(
            address asset,
            uint256 minAmount,
            uint256 baseAmount,
            address recipient
        ) external;
    }

    /// Morpho liquidation interface
    interface IMorpho {
        function liquidate(
            address borrower,
            address poolToken,
            uint256 amount,
            address collateralToken
        ) external;
    }

    /// ERC20 interface for approvals
    interface IERC20 {
        function approve(address spender, uint256 amount) external returns (bool);
        function balanceOf(address account) external view returns (uint256);
    }
}

/// Supported lending protocols for liquidation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LendingProtocol {
    AaveV2,
    AaveV3,
    CompoundV2,
    CompoundV3,
    Morpho,
}

impl LendingProtocol {
    /// Get the pool/comptroller address for this protocol on the given chain.
    pub fn pool_address(&self, chain_id: u64) -> Option<Address> {
        match (self, chain_id) {
            // Ethereum Mainnet
            (LendingProtocol::AaveV2, 1) => {
                Some(address!("7d2768dE32b0b80b7a3454c06BdAc94A69DDc7A9"))
            }
            (LendingProtocol::AaveV3, 1) => {
                Some(address!("87870Bca3F3fD6335C3F4ce8392D69350B4fA4E2"))
            }
            (LendingProtocol::CompoundV2, 1) => {
                // Comptroller
                Some(address!("3d9819210A31b4961b30EF54bE2aeD79B9c9Cd3B"))
            }
            (LendingProtocol::CompoundV3, 1) => {
                // USDC Comet
                Some(address!("c3d688B66703497DAA19211EEdff47f25384cdc3"))
            }
            (LendingProtocol::Morpho, 1) => {
                Some(address!("8888882f8f843896699869179fB6E4f7e3B58888"))
            }

            // Arbitrum
            (LendingProtocol::AaveV3, 42161) => {
                Some(address!("794a61358D6845594F94dc1DB02A252b5b4814aD"))
            }

            // Polygon
            (LendingProtocol::AaveV3, 137) => {
                Some(address!("794a61358D6845594F94dc1DB02A252b5b4814aD"))
            }

            // Base
            (LendingProtocol::AaveV3, 8453) => {
                Some(address!("A238Dd80C259a72e81d7e4664a9801593F98d1c5"))
            }
            (LendingProtocol::CompoundV3, 8453) => {
                // USDC Comet on Base
                Some(address!("9c4ec768c28520B50860ea7a15bd7213a9fF58bf"))
            }

            _ => None,
        }
    }

    /// Get the liquidation bonus in basis points for this protocol.
    pub fn default_liquidation_bonus_bps(&self) -> u32 {
        match self {
            LendingProtocol::AaveV2 => 500,     // 5%
            LendingProtocol::AaveV3 => 500,     // 5% (varies by asset)
            LendingProtocol::CompoundV2 => 800, // 8%
            LendingProtocol::CompoundV3 => 500, // 5%
            LendingProtocol::Morpho => 500,     // 5%
        }
    }
}

/// Liquidation opportunity data.
#[derive(Debug, Clone)]
pub struct LiquidationData {
    /// Lending protocol
    pub protocol: LendingProtocol,
    /// Borrower address to liquidate
    pub borrower: Address,
    /// Debt asset to repay
    pub debt_asset: Address,
    /// Collateral asset to receive
    pub collateral_asset: Address,
    /// Amount of debt to repay
    pub debt_amount: U256,
    /// Expected collateral to receive
    pub collateral_amount: U256,
    /// Liquidation bonus in basis points
    pub liquidation_bonus_bps: u32,
    /// Health factor of the position (scaled by 1e18)
    pub health_factor: U256,
    /// Whether to use flashloan for capital
    pub use_flashloan: bool,
    /// cToken address for Compound V2
    pub ctoken_collateral: Option<Address>,
}

/// Configuration for liquidation execution.
#[derive(Debug, Clone)]
pub struct LiquidationConfig {
    /// Minimum profit threshold in wei
    pub min_profit_wei: U256,
    /// Maximum gas price willing to pay
    pub max_gas_price_wei: U256,
    /// Whether to receive aTokens (Aave) instead of underlying
    pub receive_atoken: bool,
    /// Flashloan contract address for large liquidations
    pub flashloan_contract: Option<Address>,
    /// Slippage tolerance for collateral sale in basis points
    pub sale_slippage_bps: u32,
}

impl Default for LiquidationConfig {
    fn default() -> Self {
        Self {
            min_profit_wei: U256::from(10_000_000_000_000_000u64), // 0.01 ETH
            max_gas_price_wei: U256::from(100_000_000_000u64),     // 100 gwei
            receive_atoken: false,
            flashloan_contract: None,
            sale_slippage_bps: 100, // 1%
        }
    }
}

/// Liquidation executor for lending protocol opportunities.
///
/// This executor handles liquidations by:
/// 1. Checking if debt amount exceeds available balance
/// 2. If yes, use flashloan for capital
/// 3. Build liquidation call for the specific protocol
/// 4. Submit via Flashbots for MEV protection
pub struct LiquidationExecutor {
    /// Executor name
    name: String,
    /// Liquidation configuration
    config: LiquidationConfig,
}

impl LiquidationExecutor {
    /// Create a new liquidation executor with default settings.
    pub fn new() -> Self {
        Self {
            name: "liquidation".to_string(),
            config: LiquidationConfig::default(),
        }
    }

    /// Create a liquidation executor with custom configuration.
    pub fn with_config(config: LiquidationConfig) -> Self {
        Self {
            name: "liquidation".to_string(),
            config,
        }
    }

    /// Returns the name of this executor.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Check if this executor can handle the given opportunity.
    pub fn can_handle(&self, opp: &Opportunity) -> bool {
        matches!(opp.opportunity_type, OpportunityType::Liquidation)
    }

    /// Extract liquidation data from opportunity.
    fn extract_liquidation_data(&self, opp: &Opportunity) -> Result<LiquidationData> {
        let metadata = &opp.metadata;

        let protocol_str = metadata
            .get("protocol")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                MevError::Execution(ExecutionError::SubmissionFailed(
                    "Missing protocol in metadata".to_string(),
                ))
            })?;

        let protocol = match protocol_str {
            "aave_v2" => LendingProtocol::AaveV2,
            "aave_v3" => LendingProtocol::AaveV3,
            "compound_v2" => LendingProtocol::CompoundV2,
            "compound_v3" => LendingProtocol::CompoundV3,
            "morpho" => LendingProtocol::Morpho,
            _ => {
                return Err(MevError::Execution(ExecutionError::SubmissionFailed(
                    format!("Unknown protocol: {}", protocol_str),
                )))
            }
        };

        let borrower = metadata
            .get("borrower")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<Address>().ok())
            .ok_or_else(|| {
                MevError::Execution(ExecutionError::SubmissionFailed(
                    "Missing borrower address".to_string(),
                ))
            })?;

        let debt_asset = opp.tokens.first().copied().ok_or_else(|| {
            MevError::Execution(ExecutionError::SubmissionFailed(
                "Missing debt_asset".to_string(),
            ))
        })?;

        let collateral_asset = opp.tokens.get(1).copied().ok_or_else(|| {
            MevError::Execution(ExecutionError::SubmissionFailed(
                "Missing collateral_asset".to_string(),
            ))
        })?;

        let debt_amount = metadata
            .get("debt_amount")
            .and_then(|v| v.as_str())
            .and_then(|s| U256::from_str_radix(s, 10).ok())
            .ok_or_else(|| {
                MevError::Execution(ExecutionError::SubmissionFailed(
                    "Missing debt_amount".to_string(),
                ))
            })?;

        let collateral_amount = metadata
            .get("collateral_amount")
            .and_then(|v| v.as_str())
            .and_then(|s| U256::from_str_radix(s, 10).ok())
            .ok_or_else(|| {
                MevError::Execution(ExecutionError::SubmissionFailed(
                    "Missing collateral_amount".to_string(),
                ))
            })?;

        let liquidation_bonus_bps = metadata
            .get("liquidation_bonus_bps")
            .and_then(|v| v.as_u64())
            .map(|v| v as u32)
            .unwrap_or_else(|| protocol.default_liquidation_bonus_bps());

        let health_factor = metadata
            .get("health_factor")
            .and_then(|v| v.as_str())
            .and_then(|s| U256::from_str_radix(s, 10).ok())
            .unwrap_or(U256::ZERO);

        let use_flashloan = metadata
            .get("use_flashloan")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let ctoken_collateral = metadata
            .get("ctoken_collateral")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<Address>().ok());

        Ok(LiquidationData {
            protocol,
            borrower,
            debt_asset,
            collateral_asset,
            debt_amount,
            collateral_amount,
            liquidation_bonus_bps,
            health_factor,
            use_flashloan,
            ctoken_collateral,
        })
    }

    /// Build liquidation calldata for Aave V2/V3.
    fn build_aave_liquidation_call(&self, data: &LiquidationData) -> Bytes {
        let call = IAaveV3Pool::liquidationCallCall {
            collateralAsset: data.collateral_asset,
            debtAsset: data.debt_asset,
            user: data.borrower,
            debtToCover: data.debt_amount,
            receiveAToken: self.config.receive_atoken,
        };

        Bytes::from(IAaveV3Pool::IAaveV3PoolCalls::liquidationCall(call).abi_encode())
    }

    /// Build liquidation calldata for Compound V2.
    fn build_compound_v2_liquidation_call(&self, data: &LiquidationData) -> Result<Bytes> {
        let ctoken_collateral = data.ctoken_collateral.ok_or_else(|| {
            MevError::Execution(ExecutionError::SubmissionFailed(
                "Missing cToken collateral address for Compound V2".to_string(),
            ))
        })?;

        let call = ICompoundCToken::liquidateBorrowCall {
            borrower: data.borrower,
            repayAmount: data.debt_amount,
            cTokenCollateral: ctoken_collateral,
        };

        Ok(Bytes::from(
            ICompoundCToken::ICompoundCTokenCalls::liquidateBorrow(call).abi_encode(),
        ))
    }

    /// Build liquidation calldata for Compound V3.
    fn build_compound_v3_liquidation_call(&self, data: &LiquidationData, absorber: Address) -> Bytes {
        let call = ICompoundComet::absorbCall {
            absorber,
            accounts: vec![data.borrower],
        };

        Bytes::from(ICompoundComet::ICompoundCometCalls::absorb(call).abi_encode())
    }

    /// Build liquidation calldata for Morpho.
    fn build_morpho_liquidation_call(&self, data: &LiquidationData) -> Bytes {
        let pool_token = data.debt_asset; // Simplified; actual Morpho needs pool token address

        let call = IMorpho::liquidateCall {
            borrower: data.borrower,
            poolToken: pool_token,
            amount: data.debt_amount,
            collateralToken: data.collateral_asset,
        };

        Bytes::from(IMorpho::IMorphoCalls::liquidate(call).abi_encode())
    }

    /// Build ERC20 approval calldata.
    fn build_approval_call(&self, spender: Address, amount: U256) -> Bytes {
        let call = IERC20::approveCall { spender, amount };
        Bytes::from(IERC20::IERC20Calls::approve(call).abi_encode())
    }

    /// Calculate expected profit from liquidation.
    fn calculate_profit(&self, data: &LiquidationData, gas_cost: U256) -> U256 {
        // Profit = collateral_value - debt_amount - gas_cost
        // Collateral includes liquidation bonus

        // For simplicity, assume collateral_amount is already including bonus
        // In production, you'd convert both to a common denomination (e.g., ETH or USD)

        if data.collateral_amount > data.debt_amount + gas_cost {
            data.collateral_amount - data.debt_amount - gas_cost
        } else {
            U256::ZERO
        }
    }

    /// Validate that the position is actually liquidatable.
    fn validate_liquidatable(&self, data: &LiquidationData) -> Result<()> {
        // Health factor < 1e18 means position is liquidatable
        let one_e18 = U256::from(1_000_000_000_000_000_000u128);

        if data.health_factor >= one_e18 {
            return Err(MevError::Execution(ExecutionError::SubmissionFailed(
                format!(
                    "Position not liquidatable: health factor {} >= 1e18",
                    data.health_factor
                ),
            )));
        }

        Ok(())
    }

    /// Execute direct liquidation (using own capital).
    async fn execute_direct_liquidation<P>(
        &self,
        data: &LiquidationData,
        ctx: &ExecutorContext<P>,
    ) -> Result<ExecutionResult>
    where
        P: AlloyProvider + Clone + Send + Sync + 'static,
    {
        let start = std::time::Instant::now();

        let pool_address = data.protocol.pool_address(ctx.config.chain_id).ok_or_else(|| {
            MevError::Execution(ExecutionError::SubmissionFailed(
                "Protocol not deployed on this chain".to_string(),
            ))
        })?;

        let tx_builder = TxBuilder::new(
            Arc::clone(&ctx.provider),
            Arc::clone(&ctx.signer),
            ctx.config.chain_id,
        );

        // Build approval transaction for debt asset
        let approval_calldata = self.build_approval_call(pool_address, data.debt_amount);
        let approval_tx = tx_builder
            .build_raw_tx(data.debt_asset, U256::ZERO, approval_calldata, None)
            .await?;

        // Build liquidation transaction
        let liquidation_calldata = match data.protocol {
            LendingProtocol::AaveV2 | LendingProtocol::AaveV3 => {
                self.build_aave_liquidation_call(data)
            }
            LendingProtocol::CompoundV2 => self.build_compound_v2_liquidation_call(data)?,
            LendingProtocol::CompoundV3 => {
                self.build_compound_v3_liquidation_call(data, ctx.signer.address())
            }
            LendingProtocol::Morpho => self.build_morpho_liquidation_call(data),
        };

        let liquidation_tx = tx_builder
            .build_raw_tx(pool_address, U256::ZERO, liquidation_calldata, None)
            .await?;

        // Sign transactions
        let signed_approval = tx_builder.sign_tx(&approval_tx).await?;
        let signed_liquidation = tx_builder.sign_tx(&liquidation_tx).await?;

        // Get current block number
        let block_number = ctx
            .provider
            .get_block_number()
            .await
            .map_err(|e| MevError::Execution(ExecutionError::SubmissionFailed(e.to_string())))?;

        // Create Flashbots bundle
        let bundle = FlashbotsBundle::new(
            vec![signed_approval, signed_liquidation],
            block_number + 1,
        )
        .with_revert_on_fail(true);

        // Simulate bundle
        let sim_result = ctx.flashbots.simulate_bundle(bundle.clone()).await?;

        if !sim_result.success {
            error!(
                error = ?sim_result.error,
                "Liquidation bundle simulation failed"
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

        // Calculate profit
        let gas_cost = U256::from(sim_result.total_gas_used) * sim_result.gas_price;
        let expected_profit = self.calculate_profit(data, gas_cost);

        if expected_profit < ctx.config.min_profit_wei {
            warn!(
                expected_profit = %expected_profit,
                min_required = %ctx.config.min_profit_wei,
                "Liquidation not profitable enough"
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
        let response = ctx.flashbots.send_bundle(bundle).await?;

        info!(
            bundle_hash = %response.bundle_hash,
            borrower = %data.borrower,
            protocol = ?data.protocol,
            profit = %expected_profit,
            latency_ms = start.elapsed().as_millis(),
            "Liquidation execution complete"
        );

        Ok(ExecutionResult {
            success: true,
            tx_hash: None,
            bundle_hash: Some(response.bundle_hash),
            block_number: None,
            actual_profit: Some(expected_profit),
            gas_used: Some(sim_result.total_gas_used),
            gas_price: Some(sim_result.gas_price),
            error: None,
            latency_ms: start.elapsed().as_millis() as u64,
        })
    }

    /// Execute flashloan-assisted liquidation (for large positions).
    async fn execute_flashloan_liquidation<P>(
        &self,
        data: &LiquidationData,
        ctx: &ExecutorContext<P>,
    ) -> Result<ExecutionResult>
    where
        P: AlloyProvider + Clone + Send + Sync + 'static,
    {
        let start = std::time::Instant::now();

        let flashloan_contract = self.config.flashloan_contract.ok_or_else(|| {
            MevError::Execution(ExecutionError::SubmissionFailed(
                "Flashloan contract not configured".to_string(),
            ))
        })?;

        info!(
            borrower = %data.borrower,
            debt_amount = %data.debt_amount,
            "Executing flashloan liquidation"
        );

        // For flashloan liquidation, we call our custom contract that:
        // 1. Takes flashloan for debt amount
        // 2. Approves and calls liquidation
        // 3. Sells collateral if needed
        // 4. Repays flashloan
        // 5. Keeps profit

        // This is a simplified implementation; production would use actual flashloan contract
        let tx_builder = TxBuilder::new(
            Arc::clone(&ctx.provider),
            Arc::clone(&ctx.signer),
            ctx.config.chain_id,
        );

        // Encode flashloan liquidation params
        let params = encode_flashloan_liquidation_params(data);

        let tx = tx_builder
            .build_raw_tx(flashloan_contract, U256::ZERO, params, Some(800_000))
            .await?;

        let signed_tx = tx_builder.sign_tx(&tx).await?;

        // Get current block
        let block_number = ctx
            .provider
            .get_block_number()
            .await
            .map_err(|e| MevError::Execution(ExecutionError::SubmissionFailed(e.to_string())))?;

        // Create bundle
        let bundle = FlashbotsBundle::new(vec![signed_tx], block_number + 1)
            .with_revert_on_fail(true);

        // Simulate
        let sim_result = ctx.flashbots.simulate_bundle(bundle.clone()).await?;

        if !sim_result.success {
            error!(
                error = ?sim_result.error,
                "Flashloan liquidation simulation failed"
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

        // Calculate profit
        let gas_cost = U256::from(sim_result.total_gas_used) * sim_result.gas_price;
        let expected_profit = self.calculate_profit(data, gas_cost);

        if expected_profit < ctx.config.min_profit_wei {
            return Ok(ExecutionResult {
                success: false,
                tx_hash: None,
                bundle_hash: None,
                block_number: None,
                actual_profit: None,
                gas_used: Some(sim_result.total_gas_used),
                gas_price: Some(sim_result.gas_price),
                error: Some("Not profitable after costs".to_string()),
                latency_ms: start.elapsed().as_millis() as u64,
            });
        }

        // Submit bundle
        let response = ctx.flashbots.send_bundle(bundle).await?;

        Ok(ExecutionResult {
            success: true,
            tx_hash: None,
            bundle_hash: Some(response.bundle_hash),
            block_number: None,
            actual_profit: Some(expected_profit),
            gas_used: Some(sim_result.total_gas_used),
            gas_price: Some(sim_result.gas_price),
            error: None,
            latency_ms: start.elapsed().as_millis() as u64,
        })
    }
}

impl Default for LiquidationExecutor {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl<P> Executor<P> for LiquidationExecutor
where
    P: AlloyProvider + Clone + Send + Sync + 'static,
{
    fn name(&self) -> &str {
        &self.name
    }

    fn can_handle(&self, opp: &Opportunity) -> bool {
        matches!(opp.opportunity_type, OpportunityType::Liquidation)
    }

    #[instrument(skip(self, ctx), fields(executor = %self.name, opportunity_id = %opp.id))]
    async fn execute(&self, opp: &Opportunity, ctx: &ExecutorContext<P>) -> Result<ExecutionResult> {
        // Extract liquidation data
        let liq_data = self.extract_liquidation_data(opp)?;

        // Validate position is liquidatable
        self.validate_liquidatable(&liq_data)?;

        info!(
            protocol = ?liq_data.protocol,
            borrower = %liq_data.borrower,
            debt_amount = %liq_data.debt_amount,
            health_factor = %liq_data.health_factor,
            use_flashloan = liq_data.use_flashloan,
            "Executing liquidation"
        );

        // Check if we need flashloan
        if liq_data.use_flashloan {
            self.execute_flashloan_liquidation(&liq_data, ctx).await
        } else {
            self.execute_direct_liquidation(&liq_data, ctx).await
        }
    }

    #[instrument(skip(self, ctx), fields(executor = %self.name))]
    async fn simulate(&self, opp: &Opportunity, ctx: &ExecutorContext<P>) -> Result<SimulationResult> {
        let liq_data = self.extract_liquidation_data(opp)?;

        // Validate liquidatable
        self.validate_liquidatable(&liq_data)?;

        // Estimate gas based on protocol
        let estimated_gas = match liq_data.protocol {
            LendingProtocol::AaveV2 | LendingProtocol::AaveV3 => 350_000u64,
            LendingProtocol::CompoundV2 => 400_000u64,
            LendingProtocol::CompoundV3 => 300_000u64,
            LendingProtocol::Morpho => 350_000u64,
        };

        // Add gas for flashloan if needed
        let total_gas = if liq_data.use_flashloan {
            estimated_gas + 100_000
        } else {
            estimated_gas + 50_000 // Approval
        };

        let gas_price = ctx
            .provider
            .get_gas_price()
            .await
            .map_err(|e| MevError::Execution(ExecutionError::SubmissionFailed(e.to_string())))?;

        let gas_cost = U256::from(total_gas) * U256::from(gas_price);
        let profit = self.calculate_profit(&liq_data, gas_cost);
        let is_profitable = profit >= ctx.config.min_profit_wei;

        debug!(
            estimated_gas = total_gas,
            gas_cost = %gas_cost,
            profit = %profit,
            is_profitable = is_profitable,
            "Liquidation simulation complete"
        );

        Ok(SimulationResult {
            success: true,
            profit,
            gas_used: total_gas,
            state_changes: Vec::new(),
            logs: Vec::new(),
            error: None,
            is_profitable,
        })
    }
}

/// Encode parameters for flashloan liquidation contract.
fn encode_flashloan_liquidation_params(data: &LiquidationData) -> Bytes {
    // This would encode the params for the flashloan liquidation contract
    // Format: protocol_id(1) + borrower(20) + debt_asset(20) + collateral_asset(20) + amount(32)

    let mut params = Vec::new();

    // Protocol ID
    let protocol_id = match data.protocol {
        LendingProtocol::AaveV2 => 0u8,
        LendingProtocol::AaveV3 => 1u8,
        LendingProtocol::CompoundV2 => 2u8,
        LendingProtocol::CompoundV3 => 3u8,
        LendingProtocol::Morpho => 4u8,
    };
    params.push(protocol_id);

    // Borrower address (20 bytes)
    params.extend_from_slice(data.borrower.as_slice());

    // Debt asset (20 bytes)
    params.extend_from_slice(data.debt_asset.as_slice());

    // Collateral asset (20 bytes)
    params.extend_from_slice(data.collateral_asset.as_slice());

    // Debt amount (32 bytes)
    let amount_bytes: [u8; 32] = data.debt_amount.to_be_bytes();
    params.extend_from_slice(&amount_bytes);

    Bytes::from(params)
}

/// Monitor a position's health factor and return when liquidatable.
pub async fn monitor_position_health<P>(
    _provider: &P,
    _protocol: LendingProtocol,
    _user: Address,
    _threshold: U256,
) -> Option<U256>
where
    P: AlloyProvider + Clone + 'static,
{
    // In production, this would call the protocol's health factor function
    // and return when it drops below the threshold

    // Example for Aave V3:
    // provider.call(pool.getUserAccountData(user)).await
    // Extract health factor from result
    // Return if below threshold

    None
}

/// Calculate the maximum profitable liquidation amount.
pub fn calculate_max_liquidation_amount(
    total_debt: U256,
    max_liquidation_fraction: u32, // e.g., 5000 for 50%
    available_capital: U256,
) -> U256 {
    let max_by_protocol = total_debt * U256::from(max_liquidation_fraction) / U256::from(10000);
    max_by_protocol.min(available_capital)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_liquidation_executor_creation() {
        let executor = LiquidationExecutor::new();
        assert_eq!(executor.name(), "liquidation");
    }

    #[test]
    fn test_can_handle() {
        let executor = LiquidationExecutor::new();

        let liq_opp = Opportunity {
            id: "test".to_string(),
            opportunity_type: OpportunityType::Liquidation,
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
            ..liq_opp.clone()
        };

        assert!(executor.can_handle(&liq_opp));
        assert!(!executor.can_handle(&arb_opp));
    }

    #[test]
    fn test_lending_protocol_addresses() {
        // Mainnet Aave V3
        let aave_v3 = LendingProtocol::AaveV3.pool_address(1);
        assert!(aave_v3.is_some());
        assert_eq!(
            aave_v3.unwrap(),
            address!("87870Bca3F3fD6335C3F4ce8392D69350B4fA4E2")
        );

        // Unknown chain
        let unknown = LendingProtocol::AaveV3.pool_address(99999);
        assert!(unknown.is_none());
    }

    #[test]
    fn test_liquidation_bonus() {
        assert_eq!(LendingProtocol::AaveV2.default_liquidation_bonus_bps(), 500);
        assert_eq!(LendingProtocol::CompoundV2.default_liquidation_bonus_bps(), 800);
    }

    #[test]
    fn test_validate_liquidatable() {
        let executor = LiquidationExecutor::new();

        let mut data = LiquidationData {
            protocol: LendingProtocol::AaveV3,
            borrower: Address::ZERO,
            debt_asset: Address::ZERO,
            collateral_asset: Address::repeat_byte(1),
            debt_amount: U256::ZERO,
            collateral_amount: U256::ZERO,
            liquidation_bonus_bps: 500,
            health_factor: U256::from(900_000_000_000_000_000u128), // 0.9e18
            use_flashloan: false,
            ctoken_collateral: None,
        };

        // Should pass with health factor < 1e18
        assert!(executor.validate_liquidatable(&data).is_ok());

        // Should fail with health factor >= 1e18
        data.health_factor = U256::from(1_000_000_000_000_000_000u128);
        assert!(executor.validate_liquidatable(&data).is_err());

        data.health_factor = U256::from(1_100_000_000_000_000_000u128);
        assert!(executor.validate_liquidatable(&data).is_err());
    }

    #[test]
    fn test_calculate_profit() {
        let executor = LiquidationExecutor::new();

        let data = LiquidationData {
            protocol: LendingProtocol::AaveV3,
            borrower: Address::ZERO,
            debt_asset: Address::ZERO,
            collateral_asset: Address::repeat_byte(1),
            debt_amount: U256::from(1_000_000_000_000_000_000u128),    // 1 ETH worth
            collateral_amount: U256::from(1_100_000_000_000_000_000u128), // 1.1 ETH worth (with bonus)
            liquidation_bonus_bps: 500,
            health_factor: U256::from(900_000_000_000_000_000u128),
            use_flashloan: false,
            ctoken_collateral: None,
        };

        let gas_cost = U256::from(50_000_000_000_000_000u128); // 0.05 ETH

        let profit = executor.calculate_profit(&data, gas_cost);
        // Profit = 1.1 - 1.0 - 0.05 = 0.05 ETH
        assert_eq!(profit, U256::from(50_000_000_000_000_000u128));
    }

    #[test]
    fn test_calculate_max_liquidation_amount() {
        let total_debt = U256::from(100_000_000_000_000_000_000u128); // 100 ETH
        let max_fraction = 5000;                                       // 50%
        let available = U256::from(30_000_000_000_000_000_000u128);    // 30 ETH

        let max_amount = calculate_max_liquidation_amount(total_debt, max_fraction, available);

        // Max by protocol = 50 ETH, but we only have 30 ETH
        assert_eq!(max_amount, U256::from(30_000_000_000_000_000_000u128));
    }
}
