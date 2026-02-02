//! Uniswap V2 and fork implementations
//!
//! This module provides DEX interaction for Uniswap V2 style AMMs including
//! SushiSwap and other forks.

use super::{addresses, Dex, DexError, DexResult, PriceInfo, Reserves, SwapParams};
use alloy::primitives::{Address, Bytes, U256};
use alloy::providers::Provider;
use alloy::sol;
use alloy::sol_types::SolCall;
use alloy::transports::Transport;
use async_trait::async_trait;

// Uniswap V2 Pair interface
sol! {
    #[sol(rpc)]
    interface IUniswapV2Pair {
        function getReserves() external view returns (uint112 reserve0, uint112 reserve1, uint32 blockTimestampLast);
        function token0() external view returns (address);
        function token1() external view returns (address);
        function price0CumulativeLast() external view returns (uint256);
        function price1CumulativeLast() external view returns (uint256);
        function kLast() external view returns (uint256);
        function swap(uint256 amount0Out, uint256 amount1Out, address to, bytes calldata data) external;
        function sync() external;
    }
}

// Uniswap V2 Router interface
sol! {
    #[sol(rpc)]
    interface IUniswapV2Router02 {
        function factory() external view returns (address);
        function WETH() external view returns (address);

        function swapExactTokensForTokens(
            uint256 amountIn,
            uint256 amountOutMin,
            address[] calldata path,
            address to,
            uint256 deadline
        ) external returns (uint256[] memory amounts);

        function swapTokensForExactTokens(
            uint256 amountOut,
            uint256 amountInMax,
            address[] calldata path,
            address to,
            uint256 deadline
        ) external returns (uint256[] memory amounts);

        function swapExactETHForTokens(
            uint256 amountOutMin,
            address[] calldata path,
            address to,
            uint256 deadline
        ) external payable returns (uint256[] memory amounts);

        function swapTokensForExactETH(
            uint256 amountOut,
            uint256 amountInMax,
            address[] calldata path,
            address to,
            uint256 deadline
        ) external returns (uint256[] memory amounts);

        function swapExactTokensForETH(
            uint256 amountIn,
            uint256 amountOutMin,
            address[] calldata path,
            address to,
            uint256 deadline
        ) external returns (uint256[] memory amounts);

        function swapETHForExactTokens(
            uint256 amountOut,
            address[] calldata path,
            address to,
            uint256 deadline
        ) external payable returns (uint256[] memory amounts);

        function swapExactTokensForTokensSupportingFeeOnTransferTokens(
            uint256 amountIn,
            uint256 amountOutMin,
            address[] calldata path,
            address to,
            uint256 deadline
        ) external;

        function swapExactETHForTokensSupportingFeeOnTransferTokens(
            uint256 amountOutMin,
            address[] calldata path,
            address to,
            uint256 deadline
        ) external payable;

        function swapExactTokensForETHSupportingFeeOnTransferTokens(
            uint256 amountIn,
            uint256 amountOutMin,
            address[] calldata path,
            address to,
            uint256 deadline
        ) external;

        function getAmountOut(uint256 amountIn, uint256 reserveIn, uint256 reserveOut) external pure returns (uint256 amountOut);
        function getAmountIn(uint256 amountOut, uint256 reserveIn, uint256 reserveOut) external pure returns (uint256 amountIn);
        function getAmountsOut(uint256 amountIn, address[] calldata path) external view returns (uint256[] memory amounts);
        function getAmountsIn(uint256 amountOut, address[] calldata path) external view returns (uint256[] memory amounts);
    }
}

// Uniswap V2 Factory interface
sol! {
    #[sol(rpc)]
    interface IUniswapV2Factory {
        function getPair(address tokenA, address tokenB) external view returns (address pair);
        function allPairs(uint256) external view returns (address pair);
        function allPairsLength() external view returns (uint256);
        function feeTo() external view returns (address);
        function feeToSetter() external view returns (address);
        function createPair(address tokenA, address tokenB) external returns (address pair);
    }
}

