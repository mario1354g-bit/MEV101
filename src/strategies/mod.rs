//! Strategies module - process events and detect MEV opportunities

pub mod arbitrage;
pub mod flashloan_arb;
pub mod liquidation;
pub mod longtail;
pub mod sandwich;

pub use arbitrage::{ArbitrageStrategy, ArbitrageStrategyConfig, DexPair, TokenInfo};
pub use flashloan_arb::{FlashloanArbStrategy, FlashloanArbConfig};
pub use liquidation::{LiquidationStrategy, LiquidationStrategyConfig};
pub use longtail::{LongTailStrategy, LongTailStrategyConfig};
pub use sandwich::{SandwichStrategy, SandwichStrategyConfig};
