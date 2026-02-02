//! DEX router calldata encoding.
//!
//! This module provides utilities for encoding calldata for various DEX
//! router contracts, including Uniswap V2, V3, and multicall operations.

use alloy::primitives::{Address, Bytes, Uint, U256};
use alloy::sol;
use alloy::sol_types::SolCall;

// Define Uniswap V2 Router interface
sol! {
    /// Uniswap V2 Router interface for token swaps
    #[derive(Debug)]
    interface IUniswapV2Router {
        /// Swap exact tokens for tokens
        function swapExactTokensForTokens(
            uint256 amountIn,
            uint256 amountOutMin,
            address[] calldata path,
            address to,
            uint256 deadline
        ) external returns (uint256[] memory amounts);

        /// Swap exact ETH for tokens
        function swapExactETHForTokens(
            uint256 amountOutMin,
            address[] calldata path,
            address to,
            uint256 deadline
        ) external payable returns (uint256[] memory amounts);

        /// Swap exact tokens for ETH
        function swapExactTokensForETH(
            uint256 amountIn,
            uint256 amountOutMin,
            address[] calldata path,
            address to,
            uint256 deadline
        ) external returns (uint256[] memory amounts);

        /// Swap tokens for exact tokens
        function swapTokensForExactTokens(
            uint256 amountOut,
            uint256 amountInMax,
            address[] calldata path,
            address to,
            uint256 deadline
        ) external returns (uint256[] memory amounts);

        /// Add liquidity
        function addLiquidity(
            address tokenA,
            address tokenB,
            uint256 amountADesired,
            uint256 amountBDesired,
            uint256 amountAMin,
            uint256 amountBMin,
            address to,
            uint256 deadline
        ) external returns (uint256 amountA, uint256 amountB, uint256 liquidity);

        /// Remove liquidity
        function removeLiquidity(
            address tokenA,
            address tokenB,
            uint256 liquidity,
            uint256 amountAMin,
            uint256 amountBMin,
            address to,
            uint256 deadline
        ) external returns (uint256 amountA, uint256 amountB);
    }
}

// Define Uniswap V3 SwapRouter interface
sol! {
    /// Uniswap V3 SwapRouter interface
    #[derive(Debug)]
    interface ISwapRouter {
        /// Parameters for exactInputSingle
        struct ExactInputSingleParams {
            address tokenIn;
            address tokenOut;
            uint24 fee;
            address recipient;
            uint256 deadline;
            uint256 amountIn;
            uint256 amountOutMinimum;
            uint160 sqrtPriceLimitX96;
        }

        /// Swap exact input for single hop
        function exactInputSingle(ExactInputSingleParams calldata params) external payable returns (uint256 amountOut);

        /// Parameters for exactInput (multi-hop)
        struct ExactInputParams {
            bytes path;
            address recipient;
            uint256 deadline;
            uint256 amountIn;
            uint256 amountOutMinimum;
        }

        /// Swap exact input for multi-hop
        function exactInput(ExactInputParams calldata params) external payable returns (uint256 amountOut);

        /// Parameters for exactOutputSingle
        struct ExactOutputSingleParams {
            address tokenIn;
            address tokenOut;
            uint24 fee;
            address recipient;
            uint256 deadline;
            uint256 amountOut;
            uint256 amountInMaximum;
            uint160 sqrtPriceLimitX96;
        }

        /// Swap exact output for single hop
        function exactOutputSingle(ExactOutputSingleParams calldata params) external payable returns (uint256 amountIn);

        /// Parameters for exactOutput (multi-hop)
        struct ExactOutputParams {
            bytes path;
            address recipient;
            uint256 deadline;
            uint256 amountOut;
            uint256 amountInMaximum;
        }

        /// Swap exact output for multi-hop
        function exactOutput(ExactOutputParams calldata params) external payable returns (uint256 amountIn);
    }
}

