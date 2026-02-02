//! Flashbots bundle submission client.
//!
//! This module provides functionality for submitting transaction bundles
//! to Flashbots relays, enabling private transaction submission and
//! MEV protection.

use alloy::primitives::{keccak256, Address, Bytes, B256, U256};
use alloy::signers::local::PrivateKeySigner;
use alloy::signers::Signer;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::{debug, error, info, instrument};

use crate::error::{ExecutionError, MevError};

/// Result type for Flashbots operations.
pub type Result<T> = std::result::Result<T, MevError>;

/// Flashbots bundle submission client.
pub struct FlashbotsClient {
    /// Flashbots relay URL
    relay_url: String,
    /// Signer for bundle authentication
    signer: PrivateKeySigner,
    /// HTTP client
    client: Client,
    /// Chain ID
    #[allow(dead_code)]
    chain_id: u64,
}

impl FlashbotsClient {
    /// Create a new Flashbots client.
    ///
    /// # Arguments
    /// * `relay_url` - The Flashbots relay URL (e.g., "https://relay.flashbots.net")
    /// * `signer` - The wallet for signing bundle submissions
    /// * `chain_id` - The chain ID (1 for mainnet, 5 for goerli)
    pub fn new(relay_url: String, signer: PrivateKeySigner, chain_id: u64) -> Self {
        Self {
            relay_url,
            signer,
            client: Client::new(),
            chain_id,
        }
    }

    /// Create a Flashbots client for Ethereum mainnet.
    pub fn mainnet(signer: PrivateKeySigner) -> Self {
        Self::new(
            "https://relay.flashbots.net".to_string(),
            signer,
            1,
        )
    }

    /// Create a Flashbots client for Goerli testnet.
    pub fn goerli(signer: PrivateKeySigner) -> Self {
        Self::new(
            "https://relay-goerli.flashbots.net".to_string(),
            signer,
            5,
        )
    }

    /// Create a Flashbots client for Sepolia testnet.
    pub fn sepolia(signer: PrivateKeySigner) -> Self {
        Self::new(
            "https://relay-sepolia.flashbots.net".to_string(),
            signer,
            11155111,
        )
    }

    /// Get the signer's address.
    pub fn signer_address(&self) -> Address {
        self.signer.address()
    }

    /// Sign a message for Flashbots authentication using EIP-191 personal_sign format.
    ///
    /// Flashbots expects the signature to use EIP-191 format:
    /// `\x19Ethereum Signed Message:\n<length><message>`
    async fn sign_payload(&self, payload: &str) -> Result<String> {
        // EIP-191: Prefix the message with the standard Ethereum signed message prefix
        let prefixed_message = format!(
            "\x19Ethereum Signed Message:\n{}{}",
            payload.len(),
            payload
        );
        let message_hash = keccak256(prefixed_message.as_bytes());
        let signature = self.signer.sign_hash(&message_hash).await
            .map_err(|e| MevError::Execution(ExecutionError::SignerError(e.to_string())))?;

        Ok(format!(
            "{}:0x{}",
            self.signer.address(),
            hex::encode(signature.as_bytes())
        ))
    }

    /// Make an authenticated JSON-RPC request to Flashbots.
    async fn rpc_request<T: Serialize, R: for<'de> Deserialize<'de>>(
        &self,
        method: &str,
        params: T,
    ) -> Result<R> {
        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id: 1,
            method: method.to_string(),
            params,
        };

        let body = serde_json::to_string(&request)
            .map_err(|e| MevError::Execution(ExecutionError::SubmissionFailed(e.to_string())))?;

        let signature = self.sign_payload(&body).await?;

        debug!(method = method, "Sending Flashbots request");

        let response = self
            .client
            .post(&self.relay_url)
            .header("Content-Type", "application/json")
            .header("X-Flashbots-Signature", signature)
            .body(body)
            .send()
            .await
            .map_err(|e| MevError::Execution(ExecutionError::SubmissionFailed(e.to_string())))?;

        let status = response.status();
        let response_text = response.text().await
            .map_err(|e| MevError::Execution(ExecutionError::SubmissionFailed(e.to_string())))?;

        debug!(status = %status, "Flashbots response received");

        if !status.is_success() {
            error!(status = %status, body = %response_text, "Flashbots request failed");
            return Err(MevError::Execution(ExecutionError::FlashbotsError(
                format!("HTTP {}: {}", status, response_text),
            )));
        }

