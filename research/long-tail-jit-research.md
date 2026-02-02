# Long-Tail MEV & JIT Liquidity Attacks - Research Findings

## Research Date: February 2026

---

## 1. Long-Tail MEV Discovery Tool: mevlog-rs

### Overview:
**Project**: mevlog-rs (Rust CLI tool)
**Purpose**: Discover long-tail MEV strategies using configurable EVM tracing
**Author**: Pawel Urbanek
**Source**: https://github.com/pawurb/mevlog-rs

### Key Features:
- Query blockchain data with configurable EVM tracing
- Filter transactions by position, method signatures, events
- Detect storage changes and contract interactions
- ENS domain integration for address resolution
- Real-time monitoring mode (`mevlog watch`)
- SQLite-based signature database (800k events, 2M method signatures)

### Query Examples for Long-Tail MEV:

```bash
# Find MEV bot transactions in top positions (unknown methods = likely MEV bots)
mevlog search -b 10:latest --method "" -p 0

# Find transactions transferring specific tokens in top 10 slots
mevlog search -b 50:latest -p 0:10 --event "Transfer(address,address,uint256)|0x6982508145454ce325ddbe47a25d4ec3d2311933"

# Find top position transactions without Swap events (non-standard MEV)
mevlog search -b 22034300:22034320 -p 0 --not-event "/(Swap).+/"

# Search for events containing 'rebase' and 'Transfer' keywords
mevlog search -b 22045400:22034320 --event "/(?i)(rebase).+/" --event "/(Transfer).+/"

# Find transactions touching specific contracts (state tracing)
mevlog search -b 10:latest --touching 0xba12222222228d8ba445958a75a0704d566bf2c8 --trace rpc

# High-cost transactions with validator bribes
mevlog search -b 5:latest -p 0 --real-tx-cost ge20000000000000000 --trace revm
```

### Technical Architecture:

#### Revm vs RPC Tracing:
- **RPC Tracing**: Uses `debug_traceTransaction` RPC method
  - Recursively extracts subtraces using `{tracer: 'callTracer'}`
  - Works with nodes that expose debug APIs
  - Slower, dependent on node capabilities

- **Revm Tracing**: Uses local Anvil process + Revm SharedBackend
  - Forks off block N-1, executes transactions locally
  - Requires executing all preceding transactions in block
  - Much faster: ~0.10s for 10th slot transaction
  - Works with public RPC endpoints

#### Performance Benchmarks:
```
mevlog tx --trace revm: 9.462s total
cast run (Foundry): Significantly slower for same transaction
```

#### ENS Optimization:
- Reduced from 400ms to 100ms per lookup
- Uses custom contract for batch calls
- Background thread with file caching (cacache-rs)

### Key Insights for Solo Searchers:
1. **Long-tail MEV requires deep transaction analysis** - not just obvious arbitrage
2. **Pattern recognition is critical** - unknown methods in top positions often indicate new MEV strategies
3. **Local simulation enables profitable strategies** - Revm allows testing before execution
4. **Public RPC endpoints are sufficient** for basic queries without state tracing
5. **State tracing (Revm/RPC)** is needed for advanced filtering but requires more resources

---

## 2. JIT (Just-In-Time) Liquidity Attacks

### Research Paper: "Demystifying Just-in-Time (JIT) Liquidity Attacks on Uniswap V3"
**Authors**: Xiong, Wang, Knottenbelt, Huth (Imperial College London)
**Source**: https://eprint.iacr.org/2023/973
**Date**: 2023

### Attack Definition:
JIT liquidity attacks exploit Uniswap V3's concentrated liquidity design:
1. Adversary monitors mempool via spy node
2. Observes sizable pending swap transaction
3. Simulates attack locally with optimal parameters
4. If profitable, mints liquidity position immediately before swap
5. Burns position immediately after swap
6. Captures trading fees from the large swap

### Empirical Findings (20 months of data):

#### Attack Statistics:
- **Total Attacks Identified**: 36,671
- **Total Profit Generated**: 7,498 ETH (~$12-20M depending on ETH price)
- **Average ROI**: 0.007% (very low profitability)
- **Entry Barrier**: 269x more liquidity required than swap volume on average

#### Market Structure:
- **Whale's Game**: Overwhelmingly controlled by few bots
- **Top Bot**: 0xa57...6CF controls 92% of total profit
- **Second Place**: Much smaller share, indicating extreme concentration

