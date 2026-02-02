//! Executor module for MEV transaction execution.
//!
//! This module provides the core execution framework for MEV opportunities,
//! including the Executor trait, ExecutorManager, and context types.
//!
//! # Strategy Modules
//!
//! - [`arbitrage`] - Direct arbitrage execution (buy low, sell high)
//! - [`flashloan`] - Flashloan-assisted arbitrage for large opportunities
//! - [`sandwich`] - Sandwich attack execution (frontrun/backrun)
//! - [`backrun`] - Backrun execution after large trades
//! - [`liquidation`] - Lending protocol liquidations

pub mod arbitrage;
pub mod backrun;
pub mod flashbots;
pub mod flashloan;
pub mod liquidation;
pub mod router_encoder;
pub mod sandwich;
pub mod tx_builder;

// Re-export strategy executors
pub use arbitrage::ArbitrageExecutor;
pub use backrun::BackrunExecutor;
pub use flashloan::FlashloanExecutor;
pub use liquidation::LiquidationExecutor;
pub use sandwich::SandwichExecutor;

// Re-export core modules
pub use flashbots::*;
pub use router_encoder::*;
pub use tx_builder::*;

use alloy::primitives::{Address, Bytes, B256, U256};
use alloy::providers::Provider as AlloyProvider;
use alloy::network::EthereumWallet;
use alloy::signers::local::PrivateKeySigner;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{debug, error, info, instrument, warn};

use crate::error::MevError;

/// Result type alias for executor operations.
pub type Result<T> = std::result::Result<T, MevError>;

/// Opportunity type for execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Opportunity {
    /// Unique identifier for the opportunity
    pub id: String,
    /// Type of MEV opportunity
    pub opportunity_type: OpportunityType,
    /// Block number when detected
    pub block_number: u64,
    /// Timestamp when detected (milliseconds)
    pub timestamp: u64,
    /// Token addresses involved in the opportunity
    pub tokens: Vec<Address>,
    /// Pool addresses involved
    pub pools: Vec<Address>,
    /// Estimated profit in wei
    pub estimated_profit_wei: U256,
    /// Estimated gas cost in wei
    pub estimated_gas_wei: U256,
    /// Swap path for arbitrage opportunities
    pub swap_path: Option<SwapPath>,
    /// Target transaction for sandwich/backrun opportunities
    pub target_tx: Option<TargetTransaction>,
    /// Additional metadata
    pub metadata: serde_json::Value,
}

/// Types of MEV opportunities.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpportunityType {
    /// Cross-DEX arbitrage
    Arbitrage,
    /// Sandwich attack
    Sandwich,
    /// Backrun opportunity
    Backrun,
    /// Liquidation
    Liquidation,
    /// JIT liquidity
    JitLiquidity,
}

/// Swap path for arbitrage opportunities.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwapPath {
    /// Ordered list of swap steps
    pub steps: Vec<SwapStep>,
    /// Input amount in wei
    pub input_amount: U256,
    /// Expected output amount in wei
    pub expected_output: U256,
    /// Minimum output amount (with slippage)
    pub min_output: U256,
}

/// Single swap step in a path.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SwapStep {
    /// Pool address
    pub pool: Address,
    /// Token in
    pub token_in: Address,
    /// Token out
    pub token_out: Address,
    /// DEX protocol (e.g., "uniswap_v2", "uniswap_v3", "sushiswap")
    pub protocol: String,
    /// Fee tier (for V3 pools)
    pub fee: Option<u32>,
    /// Expected amount out
    pub amount_out: U256,
}

/// Target transaction for sandwich/backrun.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TargetTransaction {
    /// Transaction hash
    pub hash: B256,
    /// From address
    pub from: Address,
    /// To address
    pub to: Address,
    /// Transaction value
    pub value: U256,
    /// Transaction data
    pub data: Bytes,
    /// Gas price
    pub gas_price: U256,
}

