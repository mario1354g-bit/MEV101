//! Uniswap V3 implementation
//!
//! This module provides DEX interaction for Uniswap V3 concentrated liquidity pools.

use super::{addresses, fee_tiers, Dex, DexError, DexResult, PriceInfo, Reserves, SwapParams};
use alloy::primitives::{Address, Bytes, Uint, U256};
use alloy::providers::Provider;
use alloy::sol;
use alloy::sol_types::SolCall;
use alloy::transports::Transport;
use async_trait::async_trait;

// Uniswap V3 Pool interface
sol! {
    #[sol(rpc)]
    interface IUniswapV3Pool {
        function slot0() external view returns (
            uint160 sqrtPriceX96,
            int24 tick,
            uint16 observationIndex,
            uint16 observationCardinality,
            uint16 observationCardinalityNext,
            uint8 feeProtocol,
            bool unlocked
        );

        function liquidity() external view returns (uint128);
        function token0() external view returns (address);
        function token1() external view returns (address);
        function fee() external view returns (uint24);
        function tickSpacing() external view returns (int24);

        function ticks(int24 tick) external view returns (
            uint128 liquidityGross,
            int128 liquidityNet,
            uint256 feeGrowthOutside0X128,
            uint256 feeGrowthOutside1X128,
            int56 tickCumulativeOutside,
            uint160 secondsPerLiquidityOutsideX128,
            uint32 secondsOutside,
            bool initialized
        );

        function positions(bytes32 key) external view returns (
            uint128 liquidity,
            uint256 feeGrowthInside0LastX128,
            uint256 feeGrowthInside1LastX128,
            uint128 tokensOwed0,
            uint128 tokensOwed1
        );

        function observe(uint32[] calldata secondsAgos) external view returns (
            int56[] memory tickCumulatives,
            uint160[] memory secondsPerLiquidityCumulativeX128s
        );

        function swap(
            address recipient,
            bool zeroForOne,
            int256 amountSpecified,
            uint160 sqrtPriceLimitX96,
            bytes calldata data
        ) external returns (int256 amount0, int256 amount1);

        function flash(
            address recipient,
            uint256 amount0,
            uint256 amount1,
            bytes calldata data
        ) external;
    }
}

// Uniswap V3 SwapRouter interface
sol! {
    #[sol(rpc)]
    interface ISwapRouter {
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

        struct ExactInputParams {
            bytes path;
            address recipient;
            uint256 deadline;
            uint256 amountIn;
            uint256 amountOutMinimum;
        }

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

        struct ExactOutputParams {
            bytes path;
            address recipient;
            uint256 deadline;
            uint256 amountOut;
            uint256 amountInMaximum;
        }

        function exactInputSingle(ExactInputSingleParams calldata params) external payable returns (uint256 amountOut);
        function exactInput(ExactInputParams calldata params) external payable returns (uint256 amountOut);
        function exactOutputSingle(ExactOutputSingleParams calldata params) external payable returns (uint256 amountIn);
        function exactOutput(ExactOutputParams calldata params) external payable returns (uint256 amountIn);
    }
}

// Uniswap V3 SwapRouter02 interface (newer version with additional functions)
sol! {
    #[sol(rpc)]
    interface ISwapRouter02 {
        struct ExactInputSingleParams {
            address tokenIn;
            address tokenOut;
            uint24 fee;
            address recipient;
            uint256 amountIn;
            uint256 amountOutMinimum;
            uint160 sqrtPriceLimitX96;
        }

        struct ExactInputParams {
            bytes path;
            address recipient;
            uint256 amountIn;
            uint256 amountOutMinimum;
        }

        struct ExactOutputSingleParams {
            address tokenIn;
            address tokenOut;
            uint24 fee;
            address recipient;
            uint256 amountOut;
            uint256 amountInMaximum;
            uint160 sqrtPriceLimitX96;
        }

        struct ExactOutputParams {
            bytes path;
            address recipient;
            uint256 amountOut;
            uint256 amountInMaximum;
        }

        function exactInputSingle(ExactInputSingleParams calldata params) external payable returns (uint256 amountOut);
        function exactInput(ExactInputParams calldata params) external payable returns (uint256 amountOut);
        function exactOutputSingle(ExactOutputSingleParams calldata params) external payable returns (uint256 amountIn);
        function exactOutput(ExactOutputParams calldata params) external payable returns (uint256 amountIn);

        function multicall(uint256 deadline, bytes[] calldata data) external payable returns (bytes[] memory);
        function multicall(bytes[] calldata data) external payable returns (bytes[] memory);
        function multicall(bytes32 previousBlockhash, bytes[] calldata data) external payable returns (bytes[] memory);
    }
}

