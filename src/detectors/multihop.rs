//! Multi-Hop Arbitrage Detector
//!
//! Detects arbitrage opportunities across multiple pools using
//! Bellman-Ford algorithm to find negative cycles in the price graph.
//! Supports triangular arbitrage (A -> B -> C -> A) and longer paths.

use alloy::primitives::{Address, U256};
use async_trait::async_trait;
use chrono::Utc;
use std::collections::{HashMap, HashSet};

use super::{
    calculate_amount_out, estimate_gas_cost, generate_opportunity_id, u256_to_f64,
    Detector, DetectorConfig, DetectorContext, MonitorEvent, Opportunity,
    OpportunityType, PoolInfo, Priority, RegisteredPool, SwapStep,
};
use crate::error::Result;

/// Detector for multi-hop arbitrage opportunities
pub struct MultihopDetector {
    /// Name of this detector
    name: String,
    /// Maximum number of hops (gas constraint)
    max_hops: usize,
    /// Minimum profit ratio (1.001 = 0.1% profit)
    min_profit_ratio: f64,
}

impl MultihopDetector {
    pub fn new() -> Self {
        Self {
            name: "multihop_arbitrage".to_string(),
            max_hops: 4,
            min_profit_ratio: 1.001, // 0.1% minimum
        }
    }

    pub fn with_max_hops(mut self, hops: usize) -> Self {
        self.max_hops = hops.min(6).max(2); // Clamp between 2 and 6
        self
    }

    pub fn with_min_profit_ratio(mut self, ratio: f64) -> Self {
        self.min_profit_ratio = ratio;
        self
    }

    /// Build a price graph from registered pools
    fn build_graph(&self, pools: &[RegisteredPool]) -> PriceGraph {
        let mut graph = PriceGraph::new();

        for pool in pools {
            if pool.reserve0.is_zero() || pool.reserve1.is_zero() {
                continue;
            }

            // Add edges for both directions
            // Edge weight is -log(exchange_rate) for Bellman-Ford
            // A negative cycle means: product of rates > 1 (profitable)

            // token0 -> token1
            let rate_0_to_1 = self.calculate_effective_rate(
                pool.reserve0,
                pool.reserve1,
                pool.fee_bps,
            );
            if rate_0_to_1 > 0.0 {
                graph.add_edge(
                    pool.token0,
                    pool.token1,
                    -rate_0_to_1.ln(),
                    pool.clone(),
                    false,
                );
            }

            // token1 -> token0
            let rate_1_to_0 = self.calculate_effective_rate(
                pool.reserve1,
                pool.reserve0,
                pool.fee_bps,
            );
            if rate_1_to_0 > 0.0 {
                graph.add_edge(
                    pool.token1,
                    pool.token0,
                    -rate_1_to_0.ln(),
                    pool.clone(),
                    true,
                );
            }
        }

        graph
    }

    /// Calculate effective exchange rate including fees
    fn calculate_effective_rate(&self, reserve_in: U256, reserve_out: U256, fee_bps: u32) -> f64 {
        let r_in = u256_to_f64(reserve_in);
        let r_out = u256_to_f64(reserve_out);

        if r_in == 0.0 {
            return 0.0;
        }

        let fee = (fee_bps as f64) / 10_000.0;
        // Effective rate for small amounts: (reserve_out / reserve_in) * (1 - fee)
        (r_out / r_in) * (1.0 - fee)
    }

    /// Find negative cycles using Bellman-Ford variant
    fn find_negative_cycles(&self, graph: &PriceGraph) -> Vec<Cycle> {
        let mut cycles = Vec::new();
        let tokens: Vec<Address> = graph.nodes().collect();

        if tokens.is_empty() {
            return cycles;
        }

        // Run Bellman-Ford from each token
        for start in &tokens {
            if let Some(cycle) = self.bellman_ford_find_cycle(graph, *start) {
                // Verify cycle is profitable
                if self.is_profitable_cycle(&cycle) {
                    // Check for duplicates
                    if !cycles.iter().any(|c| self.cycles_equivalent(c, &cycle)) {
                        cycles.push(cycle);
                    }
                }
            }
        }

        cycles
    }

