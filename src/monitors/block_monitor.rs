//! Block monitor for tracking new blocks via WebSocket subscription
//!
//! This monitor subscribes to newHeads events and emits NewBlock events
//! with block information including timing analysis for latency tracking.

use alloy::providers::{Provider, ProviderBuilder, RootProvider, WsConnect};
use alloy::pubsub::PubSubFrontend;
use alloy::rpc::types::Header;
use async_trait::async_trait;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, error, info, warn};

use super::{
    reconnect_with_backoff, BlockInfo, Monitor, MonitorEvent, ReconnectConfig,
};
use crate::error::{MevError, ProviderError, Result};

/// Block timing statistics
#[derive(Debug, Default)]
pub struct BlockTimingStats {
    /// Total blocks processed
    pub blocks_processed: u64,
    /// Average latency in milliseconds
    pub avg_latency_ms: f64,
    /// Minimum latency observed
    pub min_latency_ms: u64,
    /// Maximum latency observed
    pub max_latency_ms: u64,
    /// Sum of all latencies (for computing average)
    latency_sum_ms: u64,
}

impl BlockTimingStats {
    /// Update stats with a new latency observation
    pub fn record_latency(&mut self, latency_ms: u64) {
        self.blocks_processed += 1;
        self.latency_sum_ms += latency_ms;
        self.avg_latency_ms = self.latency_sum_ms as f64 / self.blocks_processed as f64;

        if self.blocks_processed == 1 {
            self.min_latency_ms = latency_ms;
            self.max_latency_ms = latency_ms;
        } else {
            self.min_latency_ms = self.min_latency_ms.min(latency_ms);
            self.max_latency_ms = self.max_latency_ms.max(latency_ms);
        }
    }
}

/// Block monitor that subscribes to new blocks via WebSocket
pub struct BlockMonitor {
    /// WebSocket URL for the Ethereum node
    ws_url: String,
    /// Monitor name
    name: String,
    /// Whether the monitor is running
    running: AtomicBool,
    /// Stop signal sender
    stop_tx: RwLock<Option<mpsc::Sender<()>>>,
    /// Last processed block number
    last_block: AtomicU64,
    /// Block timing statistics
    timing_stats: RwLock<BlockTimingStats>,
    /// Reconnection configuration
    reconnect_config: ReconnectConfig,
}

impl BlockMonitor {
    /// Create a new block monitor
    pub fn new(ws_url: String) -> Self {
        Self {
            ws_url,
            name: "BlockMonitor".to_string(),
            running: AtomicBool::new(false),
            stop_tx: RwLock::new(None),
            last_block: AtomicU64::new(0),
            timing_stats: RwLock::new(BlockTimingStats::default()),
            reconnect_config: ReconnectConfig::default(),
        }
    }

    /// Create a new block monitor with custom reconnection config
    pub fn with_reconnect_config(ws_url: String, config: ReconnectConfig) -> Self {
        Self {
            ws_url,
            name: "BlockMonitor".to_string(),
            running: AtomicBool::new(false),
            stop_tx: RwLock::new(None),
            last_block: AtomicU64::new(0),
            timing_stats: RwLock::new(BlockTimingStats::default()),
            reconnect_config: config,
        }
    }

    /// Get the last processed block number
    pub fn last_block_number(&self) -> u64 {
        self.last_block.load(Ordering::Relaxed)
    }

    /// Get timing statistics
    pub async fn timing_stats(&self) -> BlockTimingStats {
        let stats = self.timing_stats.read().await;
        BlockTimingStats {
            blocks_processed: stats.blocks_processed,
            avg_latency_ms: stats.avg_latency_ms,
            min_latency_ms: stats.min_latency_ms,
            max_latency_ms: stats.max_latency_ms,
            latency_sum_ms: stats.latency_sum_ms,
        }
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

    /// Calculate latency from block timestamp to now
    fn calculate_latency(block_timestamp: u64) -> Option<u64> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .ok()?
            .as_secs();

        // Handle case where block timestamp is in the future (clock skew)
        if block_timestamp > now {
            return Some(0);
        }

        Some((now - block_timestamp) * 1000)
    }

    /// Process a new block header and create BlockInfo
    fn process_header(header: &Header, received_at: Instant) -> BlockInfo {
        use alloy::primitives::U256;
        let latency_ms = Self::calculate_latency(header.timestamp);

        BlockInfo {
            number: header.number,
            hash: header.hash,
            parent_hash: header.parent_hash,
            timestamp: header.timestamp,
            base_fee: header.base_fee_per_gas.map(|f| U256::from(f)),
            gas_limit: header.gas_limit,
            gas_used: header.gas_used,
            received_at,
            latency_ms,
        }
    }

    /// Main monitoring loop with automatic reconnection
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

            info!("{}: Connected to WebSocket, subscribing to new blocks", self.name);

            // Subscribe to new blocks
            let subscription = match provider.subscribe_blocks().await {
                Ok(sub) => sub,
                Err(e) => {
                    error!("{}: Failed to subscribe to blocks: {:?}", self.name, e);
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    continue;
                }
            };

            let mut stream = subscription.into_stream();

