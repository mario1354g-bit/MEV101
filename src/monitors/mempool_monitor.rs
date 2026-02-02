//! Mempool monitor for tracking pending transactions
//!
//! This monitor subscribes to pending transactions via WebSocket and filters
//! for DEX router interactions, decoding swap parameters for MEV detection.

use alloy::consensus::Transaction as ConsensusTx;
use alloy::primitives::{address, Address, Bytes, FixedBytes, TxHash, U256};
use alloy::providers::{Provider, ProviderBuilder, RootProvider, WsConnect};
use alloy::pubsub::PubSubFrontend;
use alloy::rpc::types::Transaction as RpcTransaction;
use async_trait::async_trait;
use std::collections::{HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, RwLock};
use tracing::{debug, error, info, trace, warn};

use super::{
    reconnect_with_backoff, Monitor, MonitorEvent, ReconnectConfig, SwapParams, Transaction,
};
use crate::error::{MevError, ProviderError, Result};

/// Known DEX router addresses (Ethereum mainnet)
pub mod routers {
    use super::*;

    /// Uniswap V2 Router
    pub const UNISWAP_V2_ROUTER: Address = address!("7a250d5630B4cF539739dF2C5dAcb4c659F2488D");
    /// Uniswap V3 Router
    pub const UNISWAP_V3_ROUTER: Address = address!("E592427A0AEce92De3Edee1F18E0157C05861564");
    /// Uniswap V3 Router 2
    pub const UNISWAP_V3_ROUTER_2: Address = address!("68b3465833fb72A70ecDF485E0e4C7bD8665Fc45");
    /// Uniswap Universal Router
    pub const UNISWAP_UNIVERSAL_ROUTER: Address =
        address!("3fC91A3afd70395Cd496C647d5a6CC9D4B2b7FAD");
    /// Sushiswap Router
    pub const SUSHISWAP_ROUTER: Address = address!("d9e1cE17f2641f24aE83637ab66a2cca9C378B9F");
    /// 1inch V5 Router
    pub const ONEINCH_V5_ROUTER: Address = address!("1111111254EEB25477B68fb85Ed929f73A960582");
    /// 0x Exchange Proxy
    pub const ZRX_EXCHANGE_PROXY: Address = address!("Def1C0ded9bec7F1a1670819833240f027b25EfF");
    /// Paraswap V5
    pub const PARASWAP_V5: Address = address!("216B4B4Ba9F3e719726886d34a177484278Bfcae");
    /// Curve Router
    pub const CURVE_ROUTER: Address = address!("99a58482BD75cbab83b27EC03CA68fF489b5788f");

    /// Get all known router addresses
    pub fn all_routers() -> Vec<Address> {
        vec![
            UNISWAP_V2_ROUTER,
            UNISWAP_V3_ROUTER,
            UNISWAP_V3_ROUTER_2,
            UNISWAP_UNIVERSAL_ROUTER,
            SUSHISWAP_ROUTER,
            ONEINCH_V5_ROUTER,
            ZRX_EXCHANGE_PROXY,
            PARASWAP_V5,
            CURVE_ROUTER,
        ]
    }
}

/// Function selectors for common swap functions
pub mod selectors {
    use alloy::primitives::FixedBytes;