// Uniswap V3 Factory interface
sol! {
    #[sol(rpc)]
    interface IUniswapV3Factory {
        function getPool(address tokenA, address tokenB, uint24 fee) external view returns (address pool);
        function createPool(address tokenA, address tokenB, uint24 fee) external returns (address pool);
        function owner() external view returns (address);
        function feeAmountTickSpacing(uint24 fee) external view returns (int24);
    }
}

// Uniswap V3 Quoter interface
sol! {
    #[sol(rpc)]
    interface IQuoterV2 {
        struct QuoteExactInputSingleParams {
            address tokenIn;
            address tokenOut;
            uint256 amountIn;
            uint24 fee;
            uint160 sqrtPriceLimitX96;
        }

        struct QuoteExactOutputSingleParams {
            address tokenIn;
            address tokenOut;
            uint256 amount;
            uint24 fee;
            uint160 sqrtPriceLimitX96;
        }

        function quoteExactInputSingle(QuoteExactInputSingleParams memory params)
            external
            returns (
                uint256 amountOut,
                uint160 sqrtPriceX96After,
                uint32 initializedTicksCrossed,
                uint256 gasEstimate
            );

        function quoteExactOutputSingle(QuoteExactOutputSingleParams memory params)
            external
            returns (
                uint256 amountIn,
                uint160 sqrtPriceX96After,
                uint32 initializedTicksCrossed,
                uint256 gasEstimate
            );

        function quoteExactInput(bytes memory path, uint256 amountIn)
            external
            returns (
                uint256 amountOut,
                uint160[] memory sqrtPriceX96AfterList,
                uint32[] memory initializedTicksCrossedList,
                uint256 gasEstimate
            );

        function quoteExactOutput(bytes memory path, uint256 amountOut)
            external
            returns (
                uint256 amountIn,
                uint160[] memory sqrtPriceX96AfterList,
                uint32[] memory initializedTicksCrossedList,
                uint256 gasEstimate
            );
    }
}

/// Function selectors for Uniswap V3 Router functions
pub mod selectors {
    /// exactInputSingle selector (original router)
    pub const EXACT_INPUT_SINGLE: [u8; 4] = [0x41, 0x4b, 0xf3, 0x89];
    /// exactInput selector (original router)
    pub const EXACT_INPUT: [u8; 4] = [0xc0, 0x4b, 0x8d, 0x59];
    /// exactOutputSingle selector (original router)
    pub const EXACT_OUTPUT_SINGLE: [u8; 4] = [0xdb, 0x3e, 0x21, 0x98];
    /// exactOutput selector (original router)
    pub const EXACT_OUTPUT: [u8; 4] = [0xf2, 0x8c, 0x02, 0x98];

    /// exactInputSingle selector (router02 - no deadline in struct)
    pub const EXACT_INPUT_SINGLE_02: [u8; 4] = [0x04, 0xe4, 0x5a, 0xaf];
    /// exactInput selector (router02)
    pub const EXACT_INPUT_02: [u8; 4] = [0xb8, 0x58, 0x18, 0x3f];
    /// exactOutputSingle selector (router02)
    pub const EXACT_OUTPUT_SINGLE_02: [u8; 4] = [0x5a, 0x47, 0xdd, 0xc3];
    /// exactOutput selector (router02)
    pub const EXACT_OUTPUT_02: [u8; 4] = [0x09, 0xb8, 0x13, 0x46];

    /// multicall selector
    pub const MULTICALL: [u8; 4] = [0xac, 0x96, 0x50, 0xd8];
    /// multicall with deadline selector
    pub const MULTICALL_DEADLINE: [u8; 4] = [0x5a, 0xe4, 0x01, 0xdc];
}

