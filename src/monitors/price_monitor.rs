//! Price monitor for tracking DEX pool prices
//!
//! This monitor polls prices for registered pools every block using multicall
//! to batch getReserves (V2) and slot0 (V3) calls efficiently.

use alloy::primitives::{address, Address, Bytes, U256};
use alloy::providers::{ProviderBuilder, RootProvider};
use alloy::sol;
use alloy::transports::http::{Client, Http};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, error, info, warn};

use super::{Monitor, MonitorEvent, PriceUpdate, ReconnectConfig};
use crate::error::{MevError, ProviderError, Result};

// Define the Multicall3 contract interface
sol! {
    #[derive(Debug)]
    struct Call3 {
        address target;
        bool allowFailure;
        bytes callData;
    }

    #[derive(Debug)]
    struct Result3 {
        bool success;
        bytes returnData;
    }

    #[sol(rpc)]
    contract Multicall3 {
        function aggregate3(Call3[] calldata calls) external payable returns (Result3[] memory returnData);
    }
}

// Uniswap V2 Pair interface
sol! {
    #[sol(rpc)]
    contract IUniswapV2Pair {
        function getReserves() external view returns (uint112 reserve0, uint112 reserve1, uint32 blockTimestampLast);
        function token0() external view returns (address);
        function token1() external view returns (address);
    }
}

// Uniswap V3 Pool interface
sol! {
    #[sol(rpc)]
    contract IUniswapV3Pool {
        function slot0() external view returns (
            uint160 sqrtPriceX96,
            int24 tick,
            uint16 observationIndex,
            uint16 observationCardinality,
            uint16 observationCardinalityNext,
            uint8 feeProtocol,
            bool unlocked
        );
        function liquidity() external view returns (uint128);
        function token0() external view returns (address);
        function token1() external view returns (address);
    }
}

/// Pool type enumeration
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PoolType {
    UniswapV2,
    UniswapV3,
    SushiswapV2,
    CurveV1,
}

/// Pool information for tracking
#[derive(Debug, Clone)]
pub struct PoolInfo {
    /// Pool address
    pub address: Address,
    /// Pool type
    pub pool_type: PoolType,
    /// Token0 address
    pub token0: Address,
    /// Token1 address
    pub token1: Address,
    /// Token0 decimals
    pub decimals0: u8,
    /// Token1 decimals
    pub decimals1: u8,
    /// Fee tier (for V3 pools)
    pub fee: Option<u32>,
}

/// Internal state for tracking pool prices
#[derive(Debug, Clone, Default)]
struct PoolState {
    /// Last known price (token1 per token0)
    last_price: f64,
    /// Last reserves (V2)
    last_reserves: Option<(U256, U256)>,
    /// Last liquidity (V3)
    last_liquidity: Option<U256>,
    /// Last tick (V3)
    last_tick: Option<i32>,
    /// Block when last updated
    last_block: u64,
}

/// Configuration for price monitoring
#[derive(Debug, Clone)]
pub struct PriceMonitorConfig {
    /// Minimum price change percentage to emit event
    pub min_price_change_pct: f64,
    /// Poll interval in milliseconds
    pub poll_interval_ms: u64,
    /// Whether to emit events for all price updates (not just significant changes)
    pub emit_all_updates: bool,
}

impl Default for PriceMonitorConfig {
    fn default() -> Self {
        Self {
            min_price_change_pct: 0.1, // 0.1% minimum change
            poll_interval_ms: 1000,    // Poll every second
            emit_all_updates: false,
        }
    }
}

/// Multicall3 contract address (same on all EVM chains)
pub const MULTICALL3_ADDRESS: Address = address!("cA11bde05977b3631167028862bE2a173976CA11");

/// Price monitor for DEX pools
pub struct PriceMonitor {
    /// HTTP RPC URL
    http_url: String,
    /// Monitor name
    name: String,
    /// Configuration
    config: PriceMonitorConfig,
    /// Registered pools
    pools: RwLock<HashMap<Address, PoolInfo>>,
    /// Pool states
    states: RwLock<HashMap<Address, PoolState>>,
    /// Whether the monitor is running
    running: AtomicBool,
    /// Stop signal sender
    stop_tx: RwLock<Option<mpsc::Sender<()>>>,
    /// Current block number
    current_block: AtomicU64,
    /// Reconnection configuration
    #[allow(dead_code)] // Reserved for WebSocket reconnection logic
    reconnect_config: ReconnectConfig,
}

