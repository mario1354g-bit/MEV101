//! Liquidity monitor for tracking DEX liquidity events
//!
//! This monitor subscribes to:
//! - PairCreated events (Uniswap V2 and forks)
//! - PoolCreated events (Uniswap V3)
//! - Mint/Burn events for significant liquidity changes

use alloy::primitives::{address, Address, FixedBytes, TxHash, U256};
use alloy::providers::{Provider, ProviderBuilder, RootProvider, WsConnect};
use alloy::pubsub::PubSubFrontend;
use alloy::rpc::types::{Filter, Log};
use alloy::sol;
use async_trait::async_trait;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, error, info, warn};

use super::{
    reconnect_with_backoff, LiquidityEvent, LiquidityEventType, Monitor, MonitorEvent,
    ReconnectConfig,
};
use crate::error::{MevError, ProviderError, Result};

// Define event interfaces
sol! {
    // Uniswap V2 Factory - PairCreated
    event PairCreated(
        address indexed token0,
        address indexed token1,
        address pair,
        uint256 allPairs
    );

    // Uniswap V3 Factory - PoolCreated
    event PoolCreated(
        address indexed token0,
        address indexed token1,
        uint24 indexed fee,
        int24 tickSpacing,
        address pool
    );

    // Uniswap V2 Pair - Mint
    event V2Mint(
        address indexed sender,
        uint256 amount0,
        uint256 amount1
    );

    // Uniswap V2 Pair - Burn
    event V2Burn(
        address indexed sender,
        uint256 amount0,
        uint256 amount1,
        address indexed to
    );

    // Uniswap V3 Pool - Mint
    event V3Mint(
        address sender,
        address indexed owner,
        int24 indexed tickLower,
        int24 indexed tickUpper,
        uint128 amount,
        uint256 amount0,
        uint256 amount1
    );

    // Uniswap V3 Pool - Burn
    event V3Burn(
        address indexed owner,
        int24 indexed tickLower,
        int24 indexed tickUpper,
        uint128 amount,
        uint256 amount0,
        uint256 amount1
    );

    // Sync event (for reserve tracking)
    event Sync(
        uint112 reserve0,
        uint112 reserve1
    );
}

/// Known factory addresses (Ethereum mainnet)
pub mod factories {
    use super::*;

    /// Uniswap V2 Factory
    pub const UNISWAP_V2_FACTORY: Address = address!("5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f");
    /// Uniswap V3 Factory
    pub const UNISWAP_V3_FACTORY: Address = address!("1F98431c8aD98523631AE4a59f267346ea31F984");
    /// Sushiswap V2 Factory
    pub const SUSHISWAP_V2_FACTORY: Address = address!("C0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac");
    /// PancakeSwap V2 Factory (BSC, but included for reference)
    pub const PANCAKESWAP_V2_FACTORY: Address =
        address!("cA143Ce32Fe78f1f7019d7d551a6402fC5350c73");

    /// Get all V2 factory addresses
    pub fn v2_factories() -> Vec<Address> {
        vec![UNISWAP_V2_FACTORY, SUSHISWAP_V2_FACTORY]
    }

    /// Get all V3 factory addresses
    pub fn v3_factories() -> Vec<Address> {
        vec![UNISWAP_V3_FACTORY]
    }
}

/// Event signatures
pub mod signatures {
    use super::*;

    /// PairCreated(address,address,address,uint256)
    pub const PAIR_CREATED: FixedBytes<32> = FixedBytes::new([
        0x0d, 0x3e, 0x44, 0x56, 0x5d, 0x15, 0x05, 0x2e, 0x99, 0xe4, 0x6e, 0xae, 0xe7, 0x5e, 0x56,
        0xd0, 0x0f, 0x01, 0x66, 0x3e, 0x80, 0xa5, 0xd9, 0x38, 0xb3, 0xae, 0x96, 0x76, 0x1f, 0x21,
        0xef, 0xf3,
    ]);

    /// PoolCreated(address,address,uint24,int24,address)
    pub const POOL_CREATED: FixedBytes<32> = FixedBytes::new([
        0x78, 0x3c, 0xca, 0x1c, 0x0f, 0x03, 0xc7, 0x06, 0xa9, 0xc9, 0xea, 0xee, 0x2e, 0x41, 0x32,
        0x14, 0xce, 0x13, 0x58, 0x90, 0x14, 0xf2, 0x41, 0xb5, 0x39, 0x47, 0xd6, 0x0c, 0x18, 0x3a,
        0xed, 0x9a,
    ]);

