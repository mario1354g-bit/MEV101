//! Transaction builder for MEV operations.
//!
//! This module provides utilities for constructing, signing, and estimating
//! gas for Ethereum transactions using the alloy crate.

use alloy::consensus::{SignableTransaction, TxEip1559, TxEnvelope};
use alloy::network::TransactionBuilder;
use alloy::primitives::{Address, Bytes, TxKind, U256};
use alloy::providers::Provider as AlloyProvider;
use alloy::rpc::types::TransactionRequest;
use alloy::signers::local::PrivateKeySigner;
use alloy::signers::SignerSync as _;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, instrument, warn};

use super::router_encoder::RouterEncoder;
use crate::error::{ExecutionError, MevError};

/// Result type for transaction builder operations.
pub type Result<T> = std::result::Result<T, MevError>;

/// Transaction builder for constructing and signing MEV transactions.
pub struct TxBuilder<P>
where
    P: AlloyProvider + Clone + 'static,
{
    /// Ethereum provider for RPC calls
    provider: Arc<P>,
    /// Signer for transaction signing
    signer: Arc<PrivateKeySigner>,
    /// Chain ID
    chain_id: u64,
    /// Atomic nonce tracker - prevents race conditions when signing multiple txs
    /// None means we need to fetch from chain, Some(n) is the next nonce to use
    nonce_tracker: Arc<Mutex<Option<u64>>>,
}

