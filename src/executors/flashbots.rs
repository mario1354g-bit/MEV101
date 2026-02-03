//! Flashbots executor - submits bundles to Flashbots relay with revm simulation

use crate::artemis::{
    Action, ArbitrageAction, ExecutionResult, Executor, LiquidationAction, SandwichAction,
};
use crate::simulation::{RevmSimulator, RevmTransaction};
use alloy::consensus::SignableTransaction;
use alloy::eips::eip2718::Encodable2718;
use alloy::network::{EthereumWallet, TransactionBuilder};
use alloy::primitives::{address, Address, B256, Bytes, U256, keccak256};
use alloy::providers::{Provider, ProviderBuilder};
use alloy::rpc::types::TransactionRequest;
use alloy::signers::local::PrivateKeySigner;
use alloy::signers::Signer;
use async_trait::async_trait;
use parking_lot::RwLock;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{debug, error, info, warn};

/// Flashbots RPC endpoints
pub mod endpoints {
    pub const FLASHBOTS_MAINNET: &str = "https://relay.flashbots.net";
    pub const FLASHBOTS_GOERLI: &str = "https://relay-goerli.flashbots.net";
    pub const FLASHBOTS_PROTECT: &str = "https://rpc.flashbots.net";
}

/// Flashbots bundle request
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FlashbotsBundle {
    pub txs: Vec<String>,
    pub block_number: String,
    pub min_timestamp: Option<u64>,
    pub max_timestamp: Option<u64>,
}

/// Flashbots send bundle params
#[derive(Debug, Clone, Serialize)]
pub struct SendBundleParams {
    pub jsonrpc: String,
    pub id: u64,
    pub method: String,
    pub params: Vec<FlashbotsBundle>,
}