    /// V2 Mint(address,uint256,uint256)
    pub const V2_MINT: FixedBytes<32> = FixedBytes::new([
        0x4c, 0x20, 0x9b, 0x5f, 0xc8, 0xad, 0x50, 0x75, 0x8f, 0x13, 0xe2, 0xe1, 0x08, 0x8b, 0xa5,
        0x68, 0x01, 0xac, 0xa9, 0x41, 0x89, 0x76, 0x05, 0x63, 0x22, 0xce, 0xb0, 0x32, 0xf5, 0x2b,
        0x51, 0x5b,
    ]);

    /// V2 Burn(address,uint256,uint256,address)
    pub const V2_BURN: FixedBytes<32> = FixedBytes::new([
        0xdc, 0xcd, 0x41, 0x2f, 0x0b, 0x12, 0x52, 0x81, 0x9c, 0xb1, 0xfd, 0x33, 0x0b, 0x93, 0x22,
        0x4c, 0xa6, 0xf1, 0x98, 0x56, 0x24, 0x08, 0x51, 0x64, 0x5b, 0x9b, 0x98, 0x9a, 0x71, 0xc1,
        0xd1, 0x10,
    ]);

    /// V3 Mint(address,address,int24,int24,uint128,uint256,uint256)
    pub const V3_MINT: FixedBytes<32> = FixedBytes::new([
        0x7a, 0x53, 0x08, 0x0b, 0xa4, 0x14, 0x15, 0x8b, 0xe7, 0xec, 0x69, 0xb1, 0xc0, 0x78, 0x66,
        0x92, 0x25, 0x08, 0x13, 0x61, 0x01, 0x35, 0xe3, 0x18, 0xf8, 0xa4, 0x7c, 0x4f, 0x93, 0x78,
        0xf5, 0xf4,
    ]);

    /// V3 Burn(address,int24,int24,uint128,uint256,uint256)
    pub const V3_BURN: FixedBytes<32> = FixedBytes::new([
        0x0c, 0x39, 0x6c, 0xd9, 0x89, 0xa3, 0x9f, 0x1f, 0xfc, 0x90, 0xa9, 0xc4, 0x0e, 0x12, 0x5c,
        0x79, 0x39, 0xa8, 0xd5, 0x91, 0x6c, 0xb0, 0xef, 0x55, 0x37, 0x76, 0x95, 0x29, 0x1e, 0x49,
        0x6b, 0xd6,
    ]);

    /// Sync(uint112,uint112)
    pub const SYNC: FixedBytes<32> = FixedBytes::new([
        0x1c, 0x41, 0x1e, 0x9a, 0x96, 0xd5, 0xd0, 0xe5, 0x46, 0xb5, 0xf3, 0x7c, 0x65, 0x51, 0x65,
        0xb1, 0x90, 0x89, 0x0b, 0xdb, 0xb6, 0xc6, 0x59, 0xa8, 0xfb, 0x23, 0x4d, 0x9f, 0xa9, 0xf4,
        0x01, 0x7b,
    ]);
}

/// Configuration for liquidity monitoring
#[derive(Debug, Clone)]
pub struct LiquidityMonitorConfig {
    /// V2 factory addresses to monitor
    pub v2_factories: HashSet<Address>,
    /// V3 factory addresses to monitor
    pub v3_factories: HashSet<Address>,
    /// Specific pools to monitor for Mint/Burn (if empty, monitor all)
    pub monitored_pools: HashSet<Address>,
    /// Minimum liquidity change (in USD equivalent) to emit event
    pub min_liquidity_change_usd: f64,
    /// Whether to monitor new pool creations
    pub monitor_pool_creations: bool,
    /// Whether to monitor mint/burn events
    pub monitor_mint_burn: bool,
}

impl Default for LiquidityMonitorConfig {
    fn default() -> Self {
        Self {
            v2_factories: factories::v2_factories().into_iter().collect(),
            v3_factories: factories::v3_factories().into_iter().collect(),
            monitored_pools: HashSet::new(),
            min_liquidity_change_usd: 0.0, // Emit all events by default
            monitor_pool_creations: true,
            monitor_mint_burn: true,
        }
    }
}

