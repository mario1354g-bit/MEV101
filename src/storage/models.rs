//! Data models for the MEV bot storage layer.
//!
//! This module defines all database entities with proper serialization
//! and sqlx integration for SQLite.

use serde::{Deserialize, Serialize};
use sqlx::FromRow;

/// Type of MEV opportunity detected.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OpportunityType {
    /// Price discrepancy between two venues for the same pair
    PriceDiscrepancy,
    /// Multi-hop arbitrage through multiple pools
    MultiHop,
    /// Sandwich attack opportunity
    Sandwich,
    /// Backrun opportunity after a large trade
    Backrun,
    /// Liquidation opportunity in lending protocols
    Liquidation,
    /// Liquidity event (add/remove) creating temporary arbitrage
    LiquidityEvent,
}

impl OpportunityType {
    pub fn as_str(&self) -> &'static str {
        match self {
            OpportunityType::PriceDiscrepancy => "price_discrepancy",
            OpportunityType::MultiHop => "multi_hop",
            OpportunityType::Sandwich => "sandwich",
            OpportunityType::Backrun => "backrun",
            OpportunityType::Liquidation => "liquidation",
            OpportunityType::LiquidityEvent => "liquidity_event",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "price_discrepancy" => Some(OpportunityType::PriceDiscrepancy),
            "multi_hop" => Some(OpportunityType::MultiHop),
            "sandwich" => Some(OpportunityType::Sandwich),
            "backrun" => Some(OpportunityType::Backrun),
            "liquidation" => Some(OpportunityType::Liquidation),
            "liquidity_event" => Some(OpportunityType::LiquidityEvent),
            _ => None,
        }
    }
}

impl std::fmt::Display for OpportunityType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// An MEV opportunity detected by the bot.
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Opportunity {
    /// Unique identifier
    pub id: i64,
    /// Unix timestamp when detected (milliseconds)
    pub timestamp: i64,
    /// Type of opportunity (stored as string in DB)
    pub opportunity_type: String,
    /// JSON array of token pairs involved
    pub token_pairs: String,
    /// JSON array of protocol/venue names
    pub protocol_venues: String,
    /// Estimated gross profit in wei (as string for precision)
    pub estimated_gross_profit_wei: String,
    /// Estimated gas cost in wei
    pub estimated_gas_cost_wei: String,
    /// Estimated net profit in wei
    pub estimated_net_profit_wei: String,
    /// Block number when opportunity was detected
    pub block_number_detected: i64,
    /// Block number when opportunity disappeared (if tracked)
    pub block_number_disappeared: Option<i64>,
    /// Number of blocks the opportunity persisted
    pub blocks_persisted: Option<i64>,
    /// Whether a competitor captured this opportunity
    pub captured_by_competitor: bool,
    /// Transaction hash of competitor's capture
    pub competitor_tx_hash: Option<String>,
    /// Whether simulation was run
    pub simulated: bool,
    /// Whether simulation showed profitability
    pub simulation_profitable: Option<bool>,
    /// JSON simulation result details
    pub simulation_result: Option<String>,
    /// Whether execution was attempted
    pub executed: bool,
    /// Transaction hash of our execution
    pub execution_tx_hash: Option<String>,
    /// Actual profit achieved in wei
    pub execution_profit_wei: Option<String>,
    /// Created timestamp
    pub created_at: Option<String>,
    /// Updated timestamp
    pub updated_at: Option<String>,
}

/// Data for inserting a new opportunity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewOpportunity {
    pub timestamp: i64,
    pub opportunity_type: OpportunityType,
    pub token_pairs: Vec<String>,
    pub protocol_venues: Vec<String>,
    pub estimated_gross_profit_wei: String,
    pub estimated_gas_cost_wei: String,
    pub estimated_net_profit_wei: String,
    pub block_number_detected: i64,
}

/// DEX liquidity pool information.
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Pool {
    /// Unique identifier
    pub id: i64,
    /// Pool contract address
    pub address: String,
    /// DEX protocol name (e.g., "uniswap_v2", "sushiswap")
    pub protocol: String,
    /// Token 0 address
    pub token0_address: String,
    /// Token 0 symbol
    pub token0_symbol: String,
    /// Token 0 decimals
    pub token0_decimals: i32,
    /// Token 1 address
    pub token1_address: String,
    /// Token 1 symbol
    pub token1_symbol: String,
    /// Token 1 decimals
    pub token1_decimals: i32,
    /// Fee tier in basis points (e.g., 30 = 0.30%)
    pub fee_bps: i32,
    /// Current reserve of token0 (as string for precision)
    pub reserve0: Option<String>,
    /// Current reserve of token1 (as string for precision)
    pub reserve1: Option<String>,
    /// Total value locked in USD
    pub tvl_usd: Option<f64>,
    /// Whether pool is actively monitored
    pub is_active: bool,
    /// Last activity timestamp
    pub last_activity: Option<i64>,
    /// Block number of last sync
    pub last_sync_block: Option<i64>,
    /// Created timestamp
    pub created_at: Option<String>,
    /// Updated timestamp
    pub updated_at: Option<String>,
}

/// Data for inserting a new pool.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewPool {
    pub address: String,
    pub protocol: String,
    pub token0_address: String,
    pub token0_symbol: String,
    pub token0_decimals: i32,
    pub token1_address: String,
    pub token1_symbol: String,
    pub token1_decimals: i32,
    pub fee_bps: i32,
}

