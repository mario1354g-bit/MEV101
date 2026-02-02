//! Simulation orchestration module for MEV bot.
//!
//! This module provides high-level simulation capabilities for MEV opportunities,
//! coordinating between eth_call simulation, gas estimation, REVM local simulation,
//! and bundle simulation.
//!
//! ## Module Overview
//!
//! - `eth_call`: RPC-based simulation using eth_call
//! - `gas_estimator`: Gas cost estimation and optimization
//! - `revm_simulator`: Local EVM simulation using REVM
//! - `fork_db`: Forking database for lazy state loading from RPC
//! - `parallel`: Parallel simulation support for evaluating multiple opportunities

pub mod eth_call;
pub mod fork_db;
pub mod gas_estimator;
pub mod parallel;
pub mod revm_simulator;
pub mod swap_simulator;

pub use eth_call::EthCallSimulator;
pub use swap_simulator::SwapSimulator;
pub use fork_db::{CacheStats, ForkDB, SharedForkDB};
pub use gas_estimator::GasEstimator;
pub use parallel::{ParallelSimStats, ParallelSimulator, SimulationAggregator};
pub use revm_simulator::{
    RevmSimulator, SandwichSimResult, SimulationResult as RevmSimulationResult,
    StateChange as RevmStateChange, Transaction as RevmTransaction,
};

use crate::dex::SwapParams;
use crate::error::{MevError, SimulationError};
use crate::storage::models::OpportunityType;

use alloy::consensus::{Transaction, TypedTransaction};
use alloy::eips::BlockNumberOrTag;
use alloy::primitives::{Address, Bytes, I256, U256};
use alloy::providers::Provider;
use alloy::transports::Transport;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{debug, info, warn};

/// Result of simulating an MEV opportunity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimulationResult {
    /// Whether the simulation succeeded without reverting
    pub success: bool,
    /// Output amount from the trade(s)
    pub output_amount: U256,
    /// Gross profit in wei (can be negative)
    pub profit_wei: I256,
    /// Gas units used
    pub gas_used: u64,
    /// Gas cost in wei
    pub gas_cost_wei: U256,
    /// Net profit in wei (profit - gas cost, can be negative)
    pub net_profit_wei: I256,
    /// Revert reason if the simulation failed
    pub revert_reason: Option<String>,
    /// Effective gas price used in simulation
    pub effective_gas_price: U256,
    /// Block number used for simulation
    pub block_number: u64,
    /// Simulated state changes (for debugging)
    pub state_changes: Option<Vec<StateChange>>,
}

impl Default for SimulationResult {
    fn default() -> Self {
        Self {
            success: false,
            output_amount: U256::ZERO,
            profit_wei: I256::ZERO,
            gas_used: 0,
            gas_cost_wei: U256::ZERO,
            net_profit_wei: I256::ZERO,
            revert_reason: None,
            effective_gas_price: U256::ZERO,
            block_number: 0,
            state_changes: None,
        }
    }
}

impl SimulationResult {
    /// Create a failed simulation result with a revert reason.
    pub fn failed(reason: String) -> Self {
        Self {
            success: false,
            revert_reason: Some(reason),
            ..Default::default()
        }
    }

    /// Create a successful simulation result.
    pub fn successful(
        output_amount: U256,
        profit_wei: I256,
        gas_used: u64,
        gas_cost_wei: U256,
    ) -> Self {
        let net_profit_wei = profit_wei - I256::try_from(gas_cost_wei).unwrap_or(I256::MAX);
        Self {
            success: true,
            output_amount,
            profit_wei,
            gas_used,
            gas_cost_wei,
            net_profit_wei,
            revert_reason: None,
            ..Default::default()
        }
    }

    /// Check if the simulation is profitable after gas costs.
    pub fn is_profitable(&self) -> bool {
        self.success && self.net_profit_wei > I256::ZERO
    }

    /// Get the net profit as U256 if profitable, otherwise return 0.
    pub fn net_profit_or_zero(&self) -> U256 {
        if self.is_profitable() {
            U256::try_from(self.net_profit_wei).unwrap_or(U256::ZERO)
        } else {
            U256::ZERO
        }
    }
}

/// State change recorded during simulation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateChange {
    /// Contract address that was modified
    pub address: Address,
    /// Storage slot that was modified
    pub slot: U256,
    /// Previous value
    pub old_value: U256,
    /// New value
    pub new_value: U256,
}

