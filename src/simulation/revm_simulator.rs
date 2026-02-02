//! Local EVM simulation using REVM.
//!
//! This module provides high-performance local EVM simulation capabilities for MEV bot
//! using the REVM (Rust EVM) implementation. It supports:
//! - Single transaction simulation
//! - Bundle simulation with state persistence
//! - Sandwich attack profitability analysis
//! - State checkpointing and rollback

use alloy::primitives::{Address, Bytes, Log, B256, U256};
use alloy::providers::Provider;
use alloy::transports::Transport;
use revm::db::{CacheDB, EmptyDB};
use revm::primitives::{
    AccountInfo, BlobExcessGasAndPrice, BlockEnv, Bytecode, CfgEnv,
    EnvWithHandlerCfg, ExecutionResult, Output, SpecId,
    TransactTo, TxEnv,
};
use revm::{Evm, InMemoryDB};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tracing::{debug, trace, warn};

use crate::error::SimulationError;

/// Result of simulating a single transaction.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SimulationResult {
    /// Whether the transaction executed successfully
    pub success: bool,
    /// Gas used by the transaction
    pub gas_used: u64,
    /// Return data from the transaction
    #[serde(with = "bytes_serde")]
    pub output: Bytes,
    /// Logs emitted during execution
    #[serde(skip)]
    pub logs: Vec<Log>,
    /// State changes made by the transaction
    pub state_changes: Vec<StateChange>,
}

/// Helper module for serializing Bytes
mod bytes_serde {
    use alloy::primitives::Bytes;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S>(bytes: &Bytes, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        hex::encode(bytes.as_ref()).serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Bytes, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        let bytes = hex::decode(s.trim_start_matches("0x"))
            .map_err(serde::de::Error::custom)?;
        Ok(Bytes::from(bytes))
    }
}

impl SimulationResult {
    /// Create a failed simulation result with an error message.
    pub fn failed(reason: &str) -> Self {
        Self {
            success: false,
            gas_used: 0,
            output: Bytes::from(reason.as_bytes().to_vec()),
            logs: Vec::new(),
            state_changes: Vec::new(),
        }
    }
}

/// Result of simulating a sandwich attack.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SandwichSimResult {
    /// Result of the frontrun transaction
    pub frontrun_result: SimulationResult,
    /// Result of the victim transaction
    pub victim_result: SimulationResult,
    /// Result of the backrun transaction
    pub backrun_result: SimulationResult,
    /// Calculated profit in wei
    pub profit: U256,
    /// Total gas used by all transactions
    pub total_gas: u64,
}

impl Default for SandwichSimResult {
    fn default() -> Self {
        Self {
            frontrun_result: SimulationResult::default(),
            victim_result: SimulationResult::default(),
            backrun_result: SimulationResult::default(),
            profit: U256::ZERO,
            total_gas: 0,
        }
    }
}

/// A state change recorded during simulation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateChange {
    /// Contract address that was modified
    pub address: Address,
    /// Storage slot that was modified
    pub slot: U256,
    /// Previous value
    pub old_value: U256,
    /// New value
    pub new_value: U256,
}

/// Transaction data for simulation.
#[derive(Debug, Clone)]
pub struct Transaction {
    /// Transaction sender
    pub from: Address,
    /// Transaction recipient (None for contract creation)
    pub to: Option<Address>,
    /// Value to transfer in wei
    pub value: U256,
    /// Transaction input data
    pub data: Bytes,
    /// Gas limit
    pub gas_limit: u64,
    /// Gas price (for legacy transactions)
    pub gas_price: Option<U256>,
    /// Max fee per gas (for EIP-1559)
    pub max_fee_per_gas: Option<U256>,
    /// Max priority fee per gas (for EIP-1559)
    pub max_priority_fee_per_gas: Option<U256>,
    /// Nonce (optional, will be fetched if not provided)
    pub nonce: Option<u64>,
}

impl Transaction {
    /// Create a new transaction for simulation.
    pub fn new(from: Address, to: Address, data: Bytes) -> Self {
        Self {
            from,
            to: Some(to),
            value: U256::ZERO,
            data,
            gas_limit: 1_000_000,
            gas_price: None,
            max_fee_per_gas: None,
            max_priority_fee_per_gas: None,
            nonce: None,
        }
    }

