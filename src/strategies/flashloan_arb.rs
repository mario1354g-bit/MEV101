//! Pure Flashloan Arbitrage Strategy
//!
//! Simple and focused:
//! 1. Use warm cache for instant prices/liquidity
//! 2. Find 2-4 hop routes that profit
//! 3. Execute atomically via flashloan
//! 4. Target $1+ profit per trade
//!
//! No sandwiches. No liquidations. Just clean arbitrage.

use crate::artemis::{Action, ArbitrageAction, DexType, Event, Strategy, SwapStep};
use crate::simulation::warm_simulator::WarmSimulator;
use alloy::primitives::{address, Address, U256};
use async_trait::async_trait;
use dashmap::DashMap;
use std::sync::Arc;
use tracing::{debug, info, warn};

/// Well-known tokens
pub mod tokens {
    use alloy::primitives::{address, Address};

    pub const WETH: Address = address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
    pub const USDC: Address = address!("A0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48");
    pub const USDT: Address = address!("dAC17F958D2ee523a2206206994597C13D831ec7");
    pub const DAI: Address = address!("6B175474E89094C44Da98b954EedeAC495271d0F");
    pub const WBTC: Address = address!("2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599");
    pub const USDE: Address = address!("4c9EDD5852cd905f086C759E8383e09bff1E68B3");
    pub const FRAX: Address = address!("853d955aCEf822Db058eb8505911ED77F175b99e");
    pub const LINK: Address = address!("514910771AF9Ca656af840dff83E8264EcF986CA");
    pub const UNI: Address = address!("1f9840a85d5aF5bf1D1762F925BDADdC4201F984");
    pub const AAVE: Address = address!("7Fc66500c84A76Ad7e9c93437bFc5Ac33E2DDaE9");
    pub const CRV: Address = address!("D533a949740bb3306d119CC777fa900bA034cd52");
    pub const MKR: Address = address!("9f8F72aA9304c8B593d555F12eF6589cC3A579A2");
    pub const SNX: Address = address!("C011a73ee8576Fb46F5E1c5751cA3B9Fe0af2a6F");
    pub const COMP: Address = address!("c00e94Cb662C3520282E6f5717214004A7f26888");
    pub const LDO: Address = address!("5A98FcBEA516Cf06857215779Fd812CA3beF1B32");
    pub const RPL: Address = address!("D33526068D116cE69F19A9ee46F0bd304F21A51f");
    pub const STETH: Address = address!("ae7ab96520DE3A18E5e111B5EaAb095312D7fE84");
    pub const RETH: Address = address!("ae78736Cd615f374D3085123A210448E74Fc6393");
    pub const CBETH: Address = address!("Be9895146f7AF43049ca1c1AE358B0541Ea49704");
    pub const WSTETH: Address = address!("7f39C581F595B53c5cb19bD0b3f8dA6c935E2Ca0");
    pub const PEPE: Address = address!("6982508145454Ce325dDbE47a25d4ec3d2311933");
    pub const SHIB: Address = address!("95aD61b0a150d79219dCF64E1E6Cc01f0B64C4cE");
}

/// Pool with cached reserves
#[derive(Debug, Clone)]
pub struct CachedPool {
    pub address: Address,
    pub dex: DexType,
    pub token0: Address,
    pub token1: Address,
    pub reserve0: U256,
    pub reserve1: U256,
    pub fee_bps: u32,
}

impl CachedPool {
    /// Calculate output amount for a swap
    pub fn get_amount_out(&self, amount_in: U256, token_in: Address) -> Option<U256> {
        let (reserve_in, reserve_out) = if token_in == self.token0 {
            (self.reserve0, self.reserve1)
        } else if token_in == self.token1 {
            (self.reserve1, self.reserve0)
        } else {
            return None;
        };

        if reserve_in == U256::ZERO || reserve_out == U256::ZERO || amount_in == U256::ZERO {
            return None;
        }

        // AMM formula: out = (reserve_out * amount_in * (10000 - fee)) / (reserve_in * 10000 + amount_in * (10000 - fee))
        let fee_factor = U256::from(10000 - self.fee_bps);
        let amount_in_with_fee = amount_in * fee_factor;
        let numerator = reserve_out * amount_in_with_fee;
        let denominator = reserve_in * U256::from(10000) + amount_in_with_fee;

        if denominator == U256::ZERO {
            return None;
        }

        Some(numerator / denominator)
    }