// Define Uniswap V3 SwapRouter02 interface (newer version)
sol! {
    /// Uniswap V3 SwapRouter02 interface
    #[derive(Debug)]
    interface ISwapRouter02 {
        /// Parameters for exactInputSingle (V3)
        struct ExactInputSingleParams {
            address tokenIn;
            address tokenOut;
            uint24 fee;
            address recipient;
            uint256 amountIn;
            uint256 amountOutMinimum;
            uint160 sqrtPriceLimitX96;
        }

        /// Swap exact input single (no deadline - handled by multicall)
        function exactInputSingle(ExactInputSingleParams calldata params) external payable returns (uint256 amountOut);

        /// Multicall for batching
        function multicall(uint256 deadline, bytes[] calldata data) external payable returns (bytes[] memory results);

        /// Multicall without deadline
        function multicall(bytes[] calldata data) external payable returns (bytes[] memory results);
    }
}

// Define ERC20 interface for approvals
sol! {
    /// ERC20 interface
    #[derive(Debug)]
    interface IERC20 {
        function approve(address spender, uint256 amount) external returns (bool);
        function transfer(address to, uint256 amount) external returns (bool);
        function transferFrom(address from, address to, uint256 amount) external returns (bool);
        function balanceOf(address account) external view returns (uint256);
        function allowance(address owner, address spender) external view returns (uint256);
    }
}

// Define WETH interface
sol! {
    /// WETH interface
    #[derive(Debug)]
    interface IWETH {
        function deposit() external payable;
        function withdraw(uint256 amount) external;
    }
}

/// Router encoder for DEX calldata construction.
pub struct RouterEncoder;

impl RouterEncoder {
    // ==================== Uniswap V2 Methods ====================

    /// Encode a Uniswap V2 swapExactTokensForTokens call.
    ///
    /// # Arguments
    /// * `amount_in` - Amount of input tokens
    /// * `amount_out_min` - Minimum amount of output tokens (slippage protection)
    /// * `path` - Swap path (array of token addresses)
    /// * `to` - Recipient address
    /// * `deadline` - Transaction deadline (unix timestamp)
    pub fn encode_v2_swap_exact_tokens(
        amount_in: U256,
        amount_out_min: U256,
        path: Vec<Address>,
        to: Address,
        deadline: u64,
    ) -> Bytes {
        let call = IUniswapV2Router::swapExactTokensForTokensCall {
            amountIn: amount_in,
            amountOutMin: amount_out_min,
            path,
            to,
            deadline: U256::from(deadline),
        };
        Bytes::from(call.abi_encode())
    }

    /// Encode a Uniswap V2 swapExactETHForTokens call.
    pub fn encode_v2_swap_exact_eth_for_tokens(
        amount_out_min: U256,
        path: Vec<Address>,
        to: Address,
        deadline: u64,
    ) -> Bytes {
        let call = IUniswapV2Router::swapExactETHForTokensCall {
            amountOutMin: amount_out_min,
            path,
            to,
            deadline: U256::from(deadline),
        };
        Bytes::from(call.abi_encode())
    }

    /// Encode a Uniswap V2 swapExactTokensForETH call.
    pub fn encode_v2_swap_exact_tokens_for_eth(
        amount_in: U256,
        amount_out_min: U256,
        path: Vec<Address>,
        to: Address,
        deadline: u64,
    ) -> Bytes {
        let call = IUniswapV2Router::swapExactTokensForETHCall {
            amountIn: amount_in,
            amountOutMin: amount_out_min,
            path,
            to,
            deadline: U256::from(deadline),
        };
        Bytes::from(call.abi_encode())
    }

    /// Encode a Uniswap V2 swapTokensForExactTokens call.
    pub fn encode_v2_swap_tokens_for_exact(
        amount_out: U256,
        amount_in_max: U256,
        path: Vec<Address>,
        to: Address,
        deadline: u64,
    ) -> Bytes {
        let call = IUniswapV2Router::swapTokensForExactTokensCall {
            amountOut: amount_out,
            amountInMax: amount_in_max,
            path,
            to,
            deadline: U256::from(deadline),
        };
        Bytes::from(call.abi_encode())
    }

