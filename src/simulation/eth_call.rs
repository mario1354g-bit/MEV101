//! Local simulation via eth_call.
//!
//! This module provides simulation capabilities using the `eth_call` RPC method,
//! allowing for local execution of transactions without committing them to the blockchain.

use crate::dex::SwapParams;
use crate::error::SimulationError;
use crate::simulation::SimulationResult;

use alloy::eips::{BlockId, BlockNumberOrTag};
use alloy::primitives::map::FbBuildHasher;
use alloy::primitives::{Address, Bytes, B256, I256, U256};
use alloy::providers::Provider;
use alloy::rpc::types::{state::AccountOverride, TransactionInput, TransactionRequest};
use alloy::transports::Transport;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{debug, trace, warn};

/// Error types specific to eth_call simulation.
#[derive(Debug, thiserror::Error)]
pub enum EthCallError {
    #[error("Provider error: {0}")]
    Provider(String),

    #[error("Simulation reverted: {0}")]
    Reverted(String),

    #[error("Decoding error: {0}")]
    Decoding(String),

    #[error("Encoding error: {0}")]
    Encoding(String),

    #[error("Invalid parameters: {0}")]
    InvalidParameters(String),
}

impl From<EthCallError> for SimulationError {
    fn from(e: EthCallError) -> Self {
        match e {
            EthCallError::Provider(msg) => SimulationError::ContractCallFailed(msg),
            EthCallError::Reverted(msg) => SimulationError::Reverted(msg),
            EthCallError::Decoding(msg) => SimulationError::ContractCallFailed(msg),
            EthCallError::Encoding(msg) => SimulationError::InvalidParameters(msg),
            EthCallError::InvalidParameters(msg) => SimulationError::InvalidParameters(msg),
        }
    }
}

/// Simulator using eth_call for local transaction execution.
pub struct EthCallSimulator<T, P>
where
    T: Transport + Clone,
    P: Provider<T>,
{
    /// Ethereum provider
    provider: Arc<P>,
    /// Default gas limit for simulations
    default_gas_limit: u64,
    /// Phantom data for transport type
    _transport: std::marker::PhantomData<T>,
}

