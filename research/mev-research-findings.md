# MEV Research Project - Detailed Findings

**Project Goal**: Comprehensive MEV research to build a simulator for finding MEV opportunities on Ethereum blockchain (focus: long tail, backrunning, sandwich attacks)

**Date**: 2026 Research Initiative

---

## COMPREHENSIVE MEV RESEARCH FINDINGS

### 1. eigenphi.io Platform Analysis

#### Platform Features Discovered:
- **Real-time MEV Live-stream**: Shows recent MEV transactions with contract addresses, profit, cost
- **MEV Types Tracked**: Arbitrage, Sandwich, Liquidation, Flashloan
- **Performance Metrics**: 24H, 7D, 30D views for MEV analysis
- **Contract Profit Leaderboard**: Top MEV-extracting contracts ranked by profitability
- **MEV Transaction Leaderboard**: Historical transaction profit records
- **Hot Tokens & Liquidity Pools**: Tracking most active MEV opportunities
- **Market Overview**: Profit vs MEV Type, Cost vs MEV Type, Revenue vs MEV Type, Volume vs MEV Type

#### Key Metrics Observed:
- Arbitrage 7D profit: $15.51M (dominant MEV type)
- Sandwich 7D profit: $18.53k (minimal compared to arbitrage)
- Liquidation 7D profit: $473.62k
- Top arbitrage contract: 0x34d...c4Ac1 with $7.96M lifetime profit
- Recent high-value arbitrage transactions: $1.99M+ individual profits

#### Platform Value for Simulator:
- Real-time detection of MEV patterns
- Profit distribution analysis (PDF charts)
- Historical transaction leaderboards for backtesting
- Token and pool popularity metrics for opportunity identification

---

### 2. Flashbots & MEV-Share Architecture

#### MEV-Share Protocol Key Features:
- **Orderflow Auction**: Users selectively share transaction data with searchers
- **MEV Redistribution**: 90% refunded to users by default
- **Privacy Controls**: Selective information sharing based on user preferences
- **Current Limitation**: MEV-Share Nodes only accept backruns (as of latest docs)
- **Credible Neutrality**: Permissionless for searchers, no single block builder enshrined

#### Flashbots Auction (MEV-Boost) Architecture:

##### Three-Party System:
1. **Searchers**: Submit bundles with inclusion preferences
   - Arbitrage and liquidation bots
   - Uniswap traders seeking frontrun protection
   - DApps requiring account abstraction/gasless transactions

2. **Block Builders**: Construct most profitable blocks from transactions
   - Specialized entities receiving bundles from searchers
   - Transmit built blocks to validators via mev-boost relay

3. **Validators**: Select among builder bids via MEV-Boost
   - Choose most profitable block
   - Reduce infrastructure requirements

##### Bundle Format (eth_sendBundle RPC):
```json
{
  "txs": "Array of signed transactions (atomic execution)",
  "blockNumber": "Target block number (hex)",
  "minTimestamp": "Valid from timestamp (optional)",
  "maxTimestamp": "Valid until timestamp (optional)",
  "revertingTxHashes": "Allowed revert hashes (optional)"
}
```

##### Flashbots Auction Timeline:
- July 2020: MEV-Ship Research Collective formation
- Nov 2020: Flashbots architecture proposal
- Jan 2021: Alpha v0.1 launch
- Multiple versions through 2022 (v0.1 to v0.6)
- Current: MEV-Boost for PoS Ethereum

##### Key Properties:
- Pre-trade privacy (transactions hidden until inclusion)
- Failed trade privacy (losing bids never public)
- Efficiency (no unnecessary network/chain congestion)
- Bundle merging capability
- Finality protection (prevents time-bandit attacks)
- Complete privacy (all intermediaries)
- Permissionless (no trusted intermediaries)

##### Auction Types Comparison:
- **Traditional**: All-pay auction, bidding wars, failed bids consume blockspace
- **Flashbots**: First-price sealed-bid, no payment for failed bids, efficient price discovery

---

### 3. Academic Research Insights

#### Paper 1: "How to Serve Your Sandwich? MEV Attacks in Private L2 Mempools" (arXiv:2601.19570, Jan 2026)

