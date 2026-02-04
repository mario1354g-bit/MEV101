//! Pool Discovery Collector - monitors for new pool deployments (true long-tail MEV)
//!
//! This collector watches for:
//! - New Uniswap V2/V3 pool deployments
//! - New SushiSwap pair creations
//! - Balancer pool registrations
//! - Any new liquidity = potential arbitrage before bots index it

use crate::artemis::{Collector, DexType, Event, NewPoolEvent};
use alloy::primitives::{address, Address, U256};
use alloy::providers::{Provider, ProviderBuilder, WsConnect};
use alloy::rpc::types::Filter;
use alloy::sol;
use alloy::sol_types::SolEvent;
use async_trait::async_trait;
use futures::StreamExt;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

// Factory event signatures
sol! {
    // Uniswap V2 Factory - PairCreated
    #[derive(Debug)]
    event PairCreated(
        address indexed token0,
        address indexed token1,
        address pair,
        uint256 allPairs
    );

    // Uniswap V3 Factory - PoolCreated
    #[derive(Debug)]
    event PoolCreated(
        address indexed token0,
        address indexed token1,
        uint24 indexed fee,
        int24 tickSpacing,
        address pool
    );

    // Balancer V2 - PoolRegistered
    #[derive(Debug)]
    event PoolRegistered(
        bytes32 indexed poolId,
        address indexed poolAddress,
        uint8 specialization
    );
}

/// Known factory addresses
pub mod factories {
    use alloy::primitives::{address, Address};

    // Uniswap
    pub const UNISWAP_V2_FACTORY: Address = address!("5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f");
    pub const UNISWAP_V3_FACTORY: Address = address!("1F98431c8aD98523631AE4a59f267346ea31F984");

    // SushiSwap
    pub const SUSHISWAP_FACTORY: Address = address!("C0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac");

    // Balancer
    pub const BALANCER_VAULT: Address = address!("BA12222222228d8Ba445958a75a0704d566BF2C8");

    // PancakeSwap V3 (yes, it's on mainnet)
    pub const PANCAKE_V3_FACTORY: Address = address!("0BFbCF9fa4f9C56B0F40a671Ad40E0805A091865");

    // Aerodrome/Velodrome style (Base, Optimism)
    pub const AERODROME_FACTORY: Address = address!("420DD381b31aEf6683db6B902084cB0FFECe40Da");
}

/// Pool discovery collector configuration
#[derive(Debug, Clone)]
pub struct PoolDiscoveryConfig {
    pub ws_url: String,
    pub track_uniswap_v2: bool,
    pub track_uniswap_v3: bool,
    pub track_sushiswap: bool,
    pub track_balancer: bool,
    /// Minimum liquidity in ETH to consider (filter dust pools)
    pub min_liquidity_eth: f64,
    /// Known valuable tokens to prioritize
    pub priority_tokens: Vec<Address>,
}

impl PoolDiscoveryConfig {
    pub fn mainnet_aggressive() -> Self {
        Self {
            ws_url: String::new(), // Set by caller
            track_uniswap_v2: true,
            track_uniswap_v3: true,
            track_sushiswap: true,
            track_balancer: true,
            min_liquidity_eth: 0.1, // Very low threshold for long-tail
            priority_tokens: vec![
                address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"), // WETH
                address!("A0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"), // USDC
                address!("dAC17F958D2ee523a2206206994597C13D831ec7"), // USDT
                address!("6B175474E89094C44Da98b954EedeAC495271d0F"), // DAI
                address!("2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599"), // WBTC
            ],
        }
    }
}

/// Pool discovery collector - finds new pools as they're created
pub struct PoolDiscoveryCollector {
    config: PoolDiscoveryConfig,
}

impl PoolDiscoveryCollector {
    pub fn new(config: PoolDiscoveryConfig) -> Self {
        Self { config }
    }

    /// Check if a token pair involves a priority token (WETH, USDC, etc.)
    fn is_priority_pair(&self, token0: Address, token1: Address) -> bool {
        self.config.priority_tokens.contains(&token0)
            || self.config.priority_tokens.contains(&token1)
    }
}

#[async_trait]
impl Collector for PoolDiscoveryCollector {
    fn name(&self) -> &str {
        "PoolDiscoveryCollector"
    }

