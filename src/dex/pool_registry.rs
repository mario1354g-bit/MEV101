//! Pool discovery and caching for DEX pools
//!
//! This module provides functionality to discover, cache, and query DEX pools
//! across multiple protocols.

use super::{DexError, DexResult};
use alloy::primitives::{Address, U256};
use alloy::providers::Provider;
use alloy::sol;
use alloy::transports::Transport;
use dashmap::DashMap;
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{BufReader, BufWriter};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use tracing::{debug, info, warn};

/// Extension trait for Vec to add elements only if not present
trait DedupPush<T> {
    /// Push an element only if it's not already in the vector
    fn dedup_push(&mut self, item: T);
}

impl<T: PartialEq> DedupPush<T> for Vec<T> {
    fn dedup_push(&mut self, item: T) {
        if !self.contains(&item) {
            self.push(item);
        }
    }
}

// Factory interfaces for pool discovery
sol! {
    #[sol(rpc)]
    interface IUniswapV2Factory {
        function getPair(address tokenA, address tokenB) external view returns (address pair);
        function allPairs(uint256) external view returns (address pair);
        function allPairsLength() external view returns (uint256);
    }

    #[sol(rpc)]
    interface IUniswapV3Factory {
        function getPool(address tokenA, address tokenB, uint24 fee) external view returns (address pool);
    }

    #[sol(rpc)]
    interface IUniswapV2Pair {
        function token0() external view returns (address);
        function token1() external view returns (address);
        function getReserves() external view returns (uint112 reserve0, uint112 reserve1, uint32 blockTimestampLast);
    }

    #[sol(rpc)]
    interface IUniswapV3Pool {
        function token0() external view returns (address);
        function token1() external view returns (address);
        function fee() external view returns (uint24);
        function liquidity() external view returns (uint128);
    }
}

/// Type of DEX pool
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PoolType {
    /// Uniswap V2 style constant product AMM
    UniswapV2,
    /// SushiSwap (Uniswap V2 fork)
    SushiSwap,
    /// Uniswap V3 concentrated liquidity
    UniswapV3,
    /// Other DEX types
    Other(u8),
}

impl PoolType {
    /// Check if this is a V2-style pool
    pub fn is_v2_style(&self) -> bool {
        matches!(self, PoolType::UniswapV2 | PoolType::SushiSwap)
    }

    /// Check if this is a V3-style pool
    pub fn is_v3_style(&self) -> bool {
        matches!(self, PoolType::UniswapV3)
    }
}

/// Information about a DEX pool
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PoolInfo {
    /// Pool address
    pub address: Address,
    /// Token0 address
    pub token0: Address,
    /// Token1 address
    pub token1: Address,
    /// Pool type
    pub pool_type: PoolType,
    /// Fee tier (in basis points for V3, or 30 for V2)
    pub fee: u32,
    /// Factory address that created this pool
    pub factory: Address,
    /// Block number when pool was created (if known)
    pub created_block: Option<u64>,
    /// Last known liquidity (optional)
    pub liquidity: Option<U256>,
    /// Whether this pool is active (has liquidity)
    pub is_active: bool,
}

impl PoolInfo {
    /// Create a new pool info for a V2-style pool
    pub fn new_v2(address: Address, token0: Address, token1: Address, factory: Address) -> Self {
        Self {
            address,
            token0,
            token1,
            pool_type: PoolType::UniswapV2,
            fee: 30, // 0.3%
            factory,
            created_block: None,
            liquidity: None,
            is_active: true,
        }
    }

    /// Create a new pool info for a V3-style pool
    pub fn new_v3(
        address: Address,
        token0: Address,
        token1: Address,
        fee: u32,
        factory: Address,
    ) -> Self {
        Self {
            address,
            token0,
            token1,
            pool_type: PoolType::UniswapV3,
            fee,
            factory,
            created_block: None,
            liquidity: None,
            is_active: true,
        }
    }