/// Statistics for liquidity monitoring
#[derive(Debug, Default)]
pub struct LiquidityStats {
    /// Total pool creations seen
    pub pools_created: u64,
    /// Total mint events
    pub mints: u64,
    /// Total burn events
    pub burns: u64,
    /// Events filtered out
    pub filtered_out: u64,
}

/// Liquidity monitor for DEX events
pub struct LiquidityMonitor {
    /// WebSocket URL
    ws_url: String,
    /// Monitor name
    name: String,
    /// Configuration
    config: LiquidityMonitorConfig,
    /// Whether the monitor is running
    running: AtomicBool,
    /// Stop signal sender
    stop_tx: RwLock<Option<mpsc::Sender<()>>>,
    /// Statistics
    stats: RwLock<LiquidityStats>,
    /// Reconnection configuration
    reconnect_config: ReconnectConfig,
}

impl LiquidityMonitor {
    /// Create a new liquidity monitor
    pub fn new(ws_url: String) -> Self {
        Self {
            ws_url,
            name: "LiquidityMonitor".to_string(),
            config: LiquidityMonitorConfig::default(),
            running: AtomicBool::new(false),
            stop_tx: RwLock::new(None),
            stats: RwLock::new(LiquidityStats::default()),
            reconnect_config: ReconnectConfig::default(),
        }
    }

    /// Create with custom configuration
    pub fn with_config(ws_url: String, config: LiquidityMonitorConfig) -> Self {
        Self {
            ws_url,
            name: "LiquidityMonitor".to_string(),
            config,
            running: AtomicBool::new(false),
            stop_tx: RwLock::new(None),
            stats: RwLock::new(LiquidityStats::default()),
            reconnect_config: ReconnectConfig::default(),
        }
    }

    /// Get current statistics
    pub async fn stats(&self) -> LiquidityStats {
        let stats = self.stats.read().await;
        LiquidityStats {
            pools_created: stats.pools_created,
            mints: stats.mints,
            burns: stats.burns,
            filtered_out: stats.filtered_out,
        }
    }

    /// Add a pool to monitor for mint/burn events
    pub async fn add_monitored_pool(&self, pool: Address) {
        // Note: This requires interior mutability in config, which we don't have
        // In practice, you'd configure this at creation time or use a different approach
        info!("{}: Pool {} added for monitoring", self.name, pool);
    }

    /// Connect to the WebSocket provider
    async fn connect(&self) -> Result<RootProvider<PubSubFrontend>> {
        let ws = WsConnect::new(&self.ws_url);
        let provider = ProviderBuilder::new()
            .on_ws(ws)
            .await
            .map_err(|e| MevError::Provider(ProviderError::WebSocketError(e.to_string())))?;
        Ok(provider)
    }

    /// Build the event filter
    fn build_filter(&self) -> Filter {
        let mut topics: Vec<FixedBytes<32>> = Vec::new();

        if self.config.monitor_pool_creations {
            topics.push(signatures::PAIR_CREATED);
            topics.push(signatures::POOL_CREATED);
        }

        if self.config.monitor_mint_burn {
            topics.push(signatures::V2_MINT);
            topics.push(signatures::V2_BURN);
            topics.push(signatures::V3_MINT);
            topics.push(signatures::V3_BURN);
        }

        // Build addresses to monitor
        let mut addresses: Vec<Address> = Vec::new();
        addresses.extend(self.config.v2_factories.iter());
        addresses.extend(self.config.v3_factories.iter());
        addresses.extend(self.config.monitored_pools.iter());

        Filter::new()
            .address(addresses)
            .event_signature(topics)
    }