/// Result of simulating a transaction bundle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BundleSimulation {
    /// Whether all transactions in the bundle succeeded
    pub success: bool,
    /// Total gas used by all transactions
    pub total_gas_used: u64,
    /// Total gas cost in wei
    pub total_gas_cost_wei: U256,
    /// Results for each transaction in the bundle
    pub tx_results: Vec<TxSimulationResult>,
    /// Coinbase payment (MEV payment to block builder)
    pub coinbase_payment: U256,
    /// Net profit after gas and coinbase payment
    pub net_profit_wei: I256,
    /// Bundle hash (for tracking)
    pub bundle_hash: Option<String>,
}

impl Default for BundleSimulation {
    fn default() -> Self {
        Self {
            success: false,
            total_gas_used: 0,
            total_gas_cost_wei: U256::ZERO,
            tx_results: Vec::new(),
            coinbase_payment: U256::ZERO,
            net_profit_wei: I256::ZERO,
            bundle_hash: None,
        }
    }
}

/// Result of simulating a single transaction within a bundle.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TxSimulationResult {
    /// Transaction index in the bundle
    pub index: usize,
    /// Whether the transaction succeeded
    pub success: bool,
    /// Gas used by this transaction
    pub gas_used: u64,
    /// Return data from the transaction
    pub return_data: Bytes,
    /// Revert reason if failed
    pub revert_reason: Option<String>,
    /// Logs emitted by the transaction
    pub logs: Vec<SimulatedLog>,
}

/// A log emitted during simulation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimulatedLog {
    /// Emitting contract address
    pub address: Address,
    /// Log topics
    pub topics: Vec<U256>,
    /// Log data
    pub data: Bytes,
}

/// MEV opportunity representation for simulation.
#[derive(Debug, Clone)]
pub struct Opportunity {
    /// Type of MEV opportunity
    pub opportunity_type: OpportunityType,
    /// Input amount for the opportunity
    pub input_amount: U256,
    /// Expected output amount
    pub expected_output: U256,
    /// Token being sold
    pub token_in: Address,
    /// Token being bought
    pub token_out: Address,
    /// Swap parameters for the opportunity
    pub swaps: Vec<SwapParams>,
    /// Target block for execution
    pub target_block: Option<u64>,
    /// Maximum gas price willing to pay
    pub max_gas_price: Option<U256>,
    /// Deadline timestamp
    pub deadline: U256,
}

/// Main simulator orchestrator.
///
/// This struct coordinates between different simulation methods:
/// - `eth_call`: RPC-based simulation (works against live state but stateless)
/// - `revm`: Local EVM simulation (fast, stateful, supports state persistence)
/// - `parallel`: Parallel simulation for evaluating multiple opportunities
pub struct Simulator<T, P>
where
    T: Transport + Clone,
    P: Provider<T>,
{
    /// Ethereum provider
    #[allow(dead_code)] // Reserved for direct provider calls in future simulations
    provider: Arc<P>,
    /// Gas estimator for cost calculations
    gas_estimator: GasEstimator<T, P>,
    /// eth_call simulator for local simulation
    eth_call_simulator: EthCallSimulator<T, P>,
    /// Optional REVM simulator for fast local simulation
    revm_simulator: Option<RevmSimulator>,
    /// Optional parallel simulator for batch evaluation
    parallel_simulator: Option<ParallelSimulator>,
    /// Executor address (our bot's address)
    executor_address: Address,
    /// Minimum profit threshold in wei
    min_profit_wei: U256,
    /// Whether to prefer REVM simulation when available
    prefer_revm: bool,
    /// Phantom data for transport type
    _transport: std::marker::PhantomData<T>,
}