    /// swapExactTokensForTokens(uint256,uint256,address[],address,uint256)
    pub const SWAP_EXACT_TOKENS_FOR_TOKENS: FixedBytes<4> =
        FixedBytes::new([0x38, 0xed, 0x17, 0x39]);
    /// swapTokensForExactTokens(uint256,uint256,address[],address,uint256)
    pub const SWAP_TOKENS_FOR_EXACT_TOKENS: FixedBytes<4> =
        FixedBytes::new([0x88, 0x03, 0xdb, 0xee]);
    /// swapExactETHForTokens(uint256,address[],address,uint256)
    pub const SWAP_EXACT_ETH_FOR_TOKENS: FixedBytes<4> = FixedBytes::new([0x7f, 0xf3, 0x6a, 0xb5]);
    /// swapTokensForExactETH(uint256,uint256,address[],address,uint256)
    pub const SWAP_TOKENS_FOR_EXACT_ETH: FixedBytes<4> = FixedBytes::new([0x4a, 0x25, 0xd9, 0x4a]);
    /// swapExactTokensForETH(uint256,uint256,address[],address,uint256)
    pub const SWAP_EXACT_TOKENS_FOR_ETH: FixedBytes<4> = FixedBytes::new([0x18, 0xcb, 0xaf, 0xe5]);
    /// swapETHForExactTokens(uint256,address[],address,uint256)
    pub const SWAP_ETH_FOR_EXACT_TOKENS: FixedBytes<4> = FixedBytes::new([0xfb, 0x3b, 0xdb, 0x41]);
    /// exactInputSingle (V3)
    pub const EXACT_INPUT_SINGLE: FixedBytes<4> = FixedBytes::new([0x41, 0x4b, 0xf3, 0x89]);
    /// exactInput (V3)
    pub const EXACT_INPUT: FixedBytes<4> = FixedBytes::new([0xc0, 0x4b, 0x8d, 0x59]);
    /// exactOutputSingle (V3)
    pub const EXACT_OUTPUT_SINGLE: FixedBytes<4> = FixedBytes::new([0xdb, 0x3e, 0x21, 0x98]);
    /// exactOutput (V3)
    pub const EXACT_OUTPUT: FixedBytes<4> = FixedBytes::new([0xf2, 0x8c, 0x05, 0x98]);
    /// multicall (Universal Router)
    pub const MULTICALL: FixedBytes<4> = FixedBytes::new([0xac, 0x96, 0x50, 0xd8]);
    /// execute (Universal Router)
    pub const EXECUTE: FixedBytes<4> = FixedBytes::new([0x24, 0x85, 0x6b, 0xc3]);
}

/// Statistics for mempool monitoring
#[derive(Debug, Default)]
pub struct MempoolStats {
    /// Total transactions seen
    pub total_seen: u64,
    /// Transactions to known routers
    pub router_txs: u64,
    /// Successfully decoded swaps
    pub decoded_swaps: u64,
    /// Failed decodes
    pub failed_decodes: u64,
    /// Transactions filtered out
    pub filtered_out: u64,
}

/// Configuration for mempool monitoring
#[derive(Debug, Clone)]
pub struct MempoolConfig {
    /// Router addresses to monitor
    pub routers: HashSet<Address>,
    /// Minimum transaction value to process (in wei)
    pub min_value: U256,
    /// Whether to fetch full transaction details
    pub fetch_full_tx: bool,
    /// Maximum pending transactions to process per second
    pub rate_limit: Option<u32>,
}

impl Default for MempoolConfig {
    fn default() -> Self {
        Self {
            routers: routers::all_routers().into_iter().collect(),
            min_value: U256::ZERO,
            fetch_full_tx: true,
            rate_limit: None,
        }
    }
}

/// Mempool monitor for pending transactions
pub struct MempoolMonitor {
    /// WebSocket URL
    ws_url: String,
    /// HTTP RPC URL for fetching full transactions
    http_url: String,
    /// Monitor name
    name: String,
    /// Configuration
    config: MempoolConfig,
    /// Whether the monitor is running
    running: AtomicBool,
    /// Stop signal sender
    stop_tx: RwLock<Option<mpsc::Sender<()>>>,
    /// Statistics
    stats: RwLock<MempoolStats>,
    /// Reconnection configuration
    reconnect_config: ReconnectConfig,
    /// Seen transaction hashes with LRU ordering (HashSet for O(1) lookup, VecDeque for eviction order)
    seen_txs: RwLock<(HashSet<TxHash>, VecDeque<TxHash>)>,
    /// Maximum seen transactions to keep
    max_seen_txs: usize,
}

