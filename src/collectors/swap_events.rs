//! Swap event collector - monitors DEX swap events

use crate::artemis::{Collector, DexType, Event, PriceUpdateEvent, SwapEvent};
use alloy::primitives::{address, Address, B256, U256};
use alloy::providers::{Provider, ProviderBuilder, WsConnect};
use alloy::rpc::types::Filter;
use alloy::sol;
use alloy::sol_types::SolEvent;
use async_trait::async_trait;
use futures::StreamExt;
use std::collections::HashMap;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

// Swap event signatures
sol! {
    #[derive(Debug)]
    event UniV2Swap(
        address indexed sender,
        uint256 amount0In,
        uint256 amount1In,
        uint256 amount0Out,
        uint256 amount1Out,
        address indexed to
    );

    #[derive(Debug)]
    event UniV3Swap(
        address indexed sender,
        address indexed recipient,
        int256 amount0,
        int256 amount1,
        uint160 sqrtPriceX96,
        uint128 liquidity,
        int24 tick
    );

    #[derive(Debug)]
    event CurveExchange(
        address indexed buyer,
        int128 sold_id,
        uint256 tokens_sold,
        int128 bought_id,
        uint256 tokens_bought
    );
}

/// Monitored pools with metadata
#[derive(Debug, Clone)]
pub struct PoolInfo {
    pub address: Address,
    pub dex: DexType,
    pub token0: Address,
    pub token1: Address,
    pub name: String,
}

/// Swap event collector configuration
#[derive(Debug, Clone)]
pub struct SwapEventCollectorConfig {
    pub ws_url: String,
    pub pools: Vec<PoolInfo>,
}

impl SwapEventCollectorConfig {
    pub fn mainnet_defaults(ws_url: String) -> Self {
        let pools = vec![
            // UniswapV2 pools
            PoolInfo {
                address: address!("B4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc"),
                dex: DexType::UniswapV2,
                token0: address!("A0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"), // USDC
                token1: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"), // WETH
                name: "UniV2 USDC/WETH".to_string(),
            },
            PoolInfo {
                address: address!("0d4a11d5EEaaC28EC3F61d100daF4d40471f1852"),
                dex: DexType::UniswapV2,
                token0: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"), // WETH
                token1: address!("dAC17F958D2ee523a2206206994597C13D831ec7"), // USDT
                name: "UniV2 WETH/USDT".to_string(),
            },
            // UniswapV3 pools
            PoolInfo {
                address: address!("88e6A0c2dDD26FEEb64F039a2c41296FcB3f5640"),
                dex: DexType::UniswapV3,
                token0: address!("A0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"), // USDC
                token1: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"), // WETH
                name: "UniV3 USDC/WETH 0.05%".to_string(),
            },
            PoolInfo {
                address: address!("4e68Ccd3E89f51C3074ca5072bbAC773960dFa36"),
                dex: DexType::UniswapV3,
                token0: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"), // WETH
                token1: address!("dAC17F958D2ee523a2206206994597C13D831ec7"), // USDT
                name: "UniV3 WETH/USDT 0.3%".to_string(),
            },
            PoolInfo {
                address: address!("11b815efB8f581194ae79006d24E0d814B7697F6"),
                dex: DexType::UniswapV3,
                token0: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"), // WETH
                token1: address!("dAC17F958D2ee523a2206206994597C13D831ec7"), // USDT
                name: "UniV3 WETH/USDT 0.05%".to_string(),
            },
            PoolInfo {
                address: address!("Cbcdf9626bC03E24f779434178A73a0B4bad62eD"),
                dex: DexType::UniswapV3,
                token0: address!("2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599"), // WBTC
                token1: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"), // WETH
                name: "UniV3 WBTC/WETH 0.3%".to_string(),
            },
            // SushiSwap pools
            PoolInfo {
                address: address!("397FF1542f962076d0BFE58eA045FfA2d347ACa0"),
                dex: DexType::SushiSwap,
                token0: address!("A0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"), // USDC
                token1: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"), // WETH
                name: "Sushi USDC/WETH".to_string(),
            },
            // Curve pools
            PoolInfo {
                address: address!("bEbc44782C7dB0a1A60Cb6fe97d0b483032FF1C7"),
                dex: DexType::Curve,
                token0: address!("6B175474E89094C44Da98b954EedeAC495271d0F"), // DAI
                token1: address!("A0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"), // USDC
                name: "Curve 3pool".to_string(),
            },
        ];

        Self { ws_url, pools }
    }
}

/// Swap event collector
pub struct SwapEventCollector {
    config: SwapEventCollectorConfig,
}

