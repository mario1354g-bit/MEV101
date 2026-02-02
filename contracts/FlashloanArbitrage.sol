// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import "./interfaces/IBalancerVault.sol";
import "./interfaces/IAaveV3.sol";
import "./interfaces/IUniswapV2.sol";
import "./interfaces/IUniswapV3.sol";
import "./interfaces/ICurve.sol";

/**
 * @title FlashloanArbitrage
 * @notice Gas-optimized flashloan arbitrage contract for MEV extraction
 * @dev Supports Balancer (0% fee) and Aave V3 flashloans with multi-DEX routing
 *
 * SECURITY CONSIDERATIONS:
 * - Uses Solidity 0.8.20+ with built-in overflow/underflow protection
 * - Implements custom reentrancy guard to prevent cross-function reentrancy
 * - Flash loan callbacks validated via sender check AND in-progress flag
 * - Two-step ownership transfer prevents accidental ownership loss
 * - All external calls use safe wrappers with return value checks
 * - Approved router whitelist prevents arbitrary contract interactions
 * - No delegatecall usage to prevent storage corruption attacks
 */
contract FlashloanArbitrage is IFlashLoanRecipient {
    /*//////////////////////////////////////////////////////////////
                                 ERRORS
    //////////////////////////////////////////////////////////////*/

    error Unauthorized();
    error InvalidCallback();
    error NoProfitRealized();
    error InsufficientOutput();
    error SwapFailed();
    error InvalidSwapData();
    error ReentrancyGuard();
    error ZeroAddress();
    error ZeroAmount();
    error InvalidPool();
    error RouterNotApproved();
    error ArrayLengthMismatch();

    /*//////////////////////////////////////////////////////////////
                                 EVENTS
    //////////////////////////////////////////////////////////////*/

    event ArbitrageExecuted(
        address indexed token,
        uint256 flashAmount,
        uint256 profit,
        uint256 gasUsed
    );
    event OwnershipTransferred(
        address indexed previousOwner,
        address indexed newOwner
    );
    event Withdrawal(address indexed token, uint256 amount, address indexed to);

    /*//////////////////////////////////////////////////////////////
                                 STRUCTS
    //////////////////////////////////////////////////////////////*/

    /**
     * @notice Represents a single swap step in the arbitrage path
     * @param protocol Protocol identifier (1=UniV2, 2=UniV3, 3=Balancer, 4=Curve)
     * @param router Router/pool address to execute swap on
     * @param tokenIn Input token address
     * @param tokenOut Output token address
     * @param swapData Encoded swap parameters
     * @param amountIn Amount of input tokens (0 = use all available)
     * @param minAmountOut Minimum output amount (slippage protection)
     */
    struct SwapStep {
        uint8 protocol;
        address router;
        address tokenIn;
        address tokenOut;
        bytes swapData;
        uint256 amountIn;
        uint256 minAmountOut;
    }

    /*//////////////////////////////////////////////////////////////
                                CONSTANTS
    //////////////////////////////////////////////////////////////*/

    /// @notice Balancer V2 Vault address (same on all chains)
    address public constant BALANCER_VAULT =
        0xBA12222222228d8Ba445958a75a0704d566BF2C8;

    /// @notice Protocol identifiers for gas-efficient routing
    uint8 public constant PROTOCOL_UNISWAP_V2 = 1;
    uint8 public constant PROTOCOL_UNISWAP_V3 = 2;
    uint8 public constant PROTOCOL_BALANCER = 3;
    uint8 public constant PROTOCOL_CURVE = 4;

    /// @notice Reentrancy lock states
    uint256 private constant NOT_ENTERED = 1;
    uint256 private constant ENTERED = 2;

    /*//////////////////////////////////////////////////////////////
                                 STATE
    //////////////////////////////////////////////////////////////*/

    /// @notice Contract owner
    address public owner;

    /// @notice Pending owner for two-step transfer
    address public pendingOwner;

    /// @notice Reentrancy guard state
    uint256 private _status;

    /// @notice Approved routers for swap execution
    /// @dev SECURITY: Only whitelisted routers can be used to prevent malicious contract calls
    mapping(address => bool) public approvedRouters;

    /// @notice Approved Aave pools for flashloan execution
    /// @dev SECURITY: Only whitelisted pools can be used to prevent fake pool attacks
    mapping(address => bool) public approvedAavePools;

    /// @notice Flash loan in progress flag (for callback validation)
    /// @dev SECURITY: Prevents unauthorized external calls to callback functions
    bool private _flashLoanInProgress;

    /// @notice Expected Aave pool for current flashloan (for callback validation)
    /// @dev SECURITY: Validates that callback comes from the expected pool
    address private _expectedAavePool;

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

    /*//////////////////////////////////////////////////////////////
                              CONSTRUCTOR
    //////////////////////////////////////////////////////////////*/

    constructor() {
        owner = msg.sender;
        _status = NOT_ENTERED;
        emit OwnershipTransferred(address(0), msg.sender);
    }

    /*//////////////////////////////////////////////////////////////
                          FLASHLOAN FUNCTIONS
    //////////////////////////////////////////////////////////////*/

    /**
     * @notice Execute arbitrage using Balancer flashloan (0% fee)
     * @param tokens Array of token addresses to borrow
     * @param amounts Array of amounts to borrow
     * @param swapData Encoded swap steps for arbitrage execution
     * @dev Balancer flashloans are preferred due to zero fees
     *
     * SECURITY NOTES:
     * - onlyOwner: Prevents unauthorized users from executing arbitrage
     * - nonReentrant: Prevents reentrancy attacks during swap execution
     * - _flashLoanInProgress flag: Validates callback authenticity
     * - Array length validation: Prevents out-of-bounds access
     */
    function executeBalancerFlashloan(
        address[] calldata tokens,
        uint256[] calldata amounts,
        bytes calldata swapData
    ) external onlyOwner nonReentrant {
        uint256 gasStart = gasleft();

        // SECURITY: Validate array lengths to prevent out-of-bounds access
        if (tokens.length == 0) revert InvalidSwapData();
        if (tokens.length != amounts.length) revert ArrayLengthMismatch();

        // SECURITY: Validate no zero addresses or amounts
        for (uint256 i = 0; i < tokens.length; ) {
            if (tokens[i] == address(0)) revert ZeroAddress();
            if (amounts[i] == 0) revert ZeroAmount();
            unchecked { ++i; }
        }

        // Set flag to validate callback
        _flashLoanInProgress = true;

        // Execute flashloan - callback will be receiveFlashLoan
        IBalancerVault(BALANCER_VAULT).flashLoan(
            IFlashLoanRecipient(address(this)),
            tokens,
            amounts,
            swapData
        );

        // Clear flag
        _flashLoanInProgress = false;

        // Emit event with gas used
        emit ArbitrageExecuted(
            tokens[0],
            amounts[0],
            _getBalance(tokens[0]),
            gasStart - gasleft()
        );
    }

    /**
     * @notice Balancer flashloan callback
     * @param tokens Array of borrowed token addresses
     * @param amounts Array of borrowed amounts
     * @param feeAmounts Array of fees (always 0 for Balancer)
     * @param userData Encoded swap steps
     * @dev MUST repay borrowed amounts before returning
     *
     * SECURITY NOTES:
     * - Validates msg.sender is BALANCER_VAULT (hardcoded immutable address)
     * - Validates _flashLoanInProgress flag to prevent external calls
     * - Both checks required: msg.sender can be spoofed if vault is compromised,
     *   flag prevents calls when we didn't initiate the flashloan
     */
    function receiveFlashLoan(
        address[] memory tokens,
        uint256[] memory amounts,
        uint256[] memory feeAmounts,
        bytes memory userData
    ) external override {
        // SECURITY: Dual validation - sender AND in-progress flag
        // This prevents both external calls and potential vault compromise scenarios
        if (msg.sender != BALANCER_VAULT) revert InvalidCallback();
        if (!_flashLoanInProgress) revert InvalidCallback();

        // Decode and execute swaps
        SwapStep[] memory steps = abi.decode(userData, (SwapStep[]));
        _executeArbitrage(steps);

        // Repay flashloan (Balancer has 0% fee)
        for (uint256 i = 0; i < tokens.length; ) {
            uint256 amountOwed = amounts[i] + feeAmounts[i];
            uint256 balance = _getBalance(tokens[i]);

            // Ensure profit was made
            if (balance < amountOwed) revert NoProfitRealized();

            // Transfer repayment to vault
            _safeTransfer(tokens[i], BALANCER_VAULT, amountOwed);

            unchecked {
                ++i;
            }
        }
    }

    /**
     * @notice Execute arbitrage using Aave V3 flashloan
     * @param pool Aave V3 Pool address
     * @param assets Array of asset addresses to borrow
     * @param amounts Array of amounts to borrow
     * @param swapData Encoded swap steps
     * @dev Aave charges 0.09% fee - use when Balancer lacks liquidity
     *
     * SECURITY NOTES:
     * - Pool address is validated against whitelist to prevent fake pool attacks
     * - A malicious pool could call our callback with arbitrary data
     * - _expectedAavePool stores which pool should be calling back
     */
    function executeAaveFlashloan(
        address pool,
        address[] calldata assets,
        uint256[] calldata amounts,
        bytes calldata swapData
    ) external onlyOwner nonReentrant {
        uint256 gasStart = gasleft();

        // SECURITY: Validate pool is whitelisted to prevent fake pool attacks
        if (!approvedAavePools[pool]) revert InvalidPool();

        // SECURITY: Validate array lengths
        if (assets.length == 0) revert InvalidSwapData();
        if (assets.length != amounts.length) revert ArrayLengthMismatch();

        // SECURITY: Validate no zero addresses or amounts
        for (uint256 i = 0; i < assets.length; ) {
            if (assets[i] == address(0)) revert ZeroAddress();
            if (amounts[i] == 0) revert ZeroAmount();
            unchecked { ++i; }
        }

        // Set flags for callback validation
        _flashLoanInProgress = true;
        _expectedAavePool = pool;

        // Interest rate modes: 0 = no debt (repay in same tx)
        uint256[] memory modes = new uint256[](assets.length);

        // Execute Aave flashloan
        IPool(pool).flashLoan(
            address(this),
            assets,
            amounts,
            modes,
            address(this),
            swapData,
            0 // referral code
        );

        // Clear flags
        _flashLoanInProgress = false;
        _expectedAavePool = address(0);

        emit ArbitrageExecuted(
            assets[0],
            amounts[0],
            _getBalance(assets[0]),
            gasStart - gasleft()
        );
    }

    /**
     * @notice Aave V3 flashloan callback
     * @param assets Array of borrowed assets
     * @param amounts Array of borrowed amounts
     * @param premiums Array of fees to pay
     * @param initiator Address that initiated the flashloan
     * @param params Encoded swap steps
     * @return True if operation succeeded
     *
     * SECURITY NOTES:
     * - Triple validation: initiator, in-progress flag, AND expected pool
     * - initiator check ensures we initiated the loan (prevents griefing)
     * - msg.sender check ensures callback from expected pool (prevents fake pools)
     * - _flashLoanInProgress ensures we're in an active flashloan context
     */
    function executeOperation(
        address[] calldata assets,
        uint256[] calldata amounts,
        uint256[] calldata premiums,
        address initiator,
        bytes calldata params
    ) external returns (bool) {
        // SECURITY: Triple validation for callback authenticity
        // 1. Check initiator is this contract (we started the flashloan)
        if (initiator != address(this)) revert InvalidCallback();
        // 2. Check we're in an active flashloan
        if (!_flashLoanInProgress) revert InvalidCallback();
        // 3. Check msg.sender is the expected pool (prevents fake pool callbacks)
        if (msg.sender != _expectedAavePool) revert InvalidCallback();

        // Decode and execute swaps
        SwapStep[] memory steps = abi.decode(params, (SwapStep[]));
        _executeArbitrage(steps);

        // Approve repayment to Aave pool
        for (uint256 i = 0; i < assets.length; ) {
            uint256 amountOwed = amounts[i] + premiums[i];
            uint256 balance = _getBalance(assets[i]);

            if (balance < amountOwed) revert NoProfitRealized();

            // Approve pool to pull repayment
            _safeApprove(assets[i], msg.sender, amountOwed);

            unchecked {
                ++i;
            }
        }

        return true;
    }

    /*//////////////////////////////////////////////////////////////
                          ARBITRAGE EXECUTION
    //////////////////////////////////////////////////////////////*/

    /**
     * @notice Execute multi-step arbitrage swaps
     * @param steps Array of swap steps to execute
     * @return profit Total profit from arbitrage
     *
     * SECURITY NOTES:
     * - Router addresses validated against whitelist for non-Balancer swaps
     * - Token addresses validated to prevent zero address interactions
     * - minAmountOut enforced for slippage protection
     * - Uses unchecked increment for gas optimization (safe due to array bounds)
     */
    function _executeArbitrage(
        SwapStep[] memory steps
    ) internal returns (uint256 profit) {
        if (steps.length == 0) revert InvalidSwapData();

        for (uint256 i = 0; i < steps.length; ) {
            SwapStep memory step = steps[i];

            // SECURITY: Validate token addresses
            if (step.tokenIn == address(0) || step.tokenOut == address(0))
                revert ZeroAddress();

            // SECURITY: Validate router is approved (except for Balancer which uses vault)
            if (step.protocol != PROTOCOL_BALANCER) {
                if (!approvedRouters[step.router]) revert RouterNotApproved();
            }

            // Determine input amount
            uint256 amountIn = step.amountIn;
            if (amountIn == 0) {
                amountIn = _getBalance(step.tokenIn);
            }

            if (amountIn == 0) revert ZeroAmount();

            // Execute swap based on protocol
            uint256 amountOut;

            if (step.protocol == PROTOCOL_UNISWAP_V2) {
                amountOut = _swapUniswapV2(
                    step.router,
                    step.tokenIn,
                    step.tokenOut,
                    amountIn,
                    step.minAmountOut,
                    step.swapData
                );
            } else if (step.protocol == PROTOCOL_UNISWAP_V3) {
                amountOut = _swapUniswapV3(
                    step.router,
                    step.tokenIn,
                    step.tokenOut,
                    amountIn,
                    step.minAmountOut,
                    step.swapData
                );
            } else if (step.protocol == PROTOCOL_BALANCER) {
                amountOut = _swapBalancer(
                    step.tokenIn,
                    step.tokenOut,
                    amountIn,
                    step.minAmountOut,
                    step.swapData
                );
            } else if (step.protocol == PROTOCOL_CURVE) {
                amountOut = _swapCurve(
                    step.router,
                    step.tokenIn,
                    step.tokenOut,
                    amountIn,
                    step.minAmountOut,
                    step.swapData
                );
            } else {
                // Generic router call for other protocols
                amountOut = _swapGeneric(
                    step.router,
                    step.tokenIn,
                    amountIn,
                    step.swapData
                );
            }

            if (amountOut < step.minAmountOut) revert InsufficientOutput();

            unchecked {
                ++i;
            }
        }
    }

    /**
     * @notice Execute swap on Uniswap V2 compatible router
     * @dev SECURITY: Uses safeApprove pattern - approves exact amount needed
     *      This prevents approval front-running attacks where an attacker
     *      could use existing approval before new amount is set
     *
     * GAS OPTIMIZATION: Uses block.timestamp for deadline (same block execution)
     */
    function _swapUniswapV2(
        address router,
        address tokenIn,
        address tokenOut,
        uint256 amountIn,
        uint256 minAmountOut,
        bytes memory /* swapData */
    ) internal returns (uint256 amountOut) {
        // SECURITY: Reset approval to 0 first to handle non-standard tokens (e.g., USDT)
        // that require approval to be 0 before setting a new value
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

        amountOut = amounts[amounts.length - 1];

        // GAS OPTIMIZATION: Clear approval after swap to prevent lingering approvals
        // This also protects against potential router vulnerabilities
        _safeApprove(tokenIn, router, 0);
    }

    /**
     * @notice Execute swap on Uniswap V3
     * @dev SECURITY: Uses safeApprove pattern with reset to 0
     *      Fee is decoded from swapData, defaults to 0.3% (3000) if not provided
     */
    function _swapUniswapV3(
        address router,
        address tokenIn,
        address tokenOut,
        uint256 amountIn,
        uint256 minAmountOut,
        bytes memory swapData
    ) internal returns (uint256 amountOut) {
        // SECURITY: Reset and set approval (handles non-standard tokens)
        _safeApprove(tokenIn, router, 0);
        _safeApprove(tokenIn, router, amountIn);

        // Decode fee from swapData (default 0.3% = 3000 basis points)
        uint24 fee = swapData.length >= 3
            ? abi.decode(swapData, (uint24))
            : 3000;

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

        // SECURITY: Clear approval after swap
        _safeApprove(tokenIn, router, 0);
    }

    /**
     * @notice Execute swap on Balancer
     * @dev SECURITY: Balancer vault is a constant address (same on all chains)
     *      No router validation needed as BALANCER_VAULT is immutable
     */
    function _swapBalancer(
        address tokenIn,
        address tokenOut,
        uint256 amountIn,
        uint256 minAmountOut,
        bytes memory swapData
    ) internal returns (uint256 amountOut) {
        // SECURITY: Validate swapData contains poolId
        if (swapData.length < 32) revert InvalidSwapData();

        // SECURITY: Reset and set approval
        _safeApprove(tokenIn, BALANCER_VAULT, 0);
        _safeApprove(tokenIn, BALANCER_VAULT, amountIn);

        // Decode poolId from swapData
        bytes32 poolId = abi.decode(swapData, (bytes32));

        IBalancerVault.SingleSwap memory singleSwap = IBalancerVault.SingleSwap(
            {
                poolId: poolId,
                kind: IBalancerVault.SwapKind.GIVEN_IN,
                assetIn: tokenIn,
                assetOut: tokenOut,
                amount: amountIn,
                userData: ""
            }
        );

        IBalancerVault.FundManagement memory funds = IBalancerVault
            .FundManagement({
                sender: address(this),
                fromInternalBalance: false,
                recipient: payable(address(this)),
                toInternalBalance: false
            });

        amountOut = IBalancerVault(BALANCER_VAULT).swap(
            singleSwap,
            funds,
            minAmountOut,
            block.timestamp
        );

        // SECURITY: Clear approval after swap
        _safeApprove(tokenIn, BALANCER_VAULT, 0);
    }

    /**
     * @notice Execute swap on Curve Finance pool
     * @param pool Curve pool address
     * @param tokenIn Input token address
     * @param tokenOut Output token address (used for validation)
     * @param amountIn Amount of input tokens
     * @param minAmountOut Minimum output amount
     * @param swapData ABI encoded (int128 i, int128 j, bool isEthPool) coin indices and ETH flag
     * @return amountOut Amount of output tokens received
     * @dev SECURITY: Pool must be in approvedRouters whitelist
     *      swapData format: abi.encode(int128 i, int128 j, bool isEthPool)
     *      - i: input coin index in the pool
     *      - j: output coin index in the pool
     *      - isEthPool: true if pool uses native ETH (like stETH pool)
     *
     * SUPPORTED POOLS:
     * - 3pool (DAI/USDC/USDT): indices 0=DAI, 1=USDC, 2=USDT
     * - stETH pool (ETH/stETH): indices 0=ETH, 1=stETH (isEthPool=true)
     * - FRAX/USDC: indices 0=FRAX, 1=USDC
     */
    function _swapCurve(
        address pool,
        address tokenIn,
        address tokenOut,
        uint256 amountIn,
        uint256 minAmountOut,
        bytes memory swapData
    ) internal returns (uint256 amountOut) {
        // SECURITY: Validate swapData contains coin indices
        if (swapData.length < 64) revert InvalidSwapData();

        // Decode coin indices and ETH flag from swapData
        (int128 i, int128 j, bool isEthPool) = abi.decode(swapData, (int128, int128, bool));

        // Record output token balance before swap
        uint256 balanceBefore = _getBalance(tokenOut);

        if (isEthPool && i == 0) {
            // ETH input: for pools like stETH where coin 0 is ETH
            // We need WETH to be unwrapped first, or use ETH directly
            // For simplicity, assume we're working with WETH and need to unwrap
            // In production, you might need to handle WETH unwrapping

            // Execute swap with ETH value
            ICurvePoolETH(pool).exchange{value: amountIn}(
                i,
                j,
                amountIn,
                minAmountOut
            );
        } else {
            // ERC20 token input
            // SECURITY: Reset and set approval
            _safeApprove(tokenIn, pool, 0);
            _safeApprove(tokenIn, pool, amountIn);

            // Execute Curve exchange
            // Using try/catch to handle both int128 and uint256 pool interfaces
            try ICurvePool(pool).exchange(i, j, amountIn, minAmountOut) returns (uint256 result) {
                // Some pools return the output amount
                if (result > 0) {
                    amountOut = result;
                }
            } catch {
                // Fallback: pool might not return value, calculate from balance change
            }

            // SECURITY: Clear approval after swap
            _safeApprove(tokenIn, pool, 0);
        }

        // Calculate actual output from balance change if not set
        if (amountOut == 0) {
            uint256 balanceAfter = _getBalance(tokenOut);
            if (balanceAfter > balanceBefore) {
                amountOut = balanceAfter - balanceBefore;
            }
        }

        // For ETH output (j == 0 on ETH pools), check ETH balance
        // Note: Contract should have receive() function to accept ETH
    }

    /**
     * @notice Execute generic swap with raw calldata
     * @dev SECURITY: This function allows arbitrary calls to approved routers
     *      Router MUST be in approvedRouters whitelist (checked in _executeArbitrage)
     *      Use with caution - ensure swapData is properly validated off-chain
     *
     * WARNING: This calculates output as change in tokenIn balance, which may not
     *          be accurate for all swap types. Consider using protocol-specific
     *          functions when possible.
     */
    function _swapGeneric(
        address router,
        address tokenIn,
        uint256 amountIn,
        bytes memory swapData
    ) internal returns (uint256 amountOut) {
        // SECURITY: Validate swapData is not empty
        if (swapData.length == 0) revert InvalidSwapData();

        // SECURITY: Reset and set approval
        _safeApprove(tokenIn, router, 0);
        _safeApprove(tokenIn, router, amountIn);

        // Record balance before
        uint256 balanceBefore = _getBalance(tokenIn);

        // Execute raw call
        // SECURITY NOTE: Router is validated in _executeArbitrage before reaching here
        (bool success, ) = router.call(swapData);
        if (!success) revert SwapFailed();

        // Calculate output (assume single output token)
        amountOut = balanceBefore - _getBalance(tokenIn);

        // SECURITY: Clear approval after swap
        _safeApprove(tokenIn, router, 0);
    }

    /*//////////////////////////////////////////////////////////////
                           ADMIN FUNCTIONS
    //////////////////////////////////////////////////////////////*/

    /**
     * @notice Approve a router for swap execution
     * @param router Router address to approve
     * @param approved Approval status
     * @dev SECURITY: Only owner can modify whitelist. Review router contracts
     *      before adding to whitelist to ensure they don't have vulnerabilities.
     */
    function setApprovedRouter(
        address router,
        bool approved
    ) external onlyOwner {
        if (router == address(0)) revert ZeroAddress();
        approvedRouters[router] = approved;
    }

    /**
     * @notice Approve an Aave pool for flashloan execution
     * @param pool Aave V3 Pool address to approve
     * @param approved Approval status
     * @dev SECURITY: Only add official Aave pool addresses. Fake pools can
     *      call our callback with malicious data.
     */
    function setApprovedAavePool(
        address pool,
        bool approved
    ) external onlyOwner {
        if (pool == address(0)) revert ZeroAddress();
        approvedAavePools[pool] = approved;
    }

    /**
     * @notice Withdraw tokens from contract
     * @param token Token address (address(0) for ETH)
     * @param amount Amount to withdraw
     */
    function withdraw(address token, uint256 amount) external onlyOwner {
        if (amount == 0) revert ZeroAmount();

        if (token == address(0)) {
            // Withdraw ETH
            (bool success, ) = owner.call{value: amount}("");
            if (!success) revert SwapFailed();
        } else {
            // Withdraw ERC20
            _safeTransfer(token, owner, amount);
        }

        emit Withdrawal(token, amount, owner);
    }

    /**
     * @notice Withdraw all of a token
     * @param token Token address
     */
    function withdrawAll(address token) external onlyOwner {
        uint256 balance = token == address(0)
            ? address(this).balance
            : _getBalance(token);

        if (balance == 0) revert ZeroAmount();

        if (token == address(0)) {
            (bool success, ) = owner.call{value: balance}("");
            if (!success) revert SwapFailed();
        } else {
            _safeTransfer(token, owner, balance);
        }

        emit Withdrawal(token, balance, owner);
    }

    /**
     * @notice Transfer ownership (two-step for safety)
     * @param newOwner New owner address
     */
    function transferOwnership(address newOwner) external onlyOwner {
        if (newOwner == address(0)) revert ZeroAddress();
        pendingOwner = newOwner;
    }

    /**
     * @notice Accept ownership transfer
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
     * @notice Get token balance of this contract
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