/// Uniswap V3 DEX implementation
#[derive(Debug, Clone)]
pub struct UniswapV3 {
    /// Name of this DEX instance
    name: String,
    /// Router address (original SwapRouter)
    router: Address,
    /// Router02 address (newer version)
    router02: Address,
    /// Factory address
    factory: Address,
    /// Quoter V2 address
    quoter: Address,
    /// Whether to use Router02 by default
    use_router02: bool,
}

impl UniswapV3 {
    /// Create a new Uniswap V3 DEX instance
    pub fn new(
        name: impl Into<String>,
        router: Address,
        router02: Address,
        factory: Address,
        quoter: Address,
    ) -> Self {
        Self {
            name: name.into(),
            router,
            router02,
            factory,
            quoter,
            use_router02: true,
        }
    }

    /// Create a Uniswap V3 mainnet instance
    pub fn uniswap() -> Self {
        Self::new(
            "Uniswap V3",
            addresses::UNISWAP_V3_ROUTER,
            addresses::UNISWAP_V3_ROUTER_02,
            addresses::UNISWAP_V3_FACTORY,
            addresses::UNISWAP_V3_QUOTER_V2,
        )
    }

    /// Set whether to use Router02
    pub fn with_router02(mut self, use_router02: bool) -> Self {
        self.use_router02 = use_router02;
        self
    }

    /// Get the factory address
    pub fn factory_address(&self) -> Address {
        self.factory
    }

    /// Get the quoter address
    pub fn quoter_address(&self) -> Address {
        self.quoter
    }

    /// Get pool address for a token pair and fee tier
    pub async fn get_pool<T: Transport + Clone, P: Provider<T>>(
        &self,
        token_a: Address,
        token_b: Address,
        fee: u32,
        provider: &P,
    ) -> DexResult<Address> {
        if !fee_tiers::is_valid_fee(fee) {
            return Err(DexError::InvalidParameters(format!(
                "Invalid fee tier: {}",
                fee
            )));
        }

        let factory = IUniswapV3Factory::new(self.factory, provider);

        let pool = factory
            .getPool(token_a, token_b, fee.try_into().unwrap())
            .call()
            .await
            .map_err(|e| DexError::ContractCall(e.to_string()))?
            .pool;

        if pool == Address::ZERO {
            return Err(DexError::PoolNotFound(format!(
                "No pool found for {:?} and {:?} with fee {}",
                token_a, token_b, fee
            )));
        }

        Ok(pool)
    }

    /// Get slot0 data for a pool
    pub async fn get_slot0<T: Transport + Clone, P: Provider<T>>(
        &self,
        pool: &Address,
        provider: &P,
    ) -> DexResult<Slot0Data> {
        let pool_contract = IUniswapV3Pool::new(*pool, provider);

        let result = pool_contract
            .slot0()
            .call()
            .await
            .map_err(|e| DexError::ContractCall(e.to_string()))?;

        Ok(Slot0Data {
            sqrt_price_x96: U256::from(result.sqrtPriceX96),
            tick: result.tick.as_i32(),
            observation_index: result.observationIndex,
            observation_cardinality: result.observationCardinality,
            observation_cardinality_next: result.observationCardinalityNext,
            fee_protocol: result.feeProtocol,
            unlocked: result.unlocked,
        })
    }

    /// Get token addresses for a pool
    pub async fn get_pool_tokens<T: Transport + Clone, P: Provider<T>>(
        &self,
        pool: &Address,
        provider: &P,
    ) -> DexResult<(Address, Address)> {
        let pool_contract = IUniswapV3Pool::new(*pool, provider);

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

        Ok((token0, token1))
    }

    /// Get fee tier for a pool
    pub async fn get_pool_fee<T: Transport + Clone, P: Provider<T>>(
        &self,
        pool: &Address,
        provider: &P,
    ) -> DexResult<u32> {
        let pool_contract = IUniswapV3Pool::new(*pool, provider);

        let fee = pool_contract
            .fee()
            .call()
            .await
            .map_err(|e| DexError::ContractCall(e.to_string()))?
            ._0;

        Ok(fee.try_into().unwrap())
    }

