//! Forking database for REVM that fetches missing state from RPC.
//!
//! This module provides a database implementation that caches EVM state locally
//! while lazily fetching missing state from an Ethereum RPC provider. This enables
//! simulating transactions against the current chain state without needing to
//! pre-load all relevant state.

use alloy::eips::BlockId;
use alloy::primitives::{Address, B256, U256};
use alloy::providers::Provider;
use alloy::transports::Transport;
use parking_lot::RwLock;
use revm::db::{CacheDB, EmptyDB};
use revm::primitives::{AccountInfo, Bytecode, KECCAK_EMPTY};
use revm::Database;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::runtime::Handle;
use tracing::{debug, trace, warn};

/// Forking database that fetches missing state from an RPC provider.
///
/// This database wraps a provider and caches all state locally. When state is
/// requested that isn't in the cache, it fetches from the provider on-demand.
pub struct ForkDB<T, P>
where
    T: Transport + Clone,
    P: Provider<T>,
{
    /// The Ethereum provider for fetching state
    provider: Arc<P>,
    /// Cache of account information
    account_cache: RwLock<HashMap<Address, AccountInfo>>,
    /// Cache of storage values: (address, slot) -> value
    storage_cache: RwLock<HashMap<(Address, U256), U256>>,
    /// Cache of contract bytecode: code_hash -> bytecode
    code_cache: RwLock<HashMap<B256, Bytecode>>,
    /// Block number to fork from
    block_number: u64,
    /// Block ID for RPC calls
    block_id: BlockId,
    /// Tokio runtime handle for blocking calls
    runtime_handle: Handle,
    /// Phantom data for transport type
    _transport: std::marker::PhantomData<T>,
}