/// Function selectors for Uniswap V2 Router functions
pub mod selectors {
    /// swapExactTokensForTokens selector
    pub const SWAP_EXACT_TOKENS_FOR_TOKENS: [u8; 4] = [0x38, 0xed, 0x17, 0x39];
    /// swapTokensForExactTokens selector
    pub const SWAP_TOKENS_FOR_EXACT_TOKENS: [u8; 4] = [0x88, 0x03, 0xdb, 0xee];
    /// swapExactETHForTokens selector
    pub const SWAP_EXACT_ETH_FOR_TOKENS: [u8; 4] = [0x7f, 0xf3, 0x6a, 0xb5];
    /// swapTokensForExactETH selector
    pub const SWAP_TOKENS_FOR_EXACT_ETH: [u8; 4] = [0x4a, 0x25, 0xd9, 0x4a];
    /// swapExactTokensForETH selector
    pub const SWAP_EXACT_TOKENS_FOR_ETH: [u8; 4] = [0x18, 0xcb, 0xaf, 0xe5];
    /// swapETHForExactTokens selector
    pub const SWAP_ETH_FOR_EXACT_TOKENS: [u8; 4] = [0xfb, 0x3b, 0xdb, 0x41];
    /// swapExactTokensForTokensSupportingFeeOnTransferTokens selector
    pub const SWAP_EXACT_TOKENS_FOR_TOKENS_SUPPORTING_FEE: [u8; 4] = [0x5c, 0x11, 0xd7, 0x95];
    /// swapExactETHForTokensSupportingFeeOnTransferTokens selector
    pub const SWAP_EXACT_ETH_FOR_TOKENS_SUPPORTING_FEE: [u8; 4] = [0xb6, 0xf9, 0xde, 0x95];
    /// swapExactTokensForETHSupportingFeeOnTransferTokens selector
    pub const SWAP_EXACT_TOKENS_FOR_ETH_SUPPORTING_FEE: [u8; 4] = [0x79, 0x1a, 0xc9, 0x47];
}

/// Uniswap V2 DEX implementation
#[derive(Debug, Clone)]
pub struct UniswapV2 {
    /// Name of this DEX instance
    name: String,
    /// Router address
    router: Address,
    /// Factory address
    factory: Address,
    /// Fee numerator (default 997 for 0.3% fee)
    fee_numerator: u64,
    /// Fee denominator (default 1000)
    fee_denominator: u64,
}

impl UniswapV2 {
    /// Create a new Uniswap V2 DEX instance
    pub fn new(name: impl Into<String>, router: Address, factory: Address) -> Self {
        Self {
            name: name.into(),
            router,
            factory,
            fee_numerator: 997,
            fee_denominator: 1000,
        }
    }

    /// Create a Uniswap V2 mainnet instance
    pub fn uniswap() -> Self {
        Self::new(
            "Uniswap V2",
            addresses::UNISWAP_V2_ROUTER,
            addresses::UNISWAP_V2_FACTORY,
        )
    }

    /// Create a SushiSwap mainnet instance
    pub fn sushiswap() -> Self {
        Self::new(
            "SushiSwap",
            addresses::SUSHISWAP_ROUTER,
            addresses::SUSHISWAP_FACTORY,
        )
    }

    /// Set custom fee parameters
    pub fn with_fee(mut self, numerator: u64, denominator: u64) -> Self {
        self.fee_numerator = numerator;
        self.fee_denominator = denominator;
        self
    }

    /// Get the factory address
    pub fn factory_address(&self) -> Address {
        self.factory
    }

    /// Get pair address for two tokens
    pub async fn get_pair<T: Transport + Clone, P: Provider<T>>(
        &self,
        token_a: Address,
        token_b: Address,
        provider: &P,
    ) -> DexResult<Address> {
        let factory = IUniswapV2Factory::new(self.factory, provider);

        let pair = factory
            .getPair(token_a, token_b)
            .call()
            .await
            .map_err(|e| DexError::ContractCall(e.to_string()))?
            .pair;

        if pair == Address::ZERO {
            return Err(DexError::PoolNotFound(format!(
                "No pair found for {:?} and {:?}",
                token_a, token_b
            )));
        }

        Ok(pair)
    }

    /// Get token0 and token1 addresses for a pair
    pub async fn get_pair_tokens<T: Transport + Clone, P: Provider<T>>(
        &self,
        pair: &Address,
        provider: &P,
    ) -> DexResult<(Address, Address)> {
        let pair_contract = IUniswapV2Pair::new(*pair, provider);

        let token0 = pair_contract
            .token0()
            .call()
            .await
            .map_err(|e| DexError::ContractCall(e.to_string()))?
            ._0;

        let token1 = pair_contract
            .token1()
            .call()
            .await
            .map_err(|e| DexError::ContractCall(e.to_string()))?
            ._0;

        Ok((token0, token1))
    }

