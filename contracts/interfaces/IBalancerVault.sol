// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

/**
 * @title IBalancerVault
 * @notice Interface for Balancer V2 Vault flash loans
 * @dev Balancer flash loans have 0% fee, making them ideal for MEV
 *
 * SECURITY CONSIDERATIONS:
 * - Balancer Vault address is constant across all chains: 0xBA12222222228d8Ba445958a75a0704d566BF2C8
 * - Flash loan callbacks MUST repay borrowed amount before returning
 * - Always validate msg.sender in receiveFlashLoan callback
 * - Use flash loan in-progress flag to prevent unauthorized callback invocation
 */
interface IBalancerVault {
    /**
     * @notice Execute flash loan from Balancer Vault
     * @param recipient Address that will receive the flash loan and handle callback
     * @param tokens Array of token addresses to borrow
     * @param amounts Array of amounts to borrow for each token
     * @param userData Arbitrary data passed to recipient's callback
     * @dev Flash loans from Balancer are FREE (0% fee)
     */
    function flashLoan(
        IFlashLoanRecipient recipient,
        address[] memory tokens,
        uint256[] memory amounts,
        bytes memory userData
    ) external;

    /**
     * @notice Swap tokens through Balancer pools
     * @param singleSwap Swap details
     * @param funds Fund management details
     * @param limit Minimum/maximum amount depending on swap kind
     * @param deadline Transaction deadline
     * @return amountCalculated The amount calculated by the swap
     */
    function swap(
        SingleSwap memory singleSwap,
        FundManagement memory funds,
        uint256 limit,
        uint256 deadline
    ) external payable returns (uint256 amountCalculated);

    /**
     * @notice Execute batch swap
     * @param kind Type of swap (GIVEN_IN or GIVEN_OUT)
     * @param swaps Array of swap steps
     * @param assets Array of token addresses
     * @param funds Fund management details
     * @param limits Array of limits for each asset
     * @param deadline Transaction deadline
     * @return assetDeltas Array of asset balance changes
     */
    function batchSwap(
        SwapKind kind,
        BatchSwapStep[] memory swaps,
        address[] memory assets,
        FundManagement memory funds,
        int256[] memory limits,
        uint256 deadline
    ) external payable returns (int256[] memory assetDeltas);

    /**
     * @notice Query batch swap without executing
     * @param kind Type of swap
     * @param swaps Array of swap steps
     * @param assets Array of token addresses
     * @param funds Fund management details
     * @return assetDeltas Expected asset balance changes
     */
    function queryBatchSwap(
        SwapKind kind,
        BatchSwapStep[] memory swaps,
        address[] memory assets,
        FundManagement memory funds
    ) external returns (int256[] memory assetDeltas);

    /**
     * @notice Get pool tokens and balances
     * @param poolId The pool identifier
     * @return tokens Array of token addresses in the pool
     * @return balances Array of token balances
     * @return lastChangeBlock Last block where pool was modified
     */
    function getPoolTokens(
        bytes32 poolId
    )
        external
        view
        returns (
            address[] memory tokens,
            uint256[] memory balances,
            uint256 lastChangeBlock
        );

    /**
     * @notice Get pool registered info
     * @param poolId The pool identifier
     * @return pool Pool address
     * @return specialization Pool specialization
     */
    function getPool(
        bytes32 poolId
    ) external view returns (address pool, PoolSpecialization specialization);

    // Enums
    enum SwapKind {
        GIVEN_IN,
        GIVEN_OUT
    }

    enum PoolSpecialization {
        GENERAL,
        MINIMAL_SWAP_INFO,
        TWO_TOKEN
    }

    // Structs
    struct SingleSwap {
        bytes32 poolId;
        SwapKind kind;
        address assetIn;
        address assetOut;
        uint256 amount;
        bytes userData;
    }

    struct BatchSwapStep {
        bytes32 poolId;
        uint256 assetInIndex;
        uint256 assetOutIndex;
        uint256 amount;
        bytes userData;
    }

    struct FundManagement {
        address sender;
        bool fromInternalBalance;
        address payable recipient;
        bool toInternalBalance;
    }
}

/**
 * @title IFlashLoanRecipient
 * @notice Interface for Balancer flash loan recipient callback
 *
 * SECURITY IMPLEMENTATION GUIDE:
 * 1. ALWAYS check msg.sender == BALANCER_VAULT in receiveFlashLoan
 * 2. Use a flash loan in-progress flag set before calling flashLoan
 * 3. Check the flag in callback to prevent unauthorized calls
 * 4. Clear the flag after flashLoan completes
 * 5. Use reentrancy guard on the function initiating the flash loan
 */
interface IFlashLoanRecipient {
    /**
     * @notice Called by Balancer Vault after flash loan
     * @param tokens Array of borrowed token addresses
     * @param amounts Array of borrowed amounts
     * @param feeAmounts Array of fee amounts (always 0 for Balancer)
     * @param userData Arbitrary data passed from flashLoan call
     * @dev MUST repay tokens + fees to Vault before returning
     * @dev With Balancer, feeAmounts are always 0
     *
     * SECURITY: Implementations MUST validate:
     * - msg.sender is the Balancer Vault
     * - Flash loan was initiated by this contract (use flag)
     */
    function receiveFlashLoan(
        address[] memory tokens,
        uint256[] memory amounts,
        uint256[] memory feeAmounts,
        bytes memory userData
    ) external;
}

/**
 * @title IBalancerPool
 * @notice Interface for Balancer weighted pool
 */
interface IBalancerPool {
    /**
     * @notice Get pool ID
     * @return Pool identifier
     */
    function getPoolId() external view returns (bytes32);

    /**
     * @notice Get swap fee
     * @return Swap fee percentage (18 decimals)
     */
    function getSwapFeePercentage() external view returns (uint256);

    /**
     * @notice Get normalized weights
     * @return weights Array of normalized weights
     */
    function getNormalizedWeights() external view returns (uint256[] memory);
}