impl<T, P> EthCallSimulator<T, P>
where
    T: Transport + Clone,
    P: Provider<T>,
{
    /// Create a new EthCallSimulator.
    pub fn new(provider: Arc<P>) -> Self {
        Self {
            provider,
            default_gas_limit: 1_000_000,
            _transport: std::marker::PhantomData,
        }
    }

    /// Create a new EthCallSimulator with custom gas limit.
    pub fn with_gas_limit(provider: Arc<P>, gas_limit: u64) -> Self {
        Self {
            provider,
            default_gas_limit: gas_limit,
            _transport: std::marker::PhantomData,
        }
    }

    /// Simulate a swap transaction.
    pub async fn simulate_swap(
        &self,
        from: Address,
        to: Address,
        data: Bytes,
        value: U256,
        block: BlockNumberOrTag,
    ) -> Result<SimulationResult, EthCallError> {
        debug!(
            from = %from,
            to = %to,
            value = %value,
            data_len = data.len(),
            "Simulating swap"
        );

        let tx = TransactionRequest {
            from: Some(from),
            to: Some(to.into()),
            input: TransactionInput::new(data),
            value: Some(value),
            gas: Some(self.default_gas_limit),
            ..Default::default()
        };

        self.execute_call(tx, block, None).await
    }

    /// Simulate a swap with state overrides.
    pub async fn simulate_swap_with_overrides(
        &self,
        from: Address,
        to: Address,
        data: Bytes,
        value: U256,
        block: BlockNumberOrTag,
        overrides: HashMap<Address, AccountOverride>,
    ) -> Result<SimulationResult, EthCallError> {
        debug!(
            from = %from,
            to = %to,
            value = %value,
            num_overrides = overrides.len(),
            "Simulating swap with state overrides"
        );

        let tx = TransactionRequest {
            from: Some(from),
            to: Some(to.into()),
            input: TransactionInput::new(data),
            value: Some(value),
            gas: Some(self.default_gas_limit),
            ..Default::default()
        };

        self.execute_call(tx, block, Some(overrides)).await
    }

    /// Simulate an arbitrage (two swaps in sequence).
    pub async fn simulate_arbitrage(
        &self,
        swap1: SwapParams,
        swap2: SwapParams,
        block: BlockNumberOrTag,
    ) -> Result<SimulationResult, EthCallError> {
        debug!(
            swap1_in = %swap1.amount_in,
            swap1_token_in = %swap1.token_in,
            swap2_token_out = %swap2.token_out,
            "Simulating arbitrage"
        );

        // Simulate first swap
        let data1 = self.encode_swap_data(&swap1)?;
        let value1 = if swap1.token_in == Address::ZERO {
            swap1.amount_in
        } else {
            U256::ZERO
        };

        let result1 = self
            .simulate_swap(swap1.recipient, swap1.recipient, data1, value1, block)
            .await?;

        if !result1.success {
            return Ok(SimulationResult::failed(format!(
                "First swap failed: {}",
                result1.revert_reason.unwrap_or_default()
            )));
        }

        // Simulate second swap with output from first
        let data2 = self.encode_swap_data(&swap2)?;
        let value2 = if swap2.token_in == Address::ZERO {
            result1.output_amount
        } else {
            U256::ZERO
        };

        let result2 = self
            .simulate_swap(swap2.recipient, swap2.recipient, data2, value2, block)
            .await?;

        if !result2.success {
            return Ok(SimulationResult::failed(format!(
                "Second swap failed: {}",
                result2.revert_reason.unwrap_or_default()
            )));
        }

        // Calculate profit: output of swap2 - input of swap1
        let profit = if result2.output_amount >= swap1.amount_in {
            I256::try_from(result2.output_amount - swap1.amount_in).unwrap_or(I256::ZERO)
        } else {
            -I256::try_from(swap1.amount_in - result2.output_amount).unwrap_or(I256::ZERO)
        };

        let total_gas = result1.gas_used + result2.gas_used;

        Ok(SimulationResult {
            success: true,
            output_amount: result2.output_amount,
            profit_wei: profit,
            gas_used: total_gas,
            gas_cost_wei: result1.gas_cost_wei + result2.gas_cost_wei,
            net_profit_wei: profit, // Will be updated with actual gas cost
            revert_reason: None,
            effective_gas_price: result1.effective_gas_price,
            block_number: result1.block_number,
            state_changes: None,
        })
    }

    /// Simulate a multi-hop arbitrage (multiple swaps in sequence).
    pub async fn simulate_multi_hop(
        &self,
        swaps: Vec<SwapParams>,
        block: BlockNumberOrTag,
    ) -> Result<SimulationResult, EthCallError> {
        if swaps.is_empty() {
            return Err(EthCallError::InvalidParameters(
                "At least one swap required".to_string(),
            ));
        }

        debug!(
            num_hops = swaps.len(),
            initial_amount = %swaps[0].amount_in,
            "Simulating multi-hop arbitrage"
        );

        let initial_input = swaps[0].amount_in;
        let mut current_amount = initial_input;
        let mut total_gas = 0u64;
        let mut total_gas_cost = U256::ZERO;
        let mut effective_gas_price = U256::ZERO;
        let mut block_number = 0u64;

        for (i, swap) in swaps.iter().enumerate() {
            let mut swap_with_amount = swap.clone();
            swap_with_amount.amount_in = current_amount;

            let data = self.encode_swap_data(&swap_with_amount)?;
            let value = if swap_with_amount.token_in == Address::ZERO {
                current_amount
            } else {
                U256::ZERO
            };

            let result = self
                .simulate_swap(
                    swap_with_amount.recipient,
                    swap_with_amount.recipient,
                    data,
                    value,
                    block,
                )
                .await?;

            if !result.success {
                return Ok(SimulationResult::failed(format!(
                    "Hop {} failed: {}",
                    i + 1,
                    result.revert_reason.unwrap_or_default()
                )));
            }

            current_amount = result.output_amount;
            total_gas += result.gas_used;
            total_gas_cost += result.gas_cost_wei;
            effective_gas_price = result.effective_gas_price;
            block_number = result.block_number;

            trace!(
                hop = i + 1,
                input = %swap_with_amount.amount_in,
                output = %result.output_amount,
                gas_used = result.gas_used,
                "Multi-hop step completed"
            );
        }

        // Calculate profit: final output - initial input
        let profit = if current_amount >= initial_input {
            I256::try_from(current_amount - initial_input).unwrap_or(I256::ZERO)
        } else {
            -I256::try_from(initial_input - current_amount).unwrap_or(I256::ZERO)
        };

        Ok(SimulationResult {
            success: true,
            output_amount: current_amount,
            profit_wei: profit,
            gas_used: total_gas,
            gas_cost_wei: total_gas_cost,
            net_profit_wei: profit,
            revert_reason: None,
            effective_gas_price,
            block_number,
            state_changes: None,
        })
    }

    /// Simulate a sandwich attack.
    pub async fn simulate_sandwich(
        &self,
        frontrun: SwapParams,
        victim_impact: U256,
        backrun: SwapParams,
        block: BlockNumberOrTag,
    ) -> Result<SimulationResult, EthCallError> {
        debug!(
            frontrun_amount = %frontrun.amount_in,
            victim_impact = %victim_impact,
            backrun_amount = %backrun.amount_in,
            "Simulating sandwich attack"
        );

        // Simulate frontrun
        let frontrun_data = self.encode_swap_data(&frontrun)?;
        let frontrun_value = if frontrun.token_in == Address::ZERO {
            frontrun.amount_in
        } else {
            U256::ZERO
        };

        let frontrun_result = self
            .simulate_swap(
                frontrun.recipient,
                frontrun.recipient,
                frontrun_data,
                frontrun_value,
                block,
            )
            .await?;

        if !frontrun_result.success {
            return Ok(SimulationResult::failed(format!(
                "Frontrun failed: {}",
                frontrun_result.revert_reason.unwrap_or_default()
            )));
        }

        // The victim transaction happens here (we can't simulate it directly)
        // We estimate its impact on our backrun

        // Simulate backrun with adjusted amounts based on victim impact
        let mut adjusted_backrun = backrun.clone();
        adjusted_backrun.amount_in = frontrun_result.output_amount;
        // Adjust expected output considering victim's impact
        adjusted_backrun.amount_out_min = adjusted_backrun
            .amount_out_min
            .saturating_add(victim_impact);

        let backrun_data = self.encode_swap_data(&adjusted_backrun)?;
        let backrun_value = if adjusted_backrun.token_in == Address::ZERO {
            adjusted_backrun.amount_in
        } else {
            U256::ZERO
        };

        let backrun_result = self
            .simulate_swap(
                adjusted_backrun.recipient,
                adjusted_backrun.recipient,
                backrun_data,
                backrun_value,
                block,
            )
            .await?;

        if !backrun_result.success {
            return Ok(SimulationResult::failed(format!(
                "Backrun failed: {}",
                backrun_result.revert_reason.unwrap_or_default()
            )));
        }

        // Calculate sandwich profit
        // Profit = backrun output - frontrun input + victim impact extracted
        let total_input = frontrun.amount_in;
        let total_output = backrun_result.output_amount;
        let profit = if total_output >= total_input {
            I256::try_from(total_output - total_input).unwrap_or(I256::ZERO)
        } else {
            -I256::try_from(total_input - total_output).unwrap_or(I256::ZERO)
        };

        let total_gas = frontrun_result.gas_used + backrun_result.gas_used;
        let total_gas_cost = frontrun_result.gas_cost_wei + backrun_result.gas_cost_wei;

        Ok(SimulationResult {
            success: true,
            output_amount: backrun_result.output_amount,
            profit_wei: profit,
            gas_used: total_gas,
            gas_cost_wei: total_gas_cost,
            net_profit_wei: profit,
            revert_reason: None,
            effective_gas_price: frontrun_result.effective_gas_price,
            block_number: frontrun_result.block_number,
            state_changes: None,
        })
    }

    /// Execute an eth_call and parse the result.
    async fn execute_call(
        &self,
        tx: TransactionRequest,
        block: BlockNumberOrTag,
        overrides: Option<HashMap<Address, AccountOverride>>,
    ) -> Result<SimulationResult, EthCallError> {
        // Build the call with optional state overrides
        let block_id: BlockId = block.into();
        let call_builder = self.provider.call(&tx).block(block_id);

        let result = if let Some(state_overrides) = overrides {
            // Alloy expects HashMap with FbBuildHasher<20> for Address keys
            let state_overrides_with_hasher: HashMap<Address, AccountOverride, FbBuildHasher<20>> =
                state_overrides.into_iter().collect();
            call_builder
                .overrides(&state_overrides_with_hasher)
                .await
        } else {
            call_builder.await
        };

        match result {
            Ok(output) => {
                // Decode output to get actual amounts
                let output_amount = decode_swap_output(&output)?;

                // Estimate gas used (simplified - would need trace for accurate value)
                let gas_used = estimate_gas_from_output(&output);

                // Get block number
                let block_number = match block {
                    BlockNumberOrTag::Number(n) => n,
                    _ => self
                        .provider
                        .get_block_number()
                        .await
                        .map_err(|e| EthCallError::Provider(e.to_string()))?,
                };

                debug!(
                    output_amount = %output_amount,
                    gas_used = gas_used,
                    "Simulation succeeded"
                );

                Ok(SimulationResult {
                    success: true,
                    output_amount,
                    profit_wei: I256::ZERO, // Calculated by caller
                    gas_used,
                    gas_cost_wei: U256::ZERO, // Calculated by caller
                    net_profit_wei: I256::ZERO,
                    revert_reason: None,
                    effective_gas_price: U256::ZERO,
                    block_number,
                    state_changes: None,
                })
            }
            Err(e) => {
                // Extract revert reason if available
                let error_string = e.to_string();
                let revert_reason = extract_revert_reason(&error_string);

                warn!(
                    error = %error_string,
                    revert_reason = %revert_reason,
                    "Simulation reverted"
                );

                Ok(SimulationResult {
                    success: false,
                    output_amount: U256::ZERO,
                    profit_wei: I256::ZERO,
                    gas_used: 0,
                    gas_cost_wei: U256::ZERO,
                    net_profit_wei: I256::ZERO,
                    revert_reason: Some(revert_reason),
                    effective_gas_price: U256::ZERO,
                    block_number: 0,
                    state_changes: None,
                })
            }
        }
    }

    /// Encode swap data for a DEX router.
    pub fn encode_swap_data(&self, params: &SwapParams) -> Result<Bytes, EthCallError> {
        // Encode for Uniswap V2 style swapExactTokensForTokens
        // Function signature: swapExactTokensForTokens(uint256,uint256,address[],address,uint256)
        let selector = [0x38, 0xed, 0x17, 0x39]; // swapExactTokensForTokens

        let mut data = Vec::with_capacity(4 + 32 * 5 + 32 * params.path.len());
        data.extend_from_slice(&selector);

        // amountIn
        data.extend_from_slice(&encode_u256(params.amount_in));
        // amountOutMin
        data.extend_from_slice(&encode_u256(params.amount_out_min));
        // path offset (dynamic array)
        data.extend_from_slice(&encode_u256(U256::from(160))); // 5 * 32 bytes offset
        // to
        data.extend_from_slice(&encode_address(params.recipient));
        // deadline
        data.extend_from_slice(&encode_u256(params.deadline));
        // path length
        data.extend_from_slice(&encode_u256(U256::from(params.path.len())));
        // path elements
        for addr in &params.path {
            data.extend_from_slice(&encode_address(*addr));
        }

        Ok(Bytes::from(data))
    }

    /// Encode swap data for ETH input (swapExactETHForTokens).
    pub fn encode_eth_swap_data(&self, params: &SwapParams) -> Result<Bytes, EthCallError> {
        // Function signature: swapExactETHForTokens(uint256,address[],address,uint256)
        let selector = [0x7f, 0xf3, 0x6a, 0xb5]; // swapExactETHForTokens

        let mut data = Vec::with_capacity(4 + 32 * 4 + 32 * params.path.len());
        data.extend_from_slice(&selector);

        // amountOutMin
        data.extend_from_slice(&encode_u256(params.amount_out_min));
        // path offset
        data.extend_from_slice(&encode_u256(U256::from(128))); // 4 * 32 bytes offset
        // to
        data.extend_from_slice(&encode_address(params.recipient));
        // deadline
        data.extend_from_slice(&encode_u256(params.deadline));
        // path length
        data.extend_from_slice(&encode_u256(U256::from(params.path.len())));
        // path elements
        for addr in &params.path {
            data.extend_from_slice(&encode_address(*addr));
        }

        Ok(Bytes::from(data))
    }

    /// Encode swap data for Uniswap V3.
    pub fn encode_v3_swap_data(&self, params: &SwapParams) -> Result<Bytes, EthCallError> {
        // exactInputSingle for single-hop V3 swap
        // Function: exactInputSingle((address,address,uint24,address,uint256,uint256,uint256,uint160))
        let selector = [0x41, 0x4b, 0xf3, 0x89]; // exactInputSingle

        let fee = params.fee.unwrap_or(3000);

        let mut data = Vec::with_capacity(4 + 32 * 8);
        data.extend_from_slice(&selector);

        // tokenIn
        data.extend_from_slice(&encode_address(params.token_in));
        // tokenOut
        data.extend_from_slice(&encode_address(params.token_out));
        // fee
        data.extend_from_slice(&encode_u256(U256::from(fee)));
        // recipient
        data.extend_from_slice(&encode_address(params.recipient));
        // deadline
        data.extend_from_slice(&encode_u256(params.deadline));
        // amountIn
        data.extend_from_slice(&encode_u256(params.amount_in));
        // amountOutMinimum
        data.extend_from_slice(&encode_u256(params.amount_out_min));
        // sqrtPriceLimitX96 (0 = no limit)
        data.extend_from_slice(&encode_u256(U256::ZERO));

        Ok(Bytes::from(data))
    }

    /// Create state overrides to simulate having a token balance.
    pub fn create_balance_override(
        &self,
        token: Address,
        holder: Address,
        balance: U256,
    ) -> HashMap<Address, AccountOverride> {
        let mut overrides = HashMap::new();

        // For ERC20 tokens, the balance mapping is typically at slot 0
        // balanceOf[holder] is stored at keccak256(holder . slot)

        // Calculate storage slot for balanceOf mapping
        let slot = calculate_balance_slot(holder, U256::ZERO);
        // Convert U256 slot to B256 for storage key
        let slot_b256 = B256::from(slot.to_be_bytes::<32>());
        // Convert balance U256 to B256 for storage value
        let balance_b256 = B256::from(balance.to_be_bytes::<32>());

        // Use state_diff with FbBuildHasher<32> for B256 keys
        let mut state_diff: HashMap<B256, B256, FbBuildHasher<32>> = HashMap::with_hasher(FbBuildHasher::<32>::default());
        state_diff.insert(slot_b256, balance_b256);

        let override_entry = AccountOverride {
            balance: None,
            nonce: None,
            code: None,
            state: None,
            state_diff: Some(state_diff),
            move_precompile_to: None,
        };

        overrides.insert(token, override_entry);
        overrides
    }

    /// Create state overrides for ETH balance.
    pub fn create_eth_balance_override(
        &self,
        account: Address,
        balance: U256,
    ) -> HashMap<Address, AccountOverride> {
        let mut overrides = HashMap::new();

        let override_entry = AccountOverride {
            balance: Some(balance),
            nonce: None,
            code: None,
            state: None,
            state_diff: None,
            move_precompile_to: None,
        };

        overrides.insert(account, override_entry);
        overrides
    }
}

