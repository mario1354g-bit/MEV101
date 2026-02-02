// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

/**
 * @title IPoolAddressesProvider
 * @notice Interface for Aave V3 Pool Addresses Provider
 */
interface IPoolAddressesProvider {
    /**
     * @notice Get the address of the Pool
     * @return Pool contract address
     */
    function getPool() external view returns (address);

    /**
     * @notice Get the address of the Price Oracle
     * @return Price Oracle contract address
     */
    function getPriceOracle() external view returns (address);
}

/**
 * @title IPool
 * @notice Interface for Aave V3 Pool
 */
interface IPool {
    /**
     * @notice Execute flash loan on single asset
     * @param receiverAddress Address receiving the flash loan
     * @param asset Address of the asset to flash loan
     * @param amount Amount to flash loan
     * @param params Arbitrary bytes to pass to receiver
     * @param referralCode Referral code (use 0)
     */
    function flashLoanSimple(
        address receiverAddress,
        address asset,
        uint256 amount,
        bytes calldata params,
        uint16 referralCode
    ) external;

    /**
     * @notice Execute flash loan on multiple assets
     * @param receiverAddress Address receiving the flash loan
     * @param assets Array of asset addresses
     * @param amounts Array of amounts to flash loan
     * @param interestRateModes Array of interest rate modes (0 = no debt, 1 = stable, 2 = variable)
     * @param onBehalfOf Address that will incur debt (if mode != 0)
     * @param params Arbitrary bytes to pass to receiver
     * @param referralCode Referral code (use 0)
     */
    function flashLoan(
        address receiverAddress,
        address[] calldata assets,
        uint256[] calldata amounts,
        uint256[] calldata interestRateModes,
        address onBehalfOf,
        bytes calldata params,
        uint16 referralCode
    ) external;

    /**
     * @notice Get reserve data for an asset
     * @param asset The asset address
     * @return configuration Reserve configuration
     * @return liquidityIndex Liquidity index
     * @return currentLiquidityRate Current liquidity rate
     * @return variableBorrowIndex Variable borrow index
     * @return currentVariableBorrowRate Current variable borrow rate
     * @return currentStableBorrowRate Current stable borrow rate
     * @return lastUpdateTimestamp Last update timestamp
     * @return id Reserve id
     * @return aTokenAddress aToken address
     * @return stableDebtTokenAddress Stable debt token address
     * @return variableDebtTokenAddress Variable debt token address
     * @return interestRateStrategyAddress Interest rate strategy address
     * @return accruedToTreasury Accrued to treasury
     * @return unbacked Unbacked amount
     * @return isolationModeTotalDebt Isolation mode total debt
     */
    function getReserveData(
        address asset
    )
        external
        view
        returns (
            uint256 configuration,
            uint128 liquidityIndex,
            uint128 currentLiquidityRate,
            uint128 variableBorrowIndex,
            uint128 currentVariableBorrowRate,
            uint128 currentStableBorrowRate,
            uint40 lastUpdateTimestamp,
            uint16 id,
            address aTokenAddress,
            address stableDebtTokenAddress,
            address variableDebtTokenAddress,
            address interestRateStrategyAddress,
            uint128 accruedToTreasury,
            uint128 unbacked,
            uint128 isolationModeTotalDebt
        );

    /**
     * @notice Get flash loan premium total
     * @return Premium in basis points (e.g., 9 = 0.09%)
     */
    function FLASHLOAN_PREMIUM_TOTAL() external view returns (uint128);
}

/**
 * @title IFlashLoanSimpleReceiver
 * @notice Interface for Aave V3 simple flash loan receiver
 */
interface IFlashLoanSimpleReceiver {
    /**
     * @notice Execute operation after receiving flash loan
     * @param asset The address of the flash-borrowed asset
     * @param amount The amount of the flash-borrowed asset
     * @param premium The fee for the flash loan
     * @param initiator The address that initiated the flash loan
     * @param params Arbitrary bytes passed from flash loan call
     * @return True if operation succeeded and repayment is approved
     */
    function executeOperation(
        address asset,
        uint256 amount,
        uint256 premium,
        address initiator,
        bytes calldata params
    ) external returns (bool);

    /**
     * @notice Get the Pool Addresses Provider
     * @return Pool Addresses Provider address
     */
    function ADDRESSES_PROVIDER()
        external
        view
        returns (IPoolAddressesProvider);

    /**
     * @notice Get the Pool
     * @return Pool address
     */
    function POOL() external view returns (IPool);
}

/**
 * @title IFlashLoanReceiver
 * @notice Interface for Aave V3 multi-asset flash loan receiver
 */
interface IFlashLoanReceiver {
    /**
     * @notice Execute operation after receiving flash loan
     * @param assets Array of flash-borrowed asset addresses
     * @param amounts Array of flash-borrowed amounts
     * @param premiums Array of fees for each asset
     * @param initiator The address that initiated the flash loan
     * @param params Arbitrary bytes passed from flash loan call
     * @return True if operation succeeded and repayment is approved
     */
    function executeOperation(
        address[] calldata assets,
        uint256[] calldata amounts,
        uint256[] calldata premiums,
        address initiator,
        bytes calldata params
    ) external returns (bool);

    /**
     * @notice Get the Pool Addresses Provider
     * @return Pool Addresses Provider address
     */
    function ADDRESSES_PROVIDER()
        external
        view
        returns (IPoolAddressesProvider);

    /**
     * @notice Get the Pool
     * @return Pool address
     */
    function POOL() external view returns (IPool);
}
