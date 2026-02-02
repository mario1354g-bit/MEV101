//! Monitoring modules for MEV detection
//!
//! This module provides various monitors that track blockchain state:
//! - Block monitor: Tracks new blocks and timing
//! - Mempool monitor: Monitors pending transactions for swap opportunities
//! - Price monitor: Tracks price changes across DEX pools
//! - Liquidity monitor: Monitors liquidity events (pool creation, mints, burns)
//! - Liquidation monitor: Tracks health factors for lending protocol positions

pub mod block_monitor;
pub mod liquidity_monitor;
pub mod liquidation_monitor;
pub mod mempool_monitor;
pub mod price_monitor;

pub use block_monitor::BlockMonitor;
pub use liquidity_monitor::LiquidityMonitor;
pub use liquidation_monitor::LiquidationMonitor;
pub use mempool_monitor::MempoolMonitor;
pub use price_monitor::PriceMonitor;

use alloy::primitives::{Address, Bytes, TxHash, U256};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, RwLock};
use tracing::{error, info, warn};

use crate::error::Result;

/// Monitor trait that all monitors must implement
#[async_trait]
pub trait Monitor: Send + Sync {
    /// Returns the name of the monitor
    fn name(&self) -> &str;

    /// Starts the monitor and sends events through the provided channel
    async fn start(&self, tx: mpsc::Sender<MonitorEvent>) -> Result<()>;

    /// Stops the monitor gracefully
    async fn stop(&self) -> Result<()>;

    /// Returns whether the monitor is currently running
    fn is_running(&self) -> bool;
}

/// Block information from new block events
#[derive(Debug, Clone)]
pub struct BlockInfo {
    /// Block number
    pub number: u64,
    /// Block hash
    pub hash: TxHash,
    /// Parent block hash
    pub parent_hash: TxHash,
    /// Block timestamp (Unix seconds)
    pub timestamp: u64,
    /// Base fee per gas (EIP-1559)
    pub base_fee: Option<U256>,
    /// Gas limit for the block
    pub gas_limit: u64,
    /// Gas used by transactions in the block
    pub gas_used: u64,
    /// Time when the block was received locally
    pub received_at: Instant,
    /// Latency from block timestamp to local receive time
    pub latency_ms: Option<u64>,
}

/// Price update information
#[derive(Debug, Clone)]
pub struct PriceUpdate {
    /// Pool address
    pub pool: Address,
    /// Token0 address
    pub token0: Address,
    /// Token1 address
    pub token1: Address,
    /// Current price (token1 per token0)
    pub price: f64,
    /// Previous price
    pub previous_price: f64,
    /// Price change percentage
    pub price_change_pct: f64,
    /// Block number when update occurred
    pub block_number: u64,
    /// Pool reserves (for V2 pools)
    pub reserves: Option<(U256, U256)>,
    /// Pool liquidity (for V3 pools)
    pub liquidity: Option<U256>,
    /// Current tick (for V3 pools)
    pub tick: Option<i32>,
}

/// Liquidity event types
#[derive(Debug, Clone)]
pub enum LiquidityEventType {
    /// New pool created
    PoolCreated {
        factory: Address,
        token0: Address,
        token1: Address,
        fee: Option<u32>,
    },
    /// Liquidity added (mint)
    Mint {
        sender: Address,
        amount0: U256,
        amount1: U256,
        liquidity: Option<U256>,
    },
    /// Liquidity removed (burn)
    Burn {
        sender: Address,
        amount0: U256,
        amount1: U256,
        liquidity: Option<U256>,
    },
    /// Significant reserves change
    ReservesChanged {
        reserve0: U256,
        reserve1: U256,
        previous_reserve0: U256,
        previous_reserve1: U256,
    },
}

/// Liquidity event information
#[derive(Debug, Clone)]
pub struct LiquidityEvent {
    /// Pool address
    pub pool: Address,
    /// Event type
    pub event_type: LiquidityEventType,
    /// Block number
    pub block_number: u64,
    /// Transaction hash
    pub tx_hash: TxHash,
    /// Log index
    pub log_index: u64,
}

/// Lending protocol types
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LendingProtocol {
    AaveV3,
    CompoundV3,
    Euler,
    Morpho,
}