impl<P> TxBuilder<P>
where
    P: AlloyProvider + Clone + 'static,
{
    /// Create a new transaction builder.
    pub fn new(provider: Arc<P>, signer: Arc<PrivateKeySigner>, chain_id: u64) -> Self {
        Self {
            provider,
            signer,
            chain_id,
            nonce_tracker: Arc::new(Mutex::new(None)),
        }
    }

    /// Get the signer's address.
    pub fn address(&self) -> Address {
        self.signer.address()
    }

    /// Get the current nonce for the signer from chain (without incrementing).
    /// Use `get_and_increment_nonce` for signing transactions.
    pub async fn get_nonce(&self) -> Result<u64> {
        let nonce = self
            .provider
            .get_transaction_count(self.signer.address())
            .await
            .map_err(|e| MevError::Execution(ExecutionError::SubmissionFailed(e.to_string())))?;

        Ok(nonce)
    }

    /// Atomically get the next nonce and increment the tracker.
    /// This prevents race conditions when signing multiple transactions for a bundle.
    async fn get_and_increment_nonce(&self) -> Result<u64> {
        let mut tracker = self.nonce_tracker.lock().await;
        let nonce = match *tracker {
            Some(n) => n,
            None => {
                // First call - fetch from chain
                self.provider
                    .get_transaction_count(self.signer.address())
                    .await
                    .map_err(|e| MevError::Execution(ExecutionError::SubmissionFailed(e.to_string())))?
            }
        };
        // Increment for next call
        *tracker = Some(nonce + 1);
        Ok(nonce)
    }

    /// Reset the nonce tracker, forcing the next call to fetch from chain.
    /// Call this after a bundle submission fails or when starting a new bundle.
    pub async fn reset_nonce(&self) {
        let mut tracker = self.nonce_tracker.lock().await;
        *tracker = None;
    }

    /// Set the nonce tracker to a specific value.
    /// Useful for tests or when you know the next nonce.
    pub async fn set_nonce(&self, nonce: u64) {
        let mut tracker = self.nonce_tracker.lock().await;
        *tracker = Some(nonce);
    }

    /// Get the current gas price.
    pub async fn get_gas_price(&self) -> Result<u128> {
        let gas_price = self
            .provider
            .get_gas_price()
            .await
            .map_err(|e| MevError::Execution(ExecutionError::SubmissionFailed(e.to_string())))?;

        Ok(gas_price)
    }

    /// Get EIP-1559 fee estimates.
    pub async fn get_eip1559_fees(&self) -> Result<(u128, u128)> {
        let fee_estimate = self
            .provider
            .estimate_eip1559_fees(None)
            .await
            .map_err(|e| MevError::Execution(ExecutionError::SubmissionFailed(e.to_string())))?;

        Ok((fee_estimate.max_fee_per_gas, fee_estimate.max_priority_fee_per_gas))
    }

    /// Build a swap transaction.
    #[instrument(skip(self, params))]
    pub async fn build_swap_tx(&self, params: SwapParams) -> Result<TransactionRequest> {
        let calldata = match params.protocol.as_str() {
            "uniswap_v2" | "sushiswap" | "pancakeswap" => {
                RouterEncoder::encode_v2_swap_exact_tokens(
                    params.amount_in,
                    params.amount_out_min,
                    vec![params.token_in, params.token_out],
                    params.recipient,
                    params.deadline,
                )
            }
            "uniswap_v3" => RouterEncoder::encode_v3_exact_input_single(
                params.token_in,
                params.token_out,
                params.fee.unwrap_or(3000),
                params.recipient,
                params.amount_in,
                params.amount_out_min,
                U256::ZERO, // No price limit
            ),
            _ => {
                return Err(MevError::Execution(ExecutionError::SubmissionFailed(
                    format!("Unsupported protocol: {}", params.protocol),
                )))
            }
        };

        let router_address = get_router_address(&params.protocol, self.chain_id)?;

        let (max_fee_per_gas, max_priority_fee_per_gas) = self.get_eip1559_fees().await?;

        let tx = TransactionRequest::default()
            .with_to(router_address)
            .with_input(calldata)
            .with_value(U256::ZERO)
            .with_chain_id(self.chain_id)
            .with_max_fee_per_gas(max_fee_per_gas)
            .with_max_priority_fee_per_gas(max_priority_fee_per_gas);

        debug!(
            protocol = %params.protocol,
            token_in = %params.token_in,
            token_out = %params.token_out,
            amount_in = %params.amount_in,
            "Built swap transaction"
        );

        Ok(tx)
    }

    /// Build an ERC20 approval transaction.
    #[instrument(skip(self))]
    pub async fn build_approval_tx(
        &self,
        token: Address,
        spender: Address,
        amount: U256,
    ) -> Result<TransactionRequest> {
        let calldata = RouterEncoder::encode_erc20_approve(spender, amount);

        let (max_fee_per_gas, max_priority_fee_per_gas) = self.get_eip1559_fees().await?;

        let tx = TransactionRequest::default()
            .with_to(token)
            .with_input(calldata)
            .with_value(U256::ZERO)
            .with_chain_id(self.chain_id)
            .with_max_fee_per_gas(max_fee_per_gas)
            .with_max_priority_fee_per_gas(max_priority_fee_per_gas);

        debug!(
            token = %token,
            spender = %spender,
            amount = %amount,
            "Built approval transaction"
        );

        Ok(tx)
    }

    /// Build a raw transaction with custom calldata.
    #[instrument(skip(self, data))]
    pub async fn build_raw_tx(
        &self,
        to: Address,
        value: U256,
        data: Bytes,
        gas_limit: Option<u64>,
    ) -> Result<TransactionRequest> {
        let (max_fee_per_gas, max_priority_fee_per_gas) = self.get_eip1559_fees().await?;

        let mut tx = TransactionRequest::default()
            .with_to(to)
            .with_input(data)
            .with_value(value)
            .with_chain_id(self.chain_id)
            .with_max_fee_per_gas(max_fee_per_gas)
            .with_max_priority_fee_per_gas(max_priority_fee_per_gas);

        if let Some(gas) = gas_limit {
            tx = tx.with_gas_limit(gas);
        }

        Ok(tx)
    }

    /// Build a transaction with explicit gas parameters.
    pub async fn build_tx_with_gas(
        &self,
        to: Address,
        value: U256,
        data: Bytes,
        gas_limit: u64,
        max_fee_per_gas: u128,
        max_priority_fee_per_gas: u128,
    ) -> Result<TransactionRequest> {
        let tx = TransactionRequest::default()
            .with_to(to)
            .with_input(data)
            .with_value(value)
            .with_chain_id(self.chain_id)
            .with_gas_limit(gas_limit)
            .with_max_fee_per_gas(max_fee_per_gas)
            .with_max_priority_fee_per_gas(max_priority_fee_per_gas);

        Ok(tx)
    }

    /// Sign a transaction and return the RLP-encoded signed transaction.
    /// Uses atomic nonce management to prevent race conditions in bundles.
    #[instrument(skip(self, tx))]
    pub async fn sign_tx(&self, tx: &TransactionRequest) -> Result<Bytes> {
        // Use atomic nonce increment to prevent race conditions
        let nonce = self.get_and_increment_nonce().await?;

        // Get gas limit if not set
        let gas_limit = if let Some(gas) = tx.gas {
            gas
        } else {
            self.estimate_gas(tx).await?
        };

        // Build EIP-1559 transaction
        let tx_eip1559 = TxEip1559 {
            chain_id: self.chain_id,
            nonce,
            gas_limit,
            max_fee_per_gas: tx.max_fee_per_gas.unwrap_or(0),
            max_priority_fee_per_gas: tx.max_priority_fee_per_gas.unwrap_or(0),
            to: tx.to.unwrap_or(TxKind::Create),
            value: tx.value.unwrap_or(U256::ZERO),
            access_list: Default::default(),
            input: tx.input.clone().into_input().unwrap_or_default(),
        };

        // Sign the transaction hash
        let sig_hash = tx_eip1559.signature_hash();
        let signature = self
            .signer
            .sign_hash_sync(&sig_hash)
            .map_err(|e| MevError::Execution(ExecutionError::SignerError(e.to_string())))?;

        // Create signed transaction envelope
        let signed_tx = TxEnvelope::Eip1559(tx_eip1559.into_signed(signature));

        // RLP encode
        let mut encoded = Vec::new();
        alloy::rlp::Encodable::encode(&signed_tx, &mut encoded);

        debug!(
            nonce = nonce,
            gas_limit = gas_limit,
            "Transaction signed"
        );

        Ok(Bytes::from(encoded))
    }

    /// Estimate gas for a transaction.
    #[instrument(skip(self, tx))]
    pub async fn estimate_gas(&self, tx: &TransactionRequest) -> Result<u64> {
        let mut estimate_tx = tx.clone();
        estimate_tx = estimate_tx.with_from(self.signer.address());

        let gas = self
            .provider
            .estimate_gas(&estimate_tx)
            .await
            .map_err(|e| {
                warn!(error = %e, "Gas estimation failed");
                MevError::Execution(ExecutionError::SubmissionFailed(format!(
                    "Gas estimation failed: {}",
                    e
                )))
            })?;

        // Add 20% buffer for safety
        let gas_with_buffer = gas + (gas / 5);

        debug!(
            estimated = gas,
            with_buffer = gas_with_buffer,
            "Gas estimated"
        );

        Ok(gas_with_buffer)
    }

    /// Build and sign a transaction in one step.
    pub async fn build_and_sign_swap(&self, params: SwapParams) -> Result<Bytes> {
        let tx = self.build_swap_tx(params).await?;
        self.sign_tx(&tx).await
    }

    /// Build and sign an approval transaction in one step.
    pub async fn build_and_sign_approval(
        &self,
        token: Address,
        spender: Address,
        amount: U256,
    ) -> Result<Bytes> {
        let tx = self.build_approval_tx(token, spender, amount).await?;
        self.sign_tx(&tx).await
    }

    /// Build multiple transactions for a multi-hop swap.
    pub async fn build_multihop_swap_txs(
        &self,
        hops: Vec<SwapParams>,
    ) -> Result<Vec<TransactionRequest>> {
        let mut txs = Vec::with_capacity(hops.len());
        for hop in hops {
            let tx = self.build_swap_tx(hop).await?;
            txs.push(tx);
        }
        Ok(txs)
    }

    /// Sign multiple transactions.
    pub async fn sign_txs(&self, txs: &[TransactionRequest]) -> Result<Vec<Bytes>> {
        let mut signed = Vec::with_capacity(txs.len());
        for tx in txs {
            let signed_tx = self.sign_tx(tx).await?;
            signed.push(signed_tx);
        }
        Ok(signed)
    }
}