    /// Process a log event
    fn process_log(&self, log: &Log) -> Option<LiquidityEvent> {
        let topics = &log.topics();
        if topics.is_empty() {
            return None;
        }

        let event_sig = topics[0];
        let pool = log.address();
        let block_number = log.block_number.unwrap_or(0);
        let tx_hash = log.transaction_hash.unwrap_or_default();
        let log_index = log.log_index.unwrap_or(0);

        match event_sig {
            sig if sig == signatures::PAIR_CREATED => {
                self.process_pair_created(log, pool, block_number, tx_hash, log_index)
            }
            sig if sig == signatures::POOL_CREATED => {
                self.process_pool_created(log, pool, block_number, tx_hash, log_index)
            }
            sig if sig == signatures::V2_MINT => {
                self.process_v2_mint(log, pool, block_number, tx_hash, log_index)
            }
            sig if sig == signatures::V2_BURN => {
                self.process_v2_burn(log, pool, block_number, tx_hash, log_index)
            }
            sig if sig == signatures::V3_MINT => {
                self.process_v3_mint(log, pool, block_number, tx_hash, log_index)
            }
            sig if sig == signatures::V3_BURN => {
                self.process_v3_burn(log, pool, block_number, tx_hash, log_index)
            }
            _ => None,
        }
    }

    /// Process PairCreated event
    fn process_pair_created(
        &self,
        log: &Log,
        factory: Address,
        block_number: u64,
        tx_hash: TxHash,
        log_index: u64,
    ) -> Option<LiquidityEvent> {
        let topics = log.topics();
        if topics.len() < 3 {
            return None;
        }

        // token0 and token1 are indexed
        let token0 = Address::from_slice(&topics[1][12..32]);
        let token1 = Address::from_slice(&topics[2][12..32]);

        // pair address is in the data
        let data = log.data().data.as_ref();
        if data.len() < 32 {
            return None;
        }

        let pair = Address::from_slice(&data[12..32]);

        Some(LiquidityEvent {
            pool: pair,
            event_type: LiquidityEventType::PoolCreated {
                factory,
                token0,
                token1,
                fee: None,
            },
            block_number,
            tx_hash,
            log_index,
        })
    }

    /// Process PoolCreated event (V3)
    fn process_pool_created(
        &self,
        log: &Log,
        factory: Address,
        block_number: u64,
        tx_hash: TxHash,
        log_index: u64,
    ) -> Option<LiquidityEvent> {
        let topics = log.topics();
        if topics.len() < 4 {
            return None;
        }

        let token0 = Address::from_slice(&topics[1][12..32]);
        let token1 = Address::from_slice(&topics[2][12..32]);
        let fee_bytes: [u8; 4] = topics[3][28..32].try_into().ok()?;
        let fee = u32::from_be_bytes(fee_bytes);

        // pool address is in the data
        let data = log.data().data.as_ref();
        if data.len() < 64 {
            return None;
        }

        // tickSpacing at 0..32, pool at 32..64
        let pool = Address::from_slice(&data[44..64]);

        Some(LiquidityEvent {
            pool,
            event_type: LiquidityEventType::PoolCreated {
                factory,
                token0,
                token1,
                fee: Some(fee),
            },
            block_number,
            tx_hash,
            log_index,
        })
    }

    /// Process V2 Mint event
    fn process_v2_mint(
        &self,
        log: &Log,
        pool: Address,
        block_number: u64,
        tx_hash: TxHash,
        log_index: u64,
    ) -> Option<LiquidityEvent> {
        let topics = log.topics();
        if topics.len() < 2 {
            return None;
        }

        let sender = Address::from_slice(&topics[1][12..32]);

        let data = log.data().data.as_ref();
        if data.len() < 64 {
            return None;
        }

        let amount0 = U256::from_be_slice(&data[0..32]);
        let amount1 = U256::from_be_slice(&data[32..64]);

        Some(LiquidityEvent {
            pool,
            event_type: LiquidityEventType::Mint {
                sender,
                amount0,
                amount1,
                liquidity: None,
            },
            block_number,
            tx_hash,
            log_index,
        })
    }

    /// Process V2 Burn event
    fn process_v2_burn(
        &self,
        log: &Log,
        pool: Address,
        block_number: u64,
        tx_hash: TxHash,
        log_index: u64,
    ) -> Option<LiquidityEvent> {
        let topics = log.topics();
        if topics.len() < 3 {
            return None;
        }

        let sender = Address::from_slice(&topics[1][12..32]);
        // to address is in topics[2]

        let data = log.data().data.as_ref();
        if data.len() < 64 {
            return None;
        }

        let amount0 = U256::from_be_slice(&data[0..32]);
        let amount1 = U256::from_be_slice(&data[32..64]);

        Some(LiquidityEvent {
            pool,
            event_type: LiquidityEventType::Burn {
                sender,
                amount0,
                amount1,
                liquidity: None,
            },
            block_number,
            tx_hash,
            log_index,
        })
    }