    /// Bellman-Ford algorithm to find a negative cycle
    fn bellman_ford_find_cycle(&self, graph: &PriceGraph, start: Address) -> Option<Cycle> {
        let tokens: Vec<Address> = graph.nodes().collect();
        let n = tokens.len();

        if n == 0 {
            return None;
        }

        // Distance and predecessor maps
        let mut dist: HashMap<Address, f64> = HashMap::new();
        let mut pred: HashMap<Address, (Address, usize)> = HashMap::new(); // (prev_token, edge_index)

        for token in &tokens {
            dist.insert(*token, f64::INFINITY);
        }
        dist.insert(start, 0.0);

        // Relax edges n-1 times
        for _ in 0..n.min(self.max_hops) {
            let mut updated = false;
            for token in &tokens {
                if let Some(edges) = graph.edges_from(*token) {
                    for (idx, edge) in edges.iter().enumerate() {
                        let d = dist.get(token).copied().unwrap_or(f64::INFINITY);
                        if d == f64::INFINITY {
                            continue;
                        }

                        let new_dist = d + edge.weight;
                        let current = dist.get(&edge.to).copied().unwrap_or(f64::INFINITY);

                        if new_dist < current {
                            dist.insert(edge.to, new_dist);
                            pred.insert(edge.to, (*token, idx));
                            updated = true;
                        }
                    }
                }
            }
            if !updated {
                break;
            }
        }

        // Check for negative cycle (one more iteration)
        for token in &tokens {
            if let Some(edges) = graph.edges_from(*token) {
                for edge in edges {
                    let d = dist.get(token).copied().unwrap_or(f64::INFINITY);
                    if d == f64::INFINITY {
                        continue;
                    }

                    let new_dist = d + edge.weight;
                    let current = dist.get(&edge.to).copied().unwrap_or(f64::INFINITY);

                    if new_dist < current {
                        // Found negative cycle - reconstruct it
                        return self.reconstruct_cycle(graph, &pred, edge.to);
                    }
                }
            }
        }

        // Alternative: directly search for profitable short cycles from start
        self.find_short_cycle(graph, start)
    }

    /// Find short profitable cycles using DFS
    fn find_short_cycle(&self, graph: &PriceGraph, start: Address) -> Option<Cycle> {
        let mut best_cycle: Option<Cycle> = None;
        let mut best_profit = 0.0f64;

        // DFS to find cycles
        self.dfs_find_cycles(
            graph,
            start,
            start,
            vec![],
            vec![],
            0.0,
            &mut best_cycle,
            &mut best_profit,
            &mut HashSet::new(),
        );

        best_cycle
    }

    /// DFS helper to find profitable cycles
    fn dfs_find_cycles(
        &self,
        graph: &PriceGraph,
        start: Address,
        current: Address,
        path: Vec<Address>,
        edges: Vec<GraphEdge>,
        total_weight: f64,
        best_cycle: &mut Option<Cycle>,
        best_profit: &mut f64,
        visited: &mut HashSet<Address>,
    ) {
        if path.len() >= self.max_hops {
            return;
        }

        if let Some(graph_edges) = graph.edges_from(current) {
            for edge in graph_edges {
                let new_weight = total_weight + edge.weight;

                // Check if we've completed a cycle back to start
                if edge.to == start && path.len() >= 2 {
                    // Negative total weight = profitable cycle
                    let profit_ratio = (-new_weight).exp();
                    if profit_ratio > self.min_profit_ratio && profit_ratio > *best_profit {
                        let mut cycle_path = path.clone();
                        cycle_path.push(current);
                        cycle_path.push(start);

                        let mut cycle_edges = edges.clone();
                        cycle_edges.push(edge.clone());

                        *best_profit = profit_ratio;
                        *best_cycle = Some(Cycle {
                            path: cycle_path,
                            edges: cycle_edges,
                            profit_ratio,
                        });
                    }
                    continue;
                }

                // Continue DFS if not visited
                if !visited.contains(&edge.to) && !path.contains(&edge.to) {
                    let mut new_path = path.clone();
                    new_path.push(current);

                    let mut new_edges = edges.clone();
                    new_edges.push(edge.clone());

                    visited.insert(edge.to);
                    self.dfs_find_cycles(
                        graph,
                        start,
                        edge.to,
                        new_path,
                        new_edges,
                        new_weight,
                        best_cycle,
                        best_profit,
                        visited,
                    );
                    visited.remove(&edge.to);
                }
            }
        }
    }