    /// Calculate price from reserves as f64
    /// Note: This may lose precision for very large reserves (> 2^53)
    /// For precise comparisons, use calculate_price_scaled instead
    fn calculate_price(reserve0: U256, reserve1: U256) -> f64 {
        if reserve0.is_zero() {
            return 0.0;
        }

        // Convert to f64 for price calculation
        // This may lose precision for very large reserves
        let r0 = reserve0.to_string().parse::<f64>().unwrap_or(0.0);
        let r1 = reserve1.to_string().parse::<f64>().unwrap_or(0.0);

        if r0 == 0.0 {
            0.0
        } else {
            r1 / r0
        }
    }

    /// Calculate price ratio as a scaled U256 for full precision
    /// Returns price scaled by 1e18 (18 decimals) to preserve precision
    /// Use this for arbitrage detection where small differences matter
    pub fn calculate_price_scaled(reserve0: U256, reserve1: U256) -> U256 {
        if reserve0.is_zero() {
            return U256::ZERO;
        }
        // Scale by 1e18 for precision: price = (reserve1 * 1e18) / reserve0
        let scale = U256::from(1_000_000_000_000_000_000u128); // 1e18
        (reserve1 * scale) / reserve0
    }

    /// Compare two prices with a threshold (both scaled by 1e18)
    /// Returns true if price_a > price_b by at least threshold_bps basis points
    pub fn price_exceeds_by_bps(price_a_scaled: U256, price_b_scaled: U256, threshold_bps: u32) -> bool {
        if price_b_scaled.is_zero() {
            return !price_a_scaled.is_zero();
        }

        // Check if price_a > price_b * (1 + threshold_bps/10000)
        // Rearranged to avoid overflow: price_a * 10000 > price_b * (10000 + threshold_bps)
        let bps_base = U256::from(10_000u32);
        let threshold = U256::from(threshold_bps);

        price_a_scaled * bps_base > price_b_scaled * (bps_base + threshold)
    }

    /// Decode path from encoded bytes (used in some swap functions)
    #[allow(dead_code)] // Reserved for extended swap decoding
    fn decode_path(data: &[u8]) -> DexResult<Vec<Address>> {
        if data.len() < 64 {
            return Err(DexError::Decoding("Path data too short".to_string()));
        }

        // Skip offset (32 bytes) and read length
        let length_bytes: [u8; 32] = data[32..64]
            .try_into()
            .map_err(|_| DexError::Decoding("Invalid length bytes".to_string()))?;
        let length = U256::from_be_bytes(length_bytes);
        let length: usize = length
            .try_into()
            .map_err(|_| DexError::Decoding("Path length too large".to_string()))?;

        let mut path = Vec::with_capacity(length);
        for i in 0..length {
            let start = 64 + i * 32 + 12; // Skip first 12 bytes of each 32-byte slot (address is 20 bytes)
            let end = start + 20;
            if end > data.len() {
                return Err(DexError::Decoding("Path data truncated".to_string()));
            }
            let addr_bytes: [u8; 20] = data[start..end]
                .try_into()
                .map_err(|_| DexError::Decoding("Invalid address bytes".to_string()))?;
            path.push(Address::from(addr_bytes));
        }

        Ok(path)
    }
}

#[async_trait]
impl Dex for UniswapV2 {
    fn name(&self) -> &str {
        &self.name
    }

    async fn get_price<T: Transport + Clone, P: Provider<T>>(
        &self,
        pool: &Address,
        provider: &P,
    ) -> DexResult<PriceInfo> {
        let reserves = self.get_reserves(pool, provider).await?;
        let (token0, token1) = self.get_pair_tokens(pool, provider).await?;

        let block_number = provider
            .get_block_number()
            .await
            .map_err(|e| DexError::Provider(e.to_string()))?;

        let price = Self::calculate_price(reserves.reserve0, reserves.reserve1);

        Ok(PriceInfo {
            token0,
            token1,
            price,
            liquidity_token0: reserves.reserve0,
            liquidity_token1: reserves.reserve1,
            block_number,
        })
    }

    async fn get_reserves<T: Transport + Clone, P: Provider<T>>(
        &self,
        pool: &Address,
        provider: &P,
    ) -> DexResult<Reserves> {
        let pair = IUniswapV2Pair::new(*pool, provider);

        let result = pair
            .getReserves()
            .call()
            .await
            .map_err(|e| DexError::ContractCall(e.to_string()))?;

        Ok(Reserves {
            reserve0: U256::from(result.reserve0),
            reserve1: U256::from(result.reserve1),
            block_timestamp_last: result.blockTimestampLast,
        })
    }

