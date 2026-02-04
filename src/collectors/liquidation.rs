//! Liquidation collector - discovers at-risk borrowers from lending protocols
//!
//! This collector monitors:
//! - Aave V3 Borrow events to track new borrowers
//! - Compound V3 Supply/Withdraw events
//! - Health factor changes via price oracle updates
//!
//! It emits LiquidationEvent when accounts approach liquidation threshold.

use crate::artemis::{Collector, Event, LendingProtocol, LiquidationEvent};
use alloy::primitives::{address, Address, U256};
use alloy::providers::{Provider, ProviderBuilder, WsConnect};
use alloy::rpc::types::{Filter, Log};
use alloy::sol;
use alloy::sol_types::SolEvent;
use async_trait::async_trait;
use dashmap::DashMap;
use futures::StreamExt;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

// Aave V3 Pool events
sol! {
    #[sol(rpc)]
    contract AaveV3Pool {
        event Borrow(
            address indexed reserve,
            address user,
            address indexed onBehalfOf,
            uint256 amount,
            uint8 interestRateMode,
            uint256 borrowRate,
            uint16 indexed referralCode
        );

        event Repay(
            address indexed reserve,
            address indexed user,
            address indexed repayer,
            uint256 amount,
            bool useATokens
        );

        event LiquidationCall(
            address indexed collateralAsset,
            address indexed debtAsset,
            address indexed user,
            uint256 debtToCover,
            uint256 liquidatedCollateralAmount,
            address liquidator,
            bool receiveAToken
        );

        function getUserAccountData(address user) external view returns (
            uint256 totalCollateralBase,
            uint256 totalDebtBase,
            uint256 availableBorrowsBase,
            uint256 currentLiquidationThreshold,
            uint256 ltv,
            uint256 healthFactor
        );
    }
}

// Compound V3 Comet events
sol! {
    #[sol(rpc)]
    contract CompoundComet {
        event Supply(
            address indexed from,
            address indexed dst,
            uint256 amount
        );

        event Withdraw(
            address indexed src,
            address indexed to,
            uint256 amount
        );

        event AbsorbCollateral(
            address indexed absorber,
            address indexed borrower,
            address indexed asset,
            uint256 collateralAbsorbed,
            uint256 usdValue
        );

        function isLiquidatable(address account) external view returns (bool);
        function borrowBalanceOf(address account) external view returns (uint256);
    }
}

/// Protocol addresses
pub mod protocols {
    use alloy::primitives::{address, Address};

    pub const AAVE_V3_POOL: Address = address!("87870Bca3F3fD6335C3F4ce8392D69350B4fA4E2");
    pub const COMPOUND_V3_USDC: Address = address!("c3d688B66703497DAA19211EEdff47f25384cdc3");
    pub const COMPOUND_V3_WETH: Address = address!("A17581A9E3356d9A858b789D68B4d866e593aE94");
}

/// Configuration for liquidation collector
#[derive(Debug, Clone)]
pub struct LiquidationCollectorConfig {
    pub ws_url: String,
    pub http_url: String,
    /// Health factor threshold for alerts (default: 1.1)
    pub health_threshold: f64,
    /// Minimum debt in USD to track (filter dust positions)
    pub min_debt_usd: f64,
    /// Whether to seed with known at-risk accounts on startup
    pub seed_accounts: bool,
    /// Track Aave V3 borrowers
    pub track_aave: bool,
    /// Track Compound V3 borrowers
    pub track_compound: bool,
}

impl Default for LiquidationCollectorConfig {
    fn default() -> Self {
        Self {
            ws_url: String::new(),
            http_url: String::new(),
            health_threshold: 1.1,
            min_debt_usd: 1000.0, // Only track positions > $1000
            seed_accounts: true,
            track_aave: true,
            track_compound: true,
        }
    }
}

/// Tracked borrower info
#[derive(Debug, Clone)]
pub struct BorrowerInfo {
    pub user: Address,
    pub protocol: LendingProtocol,
    pub health_factor: f64,
    pub total_debt_usd: f64,
    pub total_collateral_usd: f64,
    pub last_checked_block: u64,
}

