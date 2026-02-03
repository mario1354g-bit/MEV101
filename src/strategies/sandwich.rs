//! Sandwich strategy - detects and creates sandwich opportunities with REVM simulation

use crate::artemis::{
    Action, DexType, Event, PendingTxEvent, SandwichAction, SandwichTx, Strategy,
};
use crate::simulation::swap_simulator::SwapSimulator;
use alloy::consensus::Transaction as TxTrait;
use alloy::primitives::{address, Address, Bytes, U256};
use alloy::providers::{Provider, ProviderBuilder};
use alloy::sol;
use alloy::sol_types::SolCall;
use alloy::transports::http::{Client, Http};
use async_trait::async_trait;
use dashmap::DashMap;
use parking_lot::RwLock;
use std::sync::Arc;
use tracing::{debug, info, warn};

// ABI definitions for Uniswap V2 swap decoding
sol! {
    #[derive(Debug)]
    function swapExactTokensForTokens(
        uint256 amountIn,
        uint256 amountOutMin,
        address[] calldata path,
        address to,
        uint256 deadline
    ) external returns (uint256[] memory amounts);

    #[derive(Debug)]
    function swapExactETHForTokens(
        uint256 amountOutMin,
        address[] calldata path,
        address to,
        uint256 deadline
    ) external payable returns (uint256[] memory amounts);

    #[derive(Debug)]
    function swapExactTokensForETH(
        uint256 amountIn,
        uint256 amountOutMin,
        address[] calldata path,
        address to,
        uint256 deadline
    ) external returns (uint256[] memory amounts);

    #[derive(Debug)]
    function swapExactTokensForTokensSupportingFeeOnTransferTokens(
        uint256 amountIn,
        uint256 amountOutMin,
        address[] calldata path,
        address to,
        uint256 deadline
    ) external;

    #[derive(Debug)]
    function swapExactETHForTokensSupportingFeeOnTransferTokens(
        uint256 amountOutMin,
        address[] calldata path,
        address to,
        uint256 deadline
    ) external payable;

    #[derive(Debug)]
    function swapExactTokensForETHSupportingFeeOnTransferTokens(
        uint256 amountIn,
        uint256 amountOutMin,
        address[] calldata path,
        address to,
        uint256 deadline
    ) external;
}

/// Known DEX router addresses
pub mod routers {
    use alloy::primitives::{address, Address};

    // Uniswap
    pub const UNISWAP_V2_ROUTER: Address =
        address!("7a250d5630B4cF539739dF2C5dAcb4c659F2488D");
    pub const UNISWAP_V3_ROUTER: Address =
        address!("E592427A0AEce92De3Edee1F18E0157C05861564");
    pub const UNISWAP_V3_ROUTER2: Address =
        address!("68b3465833fb72A70ecDF485E0e4C7bD8665Fc45");
    pub const UNISWAP_UNIVERSAL_ROUTER: Address =
        address!("3fC91A3afd70395Cd496C647d5a6CC9D4B2b7FAD");

    // SushiSwap
    pub const SUSHISWAP_ROUTER: Address =
        address!("d9e1cE17f2641f24aE83637ab66a2cca9C378B9F");

    // 1inch
    pub const ONEINCH_V5: Address =
        address!("1111111254EEB25477B68fb85Ed929f73A960582");
    pub const ONEINCH_V6: Address =
        address!("111111125421cA6dc452d289314280a0f8842A65");

    // 0x Protocol
    pub const ZEROX_EXCHANGE_PROXY: Address =
        address!("Def1C0ded9bec7F1a1670819833240f027b25EfF");

    // Paraswap
    pub const PARASWAP_V5: Address =
        address!("DEF171Fe48CF0115B1d80b88dc8eAB59176FEe57");

    // Kyberswap
    pub const KYBERSWAP_ROUTER: Address =
        address!("6131B5fae19EA4f9D964eAc0408E4408b66337b5");

