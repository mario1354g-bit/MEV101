// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

/**
 * @title ISwapRouter
 * @notice Interface for Uniswap V3 SwapRouter
 *
 * SECURITY CONSIDERATIONS:
 * - Use amountOutMinimum for slippage protection (never set to 0 in production)
 * - sqrtPriceLimitX96 can limit price impact (0 = no limit)
 * - Approve exact amounts needed, reset to 0 after swaps
 * - For non-standard tokens, reset approval to 0 before setting new value
 * - Verify router address is legitimate before interaction
 * - deadline parameter removed in newer versions - use block.timestamp
 */
interface ISwapRouter {
    /**
     * @notice Parameters for single-hop exact input swap
     */
    struct ExactInputSingleParams {
        address tokenIn;
        address tokenOut;
        uint24 fee;
        address recipient;
        uint256 amountIn;
        uint256 amountOutMinimum;
        uint160 sqrtPriceLimitX96;
    }

    /**
     * @notice Execute single-hop exact input swap
     * @param params Swap parameters
     * @return amountOut Amount of tokens received
     */
    function exactInputSingle(
        ExactInputSingleParams calldata params
    ) external payable returns (uint256 amountOut);

    /**
     * @notice Parameters for multi-hop exact input swap
     */
    struct ExactInputParams {
        bytes path;
        address recipient;
        uint256 amountIn;
        uint256 amountOutMinimum;
    }

    /**
     * @notice Execute multi-hop exact input swap
     * @param params Swap parameters with encoded path
     * @return amountOut Amount of tokens received
     */
    function exactInput(
        ExactInputParams calldata params
    ) external payable returns (uint256 amountOut);

    /**
     * @notice Parameters for single-hop exact output swap
     */
    struct ExactOutputSingleParams {
        address tokenIn;
        address tokenOut;
        uint24 fee;
        address recipient;
        uint256 amountOut;
        uint256 amountInMaximum;
        uint160 sqrtPriceLimitX96;
    }

    /**
     * @notice Execute single-hop exact output swap
     * @param params Swap parameters
     * @return amountIn Amount of tokens spent
     */
    function exactOutputSingle(
        ExactOutputSingleParams calldata params
    ) external payable returns (uint256 amountIn);

    /**
     * @notice Parameters for multi-hop exact output swap
     */
    struct ExactOutputParams {
        bytes path;
        address recipient;
        uint256 amountOut;
        uint256 amountInMaximum;
    }

    /**
     * @notice Execute multi-hop exact output swap
     * @param params Swap parameters with encoded path
     * @return amountIn Amount of tokens spent
     */
    function exactOutput(
        ExactOutputParams calldata params
    ) external payable returns (uint256 amountIn);
}

/**
 * @title IUniswapV3Pool
 * @notice Interface for Uniswap V3 Pool
 *
 * SECURITY CONSIDERATIONS:
 * - Direct pool interaction requires proper callback implementation
 * - swap() requires uniswapV3SwapCallback implementation
 * - flash() requires uniswapV3FlashCallback implementation
 * - Verify pool address is legitimate (created by factory)
 * - sqrtPriceLimitX96 prevents execution at unfavorable prices
 */
interface IUniswapV3Pool {
    /**
     * @notice Get pool slot0 data
     * @return sqrtPriceX96 Current sqrt price as Q64.96
     * @return tick Current tick
     * @return observationIndex Current observation index
     * @return observationCardinality Current observation cardinality
     * @return observationCardinalityNext Next observation cardinality
     * @return feeProtocol Protocol fee
     * @return unlocked Pool lock status
     */
    function slot0()
        external
        view
        returns (
            uint160 sqrtPriceX96,
            int24 tick,
            uint16 observationIndex,
            uint16 observationCardinality,
            uint16 observationCardinalityNext,
            uint8 feeProtocol,
            bool unlocked
        );

    /**
     * @notice Get pool liquidity
     * @return Current liquidity in the pool
     */
    function liquidity() external view returns (uint128);

    /**
     * @notice Get token0 address
     * @return Token0 address
     */
    function token0() external view returns (address);

    /**
     * @notice Get token1 address
     * @return Token1 address
     */
    function token1() external view returns (address);

    /**
     * @notice Get pool fee tier
     * @return Fee tier in hundredths of a bip
     */
    function fee() external view returns (uint24);

    /**
     * @notice Get tick spacing
     * @return Tick spacing for the pool
     */
    function tickSpacing() external view returns (int24);

    /**
     * @notice Get max liquidity per tick
     * @return Max liquidity that can be in any tick
     */
    function maxLiquidityPerTick() external view returns (uint128);

