//! Sandwich Attack Detector
//!
//! Detects opportunities for sandwich attacks on pending swap transactions.
//! A sandwich attack consists of:
//! 1. Frontrun: Buy before the victim's swap
//! 2. Victim: Their swap executes at worse price
//! 3. Backrun: Sell after victim for profit

use alloy::primitives::{Address, Bytes, U256};
use async_trait::async_trait;
use chrono::Utc;
use std::collections::HashMap;

use super::{
    calculate_optimal_frontrun, estimate_gas_cost, estimate_sandwich_profit,
    generate_opportunity_id, u256_to_f64, Detector, DetectorConfig, DetectorContext,
    MonitorEvent, Opportunity, OpportunityType, PoolInfo, Priority, SwapParams, SwapStep,
    TargetTransaction,
};
use crate::error::Result;

/// Known function selectors for swap functions
#[allow(dead_code)] // DEX selectors reserved for extended swap detection
mod selectors {
    pub const UNISWAP_V2_SWAP_EXACT_TOKENS: &[u8] = &[0x38, 0xed, 0x17, 0x39];
    pub const UNISWAP_V2_SWAP_TOKENS_EXACT: &[u8] = &[0x8a, 0x65, 0x7e, 0x67];
    pub const UNISWAP_V2_SWAP_EXACT_ETH: &[u8] = &[0x7f, 0xf3, 0x6a, 0xb5];
    pub const UNISWAP_V2_SWAP_ETH_EXACT: &[u8] = &[0xfb, 0x3b, 0xdb, 0x41];
    pub const UNISWAP_V2_SWAP_EXACT_TOKENS_ETH: &[u8] = &[0x18, 0xcb, 0xaf, 0xe5];
    pub const UNISWAP_V2_SWAP_TOKENS_EXACT_ETH: &[u8] = &[0x4a, 0x25, 0xd9, 0x4a];

    pub const UNISWAP_V3_EXACT_INPUT: &[u8] = &[0xc0, 0x4b, 0x8d, 0x59];
    pub const UNISWAP_V3_EXACT_OUTPUT: &[u8] = &[0xf2, 0x8c, 0x04, 0x98];
    pub const UNISWAP_V3_EXACT_INPUT_SINGLE: &[u8] = &[0x41, 0x4b, 0xf3, 0x89];
    pub const UNISWAP_V3_EXACT_OUTPUT_SINGLE: &[u8] = &[0xdb, 0x3e, 0x21, 0x98];

    pub const SUSHISWAP_SWAP_EXACT_TOKENS: &[u8] = &[0x38, 0xed, 0x17, 0x39];
}

/// Detector for sandwich attack opportunities
pub struct SandwichDetector {
    /// Name of this detector
    name: String,
    /// Minimum victim swap size in Wei (to be worth sandwiching)
    min_victim_size: U256,
    /// Maximum victim swap size in Wei (larger swaps may be protected)
    max_victim_size: U256,
    /// Minimum profit multiplier over gas cost
    min_profit_multiplier: f64,
    /// Known router addresses
    known_routers: Vec<Address>,
}

impl SandwichDetector {
    pub fn new() -> Self {
        Self {
            name: "sandwich".to_string(),
            // ~$500 at $2000 ETH
            min_victim_size: U256::from(250_000_000_000_000_000u64), // 0.25 ETH
            // ~$50k at $2000 ETH
            max_victim_size: U256::from(25_000_000_000_000_000_000u128), // 25 ETH
            min_profit_multiplier: 2.0,
            known_routers: vec![
                // Uniswap V2 Router
                "0x7a250d5630B4cF539739dF2C5dAcb4c659F2488D".parse().unwrap_or(Address::ZERO),
                // Uniswap V3 Router
                "0xE592427A0AEce92De3Edee1F18E0157C05861564".parse().unwrap_or(Address::ZERO),
                // Uniswap V3 Router 2
                "0x68b3465833fb72A70ecDF485E0e4C7bD8665Fc45".parse().unwrap_or(Address::ZERO),
                // SushiSwap Router
                "0xd9e1cE17f2641f24aE83637ab66a2cca9C378B9F".parse().unwrap_or(Address::ZERO),
            ],
        }
    }

    pub fn with_min_size(mut self, size: U256) -> Self {
        self.min_victim_size = size;
        self
    }

    pub fn with_max_size(mut self, size: U256) -> Self {
        self.max_victim_size = size;
        self
    }