    /// Encode a Uniswap V2 addLiquidity call.
    #[allow(clippy::too_many_arguments)]
    pub fn encode_v2_add_liquidity(
        token_a: Address,
        token_b: Address,
        amount_a_desired: U256,
        amount_b_desired: U256,
        amount_a_min: U256,
        amount_b_min: U256,
        to: Address,
        deadline: u64,
    ) -> Bytes {
        let call = IUniswapV2Router::addLiquidityCall {
            tokenA: token_a,
            tokenB: token_b,
            amountADesired: amount_a_desired,
            amountBDesired: amount_b_desired,
            amountAMin: amount_a_min,
            amountBMin: amount_b_min,
            to,
            deadline: U256::from(deadline),
        };
        Bytes::from(call.abi_encode())
    }

    /// Encode a Uniswap V2 removeLiquidity call.
    pub fn encode_v2_remove_liquidity(
        token_a: Address,
        token_b: Address,
        liquidity: U256,
        amount_a_min: U256,
        amount_b_min: U256,
        to: Address,
        deadline: u64,
    ) -> Bytes {
        let call = IUniswapV2Router::removeLiquidityCall {
            tokenA: token_a,
            tokenB: token_b,
            liquidity,
            amountAMin: amount_a_min,
            amountBMin: amount_b_min,
            to,
            deadline: U256::from(deadline),
        };
        Bytes::from(call.abi_encode())
    }

    // ==================== Uniswap V3 Methods ====================

    /// Encode a Uniswap V3 exactInputSingle call.
    ///
    /// # Arguments
    /// * `token_in` - Input token address
    /// * `token_out` - Output token address
    /// * `fee` - Pool fee tier (500, 3000, or 10000)
    /// * `recipient` - Recipient address
    /// * `amount_in` - Amount of input tokens
    /// * `amount_out_min` - Minimum output (slippage protection)
    /// * `sqrt_price_limit` - Price limit (0 for no limit)
    pub fn encode_v3_exact_input_single(
        token_in: Address,
        token_out: Address,
        fee: u32,
        recipient: Address,
        amount_in: U256,
        amount_out_min: U256,
        sqrt_price_limit: U256,
    ) -> Bytes {
        let fee_u24: Uint<24, 1> = Uint::from(fee);
        let sqrt_limit: Uint<160, 3> = if sqrt_price_limit.is_zero() {
            Uint::ZERO
        } else {
            Uint::from_limbs_slice(&sqrt_price_limit.as_limbs()[..3])
        };
        // Use current timestamp + 2 minutes as deadline to prevent stale execution
        let deadline = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| U256::from(d.as_secs() + 120))
            .unwrap_or(U256::MAX);

