//! Liquidation monitor for tracking health factors on lending protocols
//!
//! This monitor polls health factors for tracked positions across:
//! - Aave V3
//! - Compound V3
//! - Euler
//! - Morpho
//!
//! Emits LiquidationCandidate events when health factor < 1.05

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

use super::{
    LendingProtocol, LiquidationInfo, Monitor, MonitorEvent,
    ReconnectConfig,
};
use crate::error::{MevError, ProviderError, Result};

// Multicall3 interface
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

// Aave V3 Pool interface
sol! {
    #[sol(rpc)]
    contract IAaveV3Pool {
        function getUserAccountData(address user) external view returns (
            uint256 totalCollateralBase,
            uint256 totalDebtBase,
            uint256 availableBorrowsBase,
            uint256 currentLiquidationThreshold,
            uint256 ltv,
            uint256 healthFactor
        );
    }

    #[sol(rpc)]
    contract IAaveV3DataProvider {
        function getUserReserveData(address asset, address user) external view returns (
            uint256 currentATokenBalance,
            uint256 currentStableDebt,
            uint256 currentVariableDebt,
            uint256 principalStableDebt,
            uint256 scaledVariableDebt,
            uint256 stableBorrowRate,
            uint256 liquidityRate,
            uint40 stableRateLastUpdated,
            bool usageAsCollateralEnabled
        );
    }
}

// Compound V3 (Comet) interface
sol! {
    #[sol(rpc)]
    contract IComet {
        function borrowBalanceOf(address account) external view returns (uint256);
        function collateralBalanceOf(address account, address asset) external view returns (uint128);
        function isLiquidatable(address account) external view returns (bool);
        function liquidatorPoints(address liquidator) external view returns (uint32, uint128, uint128, uint104);
        function getAssetInfo(uint8 i) external view returns (
            uint8 offset,
            address asset,
            address priceFeed,
            uint64 scale,
            uint64 borrowCollateralFactor,
            uint64 liquidateCollateralFactor,
            uint64 liquidationFactor,
            uint128 supplyCap
        );
        function numAssets() external view returns (uint8);
        function baseToken() external view returns (address);
    }
}

// Euler interface
sol! {
    #[sol(rpc)]
    contract IEulerLens {
        function getAccountStatus(address account) external view returns (
            uint256 collateralValue,
            uint256 liabilityValue,
            uint256 healthScore
        );
    }
}

// Morpho interface
sol! {
    #[sol(rpc)]
    contract IMorpho {
        function userCollaterals(address user, address market) external view returns (uint256);
        function userBorrows(address user, address market) external view returns (uint256);
    }
}

/// Known protocol addresses (Ethereum mainnet)
pub mod protocols {
    use super::*;

    /// Aave V3 Pool
    pub const AAVE_V3_POOL: Address = address!("87870Bca3F3fD6335C3F4ce8392D69350B4fA4E2");
    /// Aave V3 Data Provider
    pub const AAVE_V3_DATA_PROVIDER: Address =
        address!("7B4EB56E7CD4b454BA8ff71E4518426369a138a3");

    /// Compound V3 USDC Comet
    pub const COMPOUND_V3_USDC: Address = address!("c3d688B66703497DAA19211EEdff47f25384cdc3");
    /// Compound V3 WETH Comet
    pub const COMPOUND_V3_WETH: Address = address!("A17581A9E3356d9A858b789D68B4d866e593aE94");

    /// Euler main contract
    pub const EULER_MAIN: Address = address!("27182842E098f60e3D576794A5bFFb0777E025d3");
    /// Euler Lens
    pub const EULER_LENS: Address = address!("5077B7642abF198b4a5b7C4BdCE4f03016C7089C");

    /// Morpho Blue
    pub const MORPHO_BLUE: Address = address!("BBBBBbbBBb9cC5e90e3b3Af64bdAF62C37EEFFCb");
}

/// Multicall3 address
pub const MULTICALL3_ADDRESS: Address = address!("cA11bde05977b3631167028862bE2a173976CA11");

/// Health factor threshold for liquidation alerts
pub const LIQUIDATION_THRESHOLD: f64 = 1.05;

/// Position information for tracking
#[derive(Debug, Clone)]
pub struct TrackedPosition {
    /// User address
    pub user: Address,
    /// Protocol
    pub protocol: LendingProtocol,
    /// Last known health factor
    pub last_health_factor: f64,
    /// Last check block
    pub last_check_block: u64,
    /// Protocol-specific identifier (e.g., market address for Morpho)
    pub market: Option<Address>,
}

