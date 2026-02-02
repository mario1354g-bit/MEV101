-- MEV Bot Initial Schema Migration
-- Creates all core tables for the monitoring system
-- Version: 001
-- Date: 2024

-- ============================================================================
-- Schema Migrations Tracking Table
-- ============================================================================

CREATE TABLE IF NOT EXISTS schema_migrations (
    version INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    applied_at TEXT DEFAULT (datetime('now'))
);

-- ============================================================================
-- Opportunities Table
-- Stores detected MEV opportunities
-- ============================================================================

CREATE TABLE IF NOT EXISTS opportunities (
    -- Primary key
    id INTEGER PRIMARY KEY AUTOINCREMENT,

    -- Timing information
    timestamp INTEGER NOT NULL,                      -- Unix timestamp in milliseconds

    -- Opportunity classification
    opportunity_type TEXT NOT NULL,                  -- Type: price_discrepancy, multi_hop, sandwich, etc.

    -- Tokens and venues involved
    token_pairs TEXT NOT NULL,                       -- JSON array of token pairs
    protocol_venues TEXT NOT NULL,                   -- JSON array of DEX/protocol names

    -- Profit estimations (stored as strings for precision)
    estimated_gross_profit_wei TEXT NOT NULL,        -- Gross profit before gas
    estimated_gas_cost_wei TEXT NOT NULL,            -- Estimated gas cost
    estimated_net_profit_wei TEXT NOT NULL,          -- Net profit after gas

    -- Block tracking
    block_number_detected INTEGER NOT NULL,          -- Block where opportunity was found
    block_number_disappeared INTEGER,                -- Block where opportunity ended
    blocks_persisted INTEGER,                        -- Number of blocks opportunity lasted

    -- Competitor tracking
    captured_by_competitor INTEGER NOT NULL DEFAULT 0,  -- Boolean flag
    competitor_tx_hash TEXT,                         -- Competitor's transaction hash

    -- Simulation results
    simulated INTEGER NOT NULL DEFAULT 0,            -- Boolean: was simulation run?
    simulation_profitable INTEGER,                   -- Boolean: was simulation profitable?
    simulation_result TEXT,                          -- JSON blob with simulation details

    -- Execution results
    executed INTEGER NOT NULL DEFAULT 0,             -- Boolean: was execution attempted?
    execution_tx_hash TEXT,                          -- Our transaction hash
    execution_profit_wei TEXT,                       -- Actual profit achieved

    -- Timestamps
    created_at TEXT DEFAULT (datetime('now')),
    updated_at TEXT DEFAULT (datetime('now'))
);

-- ============================================================================
-- Pools Table
-- Stores DEX liquidity pool information
-- ============================================================================

CREATE TABLE IF NOT EXISTS pools (
    -- Primary key
    id INTEGER PRIMARY KEY AUTOINCREMENT,

    -- Pool identification
    address TEXT NOT NULL UNIQUE,                    -- Pool contract address
    protocol TEXT NOT NULL,                          -- DEX name (uniswap_v2, sushiswap, etc.)

    -- Token 0 information
    token0_address TEXT NOT NULL,                    -- Token 0 contract address
    token0_symbol TEXT NOT NULL,                     -- Token 0 symbol (e.g., WETH)
    token0_decimals INTEGER NOT NULL,                -- Token 0 decimal places

    -- Token 1 information
    token1_address TEXT NOT NULL,                    -- Token 1 contract address
    token1_symbol TEXT NOT NULL,                     -- Token 1 symbol (e.g., USDC)
    token1_decimals INTEGER NOT NULL,                -- Token 1 decimal places

    -- Pool parameters
    fee_bps INTEGER NOT NULL DEFAULT 30,             -- Fee in basis points (30 = 0.30%)

    -- Current state
    reserve0 TEXT,                                   -- Current reserve of token0 (string for precision)
    reserve1 TEXT,                                   -- Current reserve of token1
    tvl_usd REAL,                                    -- Total value locked in USD

    -- Activity tracking
    is_active INTEGER NOT NULL DEFAULT 1,            -- Boolean: is pool being monitored?
    last_activity INTEGER,                           -- Unix timestamp of last activity
    last_sync_block INTEGER,                         -- Block number of last sync

    -- Timestamps
    created_at TEXT DEFAULT (datetime('now')),
    updated_at TEXT DEFAULT (datetime('now'))
);

-- ============================================================================
-- Price Snapshots Table
-- Historical price data for pools
-- ============================================================================