    /// Get the token pair as a tuple (sorted by address)
    pub fn token_pair(&self) -> (Address, Address) {
        if self.token0 < self.token1 {
            (self.token0, self.token1)
        } else {
            (self.token1, self.token0)
        }
    }

    /// Check if this pool contains a specific token
    pub fn contains_token(&self, token: &Address) -> bool {
        self.token0 == *token || self.token1 == *token
    }

    /// Get the other token in the pair
    pub fn other_token(&self, token: &Address) -> Option<Address> {
        if self.token0 == *token {
            Some(self.token1)
        } else if self.token1 == *token {
            Some(self.token0)
        } else {
            None
        }
    }
}

/// Registry for caching and querying DEX pools
#[derive(Debug)]
pub struct PoolRegistry {
    /// All known pools indexed by address
    pools: DashMap<Address, PoolInfo>,
    /// Pools indexed by token pair (both directions)
    pairs: DashMap<(Address, Address), Vec<Address>>,
    /// Pools indexed by single token
    token_pools: DashMap<Address, Vec<Address>>,
    /// Number of pools discovered
    pool_count: AtomicU64,
}

impl Default for PoolRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl PoolRegistry {
    /// Create a new empty pool registry
    pub fn new() -> Self {
        Self {
            pools: DashMap::new(),
            pairs: DashMap::new(),
            token_pools: DashMap::new(),
            pool_count: AtomicU64::new(0),
        }
    }

    /// Get the number of pools in the registry
    pub fn len(&self) -> usize {
        self.pool_count.load(Ordering::Relaxed) as usize
    }

    /// Check if the registry is empty
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Add a pool to the registry
    /// Uses atomic operations to prevent race conditions
    /// and avoid duplicate entries in index vectors
    pub fn add_pool(&self, pool: PoolInfo) {
        let address = pool.address;
        let token0 = pool.token0;
        let token1 = pool.token1;

        // First check if pool already exists to avoid duplicate index entries
        let already_exists = self.pools.contains_key(&address);

        // Insert or update the pool data
        self.pools.insert(address, pool);

        // Only update indexes and count if this is a new pool
        if !already_exists {
            // Atomically increment pool count - use SeqCst for proper ordering
            self.pool_count.fetch_add(1, Ordering::SeqCst);

            // Add to pair index (both directions), avoiding duplicates
            self.pairs
                .entry((token0, token1))
                .or_default()
                .dedup_push(address);

            if token0 != token1 {
                self.pairs
                    .entry((token1, token0))
                    .or_default()
                    .dedup_push(address);
            }

            // Add to single token index, avoiding duplicates
            self.token_pools
                .entry(token0)
                .or_default()
                .dedup_push(address);

            if token0 != token1 {
                self.token_pools
                    .entry(token1)
                    .or_default()
                    .dedup_push(address);
            }
        }
    }

    /// Add a pool only if it doesn't already exist
    /// Returns true if the pool was added, false if it already existed
    pub fn try_add_pool(&self, pool: PoolInfo) -> bool {
        let address = pool.address;

        // Check if pool already exists
        if self.pools.contains_key(&address) {
            return false;
        }

        // Add the pool
        self.add_pool(pool);
        true
    }

    /// Get a pool by address
    pub fn get_pool(&self, address: &Address) -> Option<PoolInfo> {
        self.pools.get(address).map(|r| r.clone())
    }