        let call = ISwapRouter::exactInputSingleCall {
            params: ISwapRouter::ExactInputSingleParams {
                tokenIn: token_in,
                tokenOut: token_out,
                fee: fee_u24,
                recipient,
                deadline,
                amountIn: amount_in,
                amountOutMinimum: amount_out_min,
                sqrtPriceLimitX96: sqrt_limit,
            },
        };
        Bytes::from(call.abi_encode())
    }

    /// Encode a Uniswap V3 exactInput (multi-hop) call.
    ///
    /// # Arguments
    /// * `path` - Encoded path (token, fee, token, fee, token...)
    /// * `recipient` - Recipient address
    /// * `amount_in` - Amount of input tokens
    /// * `amount_out_min` - Minimum output (slippage protection)
    pub fn encode_v3_exact_input(
        path: Bytes,
        recipient: Address,
        amount_in: U256,
        amount_out_min: U256,
    ) -> Bytes {
        // Use current timestamp + 2 minutes as deadline
        let deadline = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| U256::from(d.as_secs() + 120))
            .unwrap_or(U256::MAX);

        let call = ISwapRouter::exactInputCall {
            params: ISwapRouter::ExactInputParams {
                path,
                recipient,
                deadline,
                amountIn: amount_in,
                amountOutMinimum: amount_out_min,
            },
        };
        Bytes::from(call.abi_encode())
    }

    /// Encode a Uniswap V3 exactOutputSingle call.
    pub fn encode_v3_exact_output_single(
        token_in: Address,
        token_out: Address,
        fee: u32,
        recipient: Address,
        amount_out: U256,
        amount_in_max: U256,
        sqrt_price_limit: U256,
    ) -> Bytes {
        let fee_u24: Uint<24, 1> = Uint::from(fee);
        let sqrt_limit: Uint<160, 3> = if sqrt_price_limit.is_zero() {
            Uint::ZERO
        } else {
            Uint::from_limbs_slice(&sqrt_price_limit.as_limbs()[..3])
        };

        // Use current timestamp + 2 minutes as deadline
        let deadline = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| U256::from(d.as_secs() + 120))
            .unwrap_or(U256::MAX);

        let call = ISwapRouter::exactOutputSingleCall {
            params: ISwapRouter::ExactOutputSingleParams {
                tokenIn: token_in,
                tokenOut: token_out,
                fee: fee_u24,
                recipient,
                deadline,
                amountOut: amount_out,
                amountInMaximum: amount_in_max,
                sqrtPriceLimitX96: sqrt_limit,
            },
        };
        Bytes::from(call.abi_encode())
    }

    /// Encode a Uniswap V3 path for multi-hop swaps.
    ///
    /// # Arguments
    /// * `tokens` - List of token addresses
    /// * `fees` - List of fee tiers (one less than tokens)
    pub fn encode_v3_path(tokens: &[Address], fees: &[u32]) -> Bytes {
        assert!(
            tokens.len() == fees.len() + 1,
            "Invalid path: tokens.len() must equal fees.len() + 1"
        );

        let mut path = Vec::new();

        for (i, token) in tokens.iter().enumerate() {
            path.extend_from_slice(token.as_slice());
            if i < fees.len() {
                // Fee is encoded as 3 bytes (24 bits)
                let fee_bytes = fees[i].to_be_bytes();
                path.extend_from_slice(&fee_bytes[1..4]); // Take last 3 bytes
            }
        }

        Bytes::from(path)
    }

    /// Encode a reverse V3 path (for exact output swaps).
    pub fn encode_v3_path_reverse(tokens: &[Address], fees: &[u32]) -> Bytes {
        let mut reversed_tokens: Vec<Address> = tokens.to_vec();
        reversed_tokens.reverse();

        let mut reversed_fees: Vec<u32> = fees.to_vec();
        reversed_fees.reverse();

        Self::encode_v3_path(&reversed_tokens, &reversed_fees)
    }

    // ==================== Multicall Methods ====================

    /// Encode a multicall with deadline.
    ///
    /// # Arguments
    /// * `deadline` - Transaction deadline (unix timestamp)
    /// * `calls` - Array of encoded function calls
    pub fn encode_multicall(deadline: u64, calls: Vec<Bytes>) -> Bytes {
        let call = ISwapRouter02::multicall_0Call {
            deadline: U256::from(deadline),
            data: calls,
        };
        Bytes::from(call.abi_encode())
    }

    /// Encode a multicall without deadline.
    pub fn encode_multicall_no_deadline(calls: Vec<Bytes>) -> Bytes {
        let call = ISwapRouter02::multicall_1Call { data: calls };
        Bytes::from(call.abi_encode())
    }

    // ==================== ERC20 Methods ====================

    /// Encode an ERC20 approve call.
    pub fn encode_erc20_approve(spender: Address, amount: U256) -> Bytes {
        let call = IERC20::approveCall { spender, amount };
        Bytes::from(call.abi_encode())
    }

    /// Encode an ERC20 transfer call.
    pub fn encode_erc20_transfer(to: Address, amount: U256) -> Bytes {
        let call = IERC20::transferCall { to, amount };
        Bytes::from(call.abi_encode())
    }

    /// Encode an ERC20 transferFrom call.
    pub fn encode_erc20_transfer_from(from: Address, to: Address, amount: U256) -> Bytes {
        let call = IERC20::transferFromCall { from, to, amount };
        Bytes::from(call.abi_encode())
    }

    /// Encode an ERC20 balanceOf call.
    pub fn encode_erc20_balance_of(account: Address) -> Bytes {
        let call = IERC20::balanceOfCall { account };
        Bytes::from(call.abi_encode())
    }

    /// Encode an ERC20 allowance call.
    pub fn encode_erc20_allowance(owner: Address, spender: Address) -> Bytes {
        let call = IERC20::allowanceCall { owner, spender };
        Bytes::from(call.abi_encode())
    }

    // ==================== WETH Methods ====================

    /// Encode a WETH deposit call.
    pub fn encode_weth_deposit() -> Bytes {
        let call = IWETH::depositCall {};
        Bytes::from(call.abi_encode())
    }

    /// Encode a WETH withdraw call.
    pub fn encode_weth_withdraw(amount: U256) -> Bytes {
        let call = IWETH::withdrawCall { amount };
        Bytes::from(call.abi_encode())
    }

    // ==================== Helper Methods ====================

    /// Calculate the minimum output with slippage.
    ///
    /// # Arguments
    /// * `expected_output` - Expected output amount
    /// * `slippage_bps` - Slippage tolerance in basis points (e.g., 50 = 0.5%)
    pub fn calculate_min_output(expected_output: U256, slippage_bps: u32) -> U256 {
        let slippage_factor = U256::from(10000 - slippage_bps);
        expected_output * slippage_factor / U256::from(10000)
    }

    /// Calculate the maximum input with slippage.
    pub fn calculate_max_input(expected_input: U256, slippage_bps: u32) -> U256 {
        let slippage_factor = U256::from(10000 + slippage_bps);
        expected_input * slippage_factor / U256::from(10000)
    }

    /// Get the function selector from calldata.
    pub fn get_selector(calldata: &Bytes) -> Option<[u8; 4]> {
        if calldata.len() >= 4 {
            let mut selector = [0u8; 4];
            selector.copy_from_slice(&calldata[..4]);
            Some(selector)
        } else {
            None
        }
    }

    /// Check if calldata is for a V2 swap.
    pub fn is_v2_swap(calldata: &Bytes) -> bool {
        if let Some(selector) = Self::get_selector(calldata) {
            // swapExactTokensForTokens: 0x38ed1739
            // swapExactETHForTokens: 0x7ff36ab5
            // swapExactTokensForETH: 0x18cbafe5
            // swapTokensForExactTokens: 0x8803dbee
            matches!(
                selector,
                [0x38, 0xed, 0x17, 0x39]
                    | [0x7f, 0xf3, 0x6a, 0xb5]
                    | [0x18, 0xcb, 0xaf, 0xe5]
                    | [0x88, 0x03, 0xdb, 0xee]
            )
        } else {
            false
        }
    }

    /// Check if calldata is for a V3 swap.
    pub fn is_v3_swap(calldata: &Bytes) -> bool {
        if let Some(selector) = Self::get_selector(calldata) {
            // exactInputSingle: 0x414bf389
            // exactInput: 0xc04b8d59
            // exactOutputSingle: 0xdb3e2198
            // exactOutput: 0xf28c0498
            matches!(
                selector,
                [0x41, 0x4b, 0xf3, 0x89]
                    | [0xc0, 0x4b, 0x8d, 0x59]
                    | [0xdb, 0x3e, 0x21, 0x98]
                    | [0xf2, 0x8c, 0x04, 0x98]
            )
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_encode_v2_swap() {
        let weth: Address = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
            .parse()
            .unwrap();
        let usdc: Address = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"
            .parse()
            .unwrap();
        let recipient: Address = "0x1234567890123456789012345678901234567890"
            .parse()
            .unwrap();

        let calldata = RouterEncoder::encode_v2_swap_exact_tokens(
            U256::from(1000000000000000000u64), // 1 ETH
            U256::from(1900000000u64),          // 1900 USDC
            vec![weth, usdc],
            recipient,
            1234567890,
        );

        // Check selector is swapExactTokensForTokens
        assert_eq!(&calldata[..4], &[0x38, 0xed, 0x17, 0x39]);
        assert!(RouterEncoder::is_v2_swap(&calldata));
        assert!(!RouterEncoder::is_v3_swap(&calldata));
    }

    #[test]
    fn test_encode_v3_exact_input_single() {
        let weth: Address = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
            .parse()
            .unwrap();
        let usdc: Address = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"
            .parse()
            .unwrap();
        let recipient: Address = "0x1234567890123456789012345678901234567890"
            .parse()
            .unwrap();

        let calldata = RouterEncoder::encode_v3_exact_input_single(
            weth,
            usdc,
            3000, // 0.3% fee tier
            recipient,
            U256::from(1000000000000000000u64), // 1 ETH
            U256::from(1900000000u64),          // 1900 USDC min
            U256::ZERO,
        );

        // Check selector is exactInputSingle
        assert_eq!(&calldata[..4], &[0x41, 0x4b, 0xf3, 0x89]);
        assert!(!RouterEncoder::is_v2_swap(&calldata));
        assert!(RouterEncoder::is_v3_swap(&calldata));
    }

    #[test]
    fn test_encode_v3_path() {
        let weth: Address = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2"
            .parse()
            .unwrap();
        let usdc: Address = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"
            .parse()
            .unwrap();
        let dai: Address = "0x6B175474E89094C44Da98b954EescdeCB5C811111"
            .parse()
            .unwrap();

        let path = RouterEncoder::encode_v3_path(&[weth, usdc, dai], &[3000, 500]);

        // Path should be: weth (20 bytes) + fee (3 bytes) + usdc (20 bytes) + fee (3 bytes) + dai (20 bytes)
        assert_eq!(path.len(), 20 + 3 + 20 + 3 + 20);
    }

    #[test]
    fn test_encode_erc20_approve() {
        let spender: Address = "0x1234567890123456789012345678901234567890"
            .parse()
            .unwrap();
        let amount = U256::MAX;

        let calldata = RouterEncoder::encode_erc20_approve(spender, amount);

        // Check selector is approve
        assert_eq!(&calldata[..4], &[0x09, 0x5e, 0xa7, 0xb3]);
    }

    #[test]
    fn test_calculate_slippage() {
        let expected_output = U256::from(1000000u64); // 1M units

        // 0.5% slippage (50 bps)
        let min_output = RouterEncoder::calculate_min_output(expected_output, 50);
        assert_eq!(min_output, U256::from(995000u64)); // 1M * 0.995 = 995K

        // 1% slippage (100 bps)
        let min_output = RouterEncoder::calculate_min_output(expected_output, 100);
        assert_eq!(min_output, U256::from(990000u64)); // 1M * 0.99 = 990K
    }

    #[test]
    fn test_multicall_encoding() {
        let call1 = Bytes::from(vec![1, 2, 3, 4]);
        let call2 = Bytes::from(vec![5, 6, 7, 8]);

        let multicall = RouterEncoder::encode_multicall(1234567890, vec![call1, call2]);

        // Should have a valid selector
        assert!(multicall.len() > 4);
    }

    #[test]
    fn test_weth_encoding() {
        let deposit_data = RouterEncoder::encode_weth_deposit();
        // deposit() selector: 0xd0e30db0
        assert_eq!(&deposit_data[..4], &[0xd0, 0xe3, 0x0d, 0xb0]);

        let withdraw_data = RouterEncoder::encode_weth_withdraw(U256::from(1000000000000000000u64));
        // withdraw(uint256) selector: 0x2e1a7d4d
        assert_eq!(&withdraw_data[..4], &[0x2e, 0x1a, 0x7d, 0x4d]);
    }
}