impl MempoolMonitor {
    /// Create a new mempool monitor
    pub fn new(ws_url: String, http_url: String) -> Self {
        Self {
            ws_url,
            http_url,
            name: "MempoolMonitor".to_string(),
            config: MempoolConfig::default(),
            running: AtomicBool::new(false),
            stop_tx: RwLock::new(None),
            stats: RwLock::new(MempoolStats::default()),
            reconnect_config: ReconnectConfig::default(),
            seen_txs: RwLock::new((HashSet::new(), VecDeque::new())),
            max_seen_txs: 100_000,
        }
    }

    /// Create with custom configuration
    pub fn with_config(ws_url: String, http_url: String, config: MempoolConfig) -> Self {
        Self {
            ws_url,
            http_url,
            name: "MempoolMonitor".to_string(),
            config,
            running: AtomicBool::new(false),
            stop_tx: RwLock::new(None),
            stats: RwLock::new(MempoolStats::default()),
            reconnect_config: ReconnectConfig::default(),
            seen_txs: RwLock::new((HashSet::new(), VecDeque::new())),
            max_seen_txs: 100_000,
        }
    }

    /// Get current statistics
    pub async fn stats(&self) -> MempoolStats {
        let stats = self.stats.read().await;
        MempoolStats {
            total_seen: stats.total_seen,
            router_txs: stats.router_txs,
            decoded_swaps: stats.decoded_swaps,
            failed_decodes: stats.failed_decodes,
            filtered_out: stats.filtered_out,
        }
    }

    /// Connect to the WebSocket provider
    async fn connect_ws(&self) -> Result<RootProvider<PubSubFrontend>> {
        let ws = WsConnect::new(&self.ws_url);
        let provider = ProviderBuilder::new()
            .on_ws(ws)
            .await
            .map_err(|e| MevError::Provider(ProviderError::WebSocketError(e.to_string())))?;
        Ok(provider)
    }

    /// Check if a transaction targets a known router
    fn is_router_target(&self, to: Option<Address>) -> bool {
        to.map(|addr| self.config.routers.contains(&addr))
            .unwrap_or(false)
    }

    /// Decode swap parameters from transaction input
    fn decode_swap_params(&self, to: Address, input: &Bytes, value: U256) -> Option<SwapParams> {
        if input.len() < 4 {
            return None;
        }

        let selector: [u8; 4] = input[0..4].try_into().ok()?;
        let selector = FixedBytes::new(selector);
        let data = &input[4..];

        // Try to decode based on selector
        match selector {
            s if s == selectors::SWAP_EXACT_TOKENS_FOR_TOKENS => {
                self.decode_v2_swap(to, data, false, false)
            }
            s if s == selectors::SWAP_TOKENS_FOR_EXACT_TOKENS => {
                self.decode_v2_swap(to, data, false, false)
            }
            s if s == selectors::SWAP_EXACT_ETH_FOR_TOKENS => {
                self.decode_v2_eth_swap(to, data, value, true)
            }
            s if s == selectors::SWAP_TOKENS_FOR_EXACT_ETH => {
                self.decode_v2_swap(to, data, false, true)
            }
            s if s == selectors::SWAP_EXACT_TOKENS_FOR_ETH => {
                self.decode_v2_swap(to, data, false, true)
            }
            s if s == selectors::SWAP_ETH_FOR_EXACT_TOKENS => {
                self.decode_v2_eth_swap(to, data, value, true)
            }
            s if s == selectors::EXACT_INPUT_SINGLE => self.decode_v3_single(to, data),
            s if s == selectors::EXACT_INPUT => self.decode_v3_exact_input(to, data),
            s if s == selectors::EXACT_OUTPUT_SINGLE => self.decode_v3_single(to, data),
            s if s == selectors::EXACT_OUTPUT => self.decode_v3_exact_output(to, data),
            _ => None, // Unknown selector
        }
    }

