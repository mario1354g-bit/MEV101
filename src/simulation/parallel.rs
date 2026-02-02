//! Parallel simulation support using REVM.
//!
//! This module provides utilities for running multiple simulations in parallel,
//! which is crucial for quickly evaluating many MEV opportunities.

use alloy::primitives::{Address, U256};
use parking_lot::Mutex;
use rayon::prelude::*;
use tracing::debug;

use super::revm_simulator::{RevmSimulator, SimulationResult, Transaction};

/// A pool of REVM simulators for parallel execution.
pub struct ParallelSimulator {
    /// Base state that simulators are cloned from
    base_simulator: RevmSimulator,
    /// Number of worker threads
    num_workers: usize,
}

impl ParallelSimulator {
    /// Create a new parallel simulator with the specified number of workers.
    pub fn new(num_workers: usize) -> Self {
        Self {
            base_simulator: RevmSimulator::new(),
            num_workers,
        }
    }

    /// Create a parallel simulator that auto-detects the number of CPU cores.
    pub fn new_auto() -> Self {
        let num_workers = num_cpus::get();
        Self::new(num_workers)
    }

    /// Create a parallel simulator from a base simulator state.
    pub fn with_base_simulator(base_simulator: RevmSimulator, num_workers: usize) -> Self {
        Self {
            base_simulator,
            num_workers,
        }
    }

    /// Set account state in the base simulator.
    pub fn set_account_state(&mut self, address: Address, balance: U256) {
        self.base_simulator.set_balance(address, balance);
    }

    /// Set storage state in the base simulator.
    pub fn set_storage_state(&mut self, address: Address, slot: U256, value: U256) {
        self.base_simulator.set_storage(address, slot, value);
    }

    /// Insert contract bytecode in the base simulator.
    pub fn insert_contract(&mut self, address: Address, bytecode: Vec<u8>) {
        self.base_simulator.insert_contract(address, bytecode);
    }

    /// Update block environment in the base simulator.
    pub fn set_block_env(&mut self, block_number: u64, timestamp: u64, base_fee: U256) {
        self.base_simulator
            .set_block_env(block_number, timestamp, base_fee);
    }

    /// Simulate multiple transactions in parallel.
    ///
    /// Each transaction is simulated independently on a fresh copy of the base state.
    pub fn simulate_many(&self, txs: Vec<Transaction>) -> Vec<SimulationResult> {
        debug!(
            num_txs = txs.len(),
            num_workers = self.num_workers,
            "Starting parallel simulation"
        );

        // Configure rayon thread pool
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(self.num_workers)
            .build()
            .unwrap_or_else(|_| rayon::ThreadPoolBuilder::new().build().expect("default rayon thread pool should always build"));

        let base_sim = &self.base_simulator;

        pool.install(|| {
            txs.par_iter()
                .map(|tx| {
                    // Clone the base simulator for this thread
                    let mut simulator = base_sim.clone();

                    // Simulate the transaction
                    match simulator.simulate_tx(tx) {
                        Ok(result) => result,
                        Err(e) => SimulationResult::failed(&e.to_string()),
                    }
                })
                .collect()
        })
    }

    /// Simulate multiple transaction bundles in parallel.
    ///
    /// Each bundle is simulated independently, maintaining state within the bundle.
    pub fn simulate_bundles(&self, bundles: Vec<Vec<Transaction>>) -> Vec<Vec<SimulationResult>> {
        debug!(
            num_bundles = bundles.len(),
            num_workers = self.num_workers,
            "Starting parallel bundle simulation"
        );

        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(self.num_workers)
            .build()
            .unwrap_or_else(|_| rayon::ThreadPoolBuilder::new().build().expect("default rayon thread pool should always build"));

        let base_sim = &self.base_simulator;

        pool.install(|| {
            bundles
                .par_iter()
                .map(|bundle| {
                    let mut simulator = base_sim.clone();

                    match simulator.simulate_bundle(bundle) {
                        Ok(results) => results,
                        Err(e) => {
                            vec![SimulationResult::failed(&e.to_string()); bundle.len()]
                        }
                    }
                })
                .collect()
        })
    }

