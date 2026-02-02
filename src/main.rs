mod config;
mod error;

use std::sync::Arc;

use alloy::providers::{Provider, ProviderBuilder, RootProvider, WsConnect};
use alloy::transports::http::{Client, Http};
use axum::{
    routing::get,
    Router,
    Json,
};
use dashmap::DashMap;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;
use tokio::signal;
use tokio::sync::broadcast;
use tower_http::cors::{Any, CorsLayer};
use tracing::{error, info, warn, Level};
use tracing_subscriber::{fmt, EnvFilter};

use config::Config;
use error::{MevError, ProviderError};

/// Type alias for HTTP provider
type HttpProvider = RootProvider<Http<Client>>;

/// Type alias for WebSocket provider
type WsProvider = RootProvider<alloy::pubsub::PubSubFrontend>;

/// Application state shared across all components
pub struct AppState {
    /// Configuration
    pub config: Config,

    /// SQLite database pool
    pub db: SqlitePool,

    /// HTTP provider for RPC calls
    pub http_provider: Arc<HttpProvider>,

    /// WebSocket provider for subscriptions (optional)
    pub ws_provider: Option<Arc<WsProvider>>,

    /// Shutdown signal broadcaster
    pub shutdown_tx: broadcast::Sender<()>,

    /// Pending transactions cache
    pub pending_txs: Arc<DashMap<String, PendingTransaction>>,

    /// Detected opportunities cache
    pub opportunities: Arc<DashMap<String, MevOpportunity>>,
}

/// Pending transaction data
#[derive(Debug, Clone)]
pub struct PendingTransaction {
    pub hash: String,
    pub from: String,
    pub to: Option<String>,
    pub value: String,
    pub data: Vec<u8>,
    pub gas_price: Option<u128>,
    pub max_fee_per_gas: Option<u128>,
    pub max_priority_fee_per_gas: Option<u128>,
    pub detected_at: chrono::DateTime<chrono::Utc>,
}

/// MEV opportunity data
#[derive(Debug, Clone, serde::Serialize)]
pub struct MevOpportunity {
    pub id: String,
    pub opportunity_type: String,
    pub target_tx: String,
    pub estimated_profit_wei: String,
    pub estimated_gas_cost_wei: String,
    pub net_profit_wei: String,
    pub detected_at: chrono::DateTime<chrono::Utc>,
    pub status: String,
}