        let rpc_response: JsonRpcResponse<R> = serde_json::from_str(&response_text)
            .map_err(|e| MevError::Execution(ExecutionError::SubmissionFailed(
                format!("Failed to parse response: {} - {}", e, response_text),
            )))?;

        if let Some(error) = rpc_response.error {
            return Err(MevError::Execution(ExecutionError::FlashbotsError(
                format!("RPC error {}: {}", error.code, error.message),
            )));
        }

        rpc_response.result.ok_or_else(|| {
            MevError::Execution(ExecutionError::FlashbotsError(
                "Empty result from Flashbots".to_string(),
            ))
        })
    }

    /// Send a bundle to Flashbots for inclusion with retry logic.
    ///
    /// # Arguments
    /// * `bundle` - The transaction bundle to submit
    ///
    /// # Returns
    /// The bundle response containing the bundle hash
    #[instrument(skip(self, bundle), fields(block_number = bundle.block_number))]
    pub async fn send_bundle(&self, bundle: FlashbotsBundle) -> Result<BundleResponse> {
        self.send_bundle_with_retries(bundle, 3).await
    }

    /// Send a bundle with configurable retry count.
    async fn send_bundle_with_retries(
        &self,
        bundle: FlashbotsBundle,
        max_retries: u8,
    ) -> Result<BundleResponse> {
        let mut last_error = None;

        for attempt in 0..=max_retries {
            match self.send_bundle_internal(&bundle).await {
                Ok(response) => return Ok(response),
                Err(e) => {
                    debug!(
                        attempt = attempt,
                        max_retries = max_retries,
                        error = %e,
                        "Bundle submission attempt failed"
                    );
                    last_error = Some(e);
                    if attempt < max_retries {
                        // Exponential backoff: 100ms, 200ms, 400ms, etc.
                        let delay_ms = 100 * (1 << attempt);
                        tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                    }
                }
            }
        }

        Err(last_error.unwrap_or_else(|| {
            MevError::Execution(ExecutionError::BundleFailed(
                "All retry attempts exhausted".to_string(),
            ))
        }))
    }

    /// Internal bundle submission without retries.
    async fn send_bundle_internal(&self, bundle: &FlashbotsBundle) -> Result<BundleResponse> {
        let txs: Vec<String> = bundle
            .txs
            .iter()
            .map(|tx| format!("0x{}", hex::encode(tx)))
            .collect();

        let params = SendBundleParams {
            txs,
            block_number: format!("0x{:x}", bundle.block_number),
            min_timestamp: bundle.min_timestamp,
            max_timestamp: bundle.max_timestamp,
            revert_on_fail: Some(bundle.revert_on_fail),
            replacement_uuid: bundle.replacement_uuid.clone(),
        };

        info!(
            block_number = bundle.block_number,
            tx_count = bundle.txs.len(),
            "Submitting bundle to Flashbots"
        );

        let response: SendBundleResponse = self
            .rpc_request("eth_sendBundle", vec![params])
            .await?;

        let bundle_hash = B256::from_slice(
            &hex::decode(response.bundle_hash.trim_start_matches("0x"))
                .map_err(|e| MevError::Execution(ExecutionError::FlashbotsError(e.to_string())))?,
        );

        info!(bundle_hash = %bundle_hash, "Bundle submitted successfully");

        Ok(BundleResponse { bundle_hash })
    }

    /// Simulate a bundle without submitting.
    ///
    /// # Arguments
    /// * `bundle` - The transaction bundle to simulate
    ///
    /// # Returns
    /// The simulation response with gas usage and potential errors
    #[instrument(skip(self, bundle), fields(block_number = bundle.block_number))]
    pub async fn simulate_bundle(&self, bundle: FlashbotsBundle) -> Result<SimulationResponse> {
        let txs: Vec<String> = bundle
            .txs
            .iter()
            .map(|tx| format!("0x{}", hex::encode(tx)))
            .collect();

        let params = CallBundleParams {
            txs,
            block_number: format!("0x{:x}", bundle.block_number),
            state_block_number: "latest".to_string(),
            timestamp: bundle.min_timestamp,
        };

        debug!(
            block_number = bundle.block_number,
            tx_count = bundle.txs.len(),
            "Simulating bundle"
        );

        let response: CallBundleResponse = self
            .rpc_request("eth_callBundle", vec![params])
            .await?;

        let total_gas_used: u64 = response
            .results
            .iter()
            .map(|r| {
                u64::from_str_radix(r.gas_used.trim_start_matches("0x"), 16).unwrap_or(0)
            })
            .sum();

        let coinbase_diff = U256::from_str_radix(
            response.coinbase_diff.trim_start_matches("0x"),
            16,
        )
        .unwrap_or(U256::ZERO);

        let has_error = response.results.iter().any(|r| r.error.is_some());

        debug!(
            total_gas_used = total_gas_used,
            coinbase_diff = %coinbase_diff,
            success = !has_error,
            "Bundle simulation complete"
        );

        Ok(SimulationResponse {
            success: !has_error,
            total_gas_used,
            coinbase_diff,
            gas_price: U256::from_str_radix(
                response.gas_price.trim_start_matches("0x"),
                16,
            )
            .unwrap_or(U256::ZERO),
            results: response
                .results
                .into_iter()
                .map(|r| TxSimulationResult {
                    tx_hash: B256::from_slice(
                        &hex::decode(r.tx_hash.trim_start_matches("0x")).unwrap_or_default(),
                    ),
                    gas_used: u64::from_str_radix(r.gas_used.trim_start_matches("0x"), 16)
                        .unwrap_or(0),
                    gas_price: U256::from_str_radix(
                        r.gas_price.trim_start_matches("0x"),
                        16,
                    )
                    .unwrap_or(U256::ZERO),
                    coinbase_diff: U256::from_str_radix(
                        r.coinbase_diff.trim_start_matches("0x"),
                        16,
                    )
                    .unwrap_or(U256::ZERO),
                    error: r.error,
                    revert: r.revert,
                })
                .collect(),
            error: None,
        })
    }

    /// Get statistics for a submitted bundle.
    ///
    /// # Arguments
    /// * `bundle_hash` - The hash of the bundle to query
    ///
    /// # Returns
    /// Statistics about the bundle's inclusion attempts
    #[instrument(skip(self), fields(bundle_hash = %bundle_hash))]
    pub async fn get_bundle_stats(&self, bundle_hash: B256) -> Result<BundleStats> {
        let params = GetBundleStatsParams {
            bundle_hash: format!("0x{}", hex::encode(bundle_hash)),
            block_number: "latest".to_string(),
        };

        debug!("Fetching bundle stats");

        let response: GetBundleStatsResponse = self
            .rpc_request("flashbots_getBundleStats", vec![params])
            .await?;

        Ok(BundleStats {
            is_simulated: response.is_simulated,
            is_sent_to_miners: response.is_sent_to_miners.unwrap_or(false),
            is_high_priority: response.is_high_priority.unwrap_or(false),
            simulated_at: response.simulated_at,
            submitted_at: response.submitted_at,
            sent_to_miners_at: response.sent_to_miners_at,
        })
    }

    /// Get the user's Flashbots stats (reputation, etc.).
    #[instrument(skip(self))]
    pub async fn get_user_stats(&self) -> Result<UserStats> {
        let params = GetUserStatsParams {
            block_number: "latest".to_string(),
        };

        let response: GetUserStatsResponse = self
            .rpc_request("flashbots_getUserStats", vec![params])
            .await?;

        Ok(UserStats {
            is_high_priority: response.is_high_priority,
            all_time_miner_payments: U256::from_str_radix(
                response.all_time_miner_payments.trim_start_matches("0x"),
                16,
            )
            .unwrap_or(U256::ZERO),
            all_time_gas_simulated: U256::from_str_radix(
                response.all_time_gas_simulated.trim_start_matches("0x"),
                16,
            )
            .unwrap_or(U256::ZERO),
            last_7d_miner_payments: U256::from_str_radix(
                response.last_7d_miner_payments.trim_start_matches("0x"),
                16,
            )
            .unwrap_or(U256::ZERO),
            last_7d_gas_simulated: U256::from_str_radix(
                response.last_7d_gas_simulated.trim_start_matches("0x"),
                16,
            )
            .unwrap_or(U256::ZERO),
            last_1d_miner_payments: U256::from_str_radix(
                response.last_1d_miner_payments.trim_start_matches("0x"),
                16,
            )
            .unwrap_or(U256::ZERO),
            last_1d_gas_simulated: U256::from_str_radix(
                response.last_1d_gas_simulated.trim_start_matches("0x"),
                16,
            )
            .unwrap_or(U256::ZERO),
        })
    }

    /// Cancel a pending bundle by its replacement UUID.
    #[instrument(skip(self))]
    pub async fn cancel_bundle(&self, replacement_uuid: &str) -> Result<bool> {
        let params = CancelBundleParams {
            replacement_uuid: replacement_uuid.to_string(),
        };

        let _response: CancelBundleResponse = self
            .rpc_request("eth_cancelBundle", vec![params])
            .await?;

        info!(replacement_uuid = replacement_uuid, "Bundle cancelled");

        Ok(true)
    }

    /// Send a private transaction (not in a bundle).
    #[instrument(skip(self, tx))]
    pub async fn send_private_transaction(
        &self,
        tx: Bytes,
        max_block_number: Option<u64>,
    ) -> Result<B256> {
        let params = SendPrivateTransactionParams {
            tx: format!("0x{}", hex::encode(&tx)),
            max_block_number: max_block_number.map(|n| format!("0x{:x}", n)),
            preferences: None,
        };

        let response: SendPrivateTransactionResponse = self
            .rpc_request("eth_sendPrivateTransaction", vec![params])
            .await?;

        let tx_hash = B256::from_slice(
            &hex::decode(response.tx_hash.trim_start_matches("0x"))
                .map_err(|e| MevError::Execution(ExecutionError::FlashbotsError(e.to_string())))?,
        );

        info!(tx_hash = %tx_hash, "Private transaction submitted");

        Ok(tx_hash)
    }
}

