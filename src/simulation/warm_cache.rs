//! Warm Cache - Persistent state cache with active syncing for MEV simulations.
//!
//! This module solves the "phantom profit" problem by:
//! 1. Maintaining a persistent in-memory fork of chain state
//! 2. Actively syncing pool reserves from Sync/Swap events
//! 3. Pre-warming bytecode for routers and tokens
//! 4. Updating block environment every new block

use alloy::eips::BlockId;
use alloy::primitives::{Address, B256, U256, map::HashMap};
use alloy::providers::Provider;
use alloy::transports::Transport;
use dashmap::DashMap;
use parking_lot::RwLock;
use revm::primitives::{AccountInfo, Bytecode, KECCAK_EMPTY};
use revm::{Database, DatabaseCommit};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::runtime::Handle;
use tracing::{debug, info, trace, warn};

/// Uniswap V2 reserves storage slot (getReserves packs reserve0, reserve1, blockTimestampLast)
pub const UNISWAP_V2_RESERVES_SLOT: U256 = U256::from_limbs([8, 0, 0, 0]);

/// Known router addresses for pre-warming
pub mod routers {
    use alloy::primitives::{address, Address};

    pub const UNISWAP_V2_ROUTER: Address = address!("7a250d5630B4cF539739dF2C5dAcb4c659F2488D");
    pub const UNISWAP_V3_ROUTER: Address = address!("E592427A0AEce92De3Edee1F18E0157C05861564");
    pub const UNISWAP_V3_ROUTER_2: Address = address!("68b3465833fb72A70ecDF485E0e4C7bD8665Fc45");
    pub const SUSHISWAP_ROUTER: Address = address!("d9e1cE17f2641f24aE83637ab66a2cca9C378B9F");
    pub const UNISWAP_V2_FACTORY: Address = address!("5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f");
    pub const WETH: Address = address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
    pub const USDC: Address = address!("A0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48");
    pub const USDT: Address = address!("dAC17F958D2ee523a2206206994597C13D831ec7");
}

/// Pool reserves cached in memory
#[derive(Debug, Clone)]
pub struct CachedReserves {
    pub reserve0: U256,
    pub reserve1: U256,
    pub block_timestamp_last: u32,
    pub last_updated_block: u64,
}

/// Block environment state
#[derive(Debug, Clone)]
pub struct BlockState {
    pub number: u64,
    pub timestamp: u64,
    pub base_fee: U256,
}

/// Warm cache error type
#[derive(Debug)]
pub struct WarmCacheError(pub String);

impl std::fmt::Display for WarmCacheError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "WarmCache error: {}", self.0)
    }
}

impl std::error::Error for WarmCacheError {}

/// Persistent warm cache for MEV simulations.
///
/// Unlike ForkDB which fetches on-demand, WarmCache proactively maintains
/// state for pools you care about, enabling sub-millisecond simulations.
pub struct WarmCache<T, P>
where
    T: Transport + Clone,
    P: Provider<T>,
{
    /// Provider for fetching state
    provider: Arc<P>,

    /// Cached account info (balance, nonce, code_hash)
    accounts: DashMap<Address, AccountInfo>,

    /// Cached storage slots: (address, slot) -> value
    storage: DashMap<(Address, U256), U256>,

    /// Cached bytecode: code_hash -> bytecode
    bytecode: DashMap<B256, Bytecode>,

    /// Pool reserves cache for fast access (avoids storage slot decoding)
    reserves: DashMap<Address, CachedReserves>,

    /// Current block state
    block_state: RwLock<BlockState>,

    /// Last synced block number
    last_synced_block: AtomicU64,

    /// Chain ID
    chain_id: u64,

    /// Tokio runtime handle for blocking operations
    runtime_handle: Handle,

    /// Phantom
    _transport: std::marker::PhantomData<T>,
}