    /// Set the value to transfer.
    pub fn with_value(mut self, value: U256) -> Self {
        self.value = value;
        self
    }

    /// Set the gas limit.
    pub fn with_gas_limit(mut self, gas_limit: u64) -> Self {
        self.gas_limit = gas_limit;
        self
    }

    /// Set the gas price (legacy).
    pub fn with_gas_price(mut self, gas_price: U256) -> Self {
        self.gas_price = Some(gas_price);
        self
    }

    /// Set EIP-1559 gas parameters.
    pub fn with_eip1559_gas(
        mut self,
        max_fee_per_gas: U256,
        max_priority_fee_per_gas: U256,
    ) -> Self {
        self.max_fee_per_gas = Some(max_fee_per_gas);
        self.max_priority_fee_per_gas = Some(max_priority_fee_per_gas);
        self
    }

    /// Set the nonce.
    pub fn with_nonce(mut self, nonce: u64) -> Self {
        self.nonce = Some(nonce);
        self
    }
}

/// Checkpoint for state rollback.
#[derive(Debug, Clone)]
struct Checkpoint {
    /// Snapshot of accounts at checkpoint time
    accounts: HashMap<Address, AccountInfo>,
    /// Snapshot of storage at checkpoint time
    storage: HashMap<(Address, U256), U256>,
}

/// Local EVM simulator using REVM.
pub struct RevmSimulator {
    /// The cached database containing EVM state
    db: InMemoryDB,
    /// Block environment settings
    block_env: BlockEnv,
    /// Configuration environment
    cfg_env: CfgEnv,
    /// Checkpoints for rollback support
    checkpoints: Vec<Checkpoint>,
    /// Current chain ID
    chain_id: u64,
}

impl RevmSimulator {
    /// Create a new REVM simulator with empty state.
    pub fn new() -> Self {
        let db = InMemoryDB::default();

        let mut block_env = BlockEnv::default();
        block_env.number = U256::from(1);
        block_env.timestamp = U256::from(1700000000u64);
        block_env.basefee = U256::from(30_000_000_000u64); // 30 gwei
        block_env.gas_limit = U256::from(30_000_000u64);
        block_env.coinbase = Address::ZERO;
        block_env.difficulty = U256::ZERO;
        block_env.prevrandao = Some(B256::ZERO);
        block_env.blob_excess_gas_and_price = Some(BlobExcessGasAndPrice::new(0, false));

        let mut cfg_env = CfgEnv::default();
        cfg_env.chain_id = 1; // Mainnet

        Self {
            db,
            block_env,
            cfg_env,
            checkpoints: Vec::new(),
            chain_id: 1,
        }
    }

    /// Create a new simulator with specific block environment.
    pub fn with_block_env(mut self, block_number: u64, timestamp: u64, base_fee: U256) -> Self {
        self.block_env.number = U256::from(block_number);
        self.block_env.timestamp = U256::from(timestamp);
        self.block_env.basefee = base_fee;
        self
    }

    /// Set the chain ID.
    pub fn with_chain_id(mut self, chain_id: u64) -> Self {
        self.chain_id = chain_id;
        self.cfg_env.chain_id = chain_id;
        self
    }

    /// Fork state from a provider at a specific block.
    ///
    /// Note: This is a simplified implementation that sets up the simulator
    /// with the correct block environment. For full forking with lazy state
    /// loading, use ForkDB instead.
    pub async fn fork_from_provider<T, P>(
        provider: &P,
        block: Option<u64>,
    ) -> Result<Self, SimulationError>
    where
        T: Transport + Clone,
        P: Provider<T>,
    {
        let block_number = match block {
            Some(n) => n,
            None => provider
                .get_block_number()
                .await
                .map_err(|e| SimulationError::ContractCallFailed(e.to_string()))?,
        };

        let block_data = provider
            .get_block_by_number(
                alloy::eips::BlockNumberOrTag::Number(block_number),
                alloy::rpc::types::BlockTransactionsKind::Hashes,
            )
            .await
            .map_err(|e| SimulationError::ContractCallFailed(e.to_string()))?
            .ok_or_else(|| SimulationError::ContractCallFailed("Block not found".to_string()))?;

        let base_fee = block_data
            .header
            .base_fee_per_gas
            .map(U256::from)
            .unwrap_or(U256::from(30_000_000_000u64));

        let chain_id = provider
            .get_chain_id()
            .await
            .map_err(|e| SimulationError::ContractCallFailed(e.to_string()))?;

        let simulator = Self::new()
            .with_block_env(block_number, block_data.header.timestamp, base_fee)
            .with_chain_id(chain_id);

        debug!(
            block_number = block_number,
            base_fee = %base_fee,
            chain_id = chain_id,
            "Created REVM simulator from provider"
        );

        Ok(simulator)
    }