/// A bundle of transactions to submit to Flashbots.
#[derive(Debug, Clone)]
pub struct FlashbotsBundle {
    /// Signed, RLP-encoded transactions
    pub txs: Vec<Bytes>,
    /// Transaction hashes to include in the bundle (for referencing mempool txs)
    /// These are used for sandwich/backrun bundles to reference target transactions
    pub tx_hashes: Vec<B256>,
    /// Target block number for inclusion
    pub block_number: u64,
    /// Minimum timestamp for bundle validity
    pub min_timestamp: Option<u64>,
    /// Maximum timestamp for bundle validity
    pub max_timestamp: Option<u64>,
    /// Whether to revert the entire bundle if any tx fails
    pub revert_on_fail: bool,
    /// UUID for bundle replacement/cancellation
    pub replacement_uuid: Option<String>,
}

impl FlashbotsBundle {
    /// Create a new bundle for a specific block.
    pub fn new(txs: Vec<Bytes>, block_number: u64) -> Self {
        Self {
            txs,
            tx_hashes: Vec::new(),
            block_number,
            min_timestamp: None,
            max_timestamp: None,
            revert_on_fail: true,
            replacement_uuid: None,
        }
    }

    /// Create a bundle that references a mempool transaction by hash.
    /// This is used for backrun bundles where we want to execute after a target tx.
    pub fn with_target_tx_hash(txs: Vec<Bytes>, target_tx_hash: B256, block_number: u64) -> Self {
        Self {
            txs,
            tx_hashes: vec![target_tx_hash],
            block_number,
            min_timestamp: None,
            max_timestamp: None,
            revert_on_fail: true,
            replacement_uuid: None,
        }
    }