/// Configuration for liquidation monitoring
#[derive(Debug, Clone)]
pub struct LiquidationMonitorConfig {
    /// Health factor threshold for alerts (default: 1.05)
    pub health_threshold: f64,
    /// Poll interval in milliseconds
    pub poll_interval_ms: u64,
    /// Maximum positions to check per poll
    pub max_positions_per_poll: usize,
    /// Whether to track Aave V3
    pub track_aave_v3: bool,
    /// Whether to track Compound V3
    pub track_compound_v3: bool,
    /// Whether to track Euler
    pub track_euler: bool,
    /// Whether to track Morpho
    pub track_morpho: bool,
}

impl Default for LiquidationMonitorConfig {
    fn default() -> Self {
        Self {
            health_threshold: LIQUIDATION_THRESHOLD,
            poll_interval_ms: 12000, // Every block roughly
            max_positions_per_poll: 100,
            track_aave_v3: true,
            track_compound_v3: true,
            track_euler: true,
            track_morpho: true,
        }
    }
}

/// Statistics for liquidation monitoring
#[derive(Debug, Default)]
pub struct LiquidationStats {
    /// Total positions tracked
    pub total_positions: u64,
    /// Total checks performed
    pub total_checks: u64,
    /// Liquidation candidates found
    pub candidates_found: u64,
    /// Errors encountered
    pub errors: u64,
}

/// Liquidation monitor for lending protocols
pub struct LiquidationMonitor {
    /// HTTP RPC URL
    http_url: String,
    /// Monitor name
    name: String,
    /// Configuration
    config: LiquidationMonitorConfig,
    /// Tracked positions
    positions: RwLock<HashMap<(Address, LendingProtocol), TrackedPosition>>,
    /// Whether the monitor is running
    running: AtomicBool,
    /// Stop signal sender
    stop_tx: RwLock<Option<mpsc::Sender<()>>>,
    /// Current block number
    current_block: AtomicU64,
    /// Statistics
    stats: RwLock<LiquidationStats>,
    /// Reconnection configuration
    reconnect_config: ReconnectConfig,
}

impl LiquidationMonitor {
    /// Create a new liquidation monitor
    pub fn new(http_url: String) -> Self {
        Self {
            http_url,
            name: "LiquidationMonitor".to_string(),
            config: LiquidationMonitorConfig::default(),
            positions: RwLock::new(HashMap::new()),
            running: AtomicBool::new(false),
            stop_tx: RwLock::new(None),
            current_block: AtomicU64::new(0),
            stats: RwLock::new(LiquidationStats::default()),
            reconnect_config: ReconnectConfig::default(),
        }
    }

    /// Create with custom configuration
    pub fn with_config(http_url: String, config: LiquidationMonitorConfig) -> Self {
        Self {
            http_url,
            name: "LiquidationMonitor".to_string(),
            config,
            positions: RwLock::new(HashMap::new()),
            running: AtomicBool::new(false),
            stop_tx: RwLock::new(None),
            current_block: AtomicU64::new(0),
            stats: RwLock::new(LiquidationStats::default()),
            reconnect_config: ReconnectConfig::default(),
        }
    }

    /// Track a position
    pub async fn track_position(&self, user: Address, protocol: LendingProtocol, market: Option<Address>) {
        let mut positions = self.positions.write().await;
        let position = TrackedPosition {
            user,
            protocol,
            last_health_factor: 0.0,
            last_check_block: 0,
            market,
        };
        positions.insert((user, protocol), position);

        info!(
            "{}: Now tracking {} position for {:?}",
            self.name, protocol, user
        );
    }

    /// Track multiple positions
    pub async fn track_positions(&self, users: Vec<(Address, LendingProtocol, Option<Address>)>) {
        let mut positions = self.positions.write().await;
        for (user, protocol, market) in users {
            let position = TrackedPosition {
                user,
                protocol,
                last_health_factor: 0.0,
                last_check_block: 0,
                market,
            };
            positions.insert((user, protocol), position);
        }

        info!(
            "{}: Now tracking {} positions",
            self.name,
            positions.len()
        );
    }

    /// Stop tracking a position
    pub async fn untrack_position(&self, user: Address, protocol: LendingProtocol) {
        let mut positions = self.positions.write().await;
        positions.remove(&(user, protocol));
    }