##### Key Findings:
- **Model Extension**: Formal model for optimal front/back-run sizing
- **AMM Types**: Covers both CPMM (constant-product) and CLMM (concentrated-liquidity) like Uniswap v3
- **Execution-Feasibility Model**: Quantifies co-inclusion constraints under private mempools
- **Empirical Results**:
  - >95% of flagged sandwich patterns on L2s are false positives
  - Median net profit for L2 sandwiches is NEGATIVE
  - Sandwich efficiency below 0.05 per 100 transactions
  - No evidence of sustained or economically meaningful sandwich attacks on L2s with private mempools

##### Theoretical Contributions:
- Optimal attacker strategy depends on tick boundaries and liquidity distribution
- Attackers benefit most pushing trades into thinner liquidity regions
- Flash-loans used to exploit thin liquidity
- L2 sandwiching is probabilistic (not atomic) due to private mempools

##### Execution Constraints:
- No guaranteed atomic inclusion on L2s (no builder markets)
- Attackers must rely on:
  1. **Timing**: Predict proposer cadence and batching windows
  2. **Fee Placement**: Base fee + priority tips calibrated to sequencer policy
  3. **Redundancy**: Parallelized, nonce-staggered submissions with protective slippage bounds

##### Mathematical Model (CPMM):
- Small-trade approximation for sandwich profitability
- Normalized input sizes: α_f and α_v
- Incremental profit approximation: ΔΠ(V_f;V_v) ≈ (1-φ)²/L(V_f·V_v - V_f²) - 2φV_f
- Where φ is swap fee, L is liquidity depth

##### Practical Implications:
- Private mempools and medium-value swaps currently limit L2 sandwiches
- As L2s transition to public mempools/OFA/higher-value trades, sandwiches may re-emerge
- Sequencing policies should evaluate economic risks explicitly

#### Paper 2: "Sandwiched and Silent: Behavioral Adaptation and Private Channel Exploitation in Ethereum MEV" (arXiv:2512.17602, Dec 2025)

##### Key Findings:
- **User Adaptation**:
  - 40% of victims migrate to private routing within 60 days after being sandwiched
  - 54% migrate with repeated exposures (n>1)
  - Churn peaks at 7.5% after first sandwich, drops to 1-2% (survivor bias)

- **Private Channel Exploitation**:
  - 2,932 private sandwich attacks in Nov-Dec 2024
  - 3,126 private victim transactions affected
  - $409,236 in user losses
  - $293,786 in attacker profits
  - Single bot accounts for 65% of private frontruns
  - Heavy concentration on small set of DEX pools

- **Critical Insight**: Private routing does NOT guarantee protection from MEV
  - Private channels remain exploitable
  - Narrow but highly concentrated attack surface
  - Potential compromise/collusion within private routing infrastructure
  - Continuous monitoring and protocol-level defenses needed

##### Research Methods:
- Windowed, n-indexed survival analysis for user behavior
- Transaction-level data from Nov 2024 to Feb 2025
- Enriched with mempool visibility (MempoolDumpster) and MEV labels (ZeroMEV)
- First systematic detection of private-path sandwich attacks

#### Additional Research Papers Identified:

##### Sandwich Attack Detection:
- **SandWatch**: Dual-task graph neural network for sandwich detection
- **GasTrace**: Cascade classification framework analyzing transaction features
- **Real-time detection systems** for Ethereum (Geth-based)
- **PoSGasTrace**: Framework for sandwich attack detection and mitigation

##### MEV Machine Learning:
- **Mecon**: GNN-based graph classification for MEV detection (2025)
- **Predictions in MEV-Boost auctions** using ML (IEEE 2025)
- ML-based event selection strategies
- AI-based models for predicting MEV-driven market movements

##### Cross-Chain & Fast-Finality:
- **Bunny Hops and Blockchain Stops**: Cross-chain MEV (multihop arbitrage)
- **MEV extraction on fast-finality blockchains** (spam-based strategies)
- **First-Come-First-Served blockchains**: MEV techniques don't translate directly

##### Formal Methods:
- **Certifying optimal MEV strategies with Lean** theorem prover (2025)
- First mechanized formalization of MEV
- Methodology to construct machine-checkable MEV proofs

---

### 4. MEV-Share Deep Dive