#### Strategy Analysis of Top Bot (0xa57...6CF):
- Uses entire token balance for each attack
- **Suboptimal Execution**: 27% of attacks were non-optimal
- **Missed Profit**: Failed to capture at least 7,766 ETH (~$16.1M)
- **Consistency**: Same strategy repeated across thousands of attacks

#### Impact on Market Participants:

**Existing Liquidity Providers (LPs):**
- Average dilution of liquidity shares: 85%
- Significant loss of fee revenue
- Harmful to passive LPs

**Liquidity Takers (Swappers):**
- Average improvement in execution price: 0.139%
- Benefit from JIT liquidity (lower slippage)
- Paradox: JIT attacks benefit traders while harming LPs

### Comparison with Sandwich Attacks:
- **JIT Entry Barrier**: 269x swap volume
- **Sandwich Entry Barrier**: Only 6x swap volume
- **Sandwich ROI**: Much higher (not specified, but "superior profitability")
- **Conclusion**: Sandwich attacks are more accessible to smaller searchers

### JIT Attack Optimization Parameters:
- **Price Range [pl, pu]**: Must cover swap execution price
- **Liquidity Amount LA**: Must be sufficient to capture meaningful fees
- **Timing**: Must be positioned immediately before and after victim swap
- **Capital Efficiency**: 269:1 capital-to-swap ratio required

### Practical Implications for Solo Searchers:

#### Why You Should AVOID JIT Attacks:
1. **Prohibitive Capital Requirement**: Need 269x the swap volume
   - Example: $100,000 swap requires $26,900,000 in liquidity
   - Solo searcher with $200-300 gas budget cannot compete

2. **Extreme Competition**: Single bot controls 92% of profits
   - Institutional-level capital and optimization
   - No room for smaller players

3. **Poor ROI**: 0.007% average return
   - Not worth the risk and capital
   - Better opportunities exist

4. **Complex Simulation Requirements**:
   - Need accurate price prediction
   - Must optimize [pl, pu] ranges
   - Timing must be perfect

#### What You CAN Learn from JIT:
1. **Concentrated liquidity awareness**: Understanding CLMM mechanics
2. **Local simulation methodology**: Test before execution
3. **Timing importance**: Positioning is everything
4. **Fee capture mechanics**: How LP fees work in Uniswap v3

---

## 3. Cross-Chain MEV Opportunities

### Sources Reviewed:
- https://www.flashbots.net/cross-chain-arbitrage
- https://cow.fi/learn/understanding-cross-chain-mev
- https://blog.sigmaprime.io/mev-cross-chain-bridge-exploits.html

### Cross-Chain Arbitrage Strategies:

#### Strategy 1: Inventory-Based Arbitrage
- **Requirement**: Hold assets on multiple chains
- **Mechanism**: Exploit price differences across chains
- **Example**: ETH on Arbitrum, MATIC on Polygon, USDC on Optimism
- **Advantage**: Faster execution, no bridge latency
- **Disadvantage**: High capital requirement, inventory management

#### Strategy 2: Bridge-Based Arbitrage
- **Requirement**: Move assets through bridges
- **Mechanism**: Buy on Chain A → Bridge → Sell on Chain B → Bridge back
- **Example**: USDT on Polygon → wETH bridge → Ethereum → Sell → Bridge back
- **Advantage**: Lower capital requirement
- **Disadvantage**: Bridge latency (minutes to hours), bridge fees

### Cross-Chain MEV Vulnerabilities:

#### Bridge Timing Attacks:
1. Front-run bridge transactions on source chain
2. Execute arbitrage on destination chain
3. Requires coordination across multiple chains

#### Oracle Manipulation:
- Exploit oracle discrepancies across chains
- Timing differences in price updates
- Example: Chainlink oracle vs Uniswap TWAP

#### Cross-Domain Sandwich Attacks:
1. Front-run bridge transaction on source chain
2. Back-run on destination chain
3. Complex multi-chain coordination

### Practical Considerations for Solo Searchers:

#### Feasibility Assessment:
- **Capital Requirement**: Medium to High (need inventory or bridge fees)
- **Complexity**: High (multi-chain coordination, different RPCs)
- **Latency**: Challenging (bridge delays, different block times)
- **Competition**: Moderate (fewer players than L1 MEV)

#### Recommended Approach:
- **Start with L2 arbitrage** (Arbitrum, Optimism, Base)
- **Use native bridges** for lower fees
- **Focus on high-volume DEXs** (Uniswap, Curve, 1inch)
- **Monitor cross-chain price feeds** (CoinGecko, CoinMarketCap)

### Cross-Chain Monitoring Tools:
- Multi-chain RPC providers (Alchemy, Infura)
- Cross-chain price oracles
- Bridge monitoring services
- L2 explorers (Arbiscan, Optimistic Etherscan)