/// Result of executing an opportunity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionResult {
    /// Whether execution was successful
    pub success: bool,
    /// Transaction hash if submitted
    pub tx_hash: Option<B256>,
    /// Bundle hash if submitted via Flashbots
    pub bundle_hash: Option<B256>,
    /// Block number where included
    pub block_number: Option<u64>,
    /// Actual profit achieved (wei)
    pub actual_profit: Option<U256>,
    /// Gas used
    pub gas_used: Option<u64>,
    /// Gas price paid
    pub gas_price: Option<U256>,
    /// Error message if failed
    pub error: Option<String>,
    /// Execution latency in milliseconds
    pub latency_ms: u64,
}

/// Result of simulating an opportunity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimulationResult {
    /// Whether simulation succeeded
    pub success: bool,
    /// Simulated profit (wei)
    pub profit: U256,
    /// Estimated gas usage
    pub gas_used: u64,
    /// State changes from simulation
    pub state_changes: Vec<StateChange>,
    /// Logs emitted during simulation
    pub logs: Vec<SimulatedLog>,
    /// Error message if failed
    pub error: Option<String>,
    /// Whether the opportunity is still profitable after gas
    pub is_profitable: bool,
}

/// State change from simulation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateChange {
    /// Contract address
    pub address: Address,
    /// Storage slot
    pub slot: B256,
    /// Previous value
    pub previous: B256,
    /// New value
    pub current: B256,
}

/// Simulated log entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimulatedLog {
    /// Emitting contract
    pub address: Address,
    /// Log topics
    pub topics: Vec<B256>,
    /// Log data
    pub data: Bytes,
}

/// Configuration for the executor.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Chain ID
    pub chain_id: u64,
    /// Minimum profit threshold (wei) to execute
    pub min_profit_wei: U256,
    /// Maximum gas price willing to pay (wei)
    pub max_gas_price_wei: U256,
    /// Slippage tolerance in basis points (e.g., 50 = 0.5%)
    pub slippage_bps: u32,
    /// Whether to use Flashbots for bundle submission
    pub use_flashbots: bool,
    /// Flashbots relay URL
    pub flashbots_relay_url: String,
    /// Number of blocks to target for bundle inclusion
    pub target_blocks: u8,
    /// Whether to revert bundle on any failure
    pub revert_on_fail: bool,
    /// Maximum number of retries for failed executions
    pub max_retries: u8,
    /// Timeout for execution in milliseconds
    pub execution_timeout_ms: u64,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            chain_id: 1, // Mainnet
            min_profit_wei: U256::from(10_000_000_000_000_000u64), // 0.01 ETH
            max_gas_price_wei: U256::from(100_000_000_000u64),     // 100 gwei
            slippage_bps: 50,                                       // 0.5%
            use_flashbots: true,
            flashbots_relay_url: "https://relay.flashbots.net".to_string(),
            target_blocks: 3,
            revert_on_fail: true,
            max_retries: 2,
            execution_timeout_ms: 12000, // 12 seconds (1 block)
        }
    }
}

/// Context provided to executors for transaction building and submission.
pub struct ExecutorContext<P>
where
    P: AlloyProvider + Clone + 'static,
{
    /// Ethereum provider for RPC calls
    pub provider: Arc<P>,
    /// Wallet for signing transactions
    pub wallet: Arc<EthereumWallet>,
    /// Signer for raw signing operations
    pub signer: Arc<PrivateKeySigner>,
    /// Flashbots client for bundle submission
    pub flashbots: Arc<FlashbotsClient>,
    /// Executor configuration
    pub config: Arc<Config>,
}

impl<P> ExecutorContext<P>
where
    P: AlloyProvider + Clone + 'static,
{
    /// Create a new executor context.
    pub fn new(
        provider: Arc<P>,
        wallet: Arc<EthereumWallet>,
        signer: Arc<PrivateKeySigner>,
        flashbots: Arc<FlashbotsClient>,
        config: Arc<Config>,
    ) -> Self {
        Self {
            provider,
            wallet,
            signer,
            flashbots,
            config,
        }
    }
}

impl<P> Clone for ExecutorContext<P>
where
    P: AlloyProvider + Clone + 'static,
{
    fn clone(&self) -> Self {
        Self {
            provider: Arc::clone(&self.provider),
            wallet: Arc::clone(&self.wallet),
            signer: Arc::clone(&self.signer),
            flashbots: Arc::clone(&self.flashbots),
            config: Arc::clone(&self.config),
        }
    }
}