#[tokio::main]
async fn main() -> Result<(), MevError> {
    // Load environment variables from .env file
    let _ = dotenv::dotenv();

    // Load configuration
    let config_path = std::env::var("CONFIG_PATH").unwrap_or_else(|_| "config.toml".to_string());
    let config = Config::load(&config_path).map_err(MevError::Config)?;

    // Initialize logging
    init_logging(&config);

    info!("Starting MEV Monitor v{}", env!("CARGO_PKG_VERSION"));
    info!("Chain ID: {}", config.ethereum.chain_id);

    // Initialize database
    let db = init_database(&config).await?;
    info!("Database initialized successfully");

    // Create provider connections
    let http_provider = create_http_provider(&config).await?;
    info!("HTTP provider connected");

    // Verify chain ID
    let chain_id = http_provider.get_chain_id().await.map_err(|e| {
        MevError::Provider(ProviderError::RpcError(e.to_string()))
    })?;

    if chain_id != config.ethereum.chain_id {
        return Err(MevError::Provider(ProviderError::ChainIdMismatch {
            expected: config.ethereum.chain_id,
            actual: chain_id,
        }));
    }
    info!("Chain ID verified: {}", chain_id);

    // Create WebSocket provider (optional)
    let ws_provider = match create_ws_provider(&config).await {
        Ok(provider) => {
            info!("WebSocket provider connected");
            Some(provider)
        }
        Err(e) => {
            warn!("WebSocket provider unavailable: {}. Falling back to HTTP polling.", e);
            None
        }
    };

    // Create shutdown channel
    let (shutdown_tx, _) = broadcast::channel::<()>(1);

    // Create application state
    let app_state = Arc::new(AppState {
        config: config.clone(),
        db,
        http_provider,
        ws_provider,
        shutdown_tx: shutdown_tx.clone(),
        pending_txs: Arc::new(DashMap::new()),
        opportunities: Arc::new(DashMap::new()),
    });

    // Spawn monitoring tasks
    let mut handles = Vec::new();

    // Mempool monitor
    if config.monitoring.mempool_enabled {
        let state = Arc::clone(&app_state);
        let mut shutdown_rx = shutdown_tx.subscribe();
        handles.push(tokio::spawn(async move {
            info!("Starting mempool monitor");
            tokio::select! {
                result = run_mempool_monitor(state) => {
                    if let Err(e) = result {
                        error!("Mempool monitor error: {}", e);
                    }
                }
                _ = shutdown_rx.recv() => {
                    info!("Mempool monitor shutting down");
                }
            }
        }));
    }

    // Block monitor
    if config.monitoring.block_enabled {
        let state = Arc::clone(&app_state);
        let mut shutdown_rx = shutdown_tx.subscribe();
        handles.push(tokio::spawn(async move {
            info!("Starting block monitor");
            tokio::select! {
                result = run_block_monitor(state) => {
                    if let Err(e) = result {
                        error!("Block monitor error: {}", e);
                    }
                }
                _ = shutdown_rx.recv() => {
                    info!("Block monitor shutting down");
                }
            }
        }));
    }

    // DEX monitor
    if config.monitoring.dex_enabled {
        let state = Arc::clone(&app_state);
        let mut shutdown_rx = shutdown_tx.subscribe();
        handles.push(tokio::spawn(async move {
            info!("Starting DEX monitor");
            tokio::select! {
                result = run_dex_monitor(state) => {
                    if let Err(e) = result {
                        error!("DEX monitor error: {}", e);
                    }
                }
                _ = shutdown_rx.recv() => {
                    info!("DEX monitor shutting down");
                }
            }
        }));
    }

    // Detector loop
    {
        let state = Arc::clone(&app_state);
        let mut shutdown_rx = shutdown_tx.subscribe();
        handles.push(tokio::spawn(async move {
            info!("Starting opportunity detector");
            tokio::select! {
                result = run_detector(state) => {
                    if let Err(e) = result {
                        error!("Detector error: {}", e);
                    }
                }
                _ = shutdown_rx.recv() => {
                    info!("Detector shutting down");
                }
            }
        }));
    }

    // Executor (if enabled)
    if config.execution.enabled {
        let state = Arc::clone(&app_state);
        let mut shutdown_rx = shutdown_tx.subscribe();
        handles.push(tokio::spawn(async move {
            info!("Starting executor (dry_run: {})", state.config.execution.dry_run);
            tokio::select! {
                result = run_executor(state) => {
                    if let Err(e) = result {
                        error!("Executor error: {}", e);
                    }
                }
                _ = shutdown_rx.recv() => {
                    info!("Executor shutting down");
                }
            }
        }));
    }

    // Dashboard server
    if config.dashboard.enabled {
        let state = Arc::clone(&app_state);
        let mut shutdown_rx = shutdown_tx.subscribe();
        let dashboard_addr = format!("{}:{}", config.dashboard.host, config.dashboard.port);
        info!("Starting dashboard at http://{}", dashboard_addr);

        handles.push(tokio::spawn(async move {
            tokio::select! {
                result = run_dashboard(state) => {
                    if let Err(e) = result {
                        error!("Dashboard error: {}", e);
                    }
                }
                _ = shutdown_rx.recv() => {
                    info!("Dashboard shutting down");
                }
            }
        }));
    }

    info!("MEV Monitor is running. Press Ctrl+C to stop.");

    // Wait for shutdown signal
    shutdown_signal().await;
    info!("Shutdown signal received");

    // Broadcast shutdown to all tasks
    let _ = shutdown_tx.send(());

    // Wait for all tasks to complete
    for handle in handles {
        let _ = handle.await;
    }

    info!("MEV Monitor stopped gracefully");
    Ok(())
}