    /// Reconstruct cycle from predecessor map
    fn reconstruct_cycle(
        &self,
        graph: &PriceGraph,
        pred: &HashMap<Address, (Address, usize)>,
        start: Address,
    ) -> Option<Cycle> {
        let mut path = Vec::new();
        let mut edges = Vec::new();
        let mut current = start;
        let mut visited = HashSet::new();

        // Walk back through predecessors
        while !visited.contains(&current) {
            visited.insert(current);
            path.push(current);

            if let Some((prev, edge_idx)) = pred.get(&current) {
                if let Some(graph_edges) = graph.edges_from(*prev) {
                    if let Some(edge) = graph_edges.get(*edge_idx) {
                        edges.push(edge.clone());
                    }
                }
                current = *prev;
            } else {
                break;
            }

            if path.len() > self.max_hops + 1 {
                break;
            }
        }

        // Find where the cycle starts
        if let Some(cycle_start_idx) = path.iter().position(|&t| t == current) {
            let cycle_path: Vec<Address> = path[..=cycle_start_idx].iter().rev().copied().collect();
            let cycle_edges: Vec<GraphEdge> = edges[..cycle_start_idx].iter().rev().cloned().collect();

            if cycle_path.len() >= 3 && cycle_path.len() <= self.max_hops + 1 {
                let profit_ratio = self.calculate_cycle_profit_ratio(&cycle_edges);
                return Some(Cycle {
                    path: cycle_path,
                    edges: cycle_edges,
                    profit_ratio,
                });
            }
        }

        None
    }

    /// Calculate profit ratio for a cycle
    fn calculate_cycle_profit_ratio(&self, edges: &[GraphEdge]) -> f64 {
        let total_weight: f64 = edges.iter().map(|e| e.weight).sum();
        (-total_weight).exp()
    }

    /// Check if cycle is profitable after gas
    fn is_profitable_cycle(&self, cycle: &Cycle) -> bool {
        cycle.profit_ratio > self.min_profit_ratio
    }

    /// Check if two cycles are equivalent (same edges in different order)
    fn cycles_equivalent(&self, a: &Cycle, b: &Cycle) -> bool {
        if a.path.len() != b.path.len() {
            return false;
        }

        // Check if b is a rotation of a
        let a_set: HashSet<_> = a.path.iter().collect();
        let b_set: HashSet<_> = b.path.iter().collect();
        a_set == b_set
    }

    /// Calculate actual profit for a cycle with a given input amount
    fn simulate_cycle(
        &self,
        cycle: &Cycle,
        input_amount: U256,
    ) -> (U256, Vec<U256>) {
        let mut current_amount = input_amount;
        let mut amounts = vec![input_amount];

        for edge in &cycle.edges {
            let (reserve_in, reserve_out) = if edge.reversed {
                (edge.pool.reserve1, edge.pool.reserve0)
            } else {
                (edge.pool.reserve0, edge.pool.reserve1)
            };

            current_amount = calculate_amount_out(
                current_amount,
                reserve_in,
                reserve_out,
                edge.pool.fee_bps,
            );
            amounts.push(current_amount);
        }

        (current_amount, amounts)
    }

