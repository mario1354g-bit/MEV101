//! Database schema constants and migration utilities.
//!
//! This module contains SQL schema definitions and migration queries
//! for the MEV bot SQLite database.

/// Initial database schema migration SQL.
/// Creates all core tables for the MEV monitoring system.
pub const MIGRATION_001_INITIAL: &str = r#"
-- Opportunities table: stores detected MEV opportunities
CREATE TABLE IF NOT EXISTS opportunities (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    timestamp INTEGER NOT NULL,
    opportunity_type TEXT NOT NULL,
    token_pairs TEXT NOT NULL,
    protocol_venues TEXT NOT NULL,
    estimated_gross_profit_wei TEXT NOT NULL,
    estimated_gas_cost_wei TEXT NOT NULL,
    estimated_net_profit_wei TEXT NOT NULL,
    block_number_detected INTEGER NOT NULL,
    block_number_disappeared INTEGER,
    blocks_persisted INTEGER,
    captured_by_competitor INTEGER NOT NULL DEFAULT 0,
    competitor_tx_hash TEXT,
    simulated INTEGER NOT NULL DEFAULT 0,
    simulation_profitable INTEGER,
    simulation_result TEXT,
    executed INTEGER NOT NULL DEFAULT 0,
    execution_tx_hash TEXT,
    execution_profit_wei TEXT,
    created_at TEXT DEFAULT (datetime('now')),
    updated_at TEXT DEFAULT (datetime('now'))
);

-- Pools table: stores DEX liquidity pool information
CREATE TABLE IF NOT EXISTS pools (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    address TEXT NOT NULL UNIQUE,
    protocol TEXT NOT NULL,
    token0_address TEXT NOT NULL,
    token0_symbol TEXT NOT NULL,
    token0_decimals INTEGER NOT NULL,
    token1_address TEXT NOT NULL,
    token1_symbol TEXT NOT NULL,
    token1_decimals INTEGER NOT NULL,
    fee_bps INTEGER NOT NULL DEFAULT 30,
    reserve0 TEXT,
    reserve1 TEXT,
    tvl_usd REAL,
    is_active INTEGER NOT NULL DEFAULT 1,
    last_activity INTEGER,
    last_sync_block INTEGER,
    created_at TEXT DEFAULT (datetime('now')),
    updated_at TEXT DEFAULT (datetime('now'))
);

-- Price snapshots table: historical price data
CREATE TABLE IF NOT EXISTS price_snapshots (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    pool_address TEXT NOT NULL,
    token_pair TEXT NOT NULL,
    price REAL NOT NULL,
    reserve0 TEXT NOT NULL,
    reserve1 TEXT NOT NULL,
    block_number INTEGER NOT NULL,
    timestamp INTEGER NOT NULL,
    tx_hash TEXT,
    created_at TEXT DEFAULT (datetime('now')),
    FOREIGN KEY (pool_address) REFERENCES pools(address)
);

-- Executions table: records of executed MEV trades
CREATE TABLE IF NOT EXISTS executions (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    opportunity_id INTEGER NOT NULL,
    tx_hash TEXT NOT NULL UNIQUE,
    block_number INTEGER,
    tx_index INTEGER,
    status TEXT NOT NULL DEFAULT 'pending',
    gas_price_wei TEXT NOT NULL,
    gas_limit INTEGER NOT NULL,
    gas_used INTEGER,
    gas_cost_wei TEXT,
    gross_profit_wei TEXT,
    net_profit_wei TEXT,
    slippage_bps INTEGER,
    error_message TEXT,
    submitted_at INTEGER NOT NULL,
    confirmed_at INTEGER,
    execution_details TEXT,
    created_at TEXT DEFAULT (datetime('now')),
    updated_at TEXT DEFAULT (datetime('now')),
    FOREIGN KEY (opportunity_id) REFERENCES opportunities(id)
);