impl std::fmt::Display for LendingProtocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AaveV3 => write!(f, "Aave V3"),
            Self::CompoundV3 => write!(f, "Compound V3"),
            Self::Euler => write!(f, "Euler"),
            Self::Morpho => write!(f, "Morpho"),
        }
    }
}

/// Liquidation candidate information
#[derive(Debug, Clone)]
pub struct LiquidationInfo {
    /// The protocol where the position exists
    pub protocol: LendingProtocol,
    /// User address with the position
    pub user: Address,
    /// Current health factor (1.0 = at threshold)
    pub health_factor: f64,
    /// Total collateral value in USD
    pub collateral_value_usd: f64,
    /// Total debt value in USD
    pub debt_value_usd: f64,
    /// Maximum liquidatable debt amount in USD
    pub max_liquidation_usd: f64,
    /// Collateral assets
    pub collateral_assets: Vec<CollateralAsset>,
    /// Debt assets
    pub debt_assets: Vec<DebtAsset>,
    /// Block number when detected
    pub block_number: u64,
    /// Estimated profit from liquidation
    pub estimated_profit_usd: f64,
}

/// Collateral asset information
#[derive(Debug, Clone)]
pub struct CollateralAsset {
    /// Token address
    pub token: Address,
    /// Token symbol
    pub symbol: String,
    /// Amount deposited
    pub amount: U256,
    /// Value in USD
    pub value_usd: f64,
    /// Liquidation threshold
    pub liquidation_threshold: f64,
}

/// Debt asset information
#[derive(Debug, Clone)]
pub struct DebtAsset {
    /// Token address
    pub token: Address,
    /// Token symbol
    pub symbol: String,
    /// Amount borrowed
    pub amount: U256,
    /// Value in USD
    pub value_usd: f64,
}

/// Transaction information for pending transactions
#[derive(Debug, Clone)]
pub struct Transaction {
    /// Transaction hash
    pub hash: TxHash,
    /// Sender address
    pub from: Address,
    /// Recipient address (None for contract creation)
    pub to: Option<Address>,
    /// Transaction value in wei
    pub value: U256,
    /// Transaction input data
    pub input: Bytes,
    /// Gas price (legacy) or max fee per gas (EIP-1559)
    pub gas_price: Option<U256>,
    /// Max priority fee per gas (EIP-1559)
    pub max_priority_fee: Option<U256>,
    /// Max fee per gas (EIP-1559)
    pub max_fee_per_gas: Option<U256>,
    /// Gas limit
    pub gas: u64,
    /// Nonce
    pub nonce: u64,
    /// Decoded swap parameters (if applicable)
    pub swap_params: Option<SwapParams>,
    /// Time when transaction was first seen
    pub first_seen: Instant,
}

/// Decoded swap parameters
#[derive(Debug, Clone)]
pub struct SwapParams {
    /// DEX router used
    pub router: Address,
    /// Token being sold
    pub token_in: Address,
    /// Token being bought
    pub token_out: Address,
    /// Amount of token_in
    pub amount_in: U256,
    /// Minimum amount of token_out
    pub amount_out_min: U256,
    /// Swap path (for multi-hop swaps)
    pub path: Vec<Address>,
    /// Swap deadline
    pub deadline: Option<U256>,
    /// Recipient of the swap output
    pub recipient: Address,
}

/// Events emitted by monitors
#[derive(Debug, Clone)]
pub enum MonitorEvent {
    /// New block received
    NewBlock(BlockInfo),
    /// Pending transaction detected (boxed to reduce enum size)
    PendingTransaction(Box<Transaction>),
    /// Price update detected
    PriceUpdate(PriceUpdate),
    /// Liquidity change detected
    LiquidityChange(LiquidityEvent),
    /// Liquidation candidate detected
    LiquidationCandidate(LiquidationInfo),
    /// Chain reorganization detected
    ChainReorg {
        /// Previous head block number
        old_head: u64,
        /// New head block number after reorg
        new_head: u64,
        /// Depth of the reorganization
        depth: u64,
    },
}

/// Configuration for WebSocket reconnection
#[derive(Debug, Clone)]
pub struct ReconnectConfig {
    /// Initial delay before first reconnection attempt
    pub initial_delay: Duration,
    /// Maximum delay between reconnection attempts
    pub max_delay: Duration,
    /// Multiplier for exponential backoff
    pub multiplier: f64,
    /// Maximum number of reconnection attempts (None = infinite)
    pub max_attempts: Option<u32>,
}