    fn encode_swap(&self, params: &SwapParams) -> DexResult<Bytes> {
        if params.path.len() < 2 {
            return Err(DexError::InvalidParameters(
                "Path must contain at least 2 tokens".to_string(),
            ));
        }

        // Check if this involves ETH/WETH
        let is_eth_input = params.token_in == addresses::WETH;
        let is_eth_output = params.token_out == addresses::WETH;

        let calldata = if is_eth_input {
            // swapExactETHForTokens - amount is sent as msg.value
            IUniswapV2Router02::swapExactETHForTokensCall {
                amountOutMin: params.amount_out_min,
                path: params.path.clone(),
                to: params.recipient,
                deadline: params.deadline,
            }
            .abi_encode()
        } else if is_eth_output {
            // swapExactTokensForETH
            IUniswapV2Router02::swapExactTokensForETHCall {
                amountIn: params.amount_in,
                amountOutMin: params.amount_out_min,
                path: params.path.clone(),
                to: params.recipient,
                deadline: params.deadline,
            }
            .abi_encode()
        } else {
            // swapExactTokensForTokens
            IUniswapV2Router02::swapExactTokensForTokensCall {
                amountIn: params.amount_in,
                amountOutMin: params.amount_out_min,
                path: params.path.clone(),
                to: params.recipient,
                deadline: params.deadline,
            }
            .abi_encode()
        };

        Ok(Bytes::from(calldata))
    }

    fn decode_swap_input(&self, data: &Bytes) -> DexResult<SwapParams> {
        if data.len() < 4 {
            return Err(DexError::Decoding("Data too short".to_string()));
        }

        let selector: [u8; 4] = data[0..4]
            .try_into()
            .map_err(|_| DexError::Decoding("Invalid selector".to_string()))?;

        match selector {
            selectors::SWAP_EXACT_TOKENS_FOR_TOKENS
            | selectors::SWAP_EXACT_TOKENS_FOR_TOKENS_SUPPORTING_FEE => {
                let decoded = IUniswapV2Router02::swapExactTokensForTokensCall::abi_decode(
                    &data[4..],
                    true,
                )
                .map_err(|e| DexError::Decoding(e.to_string()))?;

                if decoded.path.len() < 2 {
                    return Err(DexError::Decoding("Invalid path length".to_string()));
                }

                Ok(SwapParams {
                    token_in: decoded.path[0],
                    token_out: decoded.path[decoded.path.len() - 1],
                    amount_in: decoded.amountIn,
                    amount_out_min: decoded.amountOutMin,
                    recipient: decoded.to,
                    deadline: decoded.deadline,
                    path: decoded.path,
                    fee: None,
                })
            }
            selectors::SWAP_TOKENS_FOR_EXACT_TOKENS => {
                let decoded = IUniswapV2Router02::swapTokensForExactTokensCall::abi_decode(
                    &data[4..],
                    true,
                )
                .map_err(|e| DexError::Decoding(e.to_string()))?;

                if decoded.path.len() < 2 {
                    return Err(DexError::Decoding("Invalid path length".to_string()));
                }

                Ok(SwapParams {
                    token_in: decoded.path[0],
                    token_out: decoded.path[decoded.path.len() - 1],
                    amount_in: decoded.amountInMax, // Note: this is max input
                    amount_out_min: decoded.amountOut, // Note: this is exact output
                    recipient: decoded.to,
                    deadline: decoded.deadline,
                    path: decoded.path,
                    fee: None,
                })
            }
            selectors::SWAP_EXACT_ETH_FOR_TOKENS
            | selectors::SWAP_EXACT_ETH_FOR_TOKENS_SUPPORTING_FEE => {
                let decoded =
                    IUniswapV2Router02::swapExactETHForTokensCall::abi_decode(&data[4..], true)
                        .map_err(|e| DexError::Decoding(e.to_string()))?;

                if decoded.path.len() < 2 {
                    return Err(DexError::Decoding("Invalid path length".to_string()));
                }

                Ok(SwapParams {
                    token_in: decoded.path[0],
                    token_out: decoded.path[decoded.path.len() - 1],
                    amount_in: U256::ZERO, // ETH amount comes from msg.value
                    amount_out_min: decoded.amountOutMin,
                    recipient: decoded.to,
                    deadline: decoded.deadline,
                    path: decoded.path,
                    fee: None,
                })
            }
            selectors::SWAP_EXACT_TOKENS_FOR_ETH
            | selectors::SWAP_EXACT_TOKENS_FOR_ETH_SUPPORTING_FEE => {
                let decoded =
                    IUniswapV2Router02::swapExactTokensForETHCall::abi_decode(&data[4..], true)
                        .map_err(|e| DexError::Decoding(e.to_string()))?;

                if decoded.path.len() < 2 {
                    return Err(DexError::Decoding("Invalid path length".to_string()));
                }

                Ok(SwapParams {
                    token_in: decoded.path[0],
                    token_out: decoded.path[decoded.path.len() - 1],
                    amount_in: decoded.amountIn,
                    amount_out_min: decoded.amountOutMin,
                    recipient: decoded.to,
                    deadline: decoded.deadline,
                    path: decoded.path,
                    fee: None,
                })
            }
            selectors::SWAP_TOKENS_FOR_EXACT_ETH => {
                let decoded =
                    IUniswapV2Router02::swapTokensForExactETHCall::abi_decode(&data[4..], true)
                        .map_err(|e| DexError::Decoding(e.to_string()))?;

                if decoded.path.len() < 2 {
                    return Err(DexError::Decoding("Invalid path length".to_string()));
                }

                Ok(SwapParams {
                    token_in: decoded.path[0],
                    token_out: decoded.path[decoded.path.len() - 1],
                    amount_in: decoded.amountInMax,
                    amount_out_min: decoded.amountOut,
                    recipient: decoded.to,
                    deadline: decoded.deadline,
                    path: decoded.path,
                    fee: None,
                })
            }
            selectors::SWAP_ETH_FOR_EXACT_TOKENS => {
                let decoded =
                    IUniswapV2Router02::swapETHForExactTokensCall::abi_decode(&data[4..], true)
                        .map_err(|e| DexError::Decoding(e.to_string()))?;

                if decoded.path.len() < 2 {
                    return Err(DexError::Decoding("Invalid path length".to_string()));
                }

                Ok(SwapParams {
                    token_in: decoded.path[0],
                    token_out: decoded.path[decoded.path.len() - 1],
                    amount_in: U256::ZERO, // ETH amount comes from msg.value
                    amount_out_min: decoded.amountOut,
                    recipient: decoded.to,
                    deadline: decoded.deadline,
                    path: decoded.path,
                    fee: None,
                })
            }
            _ => Err(DexError::UnknownSelector(hex::encode(selector))),
        }
    }