-- Indexes for opportunities table
CREATE INDEX IF NOT EXISTS idx_opportunities_timestamp ON opportunities(timestamp);
CREATE INDEX IF NOT EXISTS idx_opportunities_type ON opportunities(opportunity_type);
CREATE INDEX IF NOT EXISTS idx_opportunities_block_detected ON opportunities(block_number_detected);
CREATE INDEX IF NOT EXISTS idx_opportunities_executed ON opportunities(executed);
CREATE INDEX IF NOT EXISTS idx_opportunities_simulated ON opportunities(simulated);
CREATE INDEX IF NOT EXISTS idx_opportunities_competitor ON opportunities(captured_by_competitor);
CREATE INDEX IF NOT EXISTS idx_opportunities_profit ON opportunities(estimated_net_profit_wei);

-- Indexes for pools table
CREATE INDEX IF NOT EXISTS idx_pools_protocol ON pools(protocol);
CREATE INDEX IF NOT EXISTS idx_pools_token0 ON pools(token0_address);
CREATE INDEX IF NOT EXISTS idx_pools_token1 ON pools(token1_address);
CREATE INDEX IF NOT EXISTS idx_pools_active ON pools(is_active);
CREATE INDEX IF NOT EXISTS idx_pools_tvl ON pools(tvl_usd);

-- Composite index for token pair lookups
CREATE INDEX IF NOT EXISTS idx_pools_token_pair ON pools(token0_address, token1_address);

-- Indexes for price_snapshots table
CREATE INDEX IF NOT EXISTS idx_price_snapshots_pool ON price_snapshots(pool_address);
CREATE INDEX IF NOT EXISTS idx_price_snapshots_pair ON price_snapshots(token_pair);
CREATE INDEX IF NOT EXISTS idx_price_snapshots_block ON price_snapshots(block_number);
CREATE INDEX IF NOT EXISTS idx_price_snapshots_timestamp ON price_snapshots(timestamp);

-- Composite index for time-series queries
CREATE INDEX IF NOT EXISTS idx_price_snapshots_pool_time ON price_snapshots(pool_address, timestamp);

-- Indexes for executions table
CREATE INDEX IF NOT EXISTS idx_executions_opportunity ON executions(opportunity_id);
CREATE INDEX IF NOT EXISTS idx_executions_status ON executions(status);
CREATE INDEX IF NOT EXISTS idx_executions_block ON executions(block_number);
CREATE INDEX IF NOT EXISTS idx_executions_submitted ON executions(submitted_at);
CREATE INDEX IF NOT EXISTS idx_executions_confirmed ON executions(confirmed_at);

-- Trigger to update updated_at on opportunities
CREATE TRIGGER IF NOT EXISTS opportunities_updated_at
AFTER UPDATE ON opportunities
BEGIN
    UPDATE opportunities SET updated_at = datetime('now') WHERE id = NEW.id;
END;

-- Trigger to update updated_at on pools
CREATE TRIGGER IF NOT EXISTS pools_updated_at
AFTER UPDATE ON pools
BEGIN
    UPDATE pools SET updated_at = datetime('now') WHERE id = NEW.id;
END;

-- Trigger to update updated_at on executions
CREATE TRIGGER IF NOT EXISTS executions_updated_at
AFTER UPDATE ON executions
BEGIN
    UPDATE executions SET updated_at = datetime('now') WHERE id = NEW.id;
END;
"#;

/// SQL to check if the database is initialized.
pub const CHECK_INITIALIZED: &str = r#"
SELECT COUNT(*) as count FROM sqlite_master
WHERE type='table' AND name='opportunities';
"#;

/// SQL to get database version/migration status.
pub const GET_SCHEMA_VERSION: &str = r#"
SELECT MAX(version) as version FROM schema_migrations;
"#;

/// Schema migrations table creation.
pub const CREATE_MIGRATIONS_TABLE: &str = r#"
CREATE TABLE IF NOT EXISTS schema_migrations (
    version INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    applied_at TEXT DEFAULT (datetime('now'))
);
"#;

/// Insert migration record.
pub const INSERT_MIGRATION: &str = r#"
INSERT INTO schema_migrations (version, name) VALUES (?, ?);
"#;

/// Check if migration was applied.
pub const CHECK_MIGRATION: &str = r#"
SELECT COUNT(*) as count FROM schema_migrations WHERE version = ?;
"#;

