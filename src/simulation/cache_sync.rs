//! Cache Sync - Real-time state synchronization for WarmCache.
//!
//! This module provides background tasks that keep the WarmCache in sync with
//! the live blockchain state:
//! 1. Block loop - updates block environment every new block (~12s)
//! 2. Sync event listener - updates pool reserves in real-time (<1s)
//!
//! Together, these eliminate the "stale state" problem that causes phantom profits.

use alloy::primitives::{Address, U256};
use alloy::providers::Provider;
use alloy::rpc::types::Filter;
use alloy::sol;
use alloy::sol_types::SolEvent;
use alloy::transports::Transport;
use futures::StreamExt;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;
use tracing::{debug, error, info, trace, warn};

use super::warm_cache::WarmCache;

// Uniswap V2 Sync event signature
sol! {
    #[derive(Debug)]
    event UniV2Sync(uint112 reserve0, uint112 reserve1);
}

/// Configuration for cache synchronization
#[derive(Debug, Clone)]
pub struct CacheSyncConfig {
    /// How often to poll for new blocks (if WS unavailable)
    pub block_poll_interval: Duration,
    /// Pools to monitor for Sync events
    pub monitored_pools: Vec<Address>,
    /// Whether to use WebSocket subscriptions (faster) or polling
    pub use_websocket: bool,
}

impl Default for CacheSyncConfig {
    fn default() -> Self {
        Self {
            block_poll_interval: Duration::from_secs(1),
            monitored_pools: Vec::new(),
            use_websocket: true,
        }
    }
}

/// Runs the block update loop.
///
/// This task updates the WarmCache's block environment every time a new block
/// is detected. This is CRITICAL for accurate simulations - without it, your
/// REVM thinks it's still on an old block.
///
/// # Arguments
/// * `cache` - The WarmCache to update
/// * `provider` - HTTP provider for fetching block data
/// * `shutdown_rx` - Shutdown signal receiver
pub async fn run_block_update_loop<T, P>(
    cache: Arc<WarmCache<T, P>>,
    provider: Arc<P>,
    mut shutdown_rx: broadcast::Receiver<()>,
) -> eyre::Result<()>
where
    T: Transport + Clone,
    P: Provider<T> + Clone + 'static,
{
    info!("Starting block update loop for WarmCache");

    let mut last_block = 0u64;
    let poll_interval = Duration::from_millis(500); // Poll every 500ms

    loop {
        tokio::select! {
            _ = shutdown_rx.recv() => {
                info!("Block update loop shutting down");
                break;
            }
            _ = tokio::time::sleep(poll_interval) => {
                match update_block_state(&cache, &provider, &mut last_block).await {
                    Ok(updated) => {
                        if updated {
                            trace!(block = last_block, "Block state updated");
                        }
                    }
                    Err(e) => {
                        warn!(error = %e, "Failed to update block state");
                    }
                }
            }
        }
    }

    Ok(())
}

/// Runs the block update loop using WebSocket subscriptions.
///
/// This is faster than polling because you get notified immediately when
/// a new block arrives, typically within ~100ms of block production.
pub async fn run_block_update_loop_ws<T, P, WsP>(
    cache: Arc<WarmCache<T, P>>,
    ws_provider: Arc<WsP>,
    mut shutdown_rx: broadcast::Receiver<()>,
) -> eyre::Result<()>
where
    T: Transport + Clone,
    P: Provider<T> + Clone + 'static,
    WsP: Provider<alloy::pubsub::PubSubFrontend> + Clone + 'static,
{
    info!("Starting WebSocket block subscription for WarmCache");

    let sub = ws_provider.subscribe_blocks().await?;
    let mut stream = sub.into_stream();

    loop {
        tokio::select! {
            _ = shutdown_rx.recv() => {
                info!("Block subscription shutting down");
                break;
            }
            Some(block) = stream.next() => {
                let block_number = block.inner.number;
                let timestamp = block.inner.timestamp;
                let base_fee = block.inner.base_fee_per_gas
                    .map(U256::from)
                    .unwrap_or(U256::from(30_000_000_000u64));

                cache.update_block_state(block_number, timestamp, base_fee);

                info!(
                    block = block_number,
                    base_fee_gwei = %(base_fee / U256::from(1_000_000_000u64)),
                    "New block - cache updated"
                );
            }
        }
    }

    Ok(())
}