    /// Get current liquidity for a pool
    pub async fn get_liquidity<T: Transport + Clone, P: Provider<T>>(
        &self,
        pool: &Address,
        provider: &P,
    ) -> DexResult<u128> {
        let pool_contract = IUniswapV3Pool::new(*pool, provider);

        let liquidity = pool_contract
            .liquidity()
            .call()
            .await
            .map_err(|e| DexError::ContractCall(e.to_string()))?
            ._0;

        Ok(liquidity)
    }

    /// Convert sqrtPriceX96 to actual price
    /// Returns price of token0 in terms of token1
    pub fn sqrt_price_x96_to_price(sqrt_price_x96: U256) -> f64 {
        // price = (sqrtPriceX96 / 2^96)^2
        // We compute this carefully to avoid overflow
        let q96: U256 = U256::from(1u128) << 96;

        // Convert to f64 for the calculation
        let sqrt_price_str = sqrt_price_x96.to_string();
        let q96_str = q96.to_string();

        let sqrt_price_f64: f64 = sqrt_price_str.parse().unwrap_or(0.0);
        let q96_f64: f64 = q96_str.parse().unwrap_or(1.0);

        let ratio = sqrt_price_f64 / q96_f64;
        ratio * ratio
    }

    /// Convert price to sqrtPriceX96
    pub fn price_to_sqrt_price_x96(price: f64) -> U256 {
        if price <= 0.0 {
            return U256::ZERO;
        }

        let sqrt_price = price.sqrt();
        let q96: f64 = 2.0_f64.powi(96);
        let result = sqrt_price * q96;

        // Convert to U256, handling potential overflow
        if result.is_infinite() || result.is_nan() {
            U256::MAX
        } else {
            U256::from(result as u128)
        }
    }

    /// Convert tick to price
    /// Returns price of token0 in terms of token1
    pub fn tick_to_price(tick: i32) -> f64 {
        // price = 1.0001^tick
        1.0001_f64.powi(tick)
    }

    /// Convert price to tick
    pub fn price_to_tick(price: f64) -> i32 {
        if price <= 0.0 {
            return i32::MIN;
        }
        // tick = log(price) / log(1.0001)
        (price.ln() / 1.0001_f64.ln()).round() as i32
    }

    /// Encode path for multi-hop swaps
    /// Path format: token0 (20 bytes) + fee (3 bytes) + token1 (20 bytes) + fee (3 bytes) + token2 (20 bytes) ...
    pub fn encode_path(tokens: &[Address], fees: &[u32]) -> DexResult<Bytes> {
        if tokens.len() < 2 {
            return Err(DexError::InvalidParameters(
                "Path must have at least 2 tokens".to_string(),
            ));
        }
        if fees.len() != tokens.len() - 1 {
            return Err(DexError::InvalidParameters(
                "Number of fees must be one less than number of tokens".to_string(),
            ));
        }

        let mut path = Vec::with_capacity(tokens.len() * 20 + fees.len() * 3);

        for (i, token) in tokens.iter().enumerate() {
            path.extend_from_slice(token.as_slice());
            if i < fees.len() {
                // Fee is 3 bytes, big-endian
                let fee_bytes = fees[i].to_be_bytes();
                path.extend_from_slice(&fee_bytes[1..4]); // Take last 3 bytes
            }
        }

        Ok(Bytes::from(path))
    }

    /// Decode path from bytes
    pub fn decode_path(path: &Bytes) -> DexResult<(Vec<Address>, Vec<u32>)> {
        if path.len() < 43 {
            // Minimum: 20 + 3 + 20 = 43
            return Err(DexError::Decoding("Path too short".to_string()));
        }

        let mut tokens = Vec::new();
        let mut fees = Vec::new();
        let mut offset = 0;

        // First token
        let addr_bytes: [u8; 20] = path[offset..offset + 20]
            .try_into()
            .map_err(|_| DexError::Decoding("Invalid address bytes".to_string()))?;
        tokens.push(Address::from(addr_bytes));
        offset += 20;

        // Remaining tokens and fees
        while offset + 23 <= path.len() {
            // Fee (3 bytes)
            let mut fee_bytes = [0u8; 4];
            fee_bytes[1..4].copy_from_slice(&path[offset..offset + 3]);
            let fee = u32::from_be_bytes(fee_bytes);
            fees.push(fee);
            offset += 3;

            // Token (20 bytes)
            let addr_bytes: [u8; 20] = path[offset..offset + 20]
                .try_into()
                .map_err(|_| DexError::Decoding("Invalid address bytes".to_string()))?;
            tokens.push(Address::from(addr_bytes));
            offset += 20;
        }

        Ok((tokens, fees))
    }
}

