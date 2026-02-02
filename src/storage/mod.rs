//! Storage layer for the MEV bot.
//!
//! This module provides a SQLite-based persistence layer for storing
//! opportunities, pools, price snapshots, and execution records.

pub mod models;
pub mod schema;

use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions};
use std::str::FromStr;
use thiserror::Error;

pub use models::*;
use schema::{queries, MIGRATIONS};

/// Storage layer errors.
#[derive(Debug, Error)]
pub enum StorageError {
    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),

    #[error("Migration error: {0}")]
    Migration(String),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("Record not found: {0}")]
    NotFound(String),

    #[error("Invalid data: {0}")]
    InvalidData(String),
}

pub type Result<T> = std::result::Result<T, StorageError>;

/// Main database interface for the MEV bot storage layer.
#[derive(Debug, Clone)]
pub struct Database {
    pool: SqlitePool,
}

impl Database {
    /// Create a new database connection pool.
    ///
    /// # Arguments
    /// * `path` - Path to the SQLite database file (use ":memory:" for in-memory)
    ///
    /// # Returns
    /// A new Database instance with an active connection pool.
    pub async fn new(path: &str) -> Result<Self> {
        let options = SqliteConnectOptions::from_str(path)
            .map_err(StorageError::Database)?
            .create_if_missing(true)
            .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
            .synchronous(sqlx::sqlite::SqliteSynchronous::Normal)
            .busy_timeout(std::time::Duration::from_secs(30));

        let pool = SqlitePoolOptions::new()
            .max_connections(10)
            .min_connections(1)
            .acquire_timeout(std::time::Duration::from_secs(10))
            .connect_with(options)
            .await?;

        Ok(Self { pool })
    }

    /// Run all pending database migrations.
    pub async fn run_migrations(&self) -> Result<()> {
        // Create migrations tracking table
        sqlx::query(schema::CREATE_MIGRATIONS_TABLE)
            .execute(&self.pool)
            .await?;

        // Run each migration if not already applied
        for migration in MIGRATIONS {
            let applied: (i64,) = sqlx::query_as(schema::CHECK_MIGRATION)
                .bind(migration.version)
                .fetch_one(&self.pool)
                .await?;

            if applied.0 == 0 {
                tracing::info!(
                    "Applying migration {}: {}",
                    migration.version,
                    migration.name
                );

                // Execute migration SQL
                sqlx::raw_sql(migration.sql)
                    .execute(&self.pool)
                    .await
                    .map_err(|e| {
                        StorageError::Migration(format!(
                            "Failed to apply migration {}: {}",
                            migration.version, e
                        ))
                    })?;

                // Record migration
                sqlx::query(schema::INSERT_MIGRATION)
                    .bind(migration.version)
                    .bind(migration.name)
                    .execute(&self.pool)
                    .await?;

                tracing::info!("Migration {} applied successfully", migration.version);
            }
        }

        Ok(())
    }

    /// Get a reference to the underlying connection pool.
    pub fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    // ==================== Opportunity Methods ====================

    /// Insert a new opportunity.
    pub async fn insert_opportunity(&self, opp: &NewOpportunity) -> Result<i64> {
        let token_pairs_json = serde_json::to_string(&opp.token_pairs)?;
        let venues_json = serde_json::to_string(&opp.protocol_venues)?;

        let result = sqlx::query(queries::INSERT_OPPORTUNITY)
            .bind(opp.timestamp)
            .bind(opp.opportunity_type.as_str())
            .bind(&token_pairs_json)
            .bind(&venues_json)
            .bind(&opp.estimated_gross_profit_wei)
            .bind(&opp.estimated_gas_cost_wei)
            .bind(&opp.estimated_net_profit_wei)
            .bind(opp.block_number_detected)
            .execute(&self.pool)
            .await?;

        Ok(result.last_insert_rowid())
    }

    /// Get an opportunity by ID.
    pub async fn get_opportunity(&self, id: i64) -> Result<Opportunity> {
        sqlx::query_as::<_, Opportunity>(queries::GET_OPPORTUNITY_BY_ID)
            .bind(id)
            .fetch_optional(&self.pool)
            .await?
            .ok_or_else(|| StorageError::NotFound(format!("Opportunity with id {}", id)))
    }

