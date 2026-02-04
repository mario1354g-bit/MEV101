//! Artemis-based MEV bot entry point
//!
//! This module provides an alternative entry point using the Artemis architecture.

use crate::artemis::{Engine, EngineConfig, ExecutionMode};
use crate::collectors::{
    BlockCollector, LiquidationCollector, LiquidationCollectorConfig, MempoolCollector,
    MempoolCollectorConfig, SwapEventCollector, SwapEventCollectorConfig,
};
use crate::executors::{FlashbotsExecutor, FlashbotsExecutorConfig};
use crate::strategies::{
    ArbitrageStrategy, ArbitrageStrategyConfig, LiquidationStrategy,
    LiquidationStrategyConfig, SandwichStrategy, SandwichStrategyConfig,
    create_trending_pairs,
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
        sample_rate: 1, // Fetch EVERY tx for maximum opportunity detection
    };

    let swap_config = SwapEventCollectorConfig::mainnet_defaults(ws_url.clone());

    // Configure liquidation collector - monitors Aave/Compound for at-risk borrowers
    let liquidation_collector_config = LiquidationCollectorConfig {
        ws_url: ws_url.clone(),
        http_url: rpc_url.clone(),
        health_threshold: 1.1, // Alert when HF drops below 1.1
        min_debt_usd: 1000.0,  // Only track positions > $1000
        seed_accounts: true,   // Seed with known accounts on startup
        track_aave: true,
        track_compound: true,
    };

    // Configure strategies with 500+ trending pairs
    let arb_config = ArbitrageStrategyConfig {
        min_profit_bps: 5,
        max_input_eth: 10.0,
        use_flashloan: true,
        flashloan_token: address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"), // WETH
        pairs: create_trending_pairs(), // 500+ trending pairs including top 50
    };

    let sandwich_config = SandwichStrategyConfig {
        rpc_url: rpc_url.clone(),
        use_revm_simulation: true,
        ..SandwichStrategyConfig::default()
    };

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

    // Create executor and initialize with real wallet balance
    let executor = FlashbotsExecutor::new(flashbots_config)?;
    executor.init_simulator_balance().await?;

    // Build and run engine
    let engine = Engine::new(engine_config)
        // Collectors
        .add_collector(MempoolCollector::new(mempool_config))
        .add_collector(BlockCollector::new(ws_url.clone()))
        .add_collector(SwapEventCollector::new(swap_config))
        .add_collector(LiquidationCollector::new(liquidation_collector_config))
        // Strategies
        .add_strategy(ArbitrageStrategy::new(arb_config))
        .add_strategy(SandwichStrategy::new(sandwich_config))
        .add_strategy(LiquidationStrategy::new(liquidation_config))
        // Executors
        .add_executor(executor);

    info!("Artemis Engine configured, starting...");
    engine.run().await
}