/// Decode the output from a swap call.
fn decode_swap_output(output: &Bytes) -> Result<U256, EthCallError> {
    if output.is_empty() {
        return Ok(U256::ZERO);
    }

    // Uniswap V2 swapExactTokensForTokens returns uint256[]
    // The last element is the output amount
    if output.len() >= 64 {
        // Check if it's a dynamic array (starts with offset)
        let first_word = U256::from_be_slice(&output[0..32]);

        if first_word == U256::from(32) && output.len() >= 96 {
            // Dynamic array format: offset, length, elements...
            let length = U256::from_be_slice(&output[32..64]);
            let length_usize: usize = length
                .try_into()
                .map_err(|_| EthCallError::Decoding("Array length too large".to_string()))?;

            if output.len() >= 64 + 32 * length_usize && length_usize > 0 {
                // Get the last element
                let start = 64 + 32 * (length_usize - 1);
                let end = start + 32;
                return Ok(U256::from_be_slice(&output[start..end]));
            }
        } else if output.len() == 32 {
            // Single uint256 return value
            return Ok(U256::from_be_slice(&output[0..32]));
        }
    }

    // Uniswap V3 returns just the output amount as uint256
    if output.len() >= 32 {
        return Ok(U256::from_be_slice(&output[0..32]));
    }

    Err(EthCallError::Decoding(format!(
        "Unknown output format: {} bytes",
        output.len()
    )))
}