    /// Get recent opportunities with pagination.
    pub async fn get_opportunities(&self, limit: i64, offset: i64) -> Result<Vec<Opportunity>> {
        let opportunities = sqlx::query_as::<_, Opportunity>(queries::GET_RECENT_OPPORTUNITIES)
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await?;

        Ok(opportunities)
    }

    /// Get opportunities with filters.
    pub async fn get_opportunities_filtered(
        &self,
        filter: &OpportunityFilter,
    ) -> Result<Vec<Opportunity>> {
        use sqlx::QueryBuilder;

        let mut builder: QueryBuilder<sqlx::Sqlite> =
            QueryBuilder::new("SELECT * FROM opportunities WHERE 1=1");

        if let Some(ref opp_type) = filter.opportunity_type {
            builder.push(" AND opportunity_type = ");
            builder.push_bind(opp_type.as_str().to_string());
        }

        if let Some(ref min_profit) = filter.min_profit_wei {
            builder.push(" AND CAST(estimated_net_profit_wei AS INTEGER) >= ");
            builder.push_bind(min_profit.clone());
        }

        if let Some(from_ts) = filter.from_timestamp {
            builder.push(" AND timestamp >= ");
            builder.push_bind(from_ts);
        }

        if let Some(to_ts) = filter.to_timestamp {
            builder.push(" AND timestamp <= ");
            builder.push_bind(to_ts);
        }

        if let Some(from_block) = filter.from_block {
            builder.push(" AND block_number_detected >= ");
            builder.push_bind(from_block);
        }

        if let Some(to_block) = filter.to_block {
            builder.push(" AND block_number_detected <= ");
            builder.push_bind(to_block);
        }

        if let Some(simulated) = filter.simulated {
            builder.push(" AND simulated = ");
            builder.push_bind(simulated as i32);
        }

        if let Some(executed) = filter.executed {
            builder.push(" AND executed = ");
            builder.push_bind(executed as i32);
        }

        if let Some(captured) = filter.captured_by_competitor {
            builder.push(" AND captured_by_competitor = ");
            builder.push_bind(captured as i32);
        }

        builder.push(" ORDER BY timestamp DESC");

        if let Some(limit) = filter.limit {
            builder.push(" LIMIT ");
            builder.push_bind(limit);
        }

        if let Some(offset) = filter.offset {
            builder.push(" OFFSET ");
            builder.push_bind(offset);
        }

        let opportunities = builder
            .build_query_as::<Opportunity>()
            .fetch_all(&self.pool)
            .await?;
        Ok(opportunities)
    }

    /// Update opportunity with simulation results.
    pub async fn update_opportunity_simulation(
        &self,
        id: i64,
        profitable: bool,
        result: Option<&str>,
    ) -> Result<()> {
        sqlx::query(queries::UPDATE_OPPORTUNITY_SIMULATION)
            .bind(profitable)
            .bind(result)
            .bind(id)
            .execute(&self.pool)
            .await?;

        Ok(())
    }

    /// Update opportunity with execution results.
    pub async fn update_opportunity_execution(
        &self,
        id: i64,
        tx_hash: &str,
        profit_wei: &str,
    ) -> Result<()> {
        sqlx::query(queries::UPDATE_OPPORTUNITY_EXECUTION)
            .bind(tx_hash)
            .bind(profit_wei)
            .bind(id)
            .execute(&self.pool)
            .await?;

        Ok(())
    }

    /// Mark opportunity as captured by competitor.
    pub async fn update_opportunity_competitor(
        &self,
        id: i64,
        competitor_tx: &str,
        disappeared_block: i64,
        blocks_persisted: i64,
    ) -> Result<()> {
        sqlx::query(queries::UPDATE_OPPORTUNITY_COMPETITOR)
            .bind(competitor_tx)
            .bind(disappeared_block)
            .bind(blocks_persisted)
            .bind(id)
            .execute(&self.pool)
            .await?;

        Ok(())
    }