    /// Get current statistics
    pub async fn stats(&self) -> LiquidationStats {
        let stats = self.stats.read().await;
        LiquidationStats {
            total_positions: stats.total_positions,
            total_checks: stats.total_checks,
            candidates_found: stats.candidates_found,
            errors: stats.errors,
        }
    }

    /// Get the number of tracked positions
    pub async fn position_count(&self) -> usize {
        self.positions.read().await.len()
    }

    /// Set current block number
    pub fn set_current_block(&self, block: u64) {
        self.current_block.store(block, Ordering::Relaxed);
    }

    /// Check Aave V3 positions
    async fn check_aave_v3_positions(
        &self,
        provider: &RootProvider<Http<Client>>,
        users: &[Address],
    ) -> Vec<(Address, f64, f64, f64)> {
        if users.is_empty() {
            return Vec::new();
        }

        // Build multicall for getUserAccountData
        let selector = hex::decode("bf92857c").unwrap(); // getUserAccountData(address)

        let calls: Vec<Call3> = users
            .iter()
            .map(|user| {
                let mut calldata = selector.clone();
                calldata.extend_from_slice(&[0u8; 12]);
                calldata.extend_from_slice(user.as_slice());
                Call3 {
                    target: protocols::AAVE_V3_POOL,
                    allowFailure: true,
                    callData: Bytes::from(calldata),
                }
            })
            .collect();

        let multicall = Multicall3::new(MULTICALL3_ADDRESS, provider);
        let results = match multicall.aggregate3(calls).call().await {
            Ok(result) => result.returnData,
            Err(e) => {
                warn!("{}: Aave V3 multicall failed: {:?}", self.name, e);
                return Vec::new();
            }
        };

        let mut checked = Vec::new();

        for (i, result) in results.iter().enumerate() {
            if !result.success || result.returnData.len() < 192 {
                continue;
            }

            let data = &result.returnData;
            let total_collateral = U256::from_be_slice(&data[0..32]);
            let total_debt = U256::from_be_slice(&data[32..64]);
            // availableBorrowsBase at 64..96
            // currentLiquidationThreshold at 96..128
            // ltv at 128..160
            let health_factor_raw = U256::from_be_slice(&data[160..192]);

            // Health factor is in 18 decimals (1e18 = 1.0)
            let health_factor = health_factor_raw.to::<u128>() as f64 / 1e18;

            // Values are in 8 decimals (base currency decimals)
            let collateral_usd = total_collateral.to::<u128>() as f64 / 1e8;
            let debt_usd = total_debt.to::<u128>() as f64 / 1e8;

            checked.push((users[i], health_factor, collateral_usd, debt_usd));
        }

        checked
    }

    /// Check Compound V3 positions
    async fn check_compound_v3_positions(
        &self,
        provider: &RootProvider<Http<Client>>,
        users: &[Address],
        comet: Address,
    ) -> Vec<(Address, bool)> {
        if users.is_empty() {
            return Vec::new();
        }

        // Build multicall for isLiquidatable
        let selector = hex::decode("17db5c02").unwrap(); // isLiquidatable(address)

        let calls: Vec<Call3> = users
            .iter()
            .map(|user| {
                let mut calldata = selector.clone();
                calldata.extend_from_slice(&[0u8; 12]);
                calldata.extend_from_slice(user.as_slice());
                Call3 {
                    target: comet,
                    allowFailure: true,
                    callData: Bytes::from(calldata),
                }
            })
            .collect();

        let multicall = Multicall3::new(MULTICALL3_ADDRESS, provider);
        let results = match multicall.aggregate3(calls).call().await {
            Ok(result) => result.returnData,
            Err(e) => {
                warn!("{}: Compound V3 multicall failed: {:?}", self.name, e);
                return Vec::new();
            }
        };

        let mut checked = Vec::new();

        for (i, result) in results.iter().enumerate() {
            if !result.success || result.returnData.len() < 32 {
                continue;
            }

            let is_liquidatable = result.returnData[31] != 0;
            checked.push((users[i], is_liquidatable));
        }

        checked
    }