/// Migration definitions.
pub struct Migration {
    pub version: i32,
    pub name: &'static str,
    pub sql: &'static str,
}

/// List of all migrations in order.
pub const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "initial_schema",
        sql: MIGRATION_001_INITIAL,
    },
];

/// SQL queries for CRUD operations.
pub mod queries {
    // Opportunity queries
    pub const INSERT_OPPORTUNITY: &str = r#"
        INSERT INTO opportunities (
            timestamp, opportunity_type, token_pairs, protocol_venues,
            estimated_gross_profit_wei, estimated_gas_cost_wei, estimated_net_profit_wei,
            block_number_detected
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)
    "#;

    pub const UPDATE_OPPORTUNITY_SIMULATION: &str = r#"
        UPDATE opportunities
        SET simulated = 1, simulation_profitable = ?, simulation_result = ?
        WHERE id = ?
    "#;

    pub const UPDATE_OPPORTUNITY_EXECUTION: &str = r#"
        UPDATE opportunities
        SET executed = 1, execution_tx_hash = ?, execution_profit_wei = ?
        WHERE id = ?
    "#;

    pub const UPDATE_OPPORTUNITY_COMPETITOR: &str = r#"
        UPDATE opportunities
        SET captured_by_competitor = 1, competitor_tx_hash = ?,
            block_number_disappeared = ?, blocks_persisted = ?
        WHERE id = ?
    "#;

    pub const UPDATE_OPPORTUNITY_DISAPPEARED: &str = r#"
        UPDATE opportunities
        SET block_number_disappeared = ?, blocks_persisted = ?
        WHERE id = ?
    "#;

    pub const GET_OPPORTUNITY_BY_ID: &str = r#"
        SELECT * FROM opportunities WHERE id = ?
    "#;

    pub const GET_RECENT_OPPORTUNITIES: &str = r#"
        SELECT * FROM opportunities
        ORDER BY timestamp DESC
        LIMIT ? OFFSET ?
    "#;