impl SwapEventCollector {
    pub fn new(config: SwapEventCollectorConfig) -> Self {
        Self { config }
    }
}

#[async_trait]
impl Collector for SwapEventCollector {
    fn name(&self) -> &str {
        "SwapEventCollector"
    }

    async fn collect(&self, sender: mpsc::Sender<Event>) -> eyre::Result<()> {
        let ws = WsConnect::new(&self.config.ws_url);
        let provider = ProviderBuilder::new().on_ws(ws).await?;

        info!(
            "SwapEventCollector: Connected, monitoring {} pools",
            self.config.pools.len()
        );

        // Build pool lookup map
        let pool_map: HashMap<Address, &PoolInfo> =
            self.config.pools.iter().map(|p| (p.address, p)).collect();

        // Build filter for all monitored pools
        let pool_addresses: Vec<Address> = self.config.pools.iter().map(|p| p.address).collect();

        // UniswapV2/Sushi Swap signature
        let univ2_swap_sig = UniV2Swap::SIGNATURE_HASH;
        // UniswapV3 Swap signature
        let univ3_swap_sig = UniV3Swap::SIGNATURE_HASH;

        let filter = Filter::new()
            .address(pool_addresses)
            .event_signature(vec![univ2_swap_sig, univ3_swap_sig]);

        let sub = provider.subscribe_logs(&filter).await?;
        let mut stream = sub.into_stream();

        let mut event_count = 0u64;

        while let Some(log) = stream.next().await {
            event_count += 1;

            let pool_addr = log.address();
            let pool_info = match pool_map.get(&pool_addr) {
                Some(info) => *info,
                None => continue,
            };

            let block_number = log.block_number.unwrap_or(0);
            let tx_hash = log.transaction_hash.unwrap_or(B256::ZERO);

            // Parse based on DEX type
            let (amount0, amount1, price) = match pool_info.dex {
                DexType::UniswapV2 | DexType::SushiSwap => {
                    if let Ok(swap) = log.log_decode::<UniV2Swap>() {
                        let data = swap.inner.data;
                        let a0_in: i128 = data.amount0In.try_into().unwrap_or(0);
                        let a1_in: i128 = data.amount1In.try_into().unwrap_or(0);
                        let a0_out: i128 = data.amount0Out.try_into().unwrap_or(0);
                        let a1_out: i128 = data.amount1Out.try_into().unwrap_or(0);

                        let amount0 = a0_in - a0_out;
                        let amount1 = a1_in - a1_out;

                        let price = if amount0 != 0 {
                            (amount1.abs() as f64) / (amount0.abs() as f64)
                        } else {
                            0.0
                        };

                        (amount0, amount1, price)
                    } else {
                        continue;
                    }
                }
                DexType::UniswapV3 => {
                    if let Ok(swap) = log.log_decode::<UniV3Swap>() {
                        let data = swap.inner.data;
                        let amount0: i128 = data.amount0.try_into().unwrap_or(0);
                        let amount1: i128 = data.amount1.try_into().unwrap_or(0);

                        // Calculate price from sqrtPriceX96
                        let sqrt_price: u128 = data.sqrtPriceX96.try_into().unwrap_or(0);
                        let price = if sqrt_price > 0 {
                            let price_x96 = (sqrt_price as f64) / (2f64.powi(96));
                            price_x96 * price_x96
                        } else if amount0 != 0 {
                            (amount1.abs() as f64) / (amount0.abs() as f64)
                        } else {
                            0.0
                        };

                        (amount0, amount1, price)
                    } else {
                        continue;
                    }
                }
                _ => continue,
            };

            // Emit swap event
            let swap_event = SwapEvent {
                pool: pool_addr,
                dex: pool_info.dex,
                token0: pool_info.token0,
                token1: pool_info.token1,
                amount0,
                amount1,
                price,
                block_number,
                tx_hash,
            };

            debug!(
                "Swap: {} | amounts: {}/{} | price: {:.6}",
                pool_info.name, amount0, amount1, price
            );

            if sender.send(Event::Swap(swap_event)).await.is_err() {
                error!("Event channel closed");
                break;
            }

            // Also emit price update
            let price_event = PriceUpdateEvent {
                pair: pool_info.name.clone(),
                pool: pool_addr,
                price,
                liquidity: U256::ZERO, // Would need separate call
                timestamp: chrono::Utc::now(),
            };

            let _ = sender.send(Event::PriceUpdate(price_event)).await;

            if event_count % 100 == 0 {
                info!("SwapEventCollector: {} events processed", event_count);
            }
        }

        Ok(())
    }
}