    /// Check Euler positions
    async fn check_euler_positions(
        &self,
        provider: &RootProvider<Http<Client>>,
        users: &[Address],
    ) -> Vec<(Address, f64, f64, f64)> {
        if users.is_empty() {
            return Vec::new();
        }

        // Build multicall for getAccountStatus
        let selector = hex::decode("7de12362").unwrap(); // getAccountStatus(address)

        let calls: Vec<Call3> = users
            .iter()
            .map(|user| {
                let mut calldata = selector.clone();
                calldata.extend_from_slice(&[0u8; 12]);
                calldata.extend_from_slice(user.as_slice());
                Call3 {
                    target: protocols::EULER_LENS,
                    allowFailure: true,
                    callData: Bytes::from(calldata),
                }
            })
            .collect();

        let multicall = Multicall3::new(MULTICALL3_ADDRESS, provider);
        let results = match multicall.aggregate3(calls).call().await {
            Ok(result) => result.returnData,
            Err(e) => {
                warn!("{}: Euler multicall failed: {:?}", self.name, e);
                return Vec::new();
            }
        };

        let mut checked = Vec::new();

        for (i, result) in results.iter().enumerate() {
            if !result.success || result.returnData.len() < 96 {
                continue;
            }

            let data = &result.returnData;
            let collateral_value = U256::from_be_slice(&data[0..32]);
            let liability_value = U256::from_be_slice(&data[32..64]);
            let health_score = U256::from_be_slice(&data[64..96]);

            // Health score is in 18 decimals
            let health_factor = health_score.to::<u128>() as f64 / 1e18;
            let collateral_usd = collateral_value.to::<u128>() as f64 / 1e18;
            let debt_usd = liability_value.to::<u128>() as f64 / 1e18;

            checked.push((users[i], health_factor, collateral_usd, debt_usd));
        }

        checked
    }

    /// Estimate liquidation profit
    fn estimate_profit(&self, protocol: LendingProtocol, debt_usd: f64) -> f64 {
        // Liquidation bonus varies by protocol and asset
        // This is a simplified estimation
        let bonus_pct = match protocol {
            LendingProtocol::AaveV3 => 0.05,     // ~5% average
            LendingProtocol::CompoundV3 => 0.08, // ~8%
            LendingProtocol::Euler => 0.10,      // ~10%
            LendingProtocol::Morpho => 0.05,     // ~5%
        };

        // Max liquidation is typically 50% of debt
        let max_liquidation = debt_usd * 0.5;
        max_liquidation * bonus_pct
    }