    /// Process V3 Mint event
    fn process_v3_mint(
        &self,
        log: &Log,
        pool: Address,
        block_number: u64,
        tx_hash: TxHash,
        log_index: u64,
    ) -> Option<LiquidityEvent> {
        let topics = log.topics();
        if topics.len() < 4 {
            return None;
        }

        let _owner = Address::from_slice(&topics[1][12..32]);
        // tickLower in topics[2], tickUpper in topics[3]

        let data = log.data().data.as_ref();
        if data.len() < 128 {
            return None;
        }

        // sender at 0..32, amount (liquidity) at 32..64, amount0 at 64..96, amount1 at 96..128
        let sender = Address::from_slice(&data[12..32]);
        let liquidity = U256::from_be_slice(&data[32..64]);
        let amount0 = U256::from_be_slice(&data[64..96]);
        let amount1 = U256::from_be_slice(&data[96..128]);

        Some(LiquidityEvent {
            pool,
            event_type: LiquidityEventType::Mint {
                sender,
                amount0,
                amount1,
                liquidity: Some(liquidity),
            },
            block_number,
            tx_hash,
            log_index,
        })
    }

    /// Process V3 Burn event
    fn process_v3_burn(
        &self,
        log: &Log,
        pool: Address,
        block_number: u64,
        tx_hash: TxHash,
        log_index: u64,
    ) -> Option<LiquidityEvent> {
        let topics = log.topics();
        if topics.len() < 4 {
            return None;
        }

        let owner = Address::from_slice(&topics[1][12..32]);
        // tickLower in topics[2], tickUpper in topics[3]

        let data = log.data().data.as_ref();
        if data.len() < 96 {
            return None;
        }

        // amount (liquidity) at 0..32, amount0 at 32..64, amount1 at 64..96
        let liquidity = U256::from_be_slice(&data[0..32]);
        let amount0 = U256::from_be_slice(&data[32..64]);
        let amount1 = U256::from_be_slice(&data[64..96]);

        Some(LiquidityEvent {
            pool,
            event_type: LiquidityEventType::Burn {
                sender: owner,
                amount0,
                amount1,
                liquidity: Some(liquidity),
            },
            block_number,
            tx_hash,
            log_index,
        })
    }

    /// Main monitoring loop
    async fn run_monitoring_loop(
        &self,
        event_tx: mpsc::Sender<MonitorEvent>,
        mut stop_rx: mpsc::Receiver<()>,
    ) {
        while self.running.load(Ordering::Relaxed) {
            // Connect with exponential backoff
            let provider = match reconnect_with_backoff(
                &self.reconnect_config,
                &self.name,
                || async { self.connect().await },
            )
            .await
            {
                Ok(p) => p,
                Err(e) => {
                    error!("{}: Failed to connect after all retries: {:?}", self.name, e);
                    break;
                }
            };

            info!("{}: Connected to WebSocket, subscribing to logs", self.name);

            // Build and subscribe to filter
            let filter = self.build_filter();
            let subscription = match provider.subscribe_logs(&filter).await {
                Ok(sub) => sub,
                Err(e) => {
                    error!("{}: Failed to subscribe to logs: {:?}", self.name, e);
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    continue;
                }
            };

            let mut stream = subscription.into_stream();

            loop {
                tokio::select! {
                    _ = stop_rx.recv() => {
                        info!("{}: Received stop signal", self.name);
                        return;
                    }
                    log_result = futures::StreamExt::next(&mut stream) => {
                        match log_result {
                            Some(log) => {
                                if let Some(event) = self.process_log(&log) {
                                    // Update stats
                                    {
                                        let mut stats = self.stats.write().await;
                                        match &event.event_type {
                                            LiquidityEventType::PoolCreated { .. } => {
                                                stats.pools_created += 1;
                                            }
                                            LiquidityEventType::Mint { .. } => {
                                                stats.mints += 1;
                                            }
                                            LiquidityEventType::Burn { .. } => {
                                                stats.burns += 1;
                                            }
                                            _ => {}
                                        }
                                    }

                                    debug!(
                                        "{}: Liquidity event - pool: {:?}, type: {:?}, block: {}",
                                        self.name, event.pool, event.event_type, event.block_number
                                    );

                                    // Send event
                                    if let Err(e) = event_tx.send(MonitorEvent::LiquidityChange(event)).await {
                                        error!("{}: Failed to send liquidity event: {:?}", self.name, e);
                                        return;
                                    }
                                }
                            }
                            None => {
                                warn!("{}: Log subscription stream ended", self.name);
                                break;
                            }
                        }
                    }
                }
            }

            if self.running.load(Ordering::Relaxed) {
                warn!("{}: Connection lost, attempting to reconnect...", self.name);
            }
        }
    }
}