/// Initialize logging based on configuration
fn init_logging(config: &Config) {
    let level = match config.logging.level.to_lowercase().as_str() {
        "trace" => Level::TRACE,
        "debug" => Level::DEBUG,
        "info" => Level::INFO,
        "warn" => Level::WARN,
        "error" => Level::ERROR,
        _ => Level::INFO,
    };

    let env_filter = EnvFilter::from_default_env()
        .add_directive(level.into())
        .add_directive("sqlx=warn".parse().unwrap())
        .add_directive("hyper=warn".parse().unwrap())
        .add_directive("reqwest=warn".parse().unwrap());

    // For JSON format, we need to use a different builder approach
    // For now, we'll just use the pretty format for all cases
    // since tracing-subscriber's json() requires additional features
    let subscriber = fmt::Subscriber::builder()
        .with_env_filter(env_filter)
        .with_target(config.logging.include_target)
        .with_ansi(config.logging.colored);

    subscriber.init();
}

/// Initialize SQLite database
async fn init_database(config: &Config) -> Result<SqlitePool, MevError> {
    // Ensure the data directory exists
    if let Some(parent) = std::path::Path::new(&config.database.path).parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            MevError::Database(error::DatabaseError::Sqlx(sqlx::Error::Io(e)))
        })?;
    }

    let connect_options = SqliteConnectOptions::new()
        .filename(&config.database.path)
        .create_if_missing(true)
        .journal_mode(if config.database.enable_wal {
            sqlx::sqlite::SqliteJournalMode::Wal
        } else {
            sqlx::sqlite::SqliteJournalMode::Delete
        });

    let pool = SqlitePoolOptions::new()
        .max_connections(config.database.max_connections)
        .acquire_timeout(std::time::Duration::from_secs(
            config.database.connection_timeout_secs,
        ))
        .connect_with(connect_options)
        .await
        .map_err(error::DatabaseError::Sqlx)?;

    // Run migrations if enabled
    if config.database.run_migrations {
        run_migrations(&pool).await?;
    }

    Ok(pool)
}

/// Run database migrations
async fn run_migrations(pool: &SqlitePool) -> Result<(), MevError> {
    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS opportunities (
            id TEXT PRIMARY KEY,
            opportunity_type TEXT NOT NULL,
            target_tx TEXT NOT NULL,
            estimated_profit_wei TEXT NOT NULL,
            estimated_gas_cost_wei TEXT NOT NULL,
            net_profit_wei TEXT NOT NULL,
            detected_at TEXT NOT NULL,
            executed_at TEXT,
            execution_tx TEXT,
            status TEXT NOT NULL,
            error_message TEXT,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        )
        "#,
    )
    .execute(pool)
    .await
    .map_err(error::DatabaseError::Sqlx)?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS transactions (
            hash TEXT PRIMARY KEY,
            block_number INTEGER,
            from_address TEXT NOT NULL,
            to_address TEXT,
            value TEXT NOT NULL,
            gas_used INTEGER,
            gas_price TEXT,
            status TEXT NOT NULL,
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        )
        "#,
    )
    .execute(pool)
    .await
    .map_err(error::DatabaseError::Sqlx)?;

    sqlx::query(
        r#"
        CREATE TABLE IF NOT EXISTS statistics (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            date TEXT NOT NULL,
            opportunities_detected INTEGER NOT NULL DEFAULT 0,
            opportunities_executed INTEGER NOT NULL DEFAULT 0,
            total_profit_wei TEXT NOT NULL DEFAULT '0',
            total_gas_spent_wei TEXT NOT NULL DEFAULT '0',
            created_at TEXT NOT NULL DEFAULT (datetime('now'))
        )
        "#,
    )
    .execute(pool)
    .await
    .map_err(error::DatabaseError::Sqlx)?;

    sqlx::query(
        r#"
        CREATE INDEX IF NOT EXISTS idx_opportunities_status ON opportunities(status)
        "#,
    )
    .execute(pool)
    .await
    .map_err(error::DatabaseError::Sqlx)?;

    sqlx::query(
        r#"
        CREATE INDEX IF NOT EXISTS idx_opportunities_detected_at ON opportunities(detected_at)
        "#,
    )
    .execute(pool)
    .await
    .map_err(error::DatabaseError::Sqlx)?;

    info!("Database migrations completed");
    Ok(())
}

