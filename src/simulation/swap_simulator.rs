//! Swap simulator - simulates DEX swaps using REVM with forked state.
//!
//! This module provides accurate swap simulation by:
//! 1. Forking mainnet state via RPC (ForkDB)
//! 2. Encoding actual swap calldata for Uniswap V2/V3
//! 3. Executing the swap in REVM
//! 4. Decoding exact output amounts

use alloy::primitives::{Address, Bytes, U256};
use alloy::providers::Provider;
use alloy::sol;
use alloy::sol_types::SolCall;
use alloy::transports::Transport;
use revm::primitives::{
    BlobExcessGasAndPrice, BlockEnv, CfgEnv, EnvWithHandlerCfg,
    ExecutionResult, Output, SpecId, TransactTo, TxEnv,
};
use revm::Evm;
use std::sync::Arc;
use tracing::{debug, info};

use super::fork_db::ForkDB;

// Uniswap V2 Router ABI
sol! {
    #[derive(Debug)]
    interface IUniswapV2Router {
        function getAmountsOut(uint amountIn, address[] calldata path)
            external view returns (uint[] memory amounts);

        function swapExactTokensForTokens(
            uint amountIn,
            uint amountOutMin,
            address[] calldata path,
            address to,
            uint deadline
        ) external returns (uint[] memory amounts);

        function swapExactETHForTokens(
            uint amountOutMin,
            address[] calldata path,
            address to,
            uint deadline
        ) external payable returns (uint[] memory amounts);

        function swapExactTokensForETH(
            uint amountIn,
            uint amountOutMin,
            address[] calldata path,
            address to,
            uint deadline
        ) external returns (uint[] memory amounts);
    }
}

// Uniswap V2 Pair ABI for direct pool queries
sol! {
    #[derive(Debug)]
    interface IUniswapV2Pair {
        function getReserves() external view returns (uint112 reserve0, uint112 reserve1, uint32 blockTimestampLast);
        function token0() external view returns (address);
        function token1() external view returns (address);
    }
}

// Uniswap V3 Quoter ABI
sol! {
    #[derive(Debug)]
    interface IUniswapV3Quoter {
        function quoteExactInputSingle(
            address tokenIn,
            address tokenOut,
            uint24 fee,
            uint256 amountIn,
            uint160 sqrtPriceLimitX96
        ) external returns (uint256 amountOut);

        function quoteExactInput(bytes calldata path, uint256 amountIn)
            external returns (uint256 amountOut);
    }
}

// ERC20 interface for balance checks
sol! {
    #[derive(Debug)]
    interface IERC20 {
        function balanceOf(address account) external view returns (uint256);
        function allowance(address owner, address spender) external view returns (uint256);
    }
}

/// Known contract addresses
pub mod addresses {
    use alloy::primitives::{address, Address};

    pub const UNISWAP_V2_ROUTER: Address = address!("7a250d5630B4cF539739dF2C5dAcb4c659F2488D");
    pub const UNISWAP_V2_FACTORY: Address = address!("5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f");
    pub const UNISWAP_V3_QUOTER: Address = address!("b27308f9F90D607463bb33eA1BeBb41C27CE5AB6");
    pub const UNISWAP_V3_QUOTER_V2: Address = address!("61fFE014bA17989E743c5F6cB21bF9697530B21e");
    pub const SUSHISWAP_ROUTER: Address = address!("d9e1cE17f2641f24aE83637ab66a2cca9C378B9F");
    pub const WETH: Address = address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
}

/// Result of a swap simulation
#[derive(Debug, Clone)]
pub struct SwapSimulationResult {
    /// Whether the simulation succeeded
    pub success: bool,
    /// Amount received from the swap
    pub amount_out: U256,
    /// Gas used by the swap
    pub gas_used: u64,
    /// Error message if failed
    pub error: Option<String>,
}

impl Default for SwapSimulationResult {
    fn default() -> Self {
        Self {
            success: false,
            amount_out: U256::ZERO,
            gas_used: 0,
            error: None,
        }
    }
}

/// Swap simulator using REVM with forked state
pub struct SwapSimulator<T, P>
where
    T: Transport + Clone,
    P: Provider<T>,
{
    /// Provider for RPC access
    provider: Arc<P>,
    /// Current block number
    block_number: u64,
    /// Base fee for gas calculations
    base_fee: U256,
    /// Chain ID
    chain_id: u64,
    /// Phantom
    _transport: std::marker::PhantomData<T>,
}