    /// Decode V2-style swap parameters
    fn decode_v2_swap(
        &self,
        router: Address,
        data: &[u8],
        _eth_in: bool,
        _eth_out: bool,
    ) -> Option<SwapParams> {
        // swapExactTokensForTokens(uint256 amountIn, uint256 amountOutMin, address[] path, address to, uint256 deadline)
        if data.len() < 160 {
            return None;
        }

        let amount_in = U256::from_be_slice(&data[0..32]);
        let amount_out_min = U256::from_be_slice(&data[32..64]);
        // path offset is at 64..96
        let recipient = Address::from_slice(&data[76..96]);
        let deadline = U256::from_be_slice(&data[96..128]);

        // Decode path array
        let path_offset = U256::from_be_slice(&data[64..96]).to::<usize>();
        if path_offset + 32 > data.len() {
            return None;
        }

        let path_len = U256::from_be_slice(&data[path_offset..path_offset + 32]).to::<usize>();
        let mut path = Vec::with_capacity(path_len);

        for i in 0..path_len {
            let start = path_offset + 32 + i * 32 + 12; // Skip 12 bytes of padding
            if start + 20 > data.len() {
                return None;
            }
            path.push(Address::from_slice(&data[start..start + 20]));
        }

        if path.len() < 2 {
            return None;
        }

        Some(SwapParams {
            router,
            token_in: path[0],
            token_out: path[path.len() - 1],
            amount_in,
            amount_out_min,
            path,
            deadline: Some(deadline),
            recipient,
        })
    }

    /// Decode V2 ETH swap parameters
    fn decode_v2_eth_swap(
        &self,
        router: Address,
        data: &[u8],
        value: U256,
        _eth_in: bool,
    ) -> Option<SwapParams> {
        // swapExactETHForTokens(uint256 amountOutMin, address[] path, address to, uint256 deadline)
        if data.len() < 128 {
            return None;
        }

        let amount_out_min = U256::from_be_slice(&data[0..32]);
        // path offset at 32..64
        let recipient = Address::from_slice(&data[44..64]);
        let deadline = U256::from_be_slice(&data[64..96]);

        // Decode path
        let path_offset = U256::from_be_slice(&data[32..64]).to::<usize>();
        if path_offset + 32 > data.len() {
            return None;
        }

        let path_len = U256::from_be_slice(&data[path_offset..path_offset + 32]).to::<usize>();
        let mut path = Vec::with_capacity(path_len);

        for i in 0..path_len {
            let start = path_offset + 32 + i * 32 + 12;
            if start + 20 > data.len() {
                return None;
            }
            path.push(Address::from_slice(&data[start..start + 20]));
        }

        if path.len() < 2 {
            return None;
        }

        Some(SwapParams {
            router,
            token_in: path[0],
            token_out: path[path.len() - 1],
            amount_in: value, // ETH value is the amount in
            amount_out_min,
            path,
            deadline: Some(deadline),
            recipient,
        })
    }

    /// Decode V3 exactInputSingle parameters
    fn decode_v3_single(&self, router: Address, data: &[u8]) -> Option<SwapParams> {
        // ExactInputSingleParams { tokenIn, tokenOut, fee, recipient, deadline, amountIn, amountOutMinimum, sqrtPriceLimitX96 }
        if data.len() < 256 {
            return None;
        }

        let token_in = Address::from_slice(&data[12..32]);
        let token_out = Address::from_slice(&data[44..64]);
        // fee at 64..96
        let recipient = Address::from_slice(&data[76..96]);
        let deadline = U256::from_be_slice(&data[96..128]);
        let amount_in = U256::from_be_slice(&data[128..160]);
        let amount_out_min = U256::from_be_slice(&data[160..192]);

        Some(SwapParams {
            router,
            token_in,
            token_out,
            amount_in,
            amount_out_min,
            path: vec![token_in, token_out],
            deadline: Some(deadline),
            recipient,
        })
    }