impl PriceMonitor {
    /// Create a new price monitor
    pub fn new(http_url: String) -> Self {
        Self {
            http_url,
            name: "PriceMonitor".to_string(),
            config: PriceMonitorConfig::default(),
            pools: RwLock::new(HashMap::new()),
            states: RwLock::new(HashMap::new()),
            running: AtomicBool::new(false),
            stop_tx: RwLock::new(None),
            current_block: AtomicU64::new(0),
            reconnect_config: ReconnectConfig::default(),
        }
    }

    /// Create with custom configuration
    pub fn with_config(http_url: String, config: PriceMonitorConfig) -> Self {
        Self {
            http_url,
            name: "PriceMonitor".to_string(),
            config,
            pools: RwLock::new(HashMap::new()),
            states: RwLock::new(HashMap::new()),
            running: AtomicBool::new(false),
            stop_tx: RwLock::new(None),
            current_block: AtomicU64::new(0),
            reconnect_config: ReconnectConfig::default(),
        }
    }

    /// Register a pool for monitoring
    pub async fn register_pool(&self, pool: PoolInfo) {
        let address = pool.address;
        let mut pools = self.pools.write().await;
        let mut states = self.states.write().await;

        pools.insert(address, pool);
        states.insert(address, PoolState::default());

        info!("{}: Registered pool {:?}", self.name, address);
    }

    /// Register multiple pools
    pub async fn register_pools(&self, pools_to_add: Vec<PoolInfo>) {
        let mut pools = self.pools.write().await;
        let mut states = self.states.write().await;

        for pool in pools_to_add {
            let address = pool.address;
            pools.insert(address, pool);
            states.insert(address, PoolState::default());
        }

        info!("{}: Registered {} pools", self.name, pools.len());
    }

    /// Unregister a pool
    pub async fn unregister_pool(&self, address: Address) {
        let mut pools = self.pools.write().await;
        let mut states = self.states.write().await;

        pools.remove(&address);
        states.remove(&address);
    }

    /// Get the number of registered pools
    pub async fn pool_count(&self) -> usize {
        self.pools.read().await.len()
    }

    /// Update current block number (called when new block is received)
    pub fn set_current_block(&self, block: u64) {
        self.current_block.store(block, Ordering::Relaxed);
    }

    /// Build multicall for V2 pools (getReserves)
    fn build_v2_calls(&self, pools: &[(&Address, &PoolInfo)]) -> Vec<Call3> {
        // getReserves() selector: 0x0902f1ac
        let get_reserves_selector = hex::decode("0902f1ac").expect("valid hex for getReserves selector");

        pools
            .iter()
            .filter(|(_, info)| {
                matches!(info.pool_type, PoolType::UniswapV2 | PoolType::SushiswapV2)
            })
            .map(|(addr, _)| Call3 {
                target: **addr,
                allowFailure: true,
                callData: Bytes::from(get_reserves_selector.clone()),
            })
            .collect()
    }

    /// Build multicall for V3 pools (slot0 + liquidity)
    fn build_v3_calls(&self, pools: &[(&Address, &PoolInfo)]) -> Vec<Call3> {
        // slot0() selector: 0x3850c7bd
        // liquidity() selector: 0x1a686502
        let slot0_selector = hex::decode("3850c7bd").expect("valid hex for slot0 selector");
        let liquidity_selector = hex::decode("1a686502").expect("valid hex for liquidity selector");

        let mut calls = Vec::new();

        for (addr, info) in pools.iter() {
            if info.pool_type == PoolType::UniswapV3 {
                calls.push(Call3 {
                    target: **addr,
                    allowFailure: true,
                    callData: Bytes::from(slot0_selector.clone()),
                });
                calls.push(Call3 {
                    target: **addr,
                    allowFailure: true,
                    callData: Bytes::from(liquidity_selector.clone()),
                });
            }
        }

        calls
    }

    /// Calculate price from V2 reserves
    fn calculate_v2_price(
        &self,
        reserve0: U256,
        reserve1: U256,
        decimals0: u8,
        decimals1: u8,
    ) -> f64 {
        if reserve0.is_zero() {
            return 0.0;
        }

        // Price = reserve1 / reserve0, adjusted for decimals
        let r0 = reserve0.to::<u128>() as f64;
        let r1 = reserve1.to::<u128>() as f64;

        let decimal_adjustment = 10f64.powi(decimals0 as i32 - decimals1 as i32);
        (r1 / r0) * decimal_adjustment
    }