    /// Update opportunity when it disappears.
    pub async fn update_opportunity_disappeared(
        &self,
        id: i64,
        disappeared_block: i64,
        blocks_persisted: i64,
    ) -> Result<()> {
        sqlx::query(queries::UPDATE_OPPORTUNITY_DISAPPEARED)
            .bind(disappeared_block)
            .bind(blocks_persisted)
            .bind(id)
            .execute(&self.pool)
            .await?;

        Ok(())
    }

    // ==================== Pool Methods ====================

    /// Insert or update a pool.
    pub async fn insert_pool(&self, pool: &NewPool) -> Result<i64> {
        let result = sqlx::query(queries::INSERT_POOL)
            .bind(&pool.address)
            .bind(&pool.protocol)
            .bind(&pool.token0_address)
            .bind(&pool.token0_symbol)
            .bind(pool.token0_decimals)
            .bind(&pool.token1_address)
            .bind(&pool.token1_symbol)
            .bind(pool.token1_decimals)
            .bind(pool.fee_bps)
            .execute(&self.pool)
            .await?;

        Ok(result.last_insert_rowid())
    }

    /// Get a pool by address.
    pub async fn get_pool(&self, address: &str) -> Result<Pool> {
        sqlx::query_as::<_, Pool>(queries::GET_POOL_BY_ADDRESS)
            .bind(address)
            .fetch_optional(&self.pool)
            .await?
            .ok_or_else(|| StorageError::NotFound(format!("Pool with address {}", address)))
    }

    /// Get all pools for a token pair (in either direction).
    pub async fn get_pools_for_pair(&self, token0: &str, token1: &str) -> Result<Vec<Pool>> {
        let pools = sqlx::query_as::<_, Pool>(queries::GET_POOLS_FOR_PAIR)
            .bind(token0)
            .bind(token1)
            .bind(token1)
            .bind(token0)
            .fetch_all(&self.pool)
            .await?;

        Ok(pools)
    }

    /// Get all active pools.
    pub async fn get_active_pools(&self) -> Result<Vec<Pool>> {
        let pools = sqlx::query_as::<_, Pool>(queries::GET_ACTIVE_POOLS)
            .fetch_all(&self.pool)
            .await?;

        Ok(pools)
    }

    /// Get pools by protocol.
    pub async fn get_pools_by_protocol(&self, protocol: &str) -> Result<Vec<Pool>> {
        let pools = sqlx::query_as::<_, Pool>(queries::GET_POOLS_BY_PROTOCOL)
            .bind(protocol)
            .fetch_all(&self.pool)
            .await?;

        Ok(pools)
    }

    /// Update pool reserves and TVL.
    pub async fn update_pool_reserves(
        &self,
        address: &str,
        reserve0: &str,
        reserve1: &str,
        tvl_usd: f64,
        last_activity: i64,
        last_sync_block: i64,
    ) -> Result<()> {
        sqlx::query(queries::UPDATE_POOL_RESERVES)
            .bind(reserve0)
            .bind(reserve1)
            .bind(tvl_usd)
            .bind(last_activity)
            .bind(last_sync_block)
            .bind(address)
            .execute(&self.pool)
            .await?;

        Ok(())
    }

    /// Update pool activity timestamp.
    pub async fn update_pool_activity(
        &self,
        address: &str,
        last_activity: i64,
        last_sync_block: i64,
    ) -> Result<()> {
        sqlx::query(queries::UPDATE_POOL_ACTIVITY)
            .bind(last_activity)
            .bind(last_sync_block)
            .bind(address)
            .execute(&self.pool)
            .await?;

        Ok(())
    }

    /// Set pool active status.
    pub async fn set_pool_active(&self, address: &str, is_active: bool) -> Result<()> {
        sqlx::query(queries::SET_POOL_ACTIVE)
            .bind(is_active)
            .bind(address)
            .execute(&self.pool)
            .await?;

        Ok(())
    }

    // ==================== Price Snapshot Methods ====================

