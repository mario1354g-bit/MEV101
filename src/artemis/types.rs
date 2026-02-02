//! Core types for Artemis-style MEV architecture
//!
//! This module defines the event and action types that flow through
//! the Collector -> Strategy -> Executor pipeline.

use alloy::primitives::{Address, B256, U256};
use alloy::rpc::types::Transaction;
use serde::{Deserialize, Serialize};

/// Events collected from various sources (mempool, blocks, DEX events)
#[derive(Debug, Clone)]
pub enum Event {
    /// New pending transaction in mempool
    PendingTx(PendingTxEvent),
    /// New block mined
    NewBlock(NewBlockEvent),
    /// DEX swap event
    Swap(SwapEvent),
    /// Liquidation opportunity detected
    Liquidation(LiquidationEvent),
    /// Price update from DEX
    PriceUpdate(PriceUpdateEvent),
}

/// Pending transaction from mempool
#[derive(Debug, Clone)]
pub struct PendingTxEvent {
    pub tx: Transaction,
    pub received_at: chrono::DateTime<chrono::Utc>,
}

/// New block event
#[derive(Debug, Clone)]
pub struct NewBlockEvent {
    pub block_number: u64,
    pub block_hash: B256,
    pub timestamp: u64,
    pub base_fee: Option<u128>,
}

/// DEX swap event
#[derive(Debug, Clone)]
pub struct SwapEvent {
    pub pool: Address,
    pub dex: DexType,
    pub token0: Address,
    pub token1: Address,
    pub amount0: i128,
    pub amount1: i128,
    pub price: f64,
    pub block_number: u64,
    pub tx_hash: B256,
}

/// Liquidation opportunity
#[derive(Debug, Clone)]
pub struct LiquidationEvent {
    pub protocol: LendingProtocol,
    pub user: Address,
    pub collateral_token: Address,
    pub debt_token: Address,
    pub health_factor: f64,
    pub collateral_value: U256,
    pub debt_value: U256,
}

/// Price update from DEX
#[derive(Debug, Clone)]
pub struct PriceUpdateEvent {
    pub pair: String,
    pub pool: Address,
    pub price: f64,
    pub liquidity: U256,
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

/// DEX types supported
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DexType {
    UniswapV2,
    UniswapV3,
    SushiSwap,
    Curve,
    Balancer,
    PancakeSwap,
}

impl std::fmt::Display for DexType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DexType::UniswapV2 => write!(f, "UniswapV2"),
            DexType::UniswapV3 => write!(f, "UniswapV3"),
            DexType::SushiSwap => write!(f, "SushiSwap"),
            DexType::Curve => write!(f, "Curve"),
            DexType::Balancer => write!(f, "Balancer"),
            DexType::PancakeSwap => write!(f, "PancakeSwap"),
        }
    }
}

/// Lending protocols for liquidation
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum LendingProtocol {
    AaveV3,
    CompoundV3,
    MakerDAO,
}

impl std::fmt::Display for LendingProtocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LendingProtocol::AaveV3 => write!(f, "AaveV3"),
            LendingProtocol::CompoundV3 => write!(f, "CompoundV3"),
            LendingProtocol::MakerDAO => write!(f, "MakerDAO"),
        }
    }
}

/// Actions produced by strategies for executors
#[derive(Debug, Clone)]
pub enum Action {
    /// Submit arbitrage bundle
    Arbitrage(ArbitrageAction),
    /// Submit sandwich attack bundle
    Sandwich(SandwichAction),
    /// Submit liquidation transaction
    Liquidation(LiquidationAction),
    /// Submit backrun transaction
    Backrun(BackrunAction),
}

/// Arbitrage action to execute
#[derive(Debug, Clone)]
pub struct ArbitrageAction {
    pub id: String,
    pub path: Vec<SwapStep>,
    pub input_token: Address,
    pub input_amount: U256,
    pub expected_output: U256,
    pub expected_profit: U256,
    pub min_profit: U256,
    pub deadline: u64,
    pub use_flashloan: bool,
    pub flashloan_token: Option<Address>,
    pub flashloan_amount: Option<U256>,
    pub gas_price: u128,
    pub priority_fee: u128,
}

/// Single swap in arbitrage path
#[derive(Debug, Clone)]
pub struct SwapStep {
    pub dex: DexType,
    pub pool: Address,
    pub token_in: Address,
    pub token_out: Address,
    pub amount_in: U256,
    pub min_amount_out: U256,
}

/// Sandwich attack action
#[derive(Debug, Clone)]
pub struct SandwichAction {
    pub id: String,
    pub target_tx: B256,
    pub frontrun: SandwichTx,
    pub backrun: SandwichTx,
    pub expected_profit: U256,
    pub gas_price: u128,
    pub priority_fee: u128,
}

/// Single transaction in sandwich bundle
#[derive(Debug, Clone)]
pub struct SandwichTx {
    pub pool: Address,
    pub token_in: Address,
    pub token_out: Address,
    pub amount_in: U256,
    pub min_amount_out: U256,
}

/// Liquidation action
#[derive(Debug, Clone)]
pub struct LiquidationAction {
    pub id: String,
    pub protocol: LendingProtocol,
    pub user: Address,
    pub collateral_token: Address,
    pub debt_token: Address,
    pub debt_to_cover: U256,
    pub expected_collateral: U256,
    pub expected_profit: U256,
    pub use_flashloan: bool,
    pub gas_price: u128,
    pub priority_fee: u128,
}

/// Backrun action
#[derive(Debug, Clone)]
pub struct BackrunAction {
    pub id: String,
    pub target_tx: B256,
    pub arb: ArbitrageAction,
}

/// Execution result
#[derive(Debug, Clone)]
pub enum ExecutionResult {
    Success {
        action_id: String,
        tx_hash: B256,
        profit: U256,
        gas_used: u64,
    },
    Failed {
        action_id: String,
        reason: String,
    },
    Simulated {
        action_id: String,
        would_profit: U256,
        gas_estimate: u64,
    },
}