/// Extract revert reason from an error message.
fn extract_revert_reason(error: &str) -> String {
    // Try to extract Error(string) revert reason
    if let Some(start) = error.find("Error(") {
        if let Some(end) = error[start..].find(')') {
            return error[start + 6..start + end].to_string();
        }
    }

    // Try to extract Panic(uint256) code
    if let Some(start) = error.find("Panic(") {
        if let Some(end) = error[start..].find(')') {
            let panic_code = &error[start + 6..start + end];
            return format!("Panic: {}", decode_panic_reason(panic_code));
        }
    }

    // Try to find "execution reverted:" prefix
    if let Some(idx) = error.find("execution reverted:") {
        return error[idx + 19..].trim().to_string();
    }

    // Try to find hex-encoded revert data
    if let Some(idx) = error.find("0x") {
        let hex_data = &error[idx..];
        if hex_data.len() >= 10 {
            let selector = &hex_data[2..10];
            // Error(string) selector: 0x08c379a0
            if selector == "08c379a0" {
                if let Some(decoded) = decode_error_string(hex_data) {
                    return decoded;
                }
            }
        }
    }

    // Return a shortened version of the error
    if error.len() > 100 {
        format!("{}...", &error[..100])
    } else {
        error.to_string()
    }
}

/// Decode Error(string) revert data.
fn decode_error_string(hex_data: &str) -> Option<String> {
    let hex_clean = hex_data.trim_start_matches("0x");
    if hex_clean.len() < 136 {
        // 4 (selector) + 32 (offset) + 32 (length) + at least some data
        return None;
    }

    // Skip selector (8 chars) and offset (64 chars)
    let length_hex = &hex_clean[72..136];
    let length = u64::from_str_radix(length_hex, 16).ok()?;

    if length == 0 || length > 1000 {
        return None;
    }

    let data_start = 136;
    let data_end = data_start + (length as usize * 2);

    if hex_clean.len() < data_end {
        return None;
    }

    let string_hex = &hex_clean[data_start..data_end];
    let bytes: Vec<u8> = (0..string_hex.len())
        .step_by(2)
        .filter_map(|i| u8::from_str_radix(&string_hex[i..i + 2], 16).ok())
        .collect();

    String::from_utf8(bytes).ok()
}