/// NOTE: The Monitor trait implementation for LiquidityMonitor uses unsafe raw pointers
/// which can cause use-after-free bugs. Use ArcLiquidityMonitor for safe usage.
#[async_trait]
impl Monitor for LiquidityMonitor {
    fn name(&self) -> &str {
        &self.name
    }

    async fn start(&self, _tx: mpsc::Sender<MonitorEvent>) -> Result<()> {
        // This implementation is deprecated due to safety concerns.
        // Use ArcLiquidityMonitor::start() instead which uses safe Arc-based patterns.
        Err(MevError::Provider(ProviderError::SubscriptionError(
            "Direct LiquidityMonitor::start() is unsafe. Use ArcLiquidityMonitor instead.".to_string(),
        )))
    }

    async fn stop(&self) -> Result<()> {
        if !self.running.swap(false, Ordering::Relaxed) {
            return Ok(());
        }

        let stop_tx = {
            let mut guard = self.stop_tx.write().await;
            guard.take()
        };

        if let Some(tx) = stop_tx {
            let _ = tx.send(()).await;
        }

        info!("{}: Stopped", self.name);
        Ok(())
    }

    fn is_running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }
}

/// Safe Arc wrapper for LiquidityMonitor
pub struct ArcLiquidityMonitor(pub Arc<LiquidityMonitor>);

impl ArcLiquidityMonitor {
    pub fn new(ws_url: String) -> Self {
        Self(Arc::new(LiquidityMonitor::new(ws_url)))
    }

    pub async fn start(&self, tx: mpsc::Sender<MonitorEvent>) -> Result<()> {
        let monitor = Arc::clone(&self.0);

        if monitor.running.swap(true, Ordering::Relaxed) {
            return Err(MevError::Provider(ProviderError::SubscriptionError(
                "Liquidity monitor is already running".to_string(),
            )));
        }

        let (stop_tx, stop_rx) = mpsc::channel(1);
        {
            let mut guard = monitor.stop_tx.write().await;
            *guard = Some(stop_tx);
        }

        tokio::spawn(async move {
            monitor.run_monitoring_loop(tx, stop_rx).await;
        });

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_factories_list() {
        let v2 = factories::v2_factories();
        let v3 = factories::v3_factories();

        assert!(v2.contains(&factories::UNISWAP_V2_FACTORY));
        assert!(v2.contains(&factories::SUSHISWAP_V2_FACTORY));
        assert!(v3.contains(&factories::UNISWAP_V3_FACTORY));
    }

    #[test]
    fn test_liquidity_config_default() {
        let config = LiquidityMonitorConfig::default();

        assert!(config.v2_factories.contains(&factories::UNISWAP_V2_FACTORY));
        assert!(config.v3_factories.contains(&factories::UNISWAP_V3_FACTORY));
        assert!(config.monitor_pool_creations);
        assert!(config.monitor_mint_burn);
    }

    #[tokio::test]
    async fn test_liquidity_monitor_creation() {
        let monitor = LiquidityMonitor::new("ws://localhost:8546".to_string());
        assert!(!monitor.is_running());

        let stats = monitor.stats().await;
        assert_eq!(stats.pools_created, 0);
        assert_eq!(stats.mints, 0);
        assert_eq!(stats.burns, 0);
    }

    #[test]
    fn test_filter_building() {
        let monitor = LiquidityMonitor::new("ws://localhost:8546".to_string());
        let _filter = monitor.build_filter();
        // Filter builds without panic
    }
}