impl<T, P> Simulator<T, P>
where
    T: Transport + Clone,
    P: Provider<T> + Clone + 'static,
{
    /// Create a new simulator instance.
    pub fn new(
        provider: Arc<P>,
        executor_address: Address,
        min_profit_wei: U256,
    ) -> Self {
        let gas_estimator = GasEstimator::new(provider.clone());
        let eth_call_simulator = EthCallSimulator::new(provider.clone());

        Self {
            provider,
            gas_estimator,
            eth_call_simulator,
            revm_simulator: None,
            parallel_simulator: None,
            executor_address,
            min_profit_wei,
            prefer_revm: false,
            _transport: std::marker::PhantomData,
        }
    }

    /// Create a new simulator with REVM support enabled.
    pub fn with_revm(mut self) -> Self {
        self.revm_simulator = Some(RevmSimulator::new());
        self.prefer_revm = true;
        self
    }

    /// Create a new simulator with parallel simulation support.
    pub fn with_parallel(mut self, num_workers: Option<usize>) -> Self {
        let workers = num_workers.unwrap_or_else(num_cpus::get);
        self.parallel_simulator = Some(ParallelSimulator::new(workers));
        self
    }

    /// Fork REVM state from provider at a specific block.
    ///
    /// This sets up the REVM simulator with block environment matching the chain state.
    pub async fn fork_revm_from_provider(&mut self, block: Option<u64>) -> Result<(), MevError> {
        let simulator = RevmSimulator::fork_from_provider(&*self.provider, block)
            .await
            .map_err(|e| SimulationError::ContractCallFailed(e.to_string()))?;

        self.revm_simulator = Some(simulator);
        self.prefer_revm = true;

        info!("REVM simulator forked from provider");
        Ok(())
    }

    /// Set the REVM simulator block environment.
    pub fn set_revm_block_env(&mut self, block_number: u64, timestamp: u64, base_fee: U256) {
        if let Some(ref mut revm) = self.revm_simulator {
            revm.set_block_env(block_number, timestamp, base_fee);
        }
    }

    /// Set an account balance in the REVM simulator (for testing).
    pub fn set_revm_balance(&mut self, address: Address, balance: U256) {
        if let Some(ref mut revm) = self.revm_simulator {
            revm.set_balance(address, balance);
        }
    }

    /// Get the REVM simulator reference.
    pub fn revm_simulator(&self) -> Option<&RevmSimulator> {
        self.revm_simulator.as_ref()
    }

    /// Get a mutable reference to the REVM simulator.
    pub fn revm_simulator_mut(&mut self) -> Option<&mut RevmSimulator> {
        self.revm_simulator.as_mut()
    }

    /// Get the parallel simulator reference.
    pub fn parallel_simulator(&self) -> Option<&ParallelSimulator> {
        self.parallel_simulator.as_ref()
    }

    /// Simulate a transaction using REVM.
    ///
    /// Returns None if REVM is not enabled.
    pub fn simulate_tx_revm(
        &mut self,
        tx: &RevmTransaction,
    ) -> Option<Result<RevmSimulationResult, MevError>> {
        let revm = self.revm_simulator.as_mut()?;

        Some(
            revm.simulate_tx(tx)
                .map_err(|e| SimulationError::ContractCallFailed(e.to_string()).into()),
        )
    }

    /// Simulate a bundle using REVM with state persistence.
    ///
    /// Returns None if REVM is not enabled.
    pub fn simulate_bundle_revm(
        &mut self,
        txs: &[RevmTransaction],
    ) -> Option<Result<Vec<RevmSimulationResult>, MevError>> {
        let revm = self.revm_simulator.as_mut()?;

        Some(
            revm.simulate_bundle(txs)
                .map_err(|e| SimulationError::ContractCallFailed(e.to_string()).into()),
        )
    }

    /// Simulate a sandwich attack using REVM.
    ///
    /// Returns None if REVM is not enabled.
    pub fn simulate_sandwich_revm(
        &mut self,
        frontrun: &RevmTransaction,
        victim: &RevmTransaction,
        backrun: &RevmTransaction,
    ) -> Option<Result<SandwichSimResult, MevError>> {
        let revm = self.revm_simulator.as_mut()?;

        Some(
            revm.simulate_sandwich(frontrun, victim, backrun)
                .map_err(|e| SimulationError::ContractCallFailed(e.to_string()).into()),
        )
    }

    /// Simulate multiple transactions in parallel.
    ///
    /// Returns None if parallel simulator is not enabled.
    pub fn simulate_many_parallel(
        &self,
        txs: Vec<RevmTransaction>,
    ) -> Option<Vec<RevmSimulationResult>> {
        let parallel = self.parallel_simulator.as_ref()?;
        Some(parallel.simulate_many(txs))
    }

    /// Create a REVM checkpoint for rollback.
    pub fn checkpoint_revm(&mut self) -> Option<usize> {
        self.revm_simulator.as_mut().map(|r| r.checkpoint())
    }

    /// Rollback REVM to a checkpoint.
    pub fn rollback_revm_to(&mut self, checkpoint: usize) {
        if let Some(ref mut revm) = self.revm_simulator {
            revm.rollback_to(checkpoint);
        }
    }

    /// Whether REVM simulation is preferred.
    pub fn prefers_revm(&self) -> bool {
        self.prefer_revm && self.revm_simulator.is_some()
    }

    /// Set whether to prefer REVM simulation.
    pub fn set_prefer_revm(&mut self, prefer: bool) {
        self.prefer_revm = prefer;
    }

    /// Simulate an MEV opportunity.
    pub async fn simulate_opportunity(
        &self,
        opp: &Opportunity,
    ) -> Result<SimulationResult, MevError> {
        info!(
            opportunity_type = %opp.opportunity_type,
            input_amount = %opp.input_amount,
            "Simulating opportunity"
        );

        // Get the target block for simulation
        let block = match opp.target_block {
            Some(num) => BlockNumberOrTag::Number(num),
            None => BlockNumberOrTag::Latest,
        };

        // Estimate gas cost for this opportunity type
        let gas_cost = self.gas_estimator.estimate_gas_cost(opp).await?;

        // Simulate based on opportunity type
        let mut result = match opp.opportunity_type {
            OpportunityType::PriceDiscrepancy => {
                self.simulate_arbitrage(opp, block).await?
            }
            OpportunityType::MultiHop => {
                self.simulate_multi_hop(opp, block).await?
            }
            OpportunityType::Sandwich => {
                self.simulate_sandwich(opp, block).await?
            }
            OpportunityType::Backrun => {
                self.simulate_backrun(opp, block).await?
            }
            OpportunityType::Liquidation => {
                self.simulate_liquidation(opp, block).await?
            }
            OpportunityType::LiquidityEvent => {
                self.simulate_liquidity_event(opp, block).await?
            }
        };

        // Update gas cost in result
        result.gas_cost_wei = gas_cost;
        result.net_profit_wei = result.profit_wei
            - I256::try_from(gas_cost).unwrap_or(I256::MAX);

        // Log result
        if result.is_profitable() {
            info!(
                net_profit_wei = %result.net_profit_wei,
                gas_used = result.gas_used,
                "Opportunity is profitable"
            );
        } else {
            debug!(
                net_profit_wei = %result.net_profit_wei,
                success = result.success,
                revert_reason = ?result.revert_reason,
                "Opportunity is not profitable"
            );
        }

        Ok(result)
    }

    /// Simulate a price discrepancy arbitrage.
    async fn simulate_arbitrage(
        &self,
        opp: &Opportunity,
        block: BlockNumberOrTag,
    ) -> Result<SimulationResult, MevError> {
        if opp.swaps.len() < 2 {
            return Ok(SimulationResult::failed(
                "Arbitrage requires at least 2 swaps".to_string(),
            ));
        }

        let swap1 = &opp.swaps[0];
        let swap2 = &opp.swaps[1];

        self.eth_call_simulator
            .simulate_arbitrage(swap1.clone(), swap2.clone(), block)
            .await
            .map_err(|e| SimulationError::ContractCallFailed(e.to_string()).into())
    }

    /// Simulate a multi-hop arbitrage.
    async fn simulate_multi_hop(
        &self,
        opp: &Opportunity,
        block: BlockNumberOrTag,
    ) -> Result<SimulationResult, MevError> {
        if opp.swaps.is_empty() {
            return Ok(SimulationResult::failed(
                "Multi-hop requires at least 1 swap".to_string(),
            ));
        }

        self.eth_call_simulator
            .simulate_multi_hop(opp.swaps.clone(), block)
            .await
            .map_err(|e| SimulationError::ContractCallFailed(e.to_string()).into())
    }

    /// Simulate a sandwich attack.
    async fn simulate_sandwich(
        &self,
        opp: &Opportunity,
        block: BlockNumberOrTag,
    ) -> Result<SimulationResult, MevError> {
        if opp.swaps.len() < 2 {
            return Ok(SimulationResult::failed(
                "Sandwich requires frontrun and backrun swaps".to_string(),
            ));
        }

        let frontrun = &opp.swaps[0];
        let backrun = &opp.swaps[1];

        // Estimate victim impact (simplified)
        let victim_impact = opp.expected_output / U256::from(100); // 1% impact estimate

        self.eth_call_simulator
            .simulate_sandwich(frontrun.clone(), victim_impact, backrun.clone(), block)
            .await
            .map_err(|e| SimulationError::ContractCallFailed(e.to_string()).into())
    }

    /// Simulate a backrun opportunity.
    async fn simulate_backrun(
        &self,
        opp: &Opportunity,
        block: BlockNumberOrTag,
    ) -> Result<SimulationResult, MevError> {
        if opp.swaps.is_empty() {
            return Ok(SimulationResult::failed(
                "Backrun requires at least 1 swap".to_string(),
            ));
        }

        let swap = &opp.swaps[0];

        let swap_data = self
            .eth_call_simulator
            .encode_swap_data(swap)
            .map_err(|e| MevError::Simulation(SimulationError::ContractCallFailed(e.to_string())))?;

        self.eth_call_simulator
            .simulate_swap(
                self.executor_address,
                swap.recipient, // Router address
                swap_data,
                if swap.token_in == Address::ZERO {
                    swap.amount_in
                } else {
                    U256::ZERO
                },
                block,
            )
            .await
            .map_err(|e| SimulationError::ContractCallFailed(e.to_string()).into())
    }

    /// Simulate a liquidation opportunity.
    ///
    /// TODO(liquidation): Implement protocol-specific liquidation simulation
    /// This stub currently falls back to backrun simulation. Full implementation
    /// requires:
    /// - Aave V3: Call `liquidationCall` with health factor checks
    /// - Compound V3: Call `absorb` with position validation
    /// - Euler: Protocol-specific liquidation flow
    /// - Flash loan integration for capital efficiency
    ///
    /// Tracking: This needs proper protocol ABI integration and health factor
    /// calculation before production use.
    async fn simulate_liquidation(
        &self,
        opp: &Opportunity,
        block: BlockNumberOrTag,
    ) -> Result<SimulationResult, MevError> {
        // Liquidation simulation is protocol-specific
        // This is a placeholder for actual liquidation logic
        warn!("Liquidation simulation not fully implemented - falling back to backrun simulation");

        if opp.swaps.is_empty() {
            return Ok(SimulationResult::failed(
                "Liquidation requires swap parameters".to_string(),
            ));
        }

        // Simulate the liquidation as a swap for now
        self.simulate_backrun(opp, block).await
    }

    /// Simulate a liquidity event opportunity.
    async fn simulate_liquidity_event(
        &self,
        opp: &Opportunity,
        block: BlockNumberOrTag,
    ) -> Result<SimulationResult, MevError> {
        // Similar to backrun - capture the price impact
        self.simulate_backrun(opp, block).await
    }

    /// Simulate a bundle of transactions.
    pub async fn simulate_bundle(
        &self,
        txs: Vec<TypedTransaction>,
        block: BlockNumberOrTag,
    ) -> Result<BundleSimulation, MevError> {
        info!(num_txs = txs.len(), "Simulating transaction bundle");

        let mut bundle_result = BundleSimulation::default();
        let mut cumulative_gas = 0u64;

        for (index, tx) in txs.iter().enumerate() {
            // Extract transaction parameters
            let (from, to, data, value) = self.extract_tx_params(tx)?;

            // Simulate this transaction
            let sim_result = self
                .eth_call_simulator
                .simulate_swap(from, to, data.clone(), value, block)
                .await
                .map_err(|e| SimulationError::ContractCallFailed(e.to_string()))?;

            let tx_result = TxSimulationResult {
                index,
                success: sim_result.success,
                gas_used: sim_result.gas_used,
                return_data: Bytes::default(), // Would need trace data for this
                revert_reason: sim_result.revert_reason.clone(),
                logs: Vec::new(), // Would need trace data for this
            };

            cumulative_gas += sim_result.gas_used;
            bundle_result.tx_results.push(tx_result);

            // If any transaction fails, the bundle fails
            if !sim_result.success {
                bundle_result.success = false;
                warn!(
                    index = index,
                    revert_reason = ?sim_result.revert_reason,
                    "Transaction in bundle reverted"
                );
                return Ok(bundle_result);
            }
        }

        // Calculate total gas cost
        let gas_price = self.gas_estimator.get_effective_gas_price().await?;
        bundle_result.total_gas_used = cumulative_gas;
        bundle_result.total_gas_cost_wei = U256::from(cumulative_gas) * gas_price;
        bundle_result.success = true;

        info!(
            total_gas_used = cumulative_gas,
            total_gas_cost_wei = %bundle_result.total_gas_cost_wei,
            "Bundle simulation completed successfully"
        );

        Ok(bundle_result)
    }

    /// Extract transaction parameters from TypedTransaction.
    fn extract_tx_params(
        &self,
        tx: &TypedTransaction,
    ) -> Result<(Address, Address, Bytes, U256), MevError> {
        let from = self.executor_address;

        let (to, data, value) = match tx {
            TypedTransaction::Legacy(inner) => {
                let to = inner.to.to().copied().unwrap_or(Address::ZERO);
                let data = inner.input.clone();
                let value = inner.value;
                (to, data, value)
            }
            TypedTransaction::Eip2930(inner) => {
                let to = inner.to.to().copied().unwrap_or(Address::ZERO);
                let data = inner.input.clone();
                let value = inner.value;
                (to, data, value)
            }
            TypedTransaction::Eip1559(inner) => {
                let to = inner.to.to().copied().unwrap_or(Address::ZERO);
                let data = inner.input.clone();
                let value = inner.value;
                (to, data, value)
            }
            TypedTransaction::Eip4844(inner) => {
                // Eip4844 uses the Transaction trait
                let to = inner.to().unwrap_or(Address::ZERO);
                let data = inner.input().clone();
                let value = inner.value();
                (to, data, value)
            }
            TypedTransaction::Eip7702(inner) => {
                // Eip7702 has direct Address field for `to`
                let to = inner.to;
                let data = inner.input.clone();
                let value = inner.value;
                (to, data, value)
            }
        };

        Ok((from, to, data, value))
    }

    /// Check if an opportunity meets the minimum profit threshold.
    pub fn meets_min_profit(&self, result: &SimulationResult) -> bool {
        if !result.is_profitable() {
            return false;
        }

        match U256::try_from(result.net_profit_wei) {
            Ok(profit) => profit >= self.min_profit_wei,
            Err(_) => false,
        }
    }

    /// Get the gas estimator reference.
    pub fn gas_estimator(&self) -> &GasEstimator<T, P> {
        &self.gas_estimator
    }

    /// Get the eth_call simulator reference.
    pub fn eth_call_simulator(&self) -> &EthCallSimulator<T, P> {
        &self.eth_call_simulator
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simulation_result_default() {
        let result = SimulationResult::default();
        assert!(!result.success);
        assert_eq!(result.output_amount, U256::ZERO);
        assert_eq!(result.profit_wei, I256::ZERO);
    }

    #[test]
    fn test_simulation_result_failed() {
        let result = SimulationResult::failed("Test error".to_string());
        assert!(!result.success);
        assert_eq!(result.revert_reason, Some("Test error".to_string()));
        assert!(!result.is_profitable());
    }

    #[test]
    fn test_simulation_result_successful() {
        let result = SimulationResult::successful(
            U256::from(1000),
            I256::try_from(500).unwrap(),
            21000,
            U256::from(100),
        );
        assert!(result.success);
        assert_eq!(result.output_amount, U256::from(1000));
        assert_eq!(result.profit_wei, I256::try_from(500).unwrap());
        assert!(result.is_profitable());
    }

    #[test]
    fn test_simulation_result_not_profitable() {
        let result = SimulationResult::successful(
            U256::from(1000),
            I256::try_from(100).unwrap(),
            21000,
            U256::from(200), // Gas cost higher than profit
        );
        assert!(result.success);
        assert!(!result.is_profitable()); // Net is negative
    }

    #[test]
    fn test_net_profit_or_zero() {
        let profitable = SimulationResult::successful(
            U256::from(1000),
            I256::try_from(500).unwrap(),
            21000,
            U256::from(100),
        );
        assert!(profitable.net_profit_or_zero() > U256::ZERO);

        let unprofitable = SimulationResult::failed("Error".to_string());
        assert_eq!(unprofitable.net_profit_or_zero(), U256::ZERO);
    }
}