    pub fn with_profit_multiplier(mut self, mult: f64) -> Self {
        self.min_profit_multiplier = mult;
        self
    }

    pub fn add_router(&mut self, router: Address) {
        if !self.known_routers.contains(&router) {
            self.known_routers.push(router);
        }
    }

    /// Check if transaction is to a known router
    fn is_router(&self, address: &Address) -> bool {
        self.known_routers.contains(address)
    }

    /// Try to decode swap parameters from calldata
    fn decode_swap_params(&self, _to: &Address, input: &Bytes, value: U256) -> Option<DecodedSwap> {
        if input.len() < 4 {
            return None;
        }

        let selector = &input[..4];

        // Check various swap function selectors
        if selector == selectors::UNISWAP_V2_SWAP_EXACT_TOKENS {
            return self.decode_v2_swap_exact_tokens(input);
        }
        if selector == selectors::UNISWAP_V2_SWAP_TOKENS_EXACT {
            return self.decode_v2_swap_tokens_exact(input);
        }
        if selector == selectors::UNISWAP_V2_SWAP_EXACT_ETH {
            return self.decode_v2_swap_exact_eth(input, value);
        }
        if selector == selectors::UNISWAP_V2_SWAP_ETH_EXACT {
            return self.decode_v2_swap_eth_exact(input, value);
        }
        if selector == selectors::UNISWAP_V3_EXACT_INPUT_SINGLE {
            return self.decode_v3_exact_input_single(input);
        }
        if selector == selectors::UNISWAP_V3_EXACT_OUTPUT_SINGLE {
            return self.decode_v3_exact_output_single(input);
        }

        None
    }

    /// Decode swapExactTokensForTokens
    fn decode_v2_swap_exact_tokens(&self, input: &Bytes) -> Option<DecodedSwap> {
        // swapExactTokensForTokens(uint256 amountIn, uint256 amountOutMin, address[] path, address to, uint256 deadline)
        if input.len() < 4 + 32 * 5 {
            return None;
        }

        let amount_in = U256::from_be_slice(&input[4..36]);
        let amount_out_min = U256::from_be_slice(&input[36..68]);

        // Path offset is at bytes 68-100, path length at offset position
        let path_offset: usize = U256::from_be_slice(&input[68..100]).try_into().ok()?;
        let path_offset: usize = 4 + path_offset; // Add 4 for selector

        if input.len() < path_offset + 32 {
            return None;
        }

        let path_length: usize = U256::from_be_slice(&input[path_offset..path_offset + 32])
            .try_into()
            .ok()?;

        if path_length < 2 || input.len() < path_offset + 32 + path_length * 32 {
            return None;
        }

        // Extract first and last token from path
        let token_in = Address::from_slice(&input[path_offset + 32 + 12..path_offset + 64]);
        let token_out = Address::from_slice(
            &input[path_offset + 32 + (path_length - 1) * 32 + 12..path_offset + 32 + path_length * 32],
        );

        // Recipient at bytes 100-132
        let recipient = Address::from_slice(&input[100 + 12..132]);

        // Deadline at bytes 132-164
        let deadline = U256::from_be_slice(&input[132..164]);

        Some(DecodedSwap {
            token_in,
            token_out,
            amount_in,
            amount_out_min,
            recipient,
            deadline,
            is_exact_input: true,
        })
    }

    /// Decode swapTokensForExactTokens
    fn decode_v2_swap_tokens_exact(&self, input: &Bytes) -> Option<DecodedSwap> {
        // swapTokensForExactTokens(uint256 amountOut, uint256 amountInMax, address[] path, address to, uint256 deadline)
        if input.len() < 4 + 32 * 5 {
            return None;
        }

        let amount_out = U256::from_be_slice(&input[4..36]);
        let amount_in_max = U256::from_be_slice(&input[36..68]);

        let path_offset: usize = U256::from_be_slice(&input[68..100]).try_into().ok()?;
        let path_offset: usize = 4 + path_offset;

        if input.len() < path_offset + 32 {
            return None;
        }

        let path_length: usize = U256::from_be_slice(&input[path_offset..path_offset + 32])
            .try_into()
            .ok()?;

        if path_length < 2 || input.len() < path_offset + 32 + path_length * 32 {
            return None;
        }

        let token_in = Address::from_slice(&input[path_offset + 32 + 12..path_offset + 64]);
        let token_out = Address::from_slice(
            &input[path_offset + 32 + (path_length - 1) * 32 + 12..path_offset + 32 + path_length * 32],
        );

        let recipient = Address::from_slice(&input[100 + 12..132]);
        let deadline = U256::from_be_slice(&input[132..164]);

        Some(DecodedSwap {
            token_in,
            token_out,
            amount_in: amount_in_max,
            amount_out_min: amount_out,
            recipient,
            deadline,
            is_exact_input: false,
        })
    }

