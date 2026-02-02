// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

/**
 * @title IUniswapV2Router02
 * @notice Interface for Uniswap V2 Router swaps
 *
 * SECURITY CONSIDERATIONS:
 * - Always use deadline parameter to prevent stale transactions
 * - Use amountOutMin/amountInMax for slippage protection
 * - Approve exact amounts needed, reset to 0 after swaps
 * - For non-standard tokens (e.g., USDT), reset approval to 0 before setting new value
 * - Verify router address is legitimate before interaction
 */
interface IUniswapV2Router02 {
    /**
     * @notice Swaps exact tokens for tokens
     * @param amountIn Amount of input tokens
     * @param amountOutMin Minimum output tokens to receive
     * @param path Array of token addresses for the swap path
     * @param to Recipient address
     * @param deadline Transaction deadline timestamp
     * @return amounts Array of amounts for each swap in the path
     */
    function swapExactTokensForTokens(
        uint256 amountIn,
        uint256 amountOutMin,
        address[] calldata path,
        address to,
        uint256 deadline
    ) external returns (uint256[] memory amounts);

    /**
     * @notice Swaps tokens for exact output amount
     * @param amountOut Exact output amount desired
     * @param amountInMax Maximum input amount willing to spend
     * @param path Array of token addresses for the swap path
     * @param to Recipient address
     * @param deadline Transaction deadline timestamp
     * @return amounts Array of amounts for each swap in the path
     */
    function swapTokensForExactTokens(
        uint256 amountOut,
        uint256 amountInMax,
        address[] calldata path,
        address to,
        uint256 deadline
    ) external returns (uint256[] memory amounts);

    /**
     * @notice Calculate output amounts for a given input amount
     * @param amountIn Input amount
     * @param path Array of token addresses for the swap path
     * @return amounts Array of output amounts for each swap
     */
    function getAmountsOut(
        uint256 amountIn,
        address[] calldata path
    ) external view returns (uint256[] memory amounts);

    /**
     * @notice Calculate input amounts for a given output amount
     * @param amountOut Desired output amount
     * @param path Array of token addresses for the swap path
     * @return amounts Array of input amounts for each swap
     */
    function getAmountsIn(
        uint256 amountOut,
        address[] calldata path
    ) external view returns (uint256[] memory amounts);

    /**
     * @notice Swap exact ETH for tokens
     * @param amountOutMin Minimum tokens to receive
     * @param path Array of token addresses (WETH first)
     * @param to Recipient address
     * @param deadline Transaction deadline timestamp
     * @return amounts Array of amounts for each swap
     */
    function swapExactETHForTokens(
        uint256 amountOutMin,
        address[] calldata path,
        address to,
        uint256 deadline
    ) external payable returns (uint256[] memory amounts);

    /**
     * @notice Swap exact tokens for ETH
     * @param amountIn Amount of tokens to swap
     * @param amountOutMin Minimum ETH to receive
     * @param path Array of token addresses (WETH last)
     * @param to Recipient address
     * @param deadline Transaction deadline timestamp
     * @return amounts Array of amounts for each swap
     */
    function swapExactTokensForETH(
        uint256 amountIn,
        uint256 amountOutMin,
        address[] calldata path,
        address to,
        uint256 deadline
    ) external returns (uint256[] memory amounts);

    /**
     * @notice Get WETH address
     * @return WETH contract address
     */
    function WETH() external view returns (address);

    /**
     * @notice Get factory address
     * @return Factory contract address
     */
    function factory() external view returns (address);
}

/**
 * @title IUniswapV2Pair
 * @notice Interface for Uniswap V2 pair direct interaction
 *
 * SECURITY CONSIDERATIONS:
 * - Direct pair interaction bypasses router safety checks
 * - Must transfer tokens to pair before calling swap
 * - No slippage protection built-in - calculate amounts off-chain
 * - Verify pair address is legitimate (created by factory)
 * - Empty data parameter disables flash swap callback
 */
interface IUniswapV2Pair {
    /**
     * @notice Get pair reserves
     * @return reserve0 Reserve of token0
     * @return reserve1 Reserve of token1
     * @return blockTimestampLast Last block timestamp
     */
    function getReserves()
        external
        view
        returns (uint112 reserve0, uint112 reserve1, uint32 blockTimestampLast);

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
     * @notice Execute swap on pair directly
     * @param amount0Out Amount of token0 to receive
     * @param amount1Out Amount of token1 to receive
     * @param to Recipient address
     * @param data Callback data for flash swaps
     */
    function swap(
        uint256 amount0Out,
        uint256 amount1Out,
        address to,
        bytes calldata data
    ) external;

    /**
     * @notice Sync reserves to balances
     */
    function sync() external;

    /**
     * @notice Skim excess tokens to address
     * @param to Recipient of excess tokens
     */
    function skim(address to) external;

    /**
     * @notice Get cumulative price0
     * @return Cumulative price of token0
     */
    function price0CumulativeLast() external view returns (uint256);

    /**
     * @notice Get cumulative price1
     * @return Cumulative price of token1
     */
    function price1CumulativeLast() external view returns (uint256);

    /**
     * @notice Get kLast value
     * @return Last k value (reserve0 * reserve1)
     */
    function kLast() external view returns (uint256);
}

/**
 * @title IUniswapV2Factory
 * @notice Interface for Uniswap V2 factory
 */
interface IUniswapV2Factory {
    /**
     * @notice Get pair address for two tokens
     * @param tokenA First token address
     * @param tokenB Second token address
     * @return pair Pair address (or zero if doesn't exist)
     */
    function getPair(
        address tokenA,
        address tokenB
    ) external view returns (address pair);

    /**
     * @notice Create a new pair
     * @param tokenA First token address
     * @param tokenB Second token address
     * @return pair New pair address
     */
    function createPair(
        address tokenA,
        address tokenB
    ) external returns (address pair);

    /**
     * @notice Get all pairs length
     * @return Number of pairs created
     */
    function allPairsLength() external view returns (uint256);

    /**
     * @notice Get pair at index
     * @param index Index of the pair
     * @return pair Pair address at index
     */
    function allPairs(uint256 index) external view returns (address pair);
}
