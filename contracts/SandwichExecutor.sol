// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import "./interfaces/IUniswapV2.sol";
import "./interfaces/IUniswapV3.sol";

/**
 * @title SandwichExecutor
 * @notice Gas-optimized contract for executing sandwich attacks
 * @dev Designed for use with Flashbots bundles where frontrun and backrun
 *      are submitted as separate transactions in an atomic bundle
 *
 * IMPORTANT: In production, frontrun() and backrun() are called in separate
 * transactions bundled together via Flashbots. The executeSandwich() function
 * is provided for testing/simulation purposes only.
 *
 * SECURITY CONSIDERATIONS:
 * - Uses Solidity 0.8.20+ with built-in overflow/underflow protection
 * - Implements custom reentrancy guard to prevent cross-function reentrancy
 * - Two-step ownership transfer prevents accidental ownership loss
 * - Router whitelist prevents arbitrary contract interactions
 * - All external calls use safe wrappers with return value checks
 * - Slippage protection via minAmountOut parameters
 * - No delegatecall usage to prevent storage corruption attacks
 *
 * GAS OPTIMIZATIONS:
 * - Uses unchecked increments where safe
 * - Direct pair swaps available for maximum gas efficiency
 * - Protocol-specific functions avoid generic overhead
 */
contract SandwichExecutor {
    /*//////////////////////////////////////////////////////////////
                                 ERRORS
    //////////////////////////////////////////////////////////////*/

    error Unauthorized();
    error ReentrancyGuard();
    error SwapFailed();
    error InsufficientProfit();
    error ZeroAddress();
    error ZeroAmount();
    error InvalidRouter();
    error SlippageExceeded();
    error InvalidSwapData();
    error InvalidPair();

    /*//////////////////////////////////////////////////////////////
                                 EVENTS
    //////////////////////////////////////////////////////////////*/

    event SandwichExecuted(
        address indexed tokenIn,
        address indexed tokenOut,
        uint256 frontrunAmount,
        uint256 profit,
        uint256 gasUsed
    );
    event FrontrunExecuted(
        address indexed router,
        uint256 amountIn,
        uint256 amountOut
    );
    event BackrunExecuted(
        address indexed router,
        uint256 amountIn,
        uint256 amountOut,
        uint256 profit
    );
    event EmergencyWithdraw(address indexed token, uint256 amount);
    event OwnershipTransferred(
        address indexed previousOwner,
        address indexed newOwner
    );
    event RouterApproved(address indexed router, bool approved);

    /*//////////////////////////////////////////////////////////////
                                CONSTANTS
    //////////////////////////////////////////////////////////////*/

    uint256 private constant NOT_ENTERED = 1;
    uint256 private constant ENTERED = 2;

    /*//////////////////////////////////////////////////////////////
                                 STATE
    //////////////////////////////////////////////////////////////*/

    /// @notice Contract owner
    address public owner;

    /// @notice Pending owner for two-step transfer
    address public pendingOwner;

    /// @notice Reentrancy state
    uint256 private _status;

    /// @notice Approved routers for swaps
    /// @dev SECURITY: Only whitelisted routers can be used to prevent malicious contract calls
    mapping(address => bool) public approvedRouters;

    /// @notice Approved pairs for direct swaps
    /// @dev SECURITY: Only whitelisted pairs can be used for swapOnPair
    mapping(address => bool) public approvedPairs;

    /// @notice Stored state between frontrun and backrun (for bundle execution)
    /// @dev tokenOut from frontrun becomes tokenIn for backrun
    /// SECURITY NOTE: This state persists between transactions in a Flashbots bundle
    address private _pendingTokenOut;
    uint256 private _pendingAmountOut;

    /*//////////////////////////////////////////////////////////////
                               MODIFIERS
    //////////////////////////////////////////////////////////////*/

    modifier onlyOwner() {
        if (msg.sender != owner) revert Unauthorized();
        _;
    }

    modifier nonReentrant() {
        if (_status == ENTERED) revert ReentrancyGuard();
        _status = ENTERED;
        _;
        _status = NOT_ENTERED;
    }

    modifier validRouter(address router) {
        if (!approvedRouters[router]) revert InvalidRouter();
        _;
    }

    /*//////////////////////////////////////////////////////////////
                              CONSTRUCTOR
    //////////////////////////////////////////////////////////////*/

    constructor() {
        owner = msg.sender;
        _status = NOT_ENTERED;
        emit OwnershipTransferred(address(0), msg.sender);
    }

    /*//////////////////////////////////////////////////////////////
                         SANDWICH FUNCTIONS
    //////////////////////////////////////////////////////////////*/

    /**
     * @notice Execute complete sandwich attack in single transaction
     * @dev For testing/simulation only. In production, use separate
     *      frontrun() and backrun() calls in a Flashbots bundle
     * @param router DEX router address
     * @param tokenIn Token to swap from (frontrun input, backrun output)
     * @param tokenOut Token to swap to (frontrun output, backrun input)
     * @param frontrunAmount Amount to use in frontrun
     * @param minBackrunOutput Minimum output from backrun (slippage protection)
     * @param frontrunData Encoded frontrun swap data
     * @param backrunData Encoded backrun swap data
     * @return profit Net profit from sandwich
     *
     * SECURITY NOTES:
     * - onlyOwner: Prevents unauthorized execution
     * - nonReentrant: Prevents reentrancy during swaps
     * - validRouter: Ensures router is whitelisted
     * - minBackrunOutput: Slippage protection
     */
    function executeSandwich(
        address router,
        address tokenIn,
        address tokenOut,
        uint256 frontrunAmount,
        uint256 minBackrunOutput,
        bytes calldata frontrunData,
        bytes calldata backrunData
    )
        external
        onlyOwner
        nonReentrant
        validRouter(router)
        returns (uint256 profit)
    {
        // SECURITY: Validate inputs
        if (tokenIn == address(0) || tokenOut == address(0)) revert ZeroAddress();
        if (frontrunAmount == 0) revert ZeroAmount();
        if (frontrunData.length == 0 || backrunData.length == 0) revert InvalidSwapData();

        uint256 gasStart = gasleft();
        uint256 balanceBefore = _getBalance(tokenIn);

        // Execute frontrun: tokenIn -> tokenOut
        uint256 frontrunOutput = _executeSwap(
            router,
            tokenIn,
            frontrunAmount,
            frontrunData
        );

        // NOTE: In production, victim's transaction executes here
        // This function is for testing only

        // Execute backrun: tokenOut -> tokenIn
        uint256 backrunOutput = _executeSwap(
            router,
            tokenOut,
            frontrunOutput,
            backrunData
        );

        if (backrunOutput < minBackrunOutput) revert SlippageExceeded();

        // Calculate profit
        uint256 balanceAfter = _getBalance(tokenIn);
        if (balanceAfter <= balanceBefore) revert InsufficientProfit();
        profit = balanceAfter - balanceBefore;

        emit SandwichExecuted(
            tokenIn,
            tokenOut,
            frontrunAmount,
            profit,
            gasStart - gasleft()
        );
    }

    /**
     * @notice Execute frontrun swap (first tx in Flashbots bundle)
     * @dev Stores output for backrun validation
     * @param router DEX router address
     * @param tokenIn Input token
     * @param tokenOut Output token
     * @param amountIn Amount to swap
     * @param minAmountOut Minimum output amount
     * @param swapData Encoded swap parameters
     * @return amountOut Tokens received
     *
     * SECURITY NOTES:
     * - Stores pending state for backrun correlation
     * - minAmountOut provides slippage protection
     */
    function frontrun(
        address router,
        address tokenIn,
        address tokenOut,
        uint256 amountIn,
        uint256 minAmountOut,
        bytes calldata swapData
    )
        external
        onlyOwner
        nonReentrant
        validRouter(router)
        returns (uint256 amountOut)
    {
        // SECURITY: Validate inputs
        if (tokenIn == address(0) || tokenOut == address(0)) revert ZeroAddress();
        if (amountIn == 0) revert ZeroAmount();
        if (swapData.length == 0) revert InvalidSwapData();

        uint256 balanceBefore = _getBalance(tokenOut);

        amountOut = _executeSwap(router, tokenIn, amountIn, swapData);

        if (amountOut < minAmountOut) revert SlippageExceeded();

        // Store state for backrun
        _pendingTokenOut = tokenOut;
        _pendingAmountOut = _getBalance(tokenOut) - balanceBefore;

        emit FrontrunExecuted(router, amountIn, amountOut);
    }

    /**
     * @notice Execute frontrun with Uniswap V2 (gas optimized)
     * @param router Uniswap V2 router
     * @param tokenIn Input token
     * @param tokenOut Output token
     * @param amountIn Amount to swap
     * @param minAmountOut Minimum output
     * @return amountOut Tokens received
     *
     * GAS OPTIMIZATION: Direct V2 router call without generic overhead
     */
    function frontrunV2(
        address router,
        address tokenIn,
        address tokenOut,
        uint256 amountIn,
        uint256 minAmountOut
    )
        external
        onlyOwner
        nonReentrant
        validRouter(router)
        returns (uint256 amountOut)
    {
        // SECURITY: Validate inputs
        if (tokenIn == address(0) || tokenOut == address(0)) revert ZeroAddress();
        if (amountIn == 0) revert ZeroAmount();

        // SECURITY: Reset and approve router (handles non-standard tokens like USDT)
        _safeApprove(tokenIn, router, 0);
        _safeApprove(tokenIn, router, amountIn);

        // Build path
        address[] memory path = new address[](2);
        path[0] = tokenIn;
        path[1] = tokenOut;

        uint256 balanceBefore = _getBalance(tokenOut);

        // Execute swap
        uint256[] memory amounts = IUniswapV2Router02(router)
            .swapExactTokensForTokens(
                amountIn,
                minAmountOut,
                path,
                address(this),
                block.timestamp
            );

        amountOut = amounts[1];
        _pendingTokenOut = tokenOut;
        _pendingAmountOut = _getBalance(tokenOut) - balanceBefore;

        // SECURITY: Clear approval after swap
        _safeApprove(tokenIn, router, 0);

        emit FrontrunExecuted(router, amountIn, amountOut);
    }

    /**
     * @notice Execute frontrun with Uniswap V3 (gas optimized)
     * @param router Uniswap V3 SwapRouter
     * @param tokenIn Input token
     * @param tokenOut Output token
     * @param fee Pool fee tier
     * @param amountIn Amount to swap
     * @param minAmountOut Minimum output
     * @return amountOut Tokens received
     *
     * GAS OPTIMIZATION: Direct V3 router call without generic overhead
     */
    function frontrunV3(
        address router,
        address tokenIn,
        address tokenOut,
        uint24 fee,
        uint256 amountIn,
        uint256 minAmountOut
    )
        external
        onlyOwner
        nonReentrant
        validRouter(router)
        returns (uint256 amountOut)
    {
        // SECURITY: Validate inputs
        if (tokenIn == address(0) || tokenOut == address(0)) revert ZeroAddress();
        if (amountIn == 0) revert ZeroAmount();

        // SECURITY: Reset and approve router
        _safeApprove(tokenIn, router, 0);
        _safeApprove(tokenIn, router, amountIn);

        uint256 balanceBefore = _getBalance(tokenOut);

        // Execute swap
        ISwapRouter.ExactInputSingleParams memory params = ISwapRouter
            .ExactInputSingleParams({
                tokenIn: tokenIn,
                tokenOut: tokenOut,
                fee: fee,
                recipient: address(this),
                amountIn: amountIn,
                amountOutMinimum: minAmountOut,
                sqrtPriceLimitX96: 0
            });

        amountOut = ISwapRouter(router).exactInputSingle(params);

        _pendingTokenOut = tokenOut;
        _pendingAmountOut = _getBalance(tokenOut) - balanceBefore;

        // SECURITY: Clear approval after swap
        _safeApprove(tokenIn, router, 0);

        emit FrontrunExecuted(router, amountIn, amountOut);
    }

    /**
     * @notice Execute backrun swap (last tx in Flashbots bundle)
     * @dev Uses stored output from frontrun
     * @param router DEX router address
     * @param tokenIn Input token (was tokenOut in frontrun)
     * @param tokenOut Output token (was tokenIn in frontrun)
     * @param minAmountOut Minimum output (profit + principal)
     * @param swapData Encoded swap parameters
     * @return profit Net profit from sandwich
     *
     * SECURITY NOTES:
     * - Clears pending state after execution
     * - minAmountOut protects against slippage
     */
    function backrun(
        address router,
        address tokenIn,
        address tokenOut,
        uint256 minAmountOut,
        bytes calldata swapData
    )
        external
        onlyOwner
        nonReentrant
        validRouter(router)
        returns (uint256 profit)
    {
        // SECURITY: Validate inputs
        if (tokenIn == address(0) || tokenOut == address(0)) revert ZeroAddress();
        if (swapData.length == 0) revert InvalidSwapData();

        uint256 balanceBefore = _getBalance(tokenOut);

        // Use all available tokenIn (output from frontrun)
        uint256 amountIn = _getBalance(tokenIn);
        if (amountIn == 0) revert ZeroAmount();

        uint256 amountOut = _executeSwap(router, tokenIn, amountIn, swapData);

        if (amountOut < minAmountOut) revert SlippageExceeded();

        // Calculate profit
        uint256 balanceAfter = _getBalance(tokenOut);
        profit = balanceAfter - balanceBefore;

        // Clear pending state
        _pendingTokenOut = address(0);
        _pendingAmountOut = 0;

        emit BackrunExecuted(router, amountIn, amountOut, profit);
    }

    /**
     * @notice Execute backrun with Uniswap V2 (gas optimized)
     * @param router Uniswap V2 router
     * @param tokenIn Input token (frontrun output)
     * @param tokenOut Output token (frontrun input)
     * @param minAmountOut Minimum output
     * @return profit Net profit
     *
     * GAS OPTIMIZATION: Direct V2 router call without generic overhead
     */
    function backrunV2(
        address router,
        address tokenIn,
        address tokenOut,
        uint256 minAmountOut
    )
        external
        onlyOwner
        nonReentrant
        validRouter(router)
        returns (uint256 profit)
    {
        // SECURITY: Validate inputs
        if (tokenIn == address(0) || tokenOut == address(0)) revert ZeroAddress();

        uint256 balanceBefore = _getBalance(tokenOut);
        uint256 amountIn = _getBalance(tokenIn);
        if (amountIn == 0) revert ZeroAmount();

        // SECURITY: Reset and approve router
        _safeApprove(tokenIn, router, 0);
        _safeApprove(tokenIn, router, amountIn);

        // Build path
        address[] memory path = new address[](2);
        path[0] = tokenIn;
        path[1] = tokenOut;

        // Execute swap
        uint256[] memory amounts = IUniswapV2Router02(router)
            .swapExactTokensForTokens(
                amountIn,
                minAmountOut,
                path,
                address(this),
                block.timestamp
            );

        uint256 amountOut = amounts[1];
        profit = _getBalance(tokenOut) - balanceBefore;

        // SECURITY: Clear approval after swap
        _safeApprove(tokenIn, router, 0);

        // Clear pending state
        _pendingTokenOut = address(0);
        _pendingAmountOut = 0;

        emit BackrunExecuted(router, amountIn, amountOut, profit);
    }

    /**
     * @notice Execute backrun with Uniswap V3 (gas optimized)
     * @param router Uniswap V3 SwapRouter
     * @param tokenIn Input token (frontrun output)
     * @param tokenOut Output token (frontrun input)
     * @param fee Pool fee tier
     * @param minAmountOut Minimum output
     * @return profit Net profit
     *
     * GAS OPTIMIZATION: Direct V3 router call without generic overhead
     */
    function backrunV3(
        address router,
        address tokenIn,
        address tokenOut,
        uint24 fee,
        uint256 minAmountOut
    )
        external
        onlyOwner
        nonReentrant
        validRouter(router)
        returns (uint256 profit)
    {
        // SECURITY: Validate inputs
        if (tokenIn == address(0) || tokenOut == address(0)) revert ZeroAddress();

        uint256 balanceBefore = _getBalance(tokenOut);
        uint256 amountIn = _getBalance(tokenIn);
        if (amountIn == 0) revert ZeroAmount();

        // SECURITY: Reset and approve router
        _safeApprove(tokenIn, router, 0);
        _safeApprove(tokenIn, router, amountIn);

        // Execute swap
        ISwapRouter.ExactInputSingleParams memory params = ISwapRouter
            .ExactInputSingleParams({
                tokenIn: tokenIn,
                tokenOut: tokenOut,
                fee: fee,
                recipient: address(this),
                amountIn: amountIn,
                amountOutMinimum: minAmountOut,
                sqrtPriceLimitX96: 0
            });

        uint256 amountOut = ISwapRouter(router).exactInputSingle(params);
        profit = _getBalance(tokenOut) - balanceBefore;

        // SECURITY: Clear approval after swap
        _safeApprove(tokenIn, router, 0);

        // Clear pending state
        _pendingTokenOut = address(0);
        _pendingAmountOut = 0;

        emit BackrunExecuted(router, amountIn, amountOut, profit);
    }

    /*//////////////////////////////////////////////////////////////
                          DIRECT PAIR SWAPS
    //////////////////////////////////////////////////////////////*/

    /**
     * @notice Execute swap directly on Uniswap V2 pair (most gas efficient)
     * @param pair Uniswap V2 pair address
     * @param tokenIn Input token
     * @param amountIn Amount to swap
     * @param amountOut Expected output (calculated off-chain)
     * @param zeroForOne True if swapping token0 for token1
     *
     * GAS OPTIMIZATION: Direct pair interaction bypasses router overhead
     *
     * SECURITY NOTES:
     * - Pair must be whitelisted to prevent interaction with malicious contracts
     * - amountOut must be calculated off-chain accurately
     * - No slippage protection built-in (amountOut is exact)
     */
    function swapOnPair(
        address pair,
        address tokenIn,
        uint256 amountIn,
        uint256 amountOut,
        bool zeroForOne
    ) external onlyOwner nonReentrant {
        // SECURITY: Validate inputs
        if (pair == address(0) || tokenIn == address(0)) revert ZeroAddress();
        if (amountIn == 0 || amountOut == 0) revert ZeroAmount();

        // SECURITY: Validate pair is whitelisted
        if (!approvedPairs[pair]) revert InvalidPair();

        // Transfer tokens to pair
        _safeTransfer(tokenIn, pair, amountIn);

        // Execute swap
        if (zeroForOne) {
            IUniswapV2Pair(pair).swap(0, amountOut, address(this), "");
        } else {
            IUniswapV2Pair(pair).swap(amountOut, 0, address(this), "");
        }
    }

    /*//////////////////////////////////////////////////////////////
                           ADMIN FUNCTIONS
    //////////////////////////////////////////////////////////////*/

    /**
     * @notice Set router approval status
     * @param router Router address
     * @param approved Approval status
     * @dev SECURITY: Only add verified, audited router contracts
     */
    function setApprovedRouter(
        address router,
        bool approved
    ) external onlyOwner {
        if (router == address(0)) revert ZeroAddress();
        approvedRouters[router] = approved;
        emit RouterApproved(router, approved);
    }

    /**
     * @notice Set pair approval status for direct swaps
     * @param pair Pair address
     * @param approved Approval status
     * @dev SECURITY: Only add verified Uniswap V2 compatible pair contracts
     */
    function setApprovedPair(
        address pair,
        bool approved
    ) external onlyOwner {
        if (pair == address(0)) revert ZeroAddress();
        approvedPairs[pair] = approved;
    }

    /**
     * @notice Batch approve multiple routers
     * @param routers Array of router addresses
     * @param approved Approval status
     */
    function setApprovedRoutersBatch(
        address[] calldata routers,
        bool approved
    ) external onlyOwner {
        for (uint256 i = 0; i < routers.length; ) {
            if (routers[i] == address(0)) revert ZeroAddress();
            approvedRouters[routers[i]] = approved;
            emit RouterApproved(routers[i], approved);
            unchecked {
                ++i;
            }
        }
    }

    /**
     * @notice Batch approve multiple pairs
     * @param pairs Array of pair addresses
     * @param approved Approval status
     */
    function setApprovedPairsBatch(
        address[] calldata pairs,
        bool approved
    ) external onlyOwner {
        for (uint256 i = 0; i < pairs.length; ) {
            if (pairs[i] == address(0)) revert ZeroAddress();
            approvedPairs[pairs[i]] = approved;
            unchecked {
                ++i;
            }
        }
    }

    /**
     * @notice Emergency withdraw all of a token
     * @param token Token address (address(0) for ETH)
     */
    function emergencyWithdraw(address token) external onlyOwner {
        uint256 balance;

        if (token == address(0)) {
            balance = address(this).balance;
            if (balance > 0) {
                (bool success, ) = owner.call{value: balance}("");
                if (!success) revert SwapFailed();
            }
        } else {
            balance = _getBalance(token);
            if (balance > 0) {
                _safeTransfer(token, owner, balance);
            }
        }

        emit EmergencyWithdraw(token, balance);
    }

    /**
     * @notice Withdraw specific amount
     * @param token Token address
     * @param amount Amount to withdraw
     */
    function withdraw(address token, uint256 amount) external onlyOwner {
        if (amount == 0) revert ZeroAmount();

        if (token == address(0)) {
            (bool success, ) = owner.call{value: amount}("");
            if (!success) revert SwapFailed();
        } else {
            _safeTransfer(token, owner, amount);
        }
    }

    /**
     * @notice Transfer ownership (two-step)
     * @param newOwner New owner address
     */
    function transferOwnership(address newOwner) external onlyOwner {
        if (newOwner == address(0)) revert ZeroAddress();
        pendingOwner = newOwner;
    }

    /**
     * @notice Accept ownership
     */
    function acceptOwnership() external {
        if (msg.sender != pendingOwner) revert Unauthorized();
        emit OwnershipTransferred(owner, pendingOwner);
        owner = pendingOwner;
        pendingOwner = address(0);
    }

    /*//////////////////////////////////////////////////////////////
                          INTERNAL HELPERS
    //////////////////////////////////////////////////////////////*/

    /**
     * @notice Execute swap with raw calldata
     * @dev SECURITY: Router must be validated before calling this function
     *      Uses safe approve pattern to prevent approval race conditions
     */
    function _executeSwap(
        address router,
        address tokenIn,
        uint256 amountIn,
        bytes calldata swapData
    ) internal returns (uint256 amountOut) {
        // SECURITY: Reset and approve router (handles non-standard tokens)
        _safeApprove(tokenIn, router, 0);
        _safeApprove(tokenIn, router, amountIn);

        // Execute swap
        (bool success, bytes memory result) = router.call(swapData);
        if (!success) revert SwapFailed();

        // Try to decode output amount
        if (result.length >= 32) {
            amountOut = abi.decode(result, (uint256));
        }

        // SECURITY: Clear approval after swap
        _safeApprove(tokenIn, router, 0);
    }

    /**
     * @notice Get token balance
     */
    function _getBalance(address token) internal view returns (uint256) {
        (bool success, bytes memory data) = token.staticcall(
            abi.encodeWithSelector(0x70a08231, address(this))
        );
        if (!success || data.length < 32) return 0;
        return abi.decode(data, (uint256));
    }

    /**
     * @notice Safe ERC20 transfer
     */
    function _safeTransfer(address token, address to, uint256 amount) internal {
        (bool success, bytes memory data) = token.call(
            abi.encodeWithSelector(0xa9059cbb, to, amount)
        );
        if (!success || (data.length > 0 && !abi.decode(data, (bool)))) {
            revert SwapFailed();
        }
    }

    /**
     * @notice Safe ERC20 approve
     */
    function _safeApprove(
        address token,
        address spender,
        uint256 amount
    ) internal {
        (bool success, bytes memory data) = token.call(
            abi.encodeWithSelector(0x095ea7b3, spender, amount)
        );
        if (!success || (data.length > 0 && !abi.decode(data, (bool)))) {
            revert SwapFailed();
        }
    }

    /*//////////////////////////////////////////////////////////////
                              RECEIVE ETH
    //////////////////////////////////////////////////////////////*/

    receive() external payable {}
}