#### Differential Privacy Research (arXiv:2508.14284, Aug 2025):
- "Differentially Private aggregate hints in mev-share"
- Focuses on backrunning extraction and frontrunning prevention
- Private aggregation techniques for MEV-Share

#### Backrun Auctions (arXiv:2401.08302, Jan 2024):
- "Do backrun auctions protect traders?"
- Models transaction queueing infrastructure for MEV activity
- Evaluates protection mechanisms

#### MEV-Share Current Limitations:
- Only accepts backruns (no front-running or sandwich through MEV-Share)
- Privacy preferences configurable by users
- Searchers submit partial bundles without seeing full transaction data
- MEV-Share Node simulates bundles before forwarding to builders

---

### 5. MEV Bot Infrastructure Insights

#### Latency Optimization:
- Co-location with builders/validators reduces latency
- Specialized infrastructure requirements
- High-frequency trading approaches adapted to blockchain

#### MEV Extraction Techniques:
1. **Front-running**: Bid high gas to order before victim
2. **Back-running**: Lower gas price to order after victim
3. **Sandwich attacks**: Both front-run and back-run on victim
4. **Arbitrage**: Cross-DEX price differences
5. **Liquidations**: Undercollateralized positions
6. **Just-in-time (JIT) liquidity**: Provide liquidity right before swaps

#### MEV Bot Categories:
1. **Arbitrage Bots**: Exploit price differences across DEXs
2. **Liquidation Bots**: Monitor undercollateralized positions
3. **Sandwich Bots**: Target large DEX swaps
4. **Backrun Bots**: Follow profitable transactions
5. **CMEV (Cross-domain MEV)**: Exploit opportunities across chains

---

### 6. Emerging Trends 2026

#### Uniswap v4 MEV Implications:
- Hooks enable advanced customization
- MEV-resistant swaps possible via hooks
- On-chain limit orders via hooks
- Anti-MEV protections in hooks
- Shared routers introduce new attack vectors

#### EigenLayer & Restaking:
- $15B+ TVL in restaking
- Actively Validated Services (AVS) powered by staked ETH
- MEV opportunities in restaking ecosystem
- Cross-domain MEV with restaking

#### L2 Rollup Developments:
- Private mempools reduce sandwich attacks
- Sequencer ordering determines transaction placement
- PBS (Proposer-Builder Separation) evolving to ePBS (enshrined)
- Public mempools may reintroduce MEV opportunities

#### MEV Mitigation Techniques:
- Tight slippage settings
- Large liquidity pools
- Custom RPC endpoints
- Intent-based solutions (CoW Swap, etc.)
- Encrypted mempools with randomized permutation
- Protocol-level defenses in DEXs

---

### 7. Backrunning Strategies Detailed

#### Backrunning Definition:
- Execute transaction immediately after another high-value transaction
- Lower gas price to ensure ordering after target
- Capitalize on price movements created by victim

#### Backrunning Use Cases:
1. **Token launches**: Backrun new token listings on Uniswap
2. **Arbitrage follow-up**: Profit from price normalization after large trades
3. **Liquidation aftermath**: Profit from market impact of liquidations
4. **JIT unwinding**: Exit JIT liquidity positions

#### Backrunning Optimization:
- Gas price strategy: Lower than victim but still competitive
- Timing: Monitor mempool for high-value target transactions
- Slippage: Set protective slippage bounds
- Transaction structure: Single-purpose for execution speed

---

### 8. Long-Tail MEV Opportunities

#### Definition:
- Less obvious, less exploited MEV opportunities
- Niche DeFi protocols and edge cases
- Cross-protocol interactions
- Time-sensitive opportunities

#### Long-Tail Categories:
1. **Cross-Protocol Arbitrage**: Less obvious price differences
2. **Liquidation Cascades**: Multi-protocol liquidation events
3. **Governance Events**: Vote-based MEV opportunities
4. **New Protocol Launches**: First-mover advantages
5. **Yield Farming Transitions**: Strategy rotations
6. **NFT Marketplace Arbitrage**: Floor price arbitrage
7. **Oracle Manipulation**: Cross-chain price discrepancies

#### Detection Challenges:
- Lower frequency, harder to detect
- More complex patterns
- Require deeper protocol knowledge
- Often protocol-specific