---

## 4. Long-Tail MEV Categories for Solo Searchers

### Definition from EY Report:
"Long-tail MEV (LTMEV) activities denote uncommon or infrequent types of MEV not mentioned above. Long-tail MEV can be the most profitable."

### Identified Long-Tail Opportunities:

#### Category 1: NFT Marketplace Arbitrage
- **Pattern**: Floor price discrepancies across marketplaces
- **Example**: Buy on OpenSea, sell on Blur
- **Capital**: Low ($10-1,000 per NFT)
- **Competition**: Moderate
- **Feasibility**: HIGH for solo searchers

#### Category 2: Governance Event MEV
- **Pattern**: Profit from governance voting outcomes
- **Example**: Buy tokens before positive governance result, sell after
- **Capital**: Medium ($1,000-10,000)
- **Competition**: Low (few monitor governance)
- **Feasibility**: MEDIUM (requires research)

#### Category 3: Oracle Manipulation Exploitation
- **Pattern**: Profit from temporary oracle price discrepancies
- **Example**: DEX price diverges from oracle temporarily
- **Capital**: Medium ($1,000-50,000)
- **Competition**: Medium
- **Feasibility**: MEDIUM (requires monitoring)

#### Category 4: Liquidation Cascades
- **Pattern**: Sequential liquidations create arbitrage opportunities
- **Example**: Liquidation on Aave triggers liquidation on Compound
- **Capital**: Variable (depends on protocol)
- **Competition**: Medium-High
- **Feasibility**: MEDIUM (requires flashloans)

#### Category 5: New Protocol Launches
- **Pattern**: First-mover advantage on new DEX/AMM
- **Example**: New Curve pool with incentivized liquidity
- **Capital**: Variable
- **Competition**: Low (early)
- **Feasibility**: HIGH (requires monitoring)

#### Category 6: Yield Farming Transitions
- **Pattern**: Profit from strategy rotations
- **Example**: Farmers move from Pool A to Pool B
- **Capital**: Variable
- **Competition**: Medium
- **Feasibility**: MEDIUM

#### Category 7: Complex DeFi Interactions
- **Pattern**: Multi-protocol MEV not captured by simple arbitrage
- **Example**: Aave → Uniswap → Curve interaction
- **Capital**: Medium
- **Competition**: Low (complex)
- **Feasibility**: MEDIUM (requires deep knowledge)

### Detection Strategies for Long-Tail MEV:

#### mevlog-rs Queries:
```bash
# Monitor for unknown methods (new strategies)
mevlog watch -p 0:5 --method ""

# Monitor for Transfer events (NFTs, tokens)
mevlog watch --event "Transfer(address,address,uint256)"

# Monitor for Approval events (DeFi interactions)
mevlog watch --event "Approval(address,address,uint256)"

# Monitor for Swap events across multiple DEXs
mevlog watch --event "/(Swap|SwapExact|Trade).+/"

# Monitor touching specific protocols
mevlog watch --touching 0x7d2768dE32b0b80b7a3454c06BdAc94A69DDc7A9 # Uniswap v3
```

#### Real-Time Monitoring Setup:
- Filter by transaction position (0-5 for top positions)
- Filter by gas price (high gas = likely MEV)
- Filter by method signatures (unknown = potential new strategy)
- Filter by token transfers (identify profitable opportunities)
- Track specific contracts (new protocols, governance)

---

## 5. Recommendations for Solo Searchers

### Resource Assessment:
- **Gas Budget**: $200-300 ETH (limited, must be strategic)
- **Infrastructure**: AWS instance + full node + mempool (good foundation)
- **Competition**: Institutional bots with millions in capital
- **Advantage**: Agility, innovation, long-tail focus

### STRATEGIC PRIORITY (Ordered by Viability):

#### Priority 1: Simple Arbitrage (DEX-to-DEX)
- **Capital**: $500-2,000 per opportunity
- **Feasibility**: HIGH
- **Competition**: HIGH
- **Why**: Easiest to understand, predictable profit
- **Tools**: mevlog-rs + custom bot

#### Priority 2: Backrunning Large Transactions
- **Capital**: $100-1,000 per opportunity
- **Feasibility**: HIGH
- **Competition**: MEDIUM
- **Why**: Lower gas cost than frontrunning, good opportunities
- **Tools**: mempool monitoring + fast execution