/// Slot0 data from a Uniswap V3 pool
#[derive(Debug, Clone)]
pub struct Slot0Data {
    /// sqrt(price) * 2^96
    pub sqrt_price_x96: U256,
    /// Current tick
    pub tick: i32,
    /// Most recently updated observation index
    pub observation_index: u16,
    /// Current maximum number of observations
    pub observation_cardinality: u16,
    /// Next maximum number of observations
    pub observation_cardinality_next: u16,
    /// Protocol fee
    pub fee_protocol: u8,
    /// Whether pool is unlocked
    pub unlocked: bool,
}

#[async_trait]
impl Dex for UniswapV3 {
    fn name(&self) -> &str {
        &self.name
    }

    async fn get_price<T: Transport + Clone, P: Provider<T>>(
        &self,
        pool: &Address,
        provider: &P,
    ) -> DexResult<PriceInfo> {
        let slot0 = self.get_slot0(pool, provider).await?;
        let (token0, token1) = self.get_pool_tokens(pool, provider).await?;
        let liquidity = self.get_liquidity(pool, provider).await?;

        let block_number = provider
            .get_block_number()
            .await
            .map_err(|e| DexError::Provider(e.to_string()))?;

        let price = Self::sqrt_price_x96_to_price(slot0.sqrt_price_x96);

        // For V3, liquidity is in a different format
        // We provide the sqrt_price_x96 as liquidity values for now
        Ok(PriceInfo {
            token0,
            token1,
            price,
            liquidity_token0: U256::from(liquidity),
            liquidity_token1: slot0.sqrt_price_x96,
            block_number,
        })
    }

    async fn get_reserves<T: Transport + Clone, P: Provider<T>>(
        &self,
        pool: &Address,
        provider: &P,
    ) -> DexResult<Reserves> {
        // V3 doesn't have traditional reserves, but we can provide
        // liquidity information in a similar format
        let slot0 = self.get_slot0(pool, provider).await?;
        let liquidity = self.get_liquidity(pool, provider).await?;

        // For V3, we return liquidity and sqrt_price as "reserves"
        Ok(Reserves {
            reserve0: U256::from(liquidity),
            reserve1: slot0.sqrt_price_x96,
            block_timestamp_last: 0, // V3 doesn't track this the same way
        })
    }

    fn encode_swap(&self, params: &SwapParams) -> DexResult<Bytes> {
        let fee = params.fee.unwrap_or(fee_tiers::FEE_MEDIUM);

        if !fee_tiers::is_valid_fee(fee) {
            return Err(DexError::InvalidParameters(format!(
                "Invalid fee tier: {}",
                fee
            )));
        }

        // Use Router02 format (no deadline in struct, more common)
        if self.use_router02 {
            let calldata = ISwapRouter02::exactInputSingleCall {
                params: ISwapRouter02::ExactInputSingleParams {
                    tokenIn: params.token_in,
                    tokenOut: params.token_out,
                    fee: fee.try_into().unwrap(),
                    recipient: params.recipient,
                    amountIn: params.amount_in,
                    amountOutMinimum: params.amount_out_min,
                    sqrtPriceLimitX96: Uint::<160, 3>::ZERO, // No price limit
                },
            }
            .abi_encode();

            Ok(Bytes::from(calldata))
        } else {
            // Use original Router format
            let calldata = ISwapRouter::exactInputSingleCall {
                params: ISwapRouter::ExactInputSingleParams {
                    tokenIn: params.token_in,
                    tokenOut: params.token_out,
                    fee: fee.try_into().unwrap(),
                    recipient: params.recipient,
                    deadline: params.deadline,
                    amountIn: params.amount_in,
                    amountOutMinimum: params.amount_out_min,
                    sqrtPriceLimitX96: Uint::<160, 3>::ZERO,
                },
            }
            .abi_encode();

            Ok(Bytes::from(calldata))
        }
    }

