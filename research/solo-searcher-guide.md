# Solo MEV Searcher Guide - Strategy & Implementation

## Your Resources:
- **Gas Budget**: $200-300 in ETH fees
- **Infrastructure**: AWS instance + full Ethereum node + mempool access
- **Goal**: Find and execute MEV opportunities profitably
- **Focus**: Long-tail MEV, backrunning, sandwich attacks

---

## CRITICAL REALITY CHECK: What to AVOID

### ❌ Strategies That Will FAIL (Why):

#### 1. JIT Liquidity Attacks
**Why**: Requires 269x the swap volume in capital
- Example: $100 swap needs $26,900 liquidity
- Average ROI: Only 0.007%
- Single bot controls 92% of all profits
- You cannot compete with institutional capital

**Verdict**: IMPOSSIBLE for solo searcher

#### 2. Large-Scale Sandwich Attacks on Mainnet
**Why**: jaredfromsubway.eth and similar bots dominate
- They have millions in capital
- They have dedicated infrastructure
- They have faster execution
- They submit via Flashbots with optimal gas pricing

**Verdict**: TOO COMPETITIVE - only target small-medium value swaps

#### 3. Cross-Chain Bridge Arbitrage
**Why**: Bridge latency is too slow (minutes to hours)
- MEV is time-sensitive (seconds)
- Bridge fees eat profits
- Cannot compete with dedicated cross-chain arbitrageurs

**Verdict**: TOO SLOW - focus on single-chain opportunities

#### 4. Complex Flashloan Strategies (Initially)
**Why**: High complexity, risk, and gas costs
- Need to understand multiple DeFi protocols
- Gas costs can exceed profit
- Simulation is complex

**Verdict**: START SIMPLER - add flashloans in Phase 2

---

## ✅ RECOMMENDED STRATEGIES (Priority Order)

### Priority 1: Simple DEX-to-DEX Arbitrage
**Feasibility**: HIGH
**Capital**: $500-2,000 per opportunity
**Competition**: HIGH
**Expected ROI**: 5-15% of profit (80-95% goes to miner)

**Why Start Here**:
- Easiest to understand and implement
- Predictable profit calculation
- Many opportunities on popular pairs
- Good learning foundation

**Implementation Steps**:

1. **Choose Your Pairs**:
   ```
   WETH/USDC (highest volume)
   WETH/USDT
   WETH/WBTC
   USDC/USDT
   ```

2. **Monitor Multiple DEXs**:
   ```
   - Uniswap V2 (all pairs)
   - Uniswap V3 (popular tick ranges)
   - SushiSwap
   - Curve (stablecoin pairs)
   - 1inch (aggregated routes)
   ```

3. **Detect Opportunities**:
   ```javascript
   // Pseudocode
   for each pair (tokenA, tokenB):
     for each dex:
       price1 = getPrice(dex1, tokenA, tokenB)
       price2 = getPrice(dex2, tokenB, tokenA)

       if abs(price1 - price2) > threshold:
         profit = calculateProfit(amount, price1, price2)
         gasCost = estimateGasCost()

         if profit > gasCost + minProfit:
           submitArbitrage()
   ```

4. **Profit Calculation**:
   ```
   grossProfit = (amountOutDex2 - amountInDex1) - slippage
   gasCost = gasLimit * gasPrice
   netProfit = grossProfit - gasCost
   minerReward = netProfit * 0.80  // 80% to miner (Flashbots)
   yourProfit = netProfit * 0.20  // 20% for you
   ```

**Gas Management**:
- Use Flashbots for failed-bid protection
- Set minimum profit threshold (e.g., $0.50 net)
- Use gas price APIs to time submissions
- Start with conservative amounts ($10-50)

**GitHub Reference**:
- `flashbots/simple-arbitrage` - Official example
- `codeesura/Arbitrage-uniswap-sushiswap` - Smart contract arbitrage
- `6eer/uniswap-sushiswap-arbitrage-bot` - Example implementation

---

### Priority 2: Backrunning Large Transactions
**Feasibility**: HIGH
**Capital**: $100-1,000 per opportunity
**Competition**: MEDIUM
**Expected ROI**: 10-25% (less competitive than frontrunning)