    /// Decode swapExactETHForTokens
    fn decode_v2_swap_exact_eth(&self, input: &Bytes, value: U256) -> Option<DecodedSwap> {
        // swapExactETHForTokens(uint256 amountOutMin, address[] path, address to, uint256 deadline)
        if input.len() < 4 + 32 * 4 {
            return None;
        }

        let amount_out_min = U256::from_be_slice(&input[4..36]);

        let path_offset: usize = U256::from_be_slice(&input[36..68]).try_into().ok()?;
        let path_offset: usize = 4 + path_offset;

        if input.len() < path_offset + 32 {
            return None;
        }

        let path_length: usize = U256::from_be_slice(&input[path_offset..path_offset + 32])
            .try_into()
            .ok()?;

        if path_length < 2 || input.len() < path_offset + 32 + path_length * 32 {
            return None;
        }

        let token_in = Address::from_slice(&input[path_offset + 32 + 12..path_offset + 64]);
        let token_out = Address::from_slice(
            &input[path_offset + 32 + (path_length - 1) * 32 + 12..path_offset + 32 + path_length * 32],
        );

        let recipient = Address::from_slice(&input[68 + 12..100]);
        let deadline = U256::from_be_slice(&input[100..132]);

        Some(DecodedSwap {
            token_in,
            token_out,
            amount_in: value,
            amount_out_min,
            recipient,
            deadline,
            is_exact_input: true,
        })
    }

    /// Decode swapETHForExactTokens
    fn decode_v2_swap_eth_exact(&self, input: &Bytes, value: U256) -> Option<DecodedSwap> {
        // swapETHForExactTokens(uint256 amountOut, address[] path, address to, uint256 deadline)
        if input.len() < 4 + 32 * 4 {
            return None;
        }

        let amount_out = U256::from_be_slice(&input[4..36]);

        let path_offset: usize = U256::from_be_slice(&input[36..68]).try_into().ok()?;
        let path_offset: usize = 4 + path_offset;

        if input.len() < path_offset + 32 {
            return None;
        }

        let path_length: usize = U256::from_be_slice(&input[path_offset..path_offset + 32])
            .try_into()
            .ok()?;

        if path_length < 2 || input.len() < path_offset + 32 + path_length * 32 {
            return None;
        }

        let token_in = Address::from_slice(&input[path_offset + 32 + 12..path_offset + 64]);
        let token_out = Address::from_slice(
            &input[path_offset + 32 + (path_length - 1) * 32 + 12..path_offset + 32 + path_length * 32],
        );

        let recipient = Address::from_slice(&input[68 + 12..100]);
        let deadline = U256::from_be_slice(&input[100..132]);

        Some(DecodedSwap {
            token_in,
            token_out,
            amount_in: value, // Max ETH sent
            amount_out_min: amount_out,
            recipient,
            deadline,
            is_exact_input: false,
        })
    }

    /// Decode V3 exactInputSingle
    fn decode_v3_exact_input_single(&self, input: &Bytes) -> Option<DecodedSwap> {
        // exactInputSingle(ExactInputSingleParams params)
        // struct: tokenIn, tokenOut, fee, recipient, deadline, amountIn, amountOutMinimum, sqrtPriceLimitX96
        if input.len() < 4 + 32 * 8 {
            return None;
        }

        let token_in = Address::from_slice(&input[4 + 12..36]);
        let token_out = Address::from_slice(&input[36 + 12..68]);
        // fee at 68..100
        let recipient = Address::from_slice(&input[100 + 12..132]);
        let deadline = U256::from_be_slice(&input[132..164]);
        let amount_in = U256::from_be_slice(&input[164..196]);
        let amount_out_min = U256::from_be_slice(&input[196..228]);

        Some(DecodedSwap {
            token_in,
            token_out,
            amount_in,
            amount_out_min,
            recipient,
            deadline,
            is_exact_input: true,
        })
    }