    fn router_address(&self) -> Address {
        self.router
    }

    fn get_amount_out(&self, amount_in: U256, reserve_in: U256, reserve_out: U256) -> U256 {
        if amount_in.is_zero() || reserve_in.is_zero() || reserve_out.is_zero() {
            return U256::ZERO;
        }

        // amountIn * 997 * reserveOut / (reserveIn * 1000 + amountIn * 997)
        let amount_in_with_fee = amount_in * U256::from(self.fee_numerator);
        let numerator = amount_in_with_fee * reserve_out;
        let denominator =
            reserve_in * U256::from(self.fee_denominator) + amount_in_with_fee;

        if denominator.is_zero() {
            U256::ZERO
        } else {
            numerator / denominator
        }
    }

    fn get_amount_in(&self, amount_out: U256, reserve_in: U256, reserve_out: U256) -> U256 {
        if amount_out.is_zero() || reserve_in.is_zero() || reserve_out.is_zero() {
            return U256::ZERO;
        }

        if amount_out >= reserve_out {
            return U256::MAX; // Cannot get more than reserve
        }

        // (reserveIn * amountOut * 1000) / ((reserveOut - amountOut) * 997) + 1
        let numerator = reserve_in * amount_out * U256::from(self.fee_denominator);
        let denominator = (reserve_out - amount_out) * U256::from(self.fee_numerator);

        if denominator.is_zero() {
            U256::MAX
        } else {
            numerator / denominator + U256::from(1u64)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_uniswap_v2_creation() {
        let dex = UniswapV2::uniswap();
        assert_eq!(dex.name(), "Uniswap V2");
        assert_eq!(dex.router_address(), addresses::UNISWAP_V2_ROUTER);
        assert_eq!(dex.factory_address(), addresses::UNISWAP_V2_FACTORY);
    }

    #[test]
    fn test_sushiswap_creation() {
        let dex = UniswapV2::sushiswap();
        assert_eq!(dex.name(), "SushiSwap");
        assert_eq!(dex.router_address(), addresses::SUSHISWAP_ROUTER);
        assert_eq!(dex.factory_address(), addresses::SUSHISWAP_FACTORY);
    }

    #[test]
    fn test_get_amount_out() {
        let dex = UniswapV2::uniswap();

        // 1 ETH in, 1000 ETH reserve, 2000000 USDC reserve
        let amount_in = U256::from(1_000_000_000_000_000_000u128); // 1 ETH
        let reserve_in = U256::from(1000_000_000_000_000_000_000u128); // 1000 ETH
        let reserve_out = U256::from(2_000_000_000_000u128); // 2M USDC (6 decimals)

        let amount_out = dex.get_amount_out(amount_in, reserve_in, reserve_out);

        // Should get approximately 1994 USDC (with 0.3% fee and slippage)
        assert!(amount_out > U256::ZERO);
        assert!(amount_out < reserve_out);
    }

    #[test]
    fn test_get_amount_in() {
        let dex = UniswapV2::uniswap();

        // Want 1000 USDC out
        let amount_out = U256::from(1_000_000_000u128); // 1000 USDC
        let reserve_in = U256::from(1000_000_000_000_000_000_000u128); // 1000 ETH
        let reserve_out = U256::from(2_000_000_000_000u128); // 2M USDC

        let amount_in = dex.get_amount_in(amount_out, reserve_in, reserve_out);

        // Should need approximately 0.5 ETH
        assert!(amount_in > U256::ZERO);
        assert!(amount_in < reserve_in);
    }

    #[test]
    fn test_get_amount_out_zero_inputs() {
        let dex = UniswapV2::uniswap();

        assert_eq!(
            dex.get_amount_out(U256::ZERO, U256::from(1000u64), U256::from(1000u64)),
            U256::ZERO
        );
        assert_eq!(
            dex.get_amount_out(U256::from(100u64), U256::ZERO, U256::from(1000u64)),
            U256::ZERO
        );
        assert_eq!(
            dex.get_amount_out(U256::from(100u64), U256::from(1000u64), U256::ZERO),
            U256::ZERO
        );
    }

    #[test]
    fn test_calculate_price() {
        let reserve0 = U256::from(1_000_000_000_000_000_000u128); // 1 token
        let reserve1 = U256::from(2_000_000_000_000_000_000u128); // 2 tokens

        let price = UniswapV2::calculate_price(reserve0, reserve1);
        assert!((price - 2.0).abs() < 0.0001);
    }

    #[test]
    fn test_encode_swap_exact_tokens_for_tokens() {
        let dex = UniswapV2::uniswap();

        let params = SwapParams {
            token_in: Address::repeat_byte(1),
            token_out: Address::repeat_byte(2),
            amount_in: U256::from(1000u64),
            amount_out_min: U256::from(900u64),
            recipient: Address::repeat_byte(3),
            deadline: U256::from(1700000000u64),
            path: vec![Address::repeat_byte(1), Address::repeat_byte(2)],
            fee: None,
        };

        let encoded = dex.encode_swap(&params).unwrap();
        assert!(!encoded.is_empty());

        // Verify selector
        let selector: [u8; 4] = encoded[0..4].try_into().unwrap();
        assert_eq!(selector, selectors::SWAP_EXACT_TOKENS_FOR_TOKENS);
    }

    #[test]
    fn test_decode_swap_exact_tokens_for_tokens() {
        let dex = UniswapV2::uniswap();

        let original_params = SwapParams {
            token_in: Address::repeat_byte(1),
            token_out: Address::repeat_byte(2),
            amount_in: U256::from(1000u64),
            amount_out_min: U256::from(900u64),
            recipient: Address::repeat_byte(3),
            deadline: U256::from(1700000000u64),
            path: vec![Address::repeat_byte(1), Address::repeat_byte(2)],
            fee: None,
        };

        let encoded = dex.encode_swap(&original_params).unwrap();
        let decoded = dex.decode_swap_input(&encoded).unwrap();

        assert_eq!(decoded.token_in, original_params.token_in);
        assert_eq!(decoded.token_out, original_params.token_out);
        assert_eq!(decoded.amount_in, original_params.amount_in);
        assert_eq!(decoded.amount_out_min, original_params.amount_out_min);
        assert_eq!(decoded.recipient, original_params.recipient);
        assert_eq!(decoded.deadline, original_params.deadline);
        assert_eq!(decoded.path, original_params.path);
    }
}
