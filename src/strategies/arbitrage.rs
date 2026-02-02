//! Arbitrage strategy - detects cross-DEX arbitrage opportunities

use crate::artemis::{
    Action, ArbitrageAction, DexType, Event, PriceUpdateEvent, Strategy, SwapEvent, SwapStep,
};
use alloy::primitives::{address, Address, U256};
use async_trait::async_trait;
use dashmap::DashMap;
use std::sync::Arc;
use tracing::{debug, info};

/// Token info
#[derive(Debug, Clone)]
pub struct TokenInfo {
    pub address: Address,
    pub symbol: String,
    pub decimals: u8,
}

/// DEX pair for arbitrage
#[derive(Debug, Clone)]
pub struct DexPair {
    pub pool: Address,
    pub dex: DexType,
    pub token0: TokenInfo,
    pub token1: TokenInfo,
    pub fee_bps: u32, // Fee in basis points (e.g., 30 = 0.3%)
}

/// Arbitrage strategy configuration
#[derive(Debug, Clone)]
pub struct ArbitrageStrategyConfig {
    pub min_profit_bps: u32,      // Minimum profit in basis points
    pub max_input_eth: f64,       // Maximum input in ETH
    pub use_flashloan: bool,      // Use flash loans for capital
    pub flashloan_token: Address, // Token to borrow
    pub pairs: Vec<DexPair>,      // Pairs to monitor
}

impl Default for ArbitrageStrategyConfig {
    fn default() -> Self {
        Self {
            min_profit_bps: 5, // 0.05% minimum
            max_input_eth: 10.0,
            use_flashloan: true,
            flashloan_token: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"), // WETH
            pairs: Vec::new(),
        }
    }
}

/// Arbitrage strategy
pub struct ArbitrageStrategy {
    config: ArbitrageStrategyConfig,
    // Pool address -> latest price
    prices: Arc<DashMap<Address, f64>>,
    // Pair key (token0-token1) -> list of pools
    pair_pools: Arc<DashMap<String, Vec<(Address, DexType, f64)>>>,
    opportunity_count: std::sync::atomic::AtomicU64,
}

impl ArbitrageStrategy {
    pub fn new(config: ArbitrageStrategyConfig) -> Self {
        let pair_pools = Arc::new(DashMap::new());

        // Index pairs by token pair
        for pair in &config.pairs {
            let key = format!(
                "{:?}-{:?}",
                pair.token0.address, pair.token1.address
            );
            pair_pools
                .entry(key)
                .or_insert_with(Vec::new)
                .push((pair.pool, pair.dex, 0.0));
        }

        Self {
            config,
            prices: Arc::new(DashMap::new()),
            pair_pools,
            opportunity_count: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Check for arbitrage between two pools trading the same pair
    fn check_arbitrage(
        &self,
        pool1: Address,
        dex1: DexType,
        price1: f64,
        pool2: Address,
        dex2: DexType,
        price2: f64,
        token0: Address,
        token1: Address,
    ) -> Option<ArbitrageAction> {
        if price1 <= 0.0 || price2 <= 0.0 {
            return None;
        }

        // Calculate spread
        let (higher_price, lower_price, buy_pool, buy_dex, sell_pool, sell_dex) = if price1 > price2
        {
            (price1, price2, pool2, dex2, pool1, dex1)
        } else {
            (price2, price1, pool1, dex1, pool2, dex2)
        };

        let spread_bps = ((higher_price - lower_price) / lower_price * 10000.0) as u32;

        // Check if profitable after fees (~60 bps for two swaps)
        let total_fee_bps = 60; // Approximate
        if spread_bps <= total_fee_bps + self.config.min_profit_bps {
            return None;
        }

        let profit_bps = spread_bps - total_fee_bps;
        let id = format!(
            "arb-{:?}-{:?}-{}",
            buy_pool,
            sell_pool,
            chrono::Utc::now().timestamp_millis()
        );

        // Calculate optimal input amount (simplified)
        let input_amount = U256::from(1_000_000_000_000_000_000u128); // 1 ETH
        let expected_profit = U256::from((profit_bps as u128) * 1_000_000_000_000_000u128 / 10000);

        info!(
            "ARB OPPORTUNITY: {} | spread: {}bps | profit: {}bps | buy@{} sell@{}",
            id, spread_bps, profit_bps, buy_dex, sell_dex
        );

        Some(ArbitrageAction {
            id,
            path: vec![
                SwapStep {
                    dex: buy_dex,
                    pool: buy_pool,
                    token_in: token0,
                    token_out: token1,
                    amount_in: input_amount,
                    min_amount_out: U256::ZERO, // Calculated during execution
                },
                SwapStep {
                    dex: sell_dex,
                    pool: sell_pool,
                    token_in: token1,
                    token_out: token0,
                    amount_in: U256::ZERO, // Output of first swap
                    min_amount_out: input_amount,
                },
            ],
            input_token: token0,
            input_amount,
            expected_output: input_amount + expected_profit,
            expected_profit,
            min_profit: U256::from(self.config.min_profit_bps as u128 * 1_000_000_000_000_000u128 / 10000),
            deadline: chrono::Utc::now().timestamp() as u64 + 120, // 2 minutes
            use_flashloan: self.config.use_flashloan,
            flashloan_token: Some(self.config.flashloan_token),
            flashloan_amount: Some(input_amount),
            gas_price: 30_000_000_000, // 30 gwei
            priority_fee: 2_000_000_000, // 2 gwei
        })
    }
}

#[async_trait]
impl Strategy for ArbitrageStrategy {
    fn name(&self) -> &str {
        "ArbitrageStrategy"
    }

    async fn process_event(&self, event: &Event) -> eyre::Result<Option<Action>> {
        match event {
            Event::Swap(swap) => {
                // Update price cache
                self.prices.insert(swap.pool, swap.price);

                // Find other pools trading same pair
                let key = format!("{:?}-{:?}", swap.token0, swap.token1);
                let reverse_key = format!("{:?}-{:?}", swap.token1, swap.token0);

                // Check against other pools
                if let Some(pools) = self.pair_pools.get(&key) {
                    for (other_pool, other_dex, _) in pools.iter() {
                        if *other_pool == swap.pool {
                            continue;
                        }

                        if let Some(other_price) = self.prices.get(other_pool) {
                            if let Some(action) = self.check_arbitrage(
                                swap.pool,
                                swap.dex,
                                swap.price,
                                *other_pool,
                                *other_dex,
                                *other_price,
                                swap.token0,
                                swap.token1,
                            ) {
                                self.opportunity_count
                                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                return Ok(Some(Action::Arbitrage(action)));
                            }
                        }
                    }
                }

                Ok(None)
            }
            Event::PriceUpdate(update) => {
                self.prices.insert(update.pool, update.price);
                Ok(None)
            }
            _ => Ok(None),
        }
    }

    async fn on_start(&self) -> eyre::Result<()> {
        info!(
            "ArbitrageStrategy started: monitoring {} pairs",
            self.config.pairs.len()
        );
        Ok(())
    }
}
