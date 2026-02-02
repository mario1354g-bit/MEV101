//! Liquidation strategy - monitors lending protocols for liquidation opportunities

use crate::artemis::{Action, Event, LendingProtocol, LiquidationAction, LiquidationEvent, Strategy};
use alloy::primitives::{address, Address, U256};
use alloy::providers::{Provider, ProviderBuilder};
use alloy::sol;
use async_trait::async_trait;
use dashmap::DashMap;
use std::sync::Arc;
use tracing::{debug, info, warn};

// Aave V3 Pool contract
sol! {
    #[sol(rpc)]
    contract AaveV3Pool {
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

// Compound V3 Comet contract
sol! {
    #[sol(rpc)]
    contract CompoundComet {
        function isLiquidatable(address account) external view returns (bool);
        function borrowBalanceOf(address account) external view returns (uint256);
        function collateralBalanceOf(address account, address asset) external view returns (uint128);
    }
}

/// Lending protocol addresses
pub mod protocols {
    use alloy::primitives::{address, Address};

    pub const AAVE_V3_POOL: Address =
        address!("87870Bca3F3fD6335C3F4ce8392D69350B4fA4E2");
    pub const COMPOUND_V3_USDC: Address =
        address!("c3d688B66703497DAA19211EEdff47f25384cdc3");
    pub const COMPOUND_V3_WETH: Address =
        address!("A17581A9E3356d9A858b789D68B4d866e593aE94");
}

/// Liquidation strategy configuration
#[derive(Debug, Clone)]
pub struct LiquidationStrategyConfig {
    pub rpc_url: String,
    pub min_profit_eth: f64,
    pub health_factor_threshold: f64, // Watch accounts below this HF
    pub watched_accounts: Vec<Address>,
    pub use_flashloan: bool,
}

impl Default for LiquidationStrategyConfig {
    fn default() -> Self {
        Self {
            rpc_url: String::new(),
            min_profit_eth: 0.01,
            health_factor_threshold: 1.1, // Watch accounts with HF < 1.1
            watched_accounts: Vec::new(),
            use_flashloan: true,
        }
    }
}

/// Account health data
#[derive(Debug, Clone)]
pub struct AccountHealth {
    pub user: Address,
    pub protocol: LendingProtocol,
    pub health_factor: f64,
    pub total_collateral: U256,
    pub total_debt: U256,
    pub is_liquidatable: bool,
    pub last_checked: chrono::DateTime<chrono::Utc>,
}

/// Liquidation strategy
pub struct LiquidationStrategy {
    config: LiquidationStrategyConfig,
    // User -> AccountHealth
    watched_accounts: Arc<DashMap<Address, AccountHealth>>,
    check_count: std::sync::atomic::AtomicU64,
    opportunity_count: std::sync::atomic::AtomicU64,
}

impl LiquidationStrategy {
    pub fn new(config: LiquidationStrategyConfig) -> Self {
        let watched = Arc::new(DashMap::new());

        // Initialize watched accounts
        for account in &config.watched_accounts {
            watched.insert(
                *account,
                AccountHealth {
                    user: *account,
                    protocol: LendingProtocol::AaveV3,
                    health_factor: 999.0,
                    total_collateral: U256::ZERO,
                    total_debt: U256::ZERO,
                    is_liquidatable: false,
                    last_checked: chrono::Utc::now(),
                },
            );
        }

        Self {
            config,
            watched_accounts: watched,
            check_count: std::sync::atomic::AtomicU64::new(0),
            opportunity_count: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Check account health on Aave V3
    async fn check_aave_health(&self, user: Address) -> eyre::Result<Option<AccountHealth>> {
        let provider = ProviderBuilder::new().on_http(self.config.rpc_url.parse()?);
        let pool = AaveV3Pool::new(protocols::AAVE_V3_POOL, &provider);

        let result = pool.getUserAccountData(user).call().await?;

        let health_factor = result.healthFactor.try_into().unwrap_or(u128::MAX) as f64 / 1e18;
        let is_liquidatable = health_factor < 1.0;

        Ok(Some(AccountHealth {
            user,
            protocol: LendingProtocol::AaveV3,
            health_factor,
            total_collateral: result.totalCollateralBase,
            total_debt: result.totalDebtBase,
            is_liquidatable,
            last_checked: chrono::Utc::now(),
        }))
    }

    /// Check account health on Compound V3
    async fn check_compound_health(&self, user: Address) -> eyre::Result<Option<AccountHealth>> {
        let provider = ProviderBuilder::new().on_http(self.config.rpc_url.parse()?);
        let comet = CompoundComet::new(protocols::COMPOUND_V3_USDC, &provider);

        let is_liquidatable = comet.isLiquidatable(user).call().await?._0;
        let debt = comet.borrowBalanceOf(user).call().await?._0;

        Ok(Some(AccountHealth {
            user,
            protocol: LendingProtocol::CompoundV3,
            health_factor: if is_liquidatable { 0.99 } else { 1.5 },
            total_collateral: U256::ZERO, // Would need to sum collaterals
            total_debt: debt,
            is_liquidatable,
            last_checked: chrono::Utc::now(),
        }))
    }

    /// Create liquidation action
    fn create_liquidation_action(&self, health: &AccountHealth) -> Option<LiquidationAction> {
        if !health.is_liquidatable {
            return None;
        }

        // Calculate max liquidation (50% of debt on Aave)
        let debt_to_cover = health.total_debt / U256::from(2);

        // Estimate profit (liquidation bonus ~5-10%)
        let estimated_profit = debt_to_cover * U256::from(5) / U256::from(100);
        let profit_eth = estimated_profit.try_into().unwrap_or(0u128) as f64 / 1e18;

        if profit_eth < self.config.min_profit_eth {
            return None;
        }

        let id = format!(
            "liq-{:?}-{}",
            health.user,
            chrono::Utc::now().timestamp_millis()
        );

        info!(
            "LIQUIDATION OPPORTUNITY: {} | protocol: {} | HF: {:.4} | debt: {:?} | profit: {:.4} ETH",
            id, health.protocol, health.health_factor, health.total_debt, profit_eth
        );

        Some(LiquidationAction {
            id,
            protocol: health.protocol,
            user: health.user,
            collateral_token: Address::ZERO, // Would need to determine best collateral
            debt_token: Address::ZERO,       // Would need to determine debt token
            debt_to_cover,
            expected_collateral: debt_to_cover + estimated_profit,
            expected_profit: estimated_profit,
            use_flashloan: self.config.use_flashloan,
            gas_price: 30_000_000_000,
            priority_fee: 5_000_000_000, // Higher priority for liquidations
        })
    }
}

#[async_trait]
impl Strategy for LiquidationStrategy {
    fn name(&self) -> &str {
        "LiquidationStrategy"
    }

    async fn process_event(&self, event: &Event) -> eyre::Result<Option<Action>> {
        match event {
            Event::NewBlock(block) => {
                // Check watched accounts on each block
                self.check_count
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);

                let accounts: Vec<Address> =
                    self.watched_accounts.iter().map(|r| *r.key()).collect();

                for user in accounts {
                    // Check Aave
                    if let Ok(Some(health)) = self.check_aave_health(user).await {
                        self.watched_accounts.insert(user, health.clone());

                        if health.is_liquidatable {
                            if let Some(action) = self.create_liquidation_action(&health) {
                                self.opportunity_count
                                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                                return Ok(Some(Action::Liquidation(action)));
                            }
                        } else if health.health_factor < self.config.health_factor_threshold {
                            debug!(
                                "At-risk account: {:?} HF={:.4}",
                                user, health.health_factor
                            );
                        }
                    }
                }

                let checks = self
                    .check_count
                    .load(std::sync::atomic::Ordering::Relaxed);
                let opps = self
                    .opportunity_count
                    .load(std::sync::atomic::Ordering::Relaxed);

                if checks % 10 == 0 {
                    info!(
                        "LiquidationStrategy: {} blocks checked, {} accounts watched, {} opportunities",
                        checks,
                        self.watched_accounts.len(),
                        opps
                    );
                }

                Ok(None)
            }
            Event::Liquidation(liq_event) => {
                // External liquidation event - could track or backrun
                debug!(
                    "External liquidation: {:?} on {} HF={:.4}",
                    liq_event.user, liq_event.protocol, liq_event.health_factor
                );
                Ok(None)
            }
            _ => Ok(None),
        }
    }

    async fn on_start(&self) -> eyre::Result<()> {
        info!(
            "LiquidationStrategy started: watching {} accounts, HF threshold: {:.2}",
            self.config.watched_accounts.len(),
            self.config.health_factor_threshold
        );
        Ok(())
    }
}