/// Trait for MEV opportunity executors.
///
/// Implementors of this trait handle specific types of MEV opportunities
/// and know how to build, simulate, and execute the necessary transactions.
#[async_trait]
pub trait Executor<P>: Send + Sync
where
    P: AlloyProvider + Clone + 'static,
{
    /// Returns the name of this executor.
    fn name(&self) -> &str;

    /// Check if this executor can handle the given opportunity.
    fn can_handle(&self, opp: &Opportunity) -> bool;

    /// Execute the opportunity.
    ///
    /// This method should:
    /// 1. Build the necessary transactions
    /// 2. Submit them (either via mempool or Flashbots)
    /// 3. Return the execution result
    async fn execute(&self, opp: &Opportunity, ctx: &ExecutorContext<P>) -> Result<ExecutionResult>;

    /// Simulate the opportunity without executing.
    ///
    /// This method should simulate the transactions and return
    /// the expected outcome without actually submitting them.
    async fn simulate(&self, opp: &Opportunity, ctx: &ExecutorContext<P>) -> Result<SimulationResult>;
}

/// Manages multiple executors and routes opportunities to the appropriate one.
pub struct ExecutorManager<P>
where
    P: AlloyProvider + Clone + 'static,
{
    /// Registered executors
    executors: Vec<Box<dyn Executor<P>>>,
    /// Execution context
    context: ExecutorContext<P>,
    /// Statistics tracking
    stats: ExecutorStats,
}

/// Statistics for executor operations.
#[derive(Debug, Default)]
pub struct ExecutorStats {
    /// Total opportunities processed
    pub total_processed: u64,
    /// Successful executions
    pub successful_executions: u64,
    /// Failed executions
    pub failed_executions: u64,
    /// Total profit earned (wei)
    pub total_profit_wei: U256,
    /// Total gas spent (wei)
    pub total_gas_spent_wei: U256,
}

