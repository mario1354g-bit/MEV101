// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

import "./interfaces/IBalancerVault.sol";
import "./interfaces/IAaveV3.sol";
import "./interfaces/IUniswapV2.sol";
import "./interfaces/IUniswapV3.sol";

/**
 * @title FlashloanArbitrage
 * @notice Gas-optimized flashloan arbitrage contract for MEV extraction
 * @dev Supports Balancer (0% fee) and Aave V3 flashloans with multi-DEX routing
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
    mapping(address => bool) public approvedRouters;

    /// @notice Flash loan in progress flag (for callback validation)
    bool private _flashLoanInProgress;

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
     */
    function executeBalancerFlashloan(
        address[] calldata tokens,
        uint256[] calldata amounts,
        bytes calldata swapData
    ) external onlyOwner nonReentrant {
        uint256 gasStart = gasleft();

        // Validate inputs
        if (tokens.length == 0 || tokens.length != amounts.length)
            revert InvalidSwapData();

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
     */
    function receiveFlashLoan(
        address[] memory tokens,
        uint256[] memory amounts,
        uint256[] memory feeAmounts,
        bytes memory userData
    ) external override {
        // Validate callback is from Balancer Vault
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
     */
    function executeAaveFlashloan(
        address pool,
        address[] calldata assets,
        uint256[] calldata amounts,
        bytes calldata swapData
    ) external onlyOwner nonReentrant {
        uint256 gasStart = gasleft();

        // Validate inputs
        if (assets.length == 0 || assets.length != amounts.length)
            revert InvalidSwapData();

        // Set flag
        _flashLoanInProgress = true;

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

        _flashLoanInProgress = false;

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
     */
    function executeOperation(
        address[] calldata assets,
        uint256[] calldata amounts,
        uint256[] calldata premiums,
        address initiator,
        bytes calldata params
    ) external returns (bool) {
        // Validate callback
        if (initiator != address(this)) revert InvalidCallback();
        if (!_flashLoanInProgress) revert InvalidCallback();

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
     */
    function _executeArbitrage(
        SwapStep[] memory steps
    ) internal returns (uint256 profit) {
        for (uint256 i = 0; i < steps.length; ) {
            SwapStep memory step = steps[i];

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
     */
    function _swapUniswapV2(
        address router,
        address tokenIn,
        address tokenOut,
        uint256 amountIn,
        uint256 minAmountOut,
        bytes memory /* swapData */
    ) internal returns (uint256 amountOut) {
        // Approve router
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
    }

    /**
     * @notice Execute swap on Uniswap V3
     */
    function _swapUniswapV3(
        address router,
        address tokenIn,
        address tokenOut,
        uint256 amountIn,
        uint256 minAmountOut,
        bytes memory swapData
    ) internal returns (uint256 amountOut) {
        // Approve router
        _safeApprove(tokenIn, router, amountIn);

        // Decode fee from swapData
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
    }

    /**
     * @notice Execute swap on Balancer
     */
    function _swapBalancer(
        address tokenIn,
        address tokenOut,
        uint256 amountIn,
        uint256 minAmountOut,
        bytes memory swapData
    ) internal returns (uint256 amountOut) {
        // Approve vault
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
    }

    /**
     * @notice Execute generic swap with raw calldata
     */
    function _swapGeneric(
        address router,
        address tokenIn,
        uint256 amountIn,
        bytes memory swapData
    ) internal returns (uint256 amountOut) {
        // Approve router
        _safeApprove(tokenIn, router, amountIn);

        // Record balance before
        uint256 balanceBefore = _getBalance(tokenIn);

        // Execute raw call
        (bool success, ) = router.call(swapData);
        if (!success) revert SwapFailed();

        // Calculate output (assume single output token)
        amountOut = balanceBefore - _getBalance(tokenIn);
    }

    /*//////////////////////////////////////////////////////////////
                           ADMIN FUNCTIONS
    //////////////////////////////////////////////////////////////*/

    /**
     * @notice Approve a router for swap execution
     * @param router Router address to approve
     * @param approved Approval status
     */
    function setApprovedRouter(
        address router,
        bool approved
    ) external onlyOwner {
        if (router == address(0)) revert ZeroAddress();
        approvedRouters[router] = approved;
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
