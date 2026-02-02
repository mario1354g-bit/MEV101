//! Dashboard module for the MEV monitoring bot.
//!
//! Provides a web-based dashboard for monitoring MEV opportunities,
//! viewing statistics, and tracking executions.

pub mod routes;

use std::net::SocketAddr;
use std::sync::Arc;

use axum::{
    Router,
    routing::get,
};
use tokio::net::TcpListener;
use tower_http::cors::CorsLayer;
use tracing::info;

use crate::config::Config;
use crate::storage::Database;

pub use routes::*;

/// Application state shared across all routes.
#[derive(Clone)]
pub struct AppState {
    pub db: Arc<Database>,
    pub config: Arc<Config>,
}

/// Dashboard server for the MEV monitoring bot.
pub struct Dashboard {
    db: Arc<Database>,
    config: Arc<Config>,
}

impl Dashboard {
    /// Create a new Dashboard instance.
    pub fn new(db: Arc<Database>, config: Arc<Config>) -> Self {
        Self { db, config }
    }

    /// Start the dashboard server.
    pub async fn start(&self) -> Result<(), DashboardError> {
        let state = AppState {
            db: Arc::clone(&self.db),
            config: Arc::clone(&self.config),
        };

        let app = Router::new()
            // HTML routes
            .route("/", get(index_handler))
            .route("/opportunity/{id}", get(opportunity_detail_page_handler))
            // API routes
            .route("/api/stats", get(stats_handler))
            .route("/api/opportunities", get(opportunities_handler))
            .route("/api/opportunities/{id}", get(opportunity_detail_handler))
            .route("/api/analysis/by-type", get(analysis_by_type_handler))
            .route("/api/analysis/by-pair", get(analysis_by_pair_handler))
            .route("/api/analysis/hourly", get(hourly_analysis_handler))
            .route("/api/executions", get(executions_handler))
            .route("/api/executions/{id}", get(execution_detail_handler))
            .layer(CorsLayer::permissive())
            .with_state(state);

        let addr: SocketAddr = format!(
            "{}:{}",
            self.config.dashboard.host, self.config.dashboard.port
        )
        .parse()
        .map_err(|e| DashboardError::InvalidAddress(format!("{}", e)))?;

        info!("Starting dashboard server on http://{}", addr);

        let listener = TcpListener::bind(addr)
            .await
            .map_err(|e| DashboardError::BindError(e.to_string()))?;

        axum::serve(listener, app)
            .await
            .map_err(|e| DashboardError::ServerError(e.to_string()))?;

        Ok(())
    }
}

/// Dashboard-specific errors.
#[derive(Debug, thiserror::Error)]
pub enum DashboardError {
    #[error("Invalid address: {0}")]
    InvalidAddress(String),

    #[error("Failed to bind to address: {0}")]
    BindError(String),

    #[error("Server error: {0}")]
    ServerError(String),

    #[error("Database error: {0}")]
    DatabaseError(String),

    #[error("Template error: {0}")]
    TemplateError(String),
}

impl From<sqlx::Error> for DashboardError {
    fn from(err: sqlx::Error) -> Self {
        DashboardError::DatabaseError(err.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dashboard_creation() {
        // Basic test to ensure Dashboard can be instantiated
        let config = Config::default();
        assert!(config.dashboard.enabled);
    }
}