**Why This is Good for Solo Searchers**:
- Lower gas cost than frontrunning
- Good opportunities exist (large swaps, liquidations)
- Can use MEV-Share for better execution
- Less front-running competition

**Backrunning Mechanics**:

1. **Monitor Mempool**:
   ```javascript
   // Watch for large swaps
   mempool.on('pendingTransaction', (tx) => {
     if (isLargeSwap(tx)) {
       analyzeBackrunOpportunity(tx)
     }
   })
   ```

2. **Identify Targets**:
   - Large DEX swaps (> $1,000 value)
   - Liquidations (Aave, Compound, MakerDAO)
   - Protocol migrations
   - Token launches/new listings

3. **Calculate Profit Potential**:
   ```
   // After victim executes:
   priceImpact = calculatePriceImpact(victimTx)
   expectedPriceChange = priceImpact * decayFactor

   // Your backrun:
   backrunProfit = executeTrade(victimAmount * factor)
   gasCost = estimateGasCost()

   if backrunProfit > gasCost * 2:
     submitBackrun()
   ```

4. **Gas Pricing Strategy**:
   ```
   // Lower than victim to ensure ordering AFTER
   gasPrice = victimGasPrice * 0.90

   // Or use Flashbots with explicit ordering
   bundle = [victimTx, backrunTx]
   ```

**MEV-Share Integration**:
```javascript
// Submit backrun to MEV-Share
const bundle = [
  {
    transaction: backrunTx,
    canRevert: true  // Only pay if successful
  }
]

const result = await mevShare.sendPrivateTransaction(bundle)
// You get 90% of MEV, searcher (you) gets 10%
```

**GitHub Reference**:
- `flashbots/mev-boost-relay` - Understanding bundle submission
- MEV-Share documentation for backrun mechanics

---

### Priority 3: Small-Medium Value Sandwich Attacks
**Feasibility**: MEDIUM
**Capital**: $200-5,000 per opportunity
**Competition**: MEDIUM-HIGH
**Expected ROI**: 20-40% (better than arbitrage, more competitive)

**Why Target Small-Medium**:
- Large sandwiches dominated by institutional bots
- Small sandwiches ($100-1,000 victim trades) have less competition
- Profitable with lower capital
- Good learning opportunity

**Sandwich Attack Mechanics**:

1. **Detect Large Swaps**:
   ```javascript
   // Monitor mempool
   mempool.on('pendingTransaction', (tx) => {
     decoded = decodeTx(tx)

     if (decoded.method === 'swap' && decoded.value > threshold) {
       // Calculate optimal sandwich
       victimAmount = decoded.amountIn
       liquidity = getPoolLiquidity(decoded.pool)

       optimalFrontrun = calculateOptimalFrontrun(
         victimAmount,
         liquidity,
         slippage
       )

       if (optimalFrontrun.profit > minProfit) {
         submitSandwich(optimalFrontrun)
       }
     }
   })
   ```

2. **Optimal Sizing (CPMM - Uniswap V2)**:
   ```
   // From research: ΔΠ(Vf;Vv) ≈ (1-φ)²/L(Vf·Vv - Vf²) - 2φVf

   Where:
   Vf = frontrun input size (your variable)
   Vv = victim input size
   φ = swap fee (0.003 for Uniswap)
   L = pool liquidity depth

   // Optimize Vf to maximize profit
   d(ΔΠ)/d(Vf) = 0
   // Solve for optimal frontrun size
   ```

3. **Execute via Flashbots**:
   ```javascript
   const bundle = [
     frontrunTx,    // Your buy before victim
     victimTx,       // Original victim transaction
     backrunTx,      // Your sell after victim
     minerBribeTx    // Send profit to miner
   ]

   await flashbots.sendBundle(bundle, targetBlock)
   ```

4. **Slippage Management**:
   ```
   // Victim's slippage constraint
   victimSlippage = decoded.amountOutMin / decoded.amountOut

   // Your frontrun should not violate victim's slippage
   // Otherwise victim transaction fails, sandwich fails
   ```

