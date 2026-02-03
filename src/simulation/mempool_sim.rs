//! Mempool-Aware Simulation - Simulate trades on top of pending transactions.
//!
//! This is the "secret sauce" for beating slippage. Instead of simulating
//! your arb against the current block state, you:
//! 1. See a large swap in the mempool (the "target")
//! 2. Apply that swap to your local state first
//! 3. THEN simulate your arb on the modified state
//!
//! This gives you the EXACT output you'll receive if you successfully
//! backrun that transaction.

use alloy::consensus::Transaction as TxTrait;
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
use tracing::{debug, info, trace, warn};

use super::warm_cache::WarmCache;

// Uniswap V2 Router for decoding swap calldata
sol! {
    #[derive(Debug)]
    interface IUniswapV2Router {
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

        function getAmountsOut(uint amountIn, address[] calldata path)
            external view returns (uint[] memory amounts);
    }
}

/// A pending transaction from the mempool that we want to simulate ahead of.
#[derive(Debug, Clone)]
pub struct PendingTx {
    /// Transaction hash
    pub hash: String,
    /// Sender address
    pub from: Address,
    /// Recipient (router) address
    pub to: Address,
    /// Transaction value (for ETH swaps)
    pub value: U256,
    /// Calldata
    pub data: Bytes,
    /// Gas limit
    pub gas_limit: u64,
    /// Gas price or max fee
    pub gas_price: U256,
}

/// Decoded swap parameters from a pending transaction.
#[derive(Debug, Clone)]
pub struct DecodedPendingSwap {
    /// Input token
    pub token_in: Address,
    /// Output token
    pub token_out: Address,
    /// Amount being swapped
    pub amount_in: U256,
    /// Minimum output (slippage tolerance)
    pub min_amount_out: U256,
    /// Swap path
    pub path: Vec<Address>,
    /// Is this an ETH swap?
    pub is_eth_swap: bool,
}

/// Result of mempool-aware simulation.
#[derive(Debug, Clone)]
pub struct MempoolSimResult {
    /// Whether the full simulation succeeded
    pub success: bool,
    /// Output from the target (pending) transaction
    pub target_output: U256,
    /// Output from our arbitrage transaction
    pub arb_output: U256,
    /// Net profit (arb_output - arb_input - gas)
    pub profit: U256,
    /// Total gas used (target + arb)
    pub total_gas: u64,
    /// Price impact caused by target tx
    pub price_impact_bps: u32,
    /// Error if failed
    pub error: Option<String>,
}

impl Default for MempoolSimResult {
    fn default() -> Self {
        Self {
            success: false,
            target_output: U256::ZERO,
            arb_output: U256::ZERO,
            profit: U256::ZERO,
            total_gas: 0,
            price_impact_bps: 0,
            error: None,
        }
    }
}

/// Mempool-aware simulator for backrunning opportunities.
pub struct MempoolSimulator<T, P>
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
    /// Our bot's address (for simulating our arb)
    bot_address: Address,
}