    /// Simulate a single transaction.
    pub fn simulate_tx(&mut self, tx: &Transaction) -> Result<SimulationResult, SimulationError> {
        let tx_env = self.build_tx_env(tx);

        let env = EnvWithHandlerCfg::new_with_spec_id(
            Box::new(revm::primitives::Env {
                cfg: self.cfg_env.clone(),
                block: self.block_env.clone(),
                tx: tx_env,
            }),
            SpecId::CANCUN,
        );

        let mut evm = Evm::builder()
            .with_db(&mut self.db)
            .with_env_with_handler_cfg(env)
            .build();

        let result = evm.transact();
        drop(evm); // Explicitly drop EVM to release borrow

        match result {
            Ok(result) => {
                let sim_result = Self::process_execution_result(&result.result);
                trace!(
                    success = sim_result.success,
                    gas_used = sim_result.gas_used,
                    "Transaction simulation complete"
                );
                Ok(sim_result)
            }
            Err(e) => {
                warn!(error = %format!("{:?}", e), "Transaction simulation failed");
                Ok(SimulationResult::failed(&format!("{:?}", e)))
            }
        }
    }

    /// Simulate a bundle of transactions, returning results for each.
    /// State changes persist between transactions within the bundle.
    pub fn simulate_bundle(
        &mut self,
        txs: &[Transaction],
    ) -> Result<Vec<SimulationResult>, SimulationError> {
        let mut results = Vec::with_capacity(txs.len());

        for (i, tx) in txs.iter().enumerate() {
            let tx_env = self.build_tx_env(tx);

            let env = EnvWithHandlerCfg::new_with_spec_id(
                Box::new(revm::primitives::Env {
                    cfg: self.cfg_env.clone(),
                    block: self.block_env.clone(),
                    tx: tx_env,
                }),
                SpecId::CANCUN,
            );

            let mut evm = Evm::builder()
                .with_db(&mut self.db)
                .with_env_with_handler_cfg(env)
                .build();

            let result = evm.transact_commit();
            drop(evm); // Explicitly drop EVM to release borrow

            match result {
                Ok(result) => {
                    let sim_result = Self::process_execution_result(&result);
                    trace!(
                        tx_index = i,
                        success = sim_result.success,
                        gas_used = sim_result.gas_used,
                        "Bundle transaction simulation complete"
                    );
                    results.push(sim_result);
                }
                Err(e) => {
                    warn!(
                        tx_index = i,
                        error = %format!("{:?}", e),
                        "Bundle transaction simulation failed"
                    );
                    results.push(SimulationResult::failed(&format!("{:?}", e)));
                }
            }
        }

        Ok(results)
    }

