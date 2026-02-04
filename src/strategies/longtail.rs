//! Long-tail arbitrage strategy - finds opportunities the big players miss
//!
//! This strategy focuses on:
//! 1. NEW pool deployments (arbitrage before bots index them)
//! 2. Multi-hop routes (A->B->C->A) that aren't obvious
//! 3. Obscure token pairs with thin liquidity
//! 4. Cross-DEX inefficiencies on less monitored venues

use crate::artemis::{
    Action, ArbitrageAction, DexType, Event, NewPoolEvent, Strategy, SwapEvent, SwapStep,
};
use alloy::primitives::{address, Address, U256};
use alloy::providers::{Provider, ProviderBuilder};
use alloy::sol;
use alloy::sol_types::SolCall;
use async_trait::async_trait;
use dashmap::DashMap;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use tracing::{debug, info, warn};

// ABI for reading pool reserves
sol! {
    function getReserves() external view returns (uint112 reserve0, uint112 reserve1, uint32 blockTimestampLast);
    function token0() external view returns (address);
    function token1() external view returns (address);
    function slot0() external view returns (uint160 sqrtPriceX96, int24 tick, uint16 observationIndex, uint16 observationCardinality, uint16 observationCardinalityNext, uint8 feeProtocol, bool unlocked);
}

/// Known token addresses for reference
pub mod tokens {
    use alloy::primitives::{address, Address};

    pub const WETH: Address = address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
    pub const USDC: Address = address!("A0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48");
    pub const USDT: Address = address!("dAC17F958D2ee523a2206206994597C13D831ec7");
    pub const DAI: Address = address!("6B175474E89094C44Da98b954EedeAC495271d0F");
    pub const WBTC: Address = address!("2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599");
}

/// Pool info for the liquidity graph
#[derive(Debug, Clone)]
pub struct PoolNode {
    pub pool: Address,
    pub dex: DexType,
    pub token0: Address,
    pub token1: Address,
    pub fee_bps: u32,
    pub reserve0: U256,
    pub reserve1: U256,
    pub last_updated: u64,
}

/// Long-tail strategy configuration
#[derive(Debug, Clone)]
pub struct LongTailStrategyConfig {
    pub rpc_url: String,
    /// Minimum profit in basis points to consider
    pub min_profit_bps: u32,
    /// Maximum hops in a route (2 = A->B->A, 3 = A->B->C->A)
    pub max_hops: usize,
    /// Minimum liquidity in wei to consider a pool
    pub min_liquidity_wei: U256,
    /// Maximum input amount in ETH
    pub max_input_eth: f64,
    /// Use flashloan for capital
    pub use_flashloan: bool,
}

impl Default for LongTailStrategyConfig {
    fn default() -> Self {
        Self {
            rpc_url: String::new(),
            min_profit_bps: 10, // 0.1% minimum (lower threshold for long-tail)
            max_hops: 3,        // Up to 3-hop routes
            min_liquidity_wei: U256::from(100_000_000_000_000_000u128), // 0.1 ETH
            max_input_eth: 5.0, // Conservative for long-tail
            use_flashloan: true,
        }
    }
}

/// Long-tail arbitrage strategy
pub struct LongTailStrategy {
    config: LongTailStrategyConfig,
    /// Token -> list of pools containing that token
    token_pools: Arc<DashMap<Address, Vec<PoolNode>>>,
    /// Pool address -> pool info
    pools: Arc<DashMap<Address, PoolNode>>,
    /// Recently discovered pools (for priority processing)
    new_pools: Arc<DashMap<Address, NewPoolEvent>>,
    /// Opportunity counter
    opportunity_count: std::sync::atomic::AtomicU64,
}