/// Runs the Sync event listener for real-time reserve updates.
///
/// This subscribes to Uniswap V2 Sync events and updates pool reserves
/// in the WarmCache immediately when a swap occurs. This means your
/// simulations use sub-second fresh data instead of waiting for the next block.
///
/// # Arguments
/// * `cache` - The WarmCache to update
/// * `ws_provider` - WebSocket provider for event subscriptions
/// * `pools` - List of V2 pool addresses to monitor
/// * `shutdown_rx` - Shutdown signal receiver
pub async fn run_sync_event_listener<T, P, WsP>(
    cache: Arc<WarmCache<T, P>>,
    ws_provider: Arc<WsP>,
    pools: Vec<Address>,
    mut shutdown_rx: broadcast::Receiver<()>,
) -> eyre::Result<()>
where
    T: Transport + Clone,
    P: Provider<T> + Clone + 'static,
    WsP: Provider<alloy::pubsub::PubSubFrontend> + Clone + 'static,
{
    if pools.is_empty() {
        warn!("No pools to monitor for Sync events");
        return Ok(());
    }

    info!(num_pools = pools.len(), "Starting Sync event listener");

    // Build filter for Sync events on our target pools
    let filter = Filter::new()
        .address(pools.clone())
        .event_signature(UniV2Sync::SIGNATURE_HASH);

    let sub = ws_provider.subscribe_logs(&filter).await?;
    let mut stream = sub.into_stream();

    let mut events_processed = 0u64;

    loop {
        tokio::select! {
            _ = shutdown_rx.recv() => {
                info!(events_processed, "Sync event listener shutting down");
                break;
            }
            Some(log) = stream.next() => {
                let pool_address = log.address();
                match UniV2Sync::decode_log_data(log.data(), true) {
                    Ok(sync_event) => {
                        let reserve0 = U256::from(sync_event.reserve0);
                        let reserve1 = U256::from(sync_event.reserve1);
                        let block = log.block_number.unwrap_or(0);

                        // Update reserves in cache
                        cache.update_v2_reserves(pool_address, reserve0, reserve1, block);

                        events_processed += 1;

                        trace!(
                            pool = %pool_address,
                            reserve0 = %reserve0,
                            reserve1 = %reserve1,
                            block = block,
                            "Reserves updated from Sync event"
                        );
                    }
                    Err(e) => {
                        debug!(error = %e, "Failed to decode Sync event");
                    }
                }
            }
        }
    }

    Ok(())
}

/// Helper function to update block state from provider.
async fn update_block_state<T, P>(
    cache: &WarmCache<T, P>,
    provider: &P,
    last_block: &mut u64,
) -> eyre::Result<bool>
where
    T: Transport + Clone,
    P: Provider<T>,
{
    let current_block = provider.get_block_number().await?;

    if current_block <= *last_block {
        return Ok(false); // No new block
    }

    let block = provider
        .get_block_by_number(
            alloy::eips::BlockNumberOrTag::Number(current_block),
            alloy::rpc::types::BlockTransactionsKind::Hashes,
        )
        .await?
        .ok_or_else(|| eyre::eyre!("Block not found"))?;

    let base_fee = block
        .header
        .base_fee_per_gas
        .map(U256::from)
        .unwrap_or(U256::from(30_000_000_000u64));

    cache.update_block_state(current_block, block.header.timestamp, base_fee);
    *last_block = current_block;

    debug!(
        block = current_block,
        timestamp = block.header.timestamp,
        base_fee_gwei = %(base_fee / U256::from(1_000_000_000u64)),
        "Block state updated"
    );

    Ok(true)
}