    /// Simulate a sandwich attack (frontrun, victim, backrun) and calculate profit.
    ///
    /// The profit is calculated based on balance changes of the attacker's address.
    pub fn simulate_sandwich(
        &mut self,
        frontrun: &Transaction,
        victim: &Transaction,
        backrun: &Transaction,
    ) -> Result<SandwichSimResult, SimulationError> {
        // Create a checkpoint to enable rollback after simulation
        let checkpoint = self.checkpoint();

        // Get attacker's initial balance (using frontrun sender as attacker)
        let attacker = frontrun.from;
        let initial_balance = self.get_balance(attacker)?;

        // Simulate frontrun
        let frontrun_result = self.simulate_and_commit(frontrun)?;
        if !frontrun_result.success {
            let gas_used = frontrun_result.gas_used;
            self.rollback_to(checkpoint);
            return Ok(SandwichSimResult {
                frontrun_result,
                victim_result: SimulationResult::default(),
                backrun_result: SimulationResult::default(),
                profit: U256::ZERO,
                total_gas: gas_used,
            });
        }
        let frontrun_gas = frontrun_result.gas_used;

        // Simulate victim transaction
        let victim_result = self.simulate_and_commit(victim)?;
        let victim_gas = victim_result.gas_used;
        // We don't fail the sandwich if victim fails, as this might still be profitable

        // Simulate backrun
        let backrun_result = self.simulate_and_commit(backrun)?;
        let backrun_gas = backrun_result.gas_used;
        if !backrun_result.success {
            let total_gas = frontrun_gas + victim_gas;
            self.rollback_to(checkpoint);
            return Ok(SandwichSimResult {
                frontrun_result,
                victim_result,
                backrun_result,
                profit: U256::ZERO,
                total_gas,
            });
        }

        // Calculate profit from balance change
        let final_balance = self.get_balance(attacker)?;
        let total_gas = frontrun_gas + backrun_gas;

        // Profit = final_balance - initial_balance
        // Note: This doesn't account for token profits, only ETH balance changes
        // For token profits, you'd need to check token balance changes
        let profit = if final_balance > initial_balance {
            final_balance - initial_balance
        } else {
            U256::ZERO
        };

        // Optionally rollback to checkpoint if you don't want to keep state changes
        // self.rollback_to(checkpoint);

        debug!(
            profit = %profit,
            total_gas = total_gas,
            "Sandwich simulation complete"
        );

        Ok(SandwichSimResult {
            frontrun_result,
            victim_result,
            backrun_result,
            profit,
            total_gas,
        })
    }

    /// Get the balance of an account.
    pub fn get_balance(&self, address: Address) -> Result<U256, SimulationError> {
        use revm::Database;
        // Create a mutable copy for the Database trait method
        let mut db_ref = self.db.clone();
        match db_ref.basic(address) {
            Ok(Some(info)) => Ok(info.balance),
            Ok(None) => Ok(U256::ZERO),
            Err(_) => Ok(U256::ZERO),
        }
    }

    /// Get a storage slot value.
    pub fn get_storage(&self, address: Address, slot: U256) -> Result<U256, SimulationError> {
        use revm::Database;
        let mut db_ref = self.db.clone();
        match db_ref.storage(address, slot) {
            Ok(value) => Ok(value),
            Err(_) => Ok(U256::ZERO),
        }
    }

    /// Set the balance of an account (for testing).
    pub fn set_balance(&mut self, address: Address, balance: U256) {
        let mut info = AccountInfo::default();
        info.balance = balance;
        self.db.insert_account_info(address, info);
    }

    /// Set a storage slot value.
    pub fn set_storage(&mut self, address: Address, slot: U256, value: U256) {
        self.db.insert_account_storage(address, slot, value).ok();
    }

    /// Insert account info with optional bytecode.
    pub fn insert_account(&mut self, address: Address, info: AccountInfo) {
        self.db.insert_account_info(address, info);
    }

    /// Insert contract bytecode at an address.
    pub fn insert_contract(&mut self, address: Address, bytecode: Vec<u8>) {
        let code = Bytecode::new_raw(revm::primitives::Bytes::from(bytecode));
        let code_hash = code.hash_slow();

        // Insert the contract bytecode
        self.db.insert_contract(&mut AccountInfo {
            code_hash,
            code: Some(code),
            ..Default::default()
        });

        // Update account with the code hash
        let mut info = AccountInfo::default();
        info.code_hash = code_hash;
        self.db.insert_account_info(address, info);
    }

    /// Commit pending state changes.
    pub fn commit(&mut self) {
        // CacheDB automatically commits changes during transact_commit
        // This method exists for API completeness
        trace!("State committed");
    }

    /// Rollback to the initial state (clear all checkpoints).
    pub fn rollback(&mut self) {
        self.db = CacheDB::new(EmptyDB::default());
        self.checkpoints.clear();
        trace!("State rolled back to initial");
    }

    /// Create a checkpoint for rollback.
    pub fn checkpoint(&mut self) -> usize {
        let checkpoint = Checkpoint {
            accounts: self
                .db
                .accounts
                .iter()
                .map(|(k, v)| (*k, v.info.clone()))
                .collect(),
            storage: self
                .db
                .accounts
                .iter()
                .flat_map(|(addr, account)| {
                    account
                        .storage
                        .iter()
                        .map(move |(slot, value)| ((*addr, *slot), *value))
                })
                .collect(),
        };

        let id = self.checkpoints.len();
        self.checkpoints.push(checkpoint);
        trace!(checkpoint_id = id, "Created checkpoint");
        id
    }