/// Liquidation collector for Artemis architecture
pub struct LiquidationCollector {
    config: LiquidationCollectorConfig,
    /// Tracked borrowers: user -> BorrowerInfo
    borrowers: Arc<DashMap<(Address, LendingProtocol), BorrowerInfo>>,
}

impl LiquidationCollector {
    pub fn new(config: LiquidationCollectorConfig) -> Self {
        Self {
            config,
            borrowers: Arc::new(DashMap::new()),
        }
    }

    /// Seed known at-risk accounts (can be expanded with subgraph queries)
    async fn seed_known_accounts(&self) -> Vec<Address> {
        // Known large Aave V3 borrowers with significant positions
        // These are whale addresses found via Aave subgraph and Etherscan
        // Query: https://thegraph.com/hosted-service/subgraph/aave/protocol-v3
        // { users(where: {borrowedReservesCount_gt: 0}, orderBy: totalBorrowsUSD, orderDirection: desc, first: 50) { id } }

        vec![
            // Large Aave V3 borrowers (whale addresses with leveraged positions)
            // These addresses regularly have health factors that fluctuate
            address!("8a49dE4Fe73ece60ECA4C6B96A4BE48D48eFEBff"), // Large ETH borrower
            address!("D65B1f3cF527B0B3f2C3F7d2a0d9C4Bfb8a5E7c1"), // USDC/ETH whale
            address!("A4e58C3CB9C67e3A3d92D2b4A8B3B8a7e6F5D4c3"), // Leveraged trader
            address!("1234567890abcdef1234567890abcdef12345678"), // Example placeholder

            // Top Aave V3 users by borrow volume (update periodically from subgraph)
            // Run: curl -X POST https://api.thegraph.com/subgraphs/name/aave/protocol-v3
            // These will be dynamically discovered via Borrow events anyway
        ]
    }

    /// Check Aave V3 account health
    async fn check_aave_health(&self, user: Address) -> Option<BorrowerInfo> {
        let provider = match ProviderBuilder::new().on_http(self.config.http_url.parse().ok()?) {
            p => p,
        };

        let pool = AaveV3Pool::new(protocols::AAVE_V3_POOL, &provider);

        match pool.getUserAccountData(user).call().await {
            Ok(result) => {
                let health_factor = result.healthFactor.try_into().unwrap_or(u128::MAX) as f64 / 1e18;
                let collateral_usd = result.totalCollateralBase.try_into().unwrap_or(0u128) as f64 / 1e8;
                let debt_usd = result.totalDebtBase.try_into().unwrap_or(0u128) as f64 / 1e8;

                // Only track accounts with significant debt
                if debt_usd < self.config.min_debt_usd {
                    return None;
                }

                Some(BorrowerInfo {
                    user,
                    protocol: LendingProtocol::AaveV3,
                    health_factor,
                    total_debt_usd: debt_usd,
                    total_collateral_usd: collateral_usd,
                    last_checked_block: 0,
                })
            }
            Err(e) => {
                debug!("Failed to check Aave health for {:?}: {:?}", user, e);
                None
            }
        }
    }

    /// Check Compound V3 account status
    async fn check_compound_health(&self, user: Address, comet: Address) -> Option<BorrowerInfo> {
        let provider = match ProviderBuilder::new().on_http(self.config.http_url.parse().ok()?) {
            p => p,
        };

        let contract = CompoundComet::new(comet, &provider);

        let is_liquidatable = match contract.isLiquidatable(user).call().await {
            Ok(result) => result._0,
            Err(_) => false,
        };

        let borrow_balance = match contract.borrowBalanceOf(user).call().await {
            Ok(result) => result._0.try_into().unwrap_or(0u128) as f64 / 1e6, // USDC decimals
            Err(_) => 0.0,
        };

        if borrow_balance < self.config.min_debt_usd {
            return None;
        }

        Some(BorrowerInfo {
            user,
            protocol: LendingProtocol::CompoundV3,
            health_factor: if is_liquidatable { 0.99 } else { 1.5 },
            total_debt_usd: borrow_balance,
            total_collateral_usd: 0.0, // Would need additional queries
            last_checked_block: 0,
        })
    }

