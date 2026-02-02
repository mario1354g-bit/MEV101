//! Strategies module - process events and detect MEV opportunities

pub mod arbitrage;
pub mod liquidation;
pub mod sandwich;

pub use arbitrage::{ArbitrageStrategy, ArbitrageStrategyConfig, DexPair, TokenInfo};
pub use liquidation::{LiquidationStrategy, LiquidationStrategyConfig};
pub use sandwich::{SandwichStrategy, SandwichStrategyConfig};