    fn decode_swap_input(&self, data: &Bytes) -> DexResult<SwapParams> {
        if data.len() < 4 {
            return Err(DexError::Decoding("Data too short".to_string()));
        }

        let selector: [u8; 4] = data[0..4]
            .try_into()
            .map_err(|_| DexError::Decoding("Invalid selector".to_string()))?;

        match selector {
            selectors::EXACT_INPUT_SINGLE => {
                let decoded =
                    ISwapRouter::exactInputSingleCall::abi_decode(&data[4..], true)
                        .map_err(|e| DexError::Decoding(e.to_string()))?;

                Ok(SwapParams {
                    token_in: decoded.params.tokenIn,
                    token_out: decoded.params.tokenOut,
                    amount_in: decoded.params.amountIn,
                    amount_out_min: decoded.params.amountOutMinimum,
                    recipient: decoded.params.recipient,
                    deadline: decoded.params.deadline,
                    path: vec![decoded.params.tokenIn, decoded.params.tokenOut],
                    fee: Some(decoded.params.fee.try_into().unwrap()),
                })
            }
            selectors::EXACT_INPUT_SINGLE_02 => {
                let decoded =
                    ISwapRouter02::exactInputSingleCall::abi_decode(&data[4..], true)
                        .map_err(|e| DexError::Decoding(e.to_string()))?;

                Ok(SwapParams {
                    token_in: decoded.params.tokenIn,
                    token_out: decoded.params.tokenOut,
                    amount_in: decoded.params.amountIn,
                    amount_out_min: decoded.params.amountOutMinimum,
                    recipient: decoded.params.recipient,
                    deadline: U256::MAX, // No deadline in Router02
                    path: vec![decoded.params.tokenIn, decoded.params.tokenOut],
                    fee: Some(decoded.params.fee.try_into().unwrap()),
                })
            }
            selectors::EXACT_INPUT => {
                let decoded = ISwapRouter::exactInputCall::abi_decode(&data[4..], true)
                    .map_err(|e| DexError::Decoding(e.to_string()))?;

                let (tokens, fees) = Self::decode_path(&decoded.params.path)?;
                let fee = fees.first().copied();

                Ok(SwapParams {
                    token_in: tokens[0],
                    token_out: tokens[tokens.len() - 1],
                    amount_in: decoded.params.amountIn,
                    amount_out_min: decoded.params.amountOutMinimum,
                    recipient: decoded.params.recipient,
                    deadline: decoded.params.deadline,
                    path: tokens,
                    fee,
                })
            }
            selectors::EXACT_INPUT_02 => {
                let decoded = ISwapRouter02::exactInputCall::abi_decode(&data[4..], true)
                    .map_err(|e| DexError::Decoding(e.to_string()))?;

                let (tokens, fees) = Self::decode_path(&decoded.params.path)?;
                let fee = fees.first().copied();

                Ok(SwapParams {
                    token_in: tokens[0],
                    token_out: tokens[tokens.len() - 1],
                    amount_in: decoded.params.amountIn,
                    amount_out_min: decoded.params.amountOutMinimum,
                    recipient: decoded.params.recipient,
                    deadline: U256::MAX,
                    path: tokens,
                    fee,
                })
            }
            selectors::EXACT_OUTPUT_SINGLE => {
                let decoded =
                    ISwapRouter::exactOutputSingleCall::abi_decode(&data[4..], true)
                        .map_err(|e| DexError::Decoding(e.to_string()))?;

                Ok(SwapParams {
                    token_in: decoded.params.tokenIn,
                    token_out: decoded.params.tokenOut,
                    amount_in: decoded.params.amountInMaximum,
                    amount_out_min: decoded.params.amountOut, // Exact output
                    recipient: decoded.params.recipient,
                    deadline: decoded.params.deadline,
                    path: vec![decoded.params.tokenIn, decoded.params.tokenOut],
                    fee: Some(decoded.params.fee.try_into().unwrap()),
                })
            }
            selectors::EXACT_OUTPUT_SINGLE_02 => {
                let decoded =
                    ISwapRouter02::exactOutputSingleCall::abi_decode(&data[4..], true)
                        .map_err(|e| DexError::Decoding(e.to_string()))?;

                Ok(SwapParams {
                    token_in: decoded.params.tokenIn,
                    token_out: decoded.params.tokenOut,
                    amount_in: decoded.params.amountInMaximum,
                    amount_out_min: decoded.params.amountOut,
                    recipient: decoded.params.recipient,
                    deadline: U256::MAX,
                    path: vec![decoded.params.tokenIn, decoded.params.tokenOut],
                    fee: Some(decoded.params.fee.try_into().unwrap()),
                })
            }
            selectors::EXACT_OUTPUT => {
                let decoded = ISwapRouter::exactOutputCall::abi_decode(&data[4..], true)
                    .map_err(|e| DexError::Decoding(e.to_string()))?;

                let (tokens, fees) = Self::decode_path(&decoded.params.path)?;
                let fee = fees.first().copied();

                // For exact output, path is reversed
                Ok(SwapParams {
                    token_in: tokens[tokens.len() - 1],
                    token_out: tokens[0],
                    amount_in: decoded.params.amountInMaximum,
                    amount_out_min: decoded.params.amountOut,
                    recipient: decoded.params.recipient,
                    deadline: decoded.params.deadline,
                    path: tokens.into_iter().rev().collect(),
                    fee,
                })
            }
            selectors::EXACT_OUTPUT_02 => {
                let decoded = ISwapRouter02::exactOutputCall::abi_decode(&data[4..], true)
                    .map_err(|e| DexError::Decoding(e.to_string()))?;

                let (tokens, fees) = Self::decode_path(&decoded.params.path)?;
                let fee = fees.first().copied();

                Ok(SwapParams {
                    token_in: tokens[tokens.len() - 1],
                    token_out: tokens[0],
                    amount_in: decoded.params.amountInMaximum,
                    amount_out_min: decoded.params.amountOut,
                    recipient: decoded.params.recipient,
                    deadline: U256::MAX,
                    path: tokens.into_iter().rev().collect(),
                    fee,
                })
            }
            _ => Err(DexError::UnknownSelector(hex::encode(selector))),
        }
    }