    /// Decode V3 exactInput parameters
    fn decode_v3_exact_input(&self, router: Address, data: &[u8]) -> Option<SwapParams> {
        // ExactInputParams { path, recipient, deadline, amountIn, amountOutMinimum }
        if data.len() < 160 {
            return None;
        }

        // Path is encoded as bytes, starting with offset
        let path_offset = U256::from_be_slice(&data[0..32]).to::<usize>();
        let recipient = Address::from_slice(&data[44..64]);
        let deadline = U256::from_be_slice(&data[64..96]);
        let amount_in = U256::from_be_slice(&data[96..128]);
        let amount_out_min = U256::from_be_slice(&data[128..160]);

        // Decode path bytes (format: token0 || fee || token1 || fee || token2 ...)
        if path_offset + 32 > data.len() {
            return None;
        }

        let path_len = U256::from_be_slice(&data[path_offset..path_offset + 32]).to::<usize>();
        if path_offset + 32 + path_len > data.len() {
            return None;
        }

        let path_bytes = &data[path_offset + 32..path_offset + 32 + path_len];

        // V3 path format: token0 (20) + fee (3) + token1 (20) + fee (3) + ... + tokenN (20)
        // Total length for N tokens = 20 + (N-1) * 23
        // Minimum valid path is 2 tokens: 20 + 3 + 20 = 43 bytes
        if path_bytes.len() < 43 {
            return None;
        }

        let mut path = Vec::new();
        let mut offset = 0;

        // First token (20 bytes)
        path.push(Address::from_slice(&path_bytes[offset..offset + 20]));
        offset += 20;

        // Remaining tokens: each preceded by 3-byte fee
        while offset + 23 <= path_bytes.len() {
            offset += 3; // Skip fee
            path.push(Address::from_slice(&path_bytes[offset..offset + 20]));
            offset += 20;
        }

        // Check for final token if there's exactly enough bytes left
        // This handles the case where path ends with just a token (no trailing fee)
        if offset + 3 == path_bytes.len() - 20 + 3 && path_bytes.len() > offset {
            // There might be a fee + final token remaining
            if offset + 3 + 20 == path_bytes.len() {
                offset += 3;
                path.push(Address::from_slice(&path_bytes[offset..offset + 20]));
            }
        }

        if path.len() < 2 {
            return None;
        }

        Some(SwapParams {
            router,
            token_in: path[0],
            token_out: path[path.len() - 1],
            amount_in,
            amount_out_min,
            path,
            deadline: Some(deadline),
            recipient,
        })
    }

    /// Decode V3 exactOutput parameters
    fn decode_v3_exact_output(&self, router: Address, data: &[u8]) -> Option<SwapParams> {
        // Similar to exactInput but reversed
        self.decode_v3_exact_input(router, data)
    }

    /// Convert RPC transaction to our Transaction type
    fn convert_transaction(&self, tx: &RpcTransaction, first_seen: Instant) -> Transaction {
        // Access transaction fields via the inner consensus transaction
        let to_addr = tx.inner.to();
        let input_data = tx.inner.input().clone();
        let tx_value = tx.inner.value();

        let swap_params = to_addr
            .and_then(|to| self.decode_swap_params(to, &input_data, tx_value));

        // max_priority_fee_per_gas returns Option<u128> for EIP-1559 txs
        // max_fee_per_gas returns u128 directly
        let max_priority = tx.inner.max_priority_fee_per_gas();
        let max_fee = tx.inner.max_fee_per_gas();

        Transaction {
            hash: *tx.inner.tx_hash(),
            from: tx.from,
            to: to_addr,
            value: tx_value,
            input: input_data,
            gas_price: tx.effective_gas_price.map(U256::from),
            max_priority_fee: max_priority.map(U256::from),
            max_fee_per_gas: Some(U256::from(max_fee)),
            gas: tx.inner.gas_limit(),
            nonce: tx.inner.nonce(),
            swap_params,
            first_seen,
        }
    }

