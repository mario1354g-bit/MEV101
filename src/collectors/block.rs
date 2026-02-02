//! Block collector - monitors new blocks

use crate::artemis::{Collector, Event, NewBlockEvent};
use alloy::primitives::B256;
use alloy::providers::{Provider, ProviderBuilder, WsConnect};
use async_trait::async_trait;
use futures::StreamExt;
use tokio::sync::mpsc;
use tracing::{error, info};

/// Block collector - subscribes to new blocks
pub struct BlockCollector {
    ws_url: String,
}

impl BlockCollector {
    pub fn new(ws_url: String) -> Self {
        Self { ws_url }
    }
}

#[async_trait]
impl Collector for BlockCollector {
    fn name(&self) -> &str {
        "BlockCollector"
    }

    async fn collect(&self, sender: mpsc::Sender<Event>) -> eyre::Result<()> {
        let ws = WsConnect::new(&self.ws_url);
        let provider = ProviderBuilder::new().on_ws(ws).await?;

        info!("BlockCollector: Connected");

        // Subscribe to new block headers
        let sub = provider.subscribe_blocks().await?;
        let mut stream = sub.into_stream();

        while let Some(header) = stream.next().await {
            // subscribe_blocks returns headers, not full blocks
            let event = Event::NewBlock(NewBlockEvent {
                block_number: header.number,
                block_hash: header.hash,
                timestamp: header.timestamp,
                base_fee: header.base_fee_per_gas.map(|f| f as u128),
            });

            info!(
                "BlockCollector: New block {} (base_fee: {:?})",
                header.number, header.base_fee_per_gas
            );

            if sender.send(event).await.is_err() {
                error!("Event channel closed");
                break;
            }
        }

        Ok(())
    }
}
