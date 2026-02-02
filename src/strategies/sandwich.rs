//! Sandwich strategy - detects and creates sandwich opportunities

use crate::artemis::{
    Action, DexType, Event, PendingTxEvent, SandwichAction, SandwichTx, Strategy,
};
use alloy::consensus::Transaction as TxTrait;
use alloy::primitives::{address, Address, U256};
use async_trait::async_trait;
use dashmap::DashMap;
use std::sync::Arc;
use tracing::{debug, info, warn};

/// Known DEX router addresses
pub mod routers {
    use alloy::primitives::{address, Address};

    pub const UNISWAP_V2_ROUTER: Address =
        address!("7a250d5630B4cF539739dF2C5dAcb4c659F2488D");
    pub const UNISWAP_V3_ROUTER: Address =
        address!("E592427A0AEce92De3Edee1F18E0157C05861564");
    pub const UNISWAP_V3_ROUTER2: Address =
        address!("68b3465833fb72A70ecDF485E0e4C7bD8665Fc45");
    pub const SUSHISWAP_ROUTER: Address =
        address!("d9e1cE17f2641f24aE83637ab66a2cca9C378B9F");
    pub const ONEINCH_V5: Address =
        address!("1111111254EEB25477B68fb85Ed929f73A960582");
}

/// Swap function selectors
pub mod selectors {
    // UniswapV2 Router
    pub const SWAP_EXACT_TOKENS_FOR_TOKENS: [u8; 4] = [0x38, 0xed, 0x17, 0x39];
    pub const SWAP_TOKENS_FOR_EXACT_TOKENS: [u8; 4] = [0x88, 0x03, 0xdb, 0xee];
    pub const SWAP_EXACT_ETH_FOR_TOKENS: [u8; 4] = [0x7f, 0xf3, 0x6a, 0xb5];
    pub const SWAP_TOKENS_FOR_EXACT_ETH: [u8; 4] = [0x4a, 0x25, 0xd9, 0x4a];
    pub const SWAP_EXACT_TOKENS_FOR_ETH: [u8; 4] = [0x18, 0xcb, 0xaf, 0xe5];
    pub const SWAP_ETH_FOR_EXACT_TOKENS: [u8; 4] = [0xfb, 0x3b, 0xdb, 0x41];

    // UniswapV3 Router
    pub const EXACT_INPUT_SINGLE: [u8; 4] = [0x41, 0x4b, 0xf3, 0x89];
    pub const EXACT_INPUT: [u8; 4] = [0xc0, 0x4b, 0x8d, 0x59];
    pub const EXACT_OUTPUT_SINGLE: [u8; 4] = [0xdb, 0x3e, 0x21, 0x98];
    pub const EXACT_OUTPUT: [u8; 4] = [0xf2, 0x8c, 0x02, 0x98];

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
                | EXACT_INPUT_SINGLE
                | EXACT_INPUT
                | EXACT_OUTPUT_SINGLE
                | EXACT_OUTPUT
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
}

impl Default for SandwichStrategyConfig {
    fn default() -> Self {
        Self {
            min_victim_value_eth: 0.5,
            min_profit_eth: 0.01,
            max_frontrun_eth: 5.0,
            target_routers: vec![
                routers::UNISWAP_V2_ROUTER,
                routers::UNISWAP_V3_ROUTER,
                routers::UNISWAP_V3_ROUTER2,
                routers::SUSHISWAP_ROUTER,
            ],
        }
    }
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

/// Sandwich strategy
pub struct SandwichStrategy {
    config: SandwichStrategyConfig,
    // Track pending targets to avoid duplicates
    pending_targets: Arc<DashMap<alloy::primitives::B256, DetectedSwap>>,
    analyzed_count: std::sync::atomic::AtomicU64,
    opportunity_count: std::sync::atomic::AtomicU64,
}

impl SandwichStrategy {
    pub fn new(config: SandwichStrategyConfig) -> Self {
        Self {
            config,
            pending_targets: Arc::new(DashMap::new()),
            analyzed_count: std::sync::atomic::AtomicU64::new(0),
            opportunity_count: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Decode swap from transaction input
    fn decode_swap(&self, to: Address, input: &[u8], value: U256) -> Option<DetectedSwap> {
        if input.len() < 4 {
            return None;
        }

        if !selectors::is_swap_selector(input) {
            return None;
        }

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

        // Simplified decoding - just detect it's a swap
        // Full decoding would parse the ABI-encoded parameters
        let amount_in = if value > U256::ZERO {
            value
        } else if input.len() >= 36 {
            // First param is usually amountIn
            U256::from_be_slice(&input[4..36])
        } else {
            U256::ZERO
        };

        Some(DetectedSwap {
            router: to,
            dex,
            token_in: Address::ZERO,  // Would need full decode
            token_out: Address::ZERO, // Would need full decode
            amount_in,
            min_amount_out: U256::ZERO,
            gas_price: 0,
            value,
        })
    }

    /// Evaluate if a swap is a good sandwich target
    fn evaluate_target(&self, swap: &DetectedSwap) -> Option<SandwichAction> {
        // Check minimum value
        let value_eth = swap.amount_in.try_into().unwrap_or(0u128) as f64 / 1e18;
        if value_eth < self.config.min_victim_value_eth {
            return None;
        }

        // Calculate potential profit (simplified - real calculation needs pool state)
        // Profit depends on: victim size, pool liquidity, slippage tolerance
        let estimated_profit_eth = value_eth * 0.005; // ~0.5% of victim value

        if estimated_profit_eth < self.config.min_profit_eth {
            return None;
        }

        let id = format!("sandwich-{}", chrono::Utc::now().timestamp_millis());

        info!(
            "SANDWICH TARGET: {} | value: {:.4} ETH | est profit: {:.4} ETH | router: {:?}",
            id, value_eth, estimated_profit_eth, swap.router
        );

        // Would need pool address and token info from full decode
        Some(SandwichAction {
            id,
            target_tx: alloy::primitives::B256::ZERO, // Set by caller
            frontrun: SandwichTx {
                pool: Address::ZERO,
                token_in: swap.token_in,
                token_out: swap.token_out,
                amount_in: U256::from((self.config.max_frontrun_eth * 1e18) as u128),
                min_amount_out: U256::ZERO,
            },
            backrun: SandwichTx {
                pool: Address::ZERO,
                token_in: swap.token_out,
                token_out: swap.token_in,
                amount_in: U256::ZERO, // Output of frontrun
                min_amount_out: U256::ZERO,
            },
            expected_profit: U256::from((estimated_profit_eth * 1e18) as u128),
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
                self.analyzed_count
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

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
                if !self.config.target_routers.contains(&to) {
                    return Ok(None);
                }

                let input = tx.inner.input();
                let value = tx.inner.value();

                // Try to decode as swap
                if let Some(mut swap) = self.decode_swap(to, input, value) {
                    // Get gas price
                    swap.gas_price = tx.inner.gas_price().unwrap_or(0);

                    // Store target
                    self.pending_targets.insert(tx_hash, swap.clone());

                    // Evaluate for sandwich
                    if let Some(mut action) = self.evaluate_target(&swap) {
                        action.target_tx = tx_hash;
                        self.opportunity_count
                            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
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