    /// Check if we've seen this transaction before using LRU-style eviction
    async fn mark_seen(&self, hash: TxHash) -> bool {
        let mut guard = self.seen_txs.write().await;
        let (set, queue) = &mut *guard;

        // Check if already seen
        if set.contains(&hash) {
            return true;
        }

        // Evict oldest entries if at capacity (LRU-style)
        while set.len() >= self.max_seen_txs {
            if let Some(old_hash) = queue.pop_front() {
                set.remove(&old_hash);
            } else {
                break;
            }
        }

        // Insert new hash
        set.insert(hash);
        queue.push_back(hash);

        false
    }

    /// Main monitoring loop
    async fn run_monitoring_loop(
        &self,
        event_tx: mpsc::Sender<MonitorEvent>,
        mut stop_rx: mpsc::Receiver<()>,
    ) {
        // Create HTTP provider for fetching full transactions
        let http_provider = ProviderBuilder::new().on_http(self.http_url.parse().unwrap());

        while self.running.load(Ordering::Relaxed) {
            // Connect with exponential backoff
            let ws_provider = match reconnect_with_backoff(
                &self.reconnect_config,
                &self.name,
                || async { self.connect_ws().await },
            )
            .await
            {
                Ok(p) => p,
                Err(e) => {
                    error!("{}: Failed to connect after all retries: {:?}", self.name, e);
                    break;
                }
            };

            info!(
                "{}: Connected to WebSocket, subscribing to pending transactions",
                self.name
            );

            // Subscribe to pending transactions
            let subscription = match ws_provider.subscribe_pending_transactions().await {
                Ok(sub) => sub,
                Err(e) => {
                    error!(
                        "{}: Failed to subscribe to pending transactions: {:?}",
                        self.name, e
                    );
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    continue;
                }
            };

            let mut stream = subscription.into_stream();

            loop {
                tokio::select! {
                    _ = stop_rx.recv() => {
                        info!("{}: Received stop signal", self.name);
                        return;
                    }
                    tx_hash_result = futures::StreamExt::next(&mut stream) => {
                        match tx_hash_result {
                            Some(tx_hash) => {
                                let first_seen = Instant::now();

                                // Update stats
                                {
                                    let mut stats = self.stats.write().await;
                                    stats.total_seen += 1;
                                }

                                // Check if already seen
                                if self.mark_seen(tx_hash).await {
                                    continue;
                                }

                                // Fetch full transaction if configured
                                if self.config.fetch_full_tx {
                                    let tx = match http_provider.get_transaction_by_hash(tx_hash).await {
                                        Ok(Some(tx)) => tx,
                                        Ok(None) => {
                                            trace!("{}: Transaction not found: {:?}", self.name, tx_hash);
                                            continue;
                                        }
                                        Err(e) => {
                                            trace!("{}: Failed to fetch transaction: {:?}", self.name, e);
                                            continue;
                                        }
                                    };

                                    // Check if it targets a known router
                                    if !self.is_router_target(tx.inner.to()) {
                                        let mut stats = self.stats.write().await;
                                        stats.filtered_out += 1;
                                        continue;
                                    }

                                    {
                                        let mut stats = self.stats.write().await;
                                        stats.router_txs += 1;
                                    }

                                    // Convert and check for swap params
                                    let transaction = self.convert_transaction(&tx, first_seen);

                                    if transaction.swap_params.is_some() {
                                        let mut stats = self.stats.write().await;
                                        stats.decoded_swaps += 1;
                                    }

                                    debug!(
                                        "{}: Pending swap detected - hash: {:?}, from: {:?}, to: {:?}",
                                        self.name, transaction.hash, transaction.from, transaction.to
                                    );

                                    // Send event
                                    if let Err(e) = event_tx.send(MonitorEvent::PendingTransaction(Box::new(transaction))).await {
                                        error!("{}: Failed to send transaction event: {:?}", self.name, e);
                                        return;
                                    }
                                }
                            }
                            None => {
                                warn!("{}: Pending transaction stream ended", self.name);
                                break;
                            }
                        }
                    }
                }
            }

            if self.running.load(Ordering::Relaxed) {
                warn!("{}: Connection lost, attempting to reconnect...", self.name);
            }
        }
    }
}