    /// Insert a new price snapshot.
    pub async fn insert_price_snapshot(&self, snapshot: &NewPriceSnapshot) -> Result<i64> {
        let result = sqlx::query(queries::INSERT_PRICE_SNAPSHOT)
            .bind(&snapshot.pool_address)
            .bind(&snapshot.token_pair)
            .bind(snapshot.price)
            .bind(&snapshot.reserve0)
            .bind(&snapshot.reserve1)
            .bind(snapshot.block_number)
            .bind(snapshot.timestamp)
            .bind(&snapshot.tx_hash)
            .execute(&self.pool)
            .await?;

        Ok(result.last_insert_rowid())
    }

    /// Get price history for a pool within a time range.
    pub async fn get_price_history(
        &self,
        pool_address: &str,
        from_timestamp: i64,
        to_timestamp: i64,
        limit: i64,
    ) -> Result<Vec<PriceSnapshot>> {
        let snapshots = sqlx::query_as::<_, PriceSnapshot>(queries::GET_PRICE_HISTORY)
            .bind(pool_address)
            .bind(from_timestamp)
            .bind(to_timestamp)
            .bind(limit)
            .fetch_all(&self.pool)
            .await?;

        Ok(snapshots)
    }

    /// Get the latest price for a pool.
    pub async fn get_latest_price(&self, pool_address: &str) -> Result<PriceSnapshot> {
        sqlx::query_as::<_, PriceSnapshot>(queries::GET_LATEST_PRICE)
            .bind(pool_address)
            .fetch_optional(&self.pool)
            .await?
            .ok_or_else(|| {
                StorageError::NotFound(format!("Price snapshot for pool {}", pool_address))
            })
    }

    /// Get price at a specific block.
    pub async fn get_price_at_block(
        &self,
        pool_address: &str,
        block_number: i64,
    ) -> Result<PriceSnapshot> {
        sqlx::query_as::<_, PriceSnapshot>(queries::GET_PRICE_AT_BLOCK)
            .bind(pool_address)
            .bind(block_number)
            .fetch_optional(&self.pool)
            .await?
            .ok_or_else(|| {
                StorageError::NotFound(format!(
                    "Price snapshot for pool {} at block {}",
                    pool_address, block_number
                ))
            })
    }

    // ==================== Execution Methods ====================

    /// Insert a new execution record.
    pub async fn insert_execution(&self, execution: &NewExecution) -> Result<i64> {
        let result = sqlx::query(queries::INSERT_EXECUTION)
            .bind(execution.opportunity_id)
            .bind(&execution.tx_hash)
            .bind(execution.status.as_str())
            .bind(&execution.gas_price_wei)
            .bind(execution.gas_limit)
            .bind(execution.submitted_at)
            .execute(&self.pool)
            .await?;

        Ok(result.last_insert_rowid())
    }

    /// Get an execution by transaction hash.
    pub async fn get_execution(&self, tx_hash: &str) -> Result<Execution> {
        sqlx::query_as::<_, Execution>(queries::GET_EXECUTION_BY_TX)
            .bind(tx_hash)
            .fetch_optional(&self.pool)
            .await?
            .ok_or_else(|| StorageError::NotFound(format!("Execution with tx_hash {}", tx_hash)))
    }

    /// Get all executions for an opportunity.
    pub async fn get_executions_for_opportunity(
        &self,
        opportunity_id: i64,
    ) -> Result<Vec<Execution>> {
        let executions = sqlx::query_as::<_, Execution>(queries::GET_EXECUTIONS_FOR_OPPORTUNITY)
            .bind(opportunity_id)
            .fetch_all(&self.pool)
            .await?;

        Ok(executions)
    }

    /// Get recent executions with pagination.
    pub async fn get_executions(&self, limit: i64, offset: i64) -> Result<Vec<Execution>> {
        let executions = sqlx::query_as::<_, Execution>(queries::GET_RECENT_EXECUTIONS)
            .bind(limit)
            .bind(offset)
            .fetch_all(&self.pool)
            .await?;

        Ok(executions)
    }

