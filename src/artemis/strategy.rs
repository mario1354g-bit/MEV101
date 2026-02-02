//! Strategy trait for processing events and producing actions
//!
//! Strategies are responsible for:
//! - Processing incoming events
//! - Detecting MEV opportunities
//! - Producing actions for executors

use super::types::{Action, Event};
use async_trait::async_trait;

/// Strategy trait - processes events and produces actions
#[async_trait]
pub trait Strategy: Send + Sync {
    /// Returns the name of this strategy
    fn name(&self) -> &str;

    /// Process an event and optionally return an action
    async fn process_event(&self, event: &Event) -> eyre::Result<Option<Action>>;

    /// Called when the strategy is started
    async fn on_start(&self) -> eyre::Result<()> {
        Ok(())
    }

    /// Called when the strategy is stopped
    async fn on_stop(&self) -> eyre::Result<()> {
        Ok(())
    }
}

/// Strategy with state that needs periodic sync
#[async_trait]
pub trait StatefulStrategy: Strategy {
    /// Sync state from chain (called periodically)
    async fn sync_state(&self) -> eyre::Result<()>;

    /// Get the sync interval in seconds
    fn sync_interval(&self) -> u64 {
        60 // Default 60 seconds
    }
}