/// Decode Solidity panic code to human-readable message.
fn decode_panic_reason(code: &str) -> &'static str {
    match code.trim() {
        "0x01" | "1" => "Assertion failed",
        "0x11" | "17" => "Arithmetic overflow/underflow",
        "0x12" | "18" => "Division by zero",
        "0x21" | "33" => "Invalid enum value",
        "0x22" | "34" => "Storage encoding error",
        "0x31" | "49" => "Pop from empty array",
        "0x32" | "50" => "Array index out of bounds",
        "0x41" | "65" => "Memory allocation overflow",
        "0x51" | "81" => "Called zero-initialized function",
        _ => "Unknown panic code",
    }
}

/// Estimate gas used from the output (simplified).
fn estimate_gas_from_output(output: &Bytes) -> u64 {
    // Base gas for a swap is around 100-200k
    // Add some gas per byte of output
    let base_gas = 150_000u64;
    let output_gas = (output.len() as u64) * 16; // 16 gas per byte of return data

    base_gas + output_gas
}

/// Encode a U256 as 32 bytes (big-endian, left-padded).
fn encode_u256(value: U256) -> [u8; 32] {
    value.to_be_bytes()
}

/// Encode an address as 32 bytes (left-padded).
fn encode_address(addr: Address) -> [u8; 32] {
    let mut result = [0u8; 32];
    result[12..32].copy_from_slice(addr.as_slice());
    result
}