/// Create HTTP provider
async fn create_http_provider(
    config: &Config,
) -> Result<Arc<HttpProvider>, MevError> {
    let provider = ProviderBuilder::new()
        .on_http(config.ethereum.http_rpc_url.parse().map_err(|e: url::ParseError| {
            MevError::Provider(ProviderError::ConnectionFailed(format!(
                "Invalid HTTP URL: {}",
                e
            )))
        })?);

    Ok(Arc::new(provider))
}

/// Create WebSocket provider
async fn create_ws_provider(
    config: &Config,
) -> Result<Arc<WsProvider>, MevError> {
    let ws_connect = WsConnect::new(&config.ethereum.ws_rpc_url);

    let provider = ProviderBuilder::new()
        .on_ws(ws_connect)
        .await
        .map_err(|e| {
            MevError::Provider(ProviderError::WebSocketError(e.to_string()))
        })?;

    Ok(Arc::new(provider))
}

/// Run mempool monitor
async fn run_mempool_monitor(state: Arc<AppState>) -> Result<(), MevError> {
    let poll_interval = std::time::Duration::from_millis(state.config.monitoring.poll_interval_ms);

    loop {
        // In a real implementation, this would subscribe to pending transactions
        // via WebSocket or poll the txpool_content RPC method
        tokio::time::sleep(poll_interval).await;

        // Placeholder: Log that we're monitoring
        tracing::trace!("Mempool monitor heartbeat");
    }
}

/// Run block monitor
async fn run_block_monitor(state: Arc<AppState>) -> Result<(), MevError> {
    let poll_interval = std::time::Duration::from_millis(state.config.monitoring.poll_interval_ms);
    let mut last_block: Option<u64> = None;

    loop {
        match state.http_provider.get_block_number().await {
            Ok(block_number) => {
                if last_block.map_or(true, |last| block_number > last) {
                    info!("New block: {}", block_number);
                    last_block = Some(block_number);

                    // Clear stale pending transactions
                    state.pending_txs.retain(|_, tx| {
                        let age = chrono::Utc::now()
                            .signed_duration_since(tx.detected_at)
                            .num_seconds();
                        age < 60 // Keep txs younger than 60 seconds
                    });
                }
            }
            Err(e) => {
                warn!("Failed to get block number: {}", e);
            }
        }

        tokio::time::sleep(poll_interval).await;
    }
}

/// Run DEX monitor
async fn run_dex_monitor(state: Arc<AppState>) -> Result<(), MevError> {
    let poll_interval = std::time::Duration::from_millis(state.config.monitoring.poll_interval_ms);

    info!(
        "Monitoring {} DEX routers",
        state.config.monitoring.dex_routers.len()
    );

    loop {
        // In a real implementation, this would:
        // 1. Subscribe to pending transactions
        // 2. Filter for DEX router addresses
        // 3. Decode swap parameters
        // 4. Calculate potential arbitrage/sandwich opportunities
        tokio::time::sleep(poll_interval).await;

        tracing::trace!("DEX monitor heartbeat");
    }
}

/// Run opportunity detector
async fn run_detector(state: Arc<AppState>) -> Result<(), MevError> {
    let poll_interval = std::time::Duration::from_millis(100);

    loop {
        // Process pending transactions and detect opportunities
        let pending_count = state.pending_txs.len();
        let opportunity_count = state.opportunities.len();

        if pending_count > 0 || opportunity_count > 0 {
            tracing::debug!(
                "Detector status: {} pending txs, {} opportunities",
                pending_count,
                opportunity_count
            );
        }

        tokio::time::sleep(poll_interval).await;
    }
}

