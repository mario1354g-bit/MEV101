//! Artemis-based MEV bot entry point
//!
//! This module provides an alternative entry point using the Artemis architecture.

use crate::artemis::{Engine, EngineConfig, ExecutionMode};
use crate::collectors::{
    BlockCollector, MempoolCollector, MempoolCollectorConfig, SwapEventCollector,
    SwapEventCollectorConfig,
};
use crate::executors::{FlashbotsExecutor, FlashbotsExecutorConfig};
use crate::strategies::{
    ArbitrageStrategy, ArbitrageStrategyConfig, DexPair, LiquidationStrategy,
    LiquidationStrategyConfig, SandwichStrategy, SandwichStrategyConfig, TokenInfo,
};
use alloy::primitives::{address, Address};
use tracing::info;

/// Run the Artemis-based MEV bot
pub async fn run_artemis(
    ws_url: String,
    rpc_url: String,
    signer_key: String,
    flashloan_contract: Address,
    dry_run: bool,
) -> eyre::Result<()> {
    info!("Starting Artemis MEV Engine");

    // Configure engine
    let engine_config = EngineConfig {
        event_buffer: 10_000,
        action_buffer: 1_000,
        execution_mode: if dry_run {
            ExecutionMode::DryRun
        } else {
            ExecutionMode::Live
        },
        min_profit_wei: 5_000_000_000_000_000, // 0.005 ETH
    };

    // Configure collectors
    let mempool_config = MempoolCollectorConfig {
        ws_url: ws_url.clone(),
        fetch_full_tx: true,
        sample_rate: 5, // Fetch every 5th tx
    };

    let swap_config = SwapEventCollectorConfig::mainnet_defaults(ws_url.clone());

    // Configure strategies
    let arb_config = ArbitrageStrategyConfig {
        min_profit_bps: 5,
        max_input_eth: 10.0,
        use_flashloan: true,
        flashloan_token: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"), // WETH
        pairs: create_default_pairs(),
    };

    let sandwich_config = SandwichStrategyConfig::default();

    let liquidation_config = LiquidationStrategyConfig {
        rpc_url: rpc_url.clone(),
        min_profit_eth: 0.01,
        health_factor_threshold: 1.1,
        watched_accounts: vec![
            // Add accounts to watch for liquidation
        ],
        use_flashloan: true,
    };

    // Configure executor
    let flashbots_config = FlashbotsExecutorConfig {
        relay_url: "https://relay.flashbots.net".to_string(),
        rpc_url: rpc_url.clone(),
        signer_key,
        flashloan_contract,
        dry_run,
    };

    // Build and run engine
    let engine = Engine::new(engine_config)
        // Collectors
        .add_collector(MempoolCollector::new(mempool_config))
        .add_collector(BlockCollector::new(ws_url.clone()))
        .add_collector(SwapEventCollector::new(swap_config))
        // Strategies
        .add_strategy(ArbitrageStrategy::new(arb_config))
        .add_strategy(SandwichStrategy::new(sandwich_config))
        .add_strategy(LiquidationStrategy::new(liquidation_config))
        // Executors
        .add_executor(FlashbotsExecutor::new(flashbots_config)?);

    info!("Artemis Engine configured, starting...");
    engine.run().await
}

/// Create default trading pairs for arbitrage
fn create_default_pairs() -> Vec<DexPair> {
    use crate::artemis::DexType;

    let weth = TokenInfo {
        address: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"),
        symbol: "WETH".to_string(),
        decimals: 18,
    };

    let usdc = TokenInfo {
        address: address!("A0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"),
        symbol: "USDC".to_string(),
        decimals: 6,
    };

    let usdt = TokenInfo {
        address: address!("dAC17F958D2ee523a2206206994597C13D831ec7"),
        symbol: "USDT".to_string(),
        decimals: 6,
    };

    let wbtc = TokenInfo {
        address: address!("2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599"),
        symbol: "WBTC".to_string(),
        decimals: 8,
    };

    let dai = TokenInfo {
        address: address!("6B175474E89094C44Da98b954EedeAC495271d0F"),
        symbol: "DAI".to_string(),
        decimals: 18,
    };
    let _ = dai; // Suppress unused warning

    vec![
        // WETH/USDC pairs
        DexPair {
            pool: address!("B4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc"),
            dex: DexType::UniswapV2,
            token0: usdc.clone(),
            token1: weth.clone(),
            fee_bps: 30,
        },
        DexPair {
            pool: address!("88e6A0c2dDD26FEEb64F039a2c41296FcB3f5640"),
            dex: DexType::UniswapV3,
            token0: usdc.clone(),
            token1: weth.clone(),
            fee_bps: 5,
        },
        DexPair {
            pool: address!("397FF1542f962076d0BFE58eA045FfA2d347ACa0"),
            dex: DexType::SushiSwap,
            token0: usdc.clone(),
            token1: weth.clone(),
            fee_bps: 30,
        },
        // WETH/USDT pairs
        DexPair {
            pool: address!("0d4a11d5EEaaC28EC3F61d100daF4d40471f1852"),
            dex: DexType::UniswapV2,
            token0: weth.clone(),
            token1: usdt.clone(),
            fee_bps: 30,
        },
        DexPair {
            pool: address!("11b815efB8f581194ae79006d24E0d814B7697F6"),
            dex: DexType::UniswapV3,
            token0: weth.clone(),
            token1: usdt.clone(),
            fee_bps: 5,
        },
        // WBTC/WETH pairs
        DexPair {
            pool: address!("Cbcdf9626bC03E24f779434178A73a0B4bad62eD"),
            dex: DexType::UniswapV3,
            token0: wbtc.clone(),
            token1: weth.clone(),
            fee_bps: 30,
        },
        DexPair {
            pool: address!("CEfF51756c56CeFFCA006cD410B03FFC46dd3a58"),
            dex: DexType::SushiSwap,
            token0: wbtc.clone(),
            token1: weth.clone(),
            fee_bps: 30,
        },
    ]
}
