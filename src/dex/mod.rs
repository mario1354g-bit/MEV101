//! DEX interaction modules for MEV bot
//!
//! This module provides abstractions for interacting with various DEX protocols
//! including Uniswap V2, Uniswap V3, and their forks.

pub mod pool_registry;
pub mod uniswap_v2;
pub mod uniswap_v3;

pub use pool_registry::{PoolInfo, PoolRegistry, PoolType};
pub use uniswap_v2::UniswapV2;
pub use uniswap_v3::UniswapV3;

use alloy::primitives::{Address, Bytes, U256};
use alloy::providers::Provider;
use alloy::transports::Transport;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Errors that can occur during DEX operations
#[derive(Error, Debug)]
pub enum DexError {
    #[error("Provider error: {0}")]
    Provider(String),

    #[error("Contract call failed: {0}")]
    ContractCall(String),

    #[error("Invalid pool address: {0}")]
    InvalidPool(String),

    #[error("Decoding error: {0}")]
    Decoding(String),

    #[error("Encoding error: {0}")]
    Encoding(String),

    #[error("Unknown function selector: {0}")]
    UnknownSelector(String),

    #[error("Insufficient liquidity")]
    InsufficientLiquidity,

    #[error("Invalid parameters: {0}")]
    InvalidParameters(String),

    #[error("Pool not found: {0}")]
    PoolNotFound(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
}

/// Result type for DEX operations
pub type DexResult<T> = std::result::Result<T, DexError>;

/// Price information for a pool
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PriceInfo {
    /// Token0 address
    pub token0: Address,
    /// Token1 address
    pub token1: Address,
    /// Price of token0 in terms of token1
    pub price: f64,
    /// Liquidity of token0 in the pool
    pub liquidity_token0: U256,
    /// Liquidity of token1 in the pool
    pub liquidity_token1: U256,
    /// Block number when price was fetched
    pub block_number: u64,
}

/// Reserve information for a pool
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Reserves {
    /// Reserve of token0
    pub reserve0: U256,
    /// Reserve of token1
    pub reserve1: U256,
    /// Block timestamp of last update
    pub block_timestamp_last: u32,
}

/// Parameters for executing a swap
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    /// Deadline for the swap (Unix timestamp)
    pub deadline: U256,
    /// Optional path for multi-hop swaps
    pub path: Vec<Address>,
    /// Fee tier (for V3 pools)
    pub fee: Option<u32>,
}

impl SwapParams {
    /// Create new swap parameters for a direct swap
    pub fn new(
        token_in: Address,
        token_out: Address,
        amount_in: U256,
        amount_out_min: U256,
        recipient: Address,
        deadline: U256,
    ) -> Self {
        Self {
            token_in,
            token_out,
            amount_in,
            amount_out_min,
            recipient,
            deadline,
            path: vec![token_in, token_out],
            fee: None,
        }
    }

    /// Create swap parameters with a custom path
    pub fn with_path(
        path: Vec<Address>,
        amount_in: U256,
        amount_out_min: U256,
        recipient: Address,
        deadline: U256,
    ) -> DexResult<Self> {
        if path.len() < 2 {
            return Err(DexError::InvalidParameters(
                "Path must contain at least 2 tokens".to_string(),
            ));
        }
        Ok(Self {
            token_in: path[0],
            token_out: path[path.len() - 1],
            amount_in,
            amount_out_min,
            recipient,
            deadline,
            path,
            fee: None,
        })
    }

    /// Set the fee tier for V3 swaps
    pub fn with_fee(mut self, fee: u32) -> Self {
        self.fee = Some(fee);
        self
    }
}

/// Trait for DEX implementations
#[async_trait]
pub trait Dex: Send + Sync {
    /// Returns the name of this DEX
    fn name(&self) -> &str;

    /// Get the current price for a pool
    async fn get_price<T: Transport + Clone, P: Provider<T>>(
        &self,
        pool: &Address,
        provider: &P,
    ) -> DexResult<PriceInfo>;

    /// Get the current reserves for a pool
    async fn get_reserves<T: Transport + Clone, P: Provider<T>>(
        &self,
        pool: &Address,
        provider: &P,
    ) -> DexResult<Reserves>;

    /// Encode swap calldata for the router
    fn encode_swap(&self, params: &SwapParams) -> DexResult<Bytes>;

    /// Decode swap parameters from calldata
    fn decode_swap_input(&self, data: &Bytes) -> DexResult<SwapParams>;

    /// Get the router address for this DEX
    fn router_address(&self) -> Address;

    /// Calculate the expected output amount for a given input
    fn get_amount_out(&self, amount_in: U256, reserve_in: U256, reserve_out: U256) -> U256;

    /// Calculate the required input amount for a given output
    fn get_amount_in(&self, amount_out: U256, reserve_in: U256, reserve_out: U256) -> U256;
}

/// Common DEX router addresses on Ethereum mainnet
pub mod addresses {
    use alloy::primitives::address;
    use alloy::primitives::Address;