impl<P> ExecutorManager<P>
where
    P: AlloyProvider + Clone + Send + Sync + 'static,
{
    /// Create a new executor manager.
    pub fn new(context: ExecutorContext<P>) -> Self {
        Self {
            executors: Vec::new(),
            context,
            stats: ExecutorStats::default(),
        }
    }

    /// Register an executor.
    pub fn register_executor(&mut self, executor: Box<dyn Executor<P>>) {
        info!(executor = executor.name(), "Registering executor");
        self.executors.push(executor);
    }

    /// Get the execution context.
    pub fn context(&self) -> &ExecutorContext<P> {
        &self.context
    }

    /// Get execution statistics.
    pub fn stats(&self) -> &ExecutorStats {
        &self.stats
    }

    /// Find an executor that can handle the given opportunity.
    fn find_executor(&self, opp: &Opportunity) -> Option<&dyn Executor<P>> {
        for executor in &self.executors {
            if executor.can_handle(opp) {
                debug!(
                    executor = executor.name(),
                    opportunity_id = %opp.id,
                    "Found matching executor"
                );
                return Some(executor.as_ref());
            }
        }
        None
    }

    /// Process an opportunity through the appropriate executor.
    ///
    /// This method:
    /// 1. Finds an executor that can handle the opportunity
    /// 2. Simulates the opportunity to verify profitability
    /// 3. If profitable, executes the opportunity
    /// 4. Returns the execution result or None if no executor found
    #[instrument(skip(self), fields(opportunity_id = %opp.id))]
    pub async fn process_opportunity(&mut self, opp: Opportunity) -> Result<Option<ExecutionResult>> {
        self.stats.total_processed += 1;

        // Find appropriate executor
        let executor = match self.find_executor(&opp) {
            Some(e) => e,
            None => {
                warn!(
                    opportunity_type = ?opp.opportunity_type,
                    "No executor found for opportunity"
                );
                return Ok(None);
            }
        };

        info!(
            executor = executor.name(),
            opportunity_type = ?opp.opportunity_type,
            estimated_profit = %opp.estimated_profit_wei,
            "Processing opportunity"
        );

        // Simulate first to verify profitability
        let sim_result = match executor.simulate(&opp, &self.context).await {
            Ok(result) => result,
            Err(e) => {
                error!(error = %e, "Simulation failed");
                self.stats.failed_executions += 1;
                return Err(e);
            }
        };

        if !sim_result.success {
            warn!(
                error = ?sim_result.error,
                "Simulation indicates failure"
            );
            self.stats.failed_executions += 1;
            return Ok(Some(ExecutionResult {
                success: false,
                tx_hash: None,
                bundle_hash: None,
                block_number: None,
                actual_profit: None,
                gas_used: None,
                gas_price: None,
                error: sim_result.error,
                latency_ms: 0,
            }));
        }

        if !sim_result.is_profitable {
            info!(
                simulated_profit = %sim_result.profit,
                gas_used = sim_result.gas_used,
                "Opportunity no longer profitable after simulation"
            );
            return Ok(Some(ExecutionResult {
                success: false,
                tx_hash: None,
                bundle_hash: None,
                block_number: None,
                actual_profit: Some(sim_result.profit),
                gas_used: Some(sim_result.gas_used),
                gas_price: None,
                error: Some("Not profitable after gas costs".to_string()),
                latency_ms: 0,
            }));
        }

        // Execute the opportunity
        let start = std::time::Instant::now();
        let exec_result = match executor.execute(&opp, &self.context).await {
            Ok(result) => result,
            Err(e) => {
                error!(error = %e, "Execution failed");
                self.stats.failed_executions += 1;
                return Err(e);
            }
        };

        let latency_ms = start.elapsed().as_millis() as u64;

        // Update statistics
        if exec_result.success {
            self.stats.successful_executions += 1;
            if let Some(profit) = exec_result.actual_profit {
                self.stats.total_profit_wei += profit;
            }
            if let (Some(gas_used), Some(gas_price)) = (exec_result.gas_used, exec_result.gas_price) {
                self.stats.total_gas_spent_wei += U256::from(gas_used) * gas_price;
            }
            info!(
                tx_hash = ?exec_result.tx_hash,
                profit = ?exec_result.actual_profit,
                latency_ms = latency_ms,
                "Execution successful"
            );
        } else {
            self.stats.failed_executions += 1;
            warn!(
                error = ?exec_result.error,
                latency_ms = latency_ms,
                "Execution failed"
            );
        }

        Ok(Some(ExecutionResult {
            latency_ms,
            ..exec_result
        }))
    }

    /// Simulate an opportunity without executing.
    pub async fn simulate_opportunity(&self, opp: &Opportunity) -> Result<Option<SimulationResult>> {
        let executor = match self.find_executor(opp) {
            Some(e) => e,
            None => return Ok(None),
        };

        let result = executor.simulate(opp, &self.context).await?;
        Ok(Some(result))
    }

    /// Process multiple opportunities in parallel.
    pub async fn process_opportunities_batch(
        &mut self,
        opportunities: Vec<Opportunity>,
    ) -> Vec<Result<Option<ExecutionResult>>> {
        let mut results = Vec::with_capacity(opportunities.len());

        // Process sequentially to avoid nonce conflicts
        // In a production system, you'd want proper nonce management
        for opp in opportunities {
            let result = self.process_opportunity(opp).await;
            results.push(result);
        }

        results
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_opportunity_type_serialization() {
        let opp_type = OpportunityType::Arbitrage;
        let serialized = serde_json::to_string(&opp_type).unwrap();
        assert_eq!(serialized, "\"arbitrage\"");

        let deserialized: OpportunityType = serde_json::from_str(&serialized).unwrap();
        assert_eq!(deserialized, OpportunityType::Arbitrage);
    }

    #[test]
    fn test_config_default() {
        let config = Config::default();
        assert_eq!(config.chain_id, 1);
        assert!(config.use_flashbots);
        assert_eq!(config.slippage_bps, 50);
    }

    #[test]
    fn test_executor_stats_default() {
        let stats = ExecutorStats::default();
        assert_eq!(stats.total_processed, 0);
        assert_eq!(stats.successful_executions, 0);
        assert_eq!(stats.total_profit_wei, U256::ZERO);
    }
}
