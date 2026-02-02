//! Artemis Engine - orchestrates collectors, strategies, and executors
//!
//! The engine is the core component that:
//! - Starts all collectors
//! - Routes events to strategies
//! - Routes actions to executors
//! - Manages lifecycle

use super::collector::{Collector, CollectorHandle};
use super::executor::{ExecutionMode, Executor};
use super::strategy::Strategy;
use super::types::{Action, Event, ExecutionResult};
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{error, info, warn};

/// Engine configuration
#[derive(Debug, Clone)]
pub struct EngineConfig {
    /// Event channel buffer size
    pub event_buffer: usize,
    /// Action channel buffer size
    pub action_buffer: usize,
    /// Execution mode
    pub execution_mode: ExecutionMode,
    /// Minimum profit threshold in wei
    pub min_profit_wei: u128,
}

impl Default for EngineConfig {
    fn default() -> Self {
        Self {
            event_buffer: 1000,
            action_buffer: 100,
            execution_mode: ExecutionMode::DryRun,
            min_profit_wei: 5_000_000_000_000_000, // 0.005 ETH
        }
    }
}

/// Artemis Engine
pub struct Engine {
    config: EngineConfig,
    collectors: Vec<Arc<dyn Collector>>,
    strategies: Vec<Arc<dyn Strategy>>,
    executors: Vec<Arc<dyn Executor>>,
}

impl Engine {
    pub fn new(config: EngineConfig) -> Self {
        Self {
            config,
            collectors: Vec::new(),
            strategies: Vec::new(),
            executors: Vec::new(),
        }
    }

    /// Add a collector to the engine
    pub fn add_collector(mut self, collector: impl Collector + 'static) -> Self {
        self.collectors.push(Arc::new(collector));
        self
    }

    /// Add a strategy to the engine
    pub fn add_strategy(mut self, strategy: impl Strategy + 'static) -> Self {
        self.strategies.push(Arc::new(strategy));
        self
    }

    /// Add an executor to the engine
    pub fn add_executor(mut self, executor: impl Executor + 'static) -> Self {
        self.executors.push(Arc::new(executor));
        self
    }

    /// Run the engine
    pub async fn run(self) -> eyre::Result<()> {
        info!("Starting Artemis Engine");
        info!(
            "Collectors: {}, Strategies: {}, Executors: {}",
            self.collectors.len(),
            self.strategies.len(),
            self.executors.len()
        );

        // Create channels
        let (event_tx, mut event_rx) = mpsc::channel::<Event>(self.config.event_buffer);
        let (action_tx, mut action_rx) = mpsc::channel::<Action>(self.config.action_buffer);

        // Start collectors
        let mut collector_handles = Vec::new();
        for collector in &self.collectors {
            let name = collector.name().to_string();
            let name_for_handle = name.clone();
            let collector = Arc::clone(collector);
            let tx = event_tx.clone();

            let handle = tokio::spawn(async move {
                info!("Starting collector: {}", name);
                if let Err(e) = collector.collect(tx).await {
                    error!("Collector {} failed: {}", name, e);
                    return Err(e);
                }
                Ok(())
            });

            collector_handles.push(CollectorHandle::new(name_for_handle, handle));
        }

        // Initialize strategies
        for strategy in &self.strategies {
            info!("Initializing strategy: {}", strategy.name());
            if let Err(e) = strategy.on_start().await {
                error!("Strategy {} failed to start: {}", strategy.name(), e);
            }
        }

        // Clone for tasks
        let strategies = self.strategies.clone();
        let executors = self.executors.clone();
        let config = self.config.clone();

        // Spawn event processor
        let action_tx_clone = action_tx.clone();
        let event_processor = tokio::spawn(async move {
            let mut event_count = 0u64;
            let mut action_count = 0u64;

            while let Some(event) = event_rx.recv().await {
                event_count += 1;

                // Process event through all strategies
                for strategy in &strategies {
                    match strategy.process_event(&event).await {
                        Ok(Some(action)) => {
                            action_count += 1;
                            if let Err(e) = action_tx_clone.send(action).await {
                                error!("Failed to send action: {}", e);
                            }
                        }
                        Ok(None) => {}
                        Err(e) => {
                            warn!("Strategy {} error: {}", strategy.name(), e);
                        }
                    }
                }

                // Log stats periodically
                if event_count % 1000 == 0 {
                    info!(
                        "Engine stats: {} events processed, {} actions generated",
                        event_count, action_count
                    );
                }
            }
        });

        // Spawn action executor
        let action_executor = tokio::spawn(async move {
            let mut executed = 0u64;
            let mut successful = 0u64;

            while let Some(action) = action_rx.recv().await {
                executed += 1;

                // Find executor that supports this action
                let executor = executors.iter().find(|e| e.supports(&action));

                if let Some(executor) = executor {
                    let result = match config.execution_mode {
                        ExecutionMode::DryRun => executor.simulate(&action).await,
                        ExecutionMode::Live | ExecutionMode::Force => executor.execute(action).await,
                    };

                    match result {
                        Ok(ExecutionResult::Success { action_id, tx_hash, profit, gas_used }) => {
                            successful += 1;
                            info!(
                                "Execution SUCCESS: {} | tx: {:?} | profit: {} wei | gas: {}",
                                action_id, tx_hash, profit, gas_used
                            );
                        }
                        Ok(ExecutionResult::Simulated { action_id, would_profit, gas_estimate }) => {
                            info!(
                                "Simulation: {} | profit: {} wei | gas: {}",
                                action_id, would_profit, gas_estimate
                            );
                        }
                        Ok(ExecutionResult::Failed { action_id, reason }) => {
                            warn!("Execution FAILED: {} | reason: {}", action_id, reason);
                        }
                        Err(e) => {
                            error!("Executor error: {}", e);
                        }
                    }
                } else {
                    warn!("No executor found for action");
                }

                if executed % 100 == 0 {
                    info!(
                        "Executor stats: {} executed, {} successful",
                        executed, successful
                    );
                }
            }
        });

        // Stats reporter
        let stats_reporter = tokio::spawn(async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(30));
            loop {
                interval.tick().await;
                info!("Engine running - collectors active");
            }
        });

        // Wait for shutdown
        tokio::select! {
            _ = event_processor => {
                info!("Event processor stopped");
            }
            _ = action_executor => {
                info!("Action executor stopped");
            }
            _ = stats_reporter => {
                info!("Stats reporter stopped");
            }
            _ = tokio::signal::ctrl_c() => {
                info!("Shutdown signal received");
            }
        }

        // Cleanup
        for handle in collector_handles {
            handle.abort().await;
        }

        for strategy in &self.strategies {
            let _ = strategy.on_stop().await;
        }

        info!("Artemis Engine stopped");
        Ok(())
    }
}