impl Default for ReconnectConfig {
    fn default() -> Self {
        Self {
            initial_delay: Duration::from_millis(100),
            max_delay: Duration::from_secs(60),
            multiplier: 2.0,
            max_attempts: None,
        }
    }
}

impl ReconnectConfig {
    /// Calculate the delay for a given attempt number
    pub fn delay_for_attempt(&self, attempt: u32) -> Duration {
        let delay_ms = self.initial_delay.as_millis() as f64
            * self.multiplier.powi(attempt as i32);
        Duration::from_millis(delay_ms.min(self.max_delay.as_millis() as f64) as u64)
    }

    /// Check if we should continue trying to reconnect
    pub fn should_retry(&self, attempt: u32) -> bool {
        match self.max_attempts {
            Some(max) => attempt < max,
            None => true,
        }
    }
}

/// Monitor status
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MonitorStatus {
    Stopped,
    Starting,
    Running,
    Reconnecting,
    Failed,
}

/// Statistics for a monitor
#[derive(Debug, Clone, Default)]
pub struct MonitorStats {
    /// Total events emitted
    pub events_emitted: u64,
    /// Total errors encountered
    pub errors: u64,
    /// Number of reconnections
    pub reconnections: u64,
    /// Time of last event
    pub last_event_time: Option<Instant>,
    /// Time of last error
    pub last_error_time: Option<Instant>,
}

/// Manager for coordinating multiple monitors
pub struct MonitorManager {
    /// Registered monitors
    monitors: RwLock<Vec<Arc<dyn Monitor>>>,
    /// Monitor statuses
    statuses: RwLock<HashMap<String, MonitorStatus>>,
    /// Monitor statistics
    stats: RwLock<HashMap<String, MonitorStats>>,
    /// Event channel sender
    event_tx: mpsc::Sender<MonitorEvent>,
    /// Event channel receiver
    event_rx: RwLock<Option<mpsc::Receiver<MonitorEvent>>>,
    /// Whether the manager is running
    running: RwLock<bool>,
}

impl MonitorManager {
    /// Create a new monitor manager with the specified channel capacity
    pub fn new(channel_capacity: usize) -> Self {
        let (event_tx, event_rx) = mpsc::channel(channel_capacity);
        Self {
            monitors: RwLock::new(Vec::new()),
            statuses: RwLock::new(HashMap::new()),
            stats: RwLock::new(HashMap::new()),
            event_tx,
            event_rx: RwLock::new(Some(event_rx)),
            running: RwLock::new(false),
        }
    }

    /// Register a monitor
    pub async fn register(&self, monitor: Arc<dyn Monitor>) {
        let name = monitor.name().to_string();
        let mut monitors = self.monitors.write().await;
        let mut statuses = self.statuses.write().await;
        let mut stats = self.stats.write().await;

        monitors.push(monitor);
        statuses.insert(name.clone(), MonitorStatus::Stopped);
        stats.insert(name, MonitorStats::default());
    }

    /// Start all registered monitors
    pub async fn start_all(&self) -> Result<()> {
        let monitors = self.monitors.read().await;
        let mut running = self.running.write().await;

        if *running {
            warn!("Monitor manager is already running");
            return Ok(());
        }

        *running = true;

        for monitor in monitors.iter() {
            let name = monitor.name().to_string();
            info!("Starting monitor: {}", name);

            {
                let mut statuses = self.statuses.write().await;
                statuses.insert(name.clone(), MonitorStatus::Starting);
            }

            let tx = self.event_tx.clone();
            match monitor.start(tx).await {
                Ok(_) => {
                    let mut statuses = self.statuses.write().await;
                    statuses.insert(name.clone(), MonitorStatus::Running);
                    info!("Monitor started: {}", name);
                }
                Err(e) => {
                    let mut statuses = self.statuses.write().await;
                    statuses.insert(name.clone(), MonitorStatus::Failed);
                    error!("Failed to start monitor {}: {:?}", name, e);
                }
            }
        }

        Ok(())
    }