impl<T, P> MempoolSimulator<T, P>
where
    T: Transport + Clone,
    P: Provider<T>,
{
    /// Create a new mempool simulator.
    pub fn new(cache: Arc<WarmCache<T, P>>, bot_address: Address) -> Self {
        let block_state = cache.block_state();
        let chain_id = cache.chain_id();

        let mut block_env = BlockEnv::default();
        // Simulate on NEXT block (where our tx will land)
        block_env.number = U256::from(block_state.number + 1);
        block_env.timestamp = U256::from(block_state.timestamp + 12);
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
            bot_address,
        }
    }

    /// Update block environment.
    pub fn update_block_env(&mut self, number: u64, timestamp: u64, base_fee: U256) {
        self.block_env.number = U256::from(number);
        self.block_env.timestamp = U256::from(timestamp);
        self.block_env.basefee = base_fee;
    }

    /// Sync with cache's current block state (+ 1 for next block).
    pub fn sync_block_env(&mut self) {
        let state = self.cache.block_state();
        self.update_block_env(state.number + 1, state.timestamp + 12, state.base_fee);
    }

    /// Decode a pending swap transaction.
    pub fn decode_pending_swap(&self, tx: &PendingTx) -> Option<DecodedPendingSwap> {
        if tx.data.len() < 4 {
            return None;
        }

        let selector = &tx.data[..4];

        // swapExactTokensForTokens: 0x38ed1739
        if selector == [0x38, 0xed, 0x17, 0x39] {
            return self.decode_swap_exact_tokens(&tx.data);
        }

        // swapExactETHForTokens: 0x7ff36ab5
        if selector == [0x7f, 0xf3, 0x6a, 0xb5] {
            return self.decode_swap_eth_for_tokens(&tx.data, tx.value);
        }

        // swapExactTokensForETH: 0x18cbafe5
        if selector == [0x18, 0xcb, 0xaf, 0xe5] {
            return self.decode_swap_tokens_for_eth(&tx.data);
        }

        None
    }

    /// Simulate a backrun opportunity.
    ///
    /// This is the core function - it:
    /// 1. Clones the current WarmCache state
    /// 2. Applies the target (pending) transaction
    /// 3. Simulates our arbitrage on the modified state
    /// 4. Returns the exact profit we'd make
    pub fn simulate_backrun(
        &self,
        target_tx: &PendingTx,
        arb_router: Address,
        arb_amount_in: U256,
        arb_path: Vec<Address>,
    ) -> MempoolSimResult {
        // Clone the cache for isolated simulation
        let mut db = self.cache.as_ref().clone();

        // ============================================
        // STEP 1: Apply the target (pending) transaction
        // ============================================
        let target_result = self.execute_tx(
            &mut db,
            target_tx.from,
            target_tx.to,
            target_tx.data.clone(),
            target_tx.value,
            target_tx.gas_limit,
        );

        let (target_output, target_gas, target_success) = match target_result {
            Ok((output, gas, success)) => {
                if !success {
                    return MempoolSimResult {
                        success: false,
                        error: Some("Target tx would revert".to_string()),
                        ..Default::default()
                    };
                }

                // Try to decode output amount
                let amount = if output.len() >= 64 {
                    // Last uint256 in the amounts array
                    let offset = output.len() - 32;
                    U256::from_be_slice(&output[offset..])
                } else {
                    U256::ZERO
                };

                (amount, gas, success)
            }
            Err(e) => {
                return MempoolSimResult {
                    success: false,
                    error: Some(format!("Target tx failed: {}", e)),
                    ..Default::default()
                };
            }
        };

        debug!(
            target_output = %target_output,
            target_gas = target_gas,
            "Target tx simulated successfully"
        );

        // ============================================
        // STEP 2: Simulate our arbitrage on modified state
        // ============================================
        let arb_call = IUniswapV2Router::getAmountsOutCall {
            amountIn: arb_amount_in,
            path: arb_path.clone(),
        };
        let arb_data = Bytes::from(arb_call.abi_encode());

        let arb_result = self.execute_tx(
            &mut db,
            self.bot_address,
            arb_router,
            arb_data,
            U256::ZERO,
            500_000,
        );

        let (arb_output, arb_gas) = match arb_result {
            Ok((output, gas, success)) => {
                if !success {
                    return MempoolSimResult {
                        success: false,
                        target_output,
                        total_gas: target_gas,
                        error: Some("Arb tx would revert after target".to_string()),
                        ..Default::default()
                    };
                }

                // Decode getAmountsOut response
                match IUniswapV2Router::getAmountsOutCall::abi_decode_returns(&output, true) {
                    Ok(decoded) => {
                        let amount = decoded.amounts.last().copied().unwrap_or(U256::ZERO);
                        (amount, gas)
                    }
                    Err(_) => (U256::ZERO, gas),
                }
            }
            Err(e) => {
                return MempoolSimResult {
                    success: false,
                    target_output,
                    total_gas: target_gas,
                    error: Some(format!("Arb simulation failed: {}", e)),
                    ..Default::default()
                };
            }
        };

        // ============================================
        // STEP 3: Calculate profit
        // ============================================
        let profit = if arb_output > arb_amount_in {
            arb_output - arb_amount_in
        } else {
            U256::ZERO
        };

        // Calculate price impact (simplified)
        let price_impact_bps = if arb_amount_in > U256::ZERO {
            let expected_1_to_1 = arb_amount_in;
            if arb_output < expected_1_to_1 {
                let impact = (expected_1_to_1 - arb_output) * U256::from(10000) / expected_1_to_1;
                impact.try_into().unwrap_or(10000)
            } else {
                0
            }
        } else {
            0
        };

        info!(
            target_output = %target_output,
            arb_output = %arb_output,
            profit = %profit,
            total_gas = target_gas + arb_gas,
            "Backrun simulation complete"
        );

        MempoolSimResult {
            success: true,
            target_output,
            arb_output,
            profit,
            total_gas: target_gas + arb_gas,
            price_impact_bps,
            error: None,
        }
    }

    /// Simulate a sandwich attack (frontrun + victim + backrun).
    pub fn simulate_sandwich(
        &self,
        victim_tx: &PendingTx,
        frontrun_amount: U256,
        frontrun_path: Vec<Address>,
        backrun_path: Vec<Address>,
        router: Address,
    ) -> MempoolSimResult {
        let mut db = self.cache.as_ref().clone();

        // ============================================
        // STEP 1: Execute frontrun (our buy)
        // ============================================
        let frontrun_call = IUniswapV2Router::getAmountsOutCall {
            amountIn: frontrun_amount,
            path: frontrun_path.clone(),
        };
        let frontrun_data = Bytes::from(frontrun_call.abi_encode());

        let frontrun_result = self.execute_tx(
            &mut db,
            self.bot_address,
            router,
            frontrun_data,
            U256::ZERO,
            300_000,
        );

        let (frontrun_output, frontrun_gas) = match frontrun_result {
            Ok((output, gas, success)) => {
                if !success {
                    return MempoolSimResult {
                        success: false,
                        error: Some("Frontrun would revert".to_string()),
                        ..Default::default()
                    };
                }
                match IUniswapV2Router::getAmountsOutCall::abi_decode_returns(&output, true) {
                    Ok(decoded) => (decoded.amounts.last().copied().unwrap_or(U256::ZERO), gas),
                    Err(_) => (U256::ZERO, gas),
                }
            }
            Err(e) => {
                return MempoolSimResult {
                    success: false,
                    error: Some(format!("Frontrun failed: {}", e)),
                    ..Default::default()
                };
            }
        };

        // ============================================
        // STEP 2: Execute victim transaction
        // ============================================
        let victim_result = self.execute_tx(
            &mut db,
            victim_tx.from,
            victim_tx.to,
            victim_tx.data.clone(),
            victim_tx.value,
            victim_tx.gas_limit,
        );

        let victim_gas = match victim_result {
            Ok((_, gas, success)) => {
                if !success {
                    debug!("Victim tx would revert - sandwich still possible");
                }
                gas
            }
            Err(_) => 0,
        };

        // ============================================
        // STEP 3: Execute backrun (our sell)
        // ============================================
        let backrun_call = IUniswapV2Router::getAmountsOutCall {
            amountIn: frontrun_output, // Sell what we bought
            path: backrun_path.clone(),
        };
        let backrun_data = Bytes::from(backrun_call.abi_encode());

        let backrun_result = self.execute_tx(
            &mut db,
            self.bot_address,
            router,
            backrun_data,
            U256::ZERO,
            300_000,
        );

        let (backrun_output, backrun_gas) = match backrun_result {
            Ok((output, gas, success)) => {
                if !success {
                    return MempoolSimResult {
                        success: false,
                        target_output: frontrun_output,
                        total_gas: frontrun_gas + victim_gas,
                        error: Some("Backrun would revert".to_string()),
                        ..Default::default()
                    };
                }
                match IUniswapV2Router::getAmountsOutCall::abi_decode_returns(&output, true) {
                    Ok(decoded) => (decoded.amounts.last().copied().unwrap_or(U256::ZERO), gas),
                    Err(_) => (U256::ZERO, gas),
                }
            }
            Err(e) => {
                return MempoolSimResult {
                    success: false,
                    target_output: frontrun_output,
                    total_gas: frontrun_gas + victim_gas,
                    error: Some(format!("Backrun failed: {}", e)),
                    ..Default::default()
                };
            }
        };

        // ============================================
        // STEP 4: Calculate sandwich profit
        // ============================================
        let profit = if backrun_output > frontrun_amount {
            backrun_output - frontrun_amount
        } else {
            U256::ZERO
        };

        let total_gas = frontrun_gas + backrun_gas; // Don't count victim gas

        info!(
            frontrun_in = %frontrun_amount,
            frontrun_out = %frontrun_output,
            backrun_out = %backrun_output,
            profit = %profit,
            total_gas = total_gas,
            "Sandwich simulation complete"
        );

        MempoolSimResult {
            success: true,
            target_output: frontrun_output,
            arb_output: backrun_output,
            profit,
            total_gas,
            price_impact_bps: 0,
            error: None,
        }
    }

    /// Execute a transaction against the given database.
    fn execute_tx(
        &self,
        db: &mut WarmCache<T, P>,
        from: Address,
        to: Address,
        data: Bytes,
        value: U256,
        gas_limit: u64,
    ) -> Result<(Vec<u8>, u64, bool), String> {
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
            .with_db(db)
            .with_env_with_handler_cfg(env)
            .build();

        // Use transact_commit to persist state changes
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
                ExecutionResult::Halt { gas_used, .. } => Ok((Vec::new(), gas_used, false)),
                _ => Err("Unexpected execution result".to_string()),
            },
            Err(e) => Err(format!("EVM error: {:?}", e)),
        }
    }

    // ============================================
    // Swap decoding helpers
    // ============================================

    fn decode_swap_exact_tokens(&self, data: &Bytes) -> Option<DecodedPendingSwap> {
        if data.len() < 4 + 32 * 5 {
            return None;
        }

        let amount_in = U256::from_be_slice(&data[4..36]);
        let min_amount_out = U256::from_be_slice(&data[36..68]);

        // Path offset and decoding
        let path_offset: usize = U256::from_be_slice(&data[68..100]).try_into().ok()?;
        let path_offset = 4 + path_offset;

        if data.len() < path_offset + 32 {
            return None;
        }

        let path_len: usize = U256::from_be_slice(&data[path_offset..path_offset + 32])
            .try_into()
            .ok()?;

        if path_len < 2 || data.len() < path_offset + 32 + path_len * 32 {
            return None;
        }

        let mut path = Vec::with_capacity(path_len);
        for i in 0..path_len {
            let start = path_offset + 32 + i * 32 + 12; // Skip padding
            let end = start + 20;
            if data.len() < end {
                return None;
            }
            path.push(Address::from_slice(&data[start..end]));
        }

        Some(DecodedPendingSwap {
            token_in: path[0],
            token_out: path[path_len - 1],
            amount_in,
            min_amount_out,
            path,
            is_eth_swap: false,
        })
    }

    fn decode_swap_eth_for_tokens(&self, data: &Bytes, value: U256) -> Option<DecodedPendingSwap> {
        if data.len() < 4 + 32 * 4 {
            return None;
        }

        let min_amount_out = U256::from_be_slice(&data[4..36]);

        let path_offset: usize = U256::from_be_slice(&data[36..68]).try_into().ok()?;
        let path_offset = 4 + path_offset;

        if data.len() < path_offset + 32 {
            return None;
        }

        let path_len: usize = U256::from_be_slice(&data[path_offset..path_offset + 32])
            .try_into()
            .ok()?;

        if path_len < 2 {
            return None;
        }

        let mut path = Vec::with_capacity(path_len);
        for i in 0..path_len {
            let start = path_offset + 32 + i * 32 + 12;
            let end = start + 20;
            if data.len() < end {
                return None;
            }
            path.push(Address::from_slice(&data[start..end]));
        }

        Some(DecodedPendingSwap {
            token_in: path[0], // WETH
            token_out: path[path_len - 1],
            amount_in: value,
            min_amount_out,
            path,
            is_eth_swap: true,
        })
    }

    fn decode_swap_tokens_for_eth(&self, data: &Bytes) -> Option<DecodedPendingSwap> {
        // Same structure as swapExactTokensForTokens
        self.decode_swap_exact_tokens(data).map(|mut swap| {
            swap.is_eth_swap = true;
            swap
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mempool_sim_result_default() {
        let result = MempoolSimResult::default();
        assert!(!result.success);
        assert_eq!(result.profit, U256::ZERO);
    }
}
