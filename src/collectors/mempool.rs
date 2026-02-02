//! Mempool collector - monitors pending transactions

use crate::artemis::{Collector, Event, PendingTxEvent};
use alloy::primitives::B256;
use alloy::providers::{Provider, ProviderBuilder, WsConnect};
use async_trait::async_trait;
use futures::StreamExt;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

/// Mempool collector configuration
#[derive(Debug, Clone)]
pub struct MempoolCollectorConfig {
    pub ws_url: String,
    pub fetch_full_tx: bool,
    pub sample_rate: u32, // Fetch every Nth tx
}

/// Mempool collector - subscribes to pending transactions
pub struct MempoolCollector {
    config: MempoolCollectorConfig,
}

impl MempoolCollector {
    pub fn new(config: MempoolCollectorConfig) -> Self {
        Self { config }
    }
}

#[async_trait]
impl Collector for MempoolCollector {
    fn name(&self) -> &str {
        "MempoolCollector"
    }

    async fn collect(&self, sender: mpsc::Sender<Event>) -> eyre::Result<()> {
        let ws = WsConnect::new(&self.config.ws_url);
        let provider = ProviderBuilder::new().on_ws(ws).await?;
        let provider = Arc::new(provider);

        info!("MempoolCollector: Connected to {}", self.config.ws_url);

        // Subscribe to pending transactions
        let sub = provider.subscribe_pending_transactions().await?;
        let mut stream = sub.into_stream();

        let mut count = 0u64;
        let sample_rate = self.config.sample_rate;

        while let Some(tx_hash) = stream.next().await {
            count += 1;

            // Sample transactions to reduce load
            if self.config.fetch_full_tx && count % sample_rate as u64 == 0 {
                let provider = Arc::clone(&provider);
                let sender = sender.clone();

                tokio::spawn(async move {
                    match provider.get_transaction_by_hash(tx_hash).await {
                        Ok(Some(tx)) => {
                            let event = Event::PendingTx(PendingTxEvent {
                                tx,
                                received_at: chrono::Utc::now(),
                            });
                            if sender.send(event).await.is_err() {
                                debug!("Event channel closed");
                            }
                        }
                        Ok(None) => {
                            debug!("Transaction not found: {:?}", tx_hash);
                        }
                        Err(e) => {
                            warn!("Failed to fetch tx {:?}: {}", tx_hash, e);
                        }
                    }
                });
            }

            if count % 1000 == 0 {
                info!("MempoolCollector: {} pending txs seen", count);
            }
        }

        Ok(())
    }
}

/// High-frequency mempool collector - fetches ALL transactions
pub struct HighFreqMempoolCollector {
    ws_url: String,
}

impl HighFreqMempoolCollector {
    pub fn new(ws_url: String) -> Self {
        Self { ws_url }
    }
}

#[async_trait]
impl Collector for HighFreqMempoolCollector {
    fn name(&self) -> &str {
        "HighFreqMempoolCollector"
    }

    async fn collect(&self, sender: mpsc::Sender<Event>) -> eyre::Result<()> {
        let ws = WsConnect::new(&self.ws_url);
        let provider = ProviderBuilder::new().on_ws(ws).await?;
        let provider = Arc::new(provider);

        info!("HighFreqMempoolCollector: Connected");

        let sub = provider.subscribe_pending_transactions().await?;
        let mut stream = sub.into_stream();

        while let Some(tx_hash) = stream.next().await {
            let provider = Arc::clone(&provider);
            let sender = sender.clone();

            // Fetch every transaction in parallel
            tokio::spawn(async move {
                if let Ok(Some(tx)) = provider.get_transaction_by_hash(tx_hash).await {
                    let event = Event::PendingTx(PendingTxEvent {
                        tx,
                        received_at: chrono::Utc::now(),
                    });
                    let _ = sender.send(event).await;
                }
            });
        }

        Ok(())
    }
}