    /**
     * @notice Get tick info
     * @param tick The tick to query
     * @return liquidityGross Total liquidity referencing this tick
     * @return liquidityNet Net liquidity when tick is crossed
     * @return feeGrowthOutside0X128 Fee growth on token0 outside tick
     * @return feeGrowthOutside1X128 Fee growth on token1 outside tick
     * @return tickCumulativeOutside Cumulative tick value outside
     * @return secondsPerLiquidityOutsideX128 Seconds per liquidity outside
     * @return secondsOutside Seconds outside tick range
     * @return initialized Whether tick is initialized
     */
    function ticks(
        int24 tick
    )
        external
        view
        returns (
            uint128 liquidityGross,
            int128 liquidityNet,
            uint256 feeGrowthOutside0X128,
            uint256 feeGrowthOutside1X128,
            int56 tickCumulativeOutside,
            uint160 secondsPerLiquidityOutsideX128,
            uint32 secondsOutside,
            bool initialized
        );

    /**
     * @notice Execute swap on pool
     * @param recipient Recipient of output tokens
     * @param zeroForOne Direction of swap (true = token0 -> token1)
     * @param amountSpecified Amount to swap (positive = exact input)
     * @param sqrtPriceLimitX96 Price limit for the swap
     * @param data Callback data
     * @return amount0 Change in token0 balance
     * @return amount1 Change in token1 balance
     */
    function swap(
        address recipient,
        bool zeroForOne,
        int256 amountSpecified,
        uint160 sqrtPriceLimitX96,
        bytes calldata data
    ) external returns (int256 amount0, int256 amount1);

    /**
     * @notice Flash loan from pool
     * @param recipient Recipient of flash loaned tokens
     * @param amount0 Amount of token0 to flash loan
     * @param amount1 Amount of token1 to flash loan
     * @param data Callback data
     */
    function flash(
        address recipient,
        uint256 amount0,
        uint256 amount1,
        bytes calldata data
    ) external;

    /**
     * @notice Get observation at index
     * @param index Observation index
     * @return blockTimestamp Observation timestamp
     * @return tickCumulative Cumulative tick
     * @return secondsPerLiquidityCumulativeX128 Seconds per liquidity
     * @return initialized Whether observation is initialized
     */
    function observations(
        uint256 index
    )
        external
        view
        returns (
            uint32 blockTimestamp,
            int56 tickCumulative,
            uint160 secondsPerLiquidityCumulativeX128,
            bool initialized
        );
}

/**
 * @title IUniswapV3Factory
 * @notice Interface for Uniswap V3 Factory
 */
interface IUniswapV3Factory {
    /**
     * @notice Get pool address for token pair and fee
     * @param tokenA First token address
     * @param tokenB Second token address
     * @param fee Fee tier
     * @return pool Pool address
     */
    function getPool(
        address tokenA,
        address tokenB,
        uint24 fee
    ) external view returns (address pool);

    /**
     * @notice Create a new pool
     * @param tokenA First token address
     * @param tokenB Second token address
     * @param fee Fee tier
     * @return pool New pool address
     */
    function createPool(
        address tokenA,
        address tokenB,
        uint24 fee
    ) external returns (address pool);

    /**
     * @notice Get fee amount tick spacing
     * @param fee Fee tier
     * @return Tick spacing for fee
     */
    function feeAmountTickSpacing(uint24 fee) external view returns (int24);
}

/**
 * @title IQuoterV2
 * @notice Interface for Uniswap V3 Quoter
 */
interface IQuoterV2 {
    /**
     * @notice Quote parameters for single-hop swap
     */
    struct QuoteExactInputSingleParams {
        address tokenIn;
        address tokenOut;
        uint256 amountIn;
        uint24 fee;
        uint160 sqrtPriceLimitX96;
    }

    /**
     * @notice Get quote for single-hop exact input
     * @param params Quote parameters
     * @return amountOut Expected output amount
     * @return sqrtPriceX96After Price after swap
     * @return initializedTicksCrossed Number of ticks crossed
     * @return gasEstimate Estimated gas cost
     */
    function quoteExactInputSingle(
        QuoteExactInputSingleParams memory params
    )
        external
        returns (
            uint256 amountOut,
            uint160 sqrtPriceX96After,
            uint32 initializedTicksCrossed,
            uint256 gasEstimate
        );

    /**
     * @notice Get quote for multi-hop exact input
     * @param path Encoded swap path
     * @param amountIn Input amount
     * @return amountOut Expected output amount
     * @return sqrtPriceX96AfterList Prices after each swap
     * @return initializedTicksCrossedList Ticks crossed in each swap
     * @return gasEstimate Estimated gas cost
     */
    function quoteExactInput(
        bytes memory path,
        uint256 amountIn
    )
        external
        returns (
            uint256 amountOut,
            uint160[] memory sqrtPriceX96AfterList,
            uint32[] memory initializedTicksCrossedList,
            uint256 gasEstimate
        );
}