    /// Create a sandwich bundle with frontrun, victim hash, and backrun.
    /// The bundle order will be: [frontrun_tx, victim_tx (by hash), backrun_tx]
    pub fn sandwich(
        frontrun_tx: Bytes,
        victim_tx_hash: B256,
        backrun_tx: Bytes,
        block_number: u64,
    ) -> Self {
        Self {
            txs: vec![frontrun_tx, backrun_tx],
            tx_hashes: vec![victim_tx_hash],
            block_number,
            min_timestamp: None,
            max_timestamp: None,
            revert_on_fail: true,
            replacement_uuid: None,
        }
    }

    /// Add a transaction hash reference (for mempool transactions).
    pub fn with_tx_hash(mut self, tx_hash: B256) -> Self {
        self.tx_hashes.push(tx_hash);
        self
    }

    /// Set timestamp constraints for the bundle.
    pub fn with_timestamp_range(mut self, min: u64, max: u64) -> Self {
        self.min_timestamp = Some(min);
        self.max_timestamp = Some(max);
        self
    }

    /// Set whether to revert on any transaction failure.
    pub fn with_revert_on_fail(mut self, revert: bool) -> Self {
        self.revert_on_fail = revert;
        self
    }

    /// Set a replacement UUID for bundle updates/cancellation.
    pub fn with_replacement_uuid(mut self, uuid: String) -> Self {
        self.replacement_uuid = Some(uuid);
        self
    }

