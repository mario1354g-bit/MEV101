# GitHub MEV Bot Analysis - Source Code Study

## Research Date: February 2026
## Objective: Analyze MEV bot implementations to understand architecture, patterns, and best practices

---

## TABLE OF CONTENTS

1. [Official Flashbots Repositories](#official-flashbots-repositories)
2. [Uniswap Arbitrage Bots](#uniswap-arbitrage-bots)
3. [SushiSwap Arbitrage](#sushiswap-arbitrage)
4. [L2 Arbitrage Bots](#l2-arbitrage-bots)
5. [Flashloan Arbitrage](#flashloan-arbitrage)
6. [MEV Simulation Tools](#mev-simulation-tools)
7. [Architecture Patterns](#architecture-patterns)
8. [Code Best Practices](#code-best-practices)
9. [Common Vulnerabilities](#common-vulnerabilities)
10. [Deployment Strategies](#deployment-strategies)

---

## OFFICIAL FLASHBOTS REPOSITORIES

### 1. flashbots/simple-arbitrage
**URL**: https://github.com/flashbots/simple-arbitrage
**Stars**: 2.1k+
**Purpose**: Example arbitrage bot using Flashbots bundles

#### Architecture:
```
┌─────────────────┐
│   Scanner       │  - Monitors DEX prices
│                 │  - Detects arbitrage
└────────┬────────┘
         │
         ▼
┌─────────────────┐
│   Evaluator    │  - Calculates profit
│                 │  - Estimates gas cost
└────────┬────────┘
         │
         ▼
┌─────────────────┐
│   Bundle Builder │  - Constructs tx bundle
│                 │  - Includes miner bribe
└────────┬────────┘
         │
         ▼
┌─────────────────┐
│ Flashbots Client │  - Submits to relay
│                 │  - Handles retries
└─────────────────┘
```

#### Smart Contract: BundleExecutor.sol
```solidity
// SPDX-License-Identifier: UNLICENSED
pragma solidity 0.6.12;
pragma experimental ABIEncoderV2;

contract FlashBotsMultiCall {
    address private immutable owner;
    address private immutable executor;
    IWETH private constant WETH = IWETH(0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2);

    modifier onlyExecutor() {
        require(msg.sender == executor);
        _;
    }

    modifier onlyOwner() {
        require(msg.sender == owner);
        _;
    }

    constructor(address _executor) public payable {
        owner = msg.sender;
        executor = _executor;
        if (msg.value > 0) {
            WETH.deposit{value: msg.value}();
        }
    }

    // Main arbitrage function
    function uniswapWeth(
        uint256 _wethAmountToFirstMarket,
        uint256 _ethAmountToCoinbase,
        address[] memory _targets,
        bytes[] memory _payloads
    ) external onlyExecutor payable {
        require(_targets.length == _payloads.length);

        uint256 _wethBalanceBefore = WETH.balanceOf(address(this));

        // Transfer WETH to first DEX
        WETH.transfer(_targets[0], _wethAmountToFirstMarket);

        // Execute all swaps sequentially
        for (uint256 i = 0; i < _targets.length; i++) {
            (bool _success, bytes memory _response) = _targets[i].call(_payloads[i]);
            require(_success);
            _response;
        }

        // Check profit
        uint256 _wethBalanceAfter = WETH.balanceOf(address(this));
        require(_wethBalanceAfter > _wethBalanceBefore + _ethAmountToCoinbase);

        // Pay miner
        if (_ethAmountToCoinbase == 0) return;

        uint256 _ethBalance = address(this).balance;
        if (_ethBalance < _ethAmountToCoinbase) {
            WETH.withdraw(_ethAmountToCoinbase - _ethBalance);
        }
        block.coinbase.transfer(_ethAmountToCoinbase);
    }

    // Owner control function
    function call(
        address payable _to,
        uint256 _value,
        bytes calldata _data
    ) external onlyOwner payable returns (bytes memory) {
        require(_to != address(0));
        (bool _success, bytes memory _result) = _to.call{value: _value}(_data);
        require(_success);
        return _result;
    }
}
```

#### Key Patterns:
1. **Sequential Execution**: All swaps in one transaction
2. **Atomicity**: Either all succeed or all revert
3. **Profit Guarantee**: Requires `balanceAfter > balanceBefore + bribe`
4. **Miner Payment**: Direct transfer to `block.coinbase`
5. **Security**: `onlyExecutor` and `onlyOwner` modifiers

#### Environment Variables:
```bash
ETHEREUM_RPC_URL="https://mainnet.infura.io/v3/YOUR_KEY"
PRIVATE_KEY="0x..."  # Bot wallet
FLASHBOTS_RELAY_SIGNING_KEY="0x..."  # Auth key
HEALTHCHECK_URL="https://your-healthcheck.com"
MINER_REWARD_PERCENTAGE=80  # 80% to miner
```

#### Usage Flow:
```bash
# 1. Install dependencies
npm install

# 2. Deploy BundleExecutor
npx hardhat run scripts/deploy.js

# 3. Fund with WETH
npx hardhat run scripts/fund.js

# 4. Start bot
PRIVATE_KEY=... \
BUNDLE_EXECUTOR_ADDRESS=0x... \
FLASHBOTS_RELAY_SIGNING_KEY=... \
npm run start
```

#### Why Use This Pattern:
- **Failed-bid protection**: Flashbots doesn't charge for failed bundles
- **Atomic execution**: All-or-nothing transaction ordering
- **Private mempool**: Hides your strategy from competitors
- **Predictable costs**: Know exact gas cost before submission

---

### 2. flashbots/mev-inspect-py
**URL**: https://github.com/flashbots/mev-inspect-py
**Status**: ⚠️ Deprecated (use Flashbots Data)
**Purpose**: MEV inspection and analysis tool

#### Architecture:
```
┌─────────────────┐
│   Block Listener │  - Monitors new blocks
│                 │  - Queues for inspection
└────────┬────────┘
         │
         ▼
┌─────────────────┐
│  MEV Inspectors │  - Detects sandwich attacks
│                 │  - Identifies arbitrage
│                 │  - Tracks liquidations
└────────┬────────┘
         │
         ▼
┌─────────────────┐
│   PostgreSQL    │  - Stores MEV data
│                 │  - Enables analysis
└─────────────────┘
```

#### Key Capabilities:
1. **Block Inspection**: Analyze historical blocks for MEV
2. **Real-time Listener**: Monitor incoming blocks
3. **MEV Classification**:
   - Arbitrage
   - Sandwich attacks
   - Liquidations
   - Backruns
4. **Profit Calculation**: Track MEV extraction amounts

#### Usage:
```bash
# Inspect a single block
./mev inspect 12914944

# Inspect many blocks
./mev inspect-many 12914944 12914954

# Start block listener
./mev listener start

# Tail logs
./mev listener tail

# Backfill historical blocks
./mev backfill 12914944 12915044

# Query database
./mev db
```

#### Database Queries:
```sql
-- Count swaps on UniswapV3
SELECT COUNT(*) FROM swaps
WHERE abi_name = 'UniswapV3Pool';

-- Top arbitrage by profit (WETH)
SELECT *
FROM arbitrages
WHERE profit_token_address = '0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2'
ORDER BY profit_amount DESC
LIMIT 10;
```

#### Learnings for Solo Searchers:
1. **Pattern Recognition**: Understand how MEV is detected
2. **Historical Analysis**: Study profitable patterns
3. **Database Design**: Track your own performance
4. **MEV Classification**: Differentiate MEV types

---

### 3. flashbots/searcher-sponsored-tx
**URL**: https://github.com/flashbots/searcher-sponsored-tx
**Purpose**: Sponsored transaction execution for compromised wallets

#### Use Case:
Execute transactions from compromised account without funding it directly (bots would sweep funds)

#### Architecture:
```typescript
// Bundle structure
const bundle = [
  sponsorTx,      // Transfer ETH to executor
  executorTx1,     // Execute with sponsored funds
  executorTx2,
  ...
]
```

#### Key Pattern:
```typescript
const PRIORITY_GAS_PRICE = GWEI.mul(31)

// All transactions use same gasPrice
// No direct coinbase transfers
// Atomic execution prevents sweeper bots
```

#### Environment Variables:
```bash
ETHEREUM_RPC_URL="https://..."
PRIVATE_KEY_EXECUTOR="0x..."  # Compromised wallet
PRIVATE_KEY_SPONSOR="0x..."    # Funded wallet
RECIPIENT="0x..."               # Receive assets
FLASHBOTS_RELAY_SIGNING_KEY="0x..."
```

#### Why This Pattern Matters:
- **Atomic funding**: Receive ETH and spend in same block
- **Prevents front-running**: Sweep bots can't intercept
- **Useful for MEV**: Separate execution from funding

---

### 4. flashbots/mev-flood
**URL**: https://github.com/flashbots/mev-flood
**Purpose**: Simulate MEV activity on EVM networks

#### Features:
- Generate test transactions
- Simulate various MEV patterns
- Load test MEV infrastructure
- CLI for quick setup

#### Use Cases:
- Test MEV detection systems
- Load test Flashbots relay
- Simulate MEV scenarios
- Develop and debug MEV bots

---

## UNISWAP ARBITRAGE BOTS

### 1. codeesura/Arbitrage-uniswap-sushiswap
**URL**: https://github.com/codeesura/Arbitrage-uniswap-sushiswap
**Purpose**: Smart contract arbitrage between Uniswap and SushiSwap

#### Smart Contract Pattern:
```solidity
pragma solidity ^0.6.0;

import "@uniswap/v2-core/contracts/interfaces/IUniswapV2Router02.sol";
import "@uniswap/v2-core/contracts/interfaces/IUniswapV2Factory.sol";

contract Arbitrage {
    IUniswapV2Router02 public uniswapRouter;
    IUniswapV2Router02 public sushiswapRouter;

    constructor(address _uniRouter, address _sushiRouter) public {
        uniswapRouter = IUniswapV2Router02(_uniRouter);
        sushiswapRouter = IUniswapV2Router02(_sushiRouter);
    }

    // Execute arbitrage: WETH -> Token (Uniswap) -> WETH (SushiSwap)
    function arbitrage(
        address _token,
        uint256 _amount,
        uint256 _minProfit
    ) external payable {
        // Step 1: Buy on Uniswap
        uint256[] memory amounts = uniswapRouter.swapExactETHForTokens{value: msg.value}(
            0,  // Accept any amount
            getPath(uniswapRouter.WETH(), _token),
            address(this),
            block.timestamp
        );

        // Step 2: Sell on SushiSwap
        uint256 wethAmount = sushiswapRouter.swapExactTokensForETH(
            amounts[1],  // Token amount
            _minProfit,   // Minimum WETH out
            getPath(_token, sushiswapRouter.WETH()),
            address(this),
            block.timestamp
        );

        // Return profit to sender
        msg.sender.transfer(wethAmount);
    }

    function getPath(address _tokenA, address _tokenB) internal pure returns (address[] memory) {
        address[] memory path = new address[](2);
        path[0] = _tokenA;
        path[1] = _tokenB;
        return path;
    }

    receive() external payable {}
}
```

#### Key Patterns:
1. **Router Abstraction**: Use IUniswapV2Router interfaces
2. **Path Construction**: Flexible token routing
3. **Minimum Profit**: Ensure arbitrage is profitable
4. **No Flashloans**: Requires upfront capital

---

### 2. 6eer/uniswap-sushiswap-arbitrage-bot
**URL**: https://github.com/6eer/uniswap-sushiswap-arbitrage-bot
**Purpose**: Example arbitrage bot with deployment scripts

#### Architecture:
```
┌─────────────────┐
│   Price Monitor │  - Query DEX prices
│                 │  - Detect arbitrage
└────────┬────────┘
         │
         ▼
┌─────────────────┐
│   Simulation    │  - Test on fork
│                 │  - Verify profit
└────────┬────────┘
         │
         ▼
┌─────────────────┐
│   Execution    │  - Execute trade
│                 │  - Track results
└─────────────────┘
```

#### Testing Pattern:
```javascript
// Test on forked mainnet
const network = await ethers.provider.getNetwork()
console.log("Network:", network.name)

// Fork mainnet
await hre.network.provider.request({
  method: "hardhat_reset",
  params: [{
    forking: {
      jsonRpcUrl: process.env.MAINNET_RPC_URL,
      blockNumber: await ethers.provider.getBlockNumber() - 10
    }
  }]
})

// Test arbitrage
const result = await arbitrageContract.arbitrage(
  tokenAddress,
  amount,
  minProfit
)

console.log("Profit:", result.toString())
```

---

### 3. ccyanxyz/uniswap-arbitrage-analysis
**URL**: https://github.com/ccyanxyz/uniswap-arbitrage-analysis
**Purpose**: Analyze arbitrage opportunities on Uniswap

#### Key Insights:
- **Volume Correlation**: High volume pairs have more opportunities
- **Competition**: Popular pairs are more competitive
- **Profit Distribution**: Long-tail of small profits, few large profits

---

## L2 ARBITRAGE BOTS

### 1. SimSimButDifferent/UniV3FlashSwapDualArbBot
**URL**: https://github.com/SimSimButDifferent/UniV3FlashSwapDualArbBot
**Deployed**: Arbitrum at 0xf812197dbdbcd0f80cd003c20f695dc8d06bc3b0
**Purpose**: Uniswap V3 flashswap arbitrage on L2

#### Architecture:
```
┌─────────────────┐
│   Pool Scanner │  - Query Uniswap V3 subgraph
│                 │  - Get pool data
└────────┬────────┘
         │
         ▼
┌─────────────────┐
│   Route Finder │  - Find all routes
│                 │  - Calculate profits
└────────┬────────┘
         │
         ▼
┌─────────────────┐
│   Quoter       │  - Get quotes
│                 │  - Filter profitable
└────────┬────────┘
         │
         ▼
┌─────────────────┐
│ Flashswap Exec  │  - Execute flashswap
│                 │  - Handle profit
└─────────────────┘
```

#### Pool Scanner (getPools.js):
```javascript
async function getPools() {
    const query = `
        {
            pools(
                where: {
                    token0_in: [WETH, WBTC, ARB, USDT],
                    token1_in: [WETH, WBTC, ARB, USDT],
                    volumeUSD_gte: 500000,
                    totalValueLockedUSD_gt: 1000000,
                    id_not: "0x14af1804dbbf7d621ecc2901eef292a24a0260ea"
                }
                first: 30,
                orderBy: totalValueLockedUSD,
                orderDirection: desc
            ) {
                id
                token0 { symbol }
                token1 { symbol }
                feeTier
                totalValueLockedUSD
                volumeUSD
            }
        }
    `

    const data = await fetch(UNISWAP_SUBGRAPH, {
        method: 'POST',
        body: JSON.stringify({ query })
    })

    return data.data.pools
}
```

#### Route Scanner (dualArbScan.js):
```javascript
const BATCH_SIZE = 5
const BATCH_INTERVAL = 8000  // 8 seconds

async function scanArbitrage(pools, tokenInfo) {
    const routes = getAllRoutes(pools)

    // Batch quotes to manage API limits
    for (let i = 0; i < routes.length; i += BATCH_SIZE) {
        const batch = routes.slice(i, i + BATCH_SIZE)

        const quotes = await Promise.all(
            batch.map(route => getQuote(route, tokenInfo))
        )

        for (const quote of quotes) {
            const profit = calculateProfit(quote, tokenInfo)

            if (profit > quote.profitThreshold) {
                console.log(`Found arbitrage: ${profit}`)
                await executeFlashswap(quote)
            }
        }

        // Wait between batches
        await sleep(BATCH_INTERVAL)
    }
}
```

#### Flashswap Contract Pattern:
```solidity
pragma solidity ^0.7.6;

import "@uniswap/v3-core/contracts/interfaces/IUniswapV3Pool.sol";
import "@uniswap/v3-core/contracts/libraries/TickMath.sol";
import "@openzeppelin/contracts/token/ERC20/IERC20.sol";

contract FlashSwapV3 {
    struct FlashParams {
        address pool0;
        address pool1;
        uint256 amount0;
        uint24 fee0;
        uint256 amount1;
        uint24 fee1;
        address tokenIn;
        address tokenOut;
    }

    function executeFlashswap(
        FlashParams calldata params,
        uint256 amountIn
    ) external {
        // Step 1: Borrow from pool0
        uint256 balanceBefore = IERC20(params.tokenIn).balanceOf(address(this));

        bytes memory data = abi.encode(
            params.pool1,
            params.amount1,
            params.fee1,
            params.tokenOut,
            amountIn
        );

        IUniswapV3Pool(params.pool0).swap(
            params.amount0 > 0 ? params.amount0 : -int256(amountIn),
            params.amount0 > 0 ? 0 : -int256(amountIn),
            0,
            address(this),
            data
        );

        // Step 2: Callback executed
        // Step 3: Return borrowed amount + fee
    }

    function uniswapV3SwapCallback(
        int256 amount0Delta,
        int256 amount1Delta,
        bytes calldata data
    ) external {
        // Ensure callback from pool
        require(msg.sender == abi.decode(data, (address)));

        // Decode params
        (
            address pool1,
            uint256 amount1,
            uint24 fee1,
            address tokenOut,
            uint256 originalAmount
        ) = abi.decode(data, (address, uint256, uint24, address, uint256));

        // Step 2: Swap on pool1
        IUniswapV3Pool(pool1).swap(
            amount1 > 0 ? amount1 : -int256(0),
            amount1 > 0 ? 0 : -int256(0),
            fee1,
            address(this),
            ""
        );

        // Calculate profit
        uint256 profit = IERC20(tokenOut).balanceOf(address(this)) - originalAmount;

        require(profit > 0, "No profit");

        // Step 3: Return borrowed amount + fee
        uint256 amount0Owed = amount0Delta > 0 ? uint256(amount0Delta) : 0;
        uint256 amount1Owed = amount1Delta > 0 ? uint256(amount1Delta) : 0;

        if (amount0Owed > 0) {
            IERC20(IUniswapV3Pool(msg.sender).token0()).transfer(
                msg.sender,
                amount0Owed
            );
        }

        if (amount1Owed > 0) {
            IERC20(IUniswapV3Pool(msg.sender).token1()).transfer(
                msg.sender,
                amount1Owed
            );
        }

        // Send profit to sender
        IERC20(tokenOut).transfer(tx.origin, profit);
    }
}
```

#### Key Patterns:
1. **Flashswap Callback**: Uniswap V3 callback mechanism
2. **Batch Processing**: Manage API limits with batching
3. **Subgraph Integration**: Query Uniswap V3 subgraph for pools
4. **Profit Threshold**: Only execute if profitable
5. **L2 Optimization**: Lower gas costs, compute efficiency

#### Resources Used:
- ~1.4M compute units/day (~42M/month)
- Input: $10 USD per trade
- Profit threshold: 1%

#### Developer Notes:
- "This project is merely a working prototype"
- "Probably won't make you money"
- "People you're up against are far more efficient"
- "Use at your own risk"

---

## FLASHLOAN ARBITRAGE

### 1. manuelinfosec/flash-arb-bot
**URL**: https://github.com/manuelinfosec/flash-arb-bot
**Purpose**: Flash loan arbitrage on Ethereum

#### Flashloan Pattern:
```solidity
pragma solidity ^0.8.0;

import "@aave/v3-core/contracts/interfaces/IFlashLoanSimpleReceiver.sol";
import "@aave/v3-core/contracts/interfaces/IPoolAddressesProvider.sol";

contract FlashArbitrage is IFlashLoanSimpleReceiver {
    IPoolAddressesProvider public immutable provider;

    constructor(address _provider) {
        provider = IPoolAddressesProvider(_provider);
    }

    function executeArbitrage(
        address asset,
        uint256 amount,
        address[] calldata dexes,
        bytes[] calldata swapData
    ) external {
        IPool pool = IPool(provider.getPool());

        // Initiate flash loan
        pool.flashLoanSimple(
            address(this),
            asset,
            amount,
            abi.encode(dexes, swapData),
            0  // referral code
        );
    }

    function executeOperation(
        address asset,
        uint256 amount,
        uint256 premium,
        address initiator,
        bytes calldata params
    ) external override returns (bool) {
        require(msg.sender == address(provider.getPool()), "Unauthorized");
        require(initiator == address(this), "Unauthorized");

        // Decode params
        (address[] memory dexes, bytes[] memory swapData) = abi.decode(
            params,
            (address[], bytes[])
        );

        // Execute arbitrage
        uint256 balanceBefore = IERC20(asset).balanceOf(address(this));

        for (uint256 i = 0; i < dexes.length; i++) {
            (bool success, ) = dexes[i].call(swapData[i]);
            require(success, "Swap failed");
        }

        uint256 balanceAfter = IERC20(asset).balanceOf(address(this));

        // Repay flash loan + premium
        uint256 totalOwed = amount + premium;

        require(
            balanceAfter >= totalOwed,
            "Arbitrage failed"
        );

        // Approve pool
        IERC20(asset).approve(address(provider.getPool()), totalOwed);

        return true;
    }

    receive() external payable {}
}
```

#### Key Patterns:
1. **No Upfront Capital**: Flashloan provides capital
2. **Atomic Execution**: All-or-nothing
3. **Premium Payment**: Pay flash loan fee (usually 0.09% on Aave)
4. **Profit Check**: Must repay loan + premium

#### Flashloan Providers:
- **Aave**: 0.09% fee, most popular
- **dYdX**: 0% fee, limited access
- **Uniswap V3**: Flash swap, 0% fee (requires callback)

---

## MEV SIMULATION TOOLS

### 1. M1kuW1ll/MMASim
**URL**: https://github.com/M1kuW1ll/MMASim
**Purpose**: MEV-Boost auction simulation

#### Features:
- Simulate MEV-Boost auction
- Multiple searcher strategies
- Time-step auction simulation
- Bid behavior modeling

#### Use Cases:
- Test MEV-Boost strategies
- Model auction dynamics
- Research MEV economics

---

### 2. blockchainSupport1125/MEV-bot-simulation-ethereum
**URL**: https://github.com/blockchainSupport1125/MEV-bot-simulation-ethereum
**Purpose**: MEV bot simulation for gas fights

#### Features:
- Simulate gas auctions
- Calculate profitability
- Model MEV extraction

---

## ARCHITECTURE PATTERNS

### Pattern 1: Scanner → Simulator → Executor
```
Scanner:  Monitor mempool/blocks for opportunities
   ↓
Simulator: Test opportunity on forked mainnet
   ↓
Executor: Submit profitable trades
```

**Implementation**:
```javascript
async function main() {
    // Scan
    const opportunities = await scanner.scan()

    for (const opp of opportunities) {
        // Simulate
        const result = await simulator.test(opp)

        if (result.profit > 0) {
            // Execute
            await executor.execute(opp)
        }
    }
}
```

---

### Pattern 2: Bundle-First Architecture
```
Opportunity Detection → Bundle Construction → Flashbots Submission
```

**Implementation**:
```javascript
const buildBundle = (opportunity) => {
    const bundle = []

    // Add frontrun
    if (opportunity.frontrun) {
        bundle.push(opportunity.frontrun)
    }

    // Add victim
    bundle.push(opportunity.victim)

    // Add backrun
    if (opportunity.backrun) {
        bundle.push(opportunity.backrun)
    }

    // Add bribe
    bundle.push(createBribeTx(opportunity.profit * 0.8))

    return bundle
}
```

---

### Pattern 3: Multi-DEX Routing
```
Token A → DEX 1 → Token B → DEX 2 → Token C → DEX 3 → Token A
```

**Implementation**:
```javascript
const findRoute = async (tokenIn, tokenOut, amountIn) => {
    const routes = []

    for (const dex1 of DEXs) {
        const midAmount = await dex1.getAmountOut(tokenIn, tokenOut, amountIn)

        for (const dex2 of DEXs) {
            const finalAmount = await dex2.getAmountOut(tokenOut, tokenIn, midAmount)

            if (finalAmount > amountIn) {
                routes.push({
                    path: [tokenIn, tokenOut, tokenIn],
                    dexes: [dex1, dex2],
                    profit: finalAmount - amountIn
                })
            }
        }
    }

    return routes.sort((a, b) => b.profit - a.profit)[0]
}
```

---

## CODE BEST PRACTICES

### 1. Gas Optimization
```solidity
// ✅ Good: Use calldata
function execute(bytes calldata data) external {
    // ...
}

// ❌ Bad: Use memory
function execute(bytes memory data) external {
    // ...
}

// ✅ Good: Batch operations
for (uint i = 0; i < 10; i++) {
    // One transaction
}

// ❌ Bad: Multiple transactions
for (uint i = 0; i < 10; i++) {
    // 10 transactions
}

// ✅ Good: Use uint256
uint256 amount;

// ❌ Bad: Use smaller types (costs gas to pack)
uint128 amount;
```

---

### 2. Security Practices
```solidity
// ✅ Good: Access control
modifier onlyOwner() {
    require(msg.sender == owner, "Not owner");
    _;
}

// ✅ Good: Reentrancy protection
uint256 private locked;
modifier noReentrancy() {
    require(locked == 0, "Reentrancy");
    locked = 1;
    _;
    locked = 0;
}

// ✅ Good: Check-effects-interactions
function withdraw(uint256 amount) external noReentrancy {
    require(balances[msg.sender] >= amount, "Insufficient");

    // Check
    balances[msg.sender] -= amount;

    // Effect
    emit Withdrawal(msg.sender, amount);

    // Interaction
    payable(msg.sender).transfer(amount);
}
```

---

### 3. Error Handling
```javascript
// ✅ Good: Comprehensive error handling
try {
    const result = await contract.function()
    return result
} catch (error) {
    if (error.code === 'CALL_EXCEPTION') {
        console.error("Transaction reverted:", error.reason)
    } else if (error.code === 'NETWORK_ERROR') {
        console.error("Network error:", error)
    }
    throw error
}

// ❌ Bad: No error handling
const result = await contract.function()
return result
```

---

### 4. Testing
```javascript
// ✅ Good: Test on forked mainnet
describe("Arbitrage", () => {
    beforeEach(async () => {
        await hre.network.provider.request({
            method: "hardhat_reset",
            params: [{
                forking: {
                    jsonRpcUrl: process.env.MAINNET_RPC_URL,
                    blockNumber: await ethers.provider.getBlockNumber() - 10
                }
            }]
        })
    })

    it("should execute profitable arbitrage", async () => {
        const profit = await arbitrage.execute(...)
        assert(profit > 0)
    })

    it("should fail on unprofitable arbitrage", async () => {
        await expect(
            arbitrage.execute(...)
        ).to.be.revertedWith("Not profitable")
    })
})
```

---

## COMMON VULNERABILITIES

### 1. Integer Overflow
```solidity
// ❌ Bad: Solidity < 0.8
uint256 balance = balances[user] + amount

// ✅ Good: Solidity >= 0.8 (built-in overflow check)
uint256 balance = balances[user] + amount
```

---

### 2. Reentrancy
```solidity
// ❌ Bad: Reentrancy vulnerable
function withdraw(uint256 amount) external {
    require(balances[msg.sender] >= amount);

    payable(msg.sender).transfer(amount);  // External call

    balances[msg.sender] -= amount;  // State change after
}

// ✅ Good: Reentrancy protected
function withdraw(uint256 amount) external noReentrancy {
    require(balances[msg.sender] >= amount);

    balances[msg.sender] -= amount;  // State change first

    payable(msg.sender).transfer(amount);
}
```

---

### 3. Front-Running
```javascript
// ❌ Bad: Submit to public mempool
await provider.sendTransaction(tx)

// ✅ Good: Submit via Flashbots
await flashbots.sendBundle([tx], targetBlock)
```

---

### 4. Oracle Manipulation
```solidity
// ❌ Bad: Use stale DEX price as oracle
uint256 price = uniswap.getPrice(token)

// ✅ Good: Use TWAP or Chainlink
uint256 price = chainlink.latestRoundData(token)
```

---

## DEPLOYMENT STRATEGIES

### 1. Testnet → Mainnet Progression
```bash
# 1. Test on testnet
npx hardhat run scripts/deploy.js --network goerli

# 2. Verify contracts
npx hardhat verify --network goerli <address> <args>

# 3. Backtest on forked mainnet
npx hardhat test --network hardhat

# 4. Deploy to mainnet
npx hardhat run scripts/deploy.js --network mainnet

# 5. Start with small amounts
# Monitor performance
# Scale gradually
```

---

### 2. Infrastructure Setup
```yaml
# docker-compose.yml
version: '3.8'

services:
  bot:
    build: .
    environment:
      - PRIVATE_KEY=${PRIVATE_KEY}
      - ETHEREUM_RPC_URL=${ETHEREUM_RPC_URL}
      - FLASHBOTS_RELAY=${FLASHBOTS_RELAY}
    restart: unless-stopped
    logging:
      driver: "json-file"
      options:
        max-size: "10m"
        max-file: "3"

  postgres:
    image: postgres:14
    environment:
      - POSTGRES_DB=mev
      - POSTGRES_USER=mev
      - POSTGRES_PASSWORD=secret
    volumes:
      - postgres-data:/var/lib/postgresql/data

volumes:
  postgres-data:
```

---

## SUMMARY

### Key Takeaways:
1. **Start with simple arbitrage** → progress to complex strategies
2. **Use Flashbots** → protect from front-running
3. **Test extensively** → use testnets and forked mainnet
4. **Monitor everything** → track performance, adjust strategy
5. **Use proven patterns** → learn from GitHub repos
6. **Optimize for gas** → higher profits, lower costs
7. **Security first** → avoid common vulnerabilities

### Recommended Starting Points:
1. `flashbots/simple-arbitrage` - Simple, well-documented
2. `codeesura/Arbitrage-uniswap-sushiswap` - Smart contract pattern
3. `SimSimButDifferent/UniV3FlashSwapDualArbBot` - L2 flashswap
4. `flashbots/mev-inspect-py` - MEV analysis (for learning)

### Solo Searcher Advantage:
- Agility: Iterate quickly
- Innovation: Explore long-tail
- Low overhead: Focus on profitable strategies

---

**Next Step**: Phase 2 - Build live execution system with flashloans