/// NOTE: The Monitor trait implementation for MempoolMonitor uses unsafe raw pointers
/// which can cause use-after-free bugs. Use ArcMempoolMonitor for safe usage.
/// This implementation is kept for backward compatibility but should not be used directly.
#[async_trait]
impl Monitor for MempoolMonitor {
    fn name(&self) -> &str {
        &self.name
    }

    async fn start(&self, _tx: mpsc::Sender<MonitorEvent>) -> Result<()> {
        // This implementation is deprecated due to safety concerns.
        // Use ArcMempoolMonitor::start() instead which uses safe Arc-based patterns.
        Err(MevError::Provider(ProviderError::SubscriptionError(
            "Direct MempoolMonitor::start() is unsafe. Use ArcMempoolMonitor instead.".to_string(),
        )))
    }

    async fn stop(&self) -> Result<()> {
        if !self.running.swap(false, Ordering::Relaxed) {
            return Ok(());
        }

        let stop_tx = {
            let mut guard = self.stop_tx.write().await;
            guard.take()
        };

        if let Some(tx) = stop_tx {
            let _ = tx.send(()).await;
        }

        info!("{}: Stopped", self.name);
        Ok(())
    }

    fn is_running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }
}

/// Safe Arc wrapper for MempoolMonitor
pub struct ArcMempoolMonitor(pub Arc<MempoolMonitor>);

impl ArcMempoolMonitor {
    pub fn new(ws_url: String, http_url: String) -> Self {
        Self(Arc::new(MempoolMonitor::new(ws_url, http_url)))
    }

    pub async fn start(&self, tx: mpsc::Sender<MonitorEvent>) -> Result<()> {
        let monitor = Arc::clone(&self.0);

        if monitor.running.swap(true, Ordering::Relaxed) {
            return Err(MevError::Provider(ProviderError::SubscriptionError(
                "Mempool monitor is already running".to_string(),
            )));
        }

        let (stop_tx, stop_rx) = mpsc::channel(1);
        {
            let mut guard = monitor.stop_tx.write().await;
            *guard = Some(stop_tx);
        }

        tokio::spawn(async move {
            monitor.run_monitoring_loop(tx, stop_rx).await;
        });

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_routers_list() {
        let routers = routers::all_routers();
        assert!(routers.len() >= 9);
        assert!(routers.contains(&routers::UNISWAP_V2_ROUTER));
        assert!(routers.contains(&routers::UNISWAP_V3_ROUTER));
    }

    #[test]
    fn test_mempool_config_default() {
        let config = MempoolConfig::default();
        assert!(config.routers.contains(&routers::UNISWAP_V2_ROUTER));
        assert!(config.fetch_full_tx);
        assert_eq!(config.min_value, U256::ZERO);
    }

    #[tokio::test]
    async fn test_mempool_monitor_creation() {
        let monitor = MempoolMonitor::new(
            "ws://localhost:8546".to_string(),
            "http://localhost:8545".to_string(),
        );
        assert!(!monitor.is_running());

        let stats = monitor.stats().await;
        assert_eq!(stats.total_seen, 0);
    }

    #[test]
    fn test_is_router_target() {
        let monitor = MempoolMonitor::new(
            "ws://localhost:8546".to_string(),
            "http://localhost:8545".to_string(),
        );

        assert!(monitor.is_router_target(Some(routers::UNISWAP_V2_ROUTER)));
        assert!(monitor.is_router_target(Some(routers::SUSHISWAP_ROUTER)));
        assert!(!monitor.is_router_target(None));
        assert!(!monitor.is_router_target(Some(Address::ZERO)));
    }
}