CREATE TABLE IF NOT EXISTS price_snapshots (
    -- Primary key
    id INTEGER PRIMARY KEY AUTOINCREMENT,

    -- Pool reference
    pool_address TEXT NOT NULL,                      -- Pool contract address
    token_pair TEXT NOT NULL,                        -- Human-readable pair (e.g., WETH/USDC)

    -- Price data
    price REAL NOT NULL,                             -- Price of token0 in terms of token1
    reserve0 TEXT NOT NULL,                          -- Reserve of token0 at snapshot
    reserve1 TEXT NOT NULL,                          -- Reserve of token1 at snapshot

    -- Block and time
    block_number INTEGER NOT NULL,                   -- Block number of snapshot
    timestamp INTEGER NOT NULL,                      -- Unix timestamp in milliseconds
    tx_hash TEXT,                                    -- Transaction that triggered update (optional)

    -- Timestamps
    created_at TEXT DEFAULT (datetime('now')),

    -- Foreign key constraint
    FOREIGN KEY (pool_address) REFERENCES pools(address)
);

-- ============================================================================
-- Executions Table
-- Records of executed MEV trades
-- ============================================================================

CREATE TABLE IF NOT EXISTS executions (
    -- Primary key
    id INTEGER PRIMARY KEY AUTOINCREMENT,

    -- Reference to opportunity
    opportunity_id INTEGER NOT NULL,                 -- Related opportunity ID

    -- Transaction details
    tx_hash TEXT NOT NULL UNIQUE,                    -- Transaction hash
    block_number INTEGER,                            -- Block where included
    tx_index INTEGER,                                -- Position in block

    -- Status
    status TEXT NOT NULL DEFAULT 'pending',          -- pending, confirmed, failed, frontrun, timeout

    -- Gas details
    gas_price_wei TEXT NOT NULL,                     -- Gas price used (wei)
    gas_limit INTEGER NOT NULL,                      -- Gas limit set
    gas_used INTEGER,                                -- Actual gas consumed
    gas_cost_wei TEXT,                               -- Total gas cost (wei)

    -- Profit details
    gross_profit_wei TEXT,                           -- Gross profit (wei)
    net_profit_wei TEXT,                             -- Net profit after gas (wei)
    slippage_bps INTEGER,                            -- Actual slippage in basis points

    -- Error handling
    error_message TEXT,                              -- Error message if failed

    -- Timing
    submitted_at INTEGER NOT NULL,                   -- Unix timestamp when submitted
    confirmed_at INTEGER,                            -- Unix timestamp when confirmed

    -- Additional details
    execution_details TEXT,                          -- JSON blob with extra details

    -- Timestamps
    created_at TEXT DEFAULT (datetime('now')),
    updated_at TEXT DEFAULT (datetime('now')),

    -- Foreign key constraint
    FOREIGN KEY (opportunity_id) REFERENCES opportunities(id)
);

-- ============================================================================
-- Indexes for Opportunities Table
-- ============================================================================

-- Time-based queries
CREATE INDEX IF NOT EXISTS idx_opportunities_timestamp
    ON opportunities(timestamp);

-- Filter by type
CREATE INDEX IF NOT EXISTS idx_opportunities_type
    ON opportunities(opportunity_type);

-- Block number queries
CREATE INDEX IF NOT EXISTS idx_opportunities_block_detected
    ON opportunities(block_number_detected);

-- Execution status queries
CREATE INDEX IF NOT EXISTS idx_opportunities_executed
    ON opportunities(executed);

-- Simulation status queries
CREATE INDEX IF NOT EXISTS idx_opportunities_simulated
    ON opportunities(simulated);

-- Competitor tracking queries
CREATE INDEX IF NOT EXISTS idx_opportunities_competitor
    ON opportunities(captured_by_competitor);

-- Profit ranking
CREATE INDEX IF NOT EXISTS idx_opportunities_profit
    ON opportunities(estimated_net_profit_wei);

-- Composite: time range with type filter
CREATE INDEX IF NOT EXISTS idx_opportunities_type_time
    ON opportunities(opportunity_type, timestamp);

-- Composite: unexecuted opportunities by profit
CREATE INDEX IF NOT EXISTS idx_opportunities_pending_profit
    ON opportunities(executed, simulated, estimated_net_profit_wei);

-- ============================================================================
-- Indexes for Pools Table
-- ============================================================================

-- Protocol filtering
CREATE INDEX IF NOT EXISTS idx_pools_protocol
    ON pools(protocol);

-- Token lookups
CREATE INDEX IF NOT EXISTS idx_pools_token0
    ON pools(token0_address);

CREATE INDEX IF NOT EXISTS idx_pools_token1
    ON pools(token1_address);

-- Active pool filtering
CREATE INDEX IF NOT EXISTS idx_pools_active
    ON pools(is_active);

-- TVL ranking
CREATE INDEX IF NOT EXISTS idx_pools_tvl
    ON pools(tvl_usd);

-- Composite: token pair lookups (both directions)
CREATE INDEX IF NOT EXISTS idx_pools_token_pair
    ON pools(token0_address, token1_address);

CREATE INDEX IF NOT EXISTS idx_pools_token_pair_reverse
    ON pools(token1_address, token0_address);

