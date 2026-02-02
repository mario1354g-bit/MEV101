// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

/**
 * @title ICurvePool
 * @notice Interface for Curve Finance pool swaps
 *
 * SECURITY CONSIDERATIONS:
 * - Curve pools use signed int128 for coin indices (historical design)
 * - Always verify pool address is legitimate before interaction
 * - Use min_dy parameter for slippage protection
 * - Some pools use exchange() others use exchange_underlying()
 * - ETH pools may require payable calls and use coin index 0 for ETH
 * - Approve exact amounts needed, reset to 0 after swaps
 *
 * SUPPORTED POOLS:
 * - 3pool (DAI/USDC/USDT): 0xbEbc44782C7dB0a1A60Cb6fe97d0b483032FF1C7
 *   - Coin indices: 0=DAI, 1=USDC, 2=USDT
 * - stETH pool (ETH/stETH): 0xDC24316b9AE028F1497c275EB9192a3Ea0f67022
 *   - Coin indices: 0=ETH, 1=stETH
 * - FRAX/USDC: 0xDcEF968d416a41Cdac0ED8702fAC8128A64241A2
 *   - Coin indices: 0=FRAX, 1=USDC
 */
interface ICurvePool {
    /**
     * @notice Exchange tokens in the pool
     * @param i Index of input coin (0-based)
     * @param j Index of output coin (0-based)
     * @param dx Amount of input coin to swap
     * @param min_dy Minimum amount of output coin to receive
     * @return dy Amount of output coin received
     * @dev Uses int128 for indices due to Curve's historical Vyper implementation
     *      For ETH pools, use ICurvePoolETH interface instead
     */
    function exchange(
        int128 i,
        int128 j,
        uint256 dx,
        uint256 min_dy
    ) external returns (uint256 dy);

    /**
     * @notice Exchange underlying tokens (for metapools/lending pools)
     * @param i Index of input coin (0-based)
     * @param j Index of output coin (0-based)
     * @param dx Amount of input coin to swap
     * @param min_dy Minimum amount of output coin to receive
     * @return dy Amount of output coin received
     */
    function exchange_underlying(
        int128 i,
        int128 j,
        uint256 dx,
        uint256 min_dy
    ) external returns (uint256 dy);

    /**
     * @notice Get expected output amount for a swap
     * @param i Index of input coin
     * @param j Index of output coin
     * @param dx Amount of input coin
     * @return Expected output amount
     */
    function get_dy(
        int128 i,
        int128 j,
        uint256 dx
    ) external view returns (uint256);

    /**
     * @notice Get expected output amount for underlying swap
     * @param i Index of input coin
     * @param j Index of output coin
     * @param dx Amount of input coin
     * @return Expected output amount
     */
    function get_dy_underlying(
        int128 i,
        int128 j,
        uint256 dx
    ) external view returns (uint256);

    /**
     * @notice Get coin address at index
     * @param i Coin index
     * @return Coin address
     */
    function coins(uint256 i) external view returns (address);

    /**
     * @notice Get underlying coin address at index (for metapools)
     * @param i Coin index
     * @return Underlying coin address
     */
    function underlying_coins(uint256 i) external view returns (address);

    /**
     * @notice Get pool balance for a coin
     * @param i Coin index
     * @return Balance of coin in pool
     */
    function balances(uint256 i) external view returns (uint256);

    /**
     * @notice Get virtual price of LP token
     * @return Virtual price (1e18 precision)
     */
    function get_virtual_price() external view returns (uint256);

    /**
     * @notice Get amplification coefficient
     * @return A parameter
     */
    function A() external view returns (uint256);

    /**
     * @notice Get fee in basis points
     * @return Fee (1e10 = 100%)
     */
    function fee() external view returns (uint256);
}

/**
 * @title ICurvePoolETH
 * @notice Interface for Curve ETH pools (like stETH pool)
 * @dev ETH pools use address(0xEeeeeEeeeEeEeeEeEeEeeEEEeeeeEeeeeeeeEEeE) for ETH
 *      or simply expect msg.value for ETH input
 */
interface ICurvePoolETH {
    /**
     * @notice Exchange ETH for tokens or tokens for ETH
     * @param i Index of input coin (0=ETH for stETH pool)
     * @param j Index of output coin
     * @param dx Amount of input (use msg.value for ETH)
     * @param min_dy Minimum output amount
     * @return Amount received
     * @dev For ETH input: send ETH via msg.value
     *      For ETH output: ETH is sent back to caller
     */
    function exchange(
        int128 i,
        int128 j,
        uint256 dx,
        uint256 min_dy
    ) external payable returns (uint256);
}

/**
 * @title ICurvePoolNG
 * @notice Interface for Curve Next-Gen pools (newer pools like tricrypto)
 * @dev Uses uint256 indices instead of int128
 */
interface ICurvePoolNG {
    /**
     * @notice Exchange tokens using uint256 indices
     * @param i Index of input coin
     * @param j Index of output coin
     * @param dx Amount of input coin
     * @param min_dy Minimum output amount
     * @return Amount received
     */
    function exchange(
        uint256 i,
        uint256 j,
        uint256 dx,
        uint256 min_dy
    ) external returns (uint256);

    /**
     * @notice Exchange with receiver specification
     * @param i Index of input coin
     * @param j Index of output coin
     * @param dx Amount of input coin
     * @param min_dy Minimum output amount
     * @param receiver Address to receive output
     * @return Amount received
     */
    function exchange(
        uint256 i,
        uint256 j,
        uint256 dx,
        uint256 min_dy,
        address receiver
    ) external returns (uint256);

    /**
     * @notice Get expected output amount
     * @param i Index of input coin
     * @param j Index of output coin
     * @param dx Amount of input coin
     * @return Expected output amount
     */
    function get_dy(
        uint256 i,
        uint256 j,
        uint256 dx
    ) external view returns (uint256);
}
