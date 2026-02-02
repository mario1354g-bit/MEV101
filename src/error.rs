use thiserror::Error;

/// Main error type for the MEV monitor application
#[derive(Error, Debug)]
pub enum MevError {
    #[error("Configuration error: {0}")]
    Config(#[from] ConfigError),

    #[error("Database error: {0}")]
    Database(#[from] DatabaseError),

    #[error("Provider error: {0}")]
    Provider(#[from] ProviderError),

    #[error("Simulation error: {0}")]
    Simulation(#[from] SimulationError),

    #[error("Execution error: {0}")]
    Execution(#[from] ExecutionError),

    #[error("Decoding error: {0}")]
    Decoding(#[from] DecodingError),
}

/// Configuration-related errors
#[derive(Error, Debug)]
pub enum ConfigError {
    #[error("Failed to read config file: {0}")]
    FileRead(#[from] std::io::Error),

    #[error("Failed to parse config file: {0}")]
    Parse(#[from] toml::de::Error),

    #[error("Missing required configuration: {0}")]
    MissingField(String),

    #[error("Invalid configuration value for {field}: {message}")]
    InvalidValue { field: String, message: String },

    #[error("Environment variable error: {0}")]
    EnvVar(#[from] std::env::VarError),
}

/// Database-related errors
#[derive(Error, Debug)]
pub enum DatabaseError {
    #[error("SQLx error: {0}")]
    Sqlx(#[from] sqlx::Error),

    #[error("Migration error: {0}")]
    Migration(#[from] sqlx::migrate::MigrateError),

    #[error("Connection pool exhausted")]
    PoolExhausted,

    #[error("Record not found: {0}")]
    NotFound(String),

    #[error("Duplicate entry: {0}")]
    DuplicateEntry(String),

    #[error("Transaction failed: {0}")]
    TransactionFailed(String),
}

/// Ethereum provider-related errors
#[derive(Error, Debug)]
pub enum ProviderError {
    #[error("Failed to connect to RPC endpoint: {0}")]
    ConnectionFailed(String),

    #[error("WebSocket connection error: {0}")]
    WebSocketError(String),

    #[error("RPC request failed: {0}")]
    RpcError(String),

    #[error("Block not found: {0}")]
    BlockNotFound(String),

    #[error("Transaction not found: {0}")]
    TransactionNotFound(String),

    #[error("Subscription error: {0}")]
    SubscriptionError(String),

    #[error("Provider timeout after {0} seconds")]
    Timeout(u64),

    #[error("Invalid chain ID: expected {expected}, got {actual}")]
    ChainIdMismatch { expected: u64, actual: u64 },
}

/// Simulation-related errors
#[derive(Error, Debug)]
pub enum SimulationError {
    #[error("Simulation reverted: {0}")]
    Reverted(String),

    #[error("Gas estimation failed: {0}")]
    GasEstimationFailed(String),

    #[error("State override error: {0}")]
    StateOverrideError(String),

    #[error("Trace execution failed: {0}")]
    TraceFailed(String),

    #[error("Insufficient balance for simulation")]
    InsufficientBalance,

    #[error("Contract call failed: {0}")]
    ContractCallFailed(String),

    #[error("Invalid simulation parameters: {0}")]
    InvalidParameters(String),
}

/// Execution-related errors
#[derive(Error, Debug)]
pub enum ExecutionError {
    #[error("Transaction submission failed: {0}")]
    SubmissionFailed(String),

    #[error("Transaction reverted: {0}")]
    TransactionReverted(String),

    #[error("Nonce too low")]
    NonceTooLow,

    #[error("Nonce too high")]
    NonceTooHigh,

    #[error("Gas price too low")]
    GasPriceTooLow,

    #[error("Insufficient funds for gas")]
    InsufficientFunds,

    #[error("Transaction underpriced")]
    Underpriced,

    #[error("Bundle submission failed: {0}")]
    BundleFailed(String),

    #[error("Flashbots relay error: {0}")]
    FlashbotsError(String),

    #[error("Signer error: {0}")]
    SignerError(String),

    #[error("Transaction timeout: {0}")]
    Timeout(String),

    #[error("MEV opportunity no longer profitable")]
    NotProfitable,
}

/// Decoding-related errors
#[derive(Error, Debug)]
pub enum DecodingError {
    #[error("Failed to decode ABI: {0}")]
    AbiDecode(String),

    #[error("Failed to decode calldata: {0}")]
    CalldataDecode(String),

    #[error("Failed to decode log: {0}")]
    LogDecode(String),

    #[error("Unknown function selector: {0}")]
    UnknownSelector(String),

    #[error("Invalid address format: {0}")]
    InvalidAddress(String),

    #[error("Invalid hex string: {0}")]
    InvalidHex(String),

    #[error("Failed to parse transaction: {0}")]
    TransactionParse(String),

    #[error("Unsupported token standard: {0}")]
    UnsupportedTokenStandard(String),
}

/// Result type alias for MevError
pub type Result<T> = std::result::Result<T, MevError>;

/// Result type alias for ConfigError
pub type ConfigResult<T> = std::result::Result<T, ConfigError>;

/// Result type alias for DatabaseError
pub type DatabaseResult<T> = std::result::Result<T, DatabaseError>;

/// Result type alias for ProviderError
pub type ProviderResult<T> = std::result::Result<T, ProviderError>;

/// Result type alias for SimulationError
pub type SimulationResult<T> = std::result::Result<T, SimulationError>;

/// Result type alias for ExecutionError
pub type ExecutionResult<T> = std::result::Result<T, ExecutionError>;

/// Result type alias for DecodingError
pub type DecodingResult<T> = std::result::Result<T, DecodingError>;