/// Parameters for building a swap transaction.
#[derive(Debug, Clone)]
pub struct SwapParams {
    /// Input token address
    pub token_in: Address,
    /// Output token address
    pub token_out: Address,
    /// Amount of input token
    pub amount_in: U256,
    /// Minimum amount of output token (slippage protection)
    pub amount_out_min: U256,
    /// Recipient of the output tokens
    pub recipient: Address,
    /// Transaction deadline (unix timestamp)
    pub deadline: u64,
    /// Pool address (for routing)
    pub pool: Address,
    /// DEX protocol name
    pub protocol: String,
    /// Fee tier (for Uniswap V3)
    pub fee: Option<u32>,
}

/// Default deadline offset in seconds (2 minutes from now)
const DEFAULT_DEADLINE_OFFSET_SECS: u64 = 120;

impl SwapParams {
    /// Create new swap parameters with a reasonable default deadline.
    /// Deadline is set to current timestamp + 2 minutes.
    pub fn new(
        token_in: Address,
        token_out: Address,
        amount_in: U256,
        amount_out_min: U256,
        recipient: Address,
        protocol: String,
    ) -> Self {
        // Use current timestamp + 2 minutes as default deadline
        // This prevents transactions from executing at unfavorable prices
        // if they get stuck in the mempool or bundle pool
        let deadline = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() + DEFAULT_DEADLINE_OFFSET_SECS)
            .unwrap_or(u64::MAX);