    // Uniswap V2
    pub const UNISWAP_V2_ROUTER: Address = address!("7a250d5630B4cF539739dF2C5dAcb4c659F2488D");
    pub const UNISWAP_V2_FACTORY: Address = address!("5C69bEe701ef814a2B6a3EDD4B1652CB9cc5aA6f");

    // SushiSwap
    pub const SUSHISWAP_ROUTER: Address = address!("d9e1cE17f2641f24aE83637ab66a2cca9C378B9F");
    pub const SUSHISWAP_FACTORY: Address = address!("C0AEe478e3658e2610c5F7A4A2E1777cE9e4f2Ac");

    // Uniswap V3
    pub const UNISWAP_V3_ROUTER: Address = address!("E592427A0AEce92De3Edee1F18E0157C05861564");
    pub const UNISWAP_V3_ROUTER_02: Address = address!("68b3465833fb72A70ecDF485E0e4C7bD8665Fc45");
    pub const UNISWAP_V3_FACTORY: Address = address!("1F98431c8aD98523631AE4a59f267346ea31F984");
    pub const UNISWAP_V3_QUOTER: Address = address!("b27308f9F90D607463bb33eA1BeBb41C27CE5AB6");
    pub const UNISWAP_V3_QUOTER_V2: Address = address!("61fFE014bA17989E743c5F6cB21bF9697530B21e");

    // Common tokens
    pub const WETH: Address = address!("C02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2");
    pub const USDC: Address = address!("A0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48");
    pub const USDT: Address = address!("dAC17F958D2ee523a2206206994597C13D831ec7");
    pub const DAI: Address = address!("6B175474E89094C44Da98b954EedeAC495271d0F");
    pub const WBTC: Address = address!("2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599");
}

/// Fee tiers available for Uniswap V3 pools
pub mod fee_tiers {
    /// 0.01% fee tier (1 basis point)
    pub const FEE_LOWEST: u32 = 100;
    /// 0.05% fee tier (5 basis points)
    pub const FEE_LOW: u32 = 500;
    /// 0.30% fee tier (30 basis points)
    pub const FEE_MEDIUM: u32 = 3000;
    /// 1.00% fee tier (100 basis points)
    pub const FEE_HIGH: u32 = 10000;

    /// All available fee tiers
    pub const ALL_FEE_TIERS: [u32; 4] = [FEE_LOWEST, FEE_LOW, FEE_MEDIUM, FEE_HIGH];

    /// Check if a fee tier is valid
    pub fn is_valid_fee(fee: u32) -> bool {
        ALL_FEE_TIERS.contains(&fee)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_swap_params_new() {
        let token_in = Address::ZERO;
        let token_out = Address::repeat_byte(1);
        let amount_in = U256::from(1000u64);
        let amount_out_min = U256::from(900u64);
        let recipient = Address::repeat_byte(2);
        let deadline = U256::from(1700000000u64);

        let params = SwapParams::new(
            token_in,
            token_out,
            amount_in,
            amount_out_min,
            recipient,
            deadline,
        );

        assert_eq!(params.token_in, token_in);
        assert_eq!(params.token_out, token_out);
        assert_eq!(params.amount_in, amount_in);
        assert_eq!(params.amount_out_min, amount_out_min);
        assert_eq!(params.recipient, recipient);
        assert_eq!(params.deadline, deadline);
        assert_eq!(params.path.len(), 2);
        assert!(params.fee.is_none());
    }

    #[test]
    fn test_swap_params_with_fee() {
        let params = SwapParams::new(
            Address::ZERO,
            Address::repeat_byte(1),
            U256::from(1000u64),
            U256::from(900u64),
            Address::repeat_byte(2),
            U256::from(1700000000u64),
        )
        .with_fee(3000);

        assert_eq!(params.fee, Some(3000));
    }

    #[test]
    fn test_swap_params_with_path() {
        let path = vec![
            Address::ZERO,
            Address::repeat_byte(1),
            Address::repeat_byte(2),
        ];
        let params = SwapParams::with_path(
            path.clone(),
            U256::from(1000u64),
            U256::from(900u64),
            Address::repeat_byte(3),
            U256::from(1700000000u64),
        )
        .unwrap();

        assert_eq!(params.path, path);
        assert_eq!(params.token_in, Address::ZERO);
        assert_eq!(params.token_out, Address::repeat_byte(2));
    }

    #[test]
    fn test_swap_params_with_path_too_short() {
        let path = vec![Address::ZERO];
        let result = SwapParams::with_path(
            path,
            U256::from(1000u64),
            U256::from(900u64),
            Address::repeat_byte(3),
            U256::from(1700000000u64),
        );

        assert!(result.is_err());
    }

    #[test]
    fn test_fee_tiers() {
        assert!(fee_tiers::is_valid_fee(500));
        assert!(fee_tiers::is_valid_fee(3000));
        assert!(fee_tiers::is_valid_fee(10000));
        assert!(!fee_tiers::is_valid_fee(1000));
        assert!(!fee_tiers::is_valid_fee(0));
    }
}