    /// Get the other token in the pair
    pub fn other_token(&self, token: Address) -> Option<Address> {
        if token == self.token0 {
            Some(self.token1)
        } else if token == self.token1 {
            Some(self.token0)
        } else {
            None
        }
    }
}

/// Flashloan arbitrage configuration
#[derive(Debug, Clone)]
pub struct FlashloanArbConfig {
    /// Minimum profit in USD (default: $1)
    pub min_profit_usd: f64,
    /// Maximum hops (2, 3, or 4)
    pub max_hops: usize,
    /// Flashloan amounts to try (in ETH)
    pub flashloan_amounts: Vec<f64>,
    /// Gas price estimate in gwei
    pub gas_price_gwei: u64,
    /// Estimated gas per hop
    pub gas_per_hop: u64,
    /// ETH price in USD for profit calculation
    pub eth_price_usd: f64,
}

impl Default for FlashloanArbConfig {
    fn default() -> Self {
        Self {
            min_profit_usd: 1.0,           // Just $1 profit target
            max_hops: 4,                    // Up to 4 hops
            flashloan_amounts: vec![1.0, 5.0, 10.0, 25.0, 50.0, 100.0], // ETH amounts to try
            gas_price_gwei: 30,
            gas_per_hop: 150_000,           // ~150k gas per swap
            eth_price_usd: 2500.0,          // Update this or fetch dynamically
        }
    }
}

/// Pure flashloan arbitrage strategy
pub struct FlashloanArbStrategy {
    config: FlashloanArbConfig,
    /// Token -> list of pools containing that token
    token_pools: Arc<DashMap<Address, Vec<CachedPool>>>,
    /// All pools
    pools: Arc<DashMap<Address, CachedPool>>,
    /// Opportunity counter
    opportunity_count: std::sync::atomic::AtomicU64,
}

impl FlashloanArbStrategy {
    pub fn new(config: FlashloanArbConfig) -> Self {
        let strategy = Self {
            config,
            token_pools: Arc::new(DashMap::new()),
            pools: Arc::new(DashMap::new()),
            opportunity_count: std::sync::atomic::AtomicU64::new(0),
        };

        // Seed with known high-liquidity pools
        strategy.seed_pools();
        strategy
    }

