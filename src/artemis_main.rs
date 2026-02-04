//! Artemis-based MEV bot entry point
//!
//! FOCUSED ON ONE THING: Flashloan Arbitrage
//! - Borrow ETH via flashloan
//! - Execute 2-4 hop swaps
//! - Return loan + profit
//! - No sandwiches, no liquidations, just clean arb

use crate::artemis::{Engine, EngineConfig, ExecutionMode};
use crate::collectors::{
    BlockCollector, PoolDiscoveryCollector, PoolDiscoveryConfig, SwapEventCollector,
    SwapEventCollectorConfig,
};
use crate::executors::{FlashbotsExecutor, FlashbotsExecutorConfig};
use crate::strategies::{FlashloanArbConfig, FlashloanArbStrategy, LongTailStrategy, LongTailStrategyConfig};
use alloy::primitives::Address;
use tracing::info;

/// Run the Artemis-based MEV bot - FLASHLOAN ARB ONLY
pub async fn run_artemis(
    ws_url: String,
    rpc_url: String,
    signer_key: String,
    flashloan_contract: Address,
    dry_run: bool,
) -> eyre::Result<()> {
    info!("================================================");
    info!("  FLASHLOAN ARBITRAGE BOT");
    info!("  No sandwiches. No liquidations. Just arb.");
    info!("================================================");

    // Configure engine - lower profit threshold for more opportunities
    let engine_config = EngineConfig {
        event_buffer: 10_000,
        action_buffer: 1_000,
        execution_mode: if dry_run {
            ExecutionMode::DryRun
        } else {
            ExecutionMode::Live
        },
        min_profit_wei: 1_000_000_000_000_000, // 0.001 ETH (~$2.50)
    };

    // Collectors - just what we need
    let swap_config = SwapEventCollectorConfig::mainnet_defaults(ws_url.clone());

    let pool_discovery_config = PoolDiscoveryConfig {
        ws_url: ws_url.clone(),
        ..PoolDiscoveryConfig::mainnet_aggressive()
    };

    // MAIN STRATEGY: Flashloan Arbitrage
    let flashloan_config = FlashloanArbConfig {
        min_profit_usd: 1.0,  // Target just $1 profit
        max_hops: 4,          // 2, 3, or 4 hop routes
        flashloan_amounts: vec![1.0, 5.0, 10.0, 25.0, 50.0, 100.0], // ETH amounts to try
        gas_price_gwei: 30,
        gas_per_hop: 150_000,
        eth_price_usd: 2500.0,
    };

    // BACKUP: Long-tail strategy for new pools
    let longtail_config = LongTailStrategyConfig {
        rpc_url: rpc_url.clone(),
        min_profit_bps: 10,
        max_hops: 4,
        max_input_eth: 50.0,
        use_flashloan: true,
        ..LongTailStrategyConfig::default()
    };

    // Configure executor with your 7 builders
    let flashbots_config = FlashbotsExecutorConfig {
        relay_url: "https://relay.flashbots.net".to_string(),
        rpc_url: rpc_url.clone(),
        signer_key,
        flashloan_contract,
        dry_run,
    };

    // Create executor
    let executor = FlashbotsExecutor::new(flashbots_config)?;
    executor.init_simulator_balance().await?;

    // Build engine - ONLY arbitrage strategies
    let engine = Engine::new(engine_config)
        // Collectors
        .add_collector(BlockCollector::new(ws_url.clone()))
        .add_collector(SwapEventCollector::new(swap_config))
        .add_collector(PoolDiscoveryCollector::new(pool_discovery_config))
        // Strategies - ONLY flashloan arb
        .add_strategy(FlashloanArbStrategy::new(flashloan_config))
        .add_strategy(LongTailStrategy::new(longtail_config))
        // Executor
        .add_executor(executor);

    info!("Bot configured:");
    info!("  - Flashloan amounts: 1, 5, 10, 25, 50, 100 ETH");
    info!("  - Max hops: 4");
    info!("  - Min profit: $1");
    info!("  - Multi-builder submission enabled");
    info!("");
    info!("Starting...");

    engine.run().await
}