    /// Rollback to a specific checkpoint.
    pub fn rollback_to(&mut self, checkpoint_id: usize) {
        if checkpoint_id >= self.checkpoints.len() {
            warn!(
                checkpoint_id = checkpoint_id,
                max_id = self.checkpoints.len(),
                "Invalid checkpoint ID"
            );
            return;
        }

        let checkpoint = self.checkpoints[checkpoint_id].clone();

        // Clear current state
        self.db = CacheDB::new(EmptyDB::default());

        // Restore accounts
        for (address, info) in checkpoint.accounts {
            self.db.insert_account_info(address, info);
        }

        // Restore storage
        for ((address, slot), value) in checkpoint.storage {
            self.set_storage(address, slot, value);
        }

        // Remove checkpoints after this one
        self.checkpoints.truncate(checkpoint_id);

        trace!(checkpoint_id = checkpoint_id, "Rolled back to checkpoint");
    }

    /// Update block environment.
    pub fn set_block_env(&mut self, block_number: u64, timestamp: u64, base_fee: U256) {
        self.block_env.number = U256::from(block_number);
        self.block_env.timestamp = U256::from(timestamp);
        self.block_env.basefee = base_fee;
    }

    /// Get the current block number.
    pub fn block_number(&self) -> u64 {
        self.block_env.number.try_into().unwrap_or(0)
    }

    /// Simulate transaction and commit state changes.
    fn simulate_and_commit(&mut self, tx: &Transaction) -> Result<SimulationResult, SimulationError> {
        let tx_env = self.build_tx_env(tx);

        let env = EnvWithHandlerCfg::new_with_spec_id(
            Box::new(revm::primitives::Env {
                cfg: self.cfg_env.clone(),
                block: self.block_env.clone(),
                tx: tx_env,
            }),
            SpecId::CANCUN,
        );

        let mut evm = Evm::builder()
            .with_db(&mut self.db)
            .with_env_with_handler_cfg(env)
            .build();

        let result = evm.transact_commit();
        drop(evm); // Release borrow before processing result

        match result {
            Ok(result) => Ok(Self::process_execution_result(&result)),
            Err(e) => Ok(SimulationResult::failed(&format!("{:?}", e))),
        }
    }

    /// Build transaction environment from Transaction struct.
    fn build_tx_env(&self, tx: &Transaction) -> TxEnv {
        let mut tx_env = TxEnv::default();

        tx_env.caller = tx.from;
        tx_env.transact_to = match tx.to {
            Some(addr) => TransactTo::Call(addr),
            None => TransactTo::Create,
        };
        tx_env.value = tx.value;
        tx_env.data = revm::primitives::Bytes::from(tx.data.to_vec());
        tx_env.gas_limit = tx.gas_limit;
        tx_env.nonce = tx.nonce;

        // Set gas price
        if let Some(gas_price) = tx.gas_price {
            tx_env.gas_price = gas_price;
        } else if let (Some(max_fee), Some(max_priority)) =
            (tx.max_fee_per_gas, tx.max_priority_fee_per_gas)
        {
            tx_env.gas_price = max_fee;
            tx_env.gas_priority_fee = Some(max_priority);
        } else {
            tx_env.gas_price = self.block_env.basefee;
        }

        tx_env
    }

    /// Process REVM execution result into SimulationResult.
    fn process_execution_result(result: &ExecutionResult) -> SimulationResult {
        match result {
            ExecutionResult::Success {
                gas_used,
                output,
                logs,
                ..
            } => {
                let output_bytes = match output {
                    Output::Call(bytes) => Bytes::from(bytes.to_vec()),
                    Output::Create(bytes, _) => Bytes::from(bytes.to_vec()),
                };

                let alloy_logs: Vec<Log> = logs
                    .iter()
                    .map(|log| {
                        Log::new(
                            log.address,
                            log.topics().to_vec(),
                            log.data.data.to_vec().into(),
                        )
                        .expect("Failed to create log")
                    })
                    .collect();

                SimulationResult {
                    success: true,
                    gas_used: *gas_used,
                    output: output_bytes,
                    logs: alloy_logs,
                    state_changes: Vec::new(), // Would need trace for full state changes
                }
            }
            ExecutionResult::Revert { gas_used, output } => SimulationResult {
                success: false,
                gas_used: *gas_used,
                output: Bytes::from(output.to_vec()),
                logs: Vec::new(),
                state_changes: Vec::new(),
            },
            ExecutionResult::Halt { gas_used, reason } => SimulationResult {
                success: false,
                gas_used: *gas_used,
                output: Bytes::from(format!("Halted: {:?}", reason).into_bytes()),
                logs: Vec::new(),
                state_changes: Vec::new(),
            },
        }
    }
}