impl LongTailStrategy {
    pub fn new(config: LongTailStrategyConfig) -> Self {
        Self {
            config,
            token_pools: Arc::new(DashMap::new()),
            pools: Arc::new(DashMap::new()),
            new_pools: Arc::new(DashMap::new()),
            opportunity_count: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Add a pool to the liquidity graph
    fn add_pool(&self, pool: PoolNode) {
        // Add to token -> pools mapping
        self.token_pools
            .entry(pool.token0)
            .or_insert_with(Vec::new)
            .push(pool.clone());
        self.token_pools
            .entry(pool.token1)
            .or_insert_with(Vec::new)
            .push(pool.clone());

        // Add to pool lookup
        self.pools.insert(pool.pool, pool);
    }

    /// Find arbitrage routes starting and ending with the given token
    fn find_routes(&self, start_token: Address, max_hops: usize) -> Vec<Vec<PoolNode>> {
        let mut routes = Vec::new();
        let mut visited = HashSet::new();

        self.find_routes_recursive(
            start_token,
            start_token,
            Vec::new(),
            &mut visited,
            max_hops,
            &mut routes,
        );

        routes
    }

    fn find_routes_recursive(
        &self,
        current_token: Address,
        target_token: Address,
        path: Vec<PoolNode>,
        visited: &mut HashSet<Address>,
        remaining_hops: usize,
        routes: &mut Vec<Vec<PoolNode>>,
    ) {
        if remaining_hops == 0 {
            return;
        }

        // Get pools containing current token
        if let Some(pools) = self.token_pools.get(&current_token) {
            for pool in pools.iter() {
                // Skip if we've visited this pool
                if visited.contains(&pool.pool) {
                    continue;
                }

                // Get the other token in the pair
                let next_token = if pool.token0 == current_token {
                    pool.token1
                } else {
                    pool.token0
                };

                let mut new_path = path.clone();
                new_path.push(pool.clone());

                // If we've reached the target and have at least 2 hops, this is a valid route
                if next_token == target_token && new_path.len() >= 2 {
                    routes.push(new_path.clone());
                }

                // Continue searching if we have hops remaining
                if remaining_hops > 1 {
                    visited.insert(pool.pool);
                    self.find_routes_recursive(
                        next_token,
                        target_token,
                        new_path,
                        visited,
                        remaining_hops - 1,
                        routes,
                    );
                    visited.remove(&pool.pool);
                }
            }
        }
    }

    /// Calculate profit for a route
    fn calculate_route_profit(&self, route: &[PoolNode], input_amount: U256) -> Option<(U256, U256)> {
        if route.is_empty() {
            return None;
        }

        let mut current_amount = input_amount;
        let start_token = route[0].token0; // Assuming we start with token0

        let mut current_token = start_token;

        for pool in route {
            // Determine swap direction
            let (reserve_in, reserve_out) = if current_token == pool.token0 {
                (pool.reserve0, pool.reserve1)
            } else {
                (pool.reserve1, pool.reserve0)
            };

            if reserve_in == U256::ZERO || reserve_out == U256::ZERO {
                return None;
            }

            // Calculate output using constant product formula with fee
            // output = (reserve_out * amount_in * (10000 - fee_bps)) / (reserve_in * 10000 + amount_in * (10000 - fee_bps))
            let fee_multiplier = U256::from(10000 - pool.fee_bps);
            let amount_in_with_fee = current_amount * fee_multiplier;
            let numerator = reserve_out * amount_in_with_fee;
            let denominator = reserve_in * U256::from(10000) + amount_in_with_fee;

            if denominator == U256::ZERO {
                return None;
            }

            current_amount = numerator / denominator;

            // Update current token
            current_token = if current_token == pool.token0 {
                pool.token1
            } else {
                pool.token0
            };
        }

        // Calculate profit (output - input)
        if current_amount > input_amount {
            Some((current_amount, current_amount - input_amount))
        } else {
            None
        }
    }

    /// Process a new pool deployment - immediate opportunity check
    async fn process_new_pool(&self, event: &NewPoolEvent) -> Option<ArbitrageAction> {
        info!(
            "LongTail: Processing new pool {:?} ({}/{})",
            event.pool, event.token0, event.token1
        );

        // Add pool to graph with zero reserves (will be updated)
        let pool_node = PoolNode {
            pool: event.pool,
            dex: event.dex,
            token0: event.token0,
            token1: event.token1,
            fee_bps: event.fee_bps,
            reserve0: U256::ZERO,
            reserve1: U256::ZERO,
            last_updated: event.block_number,
        };

        self.add_pool(pool_node);
        self.new_pools.insert(event.pool, event.clone());

        // If this involves WETH/USDC/USDT, look for immediate arbitrage
        if event.is_priority {
            // Find routes through this new pool
            let routes = self.find_routes(event.token0, self.config.max_hops);

            for route in routes {
                // Skip if route doesn't include our new pool
                if !route.iter().any(|p| p.pool == event.pool) {
                    continue;
                }

                // Try different input amounts
                for input_eth in [0.1, 0.5, 1.0, 2.0] {
                    let input_amount = U256::from((input_eth * 1e18) as u128);

                    if let Some((output, profit)) = self.calculate_route_profit(&route, input_amount)
                    {
                        let profit_bps =
                            ((profit * U256::from(10000)) / input_amount).try_into().unwrap_or(0u32);

                        if profit_bps >= self.config.min_profit_bps {
                            let id = format!(
                                "longtail-newpool-{:?}-{}",
                                event.pool,
                                chrono::Utc::now().timestamp_millis()
                            );

                            info!(
                                "LONG-TAIL OPPORTUNITY: {} | new pool {:?} | profit: {}bps | route: {} hops",
                                id, event.pool, profit_bps, route.len()
                            );

                            return Some(self.build_arbitrage_action(id, &route, input_amount, profit));
                        }
                    }
                }
            }
        }

        None
    }

    /// Process a swap event - check for cross-DEX arbitrage
    async fn process_swap(&self, swap: &SwapEvent) -> Option<ArbitrageAction> {
        // Update pool reserves if we know this pool
        if let Some(mut pool) = self.pools.get_mut(&swap.pool) {
            pool.last_updated = swap.block_number;
        }

        // Look for multi-hop arbitrage opportunities
        let routes = self.find_routes(swap.token0, self.config.max_hops);

        for route in routes {
            // Try different input amounts
            for input_eth in [0.5, 1.0, 2.0, 5.0] {
                let input_amount = U256::from((input_eth * 1e18) as u128);

                if let Some((_, profit)) = self.calculate_route_profit(&route, input_amount) {
                    let profit_bps =
                        ((profit * U256::from(10000)) / input_amount).try_into().unwrap_or(0u32);

                    if profit_bps >= self.config.min_profit_bps {
                        let id = format!(
                            "longtail-multihop-{:?}-{}",
                            swap.pool,
                            chrono::Utc::now().timestamp_millis()
                        );

                        info!(
                            "LONG-TAIL MULTI-HOP: {} | profit: {}bps | route: {} hops",
                            id, profit_bps, route.len()
                        );

                        return Some(self.build_arbitrage_action(id, &route, input_amount, profit));
                    }
                }
            }
        }

        None
    }

    /// Build an arbitrage action from a route
    fn build_arbitrage_action(
        &self,
        id: String,
        route: &[PoolNode],
        input_amount: U256,
        expected_profit: U256,
    ) -> ArbitrageAction {
        let mut path = Vec::new();
        let start_token = route[0].token0;
        let mut current_token = start_token;

        for pool in route {
            let token_out = if current_token == pool.token0 {
                pool.token1
            } else {
                pool.token0
            };

            path.push(SwapStep {
                dex: pool.dex,
                pool: pool.pool,
                token_in: current_token,
                token_out,
                amount_in: if path.is_empty() { input_amount } else { U256::ZERO },
                min_amount_out: U256::ZERO,
            });

            current_token = token_out;
        }

        ArbitrageAction {
            id,
            path,
            input_token: start_token,
            input_amount,
            expected_output: input_amount + expected_profit,
            expected_profit,
            min_profit: U256::from(self.config.min_profit_bps as u128 * 1_000_000_000_000_000u128 / 10000),
            deadline: chrono::Utc::now().timestamp() as u64 + 60, // 1 minute (fast for long-tail)
            use_flashloan: self.config.use_flashloan,
            flashloan_token: Some(tokens::WETH),
            flashloan_amount: Some(input_amount),
            gas_price: 30_000_000_000,
            priority_fee: 2_000_000_000,
        }
    }

    /// Seed initial pools for the graph
    pub async fn seed_known_pools(&self) -> eyre::Result<()> {
        info!("LongTailStrategy: Seeding known high-volume pools");

        // Seed with major V2 pools
        let v2_pools = vec![
            (address!("B4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc"), tokens::USDC, tokens::WETH, "USDC/WETH"),
            (address!("0d4a11d5EEaaC28EC3F61d100daF4d40471f1852"), tokens::WETH, tokens::USDT, "WETH/USDT"),
            (address!("A478c2975Ab1Ea89e8196811F51A7B7Ade33eB11"), tokens::DAI, tokens::WETH, "DAI/WETH"),
            (address!("Bb2b8038a1640196FbE3e38816F3e67Cba72D940"), tokens::WBTC, tokens::WETH, "WBTC/WETH"),
        ];

        for (pool, token0, token1, name) in v2_pools {
            let node = PoolNode {
                pool,
                dex: DexType::UniswapV2,
                token0,
                token1,
                fee_bps: 30,
                reserve0: U256::from(1000000000000000000000u128), // Placeholder
                reserve1: U256::from(1000000000000000000000u128),
                last_updated: 0,
            };
            self.add_pool(node);
            debug!("Seeded pool: {}", name);
        }

        // Seed with major V3 pools
        let v3_pools = vec![
            (address!("88e6A0c2dDD26FEEb64F039a2c41296FcB3f5640"), tokens::USDC, tokens::WETH, 5, "USDC/WETH 0.05%"),
            (address!("8ad599c3A0ff1De082011EFDDc58f1908eb6e6D8"), tokens::USDC, tokens::WETH, 30, "USDC/WETH 0.3%"),
            (address!("4e68Ccd3E89f51C3074ca5072bbAC773960dFa36"), tokens::WETH, tokens::USDT, 30, "WETH/USDT 0.3%"),
            (address!("11b815efB8f581194ae79006d24E0d814B7697F6"), tokens::WETH, tokens::USDT, 5, "WETH/USDT 0.05%"),
        ];

        for (pool, token0, token1, fee_bps, name) in v3_pools {
            let node = PoolNode {
                pool,
                dex: DexType::UniswapV3,
                token0,
                token1,
                fee_bps,
                reserve0: U256::from(1000000000000000000000u128),
                reserve1: U256::from(1000000000000000000000u128),
                last_updated: 0,
            };
            self.add_pool(node);
            debug!("Seeded pool: {}", name);
        }

        // Seed SushiSwap pools
        let sushi_pools = vec![
            (address!("397FF1542f962076d0BFE58eA045FfA2d347ACa0"), tokens::USDC, tokens::WETH, "Sushi USDC/WETH"),
            (address!("06da0fd433C1A5d7a4faa01111c044910A184553"), tokens::USDT, tokens::WETH, "Sushi USDT/WETH"),
        ];

        for (pool, token0, token1, name) in sushi_pools {
            let node = PoolNode {
                pool,
                dex: DexType::SushiSwap,
                token0,
                token1,
                fee_bps: 30,
                reserve0: U256::from(1000000000000000000000u128),
                reserve1: U256::from(1000000000000000000000u128),
                last_updated: 0,
            };
            self.add_pool(node);
            debug!("Seeded pool: {}", name);
        }

        info!(
            "LongTailStrategy: Seeded {} pools, {} unique tokens",
            self.pools.len(),
            self.token_pools.len()
        );

        Ok(())
    }
}

#[async_trait]
impl Strategy for LongTailStrategy {
    fn name(&self) -> &str {
        "LongTailStrategy"
    }

    async fn process_event(&self, event: &Event) -> eyre::Result<Option<Action>> {
        match event {
            // NEW POOL - highest priority, immediate opportunity
            Event::NewPool(pool_event) => {
                if let Some(action) = self.process_new_pool(pool_event).await {
                    self.opportunity_count
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    return Ok(Some(Action::Arbitrage(action)));
                }
                Ok(None)
            }

            // SWAP - check for multi-hop opportunities
            Event::Swap(swap) => {
                if let Some(action) = self.process_swap(swap).await {
                    self.opportunity_count
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    return Ok(Some(Action::Arbitrage(action)));
                }
                Ok(None)
            }

            _ => Ok(None),
        }
    }

    async fn on_start(&self) -> eyre::Result<()> {
        info!("LongTailStrategy started: focusing on new pools and multi-hop routes");

        // Seed known pools
        self.seed_known_pools().await?;

        info!(
            "LongTailStrategy: Graph initialized with {} pools",
            self.pools.len()
        );

        Ok(())
    }
}