    /// Get all pools for a token pair
    pub fn get_pools_for_pair(&self, token0: Address, token1: Address) -> Vec<PoolInfo> {
        self.pairs
            .get(&(token0, token1))
            .map(|addresses| {
                addresses
                    .iter()
                    .filter_map(|addr| self.pools.get(addr).map(|r| r.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Get all pools containing a specific token
    pub fn get_pools_for_token(&self, token: &Address) -> Vec<PoolInfo> {
        self.token_pools
            .get(token)
            .map(|addresses| {
                addresses
                    .iter()
                    .filter_map(|addr| self.pools.get(addr).map(|r| r.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Get all active pools
    pub fn get_active_pools(&self) -> Vec<PoolInfo> {
        self.pools
            .iter()
            .filter(|r| r.is_active)
            .map(|r| r.clone())
            .collect()
    }

    /// Get pools by type
    pub fn get_pools_by_type(&self, pool_type: PoolType) -> Vec<PoolInfo> {
        self.pools
            .iter()
            .filter(|r| r.pool_type == pool_type)
            .map(|r| r.clone())
            .collect()
    }

    /// Update pool liquidity
    pub fn update_liquidity(&self, address: &Address, liquidity: U256) {
        if let Some(mut pool) = self.pools.get_mut(address) {
            pool.liquidity = Some(liquidity);
            pool.is_active = !liquidity.is_zero();
        }
    }

    /// Mark a pool as active or inactive
    pub fn set_pool_active(&self, address: &Address, is_active: bool) {
        if let Some(mut pool) = self.pools.get_mut(address) {
            pool.is_active = is_active;
        }
    }

    /// Remove a pool from the registry
    pub fn remove_pool(&self, address: &Address) -> Option<PoolInfo> {
        if let Some((_, pool)) = self.pools.remove(address) {
            self.pool_count.fetch_sub(1, Ordering::Relaxed);

            // Remove from pair index
            if let Some(mut pools) = self.pairs.get_mut(&(pool.token0, pool.token1)) {
                pools.retain(|a| a != address);
            }
            if let Some(mut pools) = self.pairs.get_mut(&(pool.token1, pool.token0)) {
                pools.retain(|a| a != address);
            }

            // Remove from token index
            if let Some(mut pools) = self.token_pools.get_mut(&pool.token0) {
                pools.retain(|a| a != address);
            }
            if let Some(mut pools) = self.token_pools.get_mut(&pool.token1) {
                pools.retain(|a| a != address);
            }

            Some(pool)
        } else {
            None
        }
    }

    /// Discover V2-style pools from a factory
    pub async fn discover_v2_pools<T: Transport + Clone, P: Provider<T>>(
        &self,
        factory: Address,
        provider: &P,
        pool_type: PoolType,
        max_pools: Option<usize>,
    ) -> DexResult<usize> {
        let factory_contract = IUniswapV2Factory::new(factory, provider);

        // Get total number of pairs
        let total_pairs = factory_contract
            .allPairsLength()
            .call()
            .await
            .map_err(|e| DexError::ContractCall(e.to_string()))?
            ._0;

        let total_pairs: usize = total_pairs
            .try_into()
            .map_err(|_| DexError::InvalidParameters("Too many pairs".to_string()))?;

        info!(
            "Discovering {} V2 pools from factory {:?}",
            total_pairs, factory
        );

        let limit = max_pools.unwrap_or(total_pairs).min(total_pairs);
        let mut discovered = 0;

        for i in 0..limit {
            match self
                .discover_v2_pool_at_index(&factory_contract, factory, i, pool_type, provider)
                .await
            {
                Ok(_) => discovered += 1,
                Err(e) => {
                    warn!("Failed to discover pool at index {}: {}", i, e);
                }
            }

            if discovered % 100 == 0 && discovered > 0 {
                debug!("Discovered {} pools so far...", discovered);
            }
        }

        info!("Discovered {} V2 pools from factory {:?}", discovered, factory);
        Ok(discovered)
    }

    /// Discover a single V2 pool by index
    async fn discover_v2_pool_at_index<T: Transport + Clone, P: Provider<T>>(
        &self,
        factory: &IUniswapV2Factory::IUniswapV2FactoryInstance<T, &P>,
        factory_address: Address,
        index: usize,
        pool_type: PoolType,
        provider: &P,
    ) -> DexResult<PoolInfo> {
        let pair_address = factory
            .allPairs(U256::from(index))
            .call()
            .await
            .map_err(|e| DexError::ContractCall(e.to_string()))?
            .pair;

        if pair_address == Address::ZERO {
            return Err(DexError::PoolNotFound(format!("Pool at index {}", index)));
        }

        // Get token info
        let pair = IUniswapV2Pair::new(pair_address, provider);

        let token0 = pair
            .token0()
            .call()
            .await
            .map_err(|e| DexError::ContractCall(e.to_string()))?
            ._0;

        let token1 = pair
            .token1()
            .call()
            .await
            .map_err(|e| DexError::ContractCall(e.to_string()))?
            ._0;

        // Get reserves to check if pool is active
        let reserves = pair
            .getReserves()
            .call()
            .await
            .map_err(|e| DexError::ContractCall(e.to_string()))?;

        let liquidity = U256::from(reserves.reserve0) + U256::from(reserves.reserve1);
        let is_active = !liquidity.is_zero();

        let pool_info = PoolInfo {
            address: pair_address,
            token0,
            token1,
            pool_type,
            fee: 30,
            factory: factory_address,
            created_block: None,
            liquidity: Some(liquidity),
            is_active,
        };

        self.add_pool(pool_info.clone());
        Ok(pool_info)
    }

    /// Discover V3 pools for a token pair across all fee tiers
    pub async fn discover_v3_pools_for_pair<T: Transport + Clone, P: Provider<T>>(
        &self,
        factory: Address,
        token_a: Address,
        token_b: Address,
        provider: &P,
    ) -> DexResult<Vec<PoolInfo>> {
        use super::fee_tiers::ALL_FEE_TIERS;

        let factory_contract = IUniswapV3Factory::new(factory, provider);
        let mut discovered = Vec::new();

        for fee in ALL_FEE_TIERS {
            let pool_address = factory_contract
                .getPool(token_a, token_b, fee.try_into().expect("fee tier fits in u24"))
                .call()
                .await
                .map_err(|e| DexError::ContractCall(e.to_string()))?
                .pool;

            if pool_address != Address::ZERO {
                // Get pool info
                let pool_contract = IUniswapV3Pool::new(pool_address, provider);

                let token0 = pool_contract
                    .token0()
                    .call()
                    .await
                    .map_err(|e| DexError::ContractCall(e.to_string()))?
                    ._0;

                let token1 = pool_contract
                    .token1()
                    .call()
                    .await
                    .map_err(|e| DexError::ContractCall(e.to_string()))?
                    ._0;

                let liquidity = pool_contract
                    .liquidity()
                    .call()
                    .await
                    .map_err(|e| DexError::ContractCall(e.to_string()))?
                    ._0;

                let pool_info = PoolInfo {
                    address: pool_address,
                    token0,
                    token1,
                    pool_type: PoolType::UniswapV3,
                    fee,
                    factory,
                    created_block: None,
                    liquidity: Some(U256::from(liquidity)),
                    is_active: liquidity > 0,
                };

                self.add_pool(pool_info.clone());
                discovered.push(pool_info);

                debug!(
                    "Discovered V3 pool {:?} for {:?}/{:?} with fee {}",
                    pool_address, token_a, token_b, fee
                );
            }
        }

        Ok(discovered)
    }

    /// Load pools from a JSON file
    pub fn load_from_json<P: AsRef<Path>>(&self, path: P) -> DexResult<usize> {
        let file = File::open(path).map_err(DexError::Io)?;
        let reader = BufReader::new(file);
        let pools: Vec<PoolInfo> = serde_json::from_reader(reader)?;

        let count = pools.len();
        for pool in pools {
            self.add_pool(pool);
        }

        info!("Loaded {} pools from JSON file", count);
        Ok(count)
    }

    /// Save pools to a JSON file
    pub fn save_to_json<P: AsRef<Path>>(&self, path: P) -> DexResult<usize> {
        let pools: Vec<PoolInfo> = self.pools.iter().map(|r| r.clone()).collect();
        let count = pools.len();

        let file = File::create(path).map_err(DexError::Io)?;
        let writer = BufWriter::new(file);
        serde_json::to_writer_pretty(writer, &pools)?;

        info!("Saved {} pools to JSON file", count);
        Ok(count)
    }

    /// Clear all pools from the registry
    pub fn clear(&self) {
        self.pools.clear();
        self.pairs.clear();
        self.token_pools.clear();
        self.pool_count.store(0, Ordering::Relaxed);
    }

    /// Get statistics about the registry
    pub fn stats(&self) -> PoolRegistryStats {
        let total = self.len();
        let active = self.pools.iter().filter(|r| r.is_active).count();
        let v2_pools = self
            .pools
            .iter()
            .filter(|r| r.pool_type.is_v2_style())
            .count();
        let v3_pools = self
            .pools
            .iter()
            .filter(|r| r.pool_type.is_v3_style())
            .count();
        let unique_tokens = self.token_pools.len();
        let unique_pairs = self.pairs.len() / 2; // Divide by 2 because we store both directions

        PoolRegistryStats {
            total_pools: total,
            active_pools: active,
            v2_pools,
            v3_pools,
            unique_tokens,
            unique_pairs,
        }
    }
}

/// Statistics about the pool registry
#[derive(Debug, Clone)]
pub struct PoolRegistryStats {
    /// Total number of pools
    pub total_pools: usize,
    /// Number of active pools (with liquidity)
    pub active_pools: usize,
    /// Number of V2-style pools
    pub v2_pools: usize,
    /// Number of V3-style pools
    pub v3_pools: usize,
    /// Number of unique tokens
    pub unique_tokens: usize,
    /// Number of unique token pairs
    pub unique_pairs: usize,
}

/// Builder for creating pool registries with pre-loaded data
pub struct PoolRegistryBuilder {
    registry: PoolRegistry,
}

impl PoolRegistryBuilder {
    /// Create a new builder
    pub fn new() -> Self {
        Self {
            registry: PoolRegistry::new(),
        }
    }

    /// Load pools from a JSON file
    pub fn load_json<P: AsRef<Path>>(self, path: P) -> DexResult<Self> {
        self.registry.load_from_json(path)?;
        Ok(self)
    }

    /// Add pools manually
    pub fn add_pools(self, pools: Vec<PoolInfo>) -> Self {
        for pool in pools {
            self.registry.add_pool(pool);
        }
        self
    }

    /// Build the registry
    pub fn build(self) -> PoolRegistry {
        self.registry
    }
}

impl Default for PoolRegistryBuilder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_pool(id: u8) -> PoolInfo {
        PoolInfo {
            address: Address::repeat_byte(id),
            token0: Address::repeat_byte(id * 10),
            token1: Address::repeat_byte(id * 10 + 1),
            pool_type: PoolType::UniswapV2,
            fee: 30,
            factory: Address::repeat_byte(0xFF),
            created_block: Some(12345678),
            liquidity: Some(U256::from(1000000u64)),
            is_active: true,
        }
    }

    #[test]
    fn test_new_registry() {
        let registry = PoolRegistry::new();
        assert!(registry.is_empty());
        assert_eq!(registry.len(), 0);
    }

    #[test]
    fn test_add_and_get_pool() {
        let registry = PoolRegistry::new();
        let pool = create_test_pool(1);

        registry.add_pool(pool.clone());

        assert_eq!(registry.len(), 1);

        let retrieved = registry.get_pool(&pool.address).unwrap();
        assert_eq!(retrieved.address, pool.address);
        assert_eq!(retrieved.token0, pool.token0);
        assert_eq!(retrieved.token1, pool.token1);
    }

    #[test]
    fn test_get_pools_for_pair() {
        let registry = PoolRegistry::new();

        let token0 = Address::repeat_byte(10);
        let token1 = Address::repeat_byte(11);

        let pool1 = PoolInfo {
            address: Address::repeat_byte(1),
            token0,
            token1,
            pool_type: PoolType::UniswapV2,
            fee: 30,
            factory: Address::repeat_byte(0xFF),
            created_block: None,
            liquidity: None,
            is_active: true,
        };

        let pool2 = PoolInfo {
            address: Address::repeat_byte(2),
            token0,
            token1,
            pool_type: PoolType::UniswapV3,
            fee: 3000,
            factory: Address::repeat_byte(0xFE),
            created_block: None,
            liquidity: None,
            is_active: true,
        };

        registry.add_pool(pool1);
        registry.add_pool(pool2);

        // Query in original order
        let pools = registry.get_pools_for_pair(token0, token1);
        assert_eq!(pools.len(), 2);

        // Query in reverse order
        let pools_reverse = registry.get_pools_for_pair(token1, token0);
        assert_eq!(pools_reverse.len(), 2);
    }

    #[test]
    fn test_get_pools_for_token() {
        let registry = PoolRegistry::new();

        let common_token = Address::repeat_byte(10);

        let pool1 = PoolInfo {
            address: Address::repeat_byte(1),
            token0: common_token,
            token1: Address::repeat_byte(11),
            pool_type: PoolType::UniswapV2,
            fee: 30,
            factory: Address::repeat_byte(0xFF),
            created_block: None,
            liquidity: None,
            is_active: true,
        };

        let pool2 = PoolInfo {
            address: Address::repeat_byte(2),
            token0: Address::repeat_byte(12),
            token1: common_token,
            pool_type: PoolType::UniswapV2,
            fee: 30,
            factory: Address::repeat_byte(0xFF),
            created_block: None,
            liquidity: None,
            is_active: true,
        };

        registry.add_pool(pool1);
        registry.add_pool(pool2);

        let pools = registry.get_pools_for_token(&common_token);
        assert_eq!(pools.len(), 2);
    }

    #[test]
    fn test_remove_pool() {
        let registry = PoolRegistry::new();
        let pool = create_test_pool(1);
        let address = pool.address;

        registry.add_pool(pool);
        assert_eq!(registry.len(), 1);

        let removed = registry.remove_pool(&address);
        assert!(removed.is_some());
        assert_eq!(registry.len(), 0);
        assert!(registry.get_pool(&address).is_none());
    }

    #[test]
    fn test_update_liquidity() {
        let registry = PoolRegistry::new();
        let pool = create_test_pool(1);
        let address = pool.address;

        registry.add_pool(pool);

        let new_liquidity = U256::from(999999u64);
        registry.update_liquidity(&address, new_liquidity);

        let updated = registry.get_pool(&address).unwrap();
        assert_eq!(updated.liquidity, Some(new_liquidity));
    }

    #[test]
    fn test_set_pool_active() {
        let registry = PoolRegistry::new();
        let pool = create_test_pool(1);
        let address = pool.address;

        registry.add_pool(pool);

        registry.set_pool_active(&address, false);
        let updated = registry.get_pool(&address).unwrap();
        assert!(!updated.is_active);

        registry.set_pool_active(&address, true);
        let updated = registry.get_pool(&address).unwrap();
        assert!(updated.is_active);
    }

    #[test]
    fn test_get_active_pools() {
        let registry = PoolRegistry::new();

        let mut active_pool = create_test_pool(1);
        active_pool.is_active = true;

        let mut inactive_pool = create_test_pool(2);
        inactive_pool.is_active = false;

        registry.add_pool(active_pool);
        registry.add_pool(inactive_pool);

        let active_pools = registry.get_active_pools();
        assert_eq!(active_pools.len(), 1);
    }

    #[test]
    fn test_get_pools_by_type() {
        let registry = PoolRegistry::new();

        let v2_pool = PoolInfo {
            address: Address::repeat_byte(1),
            token0: Address::repeat_byte(10),
            token1: Address::repeat_byte(11),
            pool_type: PoolType::UniswapV2,
            fee: 30,
            factory: Address::repeat_byte(0xFF),
            created_block: None,
            liquidity: None,
            is_active: true,
        };

        let v3_pool = PoolInfo {
            address: Address::repeat_byte(2),
            token0: Address::repeat_byte(20),
            token1: Address::repeat_byte(21),
            pool_type: PoolType::UniswapV3,
            fee: 3000,
            factory: Address::repeat_byte(0xFE),
            created_block: None,
            liquidity: None,
            is_active: true,
        };

        registry.add_pool(v2_pool);
        registry.add_pool(v3_pool);

        let v2_pools = registry.get_pools_by_type(PoolType::UniswapV2);
        assert_eq!(v2_pools.len(), 1);

        let v3_pools = registry.get_pools_by_type(PoolType::UniswapV3);
        assert_eq!(v3_pools.len(), 1);
    }

    #[test]
    fn test_pool_info_helpers() {
        let pool = PoolInfo {
            address: Address::repeat_byte(1),
            token0: Address::repeat_byte(10),
            token1: Address::repeat_byte(20),
            pool_type: PoolType::UniswapV2,
            fee: 30,
            factory: Address::repeat_byte(0xFF),
            created_block: None,
            liquidity: None,
            is_active: true,
        };

        // Test contains_token
        assert!(pool.contains_token(&Address::repeat_byte(10)));
        assert!(pool.contains_token(&Address::repeat_byte(20)));
        assert!(!pool.contains_token(&Address::repeat_byte(30)));

        // Test other_token
        assert_eq!(
            pool.other_token(&Address::repeat_byte(10)),
            Some(Address::repeat_byte(20))
        );
        assert_eq!(
            pool.other_token(&Address::repeat_byte(20)),
            Some(Address::repeat_byte(10))
        );
        assert_eq!(pool.other_token(&Address::repeat_byte(30)), None);
    }

    #[test]
    fn test_pool_type() {
        assert!(PoolType::UniswapV2.is_v2_style());
        assert!(PoolType::SushiSwap.is_v2_style());
        assert!(!PoolType::UniswapV3.is_v2_style());

        assert!(!PoolType::UniswapV2.is_v3_style());
        assert!(PoolType::UniswapV3.is_v3_style());
    }

    #[test]
    fn test_stats() {
        let registry = PoolRegistry::new();

        let v2_pool = PoolInfo {
            address: Address::repeat_byte(1),
            token0: Address::repeat_byte(10),
            token1: Address::repeat_byte(11),
            pool_type: PoolType::UniswapV2,
            fee: 30,
            factory: Address::repeat_byte(0xFF),
            created_block: None,
            liquidity: None,
            is_active: true,
        };

        let v3_pool = PoolInfo {
            address: Address::repeat_byte(2),
            token0: Address::repeat_byte(10),
            token1: Address::repeat_byte(12),
            pool_type: PoolType::UniswapV3,
            fee: 3000,
            factory: Address::repeat_byte(0xFE),
            created_block: None,
            liquidity: None,
            is_active: false,
        };

        registry.add_pool(v2_pool);
        registry.add_pool(v3_pool);

        let stats = registry.stats();
        assert_eq!(stats.total_pools, 2);
        assert_eq!(stats.active_pools, 1);
        assert_eq!(stats.v2_pools, 1);
        assert_eq!(stats.v3_pools, 1);
    }

    #[test]
    fn test_clear() {
        let registry = PoolRegistry::new();

        registry.add_pool(create_test_pool(1));
        registry.add_pool(create_test_pool(2));
        assert_eq!(registry.len(), 2);

        registry.clear();
        assert!(registry.is_empty());
    }

    #[test]
    fn test_builder() {
        let registry = PoolRegistryBuilder::new()
            .add_pools(vec![create_test_pool(1), create_test_pool(2)])
            .build();

        assert_eq!(registry.len(), 2);
    }
}