    /// Get all pending executions.
    pub async fn get_pending_executions(&self) -> Result<Vec<Execution>> {
        let executions = sqlx::query_as::<_, Execution>(queries::GET_PENDING_EXECUTIONS)
            .fetch_all(&self.pool)
            .await?;

        Ok(executions)
    }

    /// Update execution with confirmation details.
    #[allow(clippy::too_many_arguments)]
    pub async fn update_execution_confirmed(
        &self,
        tx_hash: &str,
        block_number: i64,
        tx_index: i32,
        gas_used: i64,
        gas_cost_wei: &str,
        gross_profit_wei: &str,
        net_profit_wei: &str,
        slippage_bps: i32,
        confirmed_at: i64,
    ) -> Result<()> {
        sqlx::query(queries::UPDATE_EXECUTION_CONFIRMED)
            .bind(block_number)
            .bind(tx_index)
            .bind(gas_used)
            .bind(gas_cost_wei)
            .bind(gross_profit_wei)
            .bind(net_profit_wei)
            .bind(slippage_bps)
            .bind(confirmed_at)
            .bind(tx_hash)
            .execute(&self.pool)
            .await?;

        Ok(())
    }

    /// Update execution with failure details.
    pub async fn update_execution_failed(
        &self,
        tx_hash: &str,
        status: ExecutionStatus,
        error_message: &str,
        confirmed_at: i64,
    ) -> Result<()> {
        sqlx::query(queries::UPDATE_EXECUTION_FAILED)
            .bind(status.as_str())
            .bind(error_message)
            .bind(confirmed_at)
            .bind(tx_hash)
            .execute(&self.pool)
            .await?;

        Ok(())
    }

    // ==================== Statistics Methods ====================

    /// Get opportunity statistics for a time range.
    pub async fn get_opportunity_stats(
        &self,
        from_timestamp: i64,
        to_timestamp: i64,
    ) -> Result<OpportunityStats> {
        let stats = sqlx::query_as::<_, OpportunityStats>(queries::GET_OPPORTUNITY_STATS)
            .bind(from_timestamp)
            .bind(to_timestamp)
            .fetch_one(&self.pool)
            .await?;

        Ok(stats)
    }

    /// Get execution statistics for a time range.
    pub async fn get_execution_stats(
        &self,
        from_timestamp: i64,
        to_timestamp: i64,
    ) -> Result<ExecutionStats> {
        let stats = sqlx::query_as::<_, ExecutionStats>(queries::GET_EXECUTION_STATS)
            .bind(from_timestamp)
            .bind(to_timestamp)
            .fetch_one(&self.pool)
            .await?;

        Ok(stats)
    }

    // ==================== Cleanup Methods ====================

    /// Delete old price snapshots.
    pub async fn cleanup_old_price_snapshots(&self, before_timestamp: i64) -> Result<u64> {
        let result = sqlx::query(queries::DELETE_OLD_PRICE_SNAPSHOTS)
            .bind(before_timestamp)
            .execute(&self.pool)
            .await?;

        Ok(result.rows_affected())
    }

    /// Delete old non-executed opportunities.
    pub async fn cleanup_old_opportunities(&self, before_timestamp: i64) -> Result<u64> {
        let result = sqlx::query(queries::DELETE_OLD_OPPORTUNITIES)
            .bind(before_timestamp)
            .execute(&self.pool)
            .await?;

        Ok(result.rows_affected())
    }