**Key Insight from Research**:
- Sandwich attacks require 6x victim swap volume (vs 269x for JIT)
- Still competitive, but viable for solo searchers on small-medium trades
- Practice on testnets first!

**GitHub Reference**:
- `marksantiago290/Ethereum-MEV-BOT` - Sandwich bot implementation
- Research papers on sandwich detection (for understanding bot behavior)

---

### Priority 4: Long-Tail MEV Discovery
**Feasibility**: MEDIUM-HIGH
**Capital**: Variable ($50-10,000)
**Competition**: LOW (by definition)
**Expected ROI**: Unknown (that's the point!)

**Why Focus on Long-Tail**:
- Less competition (bots focus on obvious arbitrage)
- Higher profit margins when discovered
- You can be first to exploit
- Fits "innovative solo searcher" profile

**Long-Tail Categories**:

#### A. NFT Marketplace Arbitrage
```bash
# Use mevlog-rs to monitor
mevlog watch --event "Transfer(address,address,uint256)" -p 0:5

# Watch for:
- Floor price discrepancies (OpenSea vs Blur vs X2Y2)
- New drops with price gaps
- Cross-market arbitrage
```

**Implementation**:
```javascript
// Monitor floor prices
const openSeaFloor = await getFloorPrice('OpenSea', collection)
const blurFloor = await getFloorPrice('Blur', collection)

if (abs(openSeaFloor - blurFloor) > gapThreshold) {
  // Buy low, sell high
  profit = executeArbitrage()
}
```

#### B. Governance Event MEV
```javascript
// Monitor governance proposals
governance.on('ProposalCreated', (proposal) => {
  // Analyze proposal impact
  if (proposal.impact === 'positive') {
    // Buy token before vote, sell after
    executeStrategy()
  }
})
```

#### C. Oracle Manipulation Exploitation
```javascript
// Monitor oracle prices vs DEX prices
const chainlinkPrice = await getChainlinkPrice('ETH/USD')
const uniswapPrice = await getUniswapPrice('ETH/USDC')

if (abs(chainlinkPrice - uniswapPrice) > threshold) {
  // Exploit temporary discrepancy
  executeArbitrage()
}
```

#### D. Protocol Launch Arbitrage
```javascript
// Monitor new protocol deployments
const newPools = await monitorNewDeployments()

for (pool of newPools) {
  // Check for early arbitrage opportunities
  if (isNewPoolProfitable(pool)) {
    executeFirstMoverAdvantage()
  }
}
```

**Detection Tool: mevlog-rs**
```bash
# Install
cargo install mevlog

# Monitor for unknown methods (new strategies)
mevlog watch -p 0:5 --method ""

# Monitor specific tokens
mevlog watch --event "Transfer(address,address,uint256)|<token_address>"

# Monitor for new pools
mevlog watch --event "PoolCreated"

# Monitor for large transactions
mevlog watch --real-tx-cost ge10000000000000000
```

**Long-Tail Research Strategy**:
1. Use mevlog-rs to find patterns
2. Identify profitable unknown transactions
3. Reverse engineer the strategy
4. Replicate and optimize
5. Scale if profitable

---

### Priority 5: L2 Arbitrage
**Feasibility**: MEDIUM
**Capital**: $500-2,000 per opportunity
**Competition**: MEDIUM
**Expected ROI**: 10-30% (lower gas costs)

**Why L2 is Good**:
- Lower gas costs (100-1000x cheaper than L1)
- Less sophisticated bots than mainnet
- Growing ecosystem (Arbitrum, Optimism, Base)
- Bridge your capital once

**Target L2s**:
- **Arbitrum**: High TVL, many DEXs
- **Optimism**: Growing ecosystem
- **Base**: New, less competition
- **Polygon**: Very low gas costs

**Implementation**:
```javascript
// Arbitrum example
const arbitrumRPC = 'https://arb1.arbitrum.io/rpc'

const price1 = await getUniswapPrice('WETH/USDC', 'arbitrum')
const price2 = await getSushiPrice('WETH/USDC', 'arbitrum')

if (arbitrageExists(price1, price2)) {
  await executeArbitrage(arbitrumRPC)
}
```

**GitHub Reference**:
- `SimSimButDifferent/UniV3FlashSwapDualArbBot` - L2 (Arbitrum) Uniswap V3 flashswap bot
- Deployed on Arbitrum: 0xf812197dbdbcd0f80cd003c20f695dc8d06bc3b0

---

## IMPLEMENTATION ROADMAP (10 Weeks)

### Week 1-2: Foundation & Infrastructure
- [ ] Set up AWS instance (c5.large or better)
- [ ] Sync full Ethereum node (Geth or Erigon)
- [ ] Install and configure mevlog-rs
- [ ] Set up mempool monitoring
- [ ] Test with historical blocks
- [ ] Study successful MEV transactions

**Tasks**:
```bash
# Install mevlog-rs
cargo install mevlog

# Test monitoring
mevlog search -b 100:latest -p 0:5

# Study past sandwiches
mevlog tx -B 2 <sandwich_tx_hash>
```

### Week 3-4: Simple Arbitrage Bot
- [ ] Choose target pairs (WETH/USDC, WETH/USDT)
- [ ] Set up price monitoring (Uniswap, Sushi, Curve)
- [ ] Implement opportunity detection
- [ ] Build simple arbitrage contract
- [ ] Test on Goerli testnet
- [ ] Deploy to mainnet with conservative limits

**Reference**: `flashbots/simple-arbitrage`

**Smart Contract Template**:
```solidity
// SPDX-License-Identifier: MIT
pragma solidity ^0.8.0;

import "@openzeppelin/contracts/token/ERC20/IERC20.sol";

contract SimpleArbitrage {
    address public owner;

    constructor() {
        owner = msg.sender;
    }

    function executeArbitrage(
        address tokenA,
        address tokenB,
        uint256 amountIn,
        address[] calldata dexes,
        bytes[] calldata swapData
    ) external {
        require(msg.sender == owner, "Only owner");
        require(dexes.length == swapData.length);

        // Execute swaps
        for (uint i = 0; i < dexes.length; i++) {
            IERC20(tokenA).approve(dexes[i], amountIn);
            (bool success, bytes memory data) = dexes[i].call(swapData[i]);
            require(success, "Swap failed");
            amountIn = abi.decode(data, (uint256));
        }

        // Send profit to owner
        IERC20(tokenB).transfer(owner, amountIn);
    }
}
```

### Week 5-6: Backrunning Bot
- [ ] Set up mempool listener
- [ ] Identify backrun targets (large swaps, liquidations)
- [ ] Calculate profit potential
- [ ] Implement backrun execution
- [ ] Test with historical data
- [ ] Deploy small amounts first

**Key Implementation**:
```javascript
// Backrun large swaps
mempool.on('pendingTransaction', async (tx) => {
    const decoded = await decodeTransaction(tx)

    if (isLargeSwap(decoded)) {
        // Simulate price impact
        const impact = await simulatePriceImpact(tx)

        // Calculate backrun opportunity
        const backrunProfit = impact * 0.5  // Conservative estimate
        const gasCost = await estimateGasCost(backrunTx)

        if (backrunProfit > gasCost * 3) {
            await submitBackrun(tx, backrunTx)
        }
    }
})
```

### Week 7-8: Sandwich Attacks (Small-Medium)
- [ ] Study sandwich research papers
- [ ] Implement CPMM profit calculation
- [ ] Detect sandwich opportunities
- [ ] Optimize gas pricing
- [ ] Test on testnets extensively
- [ ] Deploy with conservative amounts

**Profit Calculation** (from research):
```javascript
// Optimal frontrun size for CPMM
function calculateOptimalFrontrun(victimAmount, liquidity, fee) {
    // ΔΠ(Vf;Vv) ≈ (1-φ)²/L(Vf·Vv - Vf²) - 2φVf
    // Maximize by taking derivative and setting to 0

    const optimalVf = (victimAmount / 2) * ((1 - fee) / 2)

    return {
        frontrunAmount: optimalVf,
        expectedProfit: calculateProfit(optimalVf, victimAmount, liquidity, fee)
    }
}
```

### Week 9-10: Long-Tail Discovery
- [ ] Use mevlog-rs to find patterns
- [ ] Analyze unknown method signatures
- [ ] Identify profitable strategies
- [ ] Replicate and test
- [ ] Scale profitable opportunities
- [ ] Abandon unprofitable ones

**Discovery Process**:
```bash
# Find unknown methods in top positions (potential new MEV)
mevlog watch -p 0:3 --method "" > unknown_methods.log

# Analyze each unknown method
for tx in unknown_methods.log:
    analyze(tx)
    reverseEngineer(tx)
    replicate(tx)

# Scale if profitable
if profit > threshold:
    addToProductionBot()
```

---

## FLASHBOTS INTEGRATION

### Why Use Flashbots:
1. **Failed-bid protection** - Don't pay for failed transactions
2. **Private mempool** - Hide your strategies
3. **Better execution** - Guaranteed atomic ordering
4. **Reduced competition** - Front-running protection

### Bundle Submission:
```javascript
import { FlashbotsBundleProvider } from "@flashbots/ethers-provider-bundle";

const flashbotsProvider = new FlashbotsBundleProvider(
    provider,
    authSigner,  // Private key for Flashbots auth
    "https://relay.flashbots.net"  // Flashbots relay
);

const signedTransactions = [
    frontrunTx,
    victimTx,
    backrunTx,
    bribeTx  // Send ETH to block.coinbase
];

const bundleSubmission = await flashbotsProvider.sendBundle(
    signedTransactions,
    targetBlockNumber
);

// Check if included
const bundleReceipt = await bundleReceipt.wait();
if (bundleReceipt === 0x0000000000000000000000000000000000000000000000000000000000000000000) {
    // Not included, try next block
} else {
    // Success!
}
```

### MEV-Share Integration:
```javascript
// Submit transaction to MEV-Share
const result = await mevShare.sendPrivateTransaction({
    transaction: yourTx,
    preferences: {
        privacy: {
            hints: [],  // Share minimal information
        },
        maxBlockNumber: targetBlock + 3
    }
})

// You get 90% of MEV back
// Searchers compete to include your transaction
```

---

## GAS OPTIMIZATION STRATEGIES

### 1. Time Your Submissions
```javascript
// Check gas prices
const gasPrice = await provider.getGasPrice()
const baseFee = await provider.getBaseFeePerGas()

// Submit when gas is low
if (gasPrice < threshold) {
    await submitBundle()
}
```

### 2. Optimize Smart Contracts
```solidity
// Use calldata instead of memory
function execute(bytes calldata data) external {
    // More gas efficient
}

// Use uint256 instead of smaller types
uint256 amount;  // Better than uint128

// Batch operations
for (uint i = 0; i < 10; i++) {
    // Batch into one transaction instead of 10
}
```

### 3. Use Flashbots for Complex Operations
```javascript
// Flashbots bundles share gas overhead
const bundle = [
    tx1,  // Frontrun
    tx2,  // Victim
    tx3   // Backrun
]

// More efficient than 3 separate transactions
```

---

## RISK MANAGEMENT

### 1. Start Small
```javascript
const MAX_TRADE_SIZE = 0.01  // $20 worth
const MAX_GAS_COST = 0.002    // $4 max gas
const MIN_PROFIT_MARGIN = 0.005 // $1 min profit
```

### 2. Test Extensively
```javascript
// Always test on forked mainnet
const fork = await provider.send("hardhat_reset", [{
    forking: {
        jsonRpcUrl: mainnetRPC,
        blockNumber: latestBlock
    }
}])

// Test your strategy
const result = await testStrategy(fork)
if (result.profit > 0) {
    // Deploy to mainnet
}
```

### 3. Monitor and Adjust
```javascript
// Track your performance
const stats = {
    totalTrades: 0,
    profitableTrades: 0,
    totalProfit: 0,
    totalGasSpent: 0
}

// If win rate drops below 50%, pause and investigate
if (stats.profitableTrades / stats.totalTrades < 0.5) {
    pauseTrading()
    investigateStrategy()
}
```

### 4. Diversify
```javascript
// Don't put all gas budget into one strategy
strategies = [
    { name: 'arbitrage', allocation: 0.4 },  // 40% of budget
    { name: 'backrunning', allocation: 0.3 }, // 30%
    { name: 'sandwich', allocation: 0.2 },    // 20%
    { name: 'longtail', allocation: 0.1 }      // 10%
]
```

---

## MONITORING & ANALYTICS

### Real-Time Monitoring
```javascript
// Log all trades
const logTrade = (trade) => {
    console.log({
        timestamp: Date.now(),
        strategy: trade.strategy,
        profit: trade.profit,
        gasCost: trade.gasCost,
        netProfit: trade.profit - trade.gasCost,
        blockNumber: trade.blockNumber
    })
}
```

### Daily Analytics
```javascript
// Generate daily report
const generateReport = () => {
    const todayTrades = trades.filter(t =>
        t.timestamp > startOfDay
    )

    return {
        totalTrades: todayTrades.length,
        totalProfit: sum(todayTrades.profit),
        totalGas: sum(todayTrades.gasCost),
        winRate: todayTrades.filter(t => t.profit > 0).length / todayTrades.length,
        bestTrade: max(todayTrades.profit),
        worstTrade: min(todayTrades.profit)
    }
}
```

---

## NEXT STEPS (Phase 1 Complete → Phase 2)

### What We've Covered:
- ✅ Research (eigenphi.io, Flashbots, papers, GitHub)
- ✅ Strategy recommendations for solo searchers
- ✅ Implementation roadmap
- ✅ Risk management guidelines

### Phase 2 Will Cover:
- 🚀 Live MEV execution system
- 🚀 Flashloan smart contract deployment
- 🚀 Advanced arbitrage strategies
- 🚀 Production-grade monitoring

---

## KEY TAKEAWAYS

1. **Start simple**: Arbitrage → Backrunning → Sandwich → Long-tail
2. **Avoid impossible strategies**: JIT, large-scale sandwiches, cross-chain bridges
3. **Use Flashbots**: Failed-bid protection, private mempool, better execution
4. **Start small and test**: Use testnets, forked mainnet, conservative limits
5. **Focus on long-tail**: Less competition, higher margins
6. **Monitor everything**: Track performance, adjust strategy, abandon losers
7. **Diversify**: Don't put all resources into one strategy

---

## SUMMARY

**Your Best Path Forward**:

1. **Weeks 1-2**: Setup infrastructure (node, mevlog-rs, monitoring)
2. **Weeks 3-4**: Simple DEX arbitrage (low hanging fruit)
3. **Weeks 5-6**: Backrunning large transactions (medium competition)
4. **Weeks 7-8**: Small sandwich attacks (higher competition, better profit)
5. **Weeks 9-10**: Long-tail discovery (innovation advantage)

**Resources**:
- AWS instance + full node + mempool ✓ (you have this)
- $200-300 gas budget ✓ (you have this)
- GitHub repositories (use as templates)
- Research papers (understand the math)
- mevlog-rs (for long-tail discovery)

**Expected Timeline**:
- First profitable trade: Week 4-5 (arbitrage)
- Consistent profitability: Week 6-8 (backrunning + sandwich)
- Scale-up opportunities: Week 9-10 (long-tail)

**Realistic Expectations**:
- You won't beat jaredfromsubway
- You won't control 92% of JIT profits
- You WILL find profitable opportunities in long-tail
- You WILL learn and improve over time
- You CAN be a profitable solo searcher

---

## FINAL RECOMMENDATIONS

### ✅ DO:
- Start with simple arbitrage
- Use Flashbots for bundle submission
- Test everything on testnets
- Start with small amounts
- Monitor your performance
- Focus on long-tail opportunities
- Learn from research and GitHub repos

### ❌ DON'T:
- Try JIT attacks (impossible)
- Compete with large-scale sandwich bots
- Spend all gas on one strategy
- Deploy without testing
- Ignore gas costs
- Overlook long-tail opportunities
- Give up after first failures

---

**Good luck, solo searcher! The dark forest is challenging, but profitable for those who are smart, patient, and strategic.**