-- Composite: active pools by protocol
CREATE INDEX IF NOT EXISTS idx_pools_protocol_active
    ON pools(protocol, is_active);

-- ============================================================================
-- Indexes for Price Snapshots Table
-- ============================================================================

-- Pool-based queries
CREATE INDEX IF NOT EXISTS idx_price_snapshots_pool
    ON price_snapshots(pool_address);

-- Token pair queries
CREATE INDEX IF NOT EXISTS idx_price_snapshots_pair
    ON price_snapshots(token_pair);

-- Block number queries
CREATE INDEX IF NOT EXISTS idx_price_snapshots_block
    ON price_snapshots(block_number);

-- Time-based queries
CREATE INDEX IF NOT EXISTS idx_price_snapshots_timestamp
    ON price_snapshots(timestamp);

-- Composite: time-series queries for specific pool
CREATE INDEX IF NOT EXISTS idx_price_snapshots_pool_time
    ON price_snapshots(pool_address, timestamp);

-- Composite: block-based queries for specific pool
CREATE INDEX IF NOT EXISTS idx_price_snapshots_pool_block
    ON price_snapshots(pool_address, block_number);

-- ============================================================================
-- Indexes for Executions Table
-- ============================================================================

-- Opportunity reference
CREATE INDEX IF NOT EXISTS idx_executions_opportunity
    ON executions(opportunity_id);

-- Status filtering
CREATE INDEX IF NOT EXISTS idx_executions_status
    ON executions(status);

-- Block number queries
CREATE INDEX IF NOT EXISTS idx_executions_block
    ON executions(block_number);

-- Submission time queries
CREATE INDEX IF NOT EXISTS idx_executions_submitted
    ON executions(submitted_at);

-- Confirmation time queries
CREATE INDEX IF NOT EXISTS idx_executions_confirmed
    ON executions(confirmed_at);

-- Composite: pending executions by submission time
CREATE INDEX IF NOT EXISTS idx_executions_pending_time
    ON executions(status, submitted_at) WHERE status = 'pending';

-- Composite: confirmed executions by profit
CREATE INDEX IF NOT EXISTS idx_executions_confirmed_profit
    ON executions(status, net_profit_wei) WHERE status = 'confirmed';

-- ============================================================================
-- Triggers for Updated Timestamps
-- ============================================================================

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

-- ============================================================================
-- Views for Common Queries
-- ============================================================================

-- View: Active opportunities (detected but not yet captured/executed)
CREATE VIEW IF NOT EXISTS v_active_opportunities AS
SELECT *
FROM opportunities
WHERE executed = 0
  AND captured_by_competitor = 0
  AND block_number_disappeared IS NULL
ORDER BY estimated_net_profit_wei DESC;

-- View: Profitable executions
CREATE VIEW IF NOT EXISTS v_profitable_executions AS
SELECT
    e.*,
    o.opportunity_type,
    o.token_pairs,
    o.protocol_venues,
    o.estimated_net_profit_wei as estimated_profit
FROM executions e
JOIN opportunities o ON e.opportunity_id = o.id
WHERE e.status = 'confirmed'
  AND CAST(e.net_profit_wei AS INTEGER) > 0
ORDER BY e.confirmed_at DESC;

-- View: Pool summary with latest prices
CREATE VIEW IF NOT EXISTS v_pool_summary AS
SELECT
    p.*,
    (
        SELECT ps.price
        FROM price_snapshots ps
        WHERE ps.pool_address = p.address
        ORDER BY ps.timestamp DESC
        LIMIT 1
    ) as latest_price,
    (
        SELECT ps.timestamp
        FROM price_snapshots ps
        WHERE ps.pool_address = p.address
        ORDER BY ps.timestamp DESC
        LIMIT 1
    ) as latest_price_timestamp
FROM pools p
WHERE p.is_active = 1;

-- View: Daily statistics
CREATE VIEW IF NOT EXISTS v_daily_stats AS
SELECT
    date(datetime(timestamp/1000, 'unixepoch')) as date,
    COUNT(*) as total_opportunities,
    SUM(CASE WHEN simulated = 1 THEN 1 ELSE 0 END) as simulated_count,
    SUM(CASE WHEN executed = 1 THEN 1 ELSE 0 END) as executed_count,
    SUM(CASE WHEN captured_by_competitor = 1 THEN 1 ELSE 0 END) as competitor_captured,
    AVG(CAST(estimated_net_profit_wei AS REAL)) as avg_estimated_profit,
    MAX(CAST(estimated_net_profit_wei AS INTEGER)) as max_estimated_profit
FROM opportunities
GROUP BY date(datetime(timestamp/1000, 'unixepoch'))
ORDER BY date DESC;

-- ============================================================================
-- Insert initial migration record
-- ============================================================================

INSERT OR IGNORE INTO schema_migrations (version, name)
VALUES (1, 'initial_schema');
