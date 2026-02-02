mod config;
mod error;

use std::sync::Arc;

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

/// Run mempool monitor - subscribes to pending transactions via WebSocket
async fn run_mempool_monitor(state: Arc<AppState>) -> Result<(), MevError> {

    let poll_interval = std::time::Duration::from_millis(state.config.monitoring.poll_interval_ms * 10);
    let mut pending_tx_count: u64 = 0;
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

/// Trading pair configuration
struct TradingPair {
    name: &'static str,
    uni_pair: Address,
    sushi_pair: Address,
    token0_decimals: u8,
    token1_decimals: u8,
}

/// Run DEX monitor - actively scans for arbitrage opportunities across multiple pairs
async fn run_dex_monitor(state: Arc<AppState>) -> Result<(), MevError> {
    let poll_interval = std::time::Duration::from_millis(state.config.monitoring.poll_interval_ms);

    // Define trading pairs to monitor (Uniswap V2 vs SushiSwap)
    let pairs = vec![
        // High liquidity pairs
        TradingPair {
            name: "WETH/USDC",
            uni_pair: "0xB4e16d0168e52d35CaCD2c6185b44281Ec28C9Dc".parse().unwrap(),
            sushi_pair: "0x397FF1542f962076d0BFE58eA045FfA2d347ACa0".parse().unwrap(),
            token0_decimals: 6,  // USDC
            token1_decimals: 18, // WETH
        },
        TradingPair {
            name: "WETH/USDT",
            uni_pair: "0x0d4a11d5EEaaC28EC3F61d100daF4d40471f1852".parse().unwrap(),
            sushi_pair: "0x06da0fd433C1A5d7a4faa01111c044910A184553".parse().unwrap(),
            token0_decimals: 18, // WETH
            token1_decimals: 6,  // USDT
        },
        TradingPair {
            name: "WETH/DAI",
            uni_pair: "0xA478c2975Ab1Ea89e8196811F51A7B7Ade33eB11".parse().unwrap(),
            sushi_pair: "0xC3D03e4F041Fd4cD388c549Ee2A29a9E5075882f".parse().unwrap(),
            token0_decimals: 18, // DAI
            token1_decimals: 18, // WETH
        },
        // Medium liquidity pairs - more opportunity
        TradingPair {
            name: "WETH/WBTC",
            uni_pair: "0xBb2b8038a1640196FbE3e38816F3e67Cba72D940".parse().unwrap(),
            sushi_pair: "0xCEfF51756c56CeFFCA006cD410B03FFC46dd3a58".parse().unwrap(),
            token0_decimals: 8,  // WBTC
            token1_decimals: 18, // WETH
        },
        TradingPair {
            name: "WETH/LINK",
            uni_pair: "0xa2107FA5B38d9bbd2C461D6EDf11B11A50F6b974".parse().unwrap(),
            sushi_pair: "0xC40D16476380e4037e6b1A2594cAF6a6cc8Da967".parse().unwrap(),
            token0_decimals: 18, // LINK
            token1_decimals: 18, // WETH
        },
        TradingPair {
            name: "WETH/UNI",
            uni_pair: "0xd3d2E2692501A5c9Ca623199D38826e513033a17".parse().unwrap(),
            sushi_pair: "0xDafd66636E2561b0284EDdE37e42d192F2844D40".parse().unwrap(),
            token0_decimals: 18, // UNI
            token1_decimals: 18, // WETH
        },
        // Lower liquidity pairs - higher spreads possible
        TradingPair {
            name: "WETH/AAVE",
            uni_pair: "0xDFC14d2Af169B0D36C4EFF567Ada9b2E0CAE044f".parse().unwrap(),
            sushi_pair: "0xD75EA151a61d06868E31F8988D28DFE5E9df57B4".parse().unwrap(),
            token0_decimals: 18, // AAVE
            token1_decimals: 18, // WETH
        },
        TradingPair {
            name: "WETH/MKR",
            uni_pair: "0xC2aDdA861F89bBB333c90c492cB837741916A225".parse().unwrap(),
            sushi_pair: "0xBa13afEcda9beB75De5c56BbAF696b880a5A50dD".parse().unwrap(),
            token0_decimals: 18, // MKR
            token1_decimals: 18, // WETH
        },
        TradingPair {
            name: "WETH/SNX",
            uni_pair: "0x43AE24960e5534731Fc831386c07755A2dc33D47".parse().unwrap(),
            sushi_pair: "0xA1d7b2d891e3A1f9ef4bBC5be20630C2FEB1c470".parse().unwrap(),
            token0_decimals: 18, // SNX
            token1_decimals: 18, // WETH
        },
        TradingPair {
            name: "WETH/CRV",
            uni_pair: "0x3dA1313aE46132A397D90d95B1424A9A7e3e0fCE".parse().unwrap(),
            sushi_pair: "0x58Dc5a51fE44589BEb22E8CE67720B5BC5378009".parse().unwrap(),
            token0_decimals: 18, // CRV
            token1_decimals: 18, // WETH
        },
        TradingPair {
            name: "WETH/COMP",
            uni_pair: "0xCFfDdeD873554F362Ac02f8Fb1f02E5ada10516f".parse().unwrap(),
            sushi_pair: "0x31503dcb60119A812feE820bb7042752019F2355".parse().unwrap(),
            token0_decimals: 18, // COMP
            token1_decimals: 18, // WETH
        },
        TradingPair {
            name: "WETH/SUSHI",
            uni_pair: "0xCE84867c3c02B05dc570d0135103d3fB9CC19433".parse().unwrap(),
            sushi_pair: "0x795065dCc9f64b5614C407a6EFDC400DA6221FB0".parse().unwrap(),
            token0_decimals: 18, // SUSHI
            token1_decimals: 18, // WETH
        },
    ];

    info!(
        "Monitoring {} trading pairs across Uniswap V2 and SushiSwap",
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
            // Fetch from both DEXes
            let uni_result = fetch_uniswap_v2_reserves(&state.http_provider, pair.uni_pair).await;
            let sushi_result = fetch_uniswap_v2_reserves(&state.http_provider, pair.sushi_pair).await;

            if let (Ok((uni_r0, uni_r1)), Ok((sushi_r0, sushi_r1))) = (uni_result, sushi_result) {
                let uni_price = calculate_price(uni_r0, uni_r1, pair.token0_decimals, pair.token1_decimals);
                let sushi_price = calculate_price(sushi_r0, sushi_r1, pair.token0_decimals, pair.token1_decimals);

                if uni_price > 0.0 && sushi_price > 0.0 {
                    let spread = if uni_price > sushi_price {
                        (uni_price - sushi_price) / sushi_price * 100.0
                    } else {
                        (sushi_price - uni_price) / uni_price * 100.0
                    };

                    // Log spreads above 0.05%
                    if spread > 0.05 {
                        opportunities_found += 1;
                        info!(
                            "[{}] Spread: {:.4}% | Uni: {:.6}, Sushi: {:.6}",
                            pair.name, spread, uni_price, sushi_price
                        );
                    }

                    // Alert on significant opportunities
                    if spread > 0.3 {
                        warn!(
                            "ARBITRAGE OPPORTUNITY: {} - {:.4}% spread (Uni: {:.6}, Sushi: {:.6})",
                            pair.name, spread, uni_price, sushi_price
                        );
                    }

                    if spread > 0.5 {
                        error!(
                            "HIGH SPREAD ALERT: {} - {:.4}% - EXECUTE NOW!",
                            pair.name, spread
                        );
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
    let mut opportunities_found: u64 = 0;

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
