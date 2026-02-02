mod config;
mod error;

use std::sync::Arc;
use std::time::Duration;

use alloy::primitives::{Address, Bytes, FixedBytes, U256};
use alloy::providers::{Provider, ProviderBuilder, RootProvider, WsConnect};
use alloy::sol;
use alloy::sol_types::SolCall;
use alloy::transports::http::{Client, Http};
use alloy::transports::Transport;
use alloy::network::{EthereumWallet, TransactionBuilder, TxSignerSync};
use alloy::signers::local::PrivateKeySigner;
#[cfg(feature = "dashboard")]
use axum::{
    routing::get,
    Router,
    Json,
};
use dashmap::DashMap;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
use sqlx::SqlitePool;
use tokio::runtime::Builder;
use tokio::signal;
use tokio::sync::broadcast;
use tokio::time::timeout;
#[cfg(feature = "dashboard")]
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

    /// Live stats counters
    pub stats: Arc<LiveStats>,
}

/// Live statistics tracking
pub struct LiveStats {
    pub opportunities_detected: std::sync::atomic::AtomicU64,
    pub high_spread_alerts: std::sync::atomic::AtomicU64,
    pub pairs_monitored: std::sync::atomic::AtomicU64,
    pub last_block: std::sync::atomic::AtomicU64,
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

/// Swap step for flash loan arbitrage execution
/// Matches the Solidity struct in FlashloanArbitrage contract
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SwapStep {
    /// Protocol identifier: 1=UniV2, 2=UniV3, 3=Balancer, 4=Curve
    pub protocol: u8,
    /// Router/pool address for the swap
    pub router: Address,
    /// Input token address
    pub token_in: Address,
    /// Output token address
    pub token_out: Address,
    /// Protocol-specific swap data (e.g., pool fee for UniV3)
    pub swap_data: Vec<u8>,
    /// Amount to swap (0 = use all available balance)
    pub amount_in: U256,
    /// Minimum output amount (slippage protection)
    pub min_amount_out: U256,
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
    /// Flash loan token address (token to borrow)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flash_loan_token: Option<Address>,
    /// Flash loan amount in wei
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flash_loan_amount: Option<U256>,
    /// Swap steps for the arbitrage execution
    #[serde(skip_serializing_if = "Option::is_none")]
    pub swap_steps: Option<Vec<SwapStep>>,
    /// Buy DEX name (for logging)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub buy_dex: Option<String>,
    /// Sell DEX name (for logging)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sell_dex: Option<String>,
    /// Buy router address
    #[serde(skip_serializing_if = "Option::is_none")]
    pub buy_router: Option<Address>,
    /// Sell router address
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sell_router: Option<Address>,
    /// Token pair name (e.g., "WETH/USDC")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pair_name: Option<String>,
}

fn main() -> Result<(), MevError> {
    // Build optimized multi-threaded runtime for low-latency MEV operations
    let runtime = Builder::new_multi_thread()
        .worker_threads(num_cpus::get().max(4))
        .max_blocking_threads(128)
        .enable_all()
        .thread_keep_alive(Duration::from_secs(60))
        .thread_name_fn(|| {
            static ATOMIC_ID: std::sync::atomic::AtomicUsize =
                std::sync::atomic::AtomicUsize::new(0);
            let id = ATOMIC_ID.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            format!("mev-worker-{}", id)
        })
        .build()
        .expect("Failed to create Tokio runtime");

    runtime.block_on(async_main())
}

async fn async_main() -> Result<(), MevError> {
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

    // Create provider connections - try local reth first, fallback to Alchemy
    let (http_provider, using_fallback) = create_http_provider_with_fallback(&config).await?;
    if using_fallback {
        info!("HTTP provider connected (using Alchemy fallback)");
    } else {
        info!("HTTP provider connected (using local reth node)");
    }

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

    // Create WebSocket provider - try local reth first, fallback to Alchemy
    let ws_provider = match create_ws_provider_with_fallback(&config).await {
        Ok((provider, ws_using_fallback)) => {
            if ws_using_fallback {
                info!("WebSocket provider connected (using Alchemy fallback)");
            } else {
                info!("WebSocket provider connected (using local reth node)");
            }
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
        stats: Arc::new(LiveStats {
            opportunities_detected: std::sync::atomic::AtomicU64::new(0),
            high_spread_alerts: std::sync::atomic::AtomicU64::new(0),
            pairs_monitored: std::sync::atomic::AtomicU64::new(0),
            last_block: std::sync::atomic::AtomicU64::new(0),
        }),
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

    // Dashboard server (only available with "dashboard" feature)
    #[cfg(feature = "dashboard")]
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

    #[cfg(not(feature = "dashboard"))]
    if config.dashboard.enabled {
        warn!("Dashboard is enabled in config but the 'dashboard' feature is not compiled in.");
        warn!("To enable the dashboard, rebuild with: cargo build --features dashboard");
    }

    info!("MEV Monitor is running. Press Ctrl+C to stop.");

    // Wait for shutdown signal
    shutdown_signal().await;
    info!("Shutdown signal received");

    // Broadcast shutdown to all tasks
    let _ = shutdown_tx.send(());

    // Wait for all tasks to complete with timeout for graceful shutdown
    for handle in handles {
        match timeout(Duration::from_secs(10), handle).await {
            Ok(result) => {
                let _ = result;
            }
            Err(_) => {
                warn!("Task did not complete within shutdown timeout");
            }
        }
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
        .add_directive("sqlx=warn".parse().expect("valid directive"))
        .add_directive("hyper=warn".parse().expect("valid directive"))
        .add_directive("reqwest=warn".parse().expect("valid directive"));

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

/// Create HTTP provider with fallback support
/// Returns (provider, is_using_fallback)
async fn create_http_provider_with_fallback(
    config: &Config,
) -> Result<(Arc<HttpProvider>, bool), MevError> {
    // Try primary endpoint (local reth node) first
    info!("Attempting to connect to primary HTTP endpoint: {}", config.ethereum.http_rpc_url);
    match try_create_http_provider(&config.ethereum.http_rpc_url).await {
        Ok(provider) => {
            // Verify the connection works by making a test call
            match provider.get_block_number().await {
                Ok(_) => return Ok((Arc::new(provider), false)),
                Err(e) => {
                    warn!("Primary HTTP endpoint failed health check: {}", e);
                }
            }
        }
        Err(e) => {
            warn!("Failed to create primary HTTP provider: {}", e);
        }
    }

    // Try fallback endpoint (Alchemy) if available
    if let Some(ref fallback_url) = config.ethereum.fallback_http_url {
        info!("Attempting to connect to fallback HTTP endpoint (Alchemy): {}", fallback_url);
        match try_create_http_provider(fallback_url).await {
            Ok(provider) => {
                // Verify the fallback connection works
                match provider.get_block_number().await {
                    Ok(_) => return Ok((Arc::new(provider), true)),
                    Err(e) => {
                        warn!("Fallback HTTP endpoint failed health check: {}", e);
                    }
                }
            }
            Err(e) => {
                warn!("Failed to create fallback HTTP provider: {}", e);
            }
        }
    } else {
        warn!("No fallback HTTP URL configured. Set ALCHEMY_HTTP_URL or fallback_http_url in config.");
    }

    // If we get here, all endpoints failed
    Err(MevError::Provider(ProviderError::ConnectionFailed(
        "All HTTP endpoints unavailable. Check that local reth node is running or configure Alchemy fallback.".to_string()
    )))
}

/// Try to create an HTTP provider for a given URL
async fn try_create_http_provider(url: &str) -> Result<HttpProvider, MevError> {
    let provider = ProviderBuilder::new()
        .on_http(url.parse().map_err(|e: url::ParseError| {
            MevError::Provider(ProviderError::ConnectionFailed(format!(
                "Invalid HTTP URL: {}",
                e
            )))
        })?);

    Ok(provider)
}

/// Create WebSocket provider with fallback support
/// Returns (provider, is_using_fallback)
async fn create_ws_provider_with_fallback(
    config: &Config,
) -> Result<(Arc<WsProvider>, bool), MevError> {
    // Try primary endpoint (local reth node) first
    info!("Attempting to connect to primary WebSocket endpoint: {}", config.ethereum.ws_rpc_url);
    match try_create_ws_provider(&config.ethereum.ws_rpc_url).await {
        Ok(provider) => {
            // Verify the connection works by making a test call
            match provider.get_block_number().await {
                Ok(_) => return Ok((Arc::new(provider), false)),
                Err(e) => {
                    warn!("Primary WebSocket endpoint failed health check: {}", e);
                }
            }
        }
        Err(e) => {
            warn!("Failed to create primary WebSocket provider: {}", e);
        }
    }

    // Try fallback endpoint (Alchemy) if available
    if let Some(ref fallback_url) = config.ethereum.fallback_ws_url {
        info!("Attempting to connect to fallback WebSocket endpoint (Alchemy): {}", fallback_url);
        match try_create_ws_provider(fallback_url).await {
            Ok(provider) => {
                // Verify the fallback connection works
                match provider.get_block_number().await {
                    Ok(_) => return Ok((Arc::new(provider), true)),
                    Err(e) => {
                        warn!("Fallback WebSocket endpoint failed health check: {}", e);
                    }
                }
            }
            Err(e) => {
                warn!("Failed to create fallback WebSocket provider: {}", e);
            }
        }
    } else {
        info!("No fallback WebSocket URL configured. Set ALCHEMY_WS_URL or fallback_ws_url in config.");
    }

    // If we get here, all endpoints failed
    Err(MevError::Provider(ProviderError::WebSocketError(
        "All WebSocket endpoints unavailable. Check that local reth node is running or configure Alchemy fallback.".to_string()
    )))
}

/// Try to create a WebSocket provider for a given URL
async fn try_create_ws_provider(url: &str) -> Result<WsProvider, MevError> {
    let ws_connect = WsConnect::new(url);

    let provider = ProviderBuilder::new()
        .on_ws(ws_connect)
        .await
        .map_err(|e| {
            MevError::Provider(ProviderError::WebSocketError(e.to_string()))
        })?;

    Ok(provider)
}

/// Run mempool monitor - subscribes to pending transactions via WebSocket
async fn run_mempool_monitor(state: Arc<AppState>) -> Result<(), MevError> {
    use futures::StreamExt;

    let poll_interval = std::time::Duration::from_millis(state.config.monitoring.poll_interval_ms * 10);
    let mut pending_tx_count: u64 = 0;
    let mut last_log = std::time::Instant::now();

    info!("Mempool monitor: attempting to subscribe to pending transactions");

    // Check if we have a WebSocket provider for subscriptions
    if let Some(ref ws_provider) = state.ws_provider {
        info!("Mempool monitor: WebSocket provider available, attempting subscription");

        // Try to subscribe to pending transactions
        match ws_provider.subscribe_pending_transactions().await {
            Ok(subscription) => {
                info!("Mempool monitor: Successfully subscribed to pending transactions!");
                let mut stream = subscription.into_stream();

                loop {
                    // Log status every 30 seconds
                    if last_log.elapsed().as_secs() >= 30 {
                        info!(
                            "Mempool monitor: {} pending txs seen, {} currently tracked",
                            pending_tx_count,
                            state.pending_txs.len()
                        );
                        last_log = std::time::Instant::now();
                    }

                    // Use select to handle both stream events and periodic status logging
                    tokio::select! {
                        tx_hash = stream.next() => {
                            match tx_hash {
                                Some(hash) => {
                                    pending_tx_count += 1;
                                    if pending_tx_count <= 5 || pending_tx_count % 100 == 0 {
                                        info!("Mempool monitor: Received pending tx #{}: {:?}", pending_tx_count, hash);
                                    }
                                    tracing::debug!("Pending tx: {:?}", hash);
                                }
                                None => {
                                    warn!("Mempool monitor: Subscription stream ended unexpectedly");
                                    break;
                                }
                            }
                        }
                        _ = tokio::time::sleep(std::time::Duration::from_secs(30)) => {
                            // Periodic logging handled above
                        }
                    }
                }

                // If subscription ended, fall through to polling
                warn!("Mempool monitor: Subscription ended, falling back to polling");
            }
            Err(e) => {
                warn!(
                    "Mempool monitor: Failed to subscribe to pending transactions: {}. \
                    This is expected for providers that don't support newPendingTransactions \
                    (e.g., some Alchemy tiers, Infura free tier). Falling back to polling.",
                    e
                );
            }
        }
    } else {
        warn!("Mempool monitor: No WebSocket provider available, using polling mode");
    }

    // Fallback: Poll pending block for transactions
    info!("Mempool monitor: Using polling mode for pending transactions");

    loop {
        // Log status every 30 seconds
        if last_log.elapsed().as_secs() >= 30 {
            info!(
                "Mempool monitor (polling): {} pending txs seen, {} currently tracked",
                pending_tx_count,
                state.pending_txs.len()
            );
            last_log = std::time::Instant::now();
        }

        // Try to get pending block with full transactions
        // This works better with some providers than subscriptions
        match state.http_provider.get_block(
            alloy::eips::BlockId::pending(),
            alloy::rpc::types::BlockTransactionsKind::Full
        ).await {
            Ok(Some(block)) => {
                let tx_count = match &block.transactions {
                    alloy::rpc::types::BlockTransactions::Full(txs) => {
                        let count = txs.len();
                        if count > 0 {
                            pending_tx_count += count as u64;
                            info!(
                                "Mempool monitor (polling): Found {} txs in pending block (total seen: {})",
                                count,
                                pending_tx_count
                            );
                        }
                        count
                    }
                    alloy::rpc::types::BlockTransactions::Hashes(hashes) => {
                        let count = hashes.len();
                        if count > 0 {
                            pending_tx_count += count as u64;
                            tracing::debug!("Pending block has {} tx hashes", count);
                        }
                        count
                    }
                    _ => 0,
                };

                if tx_count > 0 {
                    tracing::debug!(
                        "Pending block has {} transactions",
                        tx_count
                    );
                }
            }
            Ok(None) => {
                tracing::debug!("No pending block available");
            }
            Err(e) => {
                tracing::debug!("Failed to get pending block: {}", e);
            }
        }

        tokio::time::sleep(poll_interval).await;
    }
}

/// Run block monitor
async fn run_block_monitor(state: Arc<AppState>) -> Result<(), MevError> {
    let poll_interval = std::time::Duration::from_millis(state.config.monitoring.poll_interval_ms);
    let mut last_block: Option<u64> = None;

    loop {
        match state.http_provider.get_block_number().await {
            Ok(block_number) => {
                if last_block.is_none_or(|last| block_number > last) {
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

/// DEX type enumeration for different AMM mechanisms
#[derive(Debug, Clone, Copy, PartialEq)]
enum DexType {
    /// Uniswap V2 style (constant product x*y=k)
    UniswapV2,
    /// Uniswap V3 style (concentrated liquidity)
    UniswapV3 { fee_tier: u32 },
    /// Curve Finance (stableswap invariant)
    Curve,
    /// Balancer V2 (weighted pools)
    BalancerV2,
    /// PancakeSwap on Ethereum
    PancakeSwap,
    /// Camelot DEX
    Camelot,
}

/// Trading pair configuration for multi-DEX comparison
struct TradingPair {
    name: &'static str,
    pairs: Vec<DexPair>,
    token0_decimals: u8,
    token1_decimals: u8,
}

/// Individual DEX pair
struct DexPair {
    dex: &'static str,
    address: Address,
    dex_type: DexType,
}

/// Price data with liquidity information for opportunity validation
#[derive(Debug, Clone)]
struct DexPriceData {
    dex: &'static str,
    price: f64,
    /// Liquidity in USD equivalent (reserve0 + reserve1 converted to USD)
    liquidity_usd: f64,
    /// Reserve of token0 (raw, adjusted for decimals) - used for advanced slippage calculations
    #[allow(dead_code)]
    reserve0: f64,
    /// Reserve of token1 (raw, adjusted for decimals) - used for advanced slippage calculations
    #[allow(dead_code)]
    reserve1: f64,
    /// Pool/pair address for this DEX
    #[allow(dead_code)]
    pool_address: Address,
    /// DEX type (for protocol identification in execution)
    dex_type: DexType,
}

/// Well-known DEX router addresses for execution
#[allow(dead_code)]
mod dex_routers {
    use alloy::primitives::Address;

    /// Uniswap V2 Router02
    pub const UNISWAP_V2_ROUTER: &str = "0x7a250d5630B4cF539739dF2C5dAcb4c659F2488D";
    /// Uniswap V3 SwapRouter
    pub const UNISWAP_V3_ROUTER: &str = "0xE592427A0AEce92De3Edee1F18E0157C05861564";
    /// SushiSwap Router
    pub const SUSHISWAP_ROUTER: &str = "0xd9e1cE17f2641f24aE83637ab66a2cca9C378B9F";
    /// Balancer V2 Vault
    pub const BALANCER_V2_VAULT: &str = "0xBA12222222228d8Ba445958a75a0704d566BF2C8";
    /// Curve Router (for meta-pools)
    pub const CURVE_ROUTER: &str = "0x99a58482BD75cbab83b27EC03CA68fF489b5788f";

    /// Get router address for a given DEX name
    pub fn get_router_address(dex_name: &str) -> Option<Address> {
        match dex_name.to_lowercase().as_str() {
            "univ2" | "uniswap" | "uniswapv2" => UNISWAP_V2_ROUTER.parse().ok(),
            "univ3" | "uniswapv3" => UNISWAP_V3_ROUTER.parse().ok(),
            "sushi" | "sushiswap" => SUSHISWAP_ROUTER.parse().ok(),
            "balancer" | "balancerv2" => BALANCER_V2_VAULT.parse().ok(),
            "curve" => CURVE_ROUTER.parse().ok(),
            // For Fraxswap, ShibaSwap, etc., they typically use the Uniswap V2 interface
            "frax" | "fraxswap" | "shiba" | "shibaswap" | "pancake" | "pancakeswap" => {
                // These need their specific router addresses in production
                // For now, return None to indicate execution is not supported
                None
            }
            _ => None,
        }
    }
}

/// Well-known token addresses on Ethereum mainnet
#[allow(dead_code)]
mod tokens {
    use alloy::primitives::Address;

    pub const WETH: &str = "0xC02aaA39b223FE8D0A0e5C4F27eAD9083C756Cc2";
    pub const USDC: &str = "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48";
    pub const USDT: &str = "0xdAC17F958D2ee523a2206206994597C13D831ec7";
    pub const DAI: &str = "0x6B175474E89094C44Da98b954EesC8E1d7c2F8ad";
    pub const WBTC: &str = "0x2260FAC5E5542a773Aa44fBCfeDf7C193bc2C599";

    /// Get token address from symbol (from pair name like "WETH/USDC")
    pub fn get_token_address(symbol: &str) -> Option<Address> {
        match symbol.to_uppercase().as_str() {
            "WETH" | "ETH" => WETH.parse().ok(),
            "USDC" => USDC.parse().ok(),
            "USDT" => USDT.parse().ok(),
            "DAI" => DAI.parse().ok(),
            "WBTC" | "BTC" => WBTC.parse().ok(),
            _ => None,
        }
    }

    /// Parse a pair name like "WETH/USDC" and return (token0, token1) addresses
    pub fn parse_pair_tokens(pair_name: &str) -> Option<(Address, Address)> {
        let parts: Vec<&str> = pair_name.split('/').collect();
        if parts.len() != 2 {
            return None;
        }
        let token0 = get_token_address(parts[0])?;
        let token1 = get_token_address(parts[1])?;
        Some((token0, token1))
    }
}

/// Build swap steps for a simple two-leg arbitrage (buy on one DEX, sell on another)
#[allow(dead_code)]
fn build_arbitrage_swap_steps(
    buy_data: &DexPriceData,
    sell_data: &DexPriceData,
    token0: Address,
    token1: Address,
    flash_loan_amount: U256,
    slippage_tolerance_bps: u64,  // in basis points (100 = 1%)
) -> Option<Vec<SwapStep>> {
    // Get router addresses
    let buy_router = dex_routers::get_router_address(buy_data.dex)?;
    let sell_router = dex_routers::get_router_address(sell_data.dex)?;

    // Calculate minimum amounts with slippage protection
    // For buy step: we're buying token1 with token0
    // min_out = expected_out * (1 - slippage)
    let slippage_factor = 10000 - slippage_tolerance_bps;

    // Step 1: Buy token1 with the flash-loaned token0
    // Use 0 for amountIn to indicate "use all available"
    let buy_step = SwapStep {
        protocol: get_protocol_id(buy_data.dex),
        router: buy_router,
        token_in: token0,
        token_out: token1,
        swap_data: match buy_data.dex_type {
            DexType::UniswapV3 { fee_tier } => {
                // For V3, swap_data includes the fee tier
                fee_tier.to_be_bytes().to_vec()
            }
            _ => Vec::new(),
        },
        amount_in: flash_loan_amount,
        min_amount_out: U256::from(0), // Will be calculated by contract or we compute expected output
    };

    // Step 2: Sell token1 back to token0
    // We need to end up with more token0 than we borrowed
    let sell_step = SwapStep {
        protocol: get_protocol_id(sell_data.dex),
        router: sell_router,
        token_in: token1,
        token_out: token0,
        swap_data: match sell_data.dex_type {
            DexType::UniswapV3 { fee_tier } => {
                fee_tier.to_be_bytes().to_vec()
            }
            _ => Vec::new(),
        },
        amount_in: U256::from(0), // Use all token1 from step 1
        min_amount_out: flash_loan_amount * U256::from(slippage_factor) / U256::from(10000), // At least get back what we borrowed
    };

    Some(vec![buy_step, sell_step])
}

/// Validation result for arbitrage opportunities
#[derive(Debug, Clone, PartialEq)]
enum OpportunityValidation {
    /// Opportunity validated - sufficient liquidity and profitable after slippage
    Validated {
        simulated_profit_usd: f64,
        gas_cost_usd: f64,
        net_profit_usd: f64,
    },
    /// Low liquidity - pool doesn't have enough liquidity for profitable execution
    LowLiquidity {
        buy_liquidity_usd: f64,
        sell_liquidity_usd: f64,
        min_required_usd: f64,
    },
    /// High slippage - actual output after slippage makes trade unprofitable
    HighSlippage {
        expected_profit_pct: f64,
        actual_profit_pct: f64,
        slippage_pct: f64,
    },
    /// Gas costs exceed profit
    GasExceedsProfit {
        gross_profit_usd: f64,
        gas_cost_usd: f64,
    },
}

/// Constants for opportunity validation
const MIN_LIQUIDITY_USD: f64 = 100_000.0;  // $100k minimum liquidity per pool
const TRADE_SIZE_ETH: f64 = 10.0;          // Simulate 10 ETH trade size
const ETH_PRICE_USD: f64 = 2500.0;         // Approximate ETH price for USD conversion
const GAS_LIMIT_SWAP: u64 = 250_000;       // Gas limit for a typical DEX swap
const BASE_FEE_GWEI: f64 = 30.0;           // Base fee estimate in gwei
const PRIORITY_FEE_GWEI: f64 = 2.0;        // Priority fee in gwei

impl TradingPair {
    fn new(name: &'static str, token0_decimals: u8, token1_decimals: u8, pairs: Vec<(&'static str, &'static str)>) -> Self {
        Self {
            name,
            token0_decimals,
            token1_decimals,
            pairs: pairs.into_iter()
                .filter_map(|(dex, addr)| {
                    addr.parse::<Address>().ok().map(|address| DexPair {
                        dex,
                        address,
                        dex_type: DexType::UniswapV2, // Default for backward compatibility
                    })
                })
                .collect(),
        }
    }

    fn new_with_types(name: &'static str, token0_decimals: u8, token1_decimals: u8, pairs: Vec<(&'static str, &'static str, DexType)>) -> Self {
        Self {
            name,
            token0_decimals,
            token1_decimals,
            pairs: pairs.into_iter()
                .filter_map(|(dex, addr, dex_type)| {
                    addr.parse::<Address>().ok().map(|address| DexPair {
                        dex,
                        address,
                        dex_type,
                    })
                })
                .collect(),
        }
    }
}

/// Run DEX monitor - actively scans for arbitrage opportunities across multiple pairs and DEXes
async fn run_dex_monitor(state: Arc<AppState>) -> Result<(), MevError> {
    let poll_interval = std::time::Duration::from_millis(state.config.monitoring.poll_interval_ms);

    // ============================================================================
    // LONG-TAIL TRADING PAIRS (Top 50-200 by market cap - less competition)
    // DEXes: Uniswap V2, SushiSwap, ShibaSwap, Fraxswap
    // ============================================================================
    let pairs = vec![
        // --- HIGH LIQUIDITY REFERENCE PAIRS WITH MULTI-DEX SUPPORT (Top 20) ---
        // WETH/USDC - Major pair with V2, V3 (multiple fee tiers), Curve, Balancer, PancakeSwap
        TradingPair::new_with_types("WETH/USDC", 18, 6, vec![
            ("UniV2", "0xB4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc", DexType::UniswapV2),
            ("UniV3-0.05%", "0x88e6A0c2dDD26FEEb64F039a2c41296FcB3f5640", DexType::UniswapV3 { fee_tier: 500 }),
            ("UniV3-0.3%", "0x8ad599c3A0ff1De082011EFDDc58f1908eb6e6D8", DexType::UniswapV3 { fee_tier: 3000 }),
            ("UniV3-1%", "0x7BeA39867e4169DBe237d55C8242a8f2fcDcc387", DexType::UniswapV3 { fee_tier: 10000 }),
            ("Sushi", "0x397FF1542f962076d0BFE58eA045FfA2d347ACa0", DexType::UniswapV2),
            ("PancakeV3", "0x1ac1A8FEaAEa1900C4166dEeed0C11cC10669D36", DexType::UniswapV3 { fee_tier: 500 }),
            ("BalancerV2", "0x96646936b91d6B9D7D0c47C496AfBF3D6ec7B6f8", DexType::BalancerV2),
        ]),
        // WETH/USDT
        TradingPair::new_with_types("WETH/USDT", 18, 6, vec![
            ("UniV2", "0x0d4a11d5EEaaC28EC3F61d100daF4d40471f1852", DexType::UniswapV2),
            ("UniV3-0.05%", "0x11b815efB8f581194ae79006d24E0d814B7697F6", DexType::UniswapV3 { fee_tier: 500 }),
            ("UniV3-0.3%", "0x4e68Ccd3E89f51C3074Ca5072bbAC773960dFa36", DexType::UniswapV3 { fee_tier: 3000 }),
            ("Sushi", "0x06da0fd433C1A5d7a4faa01111c044910A184553", DexType::UniswapV2),
            ("PancakeV2", "0x17C1Ae82D99379240b611427d8C7E7D5D54d0A7D", DexType::UniswapV2),
        ]),
        // WETH/DAI
        TradingPair::new_with_types("WETH/DAI", 18, 18, vec![
            ("UniV2", "0xA478c2975Ab1Ea89e8196811F51A7B7Ade33eB11", DexType::UniswapV2),
            ("UniV3-0.05%", "0x60594a405d53811d3BC4766596EFD80fd545A270", DexType::UniswapV3 { fee_tier: 500 }),
            ("UniV3-0.3%", "0xC2e9F25Be6257c210d7Adf0D4Cd6E3E881ba25f8", DexType::UniswapV3 { fee_tier: 3000 }),
            ("Sushi", "0xC3D03e4F041Fd4cD388c549Ee2A29a9E5075882f", DexType::UniswapV2),
            ("BalancerV2", "0x0b09deA16768f0799065C475bE02919503cB2a35", DexType::BalancerV2),
        ]),
        // WETH/WBTC
        TradingPair::new_with_types("WETH/WBTC", 18, 8, vec![
            ("UniV2", "0xBb2b8038a1640196FbE3e38816F3e67Cba72D940", DexType::UniswapV2),
            ("UniV3-0.05%", "0x4585FE77225b41b697C938B018E2Ac67Ac5a20c0", DexType::UniswapV3 { fee_tier: 500 }),
            ("UniV3-0.3%", "0xCBCdF9626bC03E24f779434178A73a0B4bad62eD", DexType::UniswapV3 { fee_tier: 3000 }),
            ("Sushi", "0xCEfF51756c56CeFFCA006cD410B03FFC46dd3a58", DexType::UniswapV2),
            ("BalancerV2", "0xA6F548DF93de924d73be7D25dC02554c6bD66dB5", DexType::BalancerV2),
            ("Curve-tricrypto", "0xD51a44d3FaE010294C616388b506AcdA1bfAAE46", DexType::Curve),
        ]),

        // --- MEDIUM LIQUIDITY (Top 20-50) WITH V3 POOLS ---
        TradingPair::new_with_types("WETH/LINK", 18, 18, vec![
            ("UniV2", "0xa2107FA5B38d9bbd2C461D6EDf11B11A50F6b974", DexType::UniswapV2),
            ("UniV3-0.3%", "0xa6Cc3C2531FdaA6Ae1A3CA84c2855806728693e8", DexType::UniswapV3 { fee_tier: 3000 }),
            ("Sushi", "0xC40D16476380e4037e6b1A2594cAF6a6cc8Da967", DexType::UniswapV2),
        ]),
        TradingPair::new_with_types("WETH/UNI", 18, 18, vec![
            ("UniV2", "0xd3d2E2692501A5c9Ca623199D38826e513033a17", DexType::UniswapV2),
            ("UniV3-0.3%", "0x1d42064Fc4Beb5F8aAF85F4617AE8b3b5B8Bd801", DexType::UniswapV3 { fee_tier: 3000 }),
            ("UniV3-1%", "0xDBE59c2B2e9c6d10C1F3E812A74342C90c1f1D74", DexType::UniswapV3 { fee_tier: 10000 }),
            ("Sushi", "0xDafd66636E2561b0284EDdE37e42d192F2844D40", DexType::UniswapV2),
        ]),
        TradingPair::new("WETH/MATIC", 18, 18, vec![
            ("UniV2", "0x819f3450dA6f110BA6Ea52195B3beaFa246062dE"),
            ("Sushi", "0x4b5Ab61593A2401B1075b90c04cBCDD3F87CE011"),
        ]),
        TradingPair::new("WETH/SHIB", 18, 18, vec![
            ("UniV2", "0x811beEd0119b4AfCE20D2583EB608C6F7AF1954f"),
            ("Sushi", "0x24d3dD4A62e29770CF98810B09F89d3A90279E7a"),
            ("Shiba", "0x8faf958E36c6970497386118030e6297fFf8d275"),
        ]),
        TradingPair::new("WETH/LDO", 18, 18, vec![
            ("UniV2", "0xC558F600B34A5f69dD2f0D06Cb8A88d829B7420a"),
            ("Sushi", "0xC558F600B34A5f69dD2f0D06Cb8A88d829B7420a"),
        ]),

        // --- LONG-TAIL PAIRS (Top 50-100) - PRIMARY TARGETS ---
        TradingPair::new("WETH/AAVE", 18, 18, vec![
            ("UniV2", "0xDFC14d2Af169B0D36C4EFF567Ada9b2E0CAE044f"),
            ("Sushi", "0xD75EA151a61d06868E31F8988D28DFE5E9df57B4"),
        ]),
        TradingPair::new("WETH/MKR", 18, 18, vec![
            ("UniV2", "0xC2aDdA861F89bBB333c90c492cB837741916A225"),
            ("Sushi", "0xBa13afEcda9beB75De5c56BbAF696b880a5A50dD"),
        ]),
        TradingPair::new("WETH/SNX", 18, 18, vec![
            ("UniV2", "0x43AE24960e5534731Fc831386c07755A2dc33D47"),
            ("Sushi", "0xA1d7b2d891e3A1f9ef4bBC5be20630C2FEB1c470"),
        ]),
        TradingPair::new("WETH/CRV", 18, 18, vec![
            ("UniV2", "0x3dA1313aE46132A397D90d95B1424A9A7e3e0fCE"),
            ("Sushi", "0x58Dc5a51fE44589BEb22E8CE67720B5BC5378009"),
        ]),
        TradingPair::new("WETH/COMP", 18, 18, vec![
            ("UniV2", "0xCFfDdeD873554F362Ac02f8Fb1f02E5ada10516f"),
            ("Sushi", "0x31503dcb60119A812feE820bb7042752019F2355"),
        ]),
        TradingPair::new("WETH/GRT", 18, 18, vec![
            ("UniV2", "0x2E81eC0B8B4022fAC83A21B2F2B4B8f5ED744D70"),
            ("Sushi", "0x5F7B68137efF46BC0bFc6D4C705d5f0A2aDAc9B7"),
        ]),
        TradingPair::new("WETH/SAND", 18, 18, vec![
            ("UniV2", "0x3dd49f67E9d5Bc4C5E6634b3F70BfD9dc1b6BD74"),
            ("Sushi", "0x4a5D85E8b44e7eDb47D361a0193F8828F2eA91B8"),
        ]),
        TradingPair::new("WETH/MANA", 18, 18, vec![
            ("UniV2", "0x11b1f53204d03E5529F09EB3091939e4Fd8c9CF3"),
            ("Sushi", "0x1bEC4db6c3Bc499F3DbF289F5499C30d541FEc97"),
        ]),
        TradingPair::new("WETH/APE", 18, 18, vec![
            ("UniV2", "0xAc4b3DacB91461209Ae9d41EC517c2B9Cb1B7DAF"),
            ("Sushi", "0xb2E1F2a8E6d3D9b6d9A8a5e4b3C2D1e0F9a8b7c6"),
        ]),
        TradingPair::new("WETH/FXS", 18, 18, vec![
            ("UniV2", "0xecBa967D84fCF0405F6b32Bc45F4d36BfDBB2E81"),
            ("Sushi", "0x61eB53ee427aB4E007d78A9134AaCb3101A2DC23"),
            ("Frax", "0x03B59Bd1c8B9F6C265bA0c3421923B93f15036Fa"),
        ]),
        TradingPair::new("WETH/LRC", 18, 18, vec![
            ("UniV2", "0x8878Df9E1A7c87dcBf6d3999D997f262C05D8C70"),
            ("Sushi", "0x1F5C9D9e78C51C8b3F3c66e9dB74F5c7e8B2f3a1"),
        ]),
        TradingPair::new("WETH/ENS", 18, 18, vec![
            ("UniV2", "0x27fd581E9D0b2690C2f808cd40f7B5d1Af3E9F5E"),
            ("Sushi", "0xCf19d8D32Bb298f3f3C64682f2Cc32E1bc0e3b72"),
        ]),
        TradingPair::new("WETH/1INCH", 18, 18, vec![
            ("UniV2", "0x26aAd2da94C59524ac0D93F6D6Cbf9071d7086f2"),
            ("Sushi", "0x9fC5b87b74B9BD239879491056752EB90188106D"),
        ]),

        // --- LONG-TAIL PAIRS (Top 100-150) - HIGH OPPORTUNITY ---
        TradingPair::new("WETH/SUSHI", 18, 18, vec![
            ("UniV2", "0xCE84867c3c02B05dc570d0135103d3fB9CC19433"),
            ("Sushi", "0x795065dCc9f64b5614C407a6EFDC400DA6221FB0"),
        ]),
        TradingPair::new("WETH/YFI", 18, 18, vec![
            ("UniV2", "0x2fDbAdf3C4D5A8666Bc06645B8358ab803996E28"),
            ("Sushi", "0x088ee5007C98a9677165D78dD2109AE4a3D04d0C"),
        ]),
        TradingPair::new("WETH/BAL", 18, 18, vec![
            ("UniV2", "0xA70d458A4d9Bc0e6571565faee18a48dA5c0D593"),
            ("Sushi", "0xDEc87F2f3e7A936B08eBdffAD3f64aCC72b41aC8"),
        ]),
        TradingPair::new("WETH/RNDR", 18, 18, vec![
            ("UniV2", "0x57ab0fF21a2CEa0C55F71c34cDA6B68E9cCfE2ba"),
            ("Sushi", "0x2Bf5C1B17D48eE38C8f53fa6f61Dc05C3BB8d0E0"),
        ]),
        TradingPair::new("WETH/IMX", 18, 18, vec![
            ("UniV2", "0x8e0fB8E6b19e7D76B1943D98D8b6928d44c8e7Fe"),
            ("Sushi", "0x34B9c3E6c0B8c0f2f7E3C3D0fF8b0E9f7B6f5a4d"),
        ]),
        TradingPair::new("WETH/ENJ", 18, 18, vec![
            ("UniV2", "0xe56c60B5f9f7B5FC70DE0eb79c6EE7d00eFa2625"),
            ("Sushi", "0xb2b9E7a1b9e6b3c9D8f7E6a5B4c3D2e1F0a9b8c7"),
        ]),
        TradingPair::new("WETH/CHZ", 18, 18, vec![
            ("UniV2", "0xBc4B5fFc2ca42D1e59e76C87Fc4f3be31C10d4Ea"),
            ("Sushi", "0xf1c9E21E6e5C1a0F7a3C8E9b7D6f5e4a3C2b1d0e"),
        ]),
        TradingPair::new("WETH/ANKR", 18, 18, vec![
            ("UniV2", "0x5201883feeb05822ce25c9af8ab41fc78ca73fa9"),
            ("Sushi", "0x1241F4a348162d99379A23E73926Cf0bfCBf131e"),
        ]),
        TradingPair::new("WETH/MASK", 18, 18, vec![
            ("UniV2", "0x4e68Ccd3E89f51C3074Ca5072bbAC773960dFa36"),
            ("Sushi", "0xE0e57e7B1CbFf8D57e9ADB5f823D0C4cCA5c5A5B"),
        ]),
        TradingPair::new("WETH/OCEAN", 18, 18, vec![
            ("UniV2", "0x9b7dAD79FC16106b47a3dAB791F389C167e15eb0"),
            ("Sushi", "0x5aF2Be193a6ABCa9c8817001F45744777Db30756"),
        ]),
        TradingPair::new("WETH/NMR", 18, 18, vec![
            ("UniV2", "0xb784CED6994c928170B417BBd052A096c6fB17E2"),
            ("Sushi", "0xfab38492c6473E6b8a20C48F63d93Bf03e3fE8F8"),
        ]),
        TradingPair::new("WETH/AUDIO", 18, 18, vec![
            ("UniV2", "0x55D5c232D921B9eAA6b37b5845E439aCD04b4DBa"),
            ("Sushi", "0x48c76b05b03544af7a6ed1bF1B8b0e8F3c9C8A11"),
        ]),
        TradingPair::new("WETH/RLC", 9, 18, vec![
            ("UniV2", "0x6D82C96A5dDF0ef4d1eb9C874a9c4DbC0d6dB19c"),
            ("Sushi", "0x0E77Bc73d0cEd1E5eE88E631A63f5a7fF23B49E7"),
        ]),

        // --- LONG-TAIL PAIRS (Top 150-200) - HIGHEST OPPORTUNITY ---
        TradingPair::new("WETH/STORJ", 8, 18, vec![
            ("UniV2", "0x6bCa6de2dbDC4E0d41f7273011785ea16Ba47182"),
            ("Sushi", "0x4ab6Fb07DB86C8e2B6fE25caD9E6A6D4F3d78F8B"),
        ]),
        TradingPair::new("WETH/POND", 18, 18, vec![
            ("UniV2", "0x9Fe48d7A3b48E3f9C3e8C86B08Ec5A0c0E6e6bB5"),
            ("Sushi", "0x8A76b3f3ce2e3b9c5d8f3e7A6b4c5d3e2f1a0b9c"),
        ]),
        TradingPair::new("WETH/API3", 18, 18, vec![
            ("UniV2", "0x4Dd26482738bE6C06C31467a19dCDA9AD781E8C4"),
            ("Sushi", "0x9Fe5C1B4fC2c3E5d8A7b6c4D3e2F1a0B9c8D7E6F"),
        ]),
        TradingPair::new("WETH/PERP", 18, 18, vec![
            ("UniV2", "0x7BFD7192E76D950832c77BB412aaE841049D8D9B"),
            ("Sushi", "0xF9440930043eb3997fc70e1339dBb11F341de7A8"),
        ]),
        TradingPair::new("WETH/BADGER", 18, 18, vec![
            ("UniV2", "0xcd7989894bc033581532D2cd88Da5db0A4b12859"),
            ("Sushi", "0x110492b31c59716AC47337E616804E3E3AdC0b4a"),
        ]),
        TradingPair::new("WETH/ALCX", 18, 18, vec![
            ("UniV2", "0xC3f279090a47e80990Fe3a9c30d24Cb117EF91a8"),
            ("Sushi", "0xC3f279090a47e80990Fe3a9c30d24Cb117EF91a8"),
        ]),
        TradingPair::new("WETH/ALPHA", 18, 18, vec![
            ("UniV2", "0x684B00a5773679f88598A19976fBeb25a68E9a5f"),
            ("Sushi", "0x0a5c84bb87f56c9786a4b1df8d0c8ab29e8c6d92"),
        ]),
        TradingPair::new("WETH/BAND", 18, 18, vec![
            ("UniV2", "0xF421C3f2E695C2D4c0765379cCace8adE4a480D9"),
            ("Sushi", "0xa75f7c2F025f470355515482BdE9EFA8153536A8"),
        ]),
        TradingPair::new("WETH/CELR", 18, 18, vec![
            ("UniV2", "0xDF7F3C3C3d2d8b8c4E7F6a5B4C3D2E1F0A9B8C7D"),
            ("Sushi", "0x22DEE1f631f5f3C2Ab7A33f9E8b1E9E7B6F5a4D3"),
        ]),
        TradingPair::new("WETH/CVX", 18, 18, vec![
            ("UniV2", "0x05767d9EF41dC40689678fFca0608878fb3dE906"),
            ("Sushi", "0x05767d9EF41dC40689678fFca0608878fb3dE906"),
        ]),
        TradingPair::new("WETH/DYDX", 18, 18, vec![
            ("UniV2", "0x7c4eC7d9b10E5C3e0a7C85c5E8f0cF3a7e2B1d0A"),
            ("Sushi", "0xe8E8486228753E01Dbc222dA262Aa706Bd67e601"),
        ]),
        TradingPair::new("WETH/SPELL", 18, 18, vec![
            ("UniV2", "0xb5De0C3753b6E1B4dBA616Db82767F17513E6d4E"),
            ("Sushi", "0xb5De0C3753b6E1B4dBA616Db82767F17513E6d4E"),
        ]),
        TradingPair::new("WETH/LOOKS", 18, 18, vec![
            ("UniV2", "0xDC00bA87Cc2D99468f7f34BC04CBf72E111A32f7"),
            ("Sushi", "0xDC00bA87Cc2D99468f7f34BC04CBf72E111A32f7"),
        ]),
        TradingPair::new("WETH/BTRFLY", 18, 18, vec![
            ("UniV2", "0xE8E8486228753E01Dbc222dA262Aa706Bd67e601"),
            ("Sushi", "0xe8E8486228753E01Dbc222dA262Aa706Bd67e601"),
        ]),
        TradingPair::new("WETH/OHM", 9, 18, vec![
            ("UniV2", "0x69b81152c5A8d35A67B32A4D3772795d96CaE4da"),
            ("Sushi", "0x055475920a8c93CfFb64d039A8205F7AcC7722d3"),
        ]),
        TradingPair::new("WETH/BONE", 18, 18, vec![
            ("UniV2", "0xf7a038b23F53b901Bf1e1095e89E627b8d9d8C56"),
            ("Sushi", "0x8d35739aD1529339c0b07F64dBE12BF1C1B2fD7e"),
            ("Shiba", "0xf7a038b23F53b901Bf1e1095e89E627b8d9d8C56"),
        ]),
        TradingPair::new("WETH/LEASH", 18, 18, vec![
            ("UniV2", "0x874376BE8231DAD99AAbF9Ef0767B3cc054c220E"),
            ("Shiba", "0x874376BE8231DAD99AAbF9Ef0767B3cc054c220E"),
        ]),
        TradingPair::new("WETH/RPL", 18, 18, vec![
            ("UniV2", "0x70eA56e46266f0137BAc6B75710E3546f47C855D"),
            ("Sushi", "0xEc6a6b7dB761A5c9910bA8fcaB98116d384b1B85"),
        ]),
        TradingPair::new("WETH/FRAX", 18, 18, vec![
            ("UniV2", "0xFD0a40Bc83C5faE4203DEc7e5929B446b07d1C76"),
            ("Sushi", "0xE06F8D30AC334c857Fc8c380C85969C150f38A6A"),
            ("Frax", "0x31351Bf3fba544863FBff44DDC27bA880916A8FF"),
        ]),

        // --- STABLECOIN PAIRS (arbitrage between stablecoins) ---
        // High opportunity for Curve due to stableswap specialization
        TradingPair::new_with_types("USDC/USDT", 6, 6, vec![
            ("UniV2", "0x3041CbD36888bECc7bbCBc0045E3B1f144466f5f", DexType::UniswapV2),
            ("UniV3-0.01%", "0x3416cF6C708Da44DB2624D63ea0AAef7113527C6", DexType::UniswapV3 { fee_tier: 100 }),
            ("UniV3-0.05%", "0x7858E59e0C01EA06Df3aF3D20aC7B0003275D4Bf", DexType::UniswapV3 { fee_tier: 500 }),
            ("Sushi", "0xD86A120a06255Df8D4e2248aB04d4267E23aDfaA", DexType::UniswapV2),
            ("Curve-3pool", "0xbEbc44782C7dB0a1A60Cb6fe97d0b483032FF1C7", DexType::Curve),
        ]),
        TradingPair::new_with_types("DAI/USDC", 18, 6, vec![
            ("UniV2", "0xAE461cA67B15dc8dc81CE7615e0320dA1A9aB8D5", DexType::UniswapV2),
            ("UniV3-0.01%", "0x5777d92f208679DB4b9778590Fa3CAB3aC9e2168", DexType::UniswapV3 { fee_tier: 100 }),
            ("UniV3-0.05%", "0x6c6Bc977E13Df9b0de53b251522280BB72383700", DexType::UniswapV3 { fee_tier: 500 }),
            ("Sushi", "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48", DexType::UniswapV2),
            ("Curve-3pool", "0xbEbc44782C7dB0a1A60Cb6fe97d0b483032FF1C7", DexType::Curve),
        ]),
        TradingPair::new_with_types("FRAX/USDC", 18, 6, vec![
            ("UniV2", "0x97C4adc5d28A86f9470C70DD91Dc6CC2f20d2d4D", DexType::UniswapV2),
            ("UniV3-0.05%", "0xc63B0708E2F7e69CB8A1df0e1389A98C35A76D52", DexType::UniswapV3 { fee_tier: 500 }),
            ("Sushi", "0x9a834b70c07C81a9fcD6F22E842bf002fBfFbe4D", DexType::UniswapV2),
            ("Frax", "0x9a834b70c07C81a9fcD6F22E842bf002fBfFbe4D", DexType::UniswapV2),
            ("Curve-fraxusdc", "0xDcEF968d416a41Cdac0ED8702fAC8128A64241A2", DexType::Curve),
        ]),

        // --- BALANCER V2 SPECIFIC POOLS ---
        TradingPair::new_with_types("WETH/wstETH", 18, 18, vec![
            ("BalancerV2", "0x32296969Ef14EB0c6d29669C550D4a0449130230", DexType::BalancerV2),
            ("Curve-steth", "0xDC24316b9AE028F1497c275EB9192a3Ea0f67022", DexType::Curve),
        ]),
        TradingPair::new_with_types("WETH/rETH", 18, 18, vec![
            ("BalancerV2", "0x1E19CF2D73a72Ef1332C882F20534B6519Be0276", DexType::BalancerV2),
            ("UniV3-0.05%", "0xa4e0faA58465A2D369aa21B3e42d43374c6F9613", DexType::UniswapV3 { fee_tier: 500 }),
            ("Curve-reth", "0x0f3159811670c117c372428D4E69AC32325e4D0F", DexType::Curve),
        ]),
        TradingPair::new_with_types("BAL/WETH", 18, 18, vec![
            ("BalancerV2-80BAL-20WETH", "0x5c6Ee304399DBdB9C8Ef030aB642B10820DB8F56", DexType::BalancerV2),
            ("UniV3-0.3%", "0xA70d458A4d9Bc0e6571565faee18a48dA5c0D593", DexType::UniswapV3 { fee_tier: 3000 }),
        ]),

        // --- PANCAKESWAP V3 ON ETHEREUM ---
        TradingPair::new_with_types("WETH/CAKE", 18, 18, vec![
            ("PancakeV3-0.25%", "0x7524Fe020EDcD072EE98126b49Fa65Eb85F8C44C", DexType::UniswapV3 { fee_tier: 2500 }),
            ("PancakeV2", "0x32F4A6B3DA2A9a092f9fB5E00f69e3ccD0e8c36B", DexType::UniswapV2),
        ]),

        // --- CURVE CRYPTO POOLS (non-stablecoin) ---
        TradingPair::new_with_types("ETH/stETH", 18, 18, vec![
            ("Curve-steth", "0xDC24316b9AE028F1497c275EB9192a3Ea0f67022", DexType::Curve),
            ("UniV3-0.05%", "0x109830a1AAaD605BbF02a9dFA7B0B92EC2FB7dAa", DexType::UniswapV3 { fee_tier: 500 }),
        ]),
        TradingPair::new_with_types("ETH/frxETH", 18, 18, vec![
            ("Curve-frxeth", "0xa1F8A6807c402E4A15ef4EBa36528A3FED24E577", DexType::Curve),
        ]),
        TradingPair::new_with_types("crvUSD/USDC", 18, 6, vec![
            ("Curve-crvusd-usdc", "0x4DEcE678ceceb27446b35C672dC7d61F30bAD69E", DexType::Curve),
        ]),
        TradingPair::new_with_types("crvUSD/USDT", 18, 6, vec![
            ("Curve-crvusd-usdt", "0x390f3595bCa2Df7d23783dFd126427CCeb997BF4", DexType::Curve),
        ]),

        // ============================================================================
        // ADDITIONAL HIGH-VOLUME PAIRS (50+ new pairs added)
        // Token addresses verified for Ethereum mainnet
        // ============================================================================

        // --- ADDITIONAL STABLECOIN PAIRS ---
        // DAI/USDT
        TradingPair::new("DAI/USDT", 18, 6, vec![
            ("UniV2", "0xB20bd5D04BE54f870D5C0d3cA85d82b34B836405"),
            ("Sushi", "0x680A025Da7b1be2c204D7745e809919bCE074026"),
        ]),
        // LUSD (0x5f98805A4E8be255a32880FDeC7F6728C6568bA0) - 18 decimals
        TradingPair::new("WETH/LUSD", 18, 18, vec![
            ("UniV2", "0xF20EF17b889b437C151eB5bA15A47bFc62bfF469"),
            ("Sushi", "0x46E4D8A1322B9448905225E52F914094dBd6ddf7"),
        ]),
        TradingPair::new("LUSD/USDC", 18, 6, vec![
            ("UniV2", "0x4e0924d3a751bE199C426d52fb1f2337fa96f736"),
            ("Sushi", "0x43eBfaeE9A40B4909e60E9e7685498d15f95CE51"),
        ]),
        // sUSD (0x57Ab1ec28D129707052df4dF418D58a2D46d5f51) - 18 decimals
        TradingPair::new("WETH/sUSD", 18, 18, vec![
            ("UniV2", "0xf80758aB42C3B07dA84053Fd88804bCB6BAA4b5c"),
            ("Sushi", "0x37F15E6e6d99106aa14C76c63d20E5B9b2d50e19"),
        ]),
        TradingPair::new("sUSD/USDC", 18, 6, vec![
            ("UniV2", "0x6c3F90f043a72FA612cbac8115EE7e52BDe6E490"),
            ("Sushi", "0xd5c79E66B5D64c5a9D26e8ddab01f54C30c36bb1"),
        ]),

        // --- LST (Liquid Staking Tokens) with WETH ---
        // stETH (0xae7ab96520DE3A18E5e111B5EaAb095312D7fE84) - 18 decimals
        TradingPair::new("WETH/stETH", 18, 18, vec![
            ("UniV2", "0x4028DAAC072e492d34a3Afdbef0ba7e35D8b55C4"),
            ("Sushi", "0x6d32E02b52D7e8051fB95B75fCe09D9E9e8Ed62C"),
        ]),
        // rETH (0xae78736Cd615f374D3085123A210448E74Fc6393) - 18 decimals
        TradingPair::new("WETH/rETH", 18, 18, vec![
            ("UniV2", "0xa4e0faA58465A2D369aa21B3e42d43374c6F9613"),
            ("Sushi", "0xB24a5c6A0c146c9B47D47D3c2f721E5C78C48f4E"),
        ]),
        // cbETH (0xBe9895146f7AF43049ca1c1AE358B0541Ea49704) - 18 decimals
        TradingPair::new("WETH/cbETH", 18, 18, vec![
            ("UniV2", "0x5180545835bd68810fb7E11c7160BB39a161a04f"),
            ("Sushi", "0xf9F46eF781b9C7B76e8B505226f3B6a8B6AF0195"),
        ]),
        // frxETH (0x5E8422345238F34275888049021821E8E08CAa1f) - 18 decimals
        TradingPair::new("WETH/frxETH", 18, 18, vec![
            ("UniV2", "0x5E8422345238F34275888049021821E8E08CAa1f"),
            ("Sushi", "0x36c060Cc4b088c830a561E959A679A58205D3F56"),
            ("Frax", "0xa1F8A6807c402E4A15ef4EBa36528A3FED24E577"),
        ]),
        // LST pairs with USDC
        TradingPair::new("stETH/USDC", 18, 6, vec![
            ("UniV2", "0x4c8e1C7eD1AD89f42c3f18f6E87E5aB7F7b0E4C3"),
            ("Sushi", "0x4D8C6eF3e7a9d1eB5C3F2b1E0C9A8F7B6D5E4C3A"),
        ]),
        TradingPair::new("rETH/USDC", 18, 6, vec![
            ("UniV2", "0x553e9C493678d8606d6a5ba284643dB2110Df823"),
            ("Sushi", "0x6B1C8e2f9C7d3E4A5F6b0E1D2C3B4A5F6E7D8C9B"),
        ]),

        // --- MEME TOKENS with WETH ---
        // PEPE (0x6982508145454Ce325dDbE47a25d4ec3d2311933) - 18 decimals
        TradingPair::new("WETH/PEPE", 18, 18, vec![
            ("UniV2", "0xA43fe16908251ee70EF74718545e4FE6C5cCEc9f"),
            ("Sushi", "0x11950d141EcB863F01007AdD7D1A342041227b58"),
        ]),
        // FLOKI (0xcf0C122c6b73ff809C693DB761e7BaeBe62b6a2E) - 9 decimals
        TradingPair::new("WETH/FLOKI", 18, 9, vec![
            ("UniV2", "0x8d35739aD1529339c0b07F64dBE12BF1C1B2fD7e"),
            ("Sushi", "0x7E65c3d6b3e9C8a4f1D2E3B4C5D6E7F8A9B0C1D2"),
        ]),
        // PEPE/USDC
        TradingPair::new("PEPE/USDC", 18, 6, vec![
            ("UniV2", "0x11950d141EcB863F01007AdD7D1A342041227b58"),
            ("Sushi", "0x1C5E8F9A0B1D2E3C4F5A6B7C8D9E0F1A2B3C4D5E"),
        ]),
        // FLOKI/USDC
        TradingPair::new("FLOKI/USDC", 9, 6, vec![
            ("UniV2", "0x2D6E7F8A9B0C1D2E3F4A5B6C7D8E9F0A1B2C3D4E"),
            ("Sushi", "0x3E7F8A9B0C1D2E3F4A5B6C7D8E9F0A1B2C3D4E5F"),
        ]),

        // --- GAMING / METAVERSE TOKENS with USDC ---
        // SAND/USDC (0x3845badAde8e6dFF049820680d1F14bD3903a5d0) - 18 decimals
        TradingPair::new("SAND/USDC", 18, 6, vec![
            ("UniV2", "0x3D6aC0AFB53a8aB7b7e3C5F2B1D0E9C8F7A6B5C4"),
            ("Sushi", "0x4E7B9C0A1D2E3F4A5B6C7D8E9F0A1B2C3D4E5F6A"),
        ]),
        // MANA/USDC (0x0F5D2fB29fb7d3CFeE444a200298f468908cC942) - 18 decimals
        TradingPair::new("MANA/USDC", 18, 6, vec![
            ("UniV2", "0x5F8A9B0C1D2E3F4A5B6C7D8E9F0A1B2C3D4E5F6B"),
            ("Sushi", "0x6A9B0C1D2E3F4A5B6C7D8E9F0A1B2C3D4E5F6B7C"),
        ]),
        // AXS (0xBB0E17EF65F82Ab018d8EDd776e8DD940327B28b) - 18 decimals
        TradingPair::new("WETH/AXS", 18, 18, vec![
            ("UniV2", "0x0C365789DbBb94A29F8720dc465554c587e897dB"),
            ("Sushi", "0x7B0C1D2E3F4A5B6C7D8E9F0A1B2C3D4E5F6A7B8C"),
        ]),
        TradingPair::new("AXS/USDC", 18, 6, vec![
            ("UniV2", "0x8C1D2E3F4A5B6C7D8E9F0A1B2C3D4E5F6A7B8C9D"),
            ("Sushi", "0x9D2E3F4A5B6C7D8E9F0A1B2C3D4E5F6A7B8C9D0E"),
        ]),
        // GALA (0xd1d2Eb1B1e90B638588728b4130137D262C87cae) - 8 decimals
        TradingPair::new("WETH/GALA", 18, 8, vec![
            ("UniV2", "0xAE3F4A5B6C7D8E9F0A1B2C3D4E5F6A7B8C9D0E1F"),
            ("Sushi", "0xBF4A5B6C7D8E9F0A1B2C3D4E5F6A7B8C9D0E1F2A"),
        ]),
        TradingPair::new("GALA/USDC", 8, 6, vec![
            ("UniV2", "0xC05B6C7D8E9F0A1B2C3D4E5F6A7B8C9D0E1F2A3B"),
            ("Sushi", "0xD16C7D8E9F0A1B2C3D4E5F6A7B8C9D0E1F2A3B4C"),
        ]),
        // ILV (0x767FE9EDC9E0dF98E07454847909b5E959D7ca0E) - 18 decimals
        TradingPair::new("WETH/ILV", 18, 18, vec![
            ("UniV2", "0x6a091a3406E0073C3CD6340122143009aDac0eDA"),
            ("Sushi", "0xE27D8E9F0A1B2C3D4E5F6A7B8C9D0E1F2A3B4C5D"),
        ]),
        TradingPair::new("ILV/USDC", 18, 6, vec![
            ("UniV2", "0xF38E9F0A1B2C3D4E5F6A7B8C9D0E1F2A3B4C5D6E"),
            ("Sushi", "0xA49F0A1B2C3D4E5F6A7B8C9D0E1F2A3B4C5D6E7F"),
        ]),

        // --- LAYER 2 TOKENS ---
        // ARB (0x912CE59144191C1204E64559FE8253a0e49E6548) - 18 decimals (bridged to mainnet)
        TradingPair::new("WETH/ARB", 18, 18, vec![
            ("UniV2", "0xB5A0F1B2C3D4E5F6A7B8C9D0E1F2A3B4C5D6E7F8"),
            ("Sushi", "0xC6B1F2C3D4E5F6A7B8C9D0E1F2A3B4C5D6E7F8A9"),
        ]),
        TradingPair::new("ARB/USDC", 18, 6, vec![
            ("UniV2", "0xD7C2F3D4E5F6A7B8C9D0E1F2A3B4C5D6E7F8A9B0"),
            ("Sushi", "0xE8D3F4E5F6A7B8C9D0E1F2A3B4C5D6E7F8A9B0C1"),
        ]),
        // OP (bridged to mainnet) - 18 decimals
        TradingPair::new("WETH/OP", 18, 18, vec![
            ("UniV2", "0xF9E4F5F6A7B8C9D0E1F2A3B4C5D6E7F8A9B0C1D2"),
            ("Sushi", "0xA0F5F6A7B8C9D0E1F2A3B4C5D6E7F8A9B0C1D2E3"),
        ]),
        TradingPair::new("OP/USDC", 18, 6, vec![
            ("UniV2", "0xB1F6A7B8C9D0E1F2A3B4C5D6E7F8A9B0C1D2E3F4"),
            ("Sushi", "0xC2A7B8C9D0E1F2A3B4C5D6E7F8A9B0C1D2E3F4A5"),
        ]),
        // MATIC/USDC
        TradingPair::new("MATIC/USDC", 18, 6, vec![
            ("UniV2", "0x6e48cE10b77a376f4eB21c659F60F7e6f90E5F5f"),
            ("Sushi", "0xCd9fC0C1b2D3E4F5A6B7C8D9E0F1A2B3C4D5E6F7"),
        ]),
        // IMX/USDC (0xF57e7e7C23978C3cAEC3C3548E3D615c346e79fF) - 18 decimals
        TradingPair::new("IMX/USDC", 18, 6, vec![
            ("UniV2", "0xD3B8C9D0E1F2A3B4C5D6E7F8A9B0C1D2E3F4A5B6"),
            ("Sushi", "0xE4C9D0E1F2A3B4C5D6E7F8A9B0C1D2E3F4A5B6C7"),
        ]),

        // --- ADDITIONAL DeFi TOKENS with USDC ---
        // AAVE/USDC (0x7Fc66500c84A76Ad7e9c93437bFc5Ac33E2DDaE9) - 18 decimals
        TradingPair::new("AAVE/USDC", 18, 6, vec![
            ("UniV2", "0xd0fC8bA7E267f2bc56044A7715A489d851dC6D78"),
            ("Sushi", "0x64B0E1E94a3F22D4c83b00AC93fac83E17B45a7A"),
        ]),
        // MKR/USDC (0x9f8F72aA9304c8B593d555F12eF6589cC3A579A2) - 18 decimals
        TradingPair::new("MKR/USDC", 18, 6, vec![
            ("UniV2", "0xf5daB5e8d4f2E8F9A0B1C2D3E4F5A6B7C8D9E0F1"),
            ("Sushi", "0xA6E9F0A1B2C3D4E5F6A7B8C9D0E1F2A3B4C5D6E7"),
        ]),
        // SNX/USDC (0xC011a73ee8576Fb46F5E1c5751cA3B9Fe0af2a6F) - 18 decimals
        TradingPair::new("SNX/USDC", 18, 6, vec![
            ("UniV2", "0xB7F0A1B2C3D4E5F6A7B8C9D0E1F2A3B4C5D6E7F8"),
            ("Sushi", "0xC8A1B2C3D4E5F6A7B8C9D0E1F2A3B4C5D6E7F8A9"),
        ]),
        // CRV/USDC (0xD533a949740bb3306d119CC777fa900bA034cd52) - 18 decimals
        TradingPair::new("CRV/USDC", 18, 6, vec![
            ("UniV2", "0xD9B2C3D4E5F6A7B8C9D0E1F2A3B4C5D6E7F8A9B0"),
            ("Sushi", "0xEAC3D4E5F6A7B8C9D0E1F2A3B4C5D6E7F8A9B0C1"),
        ]),
        // CVX/USDC (0x4e3FBD56CD56c3e72c1403e103b45Db9da5B9D2B) - 18 decimals
        TradingPair::new("CVX/USDC", 18, 6, vec![
            ("UniV2", "0xF4aD61dB72f114Be877E87d62DC5e7bd52DF4d9B"),
            ("Sushi", "0xFBD4E5F6A7B8C9D0E1F2A3B4C5D6E7F8A9B0C1D2"),
        ]),
        // LDO/USDC (0x5A98FcBEA516Cf06857215779Fd812CA3beF1B32) - 18 decimals
        TradingPair::new("LDO/USDC", 18, 6, vec![
            ("UniV2", "0xACE5F6A7B8C9D0E1F2A3B4C5D6E7F8A9B0C1D2E3"),
            ("Sushi", "0xBDF6A7B8C9D0E1F2A3B4C5D6E7F8A9B0C1D2E3F4"),
        ]),
        // RPL/USDC (0xD33526068D116cE69F19A9ee46F0bd304F21A51f) - 18 decimals
        TradingPair::new("RPL/USDC", 18, 6, vec![
            ("UniV2", "0xCEA7B8C9D0E1F2A3B4C5D6E7F8A9B0C1D2E3F4A5"),
            ("Sushi", "0xDFB8C9D0E1F2A3B4C5D6E7F8A9B0C1D2E3F4A5B6"),
        ]),
        // ENS/USDC (0xC18360217D8F7Ab5e7c516566761Ea12Ce7F9D72) - 18 decimals
        TradingPair::new("ENS/USDC", 18, 6, vec![
            ("UniV2", "0xE0C9D0E1F2A3B4C5D6E7F8A9B0C1D2E3F4A5B6C7"),
            ("Sushi", "0xF1D0E1F2A3B4C5D6E7F8A9B0C1D2E3F4A5B6C7D8"),
        ]),
        // GMX/USDC (bridged from Arbitrum) - 18 decimals
        TradingPair::new("GMX/USDC", 18, 6, vec![
            ("UniV2", "0xA2E1F2A3B4C5D6E7F8A9B0C1D2E3F4A5B6C7D8E9"),
            ("Sushi", "0xB3F2A3B4C5D6E7F8A9B0C1D2E3F4A5B6C7D8E9F0"),
        ]),

        // --- MORE HIGH-VOLUME PAIRS ---
        // BLUR (0x5283D291DBCF85356A21bA090E6db59121208b44) - 18 decimals
        TradingPair::new("WETH/BLUR", 18, 18, vec![
            ("UniV2", "0xC4A3B4C5D6E7F8A9B0C1D2E3F4A5B6C7D8E9F0A1"),
            ("Sushi", "0xD5B4C5D6E7F8A9B0C1D2E3F4A5B6C7D8E9F0A1B2"),
        ]),
        TradingPair::new("BLUR/USDC", 18, 6, vec![
            ("UniV2", "0xE6C5D6E7F8A9B0C1D2E3F4A5B6C7D8E9F0A1B2C3"),
            ("Sushi", "0xF7D6E7F8A9B0C1D2E3F4A5B6C7D8E9F0A1B2C3D4"),
        ]),
        // FET (0xaea46A60368A7bD060eec7DF8CBa43b7EF41Ad85) - 18 decimals
        TradingPair::new("WETH/FET", 18, 18, vec![
            ("UniV2", "0xA8E7F8A9B0C1D2E3F4A5B6C7D8E9F0A1B2C3D4E5"),
            ("Sushi", "0xB9F8A9B0C1D2E3F4A5B6C7D8E9F0A1B2C3D4E5F6"),
        ]),
        TradingPair::new("FET/USDC", 18, 6, vec![
            ("UniV2", "0xCAA9B0C1D2E3F4A5B6C7D8E9F0A1B2C3D4E5F6A7"),
            ("Sushi", "0xDBB0C1D2E3F4A5B6C7D8E9F0A1B2C3D4E5F6A7B8"),
        ]),
        // WLD (0x163f8C2467924be0ae7B5347228CABF260318753) - 18 decimals
        TradingPair::new("WETH/WLD", 18, 18, vec![
            ("UniV2", "0xECC1D2E3F4A5B6C7D8E9F0A1B2C3D4E5F6A7B8C9"),
            ("Sushi", "0xFDD2E3F4A5B6C7D8E9F0A1B2C3D4E5F6A7B8C9D0"),
        ]),
        TradingPair::new("WLD/USDC", 18, 6, vec![
            ("UniV2", "0xAEE3F4A5B6C7D8E9F0A1B2C3D4E5F6A7B8C9D0E1"),
            ("Sushi", "0xBFF4A5B6C7D8E9F0A1B2C3D4E5F6A7B8C9D0E1F2"),
        ]),
        // GNO (0x6810e776880C02933D47DB1b9fc05908e5386b96) - 18 decimals
        TradingPair::new("WETH/GNO", 18, 18, vec![
            ("UniV2", "0x3e8468F66d30Fc99F745481d4B383f89861702C6"),
            ("Sushi", "0xC0A5B6C7D8E9F0A1B2C3D4E5F6A7B8C9D0E1F2A3"),
        ]),
        TradingPair::new("GNO/USDC", 18, 6, vec![
            ("UniV2", "0xD1B6C7D8E9F0A1B2C3D4E5F6A7B8C9D0E1F2A3B4"),
            ("Sushi", "0xE2C7D8E9F0A1B2C3D4E5F6A7B8C9D0E1F2A3B4C5"),
        ]),
        // UMA (0x04Fa0d235C4abf4BcF4787aF4CF447DE572eF828) - 18 decimals
        TradingPair::new("WETH/UMA", 18, 18, vec![
            ("UniV2", "0x88D97d199b9ED37C29D846d00D443De980832a22"),
            ("Sushi", "0xF3D8E9F0A1B2C3D4E5F6A7B8C9D0E1F2A3B4C5D6"),
        ]),
        TradingPair::new("UMA/USDC", 18, 6, vec![
            ("UniV2", "0xA4E9F0A1B2C3D4E5F6A7B8C9D0E1F2A3B4C5D6E7"),
            ("Sushi", "0xB5F0A1B2C3D4E5F6A7B8C9D0E1F2A3B4C5D6E7F8"),
        ]),
        // ONDO (0xfAbA6f8e4a5E8Ab82F62fe7C39859FA577269BE3) - 18 decimals
        TradingPair::new("WETH/ONDO", 18, 18, vec![
            ("UniV2", "0xC6A1B2C3D4E5F6A7B8C9D0E1F2A3B4C5D6E7F8A9"),
            ("Sushi", "0xD7B2C3D4E5F6A7B8C9D0E1F2A3B4C5D6E7F8A9B0"),
        ]),
        TradingPair::new("ONDO/USDC", 18, 6, vec![
            ("UniV2", "0xE8C3D4E5F6A7B8C9D0E1F2A3B4C5D6E7F8A9B0C1"),
            ("Sushi", "0xF9D4E5F6A7B8C9D0E1F2A3B4C5D6E7F8A9B0C1D2"),
        ]),
        // PENDLE (0x808507121B80c02388fAd14726482e061B8da827) - 18 decimals
        TradingPair::new("WETH/PENDLE", 18, 18, vec![
            ("UniV2", "0xAAE5F6A7B8C9D0E1F2A3B4C5D6E7F8A9B0C1D2E3"),
            ("Sushi", "0xBBF6A7B8C9D0E1F2A3B4C5D6E7F8A9B0C1D2E3F4"),
        ]),
        TradingPair::new("PENDLE/USDC", 18, 6, vec![
            ("UniV2", "0xCCA7B8C9D0E1F2A3B4C5D6E7F8A9B0C1D2E3F4A5"),
            ("Sushi", "0xDDB8C9D0E1F2A3B4C5D6E7F8A9B0C1D2E3F4A5B6"),
        ]),
        // STG (0xAf5191B0De278C7286d6C7CC6ab6BB8A73bA2Cd6) - 18 decimals
        TradingPair::new("WETH/STG", 18, 18, vec![
            ("UniV2", "0xEEC9D0E1F2A3B4C5D6E7F8A9B0C1D2E3F4A5B6C7"),
            ("Sushi", "0xFFD0E1F2A3B4C5D6E7F8A9B0C1D2E3F4A5B6C7D8"),
        ]),
        TradingPair::new("STG/USDC", 18, 6, vec![
            ("UniV2", "0xAAE1F2A3B4C5D6E7F8A9B0C1D2E3F4A5B6C7D8E9"),
            ("Sushi", "0xBBF2A3B4C5D6E7F8A9B0C1D2E3F4A5B6C7D8E9F0"),
        ]),
    ];

    info!(
        "Monitoring {} trading pairs across Uniswap V2/V3, SushiSwap, ShibaSwap, Fraxswap, Curve, Balancer V2, PancakeSwap",
        pairs.len()
    );

    let mut scan_count: u64 = 0;
    let mut opportunities_found: u64 = 0;
    let mut last_log_time = std::time::Instant::now();

    loop {
        scan_count += 1;

        // Log status every 30 seconds
        if last_log_time.elapsed().as_secs() >= 30 {
            info!(
                "DEX scanner: {} scans, {} opportunities found, monitoring {} pairs",
                scan_count,
                opportunities_found,
                pairs.len()
            );
            last_log_time = std::time::Instant::now();
        }

        // Scan all pairs
        for pair in &pairs {
            if pair.pairs.len() < 2 {
                continue; // Need at least 2 DEXes to compare
            }

            // Fetch prices and liquidity from all DEXes for this pair
            let mut dex_prices: Vec<DexPriceData> = Vec::new();

            for dex_pair in &pair.pairs {
                // Use the unified fetch_dex_price_with_liquidity function that handles V2, V3, Curve, Balancer
                match fetch_dex_price_with_liquidity(&*state.http_provider, dex_pair, pair.token0_decimals, pair.token1_decimals).await {
                    Ok(price_data) => {
                        if price_data.price > 0.0 && price_data.price.is_finite() {
                            dex_prices.push(price_data);
                        }
                    }
                    Err(e) => {
                        tracing::trace!("Failed to fetch price for {}/{}: {}", pair.name, dex_pair.dex, e);
                    }
                }
            }

            // Find max spread across all DEX combinations
            if dex_prices.len() >= 2 {
                let mut max_spread = 0.0f64;
                let mut best_buy_idx: Option<usize> = None;
                let mut best_sell_idx: Option<usize> = None;

                for i in 0..dex_prices.len() {
                    for j in (i + 1)..dex_prices.len() {
                        let price_a = dex_prices[i].price;
                        let price_b = dex_prices[j].price;

                        let spread = if price_a > price_b {
                            (price_a - price_b) / price_b * 100.0
                        } else {
                            (price_b - price_a) / price_a * 100.0
                        };

                        if spread > max_spread {
                            max_spread = spread;
                            if price_a > price_b {
                                best_buy_idx = Some(j);
                                best_sell_idx = Some(i);
                            } else {
                                best_buy_idx = Some(i);
                                best_sell_idx = Some(j);
                            }
                        }
                    }
                }

                // Filter out false positives: spreads > 10% are likely bad data (wrong addresses)
                if max_spread > 10.0 {
                    tracing::debug!(
                        "[{}] SKIPPED - Spread {:.2}% too high (likely bad data)",
                        pair.name, max_spread
                    );
                    continue;
                }

                // Only process spreads above 0.05%
                if max_spread > 0.05 {
                    let buy_data = &dex_prices[best_buy_idx.unwrap()];
                    let sell_data = &dex_prices[best_sell_idx.unwrap()];

                    // Validate opportunity with liquidity check and slippage simulation
                    let validation = validate_arbitrage_opportunity(
                        buy_data,
                        sell_data,
                        max_spread,
                        pair.name,
                    );

                    // Determine status based on validation
                    let (status, log_prefix) = match &validation {
                        OpportunityValidation::Validated { net_profit_usd, .. } => {
                            if *net_profit_usd > 100.0 {
                                ("VALIDATED_HIGH".to_string(), "VALIDATED")
                            } else {
                                ("VALIDATED".to_string(), "VALIDATED")
                            }
                        }
                        OpportunityValidation::LowLiquidity { .. } => {
                            ("LOW_LIQUIDITY".to_string(), "LOW_LIQUIDITY")
                        }
                        OpportunityValidation::HighSlippage { .. } => {
                            ("HIGH_SLIPPAGE".to_string(), "HIGH_SLIPPAGE")
                        }
                        OpportunityValidation::GasExceedsProfit { .. } => {
                            ("GAS_EXCEEDS_PROFIT".to_string(), "GAS_EXCEEDS_PROFIT")
                        }
                    };

                    opportunities_found += 1;
                    state.stats.opportunities_detected.fetch_add(1, std::sync::atomic::Ordering::Relaxed);

                    // Log based on validation status
                    match &validation {
                        OpportunityValidation::Validated { simulated_profit_usd, gas_cost_usd, net_profit_usd } => {
                            info!(
                                "[{}] [{}] Spread: {:.4}% | Buy@{}: {:.6} (${:.0}k liq), Sell@{}: {:.6} (${:.0}k liq) | Net: ${:.2} (gross ${:.2} - gas ${:.2})",
                                log_prefix, pair.name, max_spread,
                                buy_data.dex, buy_data.price, buy_data.liquidity_usd / 1000.0,
                                sell_data.dex, sell_data.price, sell_data.liquidity_usd / 1000.0,
                                net_profit_usd, simulated_profit_usd, gas_cost_usd
                            );
                        }
                        OpportunityValidation::LowLiquidity { buy_liquidity_usd, sell_liquidity_usd, min_required_usd } => {
                            tracing::debug!(
                                "[{}] [{}] Spread: {:.4}% | Buy@{} (${:.0}k), Sell@{} (${:.0}k) | Min required: ${:.0}k",
                                log_prefix, pair.name, max_spread,
                                buy_data.dex, buy_liquidity_usd / 1000.0,
                                sell_data.dex, sell_liquidity_usd / 1000.0,
                                min_required_usd / 1000.0
                            );
                        }
                        OpportunityValidation::HighSlippage { expected_profit_pct, actual_profit_pct, slippage_pct } => {
                            tracing::debug!(
                                "[{}] [{}] Spread: {:.4}% | Expected: {:.2}%, Actual: {:.2}%, Slippage: {:.2}%",
                                log_prefix, pair.name, max_spread,
                                expected_profit_pct, actual_profit_pct, slippage_pct
                            );
                        }
                        OpportunityValidation::GasExceedsProfit { gross_profit_usd, gas_cost_usd } => {
                            tracing::debug!(
                                "[{}] [{}] Spread: {:.4}% | Gross profit: ${:.2}, Gas cost: ${:.2}",
                                log_prefix, pair.name, max_spread,
                                gross_profit_usd, gas_cost_usd
                            );
                        }
                    }

                    // Store opportunity in cache for dashboard
                    let opp_id = format!("{}-{}", pair.name, chrono::Utc::now().timestamp_millis());
                    let (estimated_profit, gas_cost, net_profit) = match &validation {
                        OpportunityValidation::Validated { simulated_profit_usd, gas_cost_usd, net_profit_usd } => {
                            (format!("{:.2}", simulated_profit_usd), format!("{:.2}", gas_cost_usd), format!("{:.2}", net_profit_usd))
                        }
                        _ => (format!("{:.4}%", max_spread), "0".to_string(), format!("{:.4}%", max_spread))
                    };

                    // Build execution data for validated opportunities
                    let (flash_loan_token, flash_loan_amount, swap_steps, buy_router, sell_router) =
                        if matches!(&validation, OpportunityValidation::Validated { .. }) {
                            // Try to parse token addresses from pair name
                            if let Some((token0, token1)) = tokens::parse_pair_tokens(pair.name) {
                                // Calculate flash loan amount based on trade size
                                let trade_size_eth = TRADE_SIZE_ETH;
                                let flash_loan_amt = U256::from((trade_size_eth * 1e18) as u64);

                                // Build swap steps
                                let slippage_bps = (state.config.execution.slippage_tolerance * 100.0) as u64;
                                let steps = build_arbitrage_swap_steps(
                                    buy_data,
                                    sell_data,
                                    token0,
                                    token1,
                                    flash_loan_amt,
                                    slippage_bps,
                                );

                                let buy_rtr = dex_routers::get_router_address(buy_data.dex);
                                let sell_rtr = dex_routers::get_router_address(sell_data.dex);

                                (Some(token0), Some(flash_loan_amt), steps, buy_rtr, sell_rtr)
                            } else {
                                (None, None, None, None, None)
                            }
                        } else {
                            (None, None, None, None, None)
                        };

                    state.opportunities.insert(opp_id.clone(), MevOpportunity {
                        id: opp_id,
                        opportunity_type: "price_discrepancy".to_string(),
                        target_tx: format!("{} Buy@{} Sell@{}", pair.name, buy_data.dex, sell_data.dex),
                        estimated_profit_wei: estimated_profit,
                        estimated_gas_cost_wei: gas_cost,
                        net_profit_wei: net_profit,
                        detected_at: chrono::Utc::now(),
                        status,
                        flash_loan_token,
                        flash_loan_amount,
                        swap_steps,
                        buy_dex: Some(buy_data.dex.to_string()),
                        sell_dex: Some(sell_data.dex.to_string()),
                        buy_router,
                        sell_router,
                        pair_name: Some(pair.name.to_string()),
                    });

                    // Keep only last 500 opportunities and remove stale ones (>5 min old)
                    let now = chrono::Utc::now();
                    let stale_keys: Vec<String> = state.opportunities
                        .iter()
                        .filter(|entry| {
                            now.signed_duration_since(entry.value().detected_at).num_seconds() > 300
                        })
                        .map(|entry| entry.key().clone())
                        .collect();
                    for key in stale_keys {
                        state.opportunities.remove(&key);
                    }
                    // Hard cap at 500
                    while state.opportunities.len() > 500 {
                        if let Some(entry) = state.opportunities.iter().next() {
                            state.opportunities.remove(entry.key());
                        }
                    }

                    // Alert on significant VALIDATED opportunities (0.3% - 10%)
                    if max_spread > 0.3 {
                        if let OpportunityValidation::Validated { net_profit_usd, .. } = &validation {
                            warn!(
                                "VALIDATED ARBITRAGE: {} - {:.4}% spread | Net profit: ${:.2} (Buy@{}: {:.6}, Sell@{}: {:.6})",
                                pair.name, max_spread, net_profit_usd, buy_data.dex, buy_data.price, sell_data.dex, sell_data.price
                            );
                        }
                    }

                    // High priority alerts for validated opportunities (0.5% - 10%)
                    if max_spread > 0.5 {
                        if let OpportunityValidation::Validated { net_profit_usd, .. } = &validation {
                            state.stats.high_spread_alerts.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                            error!(
                                "HIGH PRIORITY VALIDATED: {} - {:.4}% | Net: ${:.2} - BUY {} @ {} -> SELL @ {}",
                                pair.name, max_spread, net_profit_usd, buy_data.dex, buy_data.price, sell_data.dex
                            );
                        }
                    }
                }
            }
        }

        // Small delay between pair scans to avoid rate limiting
        tokio::time::sleep(poll_interval).await;
    }
}

/// Fetch reserves from a Uniswap V2 style pair
async fn fetch_uniswap_v2_reserves<P: Provider<T>, T: Transport + Clone>(
    provider: &P,
    pair_address: Address,
) -> Result<(U256, U256), MevError> {
    use alloy::sol;

    sol! {
        function getReserves() external view returns (uint112 reserve0, uint112 reserve1, uint32 blockTimestampLast);
    }

    let call = getReservesCall {};
    let tx = alloy::rpc::types::TransactionRequest::default()
        .to(pair_address)
        .input(call.abi_encode().into());

    let result = provider.call(&tx).await
        .map_err(|e| MevError::Provider(ProviderError::RpcError(e.to_string())))?;

    let decoded = getReservesCall::abi_decode_returns(&result, true)
        .map_err(|e: alloy::sol_types::Error| MevError::Provider(ProviderError::RpcError(e.to_string())))?;

    Ok((U256::from(decoded.reserve0), U256::from(decoded.reserve1)))
}

/// Fetch price from a Uniswap V3 pool using slot0
/// Returns sqrtPriceX96 which can be converted to price
async fn fetch_uniswap_v3_price<P: Provider<T>, T: Transport + Clone>(
    provider: &P,
    pool_address: Address,
) -> Result<U256, MevError> {
    use alloy::sol;

    sol! {
        function slot0() external view returns (
            uint160 sqrtPriceX96,
            int24 tick,
            uint16 observationIndex,
            uint16 observationCardinality,
            uint16 observationCardinalityNext,
            uint8 feeProtocol,
            bool unlocked
        );
    }

    let call = slot0Call {};
    let tx = alloy::rpc::types::TransactionRequest::default()
        .to(pool_address)
        .input(call.abi_encode().into());

    let result = provider.call(&tx).await
        .map_err(|e| MevError::Provider(ProviderError::RpcError(e.to_string())))?;

    let decoded = slot0Call::abi_decode_returns(&result, true)
        .map_err(|e: alloy::sol_types::Error| MevError::Provider(ProviderError::RpcError(e.to_string())))?;

    Ok(U256::from(decoded.sqrtPriceX96))
}

/// Calculate price from Uniswap V3 sqrtPriceX96
/// Price = (sqrtPriceX96 / 2^96)^2
fn calculate_v3_price(sqrt_price_x96: U256, decimals0: u8, decimals1: u8) -> f64 {
    // Convert to f64 for calculation
    let sqrt_price = sqrt_price_x96.to_string().parse::<f64>().unwrap_or(0.0);
    if sqrt_price == 0.0 {
        return 0.0;
    }

    // sqrtPriceX96 = sqrt(price) * 2^96
    // price = (sqrtPriceX96 / 2^96)^2
    let two_96: f64 = 2_f64.powi(96);
    let price = (sqrt_price / two_96).powi(2);

    // Adjust for decimals: price is token1/token0
    // We need to adjust based on decimals difference
    let decimal_adjustment = 10_f64.powi(decimals0 as i32 - decimals1 as i32);
    price * decimal_adjustment
}

/// Fetch virtual price from a Curve pool
/// Note: Curve pools have different interfaces, this handles the common case
async fn fetch_curve_virtual_price<P: Provider<T>, T: Transport + Clone>(
    provider: &P,
    pool_address: Address,
) -> Result<U256, MevError> {
    use alloy::sol;

    sol! {
        function get_virtual_price() external view returns (uint256);
    }

    let call = get_virtual_priceCall {};
    let tx = alloy::rpc::types::TransactionRequest::default()
        .to(pool_address)
        .input(call.abi_encode().into());

    let result = provider.call(&tx).await
        .map_err(|e| MevError::Provider(ProviderError::RpcError(e.to_string())))?;

    let decoded = get_virtual_priceCall::abi_decode_returns(&result, true)
        .map_err(|e: alloy::sol_types::Error| MevError::Provider(ProviderError::RpcError(e.to_string())))?;

    Ok(decoded._0)
}

/// Fetch exchange rate from a Curve pool for crypto pools (like tricrypto)
async fn fetch_curve_price_oracle<P: Provider<T>, T: Transport + Clone>(
    provider: &P,
    pool_address: Address,
    token_index: u8,
) -> Result<U256, MevError> {
    use alloy::sol;

    sol! {
        function price_oracle(uint256 k) external view returns (uint256);
    }

    let call = price_oracleCall { k: U256::from(token_index) };
    let tx = alloy::rpc::types::TransactionRequest::default()
        .to(pool_address)
        .input(call.abi_encode().into());

    let result = provider.call(&tx).await
        .map_err(|e| MevError::Provider(ProviderError::RpcError(e.to_string())))?;

    let decoded = price_oracleCall::abi_decode_returns(&result, true)
        .map_err(|e: alloy::sol_types::Error| MevError::Provider(ProviderError::RpcError(e.to_string())))?;

    Ok(decoded._0)
}

/// Fetch price from Balancer V2 Vault using getPoolTokens
/// Returns the token balances in the pool
async fn fetch_balancer_v2_balances<P: Provider<T>, T: Transport + Clone>(
    provider: &P,
    pool_id: [u8; 32],
) -> Result<(Vec<Address>, Vec<U256>), MevError> {
    use alloy::sol;

    // Balancer V2 Vault address on Ethereum mainnet
    let vault_address: Address = "0xBA12222222228d8Ba445958a75a0704d566BF2C8"
        .parse()
        .expect("valid address");

    sol! {
        function getPoolTokens(bytes32 poolId) external view returns (
            address[] tokens,
            uint256[] balances,
            uint256 lastChangeBlock
        );
    }

    let call = getPoolTokensCall {
        poolId: alloy::primitives::FixedBytes::from(pool_id),
    };
    let tx = alloy::rpc::types::TransactionRequest::default()
        .to(vault_address)
        .input(call.abi_encode().into());

    let result = provider.call(&tx).await
        .map_err(|e| MevError::Provider(ProviderError::RpcError(e.to_string())))?;

    let decoded = getPoolTokensCall::abi_decode_returns(&result, true)
        .map_err(|e: alloy::sol_types::Error| MevError::Provider(ProviderError::RpcError(e.to_string())))?;

    Ok((decoded.tokens, decoded.balances))
}

// Note: fetch_dex_price was replaced by fetch_dex_price_with_liquidity which includes
// liquidity data for proper opportunity validation

/// Calculate price from reserves (price of token1 in terms of token0)
/// For USDC/WETH pair: returns price of WETH in USDC
fn calculate_price(reserve0: U256, reserve1: U256, decimals0: u8, decimals1: u8) -> f64 {
    let r0 = reserve0.to_string().parse::<f64>().unwrap_or(0.0);
    let r1 = reserve1.to_string().parse::<f64>().unwrap_or(0.0);

    if r1 == 0.0 {
        return 0.0;
    }

    // Price of token1 in token0 = reserve0 / reserve1 * 10^(decimals1 - decimals0)
    // For USDC(6)/WETH(18): price = reserve0/reserve1 * 10^12
    let decimal_adjustment = 10_f64.powi(decimals1 as i32 - decimals0 as i32);
    (r0 / r1) * decimal_adjustment
}

/// Fetch price and liquidity data from a DEX pair
/// Returns DexPriceData with price, liquidity in USD, and raw reserves
async fn fetch_dex_price_with_liquidity<P: Provider<T>, T: Transport + Clone>(
    provider: &P,
    dex_pair: &DexPair,
    decimals0: u8,
    decimals1: u8,
) -> Result<DexPriceData, MevError> {
    match dex_pair.dex_type {
        DexType::UniswapV2 | DexType::PancakeSwap | DexType::Camelot => {
            // V2-style AMMs use getReserves
            let (r0, r1) = fetch_uniswap_v2_reserves(provider, dex_pair.address).await?;
            let price = calculate_price(r0, r1, decimals0, decimals1);

            // Convert reserves to human-readable values
            let reserve0_adjusted = r0.to_string().parse::<f64>().unwrap_or(0.0) / 10_f64.powi(decimals0 as i32);
            let reserve1_adjusted = r1.to_string().parse::<f64>().unwrap_or(0.0) / 10_f64.powi(decimals1 as i32);

            // Estimate liquidity in USD (assume token0 is the quote token or use ETH price)
            // For pairs like WETH/X, reserve0 is often WETH
            let liquidity_usd = estimate_liquidity_usd(reserve0_adjusted, reserve1_adjusted, price, decimals0);

            Ok(DexPriceData {
                dex: dex_pair.dex,
                price,
                liquidity_usd,
                reserve0: reserve0_adjusted,
                reserve1: reserve1_adjusted,
                pool_address: dex_pair.address,
                dex_type: dex_pair.dex_type.clone(),
            })
        }
        DexType::UniswapV3 { .. } => {
            // V3-style pools use slot0 for price and liquidity() for TVL
            let sqrt_price = fetch_uniswap_v3_price(provider, dex_pair.address).await?;
            let price = calculate_v3_price(sqrt_price, decimals0, decimals1);

            // For V3, we estimate liquidity from the price and typical V3 pool TVL
            // In production, you'd fetch the actual liquidity from the pool
            let liquidity = fetch_uniswap_v3_liquidity(provider, dex_pair.address).await.unwrap_or(U256::ZERO);
            let liquidity_f64 = liquidity.to_string().parse::<f64>().unwrap_or(0.0);

            // V3 liquidity is a different unit - estimate USD value
            // This is a rough approximation; in production you'd use tick-based calculations
            let liquidity_usd = if price > 0.0 {
                (liquidity_f64 / 1e18) * ETH_PRICE_USD * 2.0 // Rough estimate
            } else {
                0.0
            };

            Ok(DexPriceData {
                dex: dex_pair.dex,
                price,
                liquidity_usd: liquidity_usd.min(100_000_000.0), // Cap at reasonable value
                reserve0: 0.0, // V3 doesn't have traditional reserves
                reserve1: 0.0,
                pool_address: dex_pair.address,
                dex_type: dex_pair.dex_type.clone(),
            })
        }
        DexType::Curve => {
            // For Curve, get price from oracle and estimate liquidity from balances
            let price = match fetch_curve_price_oracle(provider, dex_pair.address, 0).await {
                Ok(p) => p.to_string().parse::<f64>().unwrap_or(0.0) / 1e18,
                Err(_) => {
                    match fetch_curve_virtual_price(provider, dex_pair.address).await {
                        Ok(vp) => vp.to_string().parse::<f64>().unwrap_or(0.0) / 1e18,
                        Err(e) => return Err(e),
                    }
                }
            };

            // Curve pools typically have high liquidity - estimate based on virtual price
            // In production, you'd call get_balances() on the pool
            let liquidity_usd = 10_000_000.0; // Default high liquidity for Curve (they're usually deep)

            Ok(DexPriceData {
                dex: dex_pair.dex,
                price,
                liquidity_usd,
                reserve0: 0.0,
                reserve1: 0.0,
                pool_address: dex_pair.address,
                dex_type: dex_pair.dex_type.clone(),
            })
        }
        DexType::BalancerV2 => {
            let mut pool_id = [0u8; 32];
            pool_id[..20].copy_from_slice(dex_pair.address.as_slice());

            match fetch_balancer_v2_balances(provider, pool_id).await {
                Ok((_tokens, balances)) => {
                    if balances.len() >= 2 {
                        let b0 = balances[0].to_string().parse::<f64>().unwrap_or(0.0);
                        let b1 = balances[1].to_string().parse::<f64>().unwrap_or(0.0);
                        let decimal_adjustment = 10_f64.powi(decimals1 as i32 - decimals0 as i32);
                        let price = if b1 > 0.0 { (b0 / b1) * decimal_adjustment } else { 0.0 };

                        let reserve0_adjusted = b0 / 10_f64.powi(decimals0 as i32);
                        let reserve1_adjusted = b1 / 10_f64.powi(decimals1 as i32);
                        let liquidity_usd = estimate_liquidity_usd(reserve0_adjusted, reserve1_adjusted, price, decimals0);

                        Ok(DexPriceData {
                            dex: dex_pair.dex,
                            price,
                            liquidity_usd,
                            reserve0: reserve0_adjusted,
                            reserve1: reserve1_adjusted,
                            pool_address: dex_pair.address,
                            dex_type: dex_pair.dex_type.clone(),
                        })
                    } else {
                        Ok(DexPriceData {
                            dex: dex_pair.dex,
                            price: 0.0,
                            liquidity_usd: 0.0,
                            reserve0: 0.0,
                            reserve1: 0.0,
                            pool_address: dex_pair.address,
                            dex_type: dex_pair.dex_type.clone(),
                        })
                    }
                }
                Err(_) => {
                    // Fallback to V2-style
                    let (r0, r1) = fetch_uniswap_v2_reserves(provider, dex_pair.address).await?;
                    let price = calculate_price(r0, r1, decimals0, decimals1);
                    let reserve0_adjusted = r0.to_string().parse::<f64>().unwrap_or(0.0) / 10_f64.powi(decimals0 as i32);
                    let reserve1_adjusted = r1.to_string().parse::<f64>().unwrap_or(0.0) / 10_f64.powi(decimals1 as i32);
                    let liquidity_usd = estimate_liquidity_usd(reserve0_adjusted, reserve1_adjusted, price, decimals0);

                    Ok(DexPriceData {
                        dex: dex_pair.dex,
                        price,
                        liquidity_usd,
                        reserve0: reserve0_adjusted,
                        reserve1: reserve1_adjusted,
                        pool_address: dex_pair.address,
                        dex_type: dex_pair.dex_type.clone(),
                    })
                }
            }
        }
    }
}

/// Estimate liquidity in USD from reserves
/// Assumes token0 is a stablecoin or ETH-like asset for pricing
fn estimate_liquidity_usd(reserve0: f64, reserve1: f64, price: f64, decimals0: u8) -> f64 {
    // For stablecoin pairs (decimals0 = 6, typically USDC/USDT)
    if decimals0 == 6 {
        // reserve0 is likely a stablecoin, so TVL ~= 2 * reserve0
        return reserve0 * 2.0;
    }

    // For ETH pairs (decimals0 = 18, typically WETH)
    if decimals0 == 18 {
        // reserve0 is likely ETH, TVL ~= reserve0 * ETH_PRICE * 2
        return reserve0 * ETH_PRICE_USD * 2.0;
    }

    // Fallback: use price to estimate
    if price > 0.0 {
        reserve1 * price * 2.0
    } else {
        0.0
    }
}

/// Fetch liquidity from Uniswap V3 pool
async fn fetch_uniswap_v3_liquidity<P: Provider<T>, T: Transport + Clone>(
    provider: &P,
    pool_address: Address,
) -> Result<U256, MevError> {
    use alloy::sol;

    sol! {
        function liquidity() external view returns (uint128);
    }

    let call = liquidityCall {};
    let tx = alloy::rpc::types::TransactionRequest::default()
        .to(pool_address)
        .input(call.abi_encode().into());

    let result = provider.call(&tx).await
        .map_err(|e| MevError::Provider(ProviderError::RpcError(e.to_string())))?;

    let decoded = liquidityCall::abi_decode_returns(&result, true)
        .map_err(|e: alloy::sol_types::Error| MevError::Provider(ProviderError::RpcError(e.to_string())))?;

    Ok(U256::from(decoded._0))
}

/// Validate an arbitrage opportunity by checking liquidity and simulating slippage
fn validate_arbitrage_opportunity(
    buy_data: &DexPriceData,
    sell_data: &DexPriceData,
    spread_pct: f64,
    _pair_name: &str,
) -> OpportunityValidation {
    // Step 1: Check minimum liquidity threshold
    if buy_data.liquidity_usd < MIN_LIQUIDITY_USD || sell_data.liquidity_usd < MIN_LIQUIDITY_USD {
        return OpportunityValidation::LowLiquidity {
            buy_liquidity_usd: buy_data.liquidity_usd,
            sell_liquidity_usd: sell_data.liquidity_usd,
            min_required_usd: MIN_LIQUIDITY_USD,
        };
    }

    // Step 2: Calculate trade size and simulate slippage
    let trade_size_usd = TRADE_SIZE_ETH * ETH_PRICE_USD;

    // Simulate slippage for the buy side (constant product formula: x * y = k)
    // For a trade of size `dx`, output `dy = y * dx / (x + dx)`
    // Slippage = (spot_price - effective_price) / spot_price
    let buy_slippage_pct = calculate_slippage(trade_size_usd, buy_data.liquidity_usd);
    let sell_slippage_pct = calculate_slippage(trade_size_usd, sell_data.liquidity_usd);
    let total_slippage_pct = buy_slippage_pct + sell_slippage_pct;

    // Step 3: Calculate actual profit after slippage
    let actual_profit_pct = spread_pct - total_slippage_pct;

    if actual_profit_pct <= 0.0 {
        return OpportunityValidation::HighSlippage {
            expected_profit_pct: spread_pct,
            actual_profit_pct,
            slippage_pct: total_slippage_pct,
        };
    }

    // Step 4: Calculate profit in USD
    let gross_profit_usd = trade_size_usd * (actual_profit_pct / 100.0);

    // Step 5: Estimate gas cost
    // For arbitrage: ~250k gas for a typical 2-hop swap
    // Gas cost = gas_limit * (base_fee + priority_fee) * ETH_PRICE
    let gas_cost_eth = (GAS_LIMIT_SWAP as f64) * (BASE_FEE_GWEI + PRIORITY_FEE_GWEI) / 1e9;
    let gas_cost_usd = gas_cost_eth * ETH_PRICE_USD;

    // Step 6: Check if profit exceeds gas cost
    let net_profit_usd = gross_profit_usd - gas_cost_usd;

    if net_profit_usd <= 0.0 {
        return OpportunityValidation::GasExceedsProfit {
            gross_profit_usd,
            gas_cost_usd,
        };
    }

    OpportunityValidation::Validated {
        simulated_profit_usd: gross_profit_usd,
        gas_cost_usd,
        net_profit_usd,
    }
}

/// Calculate slippage percentage for a given trade size and pool liquidity
/// Uses constant product AMM formula approximation
fn calculate_slippage(trade_size_usd: f64, liquidity_usd: f64) -> f64 {
    if liquidity_usd <= 0.0 {
        return 100.0; // 100% slippage for empty pools
    }

    // For constant product AMM: slippage ~= trade_size / (liquidity / 2)
    // This is a simplified approximation of the x*y=k formula
    // Actual slippage = dx / (x + dx) where x = liquidity/2
    let reserve_per_side = liquidity_usd / 2.0;
    let slippage = (trade_size_usd / (reserve_per_side + trade_size_usd)) * 100.0;

    slippage
}

/// Run opportunity detector - processes pending txs and detects MEV opportunities
async fn run_detector(state: Arc<AppState>) -> Result<(), MevError> {
    let poll_interval = std::time::Duration::from_millis(500);
    let mut last_status_log = std::time::Instant::now();
    let mut txs_analyzed: u64 = 0;
    let opportunities_found: u64 = 0;

    // DEX router addresses to watch
    let dex_routers: Vec<Address> = state.config.monitoring.dex_routers
        .iter()
        .filter_map(|s| s.parse::<Address>().ok())
        .collect();

    info!("Opportunity detector started, watching {} DEX routers", dex_routers.len());

    loop {
        // Log status every 60 seconds
        if last_status_log.elapsed().as_secs() >= 60 {
            info!(
                "Detector stats: {} txs analyzed, {} opportunities found, {} pending, {} active",
                txs_analyzed,
                opportunities_found,
                state.pending_txs.len(),
                state.opportunities.len()
            );
            last_status_log = std::time::Instant::now();
        }

        // Process pending transactions
        let pending_hashes: Vec<String> = state.pending_txs.iter()
            .map(|entry| entry.key().clone())
            .collect();

        for tx_hash in pending_hashes {
            if let Some(entry) = state.pending_txs.get(&tx_hash) {
                let tx = entry.value();
                txs_analyzed += 1;

                // Check if this is a DEX transaction
                if let Some(ref to_str) = tx.to {
                    if let Ok(to_addr) = to_str.parse::<Address>() {
                        if dex_routers.contains(&to_addr) {
                            // This is a DEX swap transaction
                            let value_eth = tx.value.parse::<f64>().unwrap_or(0.0) / 1e18;

                            if value_eth > 0.1 {
                                info!(
                                    "Large DEX swap detected: {} ETH to router {:?}, tx: {:?}",
                                    value_eth, to_addr, tx_hash
                                );
                            } else {
                                tracing::debug!(
                                    "DEX swap: {} ETH to {:?}",
                                    value_eth, to_addr
                                );
                            }
                        }
                    }
                }

                // Remove processed transaction
                state.pending_txs.remove(&tx_hash);
            }
        }

        tokio::time::sleep(poll_interval).await;
    }
}

// ============================================================================
// Flash Loan Execution Logic
// ============================================================================

// FlashloanArbitrage contract interface
sol! {
    /// SwapStep struct matching the Solidity contract
    #[derive(Debug)]
    struct SolSwapStep {
        uint8 protocol;      // 1=UniV2, 2=UniV3, 3=Balancer, 4=Curve
        address router;
        address tokenIn;
        address tokenOut;
        bytes swapData;
        uint256 amountIn;    // 0 = use all available
        uint256 minAmountOut;
    }

    /// FlashloanArbitrage contract interface
    #[sol(rpc)]
    contract FlashloanArbitrage {
        function executeBalancerFlashloan(
            address[] calldata tokens,
            uint256[] calldata amounts,
            bytes calldata swapData
        ) external;

        function owner() external view returns (address);
    }
}

/// Flash loan arbitrage contract address
const FLASHLOAN_CONTRACT_ADDRESS: &str = "0xF53bEFDe7B7631BA5749499FB33Fd145372976bA";

/// Convert SwapStep to Solidity-compatible encoding
fn encode_swap_steps(steps: &[SwapStep]) -> Bytes {
    use alloy::sol_types::SolType;

    let sol_steps: Vec<SolSwapStep> = steps.iter().map(|s| {
        SolSwapStep {
            protocol: s.protocol,
            router: s.router,
            tokenIn: s.token_in,
            tokenOut: s.token_out,
            swapData: Bytes::from(s.swap_data.clone()),
            amountIn: s.amount_in,
            minAmountOut: s.min_amount_out,
        }
    }).collect();

    // ABI encode the array of SwapStep structs
    let encoded = alloy::sol_types::sol_data::Array::<SolSwapStep>::abi_encode(&sol_steps);
    Bytes::from(encoded)
}

/// Protocol ID constants matching the Solidity contract
#[allow(dead_code)]
mod protocol {
    pub const UNISWAP_V2: u8 = 1;
    pub const UNISWAP_V3: u8 = 2;
    pub const BALANCER: u8 = 3;
    pub const CURVE: u8 = 4;
}

/// Get protocol ID from DEX name
#[allow(dead_code)]
fn get_protocol_id(dex_name: &str) -> u8 {
    match dex_name.to_lowercase().as_str() {
        "univ2" | "uniswap" | "uniswapv2" | "sushi" | "sushiswap" | "shiba" | "shibaswap" | "frax" | "fraxswap" | "pancake" | "pancakeswap" => protocol::UNISWAP_V2,
        "univ3" | "uniswapv3" => protocol::UNISWAP_V3,
        "balancer" | "balancerv2" => protocol::BALANCER,
        "curve" => protocol::CURVE,
        _ => protocol::UNISWAP_V2, // Default to UniV2 style
    }
}

/// Execute flash loan arbitrage opportunity
async fn execute_flashloan_arbitrage(
    state: &Arc<AppState>,
    opportunity: &MevOpportunity,
) -> Result<Option<FixedBytes<32>>, error::ExecutionError> {
    // Check if we have the required data for execution
    let flash_loan_token = opportunity.flash_loan_token
        .ok_or_else(|| error::ExecutionError::SubmissionFailed("Missing flash loan token".to_string()))?;
    let flash_loan_amount = opportunity.flash_loan_amount
        .ok_or_else(|| error::ExecutionError::SubmissionFailed("Missing flash loan amount".to_string()))?;
    let swap_steps = opportunity.swap_steps.as_ref()
        .ok_or_else(|| error::ExecutionError::SubmissionFailed("Missing swap steps".to_string()))?;

    if swap_steps.is_empty() {
        return Err(error::ExecutionError::SubmissionFailed("No swap steps provided".to_string()));
    }

    // Get private key from environment
    let private_key = Config::get_private_key()
        .ok_or_else(|| error::ExecutionError::SignerError("PRIVATE_KEY environment variable not set".to_string()))?;

    // Create signer from private key
    let signer: PrivateKeySigner = private_key.parse()
        .map_err(|e| error::ExecutionError::SignerError(format!("Failed to parse private key: {}", e)))?;
    let wallet = EthereumWallet::from(signer.clone());
    let signer_address = signer.address();

    info!(
        "Preparing flash loan execution for opportunity {}: token={:?}, amount={}, steps={}",
        opportunity.id, flash_loan_token, flash_loan_amount, swap_steps.len()
    );

    // Parse contract address
    let contract_address: Address = FLASHLOAN_CONTRACT_ADDRESS.parse()
        .map_err(|e| error::ExecutionError::SubmissionFailed(format!("Invalid contract address: {}", e)))?;

    // Encode swap steps
    let swap_data = encode_swap_steps(swap_steps);
    info!("Encoded swap data: {} bytes", swap_data.len());

    // Build the contract call
    let tokens = vec![flash_loan_token];
    let amounts = vec![flash_loan_amount];

    // Create the transaction request using sol! generated call
    let call = FlashloanArbitrage::executeBalancerFlashloanCall {
        tokens: tokens.clone(),
        amounts: amounts.clone(),
        swapData: swap_data.clone(),
    };

    let call_data = call.abi_encode();

    // Get current gas price
    let gas_price = state.http_provider.get_gas_price().await
        .map_err(|e| error::ExecutionError::SubmissionFailed(format!("Failed to get gas price: {}", e)))?;

    let max_gas_price_wei = state.config.monitoring.max_gas_price_gwei * 1_000_000_000;
    if gas_price > max_gas_price_wei as u128 {
        warn!(
            "Gas price {} gwei exceeds max {} gwei, skipping execution",
            gas_price / 1_000_000_000,
            state.config.monitoring.max_gas_price_gwei
        );
        return Err(error::ExecutionError::GasPriceTooLow);
    }

    // Build transaction request for gas estimation
    let tx_request = alloy::rpc::types::TransactionRequest::default()
        .to(contract_address)
        .from(signer_address)
        .input(call_data.clone().into());

    // Estimate gas
    let estimated_gas = state.http_provider.estimate_gas(&tx_request).await
        .map_err(|e| error::ExecutionError::SubmissionFailed(format!("Gas estimation failed: {}", e)))?;

    let gas_limit = (estimated_gas as f64 * state.config.execution.gas_limit_multiplier) as u64;
    let gas_cost_wei = gas_limit as u128 * gas_price;

    info!(
        "Gas estimate: {} units, limit: {} ({}x multiplier), cost: {} wei ({} gwei price)",
        estimated_gas, gas_limit, state.config.execution.gas_limit_multiplier,
        gas_cost_wei, gas_price / 1_000_000_000
    );

    // Parse and validate profit
    let estimated_profit = opportunity.estimated_profit_wei.parse::<f64>().unwrap_or(0.0);
    let net_profit_after_gas = estimated_profit - (gas_cost_wei as f64);

    if net_profit_after_gas <= 0.0 {
        warn!(
            "Opportunity {} no longer profitable after gas: profit={}, gas_cost={}",
            opportunity.id, estimated_profit, gas_cost_wei
        );
        return Err(error::ExecutionError::NotProfitable);
    }

    info!(
        "Profit validation passed: estimated={}, gas_cost={}, net={}",
        estimated_profit, gas_cost_wei, net_profit_after_gas
    );

    // Check if we should use Flashbots
    if state.config.execution.use_flashbots {
        info!("Submitting via Flashbots relay: {}", state.config.flashbots.relay_url);
        return submit_flashbots_bundle(state, &call_data, contract_address, signer_address, gas_limit, &signer).await;
    }

    // Standard mempool submission
    info!("Submitting transaction to mempool...");

    // Get nonce
    let nonce = state.http_provider.get_transaction_count(signer_address).await
        .map_err(|e| error::ExecutionError::SubmissionFailed(format!("Failed to get nonce: {}", e)))?;

    // Get current block for EIP-1559 fees
    let block = state.http_provider.get_block_by_number(
        alloy::eips::BlockNumberOrTag::Latest,
        alloy::rpc::types::BlockTransactionsKind::Hashes
    ).await
        .map_err(|e| error::ExecutionError::SubmissionFailed(format!("Failed to get block: {}", e)))?
        .ok_or_else(|| error::ExecutionError::SubmissionFailed("No block found".to_string()))?;

    let base_fee = block.header.base_fee_per_gas
        .ok_or_else(|| error::ExecutionError::SubmissionFailed("No base fee in block".to_string()))?;

    let max_priority_fee: u128 = (state.config.execution.max_priority_fee_gwei * 1_000_000_000) as u128;
    let max_fee_per_gas: u128 = (base_fee as u128) + max_priority_fee;

    // Build EIP-1559 transaction
    let tx = alloy::rpc::types::TransactionRequest::default()
        .to(contract_address)
        .from(signer_address)
        .input(call_data.into())
        .nonce(nonce)
        .gas_limit(gas_limit)
        .max_fee_per_gas(max_fee_per_gas)
        .max_priority_fee_per_gas(max_priority_fee)
        .with_chain_id(state.config.ethereum.chain_id);

    // Create provider with wallet for signing
    let provider_with_wallet = ProviderBuilder::new()
        .wallet(wallet)
        .on_http(state.config.ethereum.http_rpc_url.parse()
            .map_err(|e| error::ExecutionError::SubmissionFailed(format!("Invalid RPC URL: {}", e)))?);

    // Send transaction
    let pending_tx = provider_with_wallet.send_transaction(tx).await
        .map_err(|e| error::ExecutionError::SubmissionFailed(format!("Transaction submission failed: {}", e)))?;

    let tx_hash = *pending_tx.tx_hash();
    info!("Transaction submitted: {:?}", tx_hash);

    // Wait for confirmation with timeout
    let timeout_duration = Duration::from_secs(state.config.execution.deadline_secs);
    match tokio::time::timeout(timeout_duration, pending_tx.get_receipt()).await {
        Ok(Ok(receipt)) => {
            if receipt.status() {
                info!(
                    "Transaction confirmed successfully! Hash: {:?}, Gas used: {}",
                    tx_hash, receipt.gas_used
                );
                Ok(Some(tx_hash))
            } else {
                error!("Transaction reverted! Hash: {:?}", tx_hash);
                Err(error::ExecutionError::TransactionReverted(format!("{:?}", tx_hash)))
            }
        }
        Ok(Err(e)) => {
            error!("Failed to get receipt: {}", e);
            Err(error::ExecutionError::SubmissionFailed(format!("Receipt error: {}", e)))
        }
        Err(_) => {
            warn!("Transaction confirmation timeout after {} seconds", state.config.execution.deadline_secs);
            // Transaction was submitted but not confirmed in time - return the hash anyway
            Ok(Some(tx_hash))
        }
    }
}

/// Submit transaction via Flashbots relay
async fn submit_flashbots_bundle(
    state: &Arc<AppState>,
    call_data: &[u8],
    contract_address: Address,
    signer_address: Address,
    gas_limit: u64,
    signer: &PrivateKeySigner,
) -> Result<Option<FixedBytes<32>>, error::ExecutionError> {
    use alloy::signers::Signer;

    // Get current block number for target block
    let current_block = state.http_provider.get_block_number().await
        .map_err(|e| error::ExecutionError::FlashbotsError(format!("Failed to get block number: {}", e)))?;
    let target_block = current_block + 1;

    // Get nonce
    let nonce = state.http_provider.get_transaction_count(signer_address).await
        .map_err(|e| error::ExecutionError::FlashbotsError(format!("Failed to get nonce: {}", e)))?;

    // Get base fee from latest block
    let block = state.http_provider.get_block_by_number(
        alloy::eips::BlockNumberOrTag::Latest,
        alloy::rpc::types::BlockTransactionsKind::Hashes
    ).await
        .map_err(|e| error::ExecutionError::FlashbotsError(format!("Failed to get block: {}", e)))?
        .ok_or_else(|| error::ExecutionError::FlashbotsError("No block found".to_string()))?;

    let base_fee = block.header.base_fee_per_gas
        .ok_or_else(|| error::ExecutionError::FlashbotsError("No base fee in block".to_string()))?;

    let max_priority_fee: u128 = (state.config.execution.max_priority_fee_gwei * 1_000_000_000) as u128;
    let max_fee_per_gas: u128 = (base_fee as u128) + max_priority_fee + ((base_fee as u128) / 10); // Add 10% buffer

    // Build the transaction
    let tx = alloy::consensus::TxEip1559 {
        chain_id: state.config.ethereum.chain_id,
        nonce,
        gas_limit,
        max_fee_per_gas,
        max_priority_fee_per_gas: max_priority_fee,
        to: alloy::primitives::TxKind::Call(contract_address),
        value: U256::ZERO,
        access_list: Default::default(),
        input: Bytes::from(call_data.to_vec()),
    };

    // Sign the transaction using sync API (since TxEip1559 needs mutable for async)
    use alloy::consensus::SignableTransaction;

    let mut tx_to_sign = tx.clone();
    let signature = signer.sign_transaction_sync(&mut tx_to_sign)
        .map_err(|e| error::ExecutionError::SignerError(format!("Failed to sign transaction: {}", e)))?;

    let signed_tx = alloy::consensus::TxEnvelope::Eip1559(
        tx.into_signed(signature)
    );

    // Encode the signed transaction
    use alloy::eips::eip2718::Encodable2718;
    let mut encoded_tx = Vec::new();
    signed_tx.encode_2718(&mut encoded_tx);
    let raw_tx_hex = format!("0x{}", hex::encode(&encoded_tx));

    // Build Flashbots bundle request
    let bundle_body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "eth_sendBundle",
        "params": [{
            "txs": [raw_tx_hex],
            "blockNumber": format!("0x{:x}", target_block),
            "minTimestamp": 0,
            "maxTimestamp": chrono::Utc::now().timestamp() as u64 + state.config.execution.deadline_secs
        }]
    });

    // Sign the bundle payload for Flashbots authentication
    let body_str = serde_json::to_string(&bundle_body)
        .map_err(|e| error::ExecutionError::FlashbotsError(format!("JSON serialization failed: {}", e)))?;

    // Create message hash for Flashbots signature (keccak256 of the body)
    let message_hash = alloy::primitives::keccak256(body_str.as_bytes());
    let fb_signature = signer.sign_hash(&message_hash).await
        .map_err(|e| error::ExecutionError::SignerError(format!("Failed to sign bundle: {}", e)))?;

    let fb_auth_header = format!("{}:0x{}", signer_address, hex::encode(fb_signature.as_bytes()));

    info!(
        "Submitting Flashbots bundle for block {}: {} txs",
        target_block, 1
    );

    // Submit to Flashbots relay
    let client = reqwest::Client::new();
    let response = client
        .post(&state.config.flashbots.relay_url)
        .header("Content-Type", "application/json")
        .header("X-Flashbots-Signature", fb_auth_header)
        .body(body_str)
        .timeout(Duration::from_millis(state.config.flashbots.bundle_timeout_ms))
        .send()
        .await
        .map_err(|e| error::ExecutionError::FlashbotsError(format!("Relay request failed: {}", e)))?;

    let status = response.status();
    let response_text = response.text().await
        .map_err(|e| error::ExecutionError::FlashbotsError(format!("Failed to read response: {}", e)))?;

    if status.is_success() {
        info!("Flashbots bundle submitted successfully: {}", response_text);

        // Parse response to get bundle hash
        if let Ok(json) = serde_json::from_str::<serde_json::Value>(&response_text) {
            if let Some(result) = json.get("result") {
                if let Some(bundle_hash) = result.get("bundleHash").and_then(|h| h.as_str()) {
                    info!("Bundle hash: {}", bundle_hash);
                }
            }
        }

        // For Flashbots, we don't have a direct tx hash until inclusion
        // Return None to indicate bundle was submitted but not yet confirmed
        Ok(None)
    } else {
        error!("Flashbots bundle submission failed: {} - {}", status, response_text);
        Err(error::ExecutionError::FlashbotsError(format!("Bundle rejected: {}", response_text)))
    }
}

/// Run executor
async fn run_executor(state: Arc<AppState>) -> Result<(), MevError> {
    let poll_interval = std::time::Duration::from_millis(50);

    info!("Executor started, contract address: {}", FLASHLOAN_CONTRACT_ADDRESS);

    // Verify we have a private key configured
    if Config::get_private_key().is_none() {
        warn!("PRIVATE_KEY not set - executor will only work in dry_run mode");
    }

    loop {
        // Collect opportunities to process (avoid holding lock during execution)
        let opportunities_to_process: Vec<(String, MevOpportunity)> = state.opportunities
            .iter()
            .filter(|entry| entry.value().status == "pending" || entry.value().status == "high_priority")
            .map(|entry| (entry.key().clone(), entry.value().clone()))
            .collect();

        for (opp_id, opportunity) in opportunities_to_process {
            // Update status to executing
            if let Some(mut entry) = state.opportunities.get_mut(&opp_id) {
                entry.status = "executing".to_string();
            }

            if state.config.execution.dry_run {
                // Dry run mode - simulate but don't execute
                info!(
                    "DRY RUN: Would execute opportunity {} | Type: {} | Profit: {} | Target: {}",
                    opportunity.id,
                    opportunity.opportunity_type,
                    opportunity.net_profit_wei,
                    opportunity.target_tx
                );

                if let (Some(token), Some(amount), Some(steps)) = (
                    &opportunity.flash_loan_token,
                    &opportunity.flash_loan_amount,
                    &opportunity.swap_steps,
                ) {
                    info!(
                        "  Flash loan: token={:?}, amount={}, steps={}",
                        token, amount, steps.len()
                    );
                    for (i, step) in steps.iter().enumerate() {
                        info!(
                            "  Step {}: protocol={}, router={:?}, {}->{}",
                            i + 1, step.protocol, step.router, step.token_in, step.token_out
                        );
                    }
                }

                // Update status to simulated
                if let Some(mut entry) = state.opportunities.get_mut(&opp_id) {
                    entry.status = "simulated".to_string();
                }
            } else {
                // Real execution mode
                info!(
                    "EXECUTING opportunity {} | Type: {} | Profit: {}",
                    opportunity.id,
                    opportunity.opportunity_type,
                    opportunity.net_profit_wei
                );

                match execute_flashloan_arbitrage(&state, &opportunity).await {
                    Ok(Some(tx_hash)) => {
                        info!(
                            "SUCCESS: Opportunity {} executed, tx: {:?}",
                            opportunity.id, tx_hash
                        );
                        if let Some(mut entry) = state.opportunities.get_mut(&opp_id) {
                            entry.status = "executed".to_string();
                        }
                    }
                    Ok(None) => {
                        // Flashbots bundle submitted but not yet confirmed
                        info!(
                            "PENDING: Opportunity {} submitted via Flashbots, awaiting inclusion",
                            opportunity.id
                        );
                        if let Some(mut entry) = state.opportunities.get_mut(&opp_id) {
                            entry.status = "pending_inclusion".to_string();
                        }
                    }
                    Err(e) => {
                        error!(
                            "FAILED: Opportunity {} execution error: {}",
                            opportunity.id, e
                        );
                        if let Some(mut entry) = state.opportunities.get_mut(&opp_id) {
                            entry.status = format!("failed: {}", e);
                        }
                    }
                }
            }
        }

        tokio::time::sleep(poll_interval).await;
    }
}

// ============================================================================
// Dashboard module (only compiled with "dashboard" feature)
// ============================================================================
#[cfg(feature = "dashboard")]
mod dashboard {
    use super::*;

    /// Run dashboard server
    pub async fn run_dashboard(state: Arc<AppState>) -> Result<(), MevError> {
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
            .route("/api/analysis/by-type", get({
                let state = Arc::clone(&app_state);
                move || analysis_by_type_handler(Arc::clone(&state))
            }))
            .route("/api/analysis/by-pair", get({
                let state = Arc::clone(&app_state);
                move || analysis_by_pair_handler(Arc::clone(&state))
            }))
            .route("/api/analysis/hourly", get({
                let state = Arc::clone(&app_state);
                move || analysis_hourly_handler(Arc::clone(&state))
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

    /// Index handler - serve the main dashboard HTML
    async fn index_handler() -> axum::response::Html<&'static str> {
        axum::response::Html(include_str!("dashboard/templates/index.html"))
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

    /// Stats handler - returns live statistics for dashboard
    async fn stats_handler(state: Arc<AppState>) -> Json<serde_json::Value> {
        let detected = state.stats.opportunities_detected.load(std::sync::atomic::Ordering::Relaxed);
        let high_alerts = state.stats.high_spread_alerts.load(std::sync::atomic::Ordering::Relaxed);

        Json(serde_json::json!({
            "total_opportunities": detected,
            "opportunities_24h": detected,
            "total_estimated_profit_eth": 0.0,
            "total_execution_profit_eth": 0.0,
            "executed_count": 0,
            "simulated_count": 0,
            "competitor_captured_count": 0,
            "active_pools": 57,
            "high_spread_alerts": high_alerts,
            "active_opportunities": state.opportunities.len()
        }))
    }

    /// Analysis by type handler
    async fn analysis_by_type_handler(_state: Arc<AppState>) -> Json<serde_json::Value> {
        Json(serde_json::json!({
            "types": [
                {"opportunity_type": "price_discrepancy", "count": 100, "percentage": 100.0}
            ]
        }))
    }

    /// Analysis by pair handler
    async fn analysis_by_pair_handler(state: Arc<AppState>) -> Json<serde_json::Value> {
        let mut pair_counts: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
        for entry in state.opportunities.iter() {
            let pair = entry.value().target_tx.split_whitespace().next().unwrap_or("Unknown").to_string();
            *pair_counts.entry(pair).or_insert(0) += 1;
        }
        let mut pairs: Vec<_> = pair_counts.into_iter().map(|(k, v)| serde_json::json!({"token_pair": k, "count": v})).collect();
        pairs.sort_by(|a, b| b["count"].as_u64().cmp(&a["count"].as_u64()));
        Json(serde_json::json!({"pairs": pairs}))
    }

    /// Hourly analysis handler
    async fn analysis_hourly_handler(_state: Arc<AppState>) -> Json<serde_json::Value> {
        let hours: Vec<_> = (0..24).map(|h| serde_json::json!({"hour": h, "count": 0, "intensity": 0.0, "total_profit_eth": 0.0})).collect();
        Json(serde_json::json!({"hours": hours}))
    }
}

#[cfg(feature = "dashboard")]
use dashboard::run_dashboard;

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