    // Pool queries
    pub const INSERT_POOL: &str = r#"
        INSERT INTO pools (
            address, protocol, token0_address, token0_symbol, token0_decimals,
            token1_address, token1_symbol, token1_decimals, fee_bps
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT(address) DO UPDATE SET
            protocol = excluded.protocol,
            token0_symbol = excluded.token0_symbol,
            token1_symbol = excluded.token1_symbol,
            fee_bps = excluded.fee_bps
    "#;

    pub const UPDATE_POOL_RESERVES: &str = r#"
        UPDATE pools
        SET reserve0 = ?, reserve1 = ?, tvl_usd = ?,
            last_activity = ?, last_sync_block = ?
        WHERE address = ?
    "#;

    pub const UPDATE_POOL_ACTIVITY: &str = r#"
        UPDATE pools
        SET last_activity = ?, last_sync_block = ?
        WHERE address = ?
    "#;

    pub const SET_POOL_ACTIVE: &str = r#"
        UPDATE pools SET is_active = ? WHERE address = ?
    "#;

    pub const GET_POOL_BY_ADDRESS: &str = r#"
        SELECT * FROM pools WHERE address = ?
    "#;

    pub const GET_POOLS_FOR_PAIR: &str = r#"
        SELECT * FROM pools
        WHERE (token0_address = ? AND token1_address = ?)
           OR (token0_address = ? AND token1_address = ?)
        AND is_active = 1
    "#;

    pub const GET_ACTIVE_POOLS: &str = r#"
        SELECT * FROM pools WHERE is_active = 1
    "#;

    pub const GET_POOLS_BY_PROTOCOL: &str = r#"
        SELECT * FROM pools WHERE protocol = ? AND is_active = 1
    "#;

    // Price snapshot queries
    pub const INSERT_PRICE_SNAPSHOT: &str = r#"
        INSERT INTO price_snapshots (
            pool_address, token_pair, price, reserve0, reserve1,
            block_number, timestamp, tx_hash
        ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)
    "#;

    pub const GET_PRICE_HISTORY: &str = r#"
        SELECT * FROM price_snapshots
        WHERE pool_address = ?
        AND timestamp >= ? AND timestamp <= ?
        ORDER BY timestamp DESC
        LIMIT ?
    "#;

    pub const GET_LATEST_PRICE: &str = r#"
        SELECT * FROM price_snapshots
        WHERE pool_address = ?
        ORDER BY timestamp DESC
        LIMIT 1
    "#;

    pub const GET_PRICE_AT_BLOCK: &str = r#"
        SELECT * FROM price_snapshots
        WHERE pool_address = ? AND block_number = ?
        ORDER BY timestamp DESC
        LIMIT 1
    "#;

    // Execution queries
    pub const INSERT_EXECUTION: &str = r#"
        INSERT INTO executions (
            opportunity_id, tx_hash, status, gas_price_wei, gas_limit, submitted_at
        ) VALUES (?, ?, ?, ?, ?, ?)
    "#;

    pub const UPDATE_EXECUTION_CONFIRMED: &str = r#"
        UPDATE executions
        SET status = 'confirmed', block_number = ?, tx_index = ?,
            gas_used = ?, gas_cost_wei = ?, gross_profit_wei = ?,
            net_profit_wei = ?, slippage_bps = ?, confirmed_at = ?
        WHERE tx_hash = ?
    "#;

    pub const UPDATE_EXECUTION_FAILED: &str = r#"
        UPDATE executions
        SET status = ?, error_message = ?, confirmed_at = ?
        WHERE tx_hash = ?
    "#;

    pub const GET_EXECUTION_BY_TX: &str = r#"
        SELECT * FROM executions WHERE tx_hash = ?
    "#;

    pub const GET_EXECUTIONS_FOR_OPPORTUNITY: &str = r#"
        SELECT * FROM executions WHERE opportunity_id = ?
    "#;

    pub const GET_RECENT_EXECUTIONS: &str = r#"
        SELECT * FROM executions
        ORDER BY submitted_at DESC
        LIMIT ? OFFSET ?
    "#;

    pub const GET_PENDING_EXECUTIONS: &str = r#"
        SELECT * FROM executions WHERE status = 'pending'
    "#;

    // Statistics queries
    pub const GET_OPPORTUNITY_STATS: &str = r#"
        SELECT
            COUNT(*) as total_count,
            SUM(CASE WHEN simulated = 1 THEN 1 ELSE 0 END) as simulated_count,
            SUM(CASE WHEN executed = 1 THEN 1 ELSE 0 END) as executed_count,
            SUM(CASE WHEN captured_by_competitor = 1 THEN 1 ELSE 0 END) as competitor_captured_count,
            SUM(CAST(estimated_net_profit_wei AS INTEGER)) as total_estimated_profit_wei,
            SUM(CAST(execution_profit_wei AS INTEGER)) as total_execution_profit_wei
        FROM opportunities
        WHERE timestamp >= ? AND timestamp <= ?
    "#;

    pub const GET_EXECUTION_STATS: &str = r#"
        SELECT
            COUNT(*) as total_count,
            SUM(CASE WHEN status = 'confirmed' THEN 1 ELSE 0 END) as confirmed_count,
            SUM(CASE WHEN status = 'failed' THEN 1 ELSE 0 END) as failed_count,
            SUM(CASE WHEN status = 'frontrun' THEN 1 ELSE 0 END) as frontrun_count,
            SUM(CAST(gas_cost_wei AS INTEGER)) as total_gas_spent_wei,
            SUM(CAST(gross_profit_wei AS INTEGER)) as total_gross_profit_wei,
            SUM(CAST(net_profit_wei AS INTEGER)) as total_net_profit_wei,
            AVG(slippage_bps) as avg_slippage_bps
        FROM executions
        WHERE submitted_at >= ? AND submitted_at <= ?
    "#;

    // Cleanup queries
    pub const DELETE_OLD_PRICE_SNAPSHOTS: &str = r#"
        DELETE FROM price_snapshots WHERE timestamp < ?
    "#;

    pub const DELETE_OLD_OPPORTUNITIES: &str = r#"
        DELETE FROM opportunities WHERE timestamp < ? AND executed = 0
    "#;
}