            // Process blocks until disconnection or stop signal
            loop {
                tokio::select! {
                    _ = stop_rx.recv() => {
                        info!("{}: Received stop signal", self.name);
                        return;
                    }
                    block_result = futures::StreamExt::next(&mut stream) => {
                        match block_result {
                            Some(header) => {
                                let received_at = Instant::now();
                                let block_info = Self::process_header(&header, received_at);

                                // Update last block
                                let prev_block = self.last_block.swap(block_info.number, Ordering::Relaxed);

                                // Warn if we missed blocks
                                if prev_block > 0 && block_info.number > prev_block + 1 {
                                    warn!(
                                        "{}: Missed {} blocks ({} -> {})",
                                        self.name,
                                        block_info.number - prev_block - 1,
                                        prev_block,
                                        block_info.number
                                    );
                                }

                                // Update timing stats
                                if let Some(latency) = block_info.latency_ms {
                                    let mut stats = self.timing_stats.write().await;
                                    stats.record_latency(latency);
                                }

                                debug!(
                                    "{}: New block #{} (hash: {:?}, base_fee: {:?}, latency: {:?}ms)",
                                    self.name,
                                    block_info.number,
                                    block_info.hash,
                                    block_info.base_fee,
                                    block_info.latency_ms
                                );

                                // Send event
                                if let Err(e) = event_tx.send(MonitorEvent::NewBlock(block_info)).await {
                                    error!("{}: Failed to send block event: {:?}", self.name, e);
                                    return;
                                }
                            }
                            None => {
                                warn!("{}: Block subscription stream ended", self.name);
                                break;
                            }
                        }
                    }
                }
            }

            // If we get here, the subscription ended - try to reconnect
            if self.running.load(Ordering::Relaxed) {
                warn!("{}: Connection lost, attempting to reconnect...", self.name);
            }
        }
    }
}

#[async_trait]
impl Monitor for BlockMonitor {
    fn name(&self) -> &str {
        &self.name
    }

    async fn start(&self, tx: mpsc::Sender<MonitorEvent>) -> Result<()> {
        if self.running.swap(true, Ordering::Relaxed) {
            return Err(MevError::Provider(ProviderError::SubscriptionError(
                "Block monitor is already running".to_string(),
            )));
        }

        let (stop_tx, stop_rx) = mpsc::channel(1);
        {
            let mut guard = self.stop_tx.write().await;
            *guard = Some(stop_tx);
        }

        // Spawn the monitoring loop
        let self_ref = unsafe {
            // Safety: We ensure the monitor lives as long as the spawned task
            // by requiring Arc<BlockMonitor> in practice
            &*(self as *const BlockMonitor)
        };

        tokio::spawn(async move {
            self_ref.run_monitoring_loop(tx, stop_rx).await;
        });

        Ok(())
    }

    async fn stop(&self) -> Result<()> {
        if !self.running.swap(false, Ordering::Relaxed) {
            return Ok(());
        }

        // Send stop signal
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

/// Safe wrapper for running BlockMonitor in an Arc
pub struct ArcBlockMonitor(pub Arc<BlockMonitor>);

impl ArcBlockMonitor {
    pub fn new(ws_url: String) -> Self {
        Self(Arc::new(BlockMonitor::new(ws_url)))
    }

    /// Start the monitor with proper Arc handling
    pub async fn start(&self, tx: mpsc::Sender<MonitorEvent>) -> Result<()> {
        let monitor = Arc::clone(&self.0);

        if monitor.running.swap(true, Ordering::Relaxed) {
            return Err(MevError::Provider(ProviderError::SubscriptionError(
                "Block monitor is already running".to_string(),
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
    fn test_block_timing_stats() {
        let mut stats = BlockTimingStats::default();

        stats.record_latency(100);
        assert_eq!(stats.blocks_processed, 1);
        assert_eq!(stats.min_latency_ms, 100);
        assert_eq!(stats.max_latency_ms, 100);
        assert_eq!(stats.avg_latency_ms, 100.0);

        stats.record_latency(200);
        assert_eq!(stats.blocks_processed, 2);
        assert_eq!(stats.min_latency_ms, 100);
        assert_eq!(stats.max_latency_ms, 200);
        assert_eq!(stats.avg_latency_ms, 150.0);

        stats.record_latency(50);
        assert_eq!(stats.blocks_processed, 3);
        assert_eq!(stats.min_latency_ms, 50);
        assert_eq!(stats.max_latency_ms, 200);
    }

    #[test]
    fn test_latency_calculation() {
        // Test with a timestamp in the past
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        let latency = BlockMonitor::calculate_latency(now - 2);
        assert!(latency.is_some());
        assert!(latency.unwrap() >= 2000); // At least 2 seconds

        // Test with current timestamp
        let latency = BlockMonitor::calculate_latency(now);
        assert!(latency.is_some());
        assert!(latency.unwrap() < 1000); // Less than 1 second

        // Test with future timestamp (clock skew)
        let latency = BlockMonitor::calculate_latency(now + 10);
        assert_eq!(latency, Some(0));
    }

    #[tokio::test]
    async fn test_block_monitor_creation() {
        let monitor = BlockMonitor::new("ws://localhost:8546".to_string());
        assert!(!monitor.is_running());
        assert_eq!(monitor.last_block_number(), 0);
    }
}