    /// Find optimal input amount for cycle
    fn find_optimal_amount(&self, cycle: &Cycle) -> (U256, U256) {
        // Get the first pool's reserve as reference
        let first_edge = &cycle.edges[0];
        let reserve = if first_edge.reversed {
            first_edge.pool.reserve1
        } else {
            first_edge.pool.reserve0
        };

        let reserve_f64 = u256_to_f64(reserve);

        // Binary search for optimal amount
        // Start with 0.1% of reserves, go up to 5%
        let min_ratio = 0.001;
        let max_ratio = 0.05;

        let mut best_profit = U256::ZERO;
        let mut best_amount = U256::ZERO;

        for i in 0..50 {
            let ratio = min_ratio + (i as f64 / 50.0) * (max_ratio - min_ratio);
            let amount = super::f64_to_u256(reserve_f64 * ratio);

            let (output, _) = self.simulate_cycle(cycle, amount);

            if output > amount {
                let profit = output - amount;
                if profit > best_profit {
                    best_profit = profit;
                    best_amount = amount;
                }
            }
        }

        (best_amount, best_profit)
    }

    /// Build opportunity from cycle
    fn build_opportunity(
        &self,
        cycle: &Cycle,
        input_amount: U256,
        gross_profit: U256,
        gas_cost: U256,
    ) -> Option<Opportunity> {
        if gross_profit <= gas_cost {
            return None;
        }

        let net_profit = gross_profit - gas_cost;
        let tokens: Vec<Address> = cycle.path.iter().take(cycle.path.len() - 1).copied().collect();

        let mut swap_path = Vec::new();
        let mut pools = Vec::new();

        let (_, amounts) = self.simulate_cycle(cycle, input_amount);

        for (i, edge) in cycle.edges.iter().enumerate() {
            let (token_in, token_out) = if edge.reversed {
                (edge.pool.token1, edge.pool.token0)
            } else {
                (edge.pool.token0, edge.pool.token1)
            };

            swap_path.push(SwapStep {
                pool: edge.pool.address,
                dex: edge.pool.dex.clone(),
                token_in,
                token_out,
                amount_in: amounts[i],
                min_amount_out: amounts.get(i + 1).copied().unwrap_or(U256::ZERO) * U256::from(99) / U256::from(100),
            });

            if !pools.iter().any(|p: &PoolInfo| p.address == edge.pool.address) {
                pools.push(PoolInfo {
                    address: edge.pool.address,
                    dex: edge.pool.dex.clone(),
                    token0: edge.pool.token0,
                    token1: edge.pool.token1,
                    reserve0: edge.pool.reserve0,
                    reserve1: edge.pool.reserve1,
                    fee_bps: edge.pool.fee_bps,
                });
            }
        }

        let priority = if net_profit > U256::from(10_000_000_000_000_000_000u128) {
            Priority::Critical
        } else if net_profit > U256::from(1_000_000_000_000_000_000u128) {
            Priority::High
        } else if net_profit > U256::from(100_000_000_000_000_000u128) {
            Priority::Medium
        } else {
            Priority::Low
        };

        let mut metadata = HashMap::new();
        metadata.insert("hops".to_string(), cycle.edges.len().to_string());
        metadata.insert("profit_ratio".to_string(), format!("{:.6}", cycle.profit_ratio));
        metadata.insert(
            "path".to_string(),
            tokens.iter().map(|t| format!("{:x}", t).chars().take(8).collect::<String>()).collect::<Vec<_>>().join(" -> "),
        );

        Some(Opportunity {
            id: generate_opportunity_id(OpportunityType::MultihopArbitrage, &tokens),
            opportunity_type: OpportunityType::MultihopArbitrage,
            priority,
            estimated_profit: gross_profit,
            estimated_gas_cost: gas_cost,
            net_profit,
            tokens,
            pools,
            swap_path,
            target_tx: None,
            deadline_block: None,
            detected_at: Utc::now(),
            confidence: self.calculate_confidence(cycle, &net_profit),
            metadata,
        })
    }