    /// Calculate price from V3 sqrtPriceX96 with overflow protection
    fn calculate_v3_price(&self, sqrt_price_x96: U256, decimals0: u8, decimals1: u8) -> f64 {
        // price = (sqrtPriceX96 / 2^96)^2
        // price = sqrtPriceX96^2 / 2^192

        if sqrt_price_x96.is_zero() {
            return 0.0;
        }

        // Calculate bits to check if value fits in u128
        let bits = 256 - sqrt_price_x96.leading_zeros();

        let price = if bits <= 128 {
            // Safe path: value fits in u128
            let sqrt_price = sqrt_price_x96.to::<u128>() as f64;
            let two_96 = 2f64.powi(96);
            (sqrt_price / two_96).powi(2)
        } else {
            // For very large sqrtPrice values, use logarithmic calculation to avoid overflow
            // log(price) = 2 * (log(sqrtPriceX96) - 96 * log(2))
            let shift = bits.saturating_sub(53); // f64 mantissa is 53 bits
            let shifted = sqrt_price_x96 >> shift;
            let base = shifted.to::<u128>() as f64;

            // Calculate in log space: log(sqrtPrice) = log(base) + shift * log(2)
            let log_sqrt = base.ln() + (shift as f64) * 2f64.ln();
            let log_price = 2.0 * (log_sqrt - 96.0 * 2f64.ln());
            log_price.exp()
        };

        // Adjust for decimals
        let decimal_adjustment = 10f64.powi(decimals0 as i32 - decimals1 as i32);
        price * decimal_adjustment
    }

    /// Parse V2 getReserves result
    fn parse_v2_reserves(&self, data: &[u8]) -> Option<(U256, U256)> {
        if data.len() < 96 {
            return None;
        }

        let reserve0 = U256::from_be_slice(&data[0..32]);
        let reserve1 = U256::from_be_slice(&data[32..64]);

        Some((reserve0, reserve1))
    }

    /// Parse V3 slot0 result
    fn parse_v3_slot0(&self, data: &[u8]) -> Option<(U256, i32)> {
        if data.len() < 64 {
            return None;
        }

        let sqrt_price_x96 = U256::from_be_slice(&data[0..32]);

        // tick is int24, stored in the second 32 bytes
        let tick_bytes = &data[32..64];
        let tick_raw = i32::from_be_bytes([
            tick_bytes[28],
            tick_bytes[29],
            tick_bytes[30],
            tick_bytes[31],
        ]);

        // Sign extend from 24 bits
        let tick = if tick_raw & 0x800000 != 0 {
            tick_raw | !0xFFFFFF
        } else {
            tick_raw & 0xFFFFFF
        };

        Some((sqrt_price_x96, tick))
    }

    /// Parse V3 liquidity result
    fn parse_v3_liquidity(&self, data: &[u8]) -> Option<U256> {
        if data.len() < 32 {
            return None;
        }

        Some(U256::from_be_slice(&data[0..32]))
    }