    /// Poll all tracked positions
    async fn poll_positions(
        &self,
        provider: &RootProvider<Http<Client>>,
        event_tx: &mpsc::Sender<MonitorEvent>,
    ) -> Result<()> {
        let positions = self.positions.read().await;
        if positions.is_empty() {
            return Ok(());
        }

        let current_block = self.current_block.load(Ordering::Relaxed);

        // Group positions by protocol
        let mut aave_users: Vec<Address> = Vec::new();
        let mut compound_usdc_users: Vec<Address> = Vec::new();
        let mut compound_weth_users: Vec<Address> = Vec::new();
        let mut euler_users: Vec<Address> = Vec::new();

        for ((user, protocol), _) in positions.iter() {
            match protocol {
                LendingProtocol::AaveV3 if self.config.track_aave_v3 => {
                    aave_users.push(*user);
                }
                LendingProtocol::CompoundV3 if self.config.track_compound_v3 => {
                    // For simplicity, check both comets
                    compound_usdc_users.push(*user);
                    compound_weth_users.push(*user);
                }
                LendingProtocol::Euler if self.config.track_euler => {
                    euler_users.push(*user);
                }
                _ => {}
            }
        }

        drop(positions);

        // Check Aave V3 positions
        let aave_results = self.check_aave_v3_positions(provider, &aave_users).await;
        for (user, health_factor, collateral_usd, debt_usd) in aave_results {
            let mut stats = self.stats.write().await;
            stats.total_checks += 1;

            if health_factor > 0.0 && health_factor < self.config.health_threshold {
                stats.candidates_found += 1;

                let info = LiquidationInfo {
                    protocol: LendingProtocol::AaveV3,
                    user,
                    health_factor,
                    collateral_value_usd: collateral_usd,
                    debt_value_usd: debt_usd,
                    max_liquidation_usd: debt_usd * 0.5,
                    collateral_assets: Vec::new(), // Would need additional calls to populate
                    debt_assets: Vec::new(),
                    block_number: current_block,
                    estimated_profit_usd: self.estimate_profit(LendingProtocol::AaveV3, debt_usd),
                };

                debug!(
                    "{}: Aave V3 liquidation candidate - user: {:?}, health: {:.4}, debt: ${:.2}",
                    self.name, user, health_factor, debt_usd
                );

                if let Err(e) = event_tx
                    .send(MonitorEvent::LiquidationCandidate(info))
                    .await
                {
                    error!("{}: Failed to send liquidation event: {:?}", self.name, e);
                }
            }

            // Update position state
            let mut positions = self.positions.write().await;
            if let Some(pos) = positions.get_mut(&(user, LendingProtocol::AaveV3)) {
                pos.last_health_factor = health_factor;
                pos.last_check_block = current_block;
            }
        }

        // Check Compound V3 USDC positions
        let compound_usdc_results = self
            .check_compound_v3_positions(provider, &compound_usdc_users, protocols::COMPOUND_V3_USDC)
            .await;
        for (user, is_liquidatable) in compound_usdc_results {
            let mut stats = self.stats.write().await;
            stats.total_checks += 1;

            if is_liquidatable {
                stats.candidates_found += 1;

                let info = LiquidationInfo {
                    protocol: LendingProtocol::CompoundV3,
                    user,
                    health_factor: 0.99, // Below 1.0
                    collateral_value_usd: 0.0,
                    debt_value_usd: 0.0,
                    max_liquidation_usd: 0.0,
                    collateral_assets: Vec::new(),
                    debt_assets: Vec::new(),
                    block_number: current_block,
                    estimated_profit_usd: 0.0,
                };

                debug!(
                    "{}: Compound V3 liquidation candidate - user: {:?}",
                    self.name, user
                );

                if let Err(e) = event_tx
                    .send(MonitorEvent::LiquidationCandidate(info))
                    .await
                {
                    error!("{}: Failed to send liquidation event: {:?}", self.name, e);
                }
            }
        }

        // Check Compound V3 WETH positions
        let compound_weth_results = self
            .check_compound_v3_positions(provider, &compound_weth_users, protocols::COMPOUND_V3_WETH)
            .await;
        for (user, is_liquidatable) in compound_weth_results {
            let mut stats = self.stats.write().await;
            stats.total_checks += 1;

            if is_liquidatable {
                stats.candidates_found += 1;

                let info = LiquidationInfo {
                    protocol: LendingProtocol::CompoundV3,
                    user,
                    health_factor: 0.99,
                    collateral_value_usd: 0.0,
                    debt_value_usd: 0.0,
                    max_liquidation_usd: 0.0,
                    collateral_assets: Vec::new(),
                    debt_assets: Vec::new(),
                    block_number: current_block,
                    estimated_profit_usd: 0.0,
                };

                debug!(
                    "{}: Compound V3 WETH liquidation candidate - user: {:?}",
                    self.name, user
                );

                if let Err(e) = event_tx
                    .send(MonitorEvent::LiquidationCandidate(info))
                    .await
                {
                    error!("{}: Failed to send liquidation event: {:?}", self.name, e);
                }
            }
        }

        // Check Euler positions
        let euler_results = self.check_euler_positions(provider, &euler_users).await;
        for (user, health_factor, collateral_usd, debt_usd) in euler_results {
            let mut stats = self.stats.write().await;
            stats.total_checks += 1;

            if health_factor > 0.0 && health_factor < self.config.health_threshold {
                stats.candidates_found += 1;

                let info = LiquidationInfo {
                    protocol: LendingProtocol::Euler,
                    user,
                    health_factor,
                    collateral_value_usd: collateral_usd,
                    debt_value_usd: debt_usd,
                    max_liquidation_usd: debt_usd * 0.5,
                    collateral_assets: Vec::new(),
                    debt_assets: Vec::new(),
                    block_number: current_block,
                    estimated_profit_usd: self.estimate_profit(LendingProtocol::Euler, debt_usd),
                };

                debug!(
                    "{}: Euler liquidation candidate - user: {:?}, health: {:.4}, debt: ${:.2}",
                    self.name, user, health_factor, debt_usd
                );

                if let Err(e) = event_tx
                    .send(MonitorEvent::LiquidationCandidate(info))
                    .await
                {
                    error!("{}: Failed to send liquidation event: {:?}", self.name, e);
                }
            }

            // Update position state
            let mut positions = self.positions.write().await;
            if let Some(pos) = positions.get_mut(&(user, LendingProtocol::Euler)) {
                pos.last_health_factor = health_factor;
                pos.last_check_block = current_block;
            }
        }

        // Update total positions count
        {
            let mut stats = self.stats.write().await;
            stats.total_positions = self.positions.read().await.len() as u64;
        }

        Ok(())
    }