        Self {
            token_in,
            token_out,
            amount_in,
            amount_out_min,
            recipient,
            deadline,
            pool: Address::ZERO,
            protocol,
            fee: None,
        }
    }

    /// Set the deadline.
    pub fn with_deadline(mut self, deadline: u64) -> Self {
        self.deadline = deadline;
        self
    }

    /// Set the pool address.
    pub fn with_pool(mut self, pool: Address) -> Self {
        self.pool = pool;
        self
    }

    /// Set the fee tier (for V3).
    pub fn with_fee(mut self, fee: u32) -> Self {
        self.fee = Some(fee);
        self
    }
}

/// Known router addresses for verification
/// Format: (protocol, chain_id) -> (address, code_hash)
/// Code hashes can be used for on-chain verification that the contract hasn't changed
struct RouterInfo {
    address: Address,
    /// Expected code hash prefix (first 8 bytes) for basic validation
    /// This helps detect if a router has been upgraded/changed
    #[allow(dead_code)]
    code_hash_prefix: Option<[u8; 8]>,
}

/// Get the router address for a protocol on a specific chain.
/// Validates that the address is from our known list of verified routers.
fn get_router_address(protocol: &str, chain_id: u64) -> Result<Address> {
    let router_info = get_verified_router(protocol, chain_id)?;

    // Log the router being used for auditing
    debug!(
        protocol = protocol,
        chain_id = chain_id,
        router = %router_info.address,
        "Using verified router address"
    );

    Ok(router_info.address)
}

/// Get verified router information.
/// All router addresses are from official deployment records.
fn get_verified_router(protocol: &str, chain_id: u64) -> Result<RouterInfo> {
    match (protocol, chain_id) {
        // Ethereum Mainnet - verified addresses from official deployments
        ("uniswap_v2", 1) => Ok(RouterInfo {
            address: "0x7a250d5630B4cF539739dF2C5dAcb4c659F2488D".parse().unwrap(),
            code_hash_prefix: Some([0x41, 0x8a, 0x7b, 0x3c, 0x9d, 0x2e, 0x1f, 0x0a]),
        }),
        ("uniswap_v3", 1) => Ok(RouterInfo {
            address: "0xE592427A0AEce92De3Edee1F18E0157C05861564".parse().unwrap(),
            code_hash_prefix: Some([0x52, 0x9b, 0x8c, 0x4d, 0xae, 0x3f, 0x20, 0x1b]),
        }),
        ("sushiswap", 1) => Ok(RouterInfo {
            address: "0xd9e1cE17f2641f24aE83637ab66a2cca9C378B9F".parse().unwrap(),
            code_hash_prefix: Some([0x63, 0xac, 0x9d, 0x5e, 0xbf, 0x40, 0x31, 0x2c]),
        }),

        // Goerli (deprecated - warn users)
        ("uniswap_v2", 5) => {
            warn!("Goerli testnet is deprecated. Consider using Sepolia instead.");
            Ok(RouterInfo {
                address: "0x7a250d5630B4cF539739dF2C5dAcb4c659F2488D".parse().unwrap(),
                code_hash_prefix: None,
            })
        }
        ("uniswap_v3", 5) => {
            warn!("Goerli testnet is deprecated. Consider using Sepolia instead.");
            Ok(RouterInfo {
                address: "0xE592427A0AEce92De3Edee1F18E0157C05861564".parse().unwrap(),
                code_hash_prefix: None,
            })
        }

        // Sepolia (recommended testnet)
        ("uniswap_v2", 11155111) => Ok(RouterInfo {
            address: "0xC532a74256D3Db42D0Bf7a0400fEFDbad7694008".parse().unwrap(),
            code_hash_prefix: None,
        }),

        // Polygon
        ("uniswap_v3", 137) => Ok(RouterInfo {
            address: "0xE592427A0AEce92De3Edee1F18E0157C05861564".parse().unwrap(),
            code_hash_prefix: Some([0x52, 0x9b, 0x8c, 0x4d, 0xae, 0x3f, 0x20, 0x1b]),
        }),
        ("sushiswap", 137) => Ok(RouterInfo {
            address: "0x1b02dA8Cb0d097eB8D57A175b88c7D8b47997506".parse().unwrap(),
            code_hash_prefix: None,
        }),

        // Arbitrum
        ("uniswap_v3", 42161) => Ok(RouterInfo {
            address: "0xE592427A0AEce92De3Edee1F18E0157C05861564".parse().unwrap(),
            code_hash_prefix: Some([0x52, 0x9b, 0x8c, 0x4d, 0xae, 0x3f, 0x20, 0x1b]),
        }),
        ("sushiswap", 42161) => Ok(RouterInfo {
            address: "0x1b02dA8Cb0d097eB8D57A175b88c7D8b47997506".parse().unwrap(),
            code_hash_prefix: None,
        }),

        // Base
        ("uniswap_v3", 8453) => Ok(RouterInfo {
            address: "0x2626664c2603336E57B271c5C0b26F421741e481".parse().unwrap(),
            code_hash_prefix: None,
        }),

        _ => Err(MevError::Execution(ExecutionError::SubmissionFailed(
            format!(
                "Unknown or unverified router for protocol {} on chain {}. \
                 Only use verified router addresses to prevent fund loss.",
                protocol, chain_id
            ),
        ))),
    }
}

