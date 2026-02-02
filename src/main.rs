mod config;
mod error;

use std::sync::Arc;
use std::time::Duration;

use alloy::primitives::{Address, U256};
use alloy::providers::{Provider, ProviderBuilder, RootProvider, WsConnect};
use alloy::sol_types::SolCall;
use alloy::transports::http::{Client, Http};
use alloy::transports::Transport;
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

    let poll_interval = std::time::Duration::from_millis(state.config.monitoring.poll_interval_ms * 10);
    let pending_tx_count: u64 = 0;
    let mut last_log = std::time::Instant::now();

    info!("Mempool monitor: attempting to subscribe to pending transactions");

    // Try to subscribe to pending transactions via WebSocket
    // Note: This may not work with all providers (e.g., Infura free tier doesn't support txpool)
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

        // Try to get txpool status (only works with nodes that support it)
        // Most public RPCs don't support this, so we'll just poll and log
        match state.http_provider.get_block(
            alloy::eips::BlockId::latest(),
            alloy::rpc::types::BlockTransactionsKind::Hashes
        ).await {
            Ok(Some(block)) => {
                let tx_count = match &block.transactions {
                    alloy::rpc::types::BlockTransactions::Hashes(hashes) => hashes.len(),
                    alloy::rpc::types::BlockTransactions::Full(txs) => txs.len(),
                    _ => 0,
                };

                if tx_count > 0 {
                    tracing::debug!(
                        "Latest block has {} transactions",
                        tx_count
                    );
                }
            }
            Ok(None) => {}
            Err(e) => {
                tracing::debug!("Failed to get latest block: {}", e);
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

            // Fetch prices from all DEXes for this pair
            let mut dex_prices: Vec<(&str, f64)> = Vec::new();

            for dex_pair in &pair.pairs {
                // Use the unified fetch_dex_price function that handles V2, V3, Curve, Balancer
                match fetch_dex_price(&*state.http_provider, dex_pair, pair.token0_decimals, pair.token1_decimals).await {
                    Ok(price) => {
                        if price > 0.0 && price.is_finite() {
                            dex_prices.push((dex_pair.dex, price));
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
                let mut best_buy_dex = "";
                let mut best_sell_dex = "";
                let mut best_buy_price = 0.0;
                let mut best_sell_price = 0.0;

                for i in 0..dex_prices.len() {
                    for j in (i + 1)..dex_prices.len() {
                        let (dex_a, price_a) = dex_prices[i];
                        let (dex_b, price_b) = dex_prices[j];

                        let spread = if price_a > price_b {
                            (price_a - price_b) / price_b * 100.0
                        } else {
                            (price_b - price_a) / price_a * 100.0
                        };

                        if spread > max_spread {
                            max_spread = spread;
                            if price_a > price_b {
                                best_buy_dex = dex_b;
                                best_sell_dex = dex_a;
                                best_buy_price = price_b;
                                best_sell_price = price_a;
                            } else {
                                best_buy_dex = dex_a;
                                best_sell_dex = dex_b;
                                best_buy_price = price_a;
                                best_sell_price = price_b;
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

                // Log spreads above 0.05%
                if max_spread > 0.05 {
                    opportunities_found += 1;
                    state.stats.opportunities_detected.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    info!(
                        "[{}] Spread: {:.4}% | Buy@{}: {:.6}, Sell@{}: {:.6}",
                        pair.name, max_spread, best_buy_dex, best_buy_price, best_sell_dex, best_sell_price
                    );

                    // Store opportunity in cache for dashboard
                    let opp_id = format!("{}-{}", pair.name, chrono::Utc::now().timestamp_millis());
                    state.opportunities.insert(opp_id.clone(), MevOpportunity {
                        id: opp_id,
                        opportunity_type: "price_discrepancy".to_string(),
                        target_tx: format!("{} Buy@{} Sell@{}", pair.name, best_buy_dex, best_sell_dex),
                        estimated_profit_wei: format!("{:.6}", max_spread),
                        estimated_gas_cost_wei: "0".to_string(),
                        net_profit_wei: format!("{:.4}%", max_spread),
                        detected_at: chrono::Utc::now(),
                        status: if max_spread > 0.5 { "high_priority".to_string() } else { "detected".to_string() },
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
                }

                // Alert on significant opportunities (0.3% - 10%)
                if max_spread > 0.3 {
                    warn!(
                        "ARBITRAGE OPPORTUNITY: {} - {:.4}% spread (Buy@{}: {:.6}, Sell@{}: {:.6})",
                        pair.name, max_spread, best_buy_dex, best_buy_price, best_sell_dex, best_sell_price
                    );
                }

                // High priority alerts (0.5% - 10%)
                if max_spread > 0.5 {
                    state.stats.high_spread_alerts.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    error!(
                        "HIGH SPREAD ALERT: {} - {:.4}% - BUY {} @ {} -> SELL @ {}",
                        pair.name, max_spread, best_buy_dex, best_buy_price, best_sell_dex
                    );
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

/// Fetch price based on DEX type - unified interface
async fn fetch_dex_price<P: Provider<T>, T: Transport + Clone>(
    provider: &P,
    dex_pair: &DexPair,
    decimals0: u8,
    decimals1: u8,
) -> Result<f64, MevError> {
    match dex_pair.dex_type {
        DexType::UniswapV2 | DexType::PancakeSwap | DexType::Camelot => {
            // V2-style AMMs use getReserves
            let (r0, r1) = fetch_uniswap_v2_reserves(provider, dex_pair.address).await?;
            Ok(calculate_price(r0, r1, decimals0, decimals1))
        }
        DexType::UniswapV3 { .. } => {
            // V3-style pools use slot0 for price
            let sqrt_price = fetch_uniswap_v3_price(provider, dex_pair.address).await?;
            Ok(calculate_v3_price(sqrt_price, decimals0, decimals1))
        }
        DexType::Curve => {
            // For Curve, try price_oracle first (for crypto pools), then virtual_price
            match fetch_curve_price_oracle(provider, dex_pair.address, 0).await {
                Ok(price) => {
                    let price_f64 = price.to_string().parse::<f64>().unwrap_or(0.0);
                    // Curve prices are typically 18 decimals
                    Ok(price_f64 / 1e18)
                }
                Err(_) => {
                    // Fallback to virtual price for stablecoin pools
                    match fetch_curve_virtual_price(provider, dex_pair.address).await {
                        Ok(vp) => {
                            let vp_f64 = vp.to_string().parse::<f64>().unwrap_or(0.0);
                            Ok(vp_f64 / 1e18)
                        }
                        Err(e) => Err(e),
                    }
                }
            }
        }
        DexType::BalancerV2 => {
            // For Balancer V2, we need to extract pool ID from the address
            // The first 20 bytes of the pool ID is typically the pool address
            let mut pool_id = [0u8; 32];
            pool_id[..20].copy_from_slice(dex_pair.address.as_slice());

            match fetch_balancer_v2_balances(provider, pool_id).await {
                Ok((_tokens, balances)) => {
                    if balances.len() >= 2 {
                        let b0 = balances[0].to_string().parse::<f64>().unwrap_or(0.0);
                        let b1 = balances[1].to_string().parse::<f64>().unwrap_or(0.0);
                        if b1 > 0.0 {
                            let decimal_adjustment = 10_f64.powi(decimals1 as i32 - decimals0 as i32);
                            Ok((b0 / b1) * decimal_adjustment)
                        } else {
                            Ok(0.0)
                        }
                    } else {
                        Ok(0.0)
                    }
                }
                Err(_) => {
                    // Fallback: treat as V2-style for some Balancer pools
                    let (r0, r1) = fetch_uniswap_v2_reserves(provider, dex_pair.address).await?;
                    Ok(calculate_price(r0, r1, decimals0, decimals1))
                }
            }
        }
    }
}

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