    /// Seed with major pools
    fn seed_pools(&self) {
        use tokens::*;

        // UniswapV2 pools
        let v2_pools = vec![
            (address!("B4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc"), USDC, WETH, "V2 USDC/WETH"),
            (address!("0d4a11d5EEaaC28EC3F61d100daF4d40471f1852"), WETH, USDT, "V2 WETH/USDT"),
            (address!("A478c2975Ab1Ea89e8196811F51A7B7Ade33eB11"), DAI, WETH, "V2 DAI/WETH"),
            (address!("Bb2b8038a1640196FbE3e38816F3e67Cba72D940"), WBTC, WETH, "V2 WBTC/WETH"),
            (address!("d3d2E2692501A5c9Ca623199D38826e513033a17"), UNI, WETH, "V2 UNI/WETH"),
            (address!("43AE24960e5534731Fc831386c07755A2dc33D47"), SNX, WETH, "V2 SNX/WETH"),
            (address!("a2107FA5B38d9bbd2C461D6EDf11B11A50F6b974"), LINK, WETH, "V2 LINK/WETH"),
            (address!("AE461cA67B15dc8dc81CE7615e0320dA1A9aB8D5"), DAI, USDC, "V2 DAI/USDC"),
            (address!("3041CbD36888bECc7bbCBc0045E3B1f144466f5f"), USDC, USDT, "V2 USDC/USDT"),
        ];

        for (pool, token0, token1, name) in v2_pools {
            self.add_pool(CachedPool {
                address: pool,
                dex: DexType::UniswapV2,
                token0,
                token1,
                reserve0: U256::from(10_000_000_000_000_000_000_000u128), // Placeholder - will be updated
                reserve1: U256::from(10_000_000_000_000_000_000_000u128),
                fee_bps: 30,
            });
            debug!("Seeded {}", name);
        }

        // UniswapV3 pools (using equivalent reserve model)
        let v3_pools = vec![
            (address!("88e6A0c2dDD26FEEb64F039a2c41296FcB3f5640"), USDC, WETH, 5, "V3 USDC/WETH 0.05%"),
            (address!("8ad599c3A0ff1De082011EFDDc58f1908eb6e6D8"), USDC, WETH, 30, "V3 USDC/WETH 0.3%"),
            (address!("4e68Ccd3E89f51C3074ca5072bbAC773960dFa36"), WETH, USDT, 30, "V3 WETH/USDT 0.3%"),
            (address!("11b815efB8f581194ae79006d24E0d814B7697F6"), WETH, USDT, 5, "V3 WETH/USDT 0.05%"),
            (address!("C2e9F25Be6257c210d7Adf0D4Cd6E3E881ba25f8"), DAI, WETH, 30, "V3 DAI/WETH 0.3%"),
            (address!("CBCdF9626bC03E24f779434178A73a0B4bad62eD"), WBTC, WETH, 30, "V3 WBTC/WETH 0.3%"),
            (address!("4585FE77225b41b697C938B018E2Ac67Ac5a20c0"), WBTC, WETH, 5, "V3 WBTC/WETH 0.05%"),
            (address!("1d42064Fc4Beb5F8aAF85F4617AE8b3b5B8Bd801"), UNI, WETH, 30, "V3 UNI/WETH 0.3%"),
            (address!("a6Cc3C2531FdaA6Ae1A3CA84c2855806728693e8"), LINK, WETH, 30, "V3 LINK/WETH 0.3%"),
            (address!("5777d92f208679DB4b9778590Fa3CAB3aC9e2168"), DAI, USDC, 1, "V3 DAI/USDC 0.01%"),
            (address!("6c6Bc977E13Df9b0de53b251522280BB72383700"), DAI, USDC, 5, "V3 DAI/USDC 0.05%"),
            (address!("3416cF6C708Da44DB2624D63ea0AAef7113527C6"), USDC, USDT, 1, "V3 USDC/USDT 0.01%"),
        ];

        for (pool, token0, token1, fee_bps, name) in v3_pools {
            self.add_pool(CachedPool {
                address: pool,
                dex: DexType::UniswapV3,
                token0,
                token1,
                reserve0: U256::from(10_000_000_000_000_000_000_000u128),
                reserve1: U256::from(10_000_000_000_000_000_000_000u128),
                fee_bps,
            });
            debug!("Seeded {}", name);
        }

        // SushiSwap pools
        let sushi_pools = vec![
            (address!("397FF1542f962076d0BFE58eA045FfA2d347ACa0"), USDC, WETH, "Sushi USDC/WETH"),
            (address!("06da0fd433C1A5d7a4faa01111c044910A184553"), USDT, WETH, "Sushi USDT/WETH"),
            (address!("CEFF51756c56CeFFCA006cD410B03FFC46dd3a58"), WBTC, WETH, "Sushi WBTC/WETH"),
            (address!("C3D03e4F041Fd4cD388c549Ee2A29a9E5075882f"), DAI, WETH, "Sushi DAI/WETH"),
        ];

        for (pool, token0, token1, name) in sushi_pools {
            self.add_pool(CachedPool {
                address: pool,
                dex: DexType::SushiSwap,
                token0,
                token1,
                reserve0: U256::from(10_000_000_000_000_000_000_000u128),
                reserve1: U256::from(10_000_000_000_000_000_000_000u128),
                fee_bps: 30,
            });
            debug!("Seeded {}", name);
        }

        info!(
            "FlashloanArbStrategy: Seeded {} pools across {} tokens",
            self.pools.len(),
            self.token_pools.len()
        );
    }