    /// Aggregator addresses to SKIP - they cause simulation hangs due to complex multi-hop routing
    /// These touch dozens of contracts and overwhelm REVM state fetching
    pub fn skip_aggregators() -> Vec<Address> {
        vec![
            ONEINCH_V5,           // 1inch v5 - massive internal calls
            ONEINCH_V6,           // 1inch v6 - massive internal calls
            ZEROX_EXCHANGE_PROXY, // 0x - complex routing
            PARASWAP_V5,          // Paraswap - multi-DEX aggregation
            KYBERSWAP_ROUTER,     // Kyberswap - aggregator
            UNISWAP_UNIVERSAL_ROUTER, // Universal Router - complex batched calls
        ]
    }

    /// Check if address is a skipped aggregator
    pub fn is_skip_aggregator(addr: Address) -> bool {
        skip_aggregators().contains(&addr)
    }

    /// Get all monitored router addresses (excludes aggregators that cause hangs)
    pub fn all_routers() -> Vec<Address> {
        vec![
            UNISWAP_V2_ROUTER,
            UNISWAP_V3_ROUTER,
            UNISWAP_V3_ROUTER2,
            SUSHISWAP_ROUTER,
            // Note: Aggregators removed - they cause simulation hangs
        ]
    }

    /// Get all routers including aggregators (for detection only, not simulation)
    pub fn all_routers_with_aggregators() -> Vec<Address> {
        vec![
            UNISWAP_V2_ROUTER,
            UNISWAP_V3_ROUTER,
            UNISWAP_V3_ROUTER2,
            UNISWAP_UNIVERSAL_ROUTER,
            SUSHISWAP_ROUTER,
            ONEINCH_V5,
            ONEINCH_V6,
            ZEROX_EXCHANGE_PROXY,
            PARASWAP_V5,
            KYBERSWAP_ROUTER,
        ]
    }
}

/// Swap function selectors
pub mod selectors {
    // UniswapV2 Router - Standard swaps
    pub const SWAP_EXACT_TOKENS_FOR_TOKENS: [u8; 4] = [0x38, 0xed, 0x17, 0x39];
    pub const SWAP_TOKENS_FOR_EXACT_TOKENS: [u8; 4] = [0x88, 0x03, 0xdb, 0xee];
    pub const SWAP_EXACT_ETH_FOR_TOKENS: [u8; 4] = [0x7f, 0xf3, 0x6a, 0xb5];
    pub const SWAP_TOKENS_FOR_EXACT_ETH: [u8; 4] = [0x4a, 0x25, 0xd9, 0x4a];
    pub const SWAP_EXACT_TOKENS_FOR_ETH: [u8; 4] = [0x18, 0xcb, 0xaf, 0xe5];
    pub const SWAP_ETH_FOR_EXACT_TOKENS: [u8; 4] = [0xfb, 0x3b, 0xdb, 0x41];

    // UniswapV2 Router - Fee-on-transfer token swaps (VERY COMMON!)
    pub const SWAP_EXACT_TOKENS_FOR_TOKENS_FEE: [u8; 4] = [0x5c, 0x11, 0xd7, 0x95];
    pub const SWAP_EXACT_ETH_FOR_TOKENS_FEE: [u8; 4] = [0xb6, 0xf9, 0xde, 0x95];
    pub const SWAP_EXACT_TOKENS_FOR_ETH_FEE: [u8; 4] = [0x79, 0x1a, 0xc9, 0x47];

    // UniswapV3 Router
    pub const EXACT_INPUT_SINGLE: [u8; 4] = [0x41, 0x4b, 0xf3, 0x89];
    pub const EXACT_INPUT: [u8; 4] = [0xc0, 0x4b, 0x8d, 0x59];
    pub const EXACT_OUTPUT_SINGLE: [u8; 4] = [0xdb, 0x3e, 0x21, 0x98];
    pub const EXACT_OUTPUT: [u8; 4] = [0xf2, 0x8c, 0x02, 0x98];