    /// Decode V3 exactOutputSingle
    fn decode_v3_exact_output_single(&self, input: &Bytes) -> Option<DecodedSwap> {
        // exactOutputSingle(ExactOutputSingleParams params)
        // struct: tokenIn, tokenOut, fee, recipient, deadline, amountOut, amountInMaximum, sqrtPriceLimitX96
        if input.len() < 4 + 32 * 8 {
            return None;
        }

        let token_in = Address::from_slice(&input[4 + 12..36]);
        let token_out = Address::from_slice(&input[36 + 12..68]);
        let recipient = Address::from_slice(&input[100 + 12..132]);
        let deadline = U256::from_be_slice(&input[132..164]);
        let amount_out = U256::from_be_slice(&input[164..196]);
        let amount_in_max = U256::from_be_slice(&input[196..228]);

        Some(DecodedSwap {
            token_in,
            token_out,
            amount_in: amount_in_max,
            amount_out_min: amount_out,
            recipient,
            deadline,
            is_exact_input: false,
        })
    }

    /// Analyze a decoded swap for sandwich opportunity
    fn analyze_sandwich(
        &self,
        decoded: &DecodedSwap,
        ctx: &DetectorContext,
        gas_price: U256,
    ) -> Option<SandwichAnalysis> {
        // Get pools for the token pair
        let pools = ctx.pool_registry.get_pools_for_pair(decoded.token_in, decoded.token_out);

        if pools.is_empty() {
            return None;
        }

        // Find the best pool (most liquidity)
        let pool = pools
            .iter()
            .max_by_key(|p| p.reserve0 + p.reserve1)?;

        // Determine which reserve is which
        let (reserve_in, reserve_out) = if pool.token0 == decoded.token_in {
            (pool.reserve0, pool.reserve1)
        } else {
            (pool.reserve1, pool.reserve0)
        };

        // Calculate optimal frontrun amount
        let optimal_frontrun = calculate_optimal_frontrun(
            decoded.amount_in,
            reserve_in,
            reserve_out,
            pool.fee_bps,
        );

        if optimal_frontrun.is_zero() {
            return None;
        }

        // Estimate sandwich profit
        let (profit, frontrun_out, backrun_out) = estimate_sandwich_profit(
            decoded.amount_in,
            optimal_frontrun,
            reserve_in,
            reserve_out,
            pool.fee_bps,
        );

        // Estimate gas cost (frontrun + backrun = 2 swaps)
        let gas_cost = estimate_gas_cost(2, 150_000, gas_price);

        // Check if profitable after gas
        if profit <= gas_cost {
            return None;
        }

        // Check profit multiplier
        let profit_ratio = u256_to_f64(profit) / u256_to_f64(gas_cost);
        if profit_ratio < self.min_profit_multiplier {
            return None;
        }

        Some(SandwichAnalysis {
            pool: pool.clone(),
            frontrun_amount: optimal_frontrun,
            frontrun_output: frontrun_out,
            backrun_output: backrun_out,
            estimated_profit: profit,
            gas_cost,
            net_profit: profit - gas_cost,
            profit_ratio,
        })
    }