/// Calculate the storage slot for a mapping value.
fn calculate_balance_slot(key: Address, mapping_slot: U256) -> U256 {
    use alloy::primitives::keccak256;

    let mut data = [0u8; 64];
    data[12..32].copy_from_slice(key.as_slice());
    data[32..64].copy_from_slice(&mapping_slot.to_be_bytes::<32>());

    U256::from_be_bytes(keccak256(data).0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decode_swap_output_single_u256() {
        let output = Bytes::from(U256::from(1000).to_be_bytes::<32>().to_vec());
        let result = decode_swap_output(&output).unwrap();
        assert_eq!(result, U256::from(1000));
    }

    #[test]
    fn test_decode_swap_output_array() {
        // Simulate array output: offset (32), length (2), values (100, 200)
        let mut output = Vec::new();
        output.extend_from_slice(&U256::from(32).to_be_bytes::<32>()); // offset
        output.extend_from_slice(&U256::from(2).to_be_bytes::<32>()); // length
        output.extend_from_slice(&U256::from(100).to_be_bytes::<32>()); // first value
        output.extend_from_slice(&U256::from(200).to_be_bytes::<32>()); // last value (output)

        let result = decode_swap_output(&Bytes::from(output)).unwrap();
        assert_eq!(result, U256::from(200)); // Should return last element
    }

    #[test]
    fn test_extract_revert_reason() {
        let error = "execution reverted: INSUFFICIENT_OUTPUT_AMOUNT";
        let reason = extract_revert_reason(error);
        assert_eq!(reason, "INSUFFICIENT_OUTPUT_AMOUNT");
    }

    #[test]
    fn test_extract_revert_reason_panic() {
        let error = "Panic(0x11)";
        let reason = extract_revert_reason(error);
        assert!(reason.contains("Panic"));
        assert!(reason.contains("Arithmetic"));
    }

    #[test]
    fn test_encode_u256() {
        let value = U256::from(256);
        let encoded = encode_u256(value);
        assert_eq!(encoded[31], 0);
        assert_eq!(encoded[30], 1);
    }

    #[test]
    fn test_encode_address() {
        let addr = Address::repeat_byte(0xAB);
        let encoded = encode_address(addr);
        assert_eq!(&encoded[0..12], &[0u8; 12]); // Padding
        assert_eq!(&encoded[12..32], addr.as_slice());
    }

    #[test]
    fn test_decode_panic_reason() {
        assert_eq!(decode_panic_reason("0x11"), "Arithmetic overflow/underflow");
        assert_eq!(decode_panic_reason("17"), "Arithmetic overflow/underflow");
        assert_eq!(decode_panic_reason("0x12"), "Division by zero");
    }
}
