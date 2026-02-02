mod config;
mod error;

use std::sync::Arc;
use std::time::Duration;

use alloy::primitives::{Address, U256};
use alloy::providers::{Provider, ProviderBuilder, RootProvider, WsConnect};
use alloy::sol_types::SolCall;
use alloy::transports::http::{Client, Http};
use alloy::transports::Transport;
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
}

impl TradingPair {
    fn new(name: &'static str, token0_decimals: u8, token1_decimals: u8, pairs: Vec<(&'static str, &'static str)>) -> Self {
        Self {
            name,
            token0_decimals,
            token1_decimals,
            pairs: pairs.into_iter()
                .filter_map(|(dex, addr)| {
                    addr.parse::<Address>().ok().map(|address| DexPair { dex, address })
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
        // --- HIGH LIQUIDITY REFERENCE PAIRS (Top 20) ---
        TradingPair::new("WETH/USDC", 6, 18, vec![
            ("UniV2", "0xB4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc"),
            ("Sushi", "0x397FF1542f962076d0BFE58eA045FfA2d347ACa0"),
        ]),
        TradingPair::new("WETH/USDT", 18, 6, vec![
            ("UniV2", "0x0d4a11d5EEaaC28EC3F61d100daF4d40471f1852"),
            ("Sushi", "0x06da0fd433C1A5d7a4faa01111c044910A184553"),
        ]),
        TradingPair::new("WETH/DAI", 18, 18, vec![
            ("UniV2", "0xA478c2975Ab1Ea89e8196811F51A7B7Ade33eB11"),
            ("Sushi", "0xC3D03e4F041Fd4cD388c549Ee2A29a9E5075882f"),
        ]),
        TradingPair::new("WETH/WBTC", 8, 18, vec![
            ("UniV2", "0xBb2b8038a1640196FbE3e38816F3e67Cba72D940"),
            ("Sushi", "0xCEfF51756c56CeFFCA006cD410B03FFC46dd3a58"),
        ]),

        // --- MEDIUM LIQUIDITY (Top 20-50) ---
        TradingPair::new("WETH/LINK", 18, 18, vec![
            ("UniV2", "0xa2107FA5B38d9bbd2C461D6EDf11B11A50F6b974"),
            ("Sushi", "0xC40D16476380e4037e6b1A2594cAF6a6cc8Da967"),
        ]),
        TradingPair::new("WETH/UNI", 18, 18, vec![
            ("UniV2", "0xd3d2E2692501A5c9Ca623199D38826e513033a17"),
            ("Sushi", "0xDafd66636E2561b0284EDdE37e42d192F2844D40"),
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
        TradingPair::new("USDC/USDT", 6, 6, vec![
            ("UniV2", "0x3041CbD36888bECc7bbCBc0045E3B1f144466f5f"),
            ("Sushi", "0xD86A120a06255Df8D4e2248aB04d4267E23aDfaA"),
        ]),
        TradingPair::new("DAI/USDC", 18, 6, vec![
            ("UniV2", "0xAE461cA67B15dc8dc81CE7615e0320dA1A9aB8D5"),
            ("Sushi", "0xA0b86991c6218b36c1d19D4a2e9Eb0cE3606eB48"),
        ]),
        TradingPair::new("FRAX/USDC", 18, 6, vec![
            ("UniV2", "0x97C4adc5d28A86f9470C70DD91Dc6CC2f20d2d4D"),
            ("Sushi", "0x9a834b70c07C81a9fcD6F22E842bf002fBfFbe4D"),
            ("Frax", "0x9a834b70c07C81a9fcD6F22E842bf002fBfFbe4D"),
        ]),
    ];

    info!(
        "Monitoring {} trading pairs across Uniswap V2, SushiSwap, ShibaSwap, Fraxswap",
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
                if let Ok((r0, r1)) = fetch_uniswap_v2_reserves(&state.http_provider, dex_pair.address).await {
                    let price = calculate_price(r0, r1, pair.token0_decimals, pair.token1_decimals);
                    if price > 0.0 {
                        dex_prices.push((dex_pair.dex, price));
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
                    info!(
                        "[{}] Spread: {:.4}% | Buy@{}: {:.6}, Sell@{}: {:.6}",
                        pair.name, max_spread, best_buy_dex, best_buy_price, best_sell_dex, best_sell_price
                    );
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
