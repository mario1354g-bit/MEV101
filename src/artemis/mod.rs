//! Artemis-style MEV architecture
//!
//! This module implements Paradigm's Artemis pattern using alloy:
//! - Collectors: Gather events from mempool, blocks, DEX events
//! - Strategies: Process events and detect MEV opportunities
//! - Executors: Execute actions via Flashbots or direct submission
//! - Engine: Orchestrates the event processing pipeline

pub mod collector;
pub mod engine;
pub mod executor;
pub mod strategy;
pub mod types;

pub use collector::Collector;
pub use engine::{Engine, EngineConfig};
pub use executor::{ExecutionMode, Executor, SubmissionTarget};
pub use strategy::{StatefulStrategy, Strategy};
pub use types::*;
