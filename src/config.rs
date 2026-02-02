use serde::{Deserialize, Serialize};
use std::path::Path;

use crate::error::{ConfigError, ConfigResult};

/// Main configuration struct for the MEV monitor
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub ethereum: EthereumConfig,

    #[serde(default)]
    pub database: DatabaseConfig,

    #[serde(default)]
    pub monitoring: MonitoringConfig,

    #[serde(default)]
    pub simulation: SimulationConfig,

    #[serde(default)]
    pub execution: ExecutionConfig,

    #[serde(default)]
    pub flashbots: FlashbotsConfig,

    #[serde(default)]
    pub dashboard: DashboardConfig,

    #[serde(default)]
    pub logging: LoggingConfig,
}

/// Simulation configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimulationConfig {
    /// Enable transaction simulation
    #[serde(default)]
    pub enabled: bool,

    /// Fork block number (None = latest)
    #[serde(default)]
    pub fork_block: Option<u64>,

    /// Simulation timeout in milliseconds
    #[serde(default = "default_simulation_timeout")]
    pub timeout_ms: u64,

    /// Enable parallel simulation
    #[serde(default = "default_true")]
    pub parallel: bool,

    /// Number of simulation workers
    #[serde(default = "default_simulation_workers")]
    pub workers: usize,
}

/// Flashbots configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FlashbotsConfig {
    /// Flashbots relay URL
    #[serde(default = "default_flashbots_relay")]
    pub relay_url: String,

    /// List of block builders to submit to
    #[serde(default = "default_builders")]
    pub builders: Vec<String>,

    /// Bundle submission timeout in milliseconds
    #[serde(default = "default_bundle_timeout")]
    pub bundle_timeout_ms: u64,

    /// Enable MEV-Share
    #[serde(default)]
    pub mev_share_enabled: bool,
}

/// Ethereum network and provider configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EthereumConfig {
    /// Primary HTTP RPC endpoint URL - local reth node (can be overridden by ETH_RPC_URL env var)
    #[serde(default = "default_http_rpc_url")]
    pub http_rpc_url: String,

    /// Primary WebSocket RPC endpoint URL - local reth node (can be overridden by ETH_WS_URL env var)
    #[serde(default = "default_ws_rpc_url")]
    pub ws_rpc_url: String,

    /// Fallback HTTP RPC endpoint URL - Alchemy (can be overridden by ALCHEMY_HTTP_URL env var)
    #[serde(default)]
    pub fallback_http_url: Option<String>,

    /// Fallback WebSocket RPC endpoint URL - Alchemy (can be overridden by ALCHEMY_WS_URL env var)
    #[serde(default)]
    pub fallback_ws_url: Option<String>,

    /// Chain ID (1 = mainnet, 5 = goerli, 11155111 = sepolia)
    #[serde(default = "default_chain_id")]
    pub chain_id: u64,

    /// Request timeout in seconds
    #[serde(default = "default_request_timeout")]
    pub request_timeout_secs: u64,

    /// Maximum retries for failed requests
    #[serde(default = "default_max_retries")]
    pub max_retries: u32,

    /// Retry delay in milliseconds
    #[serde(default = "default_retry_delay_ms")]
    pub retry_delay_ms: u64,

    /// Number of confirmations to wait for transactions
    #[serde(default = "default_confirmations")]
    pub confirmations: u64,
}

/// Database configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DatabaseConfig {
    /// SQLite database file path
    #[serde(default = "default_db_path")]
    pub path: String,

    /// Maximum number of connections in the pool
    #[serde(default = "default_max_connections")]
    pub max_connections: u32,

    /// Connection timeout in seconds
    #[serde(default = "default_connection_timeout")]
    pub connection_timeout_secs: u64,

    /// Enable WAL mode for better concurrency
    #[serde(default = "default_true")]
    pub enable_wal: bool,

    /// Run migrations on startup
    #[serde(default = "default_true")]
    pub run_migrations: bool,
}