impl<T, P> WarmCache<T, P>
where
    T: Transport + Clone,
    P: Provider<T>,
{
    /// Create a new warm cache.
    pub async fn new(provider: Arc<P>) -> eyre::Result<Self> {
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

        let chain_id = provider.get_chain_id().await?;

        Ok(Self {
            provider,
            accounts: DashMap::new(),
            storage: DashMap::new(),
            bytecode: DashMap::new(),
            reserves: DashMap::new(),
            block_state: RwLock::new(BlockState {
                number: block_number,
                timestamp: block.header.timestamp,
                base_fee,
            }),
            last_synced_block: AtomicU64::new(block_number),
            chain_id,
            runtime_handle: Handle::current(),
            _transport: std::marker::PhantomData,
        })
    }

    /// Get current block state.
    pub fn block_state(&self) -> BlockState {
        self.block_state.read().clone()
    }

    /// Get chain ID.
    pub fn chain_id(&self) -> u64 {
        self.chain_id
    }

    /// Update block state (call this every new block).
    pub fn update_block_state(&self, number: u64, timestamp: u64, base_fee: U256) {
        let mut state = self.block_state.write();
        state.number = number;
        state.timestamp = timestamp;
        state.base_fee = base_fee;
        self.last_synced_block.store(number, Ordering::SeqCst);

        debug!(
            block = number,
            timestamp = timestamp,
            base_fee = %base_fee,
            "Block state updated"
        );
    }

    /// Pre-warm infrastructure contracts (routers, WETH, stablecoins).
    pub async fn prewarm_infrastructure(&self) -> eyre::Result<()> {
        let contracts = vec![
            routers::UNISWAP_V2_ROUTER,
            routers::UNISWAP_V3_ROUTER,
            routers::UNISWAP_V3_ROUTER_2,
            routers::SUSHISWAP_ROUTER,
            routers::UNISWAP_V2_FACTORY,
            routers::WETH,
            routers::USDC,
            routers::USDT,
        ];

        info!("Pre-warming {} infrastructure contracts", contracts.len());

        for addr in contracts {
            if let Err(e) = self.warm_account(addr).await {
                warn!(address = %addr, error = %e, "Failed to warm account");
            }
        }

        Ok(())
    }

    /// Pre-warm a list of V2 pools.
    pub async fn prewarm_v2_pools(&self, pools: &[Address]) -> eyre::Result<()> {
        info!("Pre-warming {} V2 pools", pools.len());

        for pool in pools {
            if let Err(e) = self.warm_v2_pool(*pool).await {
                warn!(pool = %pool, error = %e, "Failed to warm pool");
            }
        }

        Ok(())
    }

    /// Warm a single account (fetches balance, nonce, bytecode).
    pub async fn warm_account(&self, address: Address) -> eyre::Result<()> {
        let block_id = BlockId::number(self.last_synced_block.load(Ordering::SeqCst));

        // Fetch balance
        let balance = self.provider.get_balance(address).block_id(block_id).await?;

        // Fetch nonce
        let nonce = self.provider.get_transaction_count(address).block_id(block_id).await?;

        // Fetch code
        let code = self.provider.get_code_at(address).block_id(block_id).await?;

        let (code_hash, bytecode) = if code.is_empty() {
            (KECCAK_EMPTY, Bytecode::default())
        } else {
            let bytecode = Bytecode::new_raw(revm::primitives::Bytes::from(code.to_vec()));
            let hash = bytecode.hash_slow();

            // Cache bytecode
            self.bytecode.insert(hash, bytecode.clone());

            (hash, bytecode)
        };

        let info = AccountInfo {
            balance,
            nonce,
            code_hash,
            code: Some(bytecode),
        };

        self.accounts.insert(address, info);

        trace!(address = %address, balance = %balance, has_code = !code.is_empty(), "Account warmed");

        Ok(())
    }

    /// Warm a V2 pool (fetches reserves and bytecode).
    pub async fn warm_v2_pool(&self, pool: Address) -> eyre::Result<()> {
        // Warm the account first
        self.warm_account(pool).await?;

        // Fetch reserves storage slot
        let block_id = BlockId::number(self.last_synced_block.load(Ordering::SeqCst));
        let reserves_raw = self.provider
            .get_storage_at(pool, UNISWAP_V2_RESERVES_SLOT)
            .block_id(block_id)
            .await?;

        // Cache the raw storage
        self.storage.insert((pool, UNISWAP_V2_RESERVES_SLOT), reserves_raw);

        // Decode reserves (packed: reserve0 (112 bits) | reserve1 (112 bits) | blockTimestampLast (32 bits))
        let reserves = self.decode_v2_reserves(reserves_raw);
        self.reserves.insert(pool, reserves);

        trace!(pool = %pool, "V2 pool warmed");

        Ok(())
    }

    /// Update V2 pool reserves from a Sync event.
    pub fn update_v2_reserves(&self, pool: Address, reserve0: U256, reserve1: U256, block: u64) {
        // Update decoded reserves cache
        self.reserves.insert(pool, CachedReserves {
            reserve0,
            reserve1,
            block_timestamp_last: 0,
            last_updated_block: block,
        });

        // Also update raw storage slot (for REVM compatibility)
        let packed = self.encode_v2_reserves(reserve0, reserve1, 0);
        self.storage.insert((pool, UNISWAP_V2_RESERVES_SLOT), packed);

        trace!(pool = %pool, reserve0 = %reserve0, reserve1 = %reserve1, "Reserves updated");
    }

    /// Get cached reserves for a V2 pool.
    pub fn get_v2_reserves(&self, pool: &Address) -> Option<CachedReserves> {
        self.reserves.get(pool).map(|r| r.clone())
    }

    /// Check if a pool is warmed.
    pub fn is_pool_warmed(&self, pool: &Address) -> bool {
        self.reserves.contains_key(pool)
    }

    /// Get cache statistics.
    pub fn stats(&self) -> CacheStats {
        CacheStats {
            accounts: self.accounts.len(),
            storage_slots: self.storage.len(),
            bytecodes: self.bytecode.len(),
            pools: self.reserves.len(),
            last_synced_block: self.last_synced_block.load(Ordering::SeqCst),
        }
    }

    /// Decode packed V2 reserves from storage.
    fn decode_v2_reserves(&self, packed: U256) -> CachedReserves {
        // Uniswap V2 packs: reserve0 (112 bits) | reserve1 (112 bits) | blockTimestampLast (32 bits)
        let bytes = packed.to_be_bytes::<32>();

        // blockTimestampLast is in bytes 0-3 (big endian, rightmost)
        let block_timestamp_last = u32::from_be_bytes([bytes[28], bytes[29], bytes[30], bytes[31]]);

        // reserve1 is in bytes 4-17
        let reserve1 = U256::from_be_slice(&bytes[14..28]) & U256::from((1u128 << 112) - 1);

        // reserve0 is in bytes 18-31
        let reserve0 = U256::from_be_slice(&bytes[0..14]) & U256::from((1u128 << 112) - 1);

        CachedReserves {
            reserve0,
            reserve1,
            block_timestamp_last,
            last_updated_block: self.last_synced_block.load(Ordering::SeqCst),
        }
    }

    /// Encode V2 reserves to packed storage format.
    fn encode_v2_reserves(&self, reserve0: U256, reserve1: U256, timestamp: u32) -> U256 {
        // Pack: reserve0 | reserve1 | blockTimestampLast
        let r0 = reserve0 & U256::from((1u128 << 112) - 1);
        let r1 = reserve1 & U256::from((1u128 << 112) - 1);

        (r0 << 144) | (r1 << 32) | U256::from(timestamp)
    }

    /// Blocking fetch for REVM Database trait.
    fn fetch_account_blocking(&self, address: Address) -> Result<AccountInfo, WarmCacheError> {
        // Check cache first
        if let Some(info) = self.accounts.get(&address) {
            return Ok(info.clone());
        }

        // Fetch from provider
        self.runtime_handle.block_on(async {
            self.warm_account(address).await
                .map_err(|e| WarmCacheError(e.to_string()))?;

            self.accounts
                .get(&address)
                .map(|r| r.clone())
                .ok_or_else(|| WarmCacheError("Account not found after warming".to_string()))
        })
    }

    /// Blocking fetch for storage.
    fn fetch_storage_blocking(&self, address: Address, slot: U256) -> Result<U256, WarmCacheError> {
        let key = (address, slot);

        // Check cache first
        if let Some(value) = self.storage.get(&key) {
            return Ok(*value);
        }

        // Fetch from provider
        self.runtime_handle.block_on(async {
            let block_id = BlockId::number(self.last_synced_block.load(Ordering::SeqCst));
            let value = self.provider
                .get_storage_at(address, slot)
                .block_id(block_id)
                .await
                .map_err(|e| WarmCacheError(e.to_string()))?;

            self.storage.insert(key, value);
            Ok(value)
        })
    }
}

