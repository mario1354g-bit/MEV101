//! Warm Simulator - High-accuracy MEV simulation using WarmCache + REVM.
//!
//! This module provides the integration layer between WarmCache and REVM,
//! enabling sub-millisecond simulations with accurate state.
//!
//! Key features:
//! - Uses pre-warmed state from WarmCache (no RPC latency)
//! - Updates block environment every new block
//! - Supports mempool-aware simulation (applying pending txs first)
//! - Checkpointing for fast rollback

use alloy::primitives::{Address, Bytes, U256};
use alloy::providers::Provider;
use alloy::sol;
use alloy::sol_types::SolCall;
use alloy::transports::Transport;
use revm::primitives::{
    BlobExcessGasAndPrice, BlockEnv, CfgEnv,
    EnvWithHandlerCfg, ExecutionResult, Output, SpecId, TransactTo, TxEnv,
};
use revm::Evm;
use std::sync::Arc;
use tracing::{debug, info, trace};

use super::warm_cache::WarmCache;

// Uniswap V2 Router interface for swap simulation
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
    }
}

// Uniswap V3 Quoter interface
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
    }
}

/// Result of a warm simulation
#[derive(Debug, Clone)]
pub struct WarmSimulationResult {
    /// Whether the simulation succeeded
    pub success: bool,
    /// Amount output from the swap/trade
    pub amount_out: U256,
    /// Gas used
    pub gas_used: u64,
    /// Error message if failed
    pub error: Option<String>,
    /// Block number used for simulation
    pub block_number: u64,
}

impl Default for WarmSimulationResult {
    fn default() -> Self {
        Self {
            success: false,
            amount_out: U256::ZERO,
            gas_used: 0,
            error: None,
            block_number: 0,
        }
    }
}

/// Warm Simulator - uses WarmCache for state and REVM for execution.
pub struct WarmSimulator<T, P>
where
    T: Transport + Clone,
    P: Provider<T>,
{
    /// The warm cache containing pre-loaded state
    cache: Arc<WarmCache<T, P>>,
    /// Current block environment
    block_env: BlockEnv,
    /// Configuration environment
    cfg_env: CfgEnv,
}