impl<T, P> ForkDB<T, P>
where
    T: Transport + Clone,
    P: Provider<T>,
{
    /// Create a new ForkDB from a provider at a specific block.
    pub fn new(provider: Arc<P>, block_number: u64) -> Self {
        Self {
            provider,
            account_cache: RwLock::new(HashMap::new()),
            storage_cache: RwLock::new(HashMap::new()),
            code_cache: RwLock::new(HashMap::new()),
            block_number,
            block_id: BlockId::number(block_number),
            runtime_handle: Handle::current(),
            _transport: std::marker::PhantomData,
        }
    }

    /// Create a new ForkDB at the latest block.
    pub async fn new_at_latest(provider: Arc<P>) -> Result<Self, String> {
        let block_number = provider
            .get_block_number()
            .await
            .map_err(|e| format!("Failed to get block number: {}", e))?;

        Ok(Self::new(provider, block_number))
    }

    /// Get the block number this fork is based on.
    pub fn block_number(&self) -> u64 {
        self.block_number
    }

    /// Prefetch account state for a set of addresses.
    ///
    /// This can improve performance by batching RPC calls for known addresses.
    pub async fn prefetch_accounts(&self, addresses: &[Address]) -> Result<(), String> {
        for address in addresses {
            self.fetch_account(*address).await?;
        }
        Ok(())
    }

    /// Prefetch storage slots for a contract.
    pub async fn prefetch_storage(
        &self,
        address: Address,
        slots: &[U256],
    ) -> Result<(), String> {
        for slot in slots {
            self.fetch_storage(address, *slot).await?;
        }
        Ok(())
    }

    /// Manually insert account info into the cache.
    pub fn insert_account(&self, address: Address, info: AccountInfo) {
        self.account_cache.write().insert(address, info);
    }

    /// Manually insert storage value into the cache.
    pub fn insert_storage(&self, address: Address, slot: U256, value: U256) {
        self.storage_cache.write().insert((address, slot), value);
    }

    /// Manually insert bytecode into the cache.
    pub fn insert_code(&self, code_hash: B256, bytecode: Bytecode) {
        self.code_cache.write().insert(code_hash, bytecode);
    }

    /// Clear all caches.
    pub fn clear_cache(&self) {
        self.account_cache.write().clear();
        self.storage_cache.write().clear();
        self.code_cache.write().clear();
    }

    /// Get cache statistics.
    pub fn cache_stats(&self) -> CacheStats {
        CacheStats {
            accounts: self.account_cache.read().len(),
            storage_slots: self.storage_cache.read().len(),
            bytecodes: self.code_cache.read().len(),
        }
    }

    /// Fetch account info from the provider.
    async fn fetch_account(&self, address: Address) -> Result<AccountInfo, String> {
        // Check cache first
        if let Some(info) = self.account_cache.read().get(&address) {
            return Ok(info.clone());
        }

        debug!(address = %address, "Fetching account from RPC");

        // Fetch balance
        let balance = self
            .provider
            .get_balance(address)
            .block_id(self.block_id)
            .await
            .map_err(|e| format!("Failed to get balance: {}", e))?;

        // Fetch nonce
        let nonce = self
            .provider
            .get_transaction_count(address)
            .block_id(self.block_id)
            .await
            .map_err(|e| format!("Failed to get nonce: {}", e))?;

        // Fetch code
        let code = self
            .provider
            .get_code_at(address)
            .block_id(self.block_id)
            .await
            .map_err(|e| format!("Failed to get code: {}", e))?;

        let (code_hash, bytecode) = if code.is_empty() {
            (KECCAK_EMPTY, Bytecode::default())
        } else {
            let bytecode = Bytecode::new_raw(revm::primitives::Bytes::from(code.to_vec()));
            let hash = bytecode.hash_slow();
            (hash, bytecode)
        };

        // Cache the bytecode
        if !code.is_empty() {
            self.code_cache.write().insert(code_hash, bytecode.clone());
        }

        let info = AccountInfo {
            balance,
            nonce,
            code_hash,
            code: Some(bytecode),
        };

        // Cache the account info
        self.account_cache.write().insert(address, info.clone());

        trace!(
            address = %address,
            balance = %balance,
            nonce = nonce,
            has_code = !code.is_empty(),
            "Fetched account info"
        );

        Ok(info)
    }

    /// Fetch storage value from the provider.
    async fn fetch_storage(&self, address: Address, slot: U256) -> Result<U256, String> {
        let key = (address, slot);

        // Check cache first
        if let Some(value) = self.storage_cache.read().get(&key) {
            return Ok(*value);
        }

        debug!(address = %address, slot = %slot, "Fetching storage from RPC");

        let value = self
            .provider
            .get_storage_at(address, slot)
            .block_id(self.block_id)
            .await
            .map_err(|e| format!("Failed to get storage: {}", e))?;

        // Cache the storage value
        self.storage_cache.write().insert(key, value);

        trace!(
            address = %address,
            slot = %slot,
            value = %value,
            "Fetched storage value"
        );

        Ok(value)
    }

    /// Fetch bytecode from the provider.
    async fn fetch_code(&self, code_hash: B256) -> Result<Bytecode, String> {
        // Check cache first
        if let Some(bytecode) = self.code_cache.read().get(&code_hash) {
            return Ok(bytecode.clone());
        }

        // If we need the code by hash, we should have it from the account fetch
        // This is a fallback for edge cases
        warn!(code_hash = %code_hash, "Code requested by hash but not in cache");

        // Return empty bytecode as fallback
        Ok(Bytecode::default())
    }

    /// Blocking wrapper for fetch_account.
    fn fetch_account_blocking(&self, address: Address) -> Result<AccountInfo, String> {
        self.runtime_handle
            .block_on(async { self.fetch_account(address).await })
    }

    /// Blocking wrapper for fetch_storage.
    fn fetch_storage_blocking(&self, address: Address, slot: U256) -> Result<U256, String> {
        self.runtime_handle
            .block_on(async { self.fetch_storage(address, slot).await })
    }

    /// Blocking wrapper for fetch_code.
    fn fetch_code_blocking(&self, code_hash: B256) -> Result<Bytecode, String> {
        self.runtime_handle
            .block_on(async { self.fetch_code(code_hash).await })
    }
}

/// Statistics about the cache state.
#[derive(Debug, Clone)]
pub struct CacheStats {
    /// Number of cached accounts
    pub accounts: usize,
    /// Number of cached storage slots
    pub storage_slots: usize,
    /// Number of cached bytecodes
    pub bytecodes: usize,
}

/// Error type for ForkDB operations.
#[derive(Debug)]
pub struct ForkDBError(pub String);

impl std::fmt::Display for ForkDBError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ForkDB error: {}", self.0)
    }
}

impl std::error::Error for ForkDBError {}