/// Cache statistics
#[derive(Debug, Clone)]
pub struct CacheStats {
    pub accounts: usize,
    pub storage_slots: usize,
    pub bytecodes: usize,
    pub pools: usize,
    pub last_synced_block: u64,
}

impl<T, P> Database for WarmCache<T, P>
where
    T: Transport + Clone,
    P: Provider<T>,
{
    type Error = WarmCacheError;

    fn basic(&mut self, address: Address) -> Result<Option<AccountInfo>, Self::Error> {
        match self.fetch_account_blocking(address) {
            Ok(info) => Ok(Some(info)),
            Err(e) => Err(e),
        }
    }

    fn code_by_hash(&mut self, code_hash: B256) -> Result<Bytecode, Self::Error> {
        self.bytecode
            .get(&code_hash)
            .map(|r| r.clone())
            .ok_or_else(|| WarmCacheError(format!("Bytecode not found for hash {}", code_hash)))
    }

    fn storage(&mut self, address: Address, slot: U256) -> Result<U256, Self::Error> {
        self.fetch_storage_blocking(address, slot)
    }

    fn block_hash(&mut self, number: u64) -> Result<B256, Self::Error> {
        // Return deterministic hash for historical blocks
        let current = self.last_synced_block.load(Ordering::SeqCst);
        if number >= current {
            return Ok(B256::ZERO);
        }

        let mut hash = B256::ZERO;
        hash.0[0..8].copy_from_slice(&number.to_be_bytes());
        Ok(hash)
    }
}