    /// Poll all registered pools
    async fn poll_pools(
        &self,
        provider: &RootProvider<Http<Client>>,
        event_tx: &mpsc::Sender<MonitorEvent>,
    ) -> Result<()> {
        let pools = self.pools.read().await;
        if pools.is_empty() {
            return Ok(());
        }

        let pool_list: Vec<_> = pools.iter().collect();
        let current_block = self.current_block.load(Ordering::Relaxed);

        // Build multicall
        let v2_calls = self.build_v2_calls(&pool_list);
        let v3_calls = self.build_v3_calls(&pool_list);

        let mut all_calls = v2_calls.clone();
        all_calls.extend(v3_calls.clone());

        if all_calls.is_empty() {
            return Ok(());
        }

        // Execute multicall
        let multicall = Multicall3::new(MULTICALL3_ADDRESS, provider);
        let results = match multicall.aggregate3(all_calls.clone()).call().await {
            Ok(result) => result.returnData,
            Err(e) => {
                warn!("{}: Multicall failed: {:?}", self.name, e);
                return Ok(());
            }
        };

        // Process results
        let mut states = self.states.write().await;
        let v2_pools: Vec<_> = pool_list
            .iter()
            .filter(|(_, info)| {
                matches!(info.pool_type, PoolType::UniswapV2 | PoolType::SushiswapV2)
            })
            .collect();

        let v3_pools: Vec<_> = pool_list
            .iter()
            .filter(|(_, info)| info.pool_type == PoolType::UniswapV3)
            .collect();

        // Process V2 results
        for (i, (addr, info)) in v2_pools.iter().enumerate() {
            if i >= results.len() {
                break;
            }

            let result = &results[i];
            if !result.success {
                continue;
            }

            if let Some((reserve0, reserve1)) = self.parse_v2_reserves(&result.returnData) {
                let price = self.calculate_v2_price(reserve0, reserve1, info.decimals0, info.decimals1);

                let state = states.entry(**addr).or_default();
                let previous_price = state.last_price;
                let price_change_pct = if previous_price > 0.0 {
                    ((price - previous_price) / previous_price * 100.0).abs()
                } else {
                    0.0
                };

                // Check if we should emit an event
                let should_emit = self.config.emit_all_updates
                    || price_change_pct >= self.config.min_price_change_pct
                    || state.last_block == 0;

                if should_emit {
                    let update = PriceUpdate {
                        pool: **addr,
                        token0: info.token0,
                        token1: info.token1,
                        price,
                        previous_price,
                        price_change_pct,
                        block_number: current_block,
                        reserves: Some((reserve0, reserve1)),
                        liquidity: None,
                        tick: None,
                    };

                    if let Err(e) = event_tx.send(MonitorEvent::PriceUpdate(update)).await {
                        error!("{}: Failed to send price update: {:?}", self.name, e);
                    }

                    debug!(
                        "{}: V2 price update for {:?}: {} -> {} ({:.4}%)",
                        self.name, addr, previous_price, price, price_change_pct
                    );
                }

                // Update state
                state.last_price = price;
                state.last_reserves = Some((reserve0, reserve1));
                state.last_block = current_block;
            }
        }

        // Process V3 results (slot0 + liquidity pairs)
        let v2_count = v2_pools.len();
        for (i, (addr, info)) in v3_pools.iter().enumerate() {
            let slot0_idx = v2_count + i * 2;
            let liquidity_idx = v2_count + i * 2 + 1;

            if slot0_idx >= results.len() || liquidity_idx >= results.len() {
                break;
            }

            let slot0_result = &results[slot0_idx];
            let liquidity_result = &results[liquidity_idx];

            if !slot0_result.success {
                continue;
            }

            if let Some((sqrt_price_x96, tick)) = self.parse_v3_slot0(&slot0_result.returnData) {
                let price = self.calculate_v3_price(sqrt_price_x96, info.decimals0, info.decimals1);

                let liquidity = if liquidity_result.success {
                    self.parse_v3_liquidity(&liquidity_result.returnData)
                } else {
                    None
                };

                let state = states.entry(**addr).or_default();
                let previous_price = state.last_price;
                let price_change_pct = if previous_price > 0.0 {
                    ((price - previous_price) / previous_price * 100.0).abs()
                } else {
                    0.0
                };

                let should_emit = self.config.emit_all_updates
                    || price_change_pct >= self.config.min_price_change_pct
                    || state.last_block == 0;

                if should_emit {
                    let update = PriceUpdate {
                        pool: **addr,
                        token0: info.token0,
                        token1: info.token1,
                        price,
                        previous_price,
                        price_change_pct,
                        block_number: current_block,
                        reserves: None,
                        liquidity,
                        tick: Some(tick),
                    };

                    if let Err(e) = event_tx.send(MonitorEvent::PriceUpdate(update)).await {
                        error!("{}: Failed to send price update: {:?}", self.name, e);
                    }

                    debug!(
                        "{}: V3 price update for {:?}: {} -> {} ({:.4}%), tick: {}",
                        self.name, addr, previous_price, price, price_change_pct, tick
                    );
                }

                // Update state
                state.last_price = price;
                state.last_liquidity = liquidity;
                state.last_tick = Some(tick);
                state.last_block = current_block;
            }
        }

        Ok(())
    }