---

### 9. Sandwich Attack Optimization

#### Mathematical Foundation (CPMM):

**Profit Function**:
```
ΔΠ(V_f; V_v) ≈ (1-φ)²/L(V_f·V_v - V_f²) - 2φV_f
```

Where:
- V_f = frontrun input size
- V_v = victim input size
- φ = swap fee (typically 0.3% for Uniswap)
- L = liquidity depth

**Optimal Frontrun Size**:
- Maximizes profit given victim trade size
- Depends on victim slippage constraint
- Balances fee cost vs profit opportunity

**CLMM (Uniswap v3) Considerations**:
- Tick boundaries affect optimal sizing
- Liquidity distribution is non-uniform
- Flash-loans enable hitting thin liquidity regions

#### Execution Strategy:

**On L1 (with Flashbots)**:
1. Monitor mempool for large swaps
2. Calculate optimal front/back-run sizes
3. Submit atomic bundle via MEV-Boost
4. Guaranteed execution (or revert all)

**On L2 (private mempool)**:
1. Monitor sequencer patterns
2. Predict batching windows
3. Submit parallel transactions with different gas prices
4. Use nonce-staggering for redundancy
5. Probabilistic execution (not guaranteed)

---

### 10. Machine Learning for MEV Detection

#### Graph Neural Networks (GNN):
- Mecon: Graph classification for MEV detection
- SandWatch: Dual-task GNN for sandwich attacks
- Model transaction relationships as graphs

#### Feature Engineering:
- Transaction features (gas price, value, timestamp)
- Address features (balance, transaction count)
- Token features (price, liquidity, volume)
- Block features (number, gas used, timestamp)

#### Classification Frameworks:
- GasTrace: Cascade classification for sandwich detection
- Multi-stage filtering to reduce false positives
- Economic consistency checks

#### MEV-Boost Prediction:
- High MEV blocks detected from first 6 seconds of bidding
- ML models predict block value from bid patterns
- Bidding data as input features

---

## SIMULATOR ARCHITECTURE RECOMMENDATIONS

Based on research findings, recommended simulator components:

### 1. Data Ingestion Layer
- Real-time mempool monitoring
- Block data ingestion from Ethereum nodes
- DEX price feeds and liquidity data
- Flashbots bundle monitoring

### 2. MEV Detection Engine
- Pattern recognition for standard MEV types
- GNN-based anomaly detection
- Cross-protocol opportunity scanning
- Long-tail opportunity heuristics

### 3. Simulation Engine
- Transaction replay and backtesting
- What-if scenario modeling
- Profitability calculation using CPMM/CLMM models
- Gas cost optimization

### 4. Strategy Testing
- Front-running simulation
- Backrunning optimization
- Sandwich attack modeling
- Arbitrage path finding

### 5. Analytics Dashboard
- Real-time MEV opportunity visualization
- Profit/Loss tracking
- Strategy performance metrics
- Alert system for high-value opportunities

---

## KEY RESEARCH QUESTIONS FOR SIMULATOR

1. **Long-Tail Detection**: How to identify non-obvious MEV opportunities?
2. **Backrunning Optimization**: What's the optimal gas price and timing strategy?
3. **Sandwich Profitability**: How to model slippage and liquidity depth accurately?
4. **Cross-Chain MEV**: How to detect and execute cross-domain opportunities?
5. **MEV-Share Integration**: How to leverage MEV-Share for better execution?
6. **L2 vs L1**: How strategies differ between Ethereum L1 and rollups?
7. **ML Enhancement**: What ML models improve MEV detection accuracy?
8. **Real-Time Execution**: How to minimize latency for competitive MEV extraction?

---

**Research Status**: Initial reconnaissance complete. Comprehensive findings gathered on platform capabilities, architecture, academic research (8+ papers), attack patterns, infrastructure, and emerging trends.

**Next Research Areas Needed**:
- GitHub MEV bot implementations and code examples
- Specific algorithms for MEV detection
- Real-world MEV extraction code
- Simulation frameworks and methodologies
- Uniswap v4 hooks and MEV opportunities
- EigenLayer AVS MEV opportunities
- Cross-chain MEV detection strategies
- Long-tail MEV case studies