/// Monitoring configuration for different MEV strategies
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonitoringConfig {
    /// Use Artemis architecture (Collector -> Strategy -> Executor pipeline)
    #[serde(default)]
    pub artemis_mode: bool,

    /// Enable mempool monitoring
    #[serde(default = "default_true")]
    pub mempool_enabled: bool,

    /// Enable block monitoring
    #[serde(default = "default_true")]
    pub block_enabled: bool,

    /// Enable DEX monitoring
    #[serde(default = "default_true")]
    pub dex_enabled: bool,

    /// List of DEX router addresses to monitor
    #[serde(default = "default_dex_routers")]
    pub dex_routers: Vec<String>,

    /// Minimum profit threshold in ETH
    #[serde(default = "default_min_profit")]
    pub min_profit_eth: f64,

    /// Token whitelist (empty = all tokens)
    #[serde(default)]
    pub token_whitelist: Vec<String>,

    /// Token blacklist
    #[serde(default)]
    pub token_blacklist: Vec<String>,

    /// Maximum gas price willing to pay (in gwei)
    #[serde(default = "default_max_gas_price")]
    pub max_gas_price_gwei: u64,

    /// Polling interval for HTTP provider in milliseconds
    #[serde(default = "default_poll_interval")]
    pub poll_interval_ms: u64,
}

/// Execution configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionConfig {
    /// Enable actual transaction execution
    #[serde(default)]
    pub enabled: bool,

    /// Dry run mode (simulate but don't execute)
    #[serde(default = "default_true")]
    pub dry_run: bool,

    /// Use Flashbots for bundle submission
    #[serde(default)]
    pub use_flashbots: bool,

    /// Maximum priority fee in gwei
    #[serde(default = "default_max_priority_fee")]
    pub max_priority_fee_gwei: u64,

    /// Gas limit multiplier (1.0 = estimated, 1.2 = 20% buffer)
    #[serde(default = "default_gas_multiplier")]
    pub gas_limit_multiplier: f64,

    /// Slippage tolerance as percentage (0.5 = 0.5%)
    #[serde(default = "default_slippage")]
    pub slippage_tolerance: f64,

    /// Deadline extension in seconds
    #[serde(default = "default_deadline")]
    pub deadline_secs: u64,
}

/// Dashboard and API configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DashboardConfig {
    /// Enable the web dashboard
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Dashboard host address
    #[serde(default = "default_dashboard_host")]
    pub host: String,

    /// Dashboard port
    #[serde(default = "default_dashboard_port")]
    pub port: u16,

    /// Enable CORS
    #[serde(default = "default_true")]
    pub enable_cors: bool,

    /// Allowed origins for CORS (empty = all)
    #[serde(default)]
    pub cors_origins: Vec<String>,

    /// API rate limit (requests per minute)
    #[serde(default = "default_rate_limit")]
    pub rate_limit_per_minute: u32,
}

/// Logging configuration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggingConfig {
    /// Log level (trace, debug, info, warn, error)
    #[serde(default = "default_log_level")]
    pub level: String,

    /// Log format (json, pretty)
    #[serde(default = "default_log_format")]
    pub format: String,

    /// Log to file
    #[serde(default)]
    pub file_enabled: bool,

    /// Log file path
    #[serde(default = "default_log_file")]
    pub file_path: String,

    /// Enable colored output
    #[serde(default = "default_true")]
    pub colored: bool,

    /// Include timestamps
    #[serde(default = "default_true")]
    pub timestamps: bool,

    /// Include target (module path)
    #[serde(default = "default_true")]
    pub include_target: bool,
}

// Default value functions
fn default_http_rpc_url() -> String {
    "http://localhost:8545".to_string()
}

fn default_ws_rpc_url() -> String {
    "ws://localhost:8546".to_string()
}

fn default_chain_id() -> u64 {
    1
}

fn default_request_timeout() -> u64 {
    30
}

fn default_max_retries() -> u32 {
    3
}

fn default_retry_delay_ms() -> u64 {
    1000
}

fn default_confirmations() -> u64 {
    1
}

fn default_db_path() -> String {
    "data/mev_monitor.db".to_string()
}

fn default_max_connections() -> u32 {
    10
}

fn default_connection_timeout() -> u64 {
    30
}

fn default_true() -> bool {
    true
}

fn default_dex_routers() -> Vec<String> {
    vec![
        "0x7a250d5630B4cF539739dF2C5dAcb4c659F2488D".to_string(), // Uniswap V2 Router
        "0xE592427A0AEce92De3Edee1F18E0157C05861564".to_string(), // Uniswap V3 Router
        "0xd9e1cE17f2641f24aE83637ab66a2cca9C378B9F".to_string(), // SushiSwap Router
    ]
}

fn default_min_profit() -> f64 {
    0.01
}

fn default_max_gas_price() -> u64 {
    500
}

fn default_poll_interval() -> u64 {
    100
}

fn default_simulation_timeout() -> u64 {
    500
}

fn default_simulation_workers() -> usize {
    4
}

fn default_flashbots_relay() -> String {
    "https://relay.flashbots.net".to_string()
}

