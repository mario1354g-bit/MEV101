//! Direct executor - submits transactions directly to mempool

use crate::artemis::{Action, ArbitrageAction, ExecutionResult, Executor, LiquidationAction};
use alloy::network::{EthereumWallet, TransactionBuilder};
use alloy::primitives::{Address, Bytes, U256};
use alloy::providers::{Provider, ProviderBuilder};
use alloy::rpc::types::TransactionRequest;
use alloy::signers::local::PrivateKeySigner;
use async_trait::async_trait;
use std::sync::Arc;
use tracing::{debug, error, info, warn};

/// Direct executor configuration
#[derive(Debug, Clone)]
pub struct DirectExecutorConfig {
    pub rpc_url: String,
    pub signer_key: String,
    pub flashloan_contract: Address,
    pub dry_run: bool,
    pub max_gas_price: u128,
    pub max_priority_fee: u128,
}

impl Default for DirectExecutorConfig {
    fn default() -> Self {
        Self {
            rpc_url: String::new(),
            signer_key: String::new(),
            flashloan_contract: Address::ZERO,
            dry_run: true,
            max_gas_price: 100_000_000_000,   // 100 gwei
            max_priority_fee: 10_000_000_000, // 10 gwei
        }
    }
}

/// Direct mempool executor
pub struct DirectExecutor {
    config: DirectExecutorConfig,
    signer: Option<PrivateKeySigner>,
}

impl DirectExecutor {
    pub fn new(config: DirectExecutorConfig) -> eyre::Result<Self> {
        let signer = if !config.signer_key.is_empty() {
            Some(config.signer_key.parse::<PrivateKeySigner>()?)
        } else {
            None
        };

        Ok(Self { config, signer })
    }

    /// Build and encode arbitrage transaction
    fn encode_arb_call(&self, action: &ArbitrageAction) -> Bytes {
        // Would ABI-encode the flash loan arbitrage call
        // executeArbitrage(path, amounts, minProfit)
        Bytes::new()
    }

    /// Build and encode liquidation transaction
    fn encode_liquidation_call(&self, action: &LiquidationAction) -> Bytes {
        // Would ABI-encode the liquidation call
        // executeLiquidation(protocol, user, collateral, debt, amount)
        Bytes::new()
    }

    /// Simulate transaction using eth_call
    async fn simulate_tx(&self, to: Address, data: Bytes, value: U256) -> eyre::Result<(bool, u64)> {
        let provider = ProviderBuilder::new().on_http(self.config.rpc_url.parse()?);

        let tx = TransactionRequest::default()
            .to(to)
            .input(data.into())
            .value(value);

        // Estimate gas (will revert if tx would fail)
        match provider.estimate_gas(&tx).await {
            Ok(gas) => Ok((true, gas)),
            Err(e) => {
                warn!("Simulation failed: {}", e);
                Ok((false, 0))
            }
        }
    }
}

#[async_trait]
impl Executor for DirectExecutor {
    fn name(&self) -> &str {
        "DirectExecutor"
    }

    fn supports(&self, action: &Action) -> bool {
        // Direct executor supports arb and liquidation (not sandwich)
        matches!(action, Action::Arbitrage(_) | Action::Liquidation(_))
    }

    async fn simulate(&self, action: &Action) -> eyre::Result<ExecutionResult> {
        let (action_id, to, data, profit) = match action {
            Action::Arbitrage(arb) => {
                let data = self.encode_arb_call(arb);
                (
                    arb.id.clone(),
                    self.config.flashloan_contract,
                    data,
                    arb.expected_profit,
                )
            }
            Action::Liquidation(liq) => {
                let data = self.encode_liquidation_call(liq);
                (
                    liq.id.clone(),
                    self.config.flashloan_contract,
                    data,
                    liq.expected_profit,
                )
            }
            _ => {
                return Ok(ExecutionResult::Failed {
                    action_id: "unknown".to_string(),
                    reason: "Unsupported action type for DirectExecutor".to_string(),
                })
            }
        };

        let (success, gas) = self.simulate_tx(to, data, U256::ZERO).await?;

        if success {
            Ok(ExecutionResult::Simulated {
                action_id,
                would_profit: profit,
                gas_estimate: gas,
            })
        } else {
            Ok(ExecutionResult::Failed {
                action_id,
                reason: "Simulation reverted".to_string(),
            })
        }
    }

    async fn execute(&self, action: Action) -> eyre::Result<ExecutionResult> {
        if self.config.dry_run {
            return self.simulate(&action).await;
        }

        let signer = self
            .signer
            .as_ref()
            .ok_or_else(|| eyre::eyre!("No signer configured"))?;

        let wallet = EthereumWallet::from(signer.clone());
        let provider = ProviderBuilder::new()
            .wallet(wallet)
            .on_http(self.config.rpc_url.parse()?);

        let (action_id, to, data, gas_price, priority_fee, profit) = match &action {
            Action::Arbitrage(arb) => {
                let data = self.encode_arb_call(arb);
                (
                    arb.id.clone(),
                    self.config.flashloan_contract,
                    data,
                    arb.gas_price.min(self.config.max_gas_price),
                    arb.priority_fee.min(self.config.max_priority_fee),
                    arb.expected_profit,
                )
            }
            Action::Liquidation(liq) => {
                let data = self.encode_liquidation_call(liq);
                (
                    liq.id.clone(),
                    self.config.flashloan_contract,
                    data,
                    liq.gas_price.min(self.config.max_gas_price),
                    liq.priority_fee.min(self.config.max_priority_fee),
                    liq.expected_profit,
                )
            }
            _ => {
                return Ok(ExecutionResult::Failed {
                    action_id: "unknown".to_string(),
                    reason: "Unsupported action type".to_string(),
                })
            }
        };

        // First simulate
        let (success, gas_estimate) = self.simulate_tx(to, data.clone(), U256::ZERO).await?;
        if !success {
            return Ok(ExecutionResult::Failed {
                action_id,
                reason: "Pre-execution simulation failed".to_string(),
            });
        }

        // Build transaction
        let tx = TransactionRequest::default()
            .to(to)
            .input(data.into())
            .gas_limit(gas_estimate + 50_000) // Add buffer
            .max_fee_per_gas(gas_price)
            .max_priority_fee_per_gas(priority_fee);

        info!("Sending transaction for {}", action_id);

        // Send transaction
        match provider.send_transaction(tx).await {
            Ok(pending) => {
                let tx_hash = *pending.tx_hash();
                info!("Transaction sent: {:?}", tx_hash);

                // Wait for receipt
                match pending.get_receipt().await {
                    Ok(receipt) => {
                        if receipt.status() {
                            Ok(ExecutionResult::Success {
                                action_id,
                                tx_hash,
                                profit,
                                gas_used: receipt.gas_used,
                            })
                        } else {
                            Ok(ExecutionResult::Failed {
                                action_id,
                                reason: "Transaction reverted".to_string(),
                            })
                        }
                    }
                    Err(e) => Ok(ExecutionResult::Failed {
                        action_id,
                        reason: format!("Failed to get receipt: {}", e),
                    }),
                }
            }
            Err(e) => {
                error!("Failed to send transaction: {}", e);
                Ok(ExecutionResult::Failed {
                    action_id,
                    reason: e.to_string(),
                })
            }
        }
    }
}