impl<T, P> SwapSimulator<T, P>
where
    T: Transport + Clone,
    P: Provider<T>,
{
    /// Create a new swap simulator
    pub async fn new(provider: Arc<P>) -> eyre::Result<Self> {
        let block_number = provider.get_block_number().await?;
        let block = provider
            .get_block_by_number(
                alloy::eips::BlockNumberOrTag::Number(block_number),
                alloy::rpc::types::BlockTransactionsKind::Hashes,
            )
            .await?
            .ok_or_else(|| eyre::eyre!("Block not found"))?;

        let base_fee = block
            .header
            .base_fee_per_gas
            .map(U256::from)
            .unwrap_or(U256::from(30_000_000_000u64));

        let chain_id = provider.get_chain_id().await?;

        Ok(Self {
            provider,
            block_number,
            base_fee,
            chain_id,
            _transport: std::marker::PhantomData,
        })
    }

    /// Simulate a Uniswap V2 style swap using getAmountsOut
    pub async fn simulate_v2_swap(
        &self,
        router: Address,
        amount_in: U256,
        path: Vec<Address>,
    ) -> eyre::Result<SwapSimulationResult> {
        if path.len() < 2 {
            return Ok(SwapSimulationResult {
                success: false,
                error: Some("Path must have at least 2 tokens".to_string()),
                ..Default::default()
            });
        }

        debug!(
            router = %router,
            amount_in = %amount_in,
            path_len = path.len(),
            "Simulating V2 swap"
        );

        // Create ForkDB (implements Database, handles its own caching)
        let mut fork_db = ForkDB::new(Arc::clone(&self.provider), self.block_number);

        // Build getAmountsOut call
        let call = IUniswapV2Router::getAmountsOutCall {
            amountIn: amount_in,
            path: path.clone(),
        };
        let calldata = Bytes::from(call.abi_encode());

        let mut block_env = BlockEnv::default();
        block_env.number = U256::from(self.block_number);
        block_env.timestamp = U256::from(chrono::Utc::now().timestamp() as u64);
        block_env.basefee = self.base_fee;
        block_env.gas_limit = U256::from(30_000_000u64);
        block_env.blob_excess_gas_and_price = Some(BlobExcessGasAndPrice::new(0, false));

        let mut cfg_env = CfgEnv::default();
        cfg_env.chain_id = self.chain_id;

        let mut tx_env = TxEnv::default();
        tx_env.caller = Address::ZERO; // View call, no caller needed
        tx_env.transact_to = TransactTo::Call(router);
        tx_env.data = revm::primitives::Bytes::from(calldata.to_vec());
        tx_env.gas_limit = 1_000_000;
        tx_env.gas_price = self.base_fee;

        let env = EnvWithHandlerCfg::new_with_spec_id(
            Box::new(revm::primitives::Env {
                cfg: cfg_env,
                block: block_env,
                tx: tx_env,
            }),
            SpecId::CANCUN,
        );

        let mut evm = Evm::builder()
            .with_db(&mut fork_db)
            .with_env_with_handler_cfg(env)
            .build();

        let result = evm.transact();
        drop(evm);

        match result {
            Ok(result) => match result.result {
                ExecutionResult::Success {
                    output: Output::Call(output),
                    gas_used,
                    ..
                } => {
                    // Decode the amounts array
                    match IUniswapV2Router::getAmountsOutCall::abi_decode_returns(&output, true) {
                        Ok(decoded) => {
                            let amounts = decoded.amounts;
                            let amount_out = amounts.last().copied().unwrap_or(U256::ZERO);

                            info!(
                                amount_in = %amount_in,
                                amount_out = %amount_out,
                                gas_used = gas_used,
                                "V2 swap simulation successful"
                            );

                            Ok(SwapSimulationResult {
                                success: true,
                                amount_out,
                                gas_used,
                                error: None,
                            })
                        }
                        Err(e) => Ok(SwapSimulationResult {
                            success: false,
                            gas_used,
                            error: Some(format!("Failed to decode output: {}", e)),
                            ..Default::default()
                        }),
                    }
                }
                ExecutionResult::Revert { output, gas_used } => {
                    let reason = String::from_utf8_lossy(&output).to_string();
                    Ok(SwapSimulationResult {
                        success: false,
                        gas_used,
                        error: Some(format!("Reverted: {}", reason)),
                        ..Default::default()
                    })
                }
                ExecutionResult::Halt { reason, gas_used } => Ok(SwapSimulationResult {
                    success: false,
                    gas_used,
                    error: Some(format!("Halted: {:?}", reason)),
                    ..Default::default()
                }),
                _ => Ok(SwapSimulationResult {
                    success: false,
                    error: Some("Unexpected execution result".to_string()),
                    ..Default::default()
                }),
            },
            Err(e) => Ok(SwapSimulationResult {
                success: false,
                error: Some(format!("EVM error: {:?}", e)),
                ..Default::default()
            }),
        }
    }

    /// Get Uniswap V2 pool reserves directly
    pub async fn get_v2_reserves(&self, pair: Address) -> eyre::Result<(U256, U256)> {
        let mut fork_db = ForkDB::new(Arc::clone(&self.provider), self.block_number);

        let call = IUniswapV2Pair::getReservesCall {};
        let calldata = Bytes::from(call.abi_encode());

        let mut block_env = BlockEnv::default();
        block_env.number = U256::from(self.block_number);
        block_env.basefee = self.base_fee;
        block_env.blob_excess_gas_and_price = Some(BlobExcessGasAndPrice::new(0, false));

        let mut cfg_env = CfgEnv::default();
        cfg_env.chain_id = self.chain_id;

        let mut tx_env = TxEnv::default();
        tx_env.caller = Address::ZERO;
        tx_env.transact_to = TransactTo::Call(pair);
        tx_env.data = revm::primitives::Bytes::from(calldata.to_vec());
        tx_env.gas_limit = 100_000;
        tx_env.gas_price = self.base_fee;

        let env = EnvWithHandlerCfg::new_with_spec_id(
            Box::new(revm::primitives::Env {
                cfg: cfg_env,
                block: block_env,
                tx: tx_env,
            }),
            SpecId::CANCUN,
        );

        let mut evm = Evm::builder()
            .with_db(&mut fork_db)
            .with_env_with_handler_cfg(env)
            .build();

        let result = evm.transact()?;
        drop(evm);

        match result.result {
            ExecutionResult::Success {
                output: Output::Call(output),
                ..
            } => {
                let decoded = IUniswapV2Pair::getReservesCall::abi_decode_returns(&output, true)?;
                Ok((U256::from(decoded.reserve0), U256::from(decoded.reserve1)))
            }
            _ => Err(eyre::eyre!("Failed to get reserves")),
        }
    }

    /// Simulate V3 swap using quoter
    pub async fn simulate_v3_swap(
        &self,
        token_in: Address,
        token_out: Address,
        fee: u32,
        amount_in: U256,
    ) -> eyre::Result<SwapSimulationResult> {
        use alloy::primitives::Uint;

        debug!(
            token_in = %token_in,
            token_out = %token_out,
            fee = fee,
            amount_in = %amount_in,
            "Simulating V3 swap"
        );

        let mut fork_db = ForkDB::new(Arc::clone(&self.provider), self.block_number);

        // Build quoteExactInputSingle call with proper types
        let fee_uint24: Uint<24, 1> = Uint::from(fee);
        let sqrt_price_limit: Uint<160, 3> = Uint::ZERO;

        let call = IUniswapV3Quoter::quoteExactInputSingleCall {
            tokenIn: token_in,
            tokenOut: token_out,
            fee: fee_uint24,
            amountIn: amount_in,
            sqrtPriceLimitX96: sqrt_price_limit,
        };
        let calldata = Bytes::from(call.abi_encode());

        let mut block_env = BlockEnv::default();
        block_env.number = U256::from(self.block_number);
        block_env.timestamp = U256::from(chrono::Utc::now().timestamp() as u64);
        block_env.basefee = self.base_fee;
        block_env.gas_limit = U256::from(30_000_000u64);
        block_env.blob_excess_gas_and_price = Some(BlobExcessGasAndPrice::new(0, false));

        let mut cfg_env = CfgEnv::default();
        cfg_env.chain_id = self.chain_id;

        let mut tx_env = TxEnv::default();
        tx_env.caller = Address::ZERO;
        tx_env.transact_to = TransactTo::Call(addresses::UNISWAP_V3_QUOTER);
        tx_env.data = revm::primitives::Bytes::from(calldata.to_vec());
        tx_env.gas_limit = 1_000_000;
        tx_env.gas_price = self.base_fee;

        let env = EnvWithHandlerCfg::new_with_spec_id(
            Box::new(revm::primitives::Env {
                cfg: cfg_env,
                block: block_env,
                tx: tx_env,
            }),
            SpecId::CANCUN,
        );

        let mut evm = Evm::builder()
            .with_db(&mut fork_db)
            .with_env_with_handler_cfg(env)
            .build();

        let result = evm.transact();
        drop(evm);

        match result {
            Ok(result) => match result.result {
                ExecutionResult::Success {
                    output: Output::Call(output),
                    gas_used,
                    ..
                } => {
                    // V3 quoter returns just the amountOut as uint256
                    if output.len() >= 32 {
                        let amount_out = U256::from_be_slice(&output[..32]);

                        info!(
                            amount_in = %amount_in,
                            amount_out = %amount_out,
                            gas_used = gas_used,
                            "V3 swap simulation successful"
                        );

                        Ok(SwapSimulationResult {
                            success: true,
                            amount_out,
                            gas_used,
                            error: None,
                        })
                    } else {
                        Ok(SwapSimulationResult {
                            success: false,
                            gas_used,
                            error: Some("Invalid output length".to_string()),
                            ..Default::default()
                        })
                    }
                }
                ExecutionResult::Revert { output, gas_used } => {
                    let reason = String::from_utf8_lossy(&output).to_string();
                    Ok(SwapSimulationResult {
                        success: false,
                        gas_used,
                        error: Some(format!("Reverted: {}", reason)),
                        ..Default::default()
                    })
                }
                ExecutionResult::Halt { reason, gas_used } => Ok(SwapSimulationResult {
                    success: false,
                    gas_used,
                    error: Some(format!("Halted: {:?}", reason)),
                    ..Default::default()
                }),
                _ => Ok(SwapSimulationResult {
                    success: false,
                    error: Some("Unexpected result".to_string()),
                    ..Default::default()
                }),
            },
            Err(e) => Ok(SwapSimulationResult {
                success: false,
                error: Some(format!("EVM error: {:?}", e)),
                ..Default::default()
            }),
        }
    }

    /// Calculate V2 swap output using constant product formula (for quick estimates)
    pub fn calculate_v2_output(
        amount_in: U256,
        reserve_in: U256,
        reserve_out: U256,
        fee_bps: u32,
    ) -> U256 {
        if reserve_in == U256::ZERO || reserve_out == U256::ZERO {
            return U256::ZERO;
        }

        // Uniswap V2 uses 0.3% fee (30 bps), amount_in_with_fee = amount_in * (10000 - fee) / 10000
        let fee_factor = 10000 - fee_bps;
        let amount_in_with_fee = amount_in * U256::from(fee_factor);
        let numerator = amount_in_with_fee * reserve_out;
        let denominator = reserve_in * U256::from(10000) + amount_in_with_fee;

        if denominator == U256::ZERO {
            return U256::ZERO;
        }

        numerator / denominator
    }

    /// Calculate price impact of a swap
    pub fn calculate_price_impact(
        amount_in: U256,
        reserve_in: U256,
        reserve_out: U256,
    ) -> f64 {
        if reserve_in == U256::ZERO || reserve_out == U256::ZERO {
            return 1.0; // 100% impact
        }

        // Spot price before swap
        let spot_price = reserve_out.to::<u128>() as f64 / reserve_in.to::<u128>() as f64;

        // Effective price after swap
        let amount_out = Self::calculate_v2_output(amount_in, reserve_in, reserve_out, 30);
        if amount_out == U256::ZERO {
            return 1.0;
        }

        let effective_price = amount_out.to::<u128>() as f64 / amount_in.to::<u128>() as f64;

        // Price impact = 1 - (effective_price / spot_price)
        1.0 - (effective_price / spot_price)
    }

    /// Update simulator to latest block
    pub async fn refresh(&mut self) -> eyre::Result<()> {
        self.block_number = self.provider.get_block_number().await?;
        let block = self
            .provider
            .get_block_by_number(
                alloy::eips::BlockNumberOrTag::Number(self.block_number),
                alloy::rpc::types::BlockTransactionsKind::Hashes,
            )
            .await?
            .ok_or_else(|| eyre::eyre!("Block not found"))?;

        self.base_fee = block
            .header
            .base_fee_per_gas
            .map(U256::from)
            .unwrap_or(U256::from(30_000_000_000u64));

        debug!(
            block = self.block_number,
            base_fee = %self.base_fee,
            "Refreshed swap simulator"
        );

        Ok(())
    }

    /// Get current block number
    pub fn block_number(&self) -> u64 {
        self.block_number
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_v2_output_calculation() {
        // Test with 1 ETH in, reserves of 1000 ETH and 2,000,000 USDC
        let amount_in = U256::from(1_000_000_000_000_000_000u128); // 1 ETH
        let reserve_in = U256::from(1000_000_000_000_000_000_000u128); // 1000 ETH
        let reserve_out = U256::from(2_000_000_000_000u128); // 2M USDC (6 decimals)

        let output = SwapSimulator::<(), ()>::calculate_v2_output(
            amount_in,
            reserve_in,
            reserve_out,
            30, // 0.3% fee
        );

        // Expected: roughly 1994 USDC (accounting for 0.3% fee and slippage)
        assert!(output > U256::from(1990_000_000u128)); // > 1990 USDC
        assert!(output < U256::from(2000_000_000u128)); // < 2000 USDC
    }

    #[test]
    fn test_price_impact() {
        let amount_in = U256::from(10_000_000_000_000_000_000u128); // 10 ETH
        let reserve_in = U256::from(100_000_000_000_000_000_000u128); // 100 ETH
        let reserve_out = U256::from(200_000_000_000u128); // 200k USDC

        let impact = SwapSimulator::<(), ()>::calculate_price_impact(
            amount_in,
            reserve_in,
            reserve_out,
        );

        // 10 ETH into 100 ETH pool should have ~9% price impact
        assert!(impact > 0.08);
        assert!(impact < 0.12);
    }
}