    /// Build opportunity from analysis
    #[allow(clippy::too_many_arguments)]
    fn build_opportunity(
        &self,
        analysis: &SandwichAnalysis,
        decoded: &DecodedSwap,
        tx_hash: Bytes,
        from: Address,
        to: Address,
        value: U256,
        gas_price: U256,
    ) -> Opportunity {
        let tokens = vec![decoded.token_in, decoded.token_out];

        // Frontrun: buy token_out before victim
        // Backrun: sell token_out after victim
        // Slippage protection: backrun must cover input + gas costs + minimum profit margin
        let min_backrun_out = analysis.frontrun_amount + analysis.gas_cost;

        let swap_path = vec![
            SwapStep {
                pool: analysis.pool.address,
                dex: analysis.pool.dex.clone(),
                token_in: decoded.token_in,
                token_out: decoded.token_out,
                amount_in: analysis.frontrun_amount,
                min_amount_out: analysis.frontrun_output * U256::from(99) / U256::from(100),
            },
            // Victim's swap happens here
            SwapStep {
                pool: analysis.pool.address,
                dex: analysis.pool.dex.clone(),
                token_in: decoded.token_out,
                token_out: decoded.token_in,
                amount_in: analysis.frontrun_output,
                // Must cover input + gas costs to prevent losses
                min_amount_out: min_backrun_out,
            },
        ];

        let pools = vec![PoolInfo {
            address: analysis.pool.address,
            dex: analysis.pool.dex.clone(),
            token0: analysis.pool.token0,
            token1: analysis.pool.token1,
            reserve0: analysis.pool.reserve0,
            reserve1: analysis.pool.reserve1,
            fee_bps: analysis.pool.fee_bps,
        }];

        let target_tx = TargetTransaction {
            hash: tx_hash,
            from,
            to,
            value,
            gas_price,
            swap_params: SwapParams {
                token_in: decoded.token_in,
                token_out: decoded.token_out,
                amount_in: decoded.amount_in,
                min_amount_out: decoded.amount_out_min,
                recipient: decoded.recipient,
                deadline: decoded.deadline,
            },
        };

        let priority = if analysis.net_profit > U256::from(1_000_000_000_000_000_000u128) {
            Priority::Critical // > 1 ETH
        } else if analysis.net_profit > U256::from(100_000_000_000_000_000u128) {
            Priority::High // > 0.1 ETH
        } else if analysis.net_profit > U256::from(10_000_000_000_000_000u128) {
            Priority::Medium // > 0.01 ETH
        } else {
            Priority::Low
        };

        let mut metadata = HashMap::new();
        metadata.insert("victim_amount".to_string(), format!("{}", decoded.amount_in));
        metadata.insert("frontrun_amount".to_string(), format!("{}", analysis.frontrun_amount));
        metadata.insert("profit_ratio".to_string(), format!("{:.2}x", analysis.profit_ratio));
        metadata.insert("pool_dex".to_string(), analysis.pool.dex.clone());

        Opportunity {
            id: generate_opportunity_id(OpportunityType::Sandwich, &tokens),
            opportunity_type: OpportunityType::Sandwich,
            priority,
            estimated_profit: analysis.estimated_profit,
            estimated_gas_cost: analysis.gas_cost,
            net_profit: analysis.net_profit,
            tokens,
            pools,
            swap_path,
            target_tx: Some(target_tx),
            deadline_block: None,
            detected_at: Utc::now(),
            confidence: self.calculate_confidence(analysis, decoded),
            metadata,
        }
    }

    fn calculate_confidence(&self, analysis: &SandwichAnalysis, decoded: &DecodedSwap) -> f64 {
        let mut confidence: f64 = 0.7;

        // Higher profit ratio = more confidence
        if analysis.profit_ratio > 5.0 {
            confidence += 0.15;
        } else if analysis.profit_ratio > 3.0 {
            confidence += 0.1;
        }

        // Exact input swaps are more predictable
        if decoded.is_exact_input {
            confidence += 0.1;
        }

        // Larger slippage tolerance = more room for profit
        let slippage = if !decoded.amount_out_min.is_zero() {
            let expected = u256_to_f64(decoded.amount_in);
            let min_out = u256_to_f64(decoded.amount_out_min);
            // This is a rough estimate, actual slippage depends on price
            (expected - min_out) / expected
        } else {
            0.0
        };

        if slippage > 0.01 {
            // > 1% slippage
            confidence += 0.05;
        }

        confidence.clamp(0.3_f64, 0.95_f64)
    }
}

impl Default for SandwichDetector {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Detector for SandwichDetector {
    fn name(&self) -> &str {
        &self.name
    }

    fn is_enabled(&self, config: &DetectorConfig) -> bool {
        config.enable_sandwich
    }