    /// Evaluate multiple arbitrage opportunities in parallel.
    ///
    /// Returns results paired with their original opportunity index.
    pub fn evaluate_opportunities<T, F>(
        &self,
        opportunities: Vec<T>,
        build_txs: F,
    ) -> Vec<(usize, Vec<SimulationResult>)>
    where
        T: Send + Sync,
        F: Fn(&T) -> Vec<Transaction> + Send + Sync,
    {
        debug!(
            num_opportunities = opportunities.len(),
            "Evaluating opportunities in parallel"
        );

        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(self.num_workers)
            .build()
            .unwrap_or_else(|_| rayon::ThreadPoolBuilder::new().build().expect("default rayon thread pool should always build"));

        let base_sim = &self.base_simulator;

        pool.install(|| {
            opportunities
                .par_iter()
                .enumerate()
                .map(|(idx, opp)| {
                    let txs = build_txs(opp);
                    let mut simulator = base_sim.clone();

                    let results = match simulator.simulate_bundle(&txs) {
                        Ok(r) => r,
                        Err(e) => vec![SimulationResult::failed(&e.to_string()); txs.len()],
                    };

                    (idx, results)
                })
                .collect()
        })
    }

    /// Find the most profitable opportunity from a list.
    ///
    /// Uses the provided profit calculator function to determine profitability.
    pub fn find_most_profitable<T, F, P>(
        &self,
        opportunities: Vec<T>,
        build_txs: F,
        calculate_profit: P,
    ) -> Option<(usize, T, U256)>
    where
        T: Clone + Send + Sync,
        F: Fn(&T) -> Vec<Transaction> + Send + Sync,
        P: Fn(&T, &[SimulationResult]) -> U256 + Send + Sync,
    {
        let results = self.evaluate_opportunities(opportunities.clone(), &build_txs);

        let mut best: Option<(usize, T, U256)> = None;

        for (idx, sim_results) in results {
            // Check if all transactions succeeded
            if sim_results.iter().all(|r| r.success) {
                let profit = calculate_profit(&opportunities[idx], &sim_results);

                if let Some((_, _, best_profit)) = &best {
                    if profit > *best_profit {
                        best = Some((idx, opportunities[idx].clone(), profit));
                    }
                } else if profit > U256::ZERO {
                    best = Some((idx, opportunities[idx].clone(), profit));
                }
            }
        }

        best
    }

    /// Get the number of workers.
    pub fn num_workers(&self) -> usize {
        self.num_workers
    }

    /// Get a reference to the base simulator.
    pub fn base_simulator(&self) -> &RevmSimulator {
        &self.base_simulator
    }

    /// Get a mutable reference to the base simulator.
    pub fn base_simulator_mut(&mut self) -> &mut RevmSimulator {
        &mut self.base_simulator
    }
}

impl Default for ParallelSimulator {
    fn default() -> Self {
        Self::new_auto()
    }
}

impl Clone for ParallelSimulator {
    fn clone(&self) -> Self {
        Self {
            base_simulator: self.base_simulator.clone(),
            num_workers: self.num_workers,
        }
    }
}

/// Result aggregator for collecting parallel simulation results.
pub struct SimulationAggregator {
    /// Collected results
    results: Mutex<Vec<(usize, SimulationResult)>>,
    /// Total simulations expected
    total: usize,
}

impl SimulationAggregator {
    /// Create a new aggregator for the expected number of results.
    pub fn new(total: usize) -> Self {
        Self {
            results: Mutex::new(Vec::with_capacity(total)),
            total,
        }
    }

    /// Add a result to the aggregator.
    pub fn add_result(&self, index: usize, result: SimulationResult) {
        self.results.lock().push((index, result));
    }

    /// Check if all results have been collected.
    pub fn is_complete(&self) -> bool {
        self.results.lock().len() >= self.total
    }

    /// Get all results, sorted by index.
    pub fn get_results(self) -> Vec<SimulationResult> {
        let mut results = self.results.into_inner();
        results.sort_by_key(|(idx, _)| *idx);
        results.into_iter().map(|(_, r)| r).collect()
    }