/// Flashbots bundle response
#[derive(Debug, Clone, Deserialize)]
pub struct BundleResponse {
    pub jsonrpc: String,
    pub id: u64,
    pub result: Option<BundleResult>,
    pub error: Option<BundleError>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BundleResult {
    pub bundle_hash: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BundleError {
    pub code: i64,
    pub message: String,
}

/// Flashbots executor configuration
#[derive(Debug, Clone)]
pub struct FlashbotsExecutorConfig {
    pub relay_url: String,
    pub rpc_url: String,
    pub signer_key: String,
    pub flashloan_contract: Address,
    pub dry_run: bool,
}

impl Default for FlashbotsExecutorConfig {
    fn default() -> Self {
        Self {
            relay_url: endpoints::FLASHBOTS_MAINNET.to_string(),
            rpc_url: String::new(),
            signer_key: String::new(),
            flashloan_contract: Address::ZERO,
            dry_run: true,
        }
    }
}

/// Flashbots executor with revm simulation
pub struct FlashbotsExecutor {
    config: FlashbotsExecutorConfig,
    client: Client,
    signer: Option<PrivateKeySigner>,
    /// REVM simulator for local simulation
    simulator: Arc<RwLock<RevmSimulator>>,
}

impl FlashbotsExecutor {
    pub fn new(config: FlashbotsExecutorConfig) -> eyre::Result<Self> {
        let signer = if !config.signer_key.is_empty() {
            Some(config.signer_key.parse::<PrivateKeySigner>()?)
        } else {
            None
        };

        // Initialize REVM simulator
        let simulator = RevmSimulator::new().with_chain_id(1);

        Ok(Self {
            config,
            client: Client::new(),
            signer,
            simulator: Arc::new(RwLock::new(simulator)),
        })
    }

    /// Initialize simulator with wallet balance
    /// In dry_run mode, uses simulated balance. In live mode, fetches real balance.
    pub async fn init_simulator_balance(&self) -> eyre::Result<()> {
        if let Some(ref signer) = self.signer {
            let bot_address = signer.address();

            let balance = if self.config.dry_run {
                // Use simulated balance for dry run (2 ETH)
                let sim_balance = U256::from(2_000_000_000_000_000_000u128); // 2 ETH
                info!("Simulator: Using simulated balance 2 ETH for {} (dry run)", bot_address);
                sim_balance
            } else {
                // Fetch real balance from chain for live mode
                let provider = ProviderBuilder::new().on_http(self.config.rpc_url.parse()?);
                let real_balance = provider.get_balance(bot_address).await?;
                let balance_eth = real_balance.to::<u128>() as f64 / 1e18;
                info!("Simulator: Set real balance {:.4} ETH for {}", balance_eth, bot_address);
                real_balance
            };

            let mut sim = self.simulator.write();
            sim.set_balance(bot_address, balance);
        }
        Ok(())
    }

    /// Update simulator block environment from RPC
    pub async fn sync_block_env(&self) -> eyre::Result<()> {
        let provider = ProviderBuilder::new().on_http(self.config.rpc_url.parse()?);

        let block_number = provider.get_block_number().await?;
        let block = provider
            .get_block_by_number(
                alloy::eips::BlockNumberOrTag::Number(block_number),
                alloy::rpc::types::BlockTransactionsKind::Hashes,
            )
            .await?
            .ok_or_else(|| eyre::eyre!("Block not found"))?;

        let base_fee = block
            .header
            .base_fee_per_gas
            .map(U256::from)
            .unwrap_or(U256::from(30_000_000_000u64));

        let mut sim = self.simulator.write();
        sim.set_block_env(block_number, block.header.timestamp, base_fee);

        debug!(
            block = block_number,
            base_fee = %base_fee,
            "Synced REVM block environment"
        );

        Ok(())
    }

    /// Build arbitrage transaction for simulation
    fn build_arb_tx_for_sim(&self, action: &ArbitrageAction) -> RevmTransaction {
        let caller = self
            .signer
            .as_ref()
            .map(|s| s.address())
            .unwrap_or(Address::ZERO);

        // Build calldata for flash loan arbitrage
        // This would encode: executeArbitrage(path, amounts, minProfit)
        let calldata = Bytes::new(); // Placeholder - would use actual encoding

        RevmTransaction::new(caller, self.config.flashloan_contract, calldata)
            .with_gas_limit(500_000)
            .with_value(U256::ZERO)
    }

    /// Build sandwich transactions for simulation
    fn build_sandwich_txs_for_sim(
        &self,
        action: &SandwichAction,
    ) -> (RevmTransaction, RevmTransaction) {
        let caller = self
            .signer
            .as_ref()
            .map(|s| s.address())
            .unwrap_or(Address::ZERO);

        // Frontrun: buy tokens before victim
        let frontrun = RevmTransaction::new(
            caller,
            action.frontrun.pool,
            Bytes::new(), // Would encode swap
        )
        .with_gas_limit(300_000)
        .with_value(U256::ZERO);

        // Backrun: sell tokens after victim
        let backrun = RevmTransaction::new(
            caller,
            action.backrun.pool,
            Bytes::new(), // Would encode reverse swap
        )
        .with_gas_limit(300_000)
        .with_value(U256::ZERO);

        (frontrun, backrun)
    }

    /// Build liquidation transaction for simulation
    fn build_liquidation_tx_for_sim(&self, action: &LiquidationAction) -> RevmTransaction {
        let caller = self
            .signer
            .as_ref()
            .map(|s| s.address())
            .unwrap_or(Address::ZERO);

        // Would encode: executeLiquidation(protocol, user, collateral, debt, amount)
        let calldata = Bytes::new();

        RevmTransaction::new(caller, self.config.flashloan_contract, calldata)
            .with_gas_limit(800_000)
            .with_value(U256::ZERO)
    }

    /// Simulate a bundle using REVM
    fn simulate_bundle_revm(&self, txs: &[RevmTransaction]) -> eyre::Result<(bool, u64, U256)> {
        let mut sim = self.simulator.write();

        // Create checkpoint for rollback
        let checkpoint = sim.checkpoint();

        let mut total_gas = 0u64;
        let mut all_success = true;

        for tx in txs {
            match sim.simulate_tx(tx) {
                Ok(result) => {
                    total_gas += result.gas_used;
                    if !result.success {
                        all_success = false;
                        warn!("Transaction in bundle reverted");
                        break;
                    }
                }
                Err(e) => {
                    all_success = false;
                    warn!("Simulation error: {}", e);
                    break;
                }
            }
        }

        // Rollback state
        sim.rollback_to(checkpoint);

        // Estimate profit (simplified - would need actual balance tracking)
        let estimated_profit = if all_success {
            U256::from(total_gas) * U256::from(1_000_000_000u64) // Rough estimate
        } else {
            U256::ZERO
        };

        Ok((all_success, total_gas, estimated_profit))
    }

    /// Submit bundle to Flashbots relay with proper signing
    async fn submit_bundle(
        &self,
        txs: Vec<String>,
        target_block: u64,
    ) -> eyre::Result<BundleResult> {
        let signer = self.signer.as_ref()
            .ok_or_else(|| eyre::eyre!("No signer configured"))?;

        let bundle = FlashbotsBundle {
            txs,
            block_number: format!("0x{:x}", target_block),
            min_timestamp: None,
            max_timestamp: None,
        };

        let request = SendBundleParams {
            jsonrpc: "2.0".to_string(),
            id: 1,
            method: "eth_sendBundle".to_string(),
            params: vec![bundle],
        };

        // Serialize request body
        let body = serde_json::to_string(&request)?;

        // Sign the request body hash for Flashbots authentication
        // Flashbots expects EIP-191 personal sign: sign("\x19Ethereum Signed Message:\n" + len + keccak256(body))
        let body_hash = keccak256(body.as_bytes());

        // Use sign_message which adds the Ethereum message prefix (EIP-191)
        let signature = signer.sign_message(body_hash.as_slice()).await?;

        // Format: address:signature (65 bytes: r[32] + s[32] + v[1])
        let sig_bytes = signature.as_bytes();
        let sig_header = format!(
            "{}:0x{}",
            signer.address(),
            hex::encode(sig_bytes)
        );

        debug!(
            target_block = target_block,
            sig_header = %sig_header,
            "Submitting signed bundle to Flashbots"
        );

        let response = self
            .client
            .post(&self.config.relay_url)
            .header("Content-Type", "application/json")
            .header("X-Flashbots-Signature", &sig_header)
            .body(body)
            .send()
            .await?;

        let status = response.status();
        let response_text = response.text().await?;

        debug!(status = %status, response = %response_text, "Flashbots response");

        if !status.is_success() {
            return Err(eyre::eyre!("Flashbots HTTP error {}: {}", status, response_text));
        }

        let bundle_response: BundleResponse = serde_json::from_str(&response_text)
            .map_err(|e| eyre::eyre!("Failed to parse response: {} - body: {}", e, response_text))?;

        if let Some(error) = bundle_response.error {
            return Err(eyre::eyre!("Flashbots error: {}", error.message));
        }

        bundle_response
            .result
            .ok_or_else(|| eyre::eyre!("No result in Flashbots response"))
    }

    /// Build flashloan arbitrage transaction
    async fn build_flashloan_arb_tx(
        &self,
        arb: &ArbitrageAction,
        nonce: u64,
        gas_price: u128,
        max_priority_fee: u128,
    ) -> eyre::Result<String> {
        let signer = self.signer.as_ref()
            .ok_or_else(|| eyre::eyre!("No signer configured"))?;

        // Encode executeBalancerFlashloan call
        // Function: executeBalancerFlashloan(address[] tokens, uint256[] amounts, bytes swapData)
        // Extract token from first step in path, or use input_token
        let flash_token = arb.flashloan_token.unwrap_or(arb.input_token);
        let flash_amount = arb.flashloan_amount.unwrap_or(arb.input_amount);
        let calldata = encode_flashloan_arb(flash_token, flash_amount);

        let tx = TransactionRequest::default()
            .with_to(self.config.flashloan_contract)
            .with_nonce(nonce)
            .with_chain_id(1)
            .with_gas_limit(500_000)
            .with_max_fee_per_gas(gas_price + max_priority_fee)
            .with_max_priority_fee_per_gas(max_priority_fee)
            .with_input(calldata);

        let wallet = EthereumWallet::from(signer.clone());
        let signed = tx.build(&wallet).await?;

        // Encode the signed transaction using RLP
        let mut encoded = Vec::new();
        signed.encode_2718(&mut encoded);
        let raw_tx = hex::encode(&encoded);
        Ok(format!("0x{}", raw_tx))
    }

    /// Build liquidation transaction
    async fn build_liquidation_tx(
        &self,
        liq: &LiquidationAction,
        nonce: u64,
        gas_price: u128,
        max_priority_fee: u128,
    ) -> eyre::Result<String> {
        let signer = self.signer.as_ref()
            .ok_or_else(|| eyre::eyre!("No signer configured"))?;

        // Encode liquidation call (simplified)
        let calldata = Bytes::new(); // Would encode actual liquidation params

        let tx = TransactionRequest::default()
            .with_to(self.config.flashloan_contract)
            .with_nonce(nonce)
            .with_chain_id(1)
            .with_gas_limit(800_000)
            .with_max_fee_per_gas(gas_price + max_priority_fee)
            .with_max_priority_fee_per_gas(max_priority_fee)
            .with_input(calldata);

        let wallet = EthereumWallet::from(signer.clone());
        let signed = tx.build(&wallet).await?;

        let mut encoded = Vec::new();
        signed.encode_2718(&mut encoded);
        let raw_tx = hex::encode(&encoded);
        Ok(format!("0x{}", raw_tx))
    }

    /// Build and sign a swap transaction
    async fn build_signed_swap_tx(
        &self,
        pool: Address,
        token_in: Address,
        token_out: Address,
        amount_in: U256,
        min_amount_out: U256,
        nonce: u64,
        gas_price: u128,
        max_priority_fee: u128,
    ) -> eyre::Result<String> {
        let signer = self.signer.as_ref()
            .ok_or_else(|| eyre::eyre!("No signer configured"))?;

        // Encode Uniswap V3 exactInputSingle call
        // Function: exactInputSingle((address,address,uint24,address,uint256,uint256,uint160))
        let fee: u32 = 3000; // 0.3% fee tier
        let sqrt_price_limit = U256::ZERO;

        // ABI encode the swap params
        let calldata = encode_exact_input_single(
            token_in,
            token_out,
            fee,
            signer.address(),
            amount_in,
            min_amount_out,
            sqrt_price_limit,
        );

        // Build EIP-1559 transaction
        let tx = TransactionRequest::default()
            .with_to(pool)
            .with_nonce(nonce)
            .with_chain_id(1)
            .with_gas_limit(300_000)
            .with_max_fee_per_gas(gas_price)
            .with_max_priority_fee_per_gas(max_priority_fee)
            .with_input(calldata);

        // Sign transaction
        let wallet = EthereumWallet::from(signer.clone());
        let signed = tx.build(&wallet).await?;

        // Encode to raw transaction hex
        let mut encoded = Vec::new();
        signed.encode_2718(&mut encoded);
        let raw_tx = hex::encode(&encoded);
        Ok(format!("0x{}", raw_tx))
    }
}

#[async_trait]
impl Executor for FlashbotsExecutor {
    fn name(&self) -> &str {
        "FlashbotsExecutor"
    }

    fn supports(&self, action: &Action) -> bool {
        matches!(
            action,
            Action::Arbitrage(_) | Action::Sandwich(_) | Action::Liquidation(_) | Action::Backrun(_)
        )
    }

    async fn simulate(&self, action: &Action) -> eyre::Result<ExecutionResult> {
        // Sync block environment before simulation
        if let Err(e) = self.sync_block_env().await {
            warn!("Failed to sync block env: {}", e);
        }

        let (action_id, txs, expected_profit) = match action {
            Action::Arbitrage(arb) => {
                let tx = self.build_arb_tx_for_sim(arb);
                (arb.id.clone(), vec![tx], arb.expected_profit)
            }
            Action::Sandwich(sandwich) => {
                let (frontrun, backrun) = self.build_sandwich_txs_for_sim(sandwich);
                (
                    sandwich.id.clone(),
                    vec![frontrun, backrun],
                    sandwich.expected_profit,
                )
            }
            Action::Liquidation(liq) => {
                let tx = self.build_liquidation_tx_for_sim(liq);
                (liq.id.clone(), vec![tx], liq.expected_profit)
            }
            Action::Backrun(backrun) => {
                let tx = self.build_arb_tx_for_sim(&backrun.arb);
                (backrun.id.clone(), vec![tx], backrun.arb.expected_profit)
            }
        };

        // Run REVM simulation
        let (success, gas_used, sim_profit) = self.simulate_bundle_revm(&txs)?;

        if success {
            info!(
                action = %action_id,
                gas_used = gas_used,
                expected_profit = %expected_profit,
                "REVM simulation successful"
            );

            Ok(ExecutionResult::Simulated {
                action_id,
                would_profit: expected_profit,
                gas_estimate: gas_used,
            })
        } else {
            Ok(ExecutionResult::Failed {
                action_id,
                reason: "REVM simulation reverted".to_string(),
            })
        }
    }

    async fn execute(&self, action: Action) -> eyre::Result<ExecutionResult> {
        // Always simulate first
        let sim_result = self.simulate(&action).await?;

        // Check if simulation passed
        if let ExecutionResult::Failed { action_id, reason } = sim_result {
            return Ok(ExecutionResult::Failed { action_id, reason });
        }

        if self.config.dry_run {
            return Ok(sim_result);
        }

        let signer = self.signer.as_ref()
            .ok_or_else(|| eyre::eyre!("No signer configured"))?;

        // Get current block and gas prices
        let provider = ProviderBuilder::new().on_http(self.config.rpc_url.parse()?);
        let current_block = provider.get_block_number().await?;
        let target_block = current_block + 1;

        // Get nonce and gas prices
        let nonce = provider.get_transaction_count(signer.address()).await?;
        let gas_price = provider.get_gas_price().await?;
        let max_priority_fee = 3_000_000_000u128; // 3 gwei priority fee

        let (action_id, profit, signed_txs) = match &action {
            Action::Sandwich(sandwich) => {
                // Verify we have raw victim transaction bytes
                if sandwich.target_tx_raw.is_empty() {
                    warn!("Sandwich {} has no raw victim tx bytes", sandwich.id);
                    return Ok(ExecutionResult::Failed {
                        action_id: sandwich.id.clone(),
                        reason: "No raw victim transaction bytes available".to_string(),
                    });
                }

                info!(
                    "Building sandwich bundle for {} - victim tx: {} ({} bytes)",
                    sandwich.id,
                    sandwich.target_tx,
                    sandwich.target_tx_raw.len()
                );

                // Determine target pool - use Uniswap V3 SwapRouter
                let swap_router = address!("E592427A0AEce92De3Edee1F18E0157C05861564");

                // Build frontrun transaction (buy tokens before victim)
                let frontrun_tx = self.build_signed_swap_tx(
                    swap_router,
                    sandwich.frontrun.token_in,
                    sandwich.frontrun.token_out,
                    sandwich.frontrun.amount_in,
                    sandwich.frontrun.min_amount_out,
                    nonce,
                    gas_price,
                    max_priority_fee,
                ).await?;

                // Victim transaction (raw bytes already encoded)
                let victim_tx = format!("0x{}", hex::encode(&sandwich.target_tx_raw));

                // Build backrun transaction (sell tokens after victim)
                let backrun_tx = self.build_signed_swap_tx(
                    swap_router,
                    sandwich.backrun.token_in,
                    sandwich.backrun.token_out,
                    sandwich.backrun.amount_in,
                    sandwich.backrun.min_amount_out,
                    nonce + 1, // Increment nonce for backrun
                    gas_price,
                    max_priority_fee,
                ).await?;

                info!(
                    "Sandwich bundle: frontrun={} bytes, victim={} bytes, backrun={} bytes",
                    frontrun_tx.len(),
                    victim_tx.len(),
                    backrun_tx.len()
                );

                // Bundle order: [frontrun, victim, backrun]
                (sandwich.id.clone(), sandwich.expected_profit, vec![frontrun_tx, victim_tx, backrun_tx])
            }
            Action::Arbitrage(arb) => {
                // Build arbitrage transaction using flashloan contract
                let arb_tx = self.build_flashloan_arb_tx(arb, nonce, gas_price, max_priority_fee).await?;
                (arb.id.clone(), arb.expected_profit, vec![arb_tx])
            }
            Action::Liquidation(liq) => {
                let liq_tx = self.build_liquidation_tx(liq, nonce, gas_price, max_priority_fee).await?;
                (liq.id.clone(), liq.expected_profit, vec![liq_tx])
            }
            Action::Backrun(backrun) => {
                let arb_tx = self.build_flashloan_arb_tx(&backrun.arb, nonce, gas_price, max_priority_fee).await?;
                (backrun.id.clone(), backrun.arb.expected_profit, vec![arb_tx])
            }
        };

        info!(
            action = %action_id,
            target_block = target_block,
            num_txs = signed_txs.len(),
            "Submitting bundle to Flashbots"
        );

        match self.submit_bundle(signed_txs, target_block).await {
            Ok(result) => {
                info!(
                    bundle_hash = %result.bundle_hash,
                    "Bundle submitted successfully"
                );
                Ok(ExecutionResult::Success {
                    action_id,
                    tx_hash: B256::ZERO, // Bundle hash, not tx hash
                    profit,
                    gas_used: 0, // Unknown until included
                })
            }
            Err(e) => {
                error!("Bundle submission failed: {}", e);
                Ok(ExecutionResult::Failed {
                    action_id,
                    reason: e.to_string(),
                })
            }
        }
    }
}

/// Encode Uniswap V3 exactInputSingle function call
fn encode_exact_input_single(
    token_in: Address,
    token_out: Address,
    fee: u32,
    recipient: Address,
    amount_in: U256,
    min_amount_out: U256,
    sqrt_price_limit: U256,
) -> Bytes {
    // Function selector for exactInputSingle((address,address,uint24,address,uint256,uint256,uint160))
    // = 0x414bf389
    let selector = [0x41, 0x4b, 0xf3, 0x89];

    let mut calldata = Vec::with_capacity(260);
    calldata.extend_from_slice(&selector);

    // Encode struct as tuple
    // tokenIn (address)
    calldata.extend_from_slice(&[0u8; 12]);
    calldata.extend_from_slice(token_in.as_slice());

    // tokenOut (address)
    calldata.extend_from_slice(&[0u8; 12]);
    calldata.extend_from_slice(token_out.as_slice());

    // fee (uint24) - padded to 32 bytes
    let mut fee_bytes = [0u8; 32];
    fee_bytes[29..32].copy_from_slice(&fee.to_be_bytes()[1..4]);
    calldata.extend_from_slice(&fee_bytes);

    // recipient (address)
    calldata.extend_from_slice(&[0u8; 12]);
    calldata.extend_from_slice(recipient.as_slice());

    // amountIn (uint256)
    calldata.extend_from_slice(&amount_in.to_be_bytes::<32>());

    // amountOutMinimum (uint256)
    calldata.extend_from_slice(&min_amount_out.to_be_bytes::<32>());

    // sqrtPriceLimitX96 (uint160) - padded to 32 bytes
    calldata.extend_from_slice(&sqrt_price_limit.to_be_bytes::<32>());

    Bytes::from(calldata)
}

/// Encode flashloan arbitrage call
fn encode_flashloan_arb(token: Address, amount: U256) -> Bytes {
    // Function: executeBalancerFlashloan(address[] tokens, uint256[] amounts, bytes swapData)
    // Selector = keccak256("executeBalancerFlashloan(address[],uint256[],bytes)")[:4]
    let selector = [0x5c, 0x38, 0x44, 0x9e]; // Computed selector

    let mut calldata = Vec::with_capacity(512);
    calldata.extend_from_slice(&selector);

    // Offset to tokens array (3 * 32 = 96)
    calldata.extend_from_slice(&U256::from(96).to_be_bytes::<32>());

    // Offset to amounts array (96 + 32 + 32 = 160)
    calldata.extend_from_slice(&U256::from(160).to_be_bytes::<32>());

    // Offset to swapData (160 + 32 + 32 = 224)
    calldata.extend_from_slice(&U256::from(224).to_be_bytes::<32>());

    // Tokens array length = 1
    calldata.extend_from_slice(&U256::from(1).to_be_bytes::<32>());

    // Token address
    calldata.extend_from_slice(&[0u8; 12]);
    calldata.extend_from_slice(token.as_slice());

    // Amounts array length = 1
    calldata.extend_from_slice(&U256::from(1).to_be_bytes::<32>());

    // Amount
    calldata.extend_from_slice(&amount.to_be_bytes::<32>());

    // SwapData length (empty for now - would contain encoded swap steps)
    calldata.extend_from_slice(&U256::ZERO.to_be_bytes::<32>());

    Bytes::from(calldata)
}