    /// Main monitoring loop
    async fn run_monitoring_loop(
        &self,
        event_tx: mpsc::Sender<MonitorEvent>,
        mut stop_rx: mpsc::Receiver<()>,
    ) {
        let provider = ProviderBuilder::new().on_http(
            self.http_url.parse().expect("http_url should be a valid URL")
        );

        let poll_interval = Duration::from_millis(self.config.poll_interval_ms);

        info!(
            "{}: Starting price monitoring with {} ms interval",
            self.name, self.config.poll_interval_ms
        );

        loop {
            tokio::select! {
                _ = stop_rx.recv() => {
                    info!("{}: Received stop signal", self.name);
                    return;
                }
                _ = tokio::time::sleep(poll_interval) => {
                    if !self.running.load(Ordering::Relaxed) {
                        return;
                    }

                    if let Err(e) = self.poll_pools(&provider, &event_tx).await {
                        warn!("{}: Error polling pools: {:?}", self.name, e);
                    }
                }
            }
        }
    }
}

/// NOTE: The Monitor trait implementation for PriceMonitor uses unsafe raw pointers
/// which can cause use-after-free bugs. Use ArcPriceMonitor for safe usage.
#[async_trait]
impl Monitor for PriceMonitor {
    fn name(&self) -> &str {
        &self.name
    }

    async fn start(&self, _tx: mpsc::Sender<MonitorEvent>) -> Result<()> {
        // This implementation is deprecated due to safety concerns.
        // Use ArcPriceMonitor::start() instead which uses safe Arc-based patterns.
        Err(MevError::Provider(ProviderError::SubscriptionError(
            "Direct PriceMonitor::start() is unsafe. Use ArcPriceMonitor instead.".to_string(),
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

/// Safe Arc wrapper for PriceMonitor
pub struct ArcPriceMonitor(pub Arc<PriceMonitor>);

impl ArcPriceMonitor {
    pub fn new(http_url: String) -> Self {
        Self(Arc::new(PriceMonitor::new(http_url)))
    }

    pub async fn start(&self, tx: mpsc::Sender<MonitorEvent>) -> Result<()> {
        let monitor = Arc::clone(&self.0);

        if monitor.running.swap(true, Ordering::Relaxed) {
            return Err(MevError::Provider(ProviderError::SubscriptionError(
                "Price monitor is already running".to_string(),
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
    fn test_price_monitor_config_default() {
        let config = PriceMonitorConfig::default();
        assert_eq!(config.min_price_change_pct, 0.1);
        assert_eq!(config.poll_interval_ms, 1000);
        assert!(!config.emit_all_updates);
    }

    #[test]
    fn test_v2_price_calculation() {
        let monitor = PriceMonitor::new("http://localhost:8545".to_string());

        // 1:1 ratio with same decimals
        let price = monitor.calculate_v2_price(
            U256::from(1_000_000_000_000_000_000u128), // 1e18
            U256::from(1_000_000_000_000_000_000u128), // 1e18
            18,
            18,
        );
        assert!((price - 1.0).abs() < 0.0001);

        // 2:1 ratio
        let price = monitor.calculate_v2_price(
            U256::from(1_000_000_000_000_000_000u128), // 1e18
            U256::from(2_000_000_000_000_000_000u128), // 2e18
            18,
            18,
        );
        assert!((price - 2.0).abs() < 0.0001);

        // Different decimals (USDC/ETH: 6 vs 18)
        let price = monitor.calculate_v2_price(
            U256::from(1_000_000u128),                  // 1 USDC (6 decimals)
            U256::from(1_000_000_000_000_000_000u128), // 1 ETH (18 decimals)
            6,
            18,
        );
        // Price should be close to 1e-12 (USDC is worth 1e12 ETH in this scenario)
        assert!(price > 0.0);
    }

    #[tokio::test]
    async fn test_price_monitor_creation() {
        let monitor = PriceMonitor::new("http://localhost:8545".to_string());
        assert!(!monitor.is_running());
        assert_eq!(monitor.pool_count().await, 0);
    }

    #[tokio::test]
    async fn test_pool_registration() {
        let monitor = PriceMonitor::new("http://localhost:8545".to_string());

        let pool = PoolInfo {
            address: Address::ZERO,
            pool_type: PoolType::UniswapV2,
            token0: Address::ZERO,
            token1: Address::ZERO,
            decimals0: 18,
            decimals1: 18,
            fee: None,
        };

        monitor.register_pool(pool).await;
        assert_eq!(monitor.pool_count().await, 1);

        monitor.unregister_pool(Address::ZERO).await;
        assert_eq!(monitor.pool_count().await, 0);
    }
}