#### Priority 3: Sandwich Attacks (Small to Medium Value)
- **Capital**: $200-5,000 per opportunity
- **Feasibility**: MEDIUM
- **Competition**: MEDIUM-HIGH
- **Why**: Profitable but competitive
- **Tools**: Flashbots bundles + simulation

#### Priority 4: Long-Tail / Niche Opportunities
- **Capital**: Variable ($50-10,000)
- **Feasibility**: MEDIUM-HIGH
- **Competition**: LOW
- **Why**: Less competition, hidden opportunities
- **Tools**: mevlog-rs + deep analysis

#### Priority 5: NFT Arbitrage
- **Capital**: Low ($10-1,000)
- **Feasibility**: HIGH
- **Competition**: MEDIUM
- **Why**: Less capital intensive, good for solo searchers

#### Priority 6: L2 Arbitrage
- **Capital**: $500-2,000 per opportunity
- **Feasibility**: MEDIUM
- **Competition**: MEDIUM
- **Why**: Lower gas costs, less sophisticated bots

#### Priority 7: Cross-Chain Arbitrage
- **Capital**: High (inventory) or Medium (bridge fees)
- **Feasibility**: MEDIUM
- **Competition**: MEDIUM
- **Why**: More complex, bridge latency challenges

### STRATEGIES TO AVOID (Why):

#### ❌ JIT Liquidity Attacks:
- **Reason**: 269x capital requirement, 0.007% ROI, whale-dominated
- **Verdict**: Impossible for solo searcher

#### ❌ Large-Scale Sandwich Attacks on Mainnet:
- **Reason**: jaredfromsubway and others dominate
- **Verdict**: Too competitive with limited capital

#### ❌ Cross-Chain Bridge Arbitrage (Bridge-Based):
- **Reason**: Bridge latency (minutes), high fees
- **Verdict**: Execution too slow for MEV

#### ❌ Complex Flashloan-Based Strategies (Initially):
- **Reason**: High complexity, risk, and gas costs
- **Verdict**: Start simpler, add flashloans later (Phase 2)

### OPTIMAL STARTING STRATEGY:

**Week 1-2: Foundation**
1. Set up mevlog-rs for monitoring
2. Study mempool patterns
3. Identify top DEXs for arbitrage
4. Test simulation on historical blocks

**Week 3-4: Simple Arbitrage**
1. Build simple 2-DEX arbitrage bot
2. Focus on popular pairs (USDC/ETH, WETH/USDT)
3. Test on testnets first
4. Deploy with conservative limits

**Week 5-6: Backrunning**
1. Monitor large swaps in mempool
2. Calculate profit potential
3. Execute backrun transactions
4. Optimize gas pricing

**Week 7-8: Long-Tail Discovery**
1. Use mevlog-rs to find unknown patterns
2. Analyze profitable transactions
3. Replicate successful strategies
4. Build custom detection logic

**Week 9-10: Niche Opportunities**
1. Monitor NFT marketplaces
2. Track governance events
3. Identify new protocol launches
4. Build specialized detectors

### INFRASTRUCTURE RECOMMENDATIONS:

#### AWS Instance Requirements:
- **Type**: c5.large or m5.large (2 vCPU, 4GB RAM minimum)
- **Storage**: 2TB SSD for full node
- **Network**: High bandwidth for RPC
- **Location**: US-East (closest to major Ethereum nodes)

#### Software Stack:
- **Node**: Geth or Erigon (with mempool access)
- **Monitoring**: mevlog-rs + custom scripts
- **Simulation**: Foundry (cast) or Hardhat
- **Execution**: ethers.js or web3.js
- **Bundles**: Flashbots SDK for MEV-Boost

#### Gas Management:
- **Track gas prices carefully**
- **Use gasprice APIs (ETH Gas Station, Blocknative)**
- **Set maximum gas limits per trade**
- **Use Flashbots for failed-bid protection**

---

## Summary:

### Key Takeaways:
1. **Long-tail MEV is where solo searchers can win** - less competition, hidden opportunities
2. **JIT attacks are not viable** - 269x capital, 0.007% ROI, whale-dominated
3. **mevlog-rs is essential** - best tool for discovering long-tail MEV
4. **Start simple, scale up** - arbitrage → backrunning → long-tail → complex
5. **Focus on areas with lower competition** - NFTs, governance, new protocols
6. **Infrastructure is competitive advantage** - full node + mempool + fast simulation

### Next Steps:
1. Install and configure mevlog-rs
2. Set up monitoring for unknown method signatures
3. Build simple arbitrage detector
4. Test on testnets before mainnet deployment
5. Optimize for gas efficiency
6. Scale profitable strategies, abandon unprofitable ones

