//! Executor trait for executing actions on-chain
//!
//! Executors are responsible for:
//! - Taking actions from strategies
//! - Simulating transactions
//! - Submitting to Flashbots or directly to mempool
//! - Tracking execution results

use super::types::{Action, ExecutionResult};
use async_trait::async_trait;

/// Executor trait - executes actions on-chain
#[async_trait]
pub trait Executor: Send + Sync {
    /// Returns the name of this executor
    fn name(&self) -> &str;

    /// Execute an action
    async fn execute(&self, action: Action) -> eyre::Result<ExecutionResult>;

    /// Simulate an action without submitting
    async fn simulate(&self, action: &Action) -> eyre::Result<ExecutionResult>;

    /// Check if executor supports this action type
    fn supports(&self, action: &Action) -> bool;
}

/// Execution mode
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionMode {
    /// Dry run - simulate only, don't submit
    DryRun,
    /// Live - simulate then submit
    Live,
    /// Force - submit without simulation (dangerous)
    Force,
}

impl Default for ExecutionMode {
    fn default() -> Self {
        Self::DryRun
    }
}

/// Submission target
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmissionTarget {
    /// Submit directly to mempool
    Mempool,
    /// Submit to Flashbots Protect
    FlashbotsProtect,
    /// Submit as Flashbots bundle
    FlashbotsBundle,
    /// Submit to MEV-Share
    MevShare,
}

impl Default for SubmissionTarget {
    fn default() -> Self {
        Self::FlashbotsBundle
    }
}