    /// Calculate a local identifier for this bundle.
    /// Note: This is NOT the same as the bundle hash returned by Flashbots relay.
    /// The actual bundle hash is computed by the relay and returned in the response.
    pub fn local_id(&self) -> B256 {
        let mut data = Vec::new();
        for tx in &self.txs {
            data.extend_from_slice(tx);
        }
        for hash in &self.tx_hashes {
            data.extend_from_slice(hash.as_slice());
        }
        keccak256(&data)
    }

    /// Check if this bundle references any mempool transactions.
    pub fn has_tx_hash_references(&self) -> bool {
        !self.tx_hashes.is_empty()
    }
}

/// Response from bundle submission.
#[derive(Debug, Clone)]
pub struct BundleResponse {
    /// Hash identifying the bundle
    pub bundle_hash: B256,
}

/// Response from bundle simulation.
#[derive(Debug, Clone)]
pub struct SimulationResponse {
    /// Whether the simulation succeeded
    pub success: bool,
    /// Total gas used by all transactions
    pub total_gas_used: u64,
    /// ETH transferred to coinbase (block.coinbase)
    pub coinbase_diff: U256,
    /// Effective gas price
    pub gas_price: U256,
    /// Individual transaction results
    pub results: Vec<TxSimulationResult>,
    /// Error message if simulation failed
    pub error: Option<String>,
}

/// Result of simulating a single transaction.
#[derive(Debug, Clone)]
pub struct TxSimulationResult {
    /// Transaction hash
    pub tx_hash: B256,
    /// Gas used
    pub gas_used: u64,
    /// Gas price
    pub gas_price: U256,
    /// Coinbase diff from this tx
    pub coinbase_diff: U256,
    /// Error if tx failed
    pub error: Option<String>,
    /// Revert reason if applicable
    pub revert: Option<String>,
}

/// Statistics about a submitted bundle.
#[derive(Debug, Clone)]
pub struct BundleStats {
    /// Whether the bundle was simulated
    pub is_simulated: bool,
    /// Whether the bundle was sent to miners/validators
    pub is_sent_to_miners: bool,
    /// Whether the sender has high priority status
    pub is_high_priority: bool,
    /// When the bundle was simulated
    pub simulated_at: Option<String>,
    /// When the bundle was submitted
    pub submitted_at: Option<String>,
    /// When the bundle was sent to miners
    pub sent_to_miners_at: Option<String>,
}

/// User statistics from Flashbots.
#[derive(Debug, Clone)]
pub struct UserStats {
    /// Whether the user has high priority status
    pub is_high_priority: bool,
    /// All-time payments to miners/validators
    pub all_time_miner_payments: U256,
    /// All-time gas simulated
    pub all_time_gas_simulated: U256,
    /// Last 7 days miner payments
    pub last_7d_miner_payments: U256,
    /// Last 7 days gas simulated
    pub last_7d_gas_simulated: U256,
    /// Last 1 day miner payments
    pub last_1d_miner_payments: U256,
    /// Last 1 day gas simulated
    pub last_1d_gas_simulated: U256,
}

// JSON-RPC request/response types

#[derive(Serialize)]
struct JsonRpcRequest<T> {
    jsonrpc: String,
    id: u64,
    method: String,
    params: T,
}

#[derive(Deserialize)]
struct JsonRpcResponse<T> {
    #[allow(dead_code)]
    jsonrpc: String,
    #[allow(dead_code)]
    id: u64,
    result: Option<T>,
    error: Option<JsonRpcError>,
}