impl Default for RevmSimulator {
    fn default() -> Self {
        Self::new()
    }
}

impl Clone for RevmSimulator {
    fn clone(&self) -> Self {
        Self {
            db: self.db.clone(),
            block_env: self.block_env.clone(),
            cfg_env: self.cfg_env.clone(),
            checkpoints: self.checkpoints.clone(),
            chain_id: self.chain_id,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_simulator() {
        let sim = RevmSimulator::new();
        assert_eq!(sim.chain_id, 1);
        assert_eq!(sim.block_number(), 1);
    }

    #[test]
    fn test_set_balance() {
        let mut sim = RevmSimulator::new();
        let address = Address::repeat_byte(0x42);

        sim.set_balance(address, U256::from(1_000_000_000_000_000_000u128));
        let balance = sim.get_balance(address).unwrap();

        assert_eq!(balance, U256::from(1_000_000_000_000_000_000u128));
    }

    #[test]
    fn test_set_storage() {
        let mut sim = RevmSimulator::new();
        let address = Address::repeat_byte(0x42);
        let slot = U256::from(1);
        let value = U256::from(0xDEADBEEFu64);

        sim.set_storage(address, slot, value);
        let retrieved = sim.get_storage(address, slot).unwrap();

        assert_eq!(retrieved, value);
    }

    #[test]
    fn test_checkpoint_and_rollback() {
        let mut sim = RevmSimulator::new();
        let address = Address::repeat_byte(0x42);

        // Set initial balance
        sim.set_balance(address, U256::from(100u64));

        // Create checkpoint
        let checkpoint = sim.checkpoint();

        // Modify balance
        sim.set_balance(address, U256::from(200u64));
        assert_eq!(sim.get_balance(address).unwrap(), U256::from(200u64));

        // Rollback to checkpoint
        sim.rollback_to(checkpoint);
        assert_eq!(sim.get_balance(address).unwrap(), U256::from(100u64));
    }

    #[test]
    fn test_transaction_creation() {
        let from = Address::repeat_byte(0x01);
        let to = Address::repeat_byte(0x02);
        let data = Bytes::from(vec![0x12, 0x34]);

        let tx = Transaction::new(from, to, data.clone())
            .with_value(U256::from(1000u64))
            .with_gas_limit(500000)
            .with_gas_price(U256::from(30_000_000_000u64));

        assert_eq!(tx.from, from);
        assert_eq!(tx.to, Some(to));
        assert_eq!(tx.data, data);
        assert_eq!(tx.value, U256::from(1000u64));
        assert_eq!(tx.gas_limit, 500000);
        assert_eq!(tx.gas_price, Some(U256::from(30_000_000_000u64)));
    }

    #[test]
    fn test_block_env_update() {
        let mut sim = RevmSimulator::new();

        sim.set_block_env(12345, 1700000000, U256::from(50_000_000_000u64));

        assert_eq!(sim.block_number(), 12345);
        assert_eq!(sim.block_env.timestamp, U256::from(1700000000u64));
        assert_eq!(sim.block_env.basefee, U256::from(50_000_000_000u64));
    }

    #[test]
    fn test_clone_simulator() {
        let mut sim = RevmSimulator::new();
        let address = Address::repeat_byte(0x42);

        sim.set_balance(address, U256::from(1000u64));

        let cloned = sim.clone();

        // Verify cloned simulator has same state
        assert_eq!(cloned.get_balance(address).unwrap(), U256::from(1000u64));
        assert_eq!(cloned.chain_id, sim.chain_id);
    }
}