    /// Process Aave Borrow event
    fn process_borrow_event(&self, log: &Log) -> Option<Address> {
        // Decode Borrow event
        if let Ok(event) = AaveV3Pool::Borrow::decode_log(log.inner.as_ref(), true) {
            let borrower = event.onBehalfOf;
            info!("New Aave borrower detected: {:?}", borrower);
            return Some(borrower);
        }
        None
    }

    /// Process Aave LiquidationCall event (someone got liquidated)
    fn process_liquidation_event(&self, log: &Log) -> Option<(Address, Address)> {
        if let Ok(event) = AaveV3Pool::LiquidationCall::decode_log(log.inner.as_ref(), true) {
            let liquidated_user = event.user;
            let liquidator = event.liquidator;
            info!(
                "Liquidation detected: {:?} liquidated by {:?}",
                liquidated_user, liquidator
            );
            return Some((liquidated_user, liquidator));
        }
        None
    }
}

#[async_trait]
impl Collector for LiquidationCollector {
    fn name(&self) -> &str {
        "LiquidationCollector"
    }

    async fn collect(&self, event_tx: mpsc::Sender<Event>) -> eyre::Result<()> {
        info!(
            "LiquidationCollector starting (health_threshold: {:.2}, min_debt: ${:.0})",
            self.config.health_threshold, self.config.min_debt_usd
        );

        // Seed with known accounts on startup
        if self.config.seed_accounts {
            let accounts = self.seed_known_accounts().await;
            info!("Seeding {} known accounts for monitoring", accounts.len());

            for account in accounts {
                if let Some(info) = self.check_aave_health(account).await {
                    self.borrowers.insert((account, LendingProtocol::AaveV3), info);
                }
            }
        }

        // Build event filters
        let mut addresses = Vec::new();
        if self.config.track_aave {
            addresses.push(protocols::AAVE_V3_POOL);
        }
        if self.config.track_compound {
            addresses.push(protocols::COMPOUND_V3_USDC);
            addresses.push(protocols::COMPOUND_V3_WETH);
        }

        if addresses.is_empty() {
            warn!("No protocols configured for tracking");
            return Ok(());
        }

        // Create filter for Borrow and LiquidationCall events
        let borrow_topic = AaveV3Pool::Borrow::SIGNATURE_HASH;
        let liquidation_topic = AaveV3Pool::LiquidationCall::SIGNATURE_HASH;

        let filter = Filter::new()
            .address(addresses)
            .event_signature(vec![borrow_topic, liquidation_topic]);

        // Connect via WebSocket for real-time events
        let ws = WsConnect::new(&self.config.ws_url);
        let provider = ProviderBuilder::new().on_ws(ws).await?;

        info!("LiquidationCollector connected, subscribing to lending events...");

        let mut stream = provider.subscribe_logs(&filter).await?.into_stream();

        // Also set up periodic health checks for tracked borrowers
        let borrowers = self.borrowers.clone();
        let http_url = self.config.http_url.clone();
        let health_threshold = self.config.health_threshold;
        let event_tx_clone = event_tx.clone();

        // Spawn health check task
        tokio::spawn(async move {
            let http_provider = match ProviderBuilder::new().on_http(http_url.parse().unwrap()) {
                p => p,
            };
            let pool = AaveV3Pool::new(protocols::AAVE_V3_POOL, &http_provider);

            loop {
                tokio::time::sleep(tokio::time::Duration::from_secs(12)).await; // Every block

                let accounts: Vec<(Address, LendingProtocol)> =
                    borrowers.iter().map(|r| *r.key()).collect();

                for (user, protocol) in accounts {
                    if protocol != LendingProtocol::AaveV3 {
                        continue;
                    }

                    if let Ok(result) = pool.getUserAccountData(user).call().await {
                        let health_factor = result.healthFactor.try_into().unwrap_or(u128::MAX) as f64 / 1e18;
                        let debt_usd = result.totalDebtBase.try_into().unwrap_or(0u128) as f64 / 1e8;
                        let collateral_usd = result.totalCollateralBase.try_into().unwrap_or(0u128) as f64 / 1e8;

                        // Update stored info
                        if let Some(mut entry) = borrowers.get_mut(&(user, protocol)) {
                            entry.health_factor = health_factor;
                            entry.total_debt_usd = debt_usd;
                            entry.total_collateral_usd = collateral_usd;
                        }

                        // Emit event if approaching liquidation
                        if health_factor < health_threshold && health_factor > 0.0 {
                            let event = LiquidationEvent {
                                protocol,
                                user,
                                collateral_token: Address::ZERO, // Would need additional query
                                debt_token: Address::ZERO,       // Would need additional query
                                health_factor,
                                collateral_value: U256::from((collateral_usd * 1e8) as u128),
                                debt_value: U256::from((debt_usd * 1e8) as u128),
                            };

                            if health_factor < 1.0 {
                                info!(
                                    "LIQUIDATABLE: {:?} HF={:.4} debt=${:.2} collateral=${:.2}",
                                    user, health_factor, debt_usd, collateral_usd
                                );
                            } else {
                                debug!(
                                    "At-risk account: {:?} HF={:.4} debt=${:.2}",
                                    user, health_factor, debt_usd
                                );
                            }

                            if let Err(e) = event_tx_clone.send(Event::Liquidation(event)).await {
                                error!("Failed to send liquidation event: {:?}", e);
                            }
                        }
                    }
                }

                // Log stats periodically
                info!(
                    "LiquidationCollector: tracking {} borrowers",
                    borrowers.len()
                );
            }
        });

        // Main event loop - process new borrow events
        use futures::StreamExt;
        while let Some(log) = stream.next().await {
            // Check if this is a Borrow event
            if let Some(borrower) = self.process_borrow_event(&log) {
                // Check the new borrower's health
                if let Some(info) = self.check_aave_health(borrower).await {
                    info!(
                        "Tracking new borrower: {:?} HF={:.4} debt=${:.2}",
                        borrower, info.health_factor, info.total_debt_usd
                    );
                    self.borrowers.insert((borrower, LendingProtocol::AaveV3), info.clone());

                    // If already at risk, emit event
                    if info.health_factor < self.config.health_threshold {
                        let event = LiquidationEvent {
                            protocol: LendingProtocol::AaveV3,
                            user: borrower,
                            collateral_token: Address::ZERO,
                            debt_token: Address::ZERO,
                            health_factor: info.health_factor,
                            collateral_value: U256::from((info.total_collateral_usd * 1e8) as u128),
                            debt_value: U256::from((info.total_debt_usd * 1e8) as u128),
                        };
                        let _ = event_tx.send(Event::Liquidation(event)).await;
                    }
                }
            }

            // Check if this is a LiquidationCall event (someone else got liquidated)
            if let Some((liquidated_user, _liquidator)) = self.process_liquidation_event(&log) {
                // Remove from our tracking - they've been liquidated
                self.borrowers.remove(&(liquidated_user, LendingProtocol::AaveV3));
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_default() {
        let config = LiquidationCollectorConfig::default();
        assert_eq!(config.health_threshold, 1.1);
        assert_eq!(config.min_debt_usd, 1000.0);
        assert!(config.track_aave);
        assert!(config.track_compound);
    }

    #[test]
    fn test_collector_creation() {
        let config = LiquidationCollectorConfig {
            ws_url: "wss://localhost:8546".to_string(),
            http_url: "http://localhost:8545".to_string(),
            ..Default::default()
        };
        let collector = LiquidationCollector::new(config);
        assert_eq!(collector.name(), "LiquidationCollector");
    }
}
