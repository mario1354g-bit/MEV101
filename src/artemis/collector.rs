//! Collector trait for gathering events from external sources
//!
//! Collectors are responsible for:
//! - Subscribing to external event sources (mempool, blocks, DEX events)
//! - Converting raw data into internal Event types
//! - Streaming events to the engine

use super::types::Event;
use async_trait::async_trait;
use tokio::sync::mpsc;

/// Collector trait - gathers events from external sources
#[async_trait]
pub trait Collector: Send + Sync {
    /// Returns the name of this collector
    fn name(&self) -> &str;

    /// Start collecting events and send them to the provided channel
    async fn collect(&self, sender: mpsc::Sender<Event>) -> eyre::Result<()>;
}

/// Collector handle for managing collector lifecycle
pub struct CollectorHandle {
    pub name: String,
    pub handle: tokio::task::JoinHandle<eyre::Result<()>>,
}

impl CollectorHandle {
    pub fn new(name: String, handle: tokio::task::JoinHandle<eyre::Result<()>>) -> Self {
        Self { name, handle }
    }

    pub fn is_finished(&self) -> bool {
        self.handle.is_finished()
    }

    pub async fn abort(self) {
        self.handle.abort();
        let _ = self.handle.await;
    }
}