    /// Stop all monitors
    pub async fn stop_all(&self) -> Result<()> {
        let monitors = self.monitors.read().await;
        let mut running = self.running.write().await;

        if !*running {
            warn!("Monitor manager is not running");
            return Ok(());
        }

        for monitor in monitors.iter() {
            let name = monitor.name().to_string();
            info!("Stopping monitor: {}", name);

            match monitor.stop().await {
                Ok(_) => {
                    let mut statuses = self.statuses.write().await;
                    statuses.insert(name.clone(), MonitorStatus::Stopped);
                    info!("Monitor stopped: {}", name);
                }
                Err(e) => {
                    error!("Error stopping monitor {}: {:?}", name, e);
                }
            }
        }

        *running = false;
        Ok(())
    }

    /// Take ownership of the event receiver
    pub async fn take_event_receiver(&self) -> Option<mpsc::Receiver<MonitorEvent>> {
        self.event_rx.write().await.take()
    }

    /// Get the event sender (for creating additional senders)
    pub fn event_sender(&self) -> mpsc::Sender<MonitorEvent> {
        self.event_tx.clone()
    }

    /// Get the status of a specific monitor
    pub async fn get_status(&self, name: &str) -> Option<MonitorStatus> {
        let statuses = self.statuses.read().await;
        statuses.get(name).copied()
    }

    /// Get all monitor statuses
    pub async fn get_all_statuses(&self) -> HashMap<String, MonitorStatus> {
        self.statuses.read().await.clone()
    }

    /// Get statistics for a specific monitor
    pub async fn get_stats(&self, name: &str) -> Option<MonitorStats> {
        let stats = self.stats.read().await;
        stats.get(name).cloned()
    }

    /// Update statistics for a monitor
    pub async fn update_stats<F>(&self, name: &str, updater: F)
    where
        F: FnOnce(&mut MonitorStats),
    {
        let mut stats = self.stats.write().await;
        if let Some(stat) = stats.get_mut(name) {
            updater(stat);
        }
    }

    /// Check if the manager is running
    pub async fn is_running(&self) -> bool {
        *self.running.read().await
    }

    /// Get the number of registered monitors
    pub async fn monitor_count(&self) -> usize {
        self.monitors.read().await.len()
    }
}

/// Helper function to perform exponential backoff reconnection
pub async fn reconnect_with_backoff<F, Fut, T>(
    config: &ReconnectConfig,
    name: &str,
    mut connect_fn: F,
) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    let mut attempt = 0u32;

    loop {
        match connect_fn().await {
            Ok(result) => {
                if attempt > 0 {
                    info!("{}: Reconnected after {} attempts", name, attempt);
                }
                return Ok(result);
            }
            Err(e) => {
                if !config.should_retry(attempt) {
                    error!(
                        "{}: Max reconnection attempts ({}) reached",
                        name,
                        config.max_attempts.unwrap_or(0)
                    );
                    return Err(e);
                }

                let delay = config.delay_for_attempt(attempt);
                warn!(
                    "{}: Connection failed (attempt {}), retrying in {:?}: {:?}",
                    name, attempt + 1, delay, e
                );

                tokio::time::sleep(delay).await;
                attempt += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_reconnect_config_delay() {
        let config = ReconnectConfig {
            initial_delay: Duration::from_millis(100),
            max_delay: Duration::from_secs(10),
            multiplier: 2.0,
            max_attempts: Some(5),
        };

        assert_eq!(config.delay_for_attempt(0), Duration::from_millis(100));
        assert_eq!(config.delay_for_attempt(1), Duration::from_millis(200));
        assert_eq!(config.delay_for_attempt(2), Duration::from_millis(400));
        assert_eq!(config.delay_for_attempt(3), Duration::from_millis(800));
        // Should cap at max_delay
        assert_eq!(config.delay_for_attempt(10), Duration::from_secs(10));
    }

    #[test]
    fn test_reconnect_config_should_retry() {
        let config_limited = ReconnectConfig {
            max_attempts: Some(3),
            ..Default::default()
        };

        assert!(config_limited.should_retry(0));
        assert!(config_limited.should_retry(2));
        assert!(!config_limited.should_retry(3));
        assert!(!config_limited.should_retry(100));

        let config_unlimited = ReconnectConfig::default();
        assert!(config_unlimited.should_retry(0));
        assert!(config_unlimited.should_retry(1000));
    }

    #[tokio::test]
    async fn test_monitor_manager_creation() {
        let manager = MonitorManager::new(1000);
        assert!(!manager.is_running().await);
        assert_eq!(manager.monitor_count().await, 0);
    }
}