/// Run executor
async fn run_executor(state: Arc<AppState>) -> Result<(), MevError> {
    let poll_interval = std::time::Duration::from_millis(50);

    loop {
        // Process detected opportunities
        for entry in state.opportunities.iter() {
            let opportunity = entry.value();

            if opportunity.status == "pending" {
                if state.config.execution.dry_run {
                    info!(
                        "Dry run: Would execute opportunity {} with profit {}",
                        opportunity.id, opportunity.net_profit_wei
                    );
                } else {
                    // Real execution would happen here
                    info!("Executing opportunity: {}", opportunity.id);
                }
            }
        }

        tokio::time::sleep(poll_interval).await;
    }
}

/// Run dashboard server
async fn run_dashboard(state: Arc<AppState>) -> Result<(), MevError> {
    let cors = if state.config.dashboard.enable_cors {
        CorsLayer::new()
            .allow_origin(Any)
            .allow_methods(Any)
            .allow_headers(Any)
    } else {
        CorsLayer::new()
    };

    let app_state = Arc::clone(&state);

    let app = Router::new()
        .route("/", get(index_handler))
        .route("/api/status", get({
            let state = Arc::clone(&app_state);
            move || status_handler(Arc::clone(&state))
        }))
        .route("/api/opportunities", get({
            let state = Arc::clone(&app_state);
            move || opportunities_handler(Arc::clone(&state))
        }))
        .route("/api/stats", get({
            let state = Arc::clone(&app_state);
            move || stats_handler(Arc::clone(&state))
        }))
        .layer(cors);

    let addr = format!(
        "{}:{}",
        state.config.dashboard.host, state.config.dashboard.port
    );

    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .map_err(|e| MevError::Provider(ProviderError::ConnectionFailed(e.to_string())))?;

    axum::serve(listener, app)
        .await
        .map_err(|e| MevError::Provider(ProviderError::RpcError(e.to_string())))?;

    Ok(())
}

/// Index handler
async fn index_handler() -> &'static str {
    "MEV Monitor Dashboard - API endpoints: /api/status, /api/opportunities, /api/stats"
}

/// Status handler
async fn status_handler(state: Arc<AppState>) -> Json<serde_json::Value> {
    let block_number = state
        .http_provider
        .get_block_number()
        .await
        .unwrap_or(0);

    Json(serde_json::json!({
        "status": "running",
        "chain_id": state.config.ethereum.chain_id,
        "current_block": block_number,
        "pending_transactions": state.pending_txs.len(),
        "active_opportunities": state.opportunities.len(),
        "execution_enabled": state.config.execution.enabled,
        "dry_run": state.config.execution.dry_run
    }))
}

/// Opportunities handler
async fn opportunities_handler(state: Arc<AppState>) -> Json<serde_json::Value> {
    let opportunities: Vec<MevOpportunity> = state
        .opportunities
        .iter()
        .map(|entry| entry.value().clone())
        .collect();

    Json(serde_json::json!({
        "count": opportunities.len(),
        "opportunities": opportunities
    }))
}

/// Stats handler
async fn stats_handler(state: Arc<AppState>) -> Json<serde_json::Value> {
    // Query database for statistics
    let result = sqlx::query_as::<_, (i64, i64, String, String)>(
        r#"
        SELECT
            COALESCE(SUM(opportunities_detected), 0) as detected,
            COALESCE(SUM(opportunities_executed), 0) as executed,
            COALESCE(SUM(CAST(total_profit_wei AS INTEGER)), 0) as profit,
            COALESCE(SUM(CAST(total_gas_spent_wei AS INTEGER)), 0) as gas
        FROM statistics
        WHERE date >= date('now', '-7 days')
        "#,
    )
    .fetch_optional(&state.db)
    .await;

    match result {
        Ok(Some((detected, executed, profit, gas))) => {
            Json(serde_json::json!({
                "period": "7_days",
                "opportunities_detected": detected,
                "opportunities_executed": executed,
                "total_profit_wei": profit,
                "total_gas_spent_wei": gas
            }))
        }
        _ => {
            Json(serde_json::json!({
                "period": "7_days",
                "opportunities_detected": 0,
                "opportunities_executed": 0,
                "total_profit_wei": "0",
                "total_gas_spent_wei": "0"
            }))
        }
    }
}

/// Wait for shutdown signal (SIGINT or SIGTERM)
async fn shutdown_signal() {
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("Failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("Failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}