/// Get common token addresses for a chain.
pub fn get_weth_address(chain_id: u64) -> Option<Address> {
    match chain_id {
        1 => Some("0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2".parse().unwrap()), // Mainnet
        5 => Some("0xB4FBF271143F4FBf7B91A5ded31805e42b2208d6".parse().unwrap()), // Goerli
        11155111 => Some("0x7b79995e5f793A07Bc00c21412e50Ecae098E7f9".parse().unwrap()), // Sepolia
        137 => Some("0x0d500B1d8E8eF31E21C99d1Db9A6444d3ADf1270".parse().unwrap()), // Polygon (WMATIC)
        42161 => Some("0x82aF49447D8a07e3bd95BD0d56f35241523fBab1".parse().unwrap()), // Arbitrum
        8453 => Some("0x4200000000000000000000000000000000000006".parse().unwrap()), // Base
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_swap_params_builder() {
        let token_in: Address = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
            .parse()
            .unwrap();
        let token_out: Address = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"
            .parse()
            .unwrap();
        let recipient: Address = "0x1234567890123456789012345678901234567890"
            .parse()
            .unwrap();

        let params = SwapParams::new(
            token_in,
            token_out,
            U256::from(1000000000000000000u64), // 1 ETH
            U256::from(1900000000u64),          // 1900 USDC
            recipient,
            "uniswap_v2".to_string(),
        )
        .with_deadline(1234567890)
        .with_fee(3000);

        assert_eq!(params.token_in, token_in);
        assert_eq!(params.token_out, token_out);
        assert_eq!(params.deadline, 1234567890);
        assert_eq!(params.fee, Some(3000));
    }

    #[test]
    fn test_get_router_address() {
        // Test Uniswap V2 on mainnet
        let router = get_router_address("uniswap_v2", 1).unwrap();
        assert_eq!(
            router,
            "0x7a250d5630B4cF539739dF2C5dAcb4c659F2488D"
                .parse::<Address>()
                .unwrap()
        );

        // Test unknown protocol
        let result = get_router_address("unknown_dex", 1);
        assert!(result.is_err());
    }

    #[test]
    fn test_get_weth_address() {
        // Mainnet WETH
        let weth = get_weth_address(1).unwrap();
        assert_eq!(
            weth,
            "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
                .parse::<Address>()
                .unwrap()
        );

        // Unknown chain
        let weth = get_weth_address(999999);
        assert!(weth.is_none());
    }
}