fn default_builders() -> Vec<String> {
    vec![
        "flashbots".to_string(),
        "bloxroute_maxprofit".to_string(),
        "builder0x69".to_string(),
        "rsync".to_string(),
    ]
}

fn default_bundle_timeout() -> u64 {
    5000
}

fn default_max_priority_fee() -> u64 {
    3
}

fn default_gas_multiplier() -> f64 {
    1.2
}

fn default_slippage() -> f64 {
    0.5
}

fn default_deadline() -> u64 {
    300
}

fn default_dashboard_host() -> String {
    "127.0.0.1".to_string()
}

fn default_dashboard_port() -> u16 {
    3000
}

fn default_rate_limit() -> u32 {
    60
}

fn default_log_level() -> String {
    "info".to_string()
}

fn default_log_format() -> String {
    "pretty".to_string()
}

fn default_log_file() -> String {
    "logs/mev_monitor.log".to_string()
}

impl Default for SimulationConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            fork_block: None,
            timeout_ms: default_simulation_timeout(),
            parallel: true,
            workers: default_simulation_workers(),
        }
    }
}

impl Default for FlashbotsConfig {
    fn default() -> Self {
        Self {
            relay_url: default_flashbots_relay(),
            builders: default_builders(),
            bundle_timeout_ms: default_bundle_timeout(),
            mev_share_enabled: false,
        }
    }
}

impl Default for EthereumConfig {
    fn default() -> Self {
        Self {
            http_rpc_url: default_http_rpc_url(),
            ws_rpc_url: default_ws_rpc_url(),
            fallback_http_url: None,
            fallback_ws_url: None,
            chain_id: default_chain_id(),
            request_timeout_secs: default_request_timeout(),
            max_retries: default_max_retries(),
            retry_delay_ms: default_retry_delay_ms(),
            confirmations: default_confirmations(),
        }
    }
}

impl Default for DatabaseConfig {
    fn default() -> Self {
        Self {
            path: default_db_path(),
            max_connections: default_max_connections(),
            connection_timeout_secs: default_connection_timeout(),
            enable_wal: true,
            run_migrations: true,
        }
    }
}

impl Default for MonitoringConfig {
    fn default() -> Self {
        Self {
            artemis_mode: false,
            mempool_enabled: true,
            block_enabled: true,
            dex_enabled: true,
            dex_routers: default_dex_routers(),
            min_profit_eth: default_min_profit(),
            token_whitelist: vec![],
            token_blacklist: vec![],
            max_gas_price_gwei: default_max_gas_price(),
            poll_interval_ms: default_poll_interval(),
        }
    }
}

impl Default for ExecutionConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            dry_run: true,
            use_flashbots: false,
            max_priority_fee_gwei: default_max_priority_fee(),
            gas_limit_multiplier: default_gas_multiplier(),
            slippage_tolerance: default_slippage(),
            deadline_secs: default_deadline(),
        }
    }
}

impl Default for DashboardConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            host: default_dashboard_host(),
            port: default_dashboard_port(),
            enable_cors: true,
            cors_origins: vec![],
            rate_limit_per_minute: default_rate_limit(),
        }
    }
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: default_log_level(),
            format: default_log_format(),
            file_enabled: false,
            file_path: default_log_file(),
            colored: true,
            timestamps: true,
            include_target: true,
        }
    }
}

impl Config {
    /// Load configuration from a TOML file
    pub fn from_file<P: AsRef<Path>>(path: P) -> ConfigResult<Self> {
        let content = std::fs::read_to_string(path)?;
        let config: Config = toml::from_str(&content)?;
        Ok(config)
    }

    /// Load configuration with environment variable overrides
    pub fn load<P: AsRef<Path>>(path: P) -> ConfigResult<Self> {
        // Load .env file if present
        let _ = dotenv::dotenv();

        // Load base config from file
        let mut config = Self::from_file(path)?;

        // Override with environment variables
        config.apply_env_overrides()?;

        // Validate the configuration
        config.validate()?;

        Ok(config)
    }