/// Spawns all cache synchronization tasks.
///
/// This is the main entry point - call this once at startup to begin
/// keeping your WarmCache in sync with the live blockchain.
///
/// # Example
/// ```ignore
/// let (sync_handles, shutdown_tx) = spawn_cache_sync_tasks(
///     warm_cache.clone(),
///     http_provider.clone(),
///     Some(ws_provider.clone()),
///     pool_addresses,
/// ).await?;
/// ```
pub async fn spawn_cache_sync_tasks<T, P, WsP>(
    cache: Arc<WarmCache<T, P>>,
    http_provider: Arc<P>,
    ws_provider: Option<Arc<WsP>>,
    pools: Vec<Address>,
) -> eyre::Result<(Vec<tokio::task::JoinHandle<()>>, broadcast::Sender<()>)>
where
    T: Transport + Clone + Send + Sync + 'static,
    P: Provider<T> + Clone + Send + Sync + 'static,
    WsP: Provider<alloy::pubsub::PubSubFrontend> + Clone + Send + Sync + 'static,
{
    let (shutdown_tx, _) = broadcast::channel::<()>(1);
    let mut handles = Vec::new();

    // Spawn block update loop
    if let Some(ref ws) = ws_provider {
        // Use WebSocket for faster block updates
        let cache_clone = Arc::clone(&cache);
        let ws_clone = Arc::clone(ws);
        let shutdown_rx = shutdown_tx.subscribe();

        let handle = tokio::spawn(async move {
            if let Err(e) = run_block_update_loop_ws(cache_clone, ws_clone, shutdown_rx).await {
                error!(error = %e, "Block update loop (WS) failed");
            }
        });
        handles.push(handle);
    } else {
        // Fall back to HTTP polling
        let cache_clone = Arc::clone(&cache);
        let provider_clone = Arc::clone(&http_provider);
        let shutdown_rx = shutdown_tx.subscribe();

        let handle = tokio::spawn(async move {
            if let Err(e) = run_block_update_loop(cache_clone, provider_clone, shutdown_rx).await {
                error!(error = %e, "Block update loop (HTTP) failed");
            }
        });
        handles.push(handle);
    }

    // Spawn Sync event listener (requires WebSocket)
    if let Some(ws) = ws_provider {
        if !pools.is_empty() {
            let cache_clone = Arc::clone(&cache);
            let ws_clone = Arc::clone(&ws);
            let shutdown_rx = shutdown_tx.subscribe();

            let handle = tokio::spawn(async move {
                if let Err(e) = run_sync_event_listener(cache_clone, ws_clone, pools, shutdown_rx).await {
                    error!(error = %e, "Sync event listener failed");
                }
            });
            handles.push(handle);
        }
    }

    info!(
        num_tasks = handles.len(),
        "Cache sync tasks spawned"
    );

    Ok((handles, shutdown_tx))
}

/// Convenience struct for managing cache synchronization.
pub struct CacheSyncManager {
    handles: Vec<tokio::task::JoinHandle<()>>,
    shutdown_tx: broadcast::Sender<()>,
}

impl CacheSyncManager {
    /// Create a new manager with the given tasks.
    pub fn new(
        handles: Vec<tokio::task::JoinHandle<()>>,
        shutdown_tx: broadcast::Sender<()>,
    ) -> Self {
        Self { handles, shutdown_tx }
    }

    /// Signal all tasks to shut down.
    pub fn shutdown(&self) {
        let _ = self.shutdown_tx.send(());
    }

    /// Wait for all tasks to complete.
    pub async fn wait(self) {
        for handle in self.handles {
            let _ = handle.await;
        }
    }

    /// Get the number of running tasks.
    pub fn task_count(&self) -> usize {
        self.handles.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_default() {
        let config = CacheSyncConfig::default();
        assert_eq!(config.block_poll_interval, Duration::from_secs(1));
        assert!(config.monitored_pools.is_empty());
        assert!(config.use_websocket);
    }
}