    /// Main monitoring loop
    async fn run_monitoring_loop(
        &self,
        event_tx: mpsc::Sender<MonitorEvent>,
        mut stop_rx: mpsc::Receiver<()>,
    ) {
        let provider = ProviderBuilder::new().on_http(self.http_url.parse().unwrap());

        let poll_interval = Duration::from_millis(self.config.poll_interval_ms);

        info!(
            "{}: Starting liquidation monitoring with {} ms interval",
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

                    if let Err(e) = self.poll_positions(&provider, &event_tx).await {
                        warn!("{}: Error polling positions: {:?}", self.name, e);
                        let mut stats = self.stats.write().await;
                        stats.errors += 1;
                    }
                }
            }
        }
    }
}

#[async_trait]
impl Monitor for LiquidationMonitor {
    fn name(&self) -> &str {
        &self.name
    }

    async fn start(&self, tx: mpsc::Sender<MonitorEvent>) -> Result<()> {
        if self.running.swap(true, Ordering::Relaxed) {
            return Err(MevError::Provider(ProviderError::SubscriptionError(
                "Liquidation monitor is already running".to_string(),
            )));
        }

        let (stop_tx, stop_rx) = mpsc::channel(1);
        {
            let mut guard = self.stop_tx.write().await;
            *guard = Some(stop_tx);
        }

        let self_ref = unsafe { &*(self as *const LiquidationMonitor) };

        tokio::spawn(async move {
            self_ref.run_monitoring_loop(tx, stop_rx).await;
        });

        Ok(())
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

/// Safe Arc wrapper for LiquidationMonitor
pub struct ArcLiquidationMonitor(pub Arc<LiquidationMonitor>);

impl ArcLiquidationMonitor {
    pub fn new(http_url: String) -> Self {
        Self(Arc::new(LiquidationMonitor::new(http_url)))
    }

    pub async fn start(&self, tx: mpsc::Sender<MonitorEvent>) -> Result<()> {
        let monitor = Arc::clone(&self.0);

        if monitor.running.swap(true, Ordering::Relaxed) {
            return Err(MevError::Provider(ProviderError::SubscriptionError(
                "Liquidation monitor is already running".to_string(),
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
    fn test_protocol_addresses() {
        assert_ne!(protocols::AAVE_V3_POOL, Address::ZERO);
        assert_ne!(protocols::COMPOUND_V3_USDC, Address::ZERO);
        assert_ne!(protocols::EULER_LENS, Address::ZERO);
        assert_ne!(protocols::MORPHO_BLUE, Address::ZERO);
    }

    #[test]
    fn test_liquidation_config_default() {
        let config = LiquidationMonitorConfig::default();
        assert_eq!(config.health_threshold, 1.05);
        assert!(config.track_aave_v3);
        assert!(config.track_compound_v3);
        assert!(config.track_euler);
        assert!(config.track_morpho);
    }

    #[tokio::test]
    async fn test_liquidation_monitor_creation() {
        let monitor = LiquidationMonitor::new("http://localhost:8545".to_string());
        assert!(!monitor.is_running());
        assert_eq!(monitor.position_count().await, 0);
    }

    #[tokio::test]
    async fn test_position_tracking() {
        let monitor = LiquidationMonitor::new("http://localhost:8545".to_string());

        let user = Address::ZERO;
        monitor.track_position(user, LendingProtocol::AaveV3, None).await;

        assert_eq!(monitor.position_count().await, 1);

        monitor.untrack_position(user, LendingProtocol::AaveV3).await;
        assert_eq!(monitor.position_count().await, 0);
    }

    #[test]
    fn test_profit_estimation() {
        let monitor = LiquidationMonitor::new("http://localhost:8545".to_string());

        let profit = monitor.estimate_profit(LendingProtocol::AaveV3, 10000.0);
        // 50% max liquidation * 5% bonus = 2.5% of debt
        assert!((profit - 250.0).abs() < 0.01);

        let profit = monitor.estimate_profit(LendingProtocol::Euler, 10000.0);
        // 50% max liquidation * 10% bonus = 5% of debt
        assert!((profit - 500.0).abs() < 0.01);
    }

    #[test]
    fn test_lending_protocol_display() {
        assert_eq!(format!("{}", LendingProtocol::AaveV3), "Aave V3");
        assert_eq!(format!("{}", LendingProtocol::CompoundV3), "Compound V3");
        assert_eq!(format!("{}", LendingProtocol::Euler), "Euler");
        assert_eq!(format!("{}", LendingProtocol::Morpho), "Morpho");
    }
}