impl<T, P> Database for ForkDB<T, P>
where
    T: Transport + Clone,
    P: Provider<T>,
{
    type Error = ForkDBError;

    fn basic(&mut self, address: Address) -> Result<Option<AccountInfo>, Self::Error> {
        match self.fetch_account_blocking(address) {
            Ok(info) => Ok(Some(info)),
            Err(e) => Err(ForkDBError(e)),
        }
    }

    fn code_by_hash(&mut self, code_hash: B256) -> Result<Bytecode, Self::Error> {
        match self.fetch_code_blocking(code_hash) {
            Ok(bytecode) => Ok(bytecode),
            Err(e) => Err(ForkDBError(e)),
        }
    }

    fn storage(&mut self, address: Address, slot: U256) -> Result<U256, Self::Error> {
        match self.fetch_storage_blocking(address, slot) {
            Ok(value) => Ok(value),
            Err(e) => Err(ForkDBError(e)),
        }
    }

    fn block_hash(&mut self, number: u64) -> Result<B256, Self::Error> {
        // For simplicity, we don't cache block hashes
        // In production, you might want to fetch this from the provider
        if number >= self.block_number {
            return Ok(B256::ZERO);
        }

        // Return a deterministic hash based on block number
        // In production, fetch from provider
        let mut hash = B256::ZERO;
        hash.0[0..8].copy_from_slice(&number.to_be_bytes());
        Ok(hash)
    }
}

/// A wrapper that provides a cloneable ForkDB backed by CacheDB.
///
/// This is useful for parallel simulations where each thread needs its own
/// database instance but shares the underlying cached state.
pub struct SharedForkDB<T, P>
where
    T: Transport + Clone,
    P: Provider<T>,
{
    /// The underlying fork database
    fork_db: Arc<RwLock<ForkDB<T, P>>>,
    /// Local cache that can be modified without affecting shared state
    local_cache: CacheDB<EmptyDB>,
}

impl<T, P> SharedForkDB<T, P>
where
    T: Transport + Clone,
    P: Provider<T>,
{
    /// Create a new shared fork database.
    pub fn new(fork_db: ForkDB<T, P>) -> Self {
        Self {
            fork_db: Arc::new(RwLock::new(fork_db)),
            local_cache: CacheDB::new(EmptyDB::default()),
        }
    }

    /// Create a new instance that shares the underlying fork database.
    pub fn clone_shared(&self) -> Self {
        Self {
            fork_db: Arc::clone(&self.fork_db),
            local_cache: CacheDB::new(EmptyDB::default()),
        }
    }

    /// Get cache statistics from the shared fork database.
    pub fn cache_stats(&self) -> CacheStats {
        self.fork_db.read().cache_stats()
    }

    /// Prefetch accounts into the shared cache.
    pub async fn prefetch_accounts(&self, addresses: &[Address]) -> Result<(), String> {
        self.fork_db.read().prefetch_accounts(addresses).await
    }

    /// Clear local cache only.
    pub fn clear_local_cache(&mut self) {
        self.local_cache = CacheDB::new(EmptyDB::default());
    }
}

impl<T, P> Database for SharedForkDB<T, P>
where
    T: Transport + Clone,
    P: Provider<T>,
{
    type Error = ForkDBError;

    fn basic(&mut self, address: Address) -> Result<Option<AccountInfo>, Self::Error> {
        // Check local cache first
        if let Some(account) = self.local_cache.accounts.get(&address) {
            return Ok(Some(account.info.clone()));
        }

        // Fall back to shared fork database
        self.fork_db.write().basic(address)
    }

    fn code_by_hash(&mut self, code_hash: B256) -> Result<Bytecode, Self::Error> {
        // Check local cache first
        if let Some(code) = self.local_cache.contracts.get(&code_hash) {
            return Ok(code.clone());
        }

        // Fall back to shared fork database
        self.fork_db.write().code_by_hash(code_hash)
    }

    fn storage(&mut self, address: Address, slot: U256) -> Result<U256, Self::Error> {
        // Check local cache first
        if let Some(account) = self.local_cache.accounts.get(&address) {
            if let Some(value) = account.storage.get(&slot) {
                return Ok(*value);
            }
        }

        // Fall back to shared fork database
        self.fork_db.write().storage(address, slot)
    }

    fn block_hash(&mut self, number: u64) -> Result<B256, Self::Error> {
        self.fork_db.write().block_hash(number)
    }
}

impl<T, P> Clone for SharedForkDB<T, P>
where
    T: Transport + Clone,
    P: Provider<T>,
{
    fn clone(&self) -> Self {
        self.clone_shared()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cache_stats_default() {
        let stats = CacheStats {
            accounts: 0,
            storage_slots: 0,
            bytecodes: 0,
        };

        assert_eq!(stats.accounts, 0);
        assert_eq!(stats.storage_slots, 0);
        assert_eq!(stats.bytecodes, 0);
    }

    #[test]
    fn test_fork_db_error_display() {
        let error = ForkDBError("test error".to_string());
        assert_eq!(format!("{}", error), "ForkDB error: test error");
    }
}