/// Price snapshot for a token pair at a specific time.
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct PriceSnapshot {
    /// Unique identifier
    pub id: i64,
    /// Pool address this snapshot is from
    pub pool_address: String,
    /// Token pair identifier (e.g., "WETH/USDC")
    pub token_pair: String,
    /// Price of token0 in terms of token1
    pub price: f64,
    /// Reserve of token0 at snapshot time
    pub reserve0: String,
    /// Reserve of token1 at snapshot time
    pub reserve1: String,
    /// Block number
    pub block_number: i64,
    /// Unix timestamp (milliseconds)
    pub timestamp: i64,
    /// Transaction hash that triggered the update (if any)
    pub tx_hash: Option<String>,
    /// Created timestamp
    pub created_at: Option<String>,
}

/// Data for inserting a new price snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewPriceSnapshot {
    pub pool_address: String,
    pub token_pair: String,
    pub price: f64,
    pub reserve0: String,
    pub reserve1: String,
    pub block_number: i64,
    pub timestamp: i64,
    pub tx_hash: Option<String>,
}

/// Execution status of an MEV trade.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionStatus {
    /// Transaction submitted to mempool
    Pending,
    /// Transaction included in block
    Confirmed,
    /// Transaction failed/reverted
    Failed,
    /// Transaction was frontrun
    Frontrun,
    /// Transaction timed out
    Timeout,
}

impl ExecutionStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            ExecutionStatus::Pending => "pending",
            ExecutionStatus::Confirmed => "confirmed",
            ExecutionStatus::Failed => "failed",
            ExecutionStatus::Frontrun => "frontrun",
            ExecutionStatus::Timeout => "timeout",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "pending" => Some(ExecutionStatus::Pending),
            "confirmed" => Some(ExecutionStatus::Confirmed),
            "failed" => Some(ExecutionStatus::Failed),
            "frontrun" => Some(ExecutionStatus::Frontrun),
            "timeout" => Some(ExecutionStatus::Timeout),
            _ => None,
        }
    }
}

impl std::fmt::Display for ExecutionStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Record of an executed MEV trade.
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct Execution {
    /// Unique identifier
    pub id: i64,
    /// Related opportunity ID
    pub opportunity_id: i64,
    /// Transaction hash
    pub tx_hash: String,
    /// Block number where executed
    pub block_number: Option<i64>,
    /// Position in block
    pub tx_index: Option<i32>,
    /// Execution status
    pub status: String,
    /// Gas price used (in wei)
    pub gas_price_wei: String,
    /// Gas limit set
    pub gas_limit: i64,
    /// Actual gas used
    pub gas_used: Option<i64>,
    /// Total gas cost in wei
    pub gas_cost_wei: Option<String>,
    /// Gross profit in wei
    pub gross_profit_wei: Option<String>,
    /// Net profit in wei (after gas)
    pub net_profit_wei: Option<String>,
    /// Slippage percentage experienced
    pub slippage_bps: Option<i32>,
    /// Error message if failed
    pub error_message: Option<String>,
    /// Unix timestamp when submitted
    pub submitted_at: i64,
    /// Unix timestamp when confirmed
    pub confirmed_at: Option<i64>,
    /// JSON blob of execution details
    pub execution_details: Option<String>,
    /// Created timestamp
    pub created_at: Option<String>,
    /// Updated timestamp
    pub updated_at: Option<String>,
}

/// Data for inserting a new execution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewExecution {
    pub opportunity_id: i64,
    pub tx_hash: String,
    pub status: ExecutionStatus,
    pub gas_price_wei: String,
    pub gas_limit: i64,
    pub submitted_at: i64,
}

/// Filters for querying opportunities.
#[derive(Debug, Clone, Default)]
pub struct OpportunityFilter {
    pub opportunity_type: Option<OpportunityType>,
    pub min_profit_wei: Option<String>,
    pub from_timestamp: Option<i64>,
    pub to_timestamp: Option<i64>,
    pub from_block: Option<i64>,
    pub to_block: Option<i64>,
    pub simulated: Option<bool>,
    pub executed: Option<bool>,
    pub captured_by_competitor: Option<bool>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

/// Filters for querying executions.
#[derive(Debug, Clone, Default)]
pub struct ExecutionFilter {
    pub opportunity_id: Option<i64>,
    pub status: Option<ExecutionStatus>,
    pub from_timestamp: Option<i64>,
    pub to_timestamp: Option<i64>,
    pub from_block: Option<i64>,
    pub to_block: Option<i64>,
    pub profitable_only: Option<bool>,
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

/// Aggregate statistics for opportunities.
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct OpportunityStats {
    pub total_count: i64,
    pub simulated_count: i64,
    pub executed_count: i64,
    pub competitor_captured_count: i64,
    pub total_estimated_profit_wei: Option<String>,
    pub total_execution_profit_wei: Option<String>,
}

/// Aggregate statistics for executions.
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct ExecutionStats {
    pub total_count: i64,
    pub confirmed_count: i64,
    pub failed_count: i64,
    pub frontrun_count: i64,
    pub total_gas_spent_wei: Option<String>,
    pub total_gross_profit_wei: Option<String>,
    pub total_net_profit_wei: Option<String>,
    pub avg_slippage_bps: Option<f64>,
}