    /// Get the number of successful simulations.
    pub fn success_count(&self) -> usize {
        self.results.lock().iter().filter(|(_, r)| r.success).count()
    }

    /// Get the number of failed simulations.
    pub fn failure_count(&self) -> usize {
        self.results.lock().iter().filter(|(_, r)| !r.success).count()
    }
}

/// Statistics about parallel simulation execution.
#[derive(Debug, Clone, Default)]
pub struct ParallelSimStats {
    /// Total number of simulations
    pub total: usize,
    /// Number of successful simulations
    pub successful: usize,
    /// Number of failed simulations
    pub failed: usize,
    /// Total gas used across all simulations
    pub total_gas_used: u64,
    /// Average gas per successful simulation
    pub avg_gas_per_success: u64,
}

impl ParallelSimStats {
    /// Calculate statistics from simulation results.
    pub fn from_results(results: &[SimulationResult]) -> Self {
        let total = results.len();
        let successful = results.iter().filter(|r| r.success).count();
        let failed = total - successful;
        let total_gas_used: u64 = results.iter().filter(|r| r.success).map(|r| r.gas_used).sum();
        let avg_gas_per_success = if successful > 0 {
            total_gas_used / successful as u64
        } else {
            0
        };

        Self {
            total,
            successful,
            failed,
            total_gas_used,
            avg_gas_per_success,
        }
    }

    /// Get the success rate as a percentage.
    pub fn success_rate(&self) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            (self.successful as f64 / self.total as f64) * 100.0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parallel_simulator_creation() {
        let sim = ParallelSimulator::new(4);
        assert_eq!(sim.num_workers(), 4);
    }

    #[test]
    fn test_parallel_simulator_auto() {
        let sim = ParallelSimulator::new_auto();
        assert!(sim.num_workers() > 0);
    }

    #[test]
    fn test_simulation_aggregator() {
        let aggregator = SimulationAggregator::new(3);

        aggregator.add_result(
            0,
            SimulationResult {
                success: true,
                gas_used: 100,
                ..Default::default()
            },
        );
        aggregator.add_result(
            2,
            SimulationResult {
                success: false,
                gas_used: 50,
                ..Default::default()
            },
        );
        aggregator.add_result(
            1,
            SimulationResult {
                success: true,
                gas_used: 150,
                ..Default::default()
            },
        );

        assert_eq!(aggregator.success_count(), 2);
        assert_eq!(aggregator.failure_count(), 1);

        let results = aggregator.get_results();
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].gas_used, 100);
        assert_eq!(results[1].gas_used, 150);
        assert_eq!(results[2].gas_used, 50);
    }

    #[test]
    fn test_parallel_sim_stats() {
        let results = vec![
            SimulationResult {
                success: true,
                gas_used: 100000,
                ..Default::default()
            },
            SimulationResult {
                success: true,
                gas_used: 150000,
                ..Default::default()
            },
            SimulationResult {
                success: false,
                gas_used: 50000,
                ..Default::default()
            },
        ];

        let stats = ParallelSimStats::from_results(&results);

        assert_eq!(stats.total, 3);
        assert_eq!(stats.successful, 2);
        assert_eq!(stats.failed, 1);
        assert_eq!(stats.total_gas_used, 250000);
        assert_eq!(stats.avg_gas_per_success, 125000);
        assert!((stats.success_rate() - 66.66666666666667).abs() < 0.01);
    }

    #[test]
    fn test_parallel_sim_stats_empty() {
        let results: Vec<SimulationResult> = vec![];
        let stats = ParallelSimStats::from_results(&results);

        assert_eq!(stats.total, 0);
        assert_eq!(stats.success_rate(), 0.0);
    }

    #[test]
    fn test_set_base_state() {
        let mut sim = ParallelSimulator::new(2);
        let address = Address::repeat_byte(0x42);

        sim.set_account_state(address, U256::from(1000u64));

        let balance = sim.base_simulator().get_balance(address).unwrap();
        assert_eq!(balance, U256::from(1000u64));
    }
}