    /// Add a pool to the index
    fn add_pool(&self, pool: CachedPool) {
        self.token_pools
            .entry(pool.token0)
            .or_insert_with(Vec::new)
            .push(pool.clone());
        self.token_pools
            .entry(pool.token1)
            .or_insert_with(Vec::new)
            .push(pool.clone());
        self.pools.insert(pool.address, pool);
    }

    /// Update pool reserves from swap event
    fn update_reserves(&self, pool_addr: Address, reserve0: U256, reserve1: U256) {
        if let Some(mut pool) = self.pools.get_mut(&pool_addr) {
            pool.reserve0 = reserve0;
            pool.reserve1 = reserve1;
        }
    }

    /// Find all profitable routes starting from WETH
    fn find_profitable_routes(&self, input_amount: U256) -> Vec<(Vec<CachedPool>, U256)> {
        let mut profitable_routes = Vec::new();
        let start_token = tokens::WETH;

        // Find 2-hop routes (WETH -> X -> WETH)
        self.find_routes_recursive(
            start_token,
            start_token,
            input_amount,
            input_amount,
            Vec::new(),
            &mut profitable_routes,
            self.config.max_hops,
        );

        // Sort by profit descending
        profitable_routes.sort_by(|a, b| b.1.cmp(&a.1));
        profitable_routes
    }

    fn find_routes_recursive(
        &self,
        current_token: Address,
        target_token: Address,
        current_amount: U256,
        input_amount: U256,
        path: Vec<CachedPool>,
        results: &mut Vec<(Vec<CachedPool>, U256)>,
        remaining_hops: usize,
    ) {
        if remaining_hops == 0 {
            return;
        }

        // Get pools containing current token
        if let Some(pools) = self.token_pools.get(&current_token) {
            for pool in pools.iter() {
                // Skip if we've already used this pool
                if path.iter().any(|p| p.address == pool.address) {
                    continue;
                }

                // Calculate output
                let output = match pool.get_amount_out(current_amount, current_token) {
                    Some(out) if out > U256::ZERO => out,
                    _ => continue,
                };

                let next_token = pool.other_token(current_token).unwrap();
                let mut new_path = path.clone();
                new_path.push(pool.clone());

                // If we've reached target and have at least 2 hops, check profit
                if next_token == target_token && new_path.len() >= 2 {
                    if output > input_amount {
                        let profit = output - input_amount;
                        results.push((new_path.clone(), profit));
                    }
                }

                // Continue searching if we have hops remaining
                if remaining_hops > 1 {
                    self.find_routes_recursive(
                        next_token,
                        target_token,
                        output,
                        input_amount,
                        new_path,
                        results,
                        remaining_hops - 1,
                    );
                }
            }
        }
    }

    /// Calculate gas cost in ETH
    fn estimate_gas_cost(&self, num_hops: usize) -> U256 {
        let gas_used = self.config.gas_per_hop * (num_hops as u64);
        let gas_price_wei = self.config.gas_price_gwei * 1_000_000_000;
        U256::from(gas_used * gas_price_wei)
    }

    /// Check if profit exceeds minimum after gas
    fn is_profitable(&self, profit: U256, num_hops: usize) -> bool {
        let gas_cost = self.estimate_gas_cost(num_hops);

        if profit <= gas_cost {
            return false;
        }

        let net_profit = profit - gas_cost;
        let net_profit_eth = net_profit.try_into().unwrap_or(0u128) as f64 / 1e18;
        let net_profit_usd = net_profit_eth * self.config.eth_price_usd;

        net_profit_usd >= self.config.min_profit_usd
    }

