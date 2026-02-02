//! Longtail MEV Monitor Library
//!
//! A comprehensive MEV (Maximal Extractable Value) monitoring and execution framework
//! for Ethereum and EVM-compatible chains.

pub mod config;
pub mod dashboard;
pub mod detectors;
pub mod dex;
pub mod error;
pub mod executor;
pub mod monitors;
pub mod simulation;
pub mod storage;

pub use config::Config;
pub use dashboard::{AppState, Dashboard, DashboardError};
pub use error::{
    ConfigError, DatabaseError, DecodingError, ExecutionError, MevError, ProviderError,
    SimulationError,
};
pub use storage::{Database, StorageError};

// Re-export dex types (without conflicting names)
pub mod dex_types {
    pub use crate::dex::{
        Dex, DexError, DexResult, PoolInfo, PoolRegistry, PoolType, PriceInfo,
        Reserves, SwapParams, UniswapV2, UniswapV3,
    };
}

// Re-export simulation types
pub mod simulation_types {
    pub use crate::simulation::{
        BundleSimulation, CacheStats, EthCallSimulator, ForkDB, GasEstimator, Opportunity,
        ParallelSimStats, ParallelSimulator, RevmSimulationResult, RevmSimulator,
        RevmStateChange, RevmTransaction, SandwichSimResult, SharedForkDB, SimulationAggregator,
        SimulationResult, Simulator,
    };
}

// Re-export executor types
pub mod executor_types {
    pub use crate::executor::{
        ArbitrageExecutor, BackrunExecutor, Config, ExecutionResult,
        Executor, ExecutorContext, ExecutorManager, FlashbotsBundle, FlashbotsClient,
        FlashloanExecutor, LiquidationExecutor, Opportunity,
        OpportunityType, SandwichExecutor, SimulationResult,
        SwapPath, SwapStep, TargetTransaction,
    };
}