    fn calculate_confidence(&self, cycle: &Cycle, net_profit: &U256) -> f64 {
        let mut confidence = 0.8;

        // More hops = less confidence (more things can go wrong)
        confidence -= (cycle.edges.len() as f64 - 2.0) * 0.1;

        // Higher profit ratio = more confidence
        if cycle.profit_ratio > 1.01 {
            confidence += 0.1;
        }

        // Larger net profit = more confidence
        if *net_profit > U256::from(1_000_000_000_000_000_000u128) {
            confidence += 0.05;
        }

        confidence.min(0.95).max(0.3)
    }
}

impl Default for MultihopDetector {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Detector for MultihopDetector {
    fn name(&self) -> &str {
        &self.name
    }

    fn is_enabled(&self, config: &DetectorConfig) -> bool {
        config.enable_multihop
    }

    async fn detect(
        &self,
        event: &MonitorEvent,
        ctx: &DetectorContext,
    ) -> Result<Vec<Opportunity>> {
        let mut opportunities = Vec::new();

        match event {
            MonitorEvent::PriceUpdate { .. } | MonitorEvent::SwapExecuted { .. } => {
                // Get all pools
                let pools = ctx.pool_registry.all_pools();

                if pools.len() < 3 {
                    return Ok(opportunities);
                }

                // Build price graph
                let graph = self.build_graph(&pools);

                // Find negative cycles
                let cycles = self.find_negative_cycles(&graph);

                let gas_price = ctx.get_gas_price().await;

                for cycle in cycles {
                    // Estimate gas for multi-hop
                    let gas_cost = estimate_gas_cost(cycle.edges.len(), 150_000, gas_price);

                    // Find optimal input amount
                    let (optimal_amount, gross_profit) = self.find_optimal_amount(&cycle);

                    if optimal_amount.is_zero() || gross_profit.is_zero() {
                        continue;
                    }

                    // Build opportunity if profitable after gas
                    if let Some(opp) = self.build_opportunity(&cycle, optimal_amount, gross_profit, gas_cost) {
                        if opp.net_profit >= ctx.config.min_profit_wei {
                            tracing::info!(
                                "Multi-hop opportunity found: {} hops, profit: {} wei",
                                cycle.edges.len(),
                                opp.net_profit
                            );
                            opportunities.push(opp);
                        }
                    }
                }
            }
            MonitorEvent::PoolCreated { pool, token0, token1, fee: _, .. } => {
                // New pool might create new arbitrage paths
                // Register it and scan for opportunities
                tracing::info!(
                    "New pool {} for pair {:?}/{:?} - scanning for multi-hop opportunities",
                    pool,
                    token0,
                    token1
                );
                // The actual registration should happen elsewhere, we just detect
            }
            _ => {}
        }

        Ok(opportunities)
    }
}

/// Price graph for arbitrage detection
struct PriceGraph {
    /// Adjacency list: token -> edges
    edges: HashMap<Address, Vec<GraphEdge>>,
}

impl PriceGraph {
    fn new() -> Self {
        Self {
            edges: HashMap::new(),
        }
    }

    fn add_edge(&mut self, from: Address, to: Address, weight: f64, pool: RegisteredPool, reversed: bool) {
        self.edges.entry(from).or_default().push(GraphEdge {
            from,
            to,
            weight,
            pool,
            reversed,
        });
    }

    fn edges_from(&self, token: Address) -> Option<&Vec<GraphEdge>> {
        self.edges.get(&token)
    }

    fn nodes(&self) -> impl Iterator<Item = Address> + '_ {
        self.edges.keys().copied()
    }
}

/// Edge in the price graph
#[derive(Clone)]
struct GraphEdge {
    from: Address,
    to: Address,
    /// Weight = -ln(exchange_rate)
    weight: f64,
    /// Pool for this edge
    pool: RegisteredPool,
    /// Whether swap is reversed (token1 -> token0)
    reversed: bool,
}