impl<T, P> WarmSimulator<T, P>
where
    T: Transport + Clone,
    P: Provider<T>,
{
    /// Create a new warm simulator from a warm cache.
    pub fn new(cache: Arc<WarmCache<T, P>>) -> Self {
        let block_state = cache.block_state();
        let chain_id = cache.chain_id();

        let mut block_env = BlockEnv::default();
        block_env.number = U256::from(block_state.number);
        block_env.timestamp = U256::from(block_state.timestamp);
        block_env.basefee = block_state.base_fee;
        block_env.gas_limit = U256::from(30_000_000u64);
        block_env.coinbase = Address::ZERO;
        block_env.difficulty = U256::ZERO;
        block_env.prevrandao = Some(alloy::primitives::B256::ZERO);
        block_env.blob_excess_gas_and_price = Some(BlobExcessGasAndPrice::new(0, false));

        let mut cfg_env = CfgEnv::default();
        cfg_env.chain_id = chain_id;

        Self {
            cache,
            block_env,
            cfg_env,
        }
    }

    /// Update the block environment (call this every new block).
    pub fn update_block_env(&mut self, number: u64, timestamp: u64, base_fee: U256) {
        self.block_env.number = U256::from(number);
        self.block_env.timestamp = U256::from(timestamp);
        self.block_env.basefee = base_fee;

        debug!(
            block = number,
            timestamp = timestamp,
            base_fee = %base_fee,
            "Simulator block env updated"
        );
    }

    /// Sync block environment from the cache's current state.
    pub fn sync_block_env(&mut self) {
        let state = self.cache.block_state();
        self.update_block_env(state.number, state.timestamp, state.base_fee);
    }

    /// Get current block number.
    pub fn block_number(&self) -> u64 {
        self.block_env.number.try_into().unwrap_or(0)
    }

    /// Simulate a V2 swap using getAmountsOut.
    pub fn simulate_v2_swap(
        &self,
        router: Address,
        amount_in: U256,
        path: Vec<Address>,
    ) -> WarmSimulationResult {
        if path.len() < 2 {
            return WarmSimulationResult {
                success: false,
                error: Some("Path must have at least 2 tokens".to_string()),
                ..Default::default()
            };
        }

        trace!(
            router = %router,
            amount_in = %amount_in,
            path_len = path.len(),
            "Simulating V2 swap with warm cache"
        );

        // Build getAmountsOut call
        let call = IUniswapV2Router::getAmountsOutCall {
            amountIn: amount_in,
            path: path.clone(),
        };
        let calldata = Bytes::from(call.abi_encode());

        // Execute against warm cache
        match self.execute_call(router, calldata, U256::ZERO) {
            Ok((output, gas_used)) => {
                // Decode the amounts array
                match IUniswapV2Router::getAmountsOutCall::abi_decode_returns(&output, true) {
                    Ok(decoded) => {
                        let amounts = decoded.amounts;
                        let amount_out = amounts.last().copied().unwrap_or(U256::ZERO);

                        info!(
                            amount_in = %amount_in,
                            amount_out = %amount_out,
                            gas_used = gas_used,
                            "Warm V2 simulation successful"
                        );

                        WarmSimulationResult {
                            success: true,
                            amount_out,
                            gas_used,
                            error: None,
                            block_number: self.block_number(),
                        }
                    }
                    Err(e) => WarmSimulationResult {
                        success: false,
                        gas_used,
                        error: Some(format!("Failed to decode output: {}", e)),
                        block_number: self.block_number(),
                        ..Default::default()
                    },
                }
            }
            Err(e) => WarmSimulationResult {
                success: false,
                error: Some(e),
                block_number: self.block_number(),
                ..Default::default()
            },
        }
    }

    /// Simulate a V3 swap using quoter.
    pub fn simulate_v3_swap(
        &self,
        quoter: Address,
        token_in: Address,
        token_out: Address,
        fee: u32,
        amount_in: U256,
    ) -> WarmSimulationResult {
        use alloy::primitives::Uint;

        trace!(
            token_in = %token_in,
            token_out = %token_out,
            fee = fee,
            amount_in = %amount_in,
            "Simulating V3 swap with warm cache"
        );

        // Build quoteExactInputSingle call
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

        match self.execute_call(quoter, calldata, U256::ZERO) {
            Ok((output, gas_used)) => {
                if output.len() >= 32 {
                    let amount_out = U256::from_be_slice(&output[..32]);

                    info!(
                        amount_in = %amount_in,
                        amount_out = %amount_out,
                        gas_used = gas_used,
                        "Warm V3 simulation successful"
                    );

                    WarmSimulationResult {
                        success: true,
                        amount_out,
                        gas_used,
                        error: None,
                        block_number: self.block_number(),
                    }
                } else {
                    WarmSimulationResult {
                        success: false,
                        gas_used,
                        error: Some("Invalid output length".to_string()),
                        block_number: self.block_number(),
                        ..Default::default()
                    }
                }
            }
            Err(e) => WarmSimulationResult {
                success: false,
                error: Some(e),
                block_number: self.block_number(),
                ..Default::default()
            },
        }
    }

    /// Execute an arbitrary call against the warm cache.
    pub fn execute_call(
        &self,
        to: Address,
        data: Bytes,
        value: U256,
    ) -> Result<(Vec<u8>, u64), String> {
        // Clone the cache for this simulation (we don't want to mutate shared state)
        let mut db = self.cache.as_ref().clone();

        let mut tx_env = TxEnv::default();
        tx_env.caller = Address::ZERO; // View call
        tx_env.transact_to = TransactTo::Call(to);
        tx_env.data = revm::primitives::Bytes::from(data.to_vec());
        tx_env.value = value;
        tx_env.gas_limit = 1_000_000;
        tx_env.gas_price = self.block_env.basefee;

        let env = EnvWithHandlerCfg::new_with_spec_id(
            Box::new(revm::primitives::Env {
                cfg: self.cfg_env.clone(),
                block: self.block_env.clone(),
                tx: tx_env,
            }),
            SpecId::CANCUN,
        );

        let mut evm = Evm::builder()
            .with_db(&mut db)
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
                } => Ok((output.to_vec(), gas_used)),
                ExecutionResult::Revert { output, gas_used } => {
                    let reason = String::from_utf8_lossy(&output).to_string();
                    Err(format!("Reverted (gas: {}): {}", gas_used, reason))
                }
                ExecutionResult::Halt { reason, gas_used } => {
                    Err(format!("Halted (gas: {}): {:?}", gas_used, reason))
                }
                _ => Err("Unexpected execution result".to_string()),
            },
            Err(e) => Err(format!("EVM error: {:?}", e)),
        }
    }

    /// Simulate a transaction with state modification (for bundle simulation).
    ///
    /// This creates a temporary fork, applies the transaction, and returns the result.
    /// State changes are NOT persisted to the shared cache.
    pub fn simulate_tx_with_state(
        &self,
        from: Address,
        to: Address,
        data: Bytes,
        value: U256,
        gas_limit: u64,
    ) -> Result<(Vec<u8>, u64, bool), String> {
        let mut db = self.cache.as_ref().clone();

        let mut tx_env = TxEnv::default();
        tx_env.caller = from;
        tx_env.transact_to = TransactTo::Call(to);
        tx_env.data = revm::primitives::Bytes::from(data.to_vec());
        tx_env.value = value;
        tx_env.gas_limit = gas_limit;
        tx_env.gas_price = self.block_env.basefee;

        let env = EnvWithHandlerCfg::new_with_spec_id(
            Box::new(revm::primitives::Env {
                cfg: self.cfg_env.clone(),
                block: self.block_env.clone(),
                tx: tx_env,
            }),
            SpecId::CANCUN,
        );

        let mut evm = Evm::builder()
            .with_db(&mut db)
            .with_env_with_handler_cfg(env)
            .build();

        // Use transact_commit to modify state
        let result = evm.transact_commit();
        drop(evm);

        match result {
            Ok(result) => match result {
                ExecutionResult::Success {
                    output: Output::Call(output),
                    gas_used,
                    ..
                } => Ok((output.to_vec(), gas_used, true)),
                ExecutionResult::Revert { output, gas_used } => {
                    Ok((output.to_vec(), gas_used, false))
                }
                ExecutionResult::Halt { gas_used, .. } => {
                    Ok((Vec::new(), gas_used, false))
                }
                _ => Err("Unexpected execution result".to_string()),
            },
            Err(e) => Err(format!("EVM error: {:?}", e)),
        }
    }

    /// Get the underlying cache reference.
    pub fn cache(&self) -> &Arc<WarmCache<T, P>> {
        &self.cache
    }

    /// Calculate V2 output using constant product formula (quick estimate).
    ///
    /// Use this for initial filtering before running full simulation.
    pub fn estimate_v2_output(
        amount_in: U256,
        reserve_in: U256,
        reserve_out: U256,
        fee_bps: u32,
    ) -> U256 {
        if reserve_in == U256::ZERO || reserve_out == U256::ZERO {
            return U256::ZERO;
        }

        // amount_out = (amount_in * (10000 - fee) * reserve_out) / (reserve_in * 10000 + amount_in * (10000 - fee))
        let fee_factor = 10000 - fee_bps;
        let amount_in_with_fee = amount_in * U256::from(fee_factor);
        let numerator = amount_in_with_fee * reserve_out;
        let denominator = reserve_in * U256::from(10000) + amount_in_with_fee;

        if denominator == U256::ZERO {
            return U256::ZERO;
        }

        numerator / denominator
    }

    /// Calculate price impact as a percentage (0.0 to 1.0).
    pub fn calculate_price_impact(
        amount_in: U256,
        reserve_in: U256,
        reserve_out: U256,
    ) -> f64 {
        if reserve_in == U256::ZERO || reserve_out == U256::ZERO {
            return 1.0; // 100% impact
        }

        let spot_price = reserve_out.to::<u128>() as f64 / reserve_in.to::<u128>() as f64;
        let amount_out = Self::estimate_v2_output(amount_in, reserve_in, reserve_out, 30);

        if amount_out == U256::ZERO {
            return 1.0;
        }

        let effective_price = amount_out.to::<u128>() as f64 / amount_in.to::<u128>() as f64;
        1.0 - (effective_price / spot_price)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_v2_output_estimation() {
        // 1 ETH into pool with 1000 ETH / 2M USDC
        let amount_in = U256::from(1_000_000_000_000_000_000u128); // 1 ETH
        let reserve_in = U256::from(1000_000_000_000_000_000_000u128); // 1000 ETH
        let reserve_out = U256::from(2_000_000_000_000u128); // 2M USDC (6 decimals)

        let output = WarmSimulator::<(), ()>::estimate_v2_output(
            amount_in,
            reserve_in,
            reserve_out,
            30, // 0.3% fee
        );

        // Should get roughly 1994 USDC (minus fee and slippage)
        assert!(output > U256::from(1990_000_000u128));
        assert!(output < U256::from(2000_000_000u128));
    }

    #[test]
    fn test_price_impact() {
        let amount_in = U256::from(10_000_000_000_000_000_000u128); // 10 ETH
        let reserve_in = U256::from(100_000_000_000_000_000_000u128); // 100 ETH
        let reserve_out = U256::from(200_000_000_000u128); // 200k USDC

        let impact = WarmSimulator::<(), ()>::calculate_price_impact(
            amount_in,
            reserve_in,
            reserve_out,
        );

        // 10% of pool should have ~9% impact
        assert!(impact > 0.08);
        assert!(impact < 0.12);
    }
}