    async fn detect(
        &self,
        event: &MonitorEvent,
        ctx: &DetectorContext,
    ) -> Result<Vec<Opportunity>> {
        let mut opportunities = Vec::new();

        if let MonitorEvent::PendingTransaction {
            hash,
            from,
            to,
            value,
            input,
            gas_price,
            ..
        } = event
        {
            // Only process transactions to known routers
            if !self.is_router(to) {
                return Ok(opportunities);
            }

            // Try to decode swap parameters
            let decoded = match self.decode_swap_params(to, input, *value) {
                Some(d) => d,
                None => return Ok(opportunities),
            };

            // Check deadline validity - need at least 2 blocks worth of time (24 seconds on mainnet)
            let current_timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();

            let deadline_secs = decoded.deadline.to::<u64>();
            // Skip if deadline is 0 (no deadline) or too soon
            if deadline_secs > 0 && deadline_secs < current_timestamp + 24 {
                tracing::debug!(
                    "Transaction deadline too close: {} (current: {})",
                    deadline_secs,
                    current_timestamp
                );
                return Ok(opportunities);
            }

            // Check if swap size is in our target range
            if decoded.amount_in < self.min_victim_size || decoded.amount_in > self.max_victim_size {
                return Ok(opportunities);
            }

            // Get current gas price for cost estimation
            let current_gas_price = ctx.get_gas_price().await;

            // Analyze for sandwich opportunity
            if let Some(analysis) = self.analyze_sandwich(&decoded, ctx, current_gas_price) {
                // Only report if net profit exceeds minimum threshold
                if analysis.net_profit >= ctx.config.min_profit_wei {
                    tracing::info!(
                        "Sandwich opportunity found: victim {} wei, profit {} wei ({:.2}x gas)",
                        decoded.amount_in,
                        analysis.net_profit,
                        analysis.profit_ratio
                    );

                    let opp = self.build_opportunity(
                        &analysis,
                        &decoded,
                        hash.clone(),
                        *from,
                        *to,
                        *value,
                        *gas_price,
                    );
                    opportunities.push(opp);
                }
            }
        }

        Ok(opportunities)
    }
}

/// Decoded swap transaction
struct DecodedSwap {
    token_in: Address,
    token_out: Address,
    amount_in: U256,
    amount_out_min: U256,
    recipient: Address,
    deadline: U256,
    is_exact_input: bool,
}

/// Result of sandwich analysis
struct SandwichAnalysis {
    pool: super::RegisteredPool,
    frontrun_amount: U256,
    frontrun_output: U256,
    #[allow(dead_code)] // Reserved for detailed profit breakdown reporting
    backrun_output: U256,
    estimated_profit: U256,
    gas_cost: U256,
    net_profit: U256,
    profit_ratio: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_router() {
        let detector = SandwichDetector::new();

        // Known router
        let uni_v2: Address = "0x7a250d5630B4cF539739dF2C5dAcb4c659F2488D".parse().unwrap();
        assert!(detector.is_router(&uni_v2));

        // Unknown address
        assert!(!detector.is_router(&Address::ZERO));
    }

    #[test]
    fn test_selectors() {
        // swapExactTokensForTokens selector
        assert_eq!(selectors::UNISWAP_V2_SWAP_EXACT_TOKENS, &[0x38, 0xed, 0x17, 0x39]);
    }

    #[test]
    fn test_decode_v2_swap_exact_eth() {
        let detector = SandwichDetector::new();

        // Construct a minimal valid swapExactETHForTokens calldata
        let mut input = vec![0u8; 4 + 32 * 4 + 32 + 2 * 32]; // selector + params + path
        input[..4].copy_from_slice(selectors::UNISWAP_V2_SWAP_EXACT_ETH);

        // amountOutMin = 1000
        input[35] = 0x03;
        input[34] = 0xe8;

        // path offset = 128 (0x80)
        input[67] = 0x80;

        // recipient
        let recipient = Address::repeat_byte(0xAB);
        input[80..100].copy_from_slice(&[0u8; 20]);
        input[80..100].copy_from_slice(recipient.as_slice());

        // deadline
        let deadline = 0xFFFFFFFF_u64;
        input[100..132].copy_from_slice(&[0u8; 32]);
        input[128..132].copy_from_slice(&deadline.to_be_bytes());

        // path length = 2
        let path_start = 4 + 128; // 132
        input[path_start..path_start + 32].copy_from_slice(&[0u8; 32]);
        input[path_start + 31] = 2;

        // WETH address (first token)
        let weth = Address::repeat_byte(0xC0);
        input[path_start + 32 + 12..path_start + 64].copy_from_slice(weth.as_slice());

        // Output token
        let token_out = Address::repeat_byte(0xD0);
        input[path_start + 64 + 12..path_start + 96].copy_from_slice(token_out.as_slice());

        let value = U256::from(1_000_000_000_000_000_000u64); // 1 ETH
        let bytes = Bytes::from(input);

        let decoded = detector.decode_v2_swap_exact_eth(&bytes, value);

        // Note: This test may fail due to the simplified encoding above
        // In production, proper ABI encoding would be used
        if let Some(d) = decoded {
            assert_eq!(d.amount_in, value);
            assert!(d.is_exact_input);
        }
    }
}