/// A cycle (potential arbitrage)
struct Cycle {
    /// Tokens in the cycle (last == first)
    path: Vec<Address>,
    /// Edges traversed
    edges: Vec<GraphEdge>,
    /// Profit ratio (> 1 means profitable)
    profit_ratio: f64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn create_test_pool(
        address: u8,
        dex: &str,
        token0: u8,
        token1: u8,
        reserve0: u128,
        reserve1: u128,
    ) -> RegisteredPool {
        RegisteredPool {
            address: Address::repeat_byte(address),
            dex: dex.to_string(),
            token0: Address::repeat_byte(token0),
            token1: Address::repeat_byte(token1),
            reserve0: U256::from(reserve0),
            reserve1: U256::from(reserve1),
            fee_bps: 30,
            last_updated: Utc::now(),
        }
    }

    #[test]
    fn test_build_graph() {
        let detector = MultihopDetector::new();

        let pools = vec![
            create_test_pool(1, "uni", 0x01, 0x02, 100e18 as u128, 200e18 as u128),
            create_test_pool(2, "sushi", 0x02, 0x03, 200e18 as u128, 100e18 as u128),
            create_test_pool(3, "uni", 0x03, 0x01, 100e18 as u128, 105e18 as u128),
        ];

        let graph = detector.build_graph(&pools);

        // Should have edges for all 3 tokens
        assert!(graph.edges_from(Address::repeat_byte(0x01)).is_some());
        assert!(graph.edges_from(Address::repeat_byte(0x02)).is_some());
        assert!(graph.edges_from(Address::repeat_byte(0x03)).is_some());
    }

    #[test]
    fn test_triangular_arbitrage() {
        let detector = MultihopDetector::new().with_min_profit_ratio(1.001);

        // Create a triangular arbitrage opportunity
        // A -> B: rate 2.0 (100 A = 200 B)
        // B -> C: rate 0.5 (200 B = 100 C)
        // C -> A: rate 1.05 (100 C = 105 A) <- profit here!
        // Total: 100 A -> 200 B -> 100 C -> 105 A (5% profit)

        let pools = vec![
            create_test_pool(1, "uni", 0x01, 0x02, 100e18 as u128, 200e18 as u128),
            create_test_pool(2, "sushi", 0x02, 0x03, 200e18 as u128, 100e18 as u128),
            create_test_pool(3, "curve", 0x03, 0x01, 100e18 as u128, 105e18 as u128),
        ];

        let graph = detector.build_graph(&pools);
        let cycles = detector.find_negative_cycles(&graph);

        // Should find at least one profitable cycle
        assert!(!cycles.is_empty(), "Should find triangular arbitrage");

        for cycle in &cycles {
            assert!(cycle.profit_ratio > 1.0, "Cycle should be profitable");
        }
    }

    #[test]
    fn test_simulate_cycle() {
        let detector = MultihopDetector::new();

        // Simple 2-hop cycle
        let pools = vec![
            create_test_pool(1, "uni", 0x01, 0x02, 100e18 as u128, 200e18 as u128),
            create_test_pool(2, "sushi", 0x02, 0x01, 200e18 as u128, 105e18 as u128),
        ];

        let graph = detector.build_graph(&pools);

        // Create a manual cycle
        let edges: Vec<GraphEdge> = vec![
            GraphEdge {
                from: Address::repeat_byte(0x01),
                to: Address::repeat_byte(0x02),
                weight: 0.0,
                pool: pools[0].clone(),
                reversed: false,
            },
            GraphEdge {
                from: Address::repeat_byte(0x02),
                to: Address::repeat_byte(0x01),
                weight: 0.0,
                pool: pools[1].clone(),
                reversed: false,
            },
        ];

        let cycle = Cycle {
            path: vec![
                Address::repeat_byte(0x01),
                Address::repeat_byte(0x02),
                Address::repeat_byte(0x01),
            ],
            edges,
            profit_ratio: 1.0,
        };

        let input = U256::from(1e18 as u128);
        let (output, amounts) = detector.simulate_cycle(&cycle, input);

        assert_eq!(amounts.len(), 3);
        assert!(output > U256::ZERO);
    }
}