#[derive(Deserialize)]
struct JsonRpcError {
    code: i64,
    message: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SendBundleParams {
    txs: Vec<String>,
    block_number: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    min_timestamp: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_timestamp: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    revert_on_fail: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    replacement_uuid: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SendBundleResponse {
    bundle_hash: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CallBundleParams {
    txs: Vec<String>,
    block_number: String,
    state_block_number: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    timestamp: Option<u64>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CallBundleResponse {
    #[allow(dead_code)]
    bundle_gas_price: String,
    #[allow(dead_code)]
    bundle_hash: String,
    coinbase_diff: String,
    #[allow(dead_code)]
    eth_sent_to_coinbase: String,
    gas_price: String,
    results: Vec<CallBundleTxResult>,
    #[allow(dead_code)]
    state_block_number: u64,
    #[allow(dead_code)]
    total_gas_used: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CallBundleTxResult {
    coinbase_diff: String,
    #[allow(dead_code)]
    eth_sent_to_coinbase: String,
    #[allow(dead_code)]
    from_address: String,
    #[allow(dead_code)]
    gas_fees: String,
    gas_price: String,
    gas_used: String,
    #[allow(dead_code)]
    to_address: String,
    tx_hash: String,
    #[allow(dead_code)]
    value: Option<String>,
    error: Option<String>,
    revert: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GetBundleStatsParams {
    bundle_hash: String,
    block_number: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GetBundleStatsResponse {
    is_simulated: bool,
    is_sent_to_miners: Option<bool>,
    is_high_priority: Option<bool>,
    simulated_at: Option<String>,
    submitted_at: Option<String>,
    sent_to_miners_at: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct GetUserStatsParams {
    block_number: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct GetUserStatsResponse {
    is_high_priority: bool,
    all_time_miner_payments: String,
    all_time_gas_simulated: String,
    last_7d_miner_payments: String,
    last_7d_gas_simulated: String,
    last_1d_miner_payments: String,
    last_1d_gas_simulated: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct CancelBundleParams {
    replacement_uuid: String,
}

#[derive(Deserialize)]
struct CancelBundleResponse {}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct SendPrivateTransactionParams {
    tx: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_block_number: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    preferences: Option<PrivateTxPreferences>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PrivateTxPreferences {
    fast: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SendPrivateTransactionResponse {
    tx_hash: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_flashbots_bundle_creation() {
        let tx1 = Bytes::from(vec![1, 2, 3]);
        let tx2 = Bytes::from(vec![4, 5, 6]);

        let bundle = FlashbotsBundle::new(vec![tx1, tx2], 12345678);

        assert_eq!(bundle.block_number, 12345678);
        assert_eq!(bundle.txs.len(), 2);
        assert!(bundle.revert_on_fail);
        assert!(bundle.min_timestamp.is_none());
    }

    #[test]
    fn test_flashbots_bundle_with_options() {
        let tx = Bytes::from(vec![1, 2, 3]);

        let bundle = FlashbotsBundle::new(vec![tx], 12345678)
            .with_timestamp_range(1000, 2000)
            .with_revert_on_fail(false);

        assert_eq!(bundle.min_timestamp, Some(1000));
        assert_eq!(bundle.max_timestamp, Some(2000));
        assert!(!bundle.revert_on_fail);
    }

    #[test]
    fn test_bundle_local_id() {
        let tx1 = Bytes::from(vec![1, 2, 3]);
        let tx2 = Bytes::from(vec![4, 5, 6]);

        let bundle = FlashbotsBundle::new(vec![tx1.clone(), tx2.clone()], 12345678);
        let hash1 = bundle.local_id();

        // Same transactions should produce same local id
        let bundle2 = FlashbotsBundle::new(vec![tx1, tx2], 99999999);
        let hash2 = bundle2.local_id();

        assert_eq!(hash1, hash2);
    }

    #[test]
    fn test_sandwich_bundle() {
        let frontrun = Bytes::from(vec![1, 2, 3]);
        let victim_hash = B256::repeat_byte(0xAB);
        let backrun = Bytes::from(vec![4, 5, 6]);

        let bundle = FlashbotsBundle::sandwich(frontrun.clone(), victim_hash, backrun.clone(), 12345);

        assert_eq!(bundle.txs.len(), 2);
        assert_eq!(bundle.tx_hashes.len(), 1);
        assert_eq!(bundle.tx_hashes[0], victim_hash);
        assert!(bundle.has_tx_hash_references());
    }

    #[test]
    fn test_backrun_bundle() {
        let backrun_tx = Bytes::from(vec![1, 2, 3]);
        let target_hash = B256::repeat_byte(0xCD);

        let bundle = FlashbotsBundle::with_target_tx_hash(vec![backrun_tx], target_hash, 12345);

        assert_eq!(bundle.txs.len(), 1);
        assert_eq!(bundle.tx_hashes.len(), 1);
        assert_eq!(bundle.tx_hashes[0], target_hash);
    }
}