    // UniswapV3 Router02 (SwapRouter02) - multicall wrapper
    pub const MULTICALL: [u8; 4] = [0xac, 0x96, 0x50, 0xd8];
    pub const MULTICALL_DEADLINE: [u8; 4] = [0x5a, 0xe4, 0x01, 0xdc];

    pub fn is_swap_selector(selector: &[u8]) -> bool {
        if selector.len() < 4 {
            return false;
        }
        let sel: [u8; 4] = [selector[0], selector[1], selector[2], selector[3]];
        matches!(
            sel,
            SWAP_EXACT_TOKENS_FOR_TOKENS
                | SWAP_TOKENS_FOR_EXACT_TOKENS
                | SWAP_EXACT_ETH_FOR_TOKENS
                | SWAP_TOKENS_FOR_EXACT_ETH
                | SWAP_EXACT_TOKENS_FOR_ETH
                | SWAP_ETH_FOR_EXACT_TOKENS
                | SWAP_EXACT_TOKENS_FOR_TOKENS_FEE
                | SWAP_EXACT_ETH_FOR_TOKENS_FEE
                | SWAP_EXACT_TOKENS_FOR_ETH_FEE
                | EXACT_INPUT_SINGLE
                | EXACT_INPUT
                | EXACT_OUTPUT_SINGLE
                | EXACT_OUTPUT
                | MULTICALL
                | MULTICALL_DEADLINE
        )
    }
}

/// Sandwich strategy configuration
#[derive(Debug, Clone)]
pub struct SandwichStrategyConfig {
    pub min_victim_value_eth: f64,
    pub min_profit_eth: f64,
    pub max_frontrun_eth: f64,
    pub target_routers: Vec<Address>,
    /// RPC URL for REVM simulation
    pub rpc_url: String,
    /// Whether to use REVM simulation (vs simple estimate)
    pub use_revm_simulation: bool,
}

impl Default for SandwichStrategyConfig {
    fn default() -> Self {
        Self {
            min_victim_value_eth: 0.05,  // Lowered to see more targets
            min_profit_eth: 0.0005,      // Lowered minimum profit
            max_frontrun_eth: 5.0,
            target_routers: routers::all_routers(),
            rpc_url: String::new(),
            use_revm_simulation: true,
        }
    }
}

/// Result of REVM sandwich simulation
#[derive(Debug, Clone)]
pub struct SandwichSimResult {
    /// Whether simulation succeeded
    pub success: bool,
    /// Amount we get from frontrun
    pub frontrun_output: U256,
    /// Amount we get from backrun
    pub backrun_output: U256,
    /// Net profit in wei (backrun_output - frontrun_input)
    pub net_profit: U256,
    /// Total gas used
    pub total_gas: u64,
    /// Error message if failed
    pub error: Option<String>,
}

/// Detected swap from mempool
#[derive(Debug, Clone)]
pub struct DetectedSwap {
    pub router: Address,
    pub dex: DexType,
    pub token_in: Address,
    pub token_out: Address,
    pub amount_in: U256,
    pub min_amount_out: U256,
    pub gas_price: u128,
    pub value: U256,
}

/// WETH address for profit calculation
const WETH: Address = address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");

/// Sandwich strategy with REVM simulation
pub struct SandwichStrategy {
    config: SandwichStrategyConfig,
    /// HTTP provider for REVM simulation
    provider: Option<Arc<alloy::providers::RootProvider<Http<Client>>>>,
    /// Track pending targets to avoid duplicates
    pending_targets: Arc<DashMap<alloy::primitives::B256, DetectedSwap>>,
    analyzed_count: std::sync::atomic::AtomicU64,
    opportunity_count: std::sync::atomic::AtomicU64,
    simulation_count: std::sync::atomic::AtomicU64,
}