    /// Close the database connection pool.
    pub async fn close(&self) {
        self.pool.close().await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn setup_test_db() -> Database {
        let db = Database::new(":memory:").await.unwrap();
        db.run_migrations().await.unwrap();
        db
    }

    #[tokio::test]
    async fn test_create_database() {
        let db = setup_test_db().await;
        assert!(db.pool().is_closed() == false);
    }

    #[tokio::test]
    async fn test_insert_and_get_opportunity() {
        let db = setup_test_db().await;

        let new_opp = NewOpportunity {
            timestamp: 1700000000000,
            opportunity_type: OpportunityType::PriceDiscrepancy,
            token_pairs: vec!["WETH/USDC".to_string()],
            protocol_venues: vec!["uniswap_v2".to_string(), "sushiswap".to_string()],
            estimated_gross_profit_wei: "1000000000000000000".to_string(),
            estimated_gas_cost_wei: "100000000000000000".to_string(),
            estimated_net_profit_wei: "900000000000000000".to_string(),
            block_number_detected: 18500000,
        };

        let id = db.insert_opportunity(&new_opp).await.unwrap();
        assert!(id > 0);

        let opp = db.get_opportunity(id).await.unwrap();
        assert_eq!(opp.opportunity_type, "price_discrepancy");
        assert_eq!(opp.block_number_detected, 18500000);
    }

    #[tokio::test]
    async fn test_insert_and_get_pool() {
        let db = setup_test_db().await;

        let new_pool = NewPool {
            address: "0x1234567890abcdef1234567890abcdef12345678".to_string(),
            protocol: "uniswap_v2".to_string(),
            token0_address: "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2".to_string(),
            token0_symbol: "WETH".to_string(),
            token0_decimals: 18,
            token1_address: "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48".to_string(),
            token1_symbol: "USDC".to_string(),
            token1_decimals: 6,
            fee_bps: 30,
        };

        let id = db.insert_pool(&new_pool).await.unwrap();
        assert!(id > 0);

        let pool = db.get_pool(&new_pool.address).await.unwrap();
        assert_eq!(pool.protocol, "uniswap_v2");
        assert_eq!(pool.token0_symbol, "WETH");
    }

    #[tokio::test]
    async fn test_insert_and_get_price_snapshot() {
        let db = setup_test_db().await;

        // First insert a pool
        let new_pool = NewPool {
            address: "0x1234567890abcdef1234567890abcdef12345678".to_string(),
            protocol: "uniswap_v2".to_string(),
            token0_address: "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2".to_string(),
            token0_symbol: "WETH".to_string(),
            token0_decimals: 18,
            token1_address: "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48".to_string(),
            token1_symbol: "USDC".to_string(),
            token1_decimals: 6,
            fee_bps: 30,
        };
        db.insert_pool(&new_pool).await.unwrap();

        let snapshot = NewPriceSnapshot {
            pool_address: new_pool.address.clone(),
            token_pair: "WETH/USDC".to_string(),
            price: 2000.5,
            reserve0: "1000000000000000000000".to_string(),
            reserve1: "2000000000000".to_string(),
            block_number: 18500000,
            timestamp: 1700000000000,
            tx_hash: Some("0xabcdef".to_string()),
        };

        let id = db.insert_price_snapshot(&snapshot).await.unwrap();
        assert!(id > 0);

        let latest = db.get_latest_price(&new_pool.address).await.unwrap();
        assert_eq!(latest.price, 2000.5);
    }

    #[tokio::test]
    async fn test_insert_and_get_execution() {
        let db = setup_test_db().await;

        // First insert an opportunity
        let new_opp = NewOpportunity {
            timestamp: 1700000000000,
            opportunity_type: OpportunityType::PriceDiscrepancy,
            token_pairs: vec!["WETH/USDC".to_string()],
            protocol_venues: vec!["uniswap_v2".to_string()],
            estimated_gross_profit_wei: "1000000000000000000".to_string(),
            estimated_gas_cost_wei: "100000000000000000".to_string(),
            estimated_net_profit_wei: "900000000000000000".to_string(),
            block_number_detected: 18500000,
        };
        let opp_id = db.insert_opportunity(&new_opp).await.unwrap();

        let new_exec = NewExecution {
            opportunity_id: opp_id,
            tx_hash: "0x1234567890abcdef".to_string(),
            status: ExecutionStatus::Pending,
            gas_price_wei: "50000000000".to_string(),
            gas_limit: 300000,
            submitted_at: 1700000001000,
        };

        let id = db.insert_execution(&new_exec).await.unwrap();
        assert!(id > 0);

        let exec = db.get_execution(&new_exec.tx_hash).await.unwrap();
        assert_eq!(exec.status, "pending");
        assert_eq!(exec.gas_limit, 300000);
    }
}