    fn router_address(&self) -> Address {
        if self.use_router02 {
            self.router02
        } else {
            self.router
        }
    }

    fn get_amount_out(&self, amount_in: U256, reserve_in: U256, reserve_out: U256) -> U256 {
        // V3 uses a different AMM formula based on concentrated liquidity
        // This is a simplified approximation for price estimation
        // For accurate quotes, use the Quoter contract
        if amount_in.is_zero() || reserve_in.is_zero() || reserve_out.is_zero() {
            return U256::ZERO;
        }

        // Simple constant product approximation (not accurate for V3)
        // Real V3 calculations require tick-by-tick simulation
        let numerator = amount_in * reserve_out;
        let denominator = reserve_in + amount_in;

        if denominator.is_zero() {
            U256::ZERO
        } else {
            numerator / denominator
        }
    }

    fn get_amount_in(&self, amount_out: U256, reserve_in: U256, reserve_out: U256) -> U256 {
        // V3 uses a different AMM formula
        // This is a simplified approximation
        if amount_out.is_zero() || reserve_in.is_zero() || reserve_out.is_zero() {
            return U256::ZERO;
        }

        if amount_out >= reserve_out {
            return U256::MAX;
        }

        let numerator = reserve_in * amount_out;
        let denominator = reserve_out - amount_out;

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
    fn test_uniswap_v3_creation() {
        let dex = UniswapV3::uniswap();
        assert_eq!(dex.name(), "Uniswap V3");
        assert_eq!(dex.router_address(), addresses::UNISWAP_V3_ROUTER_02);
        assert_eq!(dex.factory_address(), addresses::UNISWAP_V3_FACTORY);
    }

    #[test]
    fn test_sqrt_price_conversion() {
        // Test with a known value
        // sqrtPriceX96 = sqrt(price) * 2^96
        // For price = 1, sqrtPriceX96 = 2^96 ≈ 79228162514264337593543950336

        let sqrt_price_x96 = U256::from(1u128) << 96;
        let price = UniswapV3::sqrt_price_x96_to_price(sqrt_price_x96);
        assert!((price - 1.0).abs() < 0.0001);
    }

    #[test]
    fn test_tick_to_price() {
        // tick 0 should give price 1.0
        let price = UniswapV3::tick_to_price(0);
        assert!((price - 1.0).abs() < 0.0001);

        // tick 100 should give approximately 1.0001^100 ≈ 1.01005
        let price = UniswapV3::tick_to_price(100);
        assert!((price - 1.01005).abs() < 0.001);
    }

    #[test]
    fn test_price_to_tick() {
        // price 1.0 should give tick 0
        let tick = UniswapV3::price_to_tick(1.0);
        assert_eq!(tick, 0);

        // Round trip test
        let original_tick = 1234;
        let price = UniswapV3::tick_to_price(original_tick);
        let recovered_tick = UniswapV3::price_to_tick(price);
        assert_eq!(recovered_tick, original_tick);
    }

    #[test]
    fn test_encode_decode_path() {
        let tokens = vec![
            Address::repeat_byte(1),
            Address::repeat_byte(2),
            Address::repeat_byte(3),
        ];
        let fees = vec![3000, 500];

        let encoded = UniswapV3::encode_path(&tokens, &fees).unwrap();
        let (decoded_tokens, decoded_fees) = UniswapV3::decode_path(&encoded).unwrap();

        assert_eq!(tokens, decoded_tokens);
        assert_eq!(fees, decoded_fees);
    }

    #[test]
    fn test_encode_path_invalid() {
        // Too few tokens
        let result = UniswapV3::encode_path(&[Address::ZERO], &[]);
        assert!(result.is_err());

        // Wrong number of fees
        let result = UniswapV3::encode_path(
            &[Address::ZERO, Address::repeat_byte(1)],
            &[3000, 500], // Should only have 1 fee
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_encode_swap_exact_input_single() {
        let dex = UniswapV3::uniswap();

        let params = SwapParams {
            token_in: Address::repeat_byte(1),
            token_out: Address::repeat_byte(2),
            amount_in: U256::from(1000u64),
            amount_out_min: U256::from(900u64),
            recipient: Address::repeat_byte(3),
            deadline: U256::from(1700000000u64),
            path: vec![Address::repeat_byte(1), Address::repeat_byte(2)],
            fee: Some(3000),
        };

        let encoded = dex.encode_swap(&params).unwrap();
        assert!(!encoded.is_empty());

        // Verify selector (Router02 exactInputSingle)
        let selector: [u8; 4] = encoded[0..4].try_into().unwrap();
        assert_eq!(selector, selectors::EXACT_INPUT_SINGLE_02);
    }

    #[test]
    fn test_decode_swap_exact_input_single_02() {
        let dex = UniswapV3::uniswap();

        let original_params = SwapParams {
            token_in: Address::repeat_byte(1),
            token_out: Address::repeat_byte(2),
            amount_in: U256::from(1000u64),
            amount_out_min: U256::from(900u64),
            recipient: Address::repeat_byte(3),
            deadline: U256::from(1700000000u64),
            path: vec![Address::repeat_byte(1), Address::repeat_byte(2)],
            fee: Some(3000),
        };

        let encoded = dex.encode_swap(&original_params).unwrap();
        let decoded = dex.decode_swap_input(&encoded).unwrap();

        assert_eq!(decoded.token_in, original_params.token_in);
        assert_eq!(decoded.token_out, original_params.token_out);
        assert_eq!(decoded.amount_in, original_params.amount_in);
        assert_eq!(decoded.amount_out_min, original_params.amount_out_min);
        assert_eq!(decoded.recipient, original_params.recipient);
        assert_eq!(decoded.fee, original_params.fee);
    }

    #[test]
    fn test_invalid_fee_tier() {
        let dex = UniswapV3::uniswap();

        let params = SwapParams {
            token_in: Address::repeat_byte(1),
            token_out: Address::repeat_byte(2),
            amount_in: U256::from(1000u64),
            amount_out_min: U256::from(900u64),
            recipient: Address::repeat_byte(3),
            deadline: U256::from(1700000000u64),
            path: vec![Address::repeat_byte(1), Address::repeat_byte(2)],
            fee: Some(1234), // Invalid fee tier
        };

        let result = dex.encode_swap(&params);
        assert!(result.is_err());
    }
}