    /// Apply environment variable overrides
    fn apply_env_overrides(&mut self) -> ConfigResult<()> {
        // Ethereum overrides - primary endpoints (local reth node)
        if let Ok(url) = std::env::var("ETH_RPC_URL") {
            self.ethereum.http_rpc_url = url;
        }
        if let Ok(url) = std::env::var("ETH_WS_URL") {
            self.ethereum.ws_rpc_url = url;
        }

        // Fallback endpoints (Alchemy) - optional
        if let Ok(url) = std::env::var("ALCHEMY_HTTP_URL") {
            self.ethereum.fallback_http_url = Some(url);
        }
        if let Ok(url) = std::env::var("ALCHEMY_WS_URL") {
            self.ethereum.fallback_ws_url = Some(url);
        }

        if let Ok(chain_id) = std::env::var("CHAIN_ID") {
            self.ethereum.chain_id = chain_id.parse().map_err(|_| {
                ConfigError::InvalidValue {
                    field: "CHAIN_ID".to_string(),
                    message: "must be a valid u64".to_string(),
                }
            })?;
        }

        // Database overrides
        if let Ok(path) = std::env::var("DATABASE_PATH") {
            self.database.path = path;
        }

        // Flashbots overrides
        if let Ok(relay) = std::env::var("FLASHBOTS_RELAY_URL") {
            self.flashbots.relay_url = relay;
        }

        // Dashboard overrides
        if let Ok(host) = std::env::var("DASHBOARD_HOST") {
            self.dashboard.host = host;
        }
        if let Ok(port) = std::env::var("DASHBOARD_PORT") {
            self.dashboard.port = port.parse().map_err(|_| {
                ConfigError::InvalidValue {
                    field: "DASHBOARD_PORT".to_string(),
                    message: "must be a valid u16".to_string(),
                }
            })?;
        }

        // Logging overrides
        if let Ok(level) = std::env::var("LOG_LEVEL") {
            self.logging.level = level;
        }
        if let Ok(format) = std::env::var("LOG_FORMAT") {
            self.logging.format = format;
        }

        Ok(())
    }

    /// Get private key from environment variable only (never from config file)
    pub fn get_private_key() -> Option<String> {
        std::env::var("PRIVATE_KEY")
            .ok()
            .or_else(|| std::env::var("MEV_PRIVATE_KEY").ok())
    }

    /// Validate configuration values
    fn validate(&self) -> ConfigResult<()> {
        // Validate chain ID
        if self.ethereum.chain_id == 0 {
            return Err(ConfigError::InvalidValue {
                field: "ethereum.chain_id".to_string(),
                message: "chain ID cannot be 0".to_string(),
            });
        }

        // Validate RPC URLs with proper URL parsing
        if self.ethereum.http_rpc_url.is_empty() {
            return Err(ConfigError::MissingField(
                "ethereum.http_rpc_url".to_string(),
            ));
        }

        url::Url::parse(&self.ethereum.http_rpc_url).map_err(|e| {
            ConfigError::InvalidValue {
                field: "ethereum.http_rpc_url".to_string(),
                message: format!("Invalid URL: {}", e),
            }
        })?;

        if !self.ethereum.ws_rpc_url.is_empty() {
            url::Url::parse(&self.ethereum.ws_rpc_url).map_err(|e| {
                ConfigError::InvalidValue {
                    field: "ethereum.ws_rpc_url".to_string(),
                    message: format!("Invalid URL: {}", e),
                }
            })?;
        }

        // Validate gas settings
        if self.execution.gas_limit_multiplier < 1.0 {
            return Err(ConfigError::InvalidValue {
                field: "execution.gas_limit_multiplier".to_string(),
                message: "must be at least 1.0".to_string(),
            });
        }

        // Validate slippage
        if self.execution.slippage_tolerance < 0.0 || self.execution.slippage_tolerance > 100.0 {
            return Err(ConfigError::InvalidValue {
                field: "execution.slippage_tolerance".to_string(),
                message: "must be between 0 and 100".to_string(),
            });
        }

        // Validate log level
        let valid_levels = ["trace", "debug", "info", "warn", "error"];
        if !valid_levels.contains(&self.logging.level.to_lowercase().as_str()) {
            return Err(ConfigError::InvalidValue {
                field: "logging.level".to_string(),
                message: format!("must be one of: {:?}", valid_levels),
            });
        }

        // Validate log format
        let valid_formats = ["json", "pretty"];
        if !valid_formats.contains(&self.logging.format.to_lowercase().as_str()) {
            return Err(ConfigError::InvalidValue {
                field: "logging.format".to_string(),
                message: format!("must be one of: {:?}", valid_formats),
            });
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert_eq!(config.ethereum.chain_id, 1);
        assert_eq!(config.dashboard.port, 3000);
        assert!(!config.execution.enabled);
    }

    #[test]
    fn test_config_validation() {
        let mut config = Config::default();
        assert!(config.validate().is_ok());

        config.ethereum.chain_id = 0;
        assert!(config.validate().is_err());
    }
}