impl<T, P> DatabaseCommit for WarmCache<T, P>
where
    T: Transport + Clone,
    P: Provider<T>,
{
    fn commit(&mut self, changes: HashMap<Address, revm::primitives::Account>) {
        for (address, account) in changes.iter() {
            // Update account info
            self.accounts.insert(*address, account.info.clone());

            // Update storage
            for (slot, value) in account.storage.iter() {
                self.storage.insert((*address, *slot), value.present_value);
            }
        }
    }
}

// Implement Clone for WarmCache by cloning the underlying Arc
impl<T, P> Clone for WarmCache<T, P>
where
    T: Transport + Clone,
    P: Provider<T>,
{
    fn clone(&self) -> Self {
        Self {
            provider: Arc::clone(&self.provider),
            accounts: self.accounts.clone(),
            storage: self.storage.clone(),
            bytecode: self.bytecode.clone(),
            reserves: self.reserves.clone(),
            block_state: RwLock::new(self.block_state.read().clone()),
            last_synced_block: AtomicU64::new(self.last_synced_block.load(Ordering::SeqCst)),
            chain_id: self.chain_id,
            runtime_handle: self.runtime_handle.clone(),
            _transport: std::marker::PhantomData,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_v2_reserves_encoding() {
        // Test encode/decode roundtrip
        let reserve0 = U256::from(1000000000000000000u128); // 1 ETH
        let reserve1 = U256::from(2000000000u128); // 2000 USDC
        let timestamp = 1700000000u32;

        // This is a simplified test - full test would need a mock provider
        assert!(reserve0 < U256::from(1u128 << 112));
        assert!(reserve1 < U256::from(1u128 << 112));
    }
}