    async fn collect(&self, sender: mpsc::Sender<Event>) -> eyre::Result<()> {
        let ws = WsConnect::new(&self.config.ws_url);
        let provider = ProviderBuilder::new().on_ws(ws).await?;

        info!("PoolDiscoveryCollector: Connected, watching for new pool deployments");

        // Build list of factories to monitor
        let mut factory_addresses = Vec::new();
        if self.config.track_uniswap_v2 {
            factory_addresses.push(factories::UNISWAP_V2_FACTORY);
        }
        if self.config.track_uniswap_v3 {
            factory_addresses.push(factories::UNISWAP_V3_FACTORY);
        }
        if self.config.track_sushiswap {
            factory_addresses.push(factories::SUSHISWAP_FACTORY);
        }

        // Event signatures
        let pair_created_sig = PairCreated::SIGNATURE_HASH;
        let pool_created_sig = PoolCreated::SIGNATURE_HASH;

        let filter = Filter::new()
            .address(factory_addresses)
            .event_signature(vec![pair_created_sig, pool_created_sig]);

        let mut stream = provider.subscribe_logs(&filter).await?.into_stream();

        info!(
            "PoolDiscoveryCollector: Monitoring {} factories for new pools",
            if self.config.track_uniswap_v2 { 1 } else { 0 }
                + if self.config.track_uniswap_v3 { 1 } else { 0 }
                + if self.config.track_sushiswap { 1 } else { 0 }
        );

        while let Some(log) = stream.next().await {
            let factory = log.address();
            let topics = log.topics();

            if topics.is_empty() {
                continue;
            }

            let sig = topics[0];

            // Uniswap V2 / SushiSwap PairCreated
            if sig == pair_created_sig {
                if let Ok(event) = PairCreated::decode_log_data(&log.data(), true) {
                    let is_priority = self.is_priority_pair(event.token0, event.token1);

                    let dex = if factory == factories::UNISWAP_V2_FACTORY {
                        DexType::UniswapV2
                    } else if factory == factories::SUSHISWAP_FACTORY {
                        DexType::SushiSwap
                    } else {
                        DexType::UniswapV2 // Generic V2 fork
                    };

                    info!(
                        "🆕 NEW POOL: {:?} | {} | tokens: {:?}/{:?} | priority: {}",
                        event.pair,
                        dex,
                        event.token0,
                        event.token1,
                        is_priority
                    );

                    let pool_event = NewPoolEvent {
                        pool: event.pair,
                        dex,
                        token0: event.token0,
                        token1: event.token1,
                        fee_bps: 30, // Standard V2 fee
                        is_priority,
                        block_number: log.block_number.unwrap_or(0),
                    };

                    if sender.send(Event::NewPool(pool_event)).await.is_err() {
                        warn!("PoolDiscoveryCollector: Failed to send event");
                        break;
                    }
                }
            }

            // Uniswap V3 PoolCreated
            if sig == pool_created_sig {
                if let Ok(event) = PoolCreated::decode_log_data(&log.data(), true) {
                    let is_priority = self.is_priority_pair(event.token0, event.token1);

                    // V3 fee is in hundredths of a bip (100 = 0.01%, 500 = 0.05%, 3000 = 0.3%, 10000 = 1%)
                    let fee_raw: u32 = event.fee.try_into().unwrap_or(3000);
                    let fee_bps = fee_raw / 100;

                    info!(
                        "🆕 NEW V3 POOL: {:?} | fee: {}bps | tokens: {:?}/{:?} | priority: {}",
                        event.pool,
                        fee_bps,
                        event.token0,
                        event.token1,
                        is_priority
                    );

                    let pool_event = NewPoolEvent {
                        pool: event.pool,
                        dex: DexType::UniswapV3,
                        token0: event.token0,
                        token1: event.token1,
                        fee_bps,
                        is_priority,
                        block_number: log.block_number.unwrap_or(0),
                    };

                    if sender.send(Event::NewPool(pool_event)).await.is_err() {
                        warn!("PoolDiscoveryCollector: Failed to send event");
                        break;
                    }
                }
            }
        }

        Ok(())
    }
}