impl SandwichStrategy {
    pub fn new(config: SandwichStrategyConfig) -> Self {
        // Create provider for REVM if RPC URL is provided
        let provider = if !config.rpc_url.is_empty() {
            match config.rpc_url.parse() {
                Ok(url) => {
                    let provider = ProviderBuilder::new().on_http(url);
                    Some(Arc::new(provider))
                }
                Err(e) => {
                    warn!("Failed to create provider for REVM: {}", e);
                    None
                }
            }
        } else {
            None
        };

        Self {
            config,
            provider,
            pending_targets: Arc::new(DashMap::new()),
            analyzed_count: std::sync::atomic::AtomicU64::new(0),
            opportunity_count: std::sync::atomic::AtomicU64::new(0),
            simulation_count: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Decode swap from transaction input
    fn decode_swap(&self, to: Address, input: &[u8], value: U256) -> Option<DetectedSwap> {
        if input.len() < 4 {
            debug!("SandwichStrategy: Input too short ({} bytes)", input.len());
            return None;
        }

        let selector = [input[0], input[1], input[2], input[3]];
        if !selectors::is_swap_selector(input) {
            debug!("SandwichStrategy: Unknown selector {:02x}{:02x}{:02x}{:02x}",
                   selector[0], selector[1], selector[2], selector[3]);
            return None;
        }

        debug!("SandwichStrategy: Recognized swap selector {:02x}{:02x}{:02x}{:02x}",
               selector[0], selector[1], selector[2], selector[3]);

        // Determine DEX type from router
        let dex = if to == routers::UNISWAP_V2_ROUTER {
            DexType::UniswapV2
        } else if to == routers::UNISWAP_V3_ROUTER || to == routers::UNISWAP_V3_ROUTER2 {
            DexType::UniswapV3
        } else if to == routers::SUSHISWAP_ROUTER {
            DexType::SushiSwap
        } else {
            return None;
        };

        // Try to decode the actual swap parameters
        let (amount_in, min_amount_out, token_in, token_out) = self.decode_swap_params(&selector, input, value);

        debug!(
            "SandwichStrategy: Decoded - amount_in: {} wei, token_in: {:?}, token_out: {:?}",
            amount_in, token_in, token_out
        );

        Some(DetectedSwap {
            router: to,
            dex,
            token_in,
            token_out,
            amount_in,
            min_amount_out,
            gas_price: 0,
            value,
        })
    }

    /// Decode swap parameters from calldata
    fn decode_swap_params(&self, selector: &[u8; 4], input: &[u8], value: U256) -> (U256, U256, Address, Address) {
        let bytes = Bytes::from(input.to_vec());

        // swapExactTokensForTokens / swapExactTokensForTokensSupportingFeeOnTransferTokens
        // Both have same ABI: (uint256 amountIn, uint256 amountOutMin, address[] path, address to, uint256 deadline)
        if *selector == selectors::SWAP_EXACT_TOKENS_FOR_TOKENS
            || *selector == selectors::SWAP_EXACT_TOKENS_FOR_TOKENS_FEE
        {
            if let Ok(decoded) = swapExactTokensForTokensCall::abi_decode(&bytes, true) {
                let path = decoded.path;
                let token_in = path.first().copied().unwrap_or(Address::ZERO);
                let token_out = path.last().copied().unwrap_or(Address::ZERO);
                return (decoded.amountIn, decoded.amountOutMin, token_in, token_out);
            }
            // Try fee-on-transfer variant decoder
            if let Ok(decoded) = swapExactTokensForTokensSupportingFeeOnTransferTokensCall::abi_decode(&bytes, true) {
                let path = decoded.path;
                let token_in = path.first().copied().unwrap_or(Address::ZERO);
                let token_out = path.last().copied().unwrap_or(Address::ZERO);
                return (decoded.amountIn, decoded.amountOutMin, token_in, token_out);
            }
        }

        // swapExactETHForTokens / swapExactETHForTokensSupportingFeeOnTransferTokens
        // Both have same ABI: (uint256 amountOutMin, address[] path, address to, uint256 deadline) - value is ETH amount
        if *selector == selectors::SWAP_EXACT_ETH_FOR_TOKENS
            || *selector == selectors::SWAP_EXACT_ETH_FOR_TOKENS_FEE
        {
            if let Ok(decoded) = swapExactETHForTokensCall::abi_decode(&bytes, true) {
                let path = decoded.path;
                let token_in = path.first().copied().unwrap_or(Address::ZERO); // WETH
                let token_out = path.last().copied().unwrap_or(Address::ZERO);
                return (value, decoded.amountOutMin, token_in, token_out);
            }
            if let Ok(decoded) = swapExactETHForTokensSupportingFeeOnTransferTokensCall::abi_decode(&bytes, true) {
                let path = decoded.path;
                let token_in = path.first().copied().unwrap_or(Address::ZERO);
                let token_out = path.last().copied().unwrap_or(Address::ZERO);
                return (value, decoded.amountOutMin, token_in, token_out);
            }
        }

        // swapExactTokensForETH / swapExactTokensForETHSupportingFeeOnTransferTokens
        // Both have same ABI: (uint256 amountIn, uint256 amountOutMin, address[] path, address to, uint256 deadline)
        if *selector == selectors::SWAP_EXACT_TOKENS_FOR_ETH
            || *selector == selectors::SWAP_EXACT_TOKENS_FOR_ETH_FEE
        {
            if let Ok(decoded) = swapExactTokensForETHCall::abi_decode(&bytes, true) {
                let path = decoded.path;
                let token_in = path.first().copied().unwrap_or(Address::ZERO);
                let token_out = path.last().copied().unwrap_or(Address::ZERO); // WETH
                return (decoded.amountIn, decoded.amountOutMin, token_in, token_out);
            }
            if let Ok(decoded) = swapExactTokensForETHSupportingFeeOnTransferTokensCall::abi_decode(&bytes, true) {
                let path = decoded.path;
                let token_in = path.first().copied().unwrap_or(Address::ZERO);
                let token_out = path.last().copied().unwrap_or(Address::ZERO);
                return (decoded.amountIn, decoded.amountOutMin, token_in, token_out);
            }
        }

        // Fallback: try to extract amount from first 32 bytes after selector
        let amount_in = if value > U256::ZERO {
            value
        } else if input.len() >= 36 {
            U256::from_be_slice(&input[4..36])
        } else {
            U256::ZERO
        };

        debug!("SandwichStrategy: Using fallback decoder for selector {:02x}{:02x}{:02x}{:02x}",
               selector[0], selector[1], selector[2], selector[3]);

        (amount_in, U256::ZERO, Address::ZERO, Address::ZERO)
    }

    /// Simulate sandwich attack using REVM to calculate exact profit
    async fn simulate_sandwich(&self, swap: &DetectedSwap, frontrun_amount: U256) -> SandwichSimResult {
        let provider = match &self.provider {
            Some(p) => p,
            None => {
                return SandwichSimResult {
                    success: false,
                    frontrun_output: U256::ZERO,
                    backrun_output: U256::ZERO,
                    net_profit: U256::ZERO,
                    total_gas: 0,
                    error: Some("No provider configured".to_string()),
                };
            }
        };

        // Skip if we don't have valid token addresses
        if swap.token_in == Address::ZERO || swap.token_out == Address::ZERO {
            return SandwichSimResult {
                success: false,
                frontrun_output: U256::ZERO,
                backrun_output: U256::ZERO,
                net_profit: U256::ZERO,
                total_gas: 0,
                error: Some("Invalid token addresses".to_string()),
            };
        }

        self.simulation_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        // Create swap simulator
        let simulator = match SwapSimulator::new(Arc::clone(provider)).await {
            Ok(s) => s,
            Err(e) => {
                return SandwichSimResult {
                    success: false,
                    frontrun_output: U256::ZERO,
                    backrun_output: U256::ZERO,
                    net_profit: U256::ZERO,
                    total_gas: 0,
                    error: Some(format!("Failed to create simulator: {}", e)),
                };
            }
        };

        // Build path for our frontrun (same direction as victim)
        // We buy token_out with token_in before victim
        let frontrun_path = vec![swap.token_in, swap.token_out];

        // Simulate frontrun: We swap token_in -> token_out
        let frontrun_result = match simulator
            .simulate_v2_swap(swap.router, frontrun_amount, frontrun_path)
            .await
        {
            Ok(r) => r,
            Err(e) => {
                return SandwichSimResult {
                    success: false,
                    frontrun_output: U256::ZERO,
                    backrun_output: U256::ZERO,
                    net_profit: U256::ZERO,
                    total_gas: 0,
                    error: Some(format!("Frontrun sim failed: {}", e)),
                };
            }
        };

        if !frontrun_result.success {
            return SandwichSimResult {
                success: false,
                frontrun_output: U256::ZERO,
                backrun_output: U256::ZERO,
                net_profit: U256::ZERO,
                total_gas: frontrun_result.gas_used,
                error: frontrun_result.error,
            };
        }

        let frontrun_output = frontrun_result.amount_out;
        let frontrun_gas = frontrun_result.gas_used;

        // Build path for backrun (reverse direction)
        // We sell token_out for token_in after victim
        let backrun_path = vec![swap.token_out, swap.token_in];

        // Simulate backrun: We swap token_out -> token_in (selling what we bought)
        // Note: In a real sandwich, the pool state would be different after victim tx
        // This is a simplified simulation that doesn't account for victim's impact
        let backrun_result = match simulator
            .simulate_v2_swap(swap.router, frontrun_output, backrun_path)
            .await
        {
            Ok(r) => r,
            Err(e) => {
                return SandwichSimResult {
                    success: false,
                    frontrun_output,
                    backrun_output: U256::ZERO,
                    net_profit: U256::ZERO,
                    total_gas: frontrun_gas,
                    error: Some(format!("Backrun sim failed: {}", e)),
                };
            }
        };

        if !backrun_result.success {
            return SandwichSimResult {
                success: false,
                frontrun_output,
                backrun_output: U256::ZERO,
                net_profit: U256::ZERO,
                total_gas: frontrun_gas + backrun_result.gas_used,
                error: backrun_result.error,
            };
        }

        let backrun_output = backrun_result.amount_out;
        let total_gas = frontrun_gas + backrun_result.gas_used;

        // Calculate profit: backrun_output - frontrun_input
        // If we get back more than we put in, we profit
        let net_profit = if backrun_output > frontrun_amount {
            backrun_output - frontrun_amount
        } else {
            U256::ZERO
        };

        debug!(
            "REVM Sandwich Sim: frontrun {} -> {}, backrun {} -> {}, profit: {}",
            frontrun_amount, frontrun_output, frontrun_output, backrun_output, net_profit
        );

        SandwichSimResult {
            success: true,
            frontrun_output,
            backrun_output,
            net_profit,
            total_gas,
            error: None,
        }
    }

    /// Evaluate if a swap is a good sandwich target (sync version for quick filtering)
    fn evaluate_target_quick(&self, swap: &DetectedSwap) -> bool {
        // Quick filter: only swaps with ETH/WETH as input have reliable value
        if swap.token_in != WETH && swap.token_in != Address::ZERO && swap.value == U256::ZERO {
            return false;
        }

        let value_eth = if swap.token_in == WETH || swap.token_in == Address::ZERO {
            swap.amount_in.try_into().unwrap_or(0u128) as f64 / 1e18
        } else {
            swap.value.try_into().unwrap_or(0u128) as f64 / 1e18
        };

        value_eth >= self.config.min_victim_value_eth
    }

    /// Evaluate if a swap is a good sandwich target with optional REVM simulation
    async fn evaluate_target(&self, swap: &DetectedSwap) -> Option<SandwichAction> {
        // Calculate actual ETH value:
        // - If swapping ETH/WETH -> token: use amount_in
        // - If swapping token -> ETH/WETH: use tx value or skip (can't easily value tokens)
        // - Otherwise: skip (no reliable ETH value)
        let value_eth = if swap.token_in == WETH || swap.token_in == Address::ZERO {
            // Swapping ETH/WETH for tokens - amount_in IS the ETH value
            swap.amount_in.try_into().unwrap_or(0u128) as f64 / 1e18
        } else if swap.value > U256::ZERO {
            // Use tx value if available (for ETH swaps)
            swap.value.try_into().unwrap_or(0u128) as f64 / 1e18
        } else {
            // Token -> token or token -> ETH without value, skip
            debug!("Skipping swap: no reliable ETH value (token_in: {:?})", swap.token_in);
            return None;
        };

        if value_eth < self.config.min_victim_value_eth {
            return None;
        }

        let id = format!("sandwich-{}", chrono::Utc::now().timestamp_millis());

        // Try REVM simulation if enabled and we have valid tokens
        let (expected_profit, frontrun_output) = if self.config.use_revm_simulation
            && self.provider.is_some()
            && swap.token_in != Address::ZERO
            && swap.token_out != Address::ZERO
        {
            // Calculate frontrun amount based on victim size (50% of victim value, capped)
            let frontrun_eth = (value_eth * 0.5).min(self.config.max_frontrun_eth);
            let frontrun_amount = U256::from((frontrun_eth * 1e18) as u128);

            debug!(
                "Simulating: {} | victim: {:.4} ETH | frontrun: {:.4} ETH",
                id, value_eth, frontrun_eth
            );

            let sim_result = self.simulate_sandwich(swap, frontrun_amount).await;

            if sim_result.success && sim_result.net_profit > U256::ZERO {
                let profit_eth = sim_result.net_profit.try_into().unwrap_or(0u128) as f64 / 1e18;

                // Account for gas costs (estimate 300k gas at current gas price)
                let gas_cost_wei = U256::from(300_000u64) * U256::from(swap.gas_price + 2_000_000_000);
                let gas_cost_eth = gas_cost_wei.try_into().unwrap_or(0u128) as f64 / 1e18;
                let net_profit_eth = profit_eth - gas_cost_eth;

                info!(
                    "REVM SIM RESULT: {} | gross: {:.6} ETH | gas: {:.6} ETH | net: {:.6} ETH | frontrun_out: {}",
                    id, profit_eth, gas_cost_eth, net_profit_eth, sim_result.frontrun_output
                );

                if net_profit_eth < self.config.min_profit_eth {
                    debug!("Sandwich {} rejected: net profit {:.6} ETH below threshold", id, net_profit_eth);
                    return None;
                }

                (sim_result.net_profit, sim_result.frontrun_output)
            } else {
                debug!(
                    "REVM simulation failed for {}: {:?}",
                    id,
                    sim_result.error.unwrap_or_else(|| "unknown".to_string())
                );
                // Fall back to estimate
                let estimated_profit_eth = value_eth * 0.005;
                if estimated_profit_eth < self.config.min_profit_eth {
                    return None;
                }
                (U256::from((estimated_profit_eth * 1e18) as u128), U256::ZERO)
            }
        } else {
            // Simple estimate without REVM
            let estimated_profit_eth = value_eth * 0.005; // ~0.5% of victim value
            if estimated_profit_eth < self.config.min_profit_eth {
                return None;
            }
            (U256::from((estimated_profit_eth * 1e18) as u128), U256::ZERO)
        };

        let profit_eth = expected_profit.try_into().unwrap_or(0u128) as f64 / 1e18;

        info!(
            "SANDWICH TARGET: {} | value: {:.4} ETH | profit: {:.6} ETH | tokens: {:?} -> {:?}",
            id, value_eth, profit_eth, swap.token_in, swap.token_out
        );

        self.opportunity_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

        Some(SandwichAction {
            id,
            target_tx: alloy::primitives::B256::ZERO, // Set by caller
            frontrun: SandwichTx {
                pool: Address::ZERO,
                token_in: swap.token_in,
                token_out: swap.token_out,
                amount_in: U256::from((self.config.max_frontrun_eth * 1e18) as u128),
                min_amount_out: frontrun_output,
            },
            backrun: SandwichTx {
                pool: Address::ZERO,
                token_in: swap.token_out,
                token_out: swap.token_in,
                amount_in: frontrun_output,
                min_amount_out: U256::ZERO,
            },
            expected_profit,
            gas_price: swap.gas_price + 1_000_000_000, // +1 gwei
            priority_fee: 2_000_000_000,
        })
    }
}

#[async_trait]
impl Strategy for SandwichStrategy {
    fn name(&self) -> &str {
        "SandwichStrategy"
    }

    async fn process_event(&self, event: &Event) -> eyre::Result<Option<Action>> {
        match event {
            Event::PendingTx(pending) => {
                let count = self.analyzed_count
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

                // Log every 100 txs
                if count % 100 == 0 {
                    debug!("SandwichStrategy: analyzed {} pending txs", count);
                }

                let tx = &pending.tx;
                let tx_hash = *tx.inner.tx_hash();

                // Skip if already seen
                if self.pending_targets.contains_key(&tx_hash) {
                    return Ok(None);
                }

                // Get transaction details
                let to = match tx.inner.to() {
                    Some(addr) => addr,
                    None => return Ok(None), // Contract creation
                };

                // Check if it's to a known router
                let is_router = self.config.target_routers.contains(&to);

                // Log first few non-router txs to see what we're getting
                if !is_router && count < 20 {
                    debug!("SandwichStrategy: Non-router tx to {:?}", to);
                }

                if !is_router {
                    return Ok(None);
                }

                // Skip aggregators - they cause simulation hangs due to complex routing
                if routers::is_skip_aggregator(to) {
                    debug!("SandwichStrategy: Skipping aggregator {:?} (causes simulation hangs)", to);
                    return Ok(None);
                }

                debug!("SandwichStrategy: Found tx to known router {:?}", to);

                let input = tx.inner.input();
                let value = tx.inner.value();

                // Try to decode as swap
                if let Some(mut swap) = self.decode_swap(to, input, value) {
                    // Get gas price
                    swap.gas_price = tx.inner.gas_price().unwrap_or(0);

                    let value_eth = swap.amount_in.try_into().unwrap_or(0u128) as f64 / 1e18;
                    debug!(
                        "SandwichStrategy: Decoded swap - router: {:?}, value: {:.4} ETH, dex: {:?}",
                        to, value_eth, swap.dex
                    );

                    // Store target
                    self.pending_targets.insert(tx_hash, swap.clone());

                    // Evaluate for sandwich (async - uses REVM simulation)
                    if let Some(mut action) = self.evaluate_target(&swap).await {
                        action.target_tx = tx_hash;
                        return Ok(Some(Action::Sandwich(action)));
                    }
                }

                Ok(None)
            }
            Event::NewBlock(_) => {
                // Clean up old targets
                let now = chrono::Utc::now();
                self.pending_targets.retain(|_, _| true); // Would add timestamp check

                let analyzed = self
                    .analyzed_count
                    .load(std::sync::atomic::Ordering::Relaxed);
                let opportunities = self
                    .opportunity_count
                    .load(std::sync::atomic::Ordering::Relaxed);

                if analyzed > 0 && analyzed % 1000 == 0 {
                    info!(
                        "SandwichStrategy: {} txs analyzed, {} opportunities found",
                        analyzed, opportunities
                    );
                }

                Ok(None)
            }
            _ => Ok(None),
        }
    }

    async fn on_start(&self) -> eyre::Result<()> {
        info!(
            "SandwichStrategy started: monitoring {} routers",
            self.config.target_routers.len()
        );
        Ok(())
    }
}