    /// Build arbitrage action from route
    fn build_action(&self, route: &[CachedPool], input_amount: U256, profit: U256) -> ArbitrageAction {
        let id = format!(
            "flasharb-{}-{}",
            route.len(),
            chrono::Utc::now().timestamp_millis()
        );

        let mut path = Vec::new();
        let mut current_token = tokens::WETH;

        for pool in route {
            let token_out = pool.other_token(current_token).unwrap();
            path.push(SwapStep {
                dex: pool.dex,
                pool: pool.address,
                token_in: current_token,
                token_out,
                amount_in: if path.is_empty() { input_amount } else { U256::ZERO },
                min_amount_out: U256::ZERO,
            });
            current_token = token_out;
        }

        let gas_cost = self.estimate_gas_cost(route.len());
        let net_profit = if profit > gas_cost { profit - gas_cost } else { U256::ZERO };

        info!(
            "FLASHLOAN ARB: {} | {} hops | input: {} ETH | gross: {} | gas: {} | net: {} ETH",
            id,
            route.len(),
            input_amount.try_into().unwrap_or(0u128) as f64 / 1e18,
            profit.try_into().unwrap_or(0u128) as f64 / 1e18,
            gas_cost.try_into().unwrap_or(0u128) as f64 / 1e18,
            net_profit.try_into().unwrap_or(0u128) as f64 / 1e18,
        );

        ArbitrageAction {
            id,
            path,
            input_token: tokens::WETH,
            input_amount,
            expected_output: input_amount + profit,
            expected_profit: profit,
            min_profit: net_profit,
            deadline: chrono::Utc::now().timestamp() as u64 + 60,
            use_flashloan: true,
            flashloan_token: Some(tokens::WETH),
            flashloan_amount: Some(input_amount),
            gas_price: (self.config.gas_price_gwei as u128) * 1_000_000_000,
            priority_fee: 2_000_000_000,
        }
    }

    /// Main scan function - find best opportunity
    pub fn scan_opportunities(&self) -> Option<ArbitrageAction> {
        for eth_amount in &self.config.flashloan_amounts {
            let input_amount = U256::from((*eth_amount * 1e18) as u128);

            let routes = self.find_profitable_routes(input_amount);

            for (route, profit) in routes {
                if self.is_profitable(profit, route.len()) {
                    return Some(self.build_action(&route, input_amount, profit));
                }
            }
        }
        None
    }
}

#[async_trait]
impl Strategy for FlashloanArbStrategy {
    fn name(&self) -> &str {
        "FlashloanArbStrategy"
    }

    async fn process_event(&self, event: &Event) -> eyre::Result<Option<Action>> {
        match event {
            // On every new block, scan for opportunities
            Event::NewBlock(block) => {
                debug!("Block {}: Scanning for flashloan arb opportunities", block.block_number);

                if let Some(action) = self.scan_opportunities() {
                    self.opportunity_count
                        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    return Ok(Some(Action::Arbitrage(action)));
                }
                Ok(None)
            }

            // Update reserves on swap events
            Event::Swap(swap) => {
                // Could update reserves here if we had the data
                debug!("Swap on {:?}, price: {}", swap.pool, swap.price);
                Ok(None)
            }

            // On price updates, check for opportunities
            Event::PriceUpdate(_update) => {
                // Quick scan after price changes
                if let Some(action) = self.scan_opportunities() {
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
        info!("FlashloanArbStrategy started");
        info!("  - Min profit: ${}", self.config.min_profit_usd);
        info!("  - Max hops: {}", self.config.max_hops);
        info!("  - Flashloan amounts: {:?} ETH", self.config.flashloan_amounts);
        info!("  - Pools indexed: {}", self.pools.len());
        Ok(())
    }
}
