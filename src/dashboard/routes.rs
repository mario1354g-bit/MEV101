//! API route handlers for the MEV dashboard.
//!
//! This module contains all HTTP handlers for the dashboard API,
//! including statistics, opportunity listings, and analysis endpoints.

use axum::{
    Json,
    extract::{Path, Query, State},
    http::StatusCode,
    response::{Html, IntoResponse, Response},
};
use serde::{Deserialize, Serialize};

use super::AppState;

// ============================================================================
// HTML Template Handlers
// ============================================================================

/// Index page template (embedded at compile time).
const INDEX_HTML: &str = include_str!("templates/index.html");

/// Opportunity detail page template (embedded at compile time).
const OPPORTUNITY_HTML: &str = include_str!("templates/opportunity.html");

/// Serve the main dashboard index page.
pub async fn index_handler() -> Html<&'static str> {
    Html(INDEX_HTML)
}

/// Serve the opportunity detail page.
pub async fn opportunity_detail_page_handler(
    Path(id): Path<i64>,
) -> Html<String> {
    // Replace placeholder in template with actual ID
    let html = OPPORTUNITY_HTML.replace("{{OPPORTUNITY_ID}}", &id.to_string());
    Html(html)
}

// ============================================================================
// API Response Types
// ============================================================================

/// Overall statistics response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatsResponse {
    pub total_opportunities: i64,
    pub opportunities_24h: i64,
    pub simulated_count: i64,
    pub executed_count: i64,
    pub competitor_captured_count: i64,
    pub total_estimated_profit_eth: f64,
    pub total_execution_profit_eth: f64,
    pub success_rate: f64,
    pub by_type: Vec<TypeCount>,
    pub avg_profit_per_opportunity_eth: f64,
    pub active_pools: i64,
}

/// Count by opportunity type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TypeCount {
    pub opportunity_type: String,
    pub count: i64,
    pub total_profit_eth: f64,
}

/// Single opportunity response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpportunityResponse {
    pub id: i64,
    pub timestamp: i64,
    pub timestamp_formatted: String,
    pub opportunity_type: String,
    pub token_pairs: Vec<String>,
    pub protocol_venues: Vec<String>,
    pub estimated_gross_profit_eth: f64,
    pub estimated_gas_cost_eth: f64,
    pub estimated_net_profit_eth: f64,
    pub block_number_detected: i64,
    pub block_number_disappeared: Option<i64>,
    pub blocks_persisted: Option<i64>,
    pub captured_by_competitor: bool,
    pub competitor_tx_hash: Option<String>,
    pub simulated: bool,
    pub simulation_profitable: Option<bool>,
    pub executed: bool,
    pub execution_tx_hash: Option<String>,
    pub execution_profit_eth: Option<f64>,
}

/// Opportunity detail response with simulation and execution data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpportunityDetailResponse {
    pub opportunity: OpportunityResponse,
    pub simulation_result: Option<SimulationResult>,
    pub executions: Vec<ExecutionResponse>,
    pub related_opportunities: Vec<OpportunityResponse>,
}

/// Simulation result data.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SimulationResult {
    pub profitable: bool,
    pub expected_profit_eth: f64,
    pub gas_estimate: i64,
    pub revert_reason: Option<String>,
    pub raw_result: Option<serde_json::Value>,
}

/// Execution record response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionResponse {
    pub id: i64,
    pub opportunity_id: i64,
    pub tx_hash: String,
    pub block_number: Option<i64>,
    pub tx_index: Option<i32>,
    pub status: String,
    pub gas_price_gwei: f64,
    pub gas_limit: i64,
    pub gas_used: Option<i64>,
    pub gas_cost_eth: Option<f64>,
    pub gross_profit_eth: Option<f64>,
    pub net_profit_eth: Option<f64>,
    pub slippage_bps: Option<i32>,
    pub error_message: Option<String>,
    pub submitted_at: i64,
    pub submitted_at_formatted: String,
    pub confirmed_at: Option<i64>,
    pub confirmed_at_formatted: Option<String>,
}

/// Query parameters for opportunity listing.
#[derive(Debug, Clone, Deserialize)]
pub struct OpportunityQuery {
    #[serde(default)]
    pub opportunity_type: Option<String>,
    #[serde(default)]
    pub token_pair: Option<String>,
    #[serde(default)]
    pub from_timestamp: Option<i64>,
    #[serde(default)]
    pub to_timestamp: Option<i64>,
    #[serde(default)]
    pub simulated: Option<bool>,
    #[serde(default)]
    pub executed: Option<bool>,
    #[serde(default)]
    pub min_profit_eth: Option<f64>,
    #[serde(default = "default_limit")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
}

fn default_limit() -> i64 {
    50
}

/// Query parameters for execution listing.
#[derive(Debug, Clone, Deserialize)]
pub struct ExecutionQuery {
    #[serde(default)]
    pub opportunity_id: Option<i64>,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub from_timestamp: Option<i64>,
    #[serde(default)]
    pub to_timestamp: Option<i64>,
    #[serde(default = "default_limit")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
}

/// Analysis by opportunity type.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TypeAnalysis {
    pub types: Vec<TypeAnalysisEntry>,
    pub total_count: i64,
    pub total_profit_eth: f64,
}

/// Single type analysis entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TypeAnalysisEntry {
    pub opportunity_type: String,
    pub count: i64,
    pub percentage: f64,
    pub total_profit_eth: f64,
    pub avg_profit_eth: f64,
    pub success_rate: f64,
    pub simulated_count: i64,
    pub executed_count: i64,
}

/// Analysis by token pair.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairAnalysis {
    pub pairs: Vec<PairAnalysisEntry>,
    pub total_count: i64,
}

/// Single pair analysis entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairAnalysisEntry {
    pub token_pair: String,
    pub count: i64,
    pub percentage: f64,
    pub total_profit_eth: f64,
    pub avg_profit_eth: f64,
    pub protocols: Vec<String>,
}

/// Hourly analysis for heatmap.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HourlyAnalysis {
    pub hours: Vec<HourlyEntry>,
    pub peak_hour: i32,
    pub total_count: i64,
}

/// Single hour entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HourlyEntry {
    pub hour: i32,
    pub count: i64,
    pub total_profit_eth: f64,
    pub intensity: f64, // 0.0 to 1.0 for heatmap
}

/// API error response.
#[derive(Debug, Serialize)]
pub struct ApiError {
    pub error: String,
    pub code: u16,
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = StatusCode::from_u16(self.code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        (status, Json(self)).into_response()
    }
}

// ============================================================================
// API Handlers
// ============================================================================

/// Get overall statistics.
pub async fn stats_handler(
    State(state): State<AppState>,
) -> Result<Json<StatsResponse>, ApiError> {
    let now = chrono::Utc::now().timestamp_millis();
    let day_ago = now - 86_400_000; // 24 hours in milliseconds

    // Get total opportunities
    let total: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM opportunities")
        .fetch_one(state.db.pool())
        .await
        .map_err(|e| ApiError {
            error: e.to_string(),
            code: 500,
        })?;

    // Get 24h opportunities
    let count_24h: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM opportunities WHERE timestamp >= ?")
            .bind(day_ago)
            .fetch_one(state.db.pool())
            .await
            .map_err(|e| ApiError {
                error: e.to_string(),
                code: 500,
            })?;

    // Get simulated count
    let simulated: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM opportunities WHERE simulated = 1")
            .fetch_one(state.db.pool())
            .await
            .map_err(|e| ApiError {
                error: e.to_string(),
                code: 500,
            })?;

    // Get executed count
    let executed: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM opportunities WHERE executed = 1")
            .fetch_one(state.db.pool())
            .await
            .map_err(|e| ApiError {
                error: e.to_string(),
                code: 500,
            })?;

    // Get competitor captured count
    let competitor: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM opportunities WHERE captured_by_competitor = 1")
            .fetch_one(state.db.pool())
            .await
            .map_err(|e| ApiError {
                error: e.to_string(),
                code: 500,
            })?;

    // Get total estimated profit
    let estimated_profit: (Option<String>,) = sqlx::query_as(
        "SELECT SUM(CAST(estimated_net_profit_wei AS INTEGER)) FROM opportunities",
    )
    .fetch_one(state.db.pool())
    .await
    .map_err(|e| ApiError {
        error: e.to_string(),
        code: 500,
    })?;

    // Get total execution profit
    let exec_profit: (Option<String>,) = sqlx::query_as(
        "SELECT SUM(CAST(execution_profit_wei AS INTEGER)) FROM opportunities WHERE executed = 1",
    )
    .fetch_one(state.db.pool())
    .await
    .map_err(|e| ApiError {
        error: e.to_string(),
        code: 500,
    })?;

    // Get counts by type
    let type_counts: Vec<(String, i64, Option<String>)> = sqlx::query_as(
        "SELECT opportunity_type, COUNT(*), SUM(CAST(estimated_net_profit_wei AS INTEGER))
         FROM opportunities GROUP BY opportunity_type ORDER BY COUNT(*) DESC",
    )
    .fetch_all(state.db.pool())
    .await
    .map_err(|e| ApiError {
        error: e.to_string(),
        code: 500,
    })?;

    // Get active pools count
    let active_pools: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM pools WHERE is_active = 1")
        .fetch_one(state.db.pool())
        .await
        .unwrap_or((0,));

    let total_estimated_profit_eth = wei_to_eth(
        estimated_profit
            .0
            .as_deref()
            .unwrap_or("0")
            .parse::<i128>()
            .unwrap_or(0),
    );
    let total_execution_profit_eth = wei_to_eth(
        exec_profit
            .0
            .as_deref()
            .unwrap_or("0")
            .parse::<i128>()
            .unwrap_or(0),
    );

    let success_rate = if executed.0 > 0 {
        (executed.0 as f64 / total.0 as f64) * 100.0
    } else {
        0.0
    };

    let avg_profit = if total.0 > 0 {
        total_estimated_profit_eth / total.0 as f64
    } else {
        0.0
    };

    let by_type: Vec<TypeCount> = type_counts
        .into_iter()
        .map(|(t, c, p)| TypeCount {
            opportunity_type: t,
            count: c,
            total_profit_eth: wei_to_eth(
                p.as_deref()
                    .unwrap_or("0")
                    .parse::<i128>()
                    .unwrap_or(0),
            ),
        })
        .collect();

    Ok(Json(StatsResponse {
        total_opportunities: total.0,
        opportunities_24h: count_24h.0,
        simulated_count: simulated.0,
        executed_count: executed.0,
        competitor_captured_count: competitor.0,
        total_estimated_profit_eth,
        total_execution_profit_eth,
        success_rate,
        by_type,
        avg_profit_per_opportunity_eth: avg_profit,
        active_pools: active_pools.0,
    }))
}

/// Get list of opportunities with filtering.
pub async fn opportunities_handler(
    State(state): State<AppState>,
    Query(params): Query<OpportunityQuery>,
) -> Result<Json<Vec<OpportunityResponse>>, ApiError> {
    let mut query = String::from(
        "SELECT id, timestamp, opportunity_type, token_pairs, protocol_venues,
         estimated_gross_profit_wei, estimated_gas_cost_wei, estimated_net_profit_wei,
         block_number_detected, block_number_disappeared, blocks_persisted,
         captured_by_competitor, competitor_tx_hash, simulated, simulation_profitable,
         executed, execution_tx_hash, execution_profit_wei
         FROM opportunities WHERE 1=1",
    );

    let mut bindings: Vec<String> = Vec::new();

    if let Some(ref opp_type) = params.opportunity_type {
        query.push_str(" AND opportunity_type = ?");
        bindings.push(opp_type.clone());
    }

    if let Some(ref pair) = params.token_pair {
        query.push_str(" AND token_pairs LIKE ?");
        bindings.push(format!("%{}%", pair));
    }

    if let Some(from_ts) = params.from_timestamp {
        query.push_str(&format!(" AND timestamp >= {}", from_ts));
    }

    if let Some(to_ts) = params.to_timestamp {
        query.push_str(&format!(" AND timestamp <= {}", to_ts));
    }

    if let Some(sim) = params.simulated {
        query.push_str(&format!(" AND simulated = {}", if sim { 1 } else { 0 }));
    }

    if let Some(exec) = params.executed {
        query.push_str(&format!(" AND executed = {}", if exec { 1 } else { 0 }));
    }

    if let Some(min_profit) = params.min_profit_eth {
        let min_wei = eth_to_wei(min_profit);
        query.push_str(&format!(
            " AND CAST(estimated_net_profit_wei AS INTEGER) >= {}",
            min_wei
        ));
    }

    query.push_str(&format!(
        " ORDER BY timestamp DESC LIMIT {} OFFSET {}",
        params.limit, params.offset
    ));

    // Build dynamic query
    let mut sql_query = sqlx::query_as::<_, OpportunityRow>(&query);
    for binding in &bindings {
        sql_query = sql_query.bind(binding);
    }

    let rows: Vec<OpportunityRow> = sql_query.fetch_all(state.db.pool()).await.map_err(|e| {
        ApiError {
            error: e.to_string(),
            code: 500,
        }
    })?;

    let opportunities: Vec<OpportunityResponse> = rows.into_iter().map(row_to_response).collect();

    Ok(Json(opportunities))
}

/// Get single opportunity detail.
pub async fn opportunity_detail_handler(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<OpportunityDetailResponse>, ApiError> {
    // Get the opportunity
    let row: Option<OpportunityRow> = sqlx::query_as(
        "SELECT id, timestamp, opportunity_type, token_pairs, protocol_venues,
         estimated_gross_profit_wei, estimated_gas_cost_wei, estimated_net_profit_wei,
         block_number_detected, block_number_disappeared, blocks_persisted,
         captured_by_competitor, competitor_tx_hash, simulated, simulation_profitable,
         simulation_result, executed, execution_tx_hash, execution_profit_wei
         FROM opportunities WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(state.db.pool())
    .await
    .map_err(|e| ApiError {
        error: e.to_string(),
        code: 500,
    })?;

    let row = row.ok_or(ApiError {
        error: "Opportunity not found".to_string(),
        code: 404,
    })?;

    let opportunity = row_to_response(row.clone());

    // Parse simulation result if available
    let simulation_result = row.simulation_result.as_ref().and_then(|s| {
        serde_json::from_str::<serde_json::Value>(s).ok().map(|v| {
            SimulationResult {
                profitable: row.simulation_profitable.unwrap_or(false),
                expected_profit_eth: v
                    .get("expected_profit_wei")
                    .and_then(|p| p.as_str())
                    .and_then(|s| s.parse::<i128>().ok())
                    .map(wei_to_eth)
                    .unwrap_or(0.0),
                gas_estimate: v
                    .get("gas_estimate")
                    .and_then(|g| g.as_i64())
                    .unwrap_or(0),
                revert_reason: v
                    .get("revert_reason")
                    .and_then(|r| r.as_str())
                    .map(String::from),
                raw_result: Some(v),
            }
        })
    });

    // Get executions for this opportunity
    let exec_rows: Vec<ExecutionRow> = sqlx::query_as(
        "SELECT id, opportunity_id, tx_hash, block_number, tx_index, status,
         gas_price_wei, gas_limit, gas_used, gas_cost_wei, gross_profit_wei,
         net_profit_wei, slippage_bps, error_message, submitted_at, confirmed_at
         FROM executions WHERE opportunity_id = ? ORDER BY submitted_at DESC",
    )
    .bind(id)
    .fetch_all(state.db.pool())
    .await
    .map_err(|e| ApiError {
        error: e.to_string(),
        code: 500,
    })?;

    let executions: Vec<ExecutionResponse> =
        exec_rows.into_iter().map(exec_row_to_response).collect();

    // Get related opportunities (same type or similar pairs)
    let related_rows: Vec<OpportunityRow> = sqlx::query_as(
        "SELECT id, timestamp, opportunity_type, token_pairs, protocol_venues,
         estimated_gross_profit_wei, estimated_gas_cost_wei, estimated_net_profit_wei,
         block_number_detected, block_number_disappeared, blocks_persisted,
         captured_by_competitor, competitor_tx_hash, simulated, simulation_profitable,
         executed, execution_tx_hash, execution_profit_wei
         FROM opportunities
         WHERE id != ? AND (opportunity_type = ? OR token_pairs = ?)
         ORDER BY timestamp DESC LIMIT 10",
    )
    .bind(id)
    .bind(&opportunity.opportunity_type)
    .bind(&row.token_pairs)
    .fetch_all(state.db.pool())
    .await
    .unwrap_or_default();

    let related_opportunities: Vec<OpportunityResponse> =
        related_rows.into_iter().map(row_to_response).collect();

    Ok(Json(OpportunityDetailResponse {
        opportunity,
        simulation_result,
        executions,
        related_opportunities,
    }))
}

/// Get analysis breakdown by opportunity type.
pub async fn analysis_by_type_handler(
    State(state): State<AppState>,
) -> Result<Json<TypeAnalysis>, ApiError> {
    let rows: Vec<(String, i64, i64, i64, Option<String>)> = sqlx::query_as(
        "SELECT opportunity_type, COUNT(*),
         SUM(CASE WHEN simulated = 1 THEN 1 ELSE 0 END),
         SUM(CASE WHEN executed = 1 THEN 1 ELSE 0 END),
         SUM(CAST(estimated_net_profit_wei AS INTEGER))
         FROM opportunities GROUP BY opportunity_type ORDER BY COUNT(*) DESC",
    )
    .fetch_all(state.db.pool())
    .await
    .map_err(|e| ApiError {
        error: e.to_string(),
        code: 500,
    })?;

    let total_count: i64 = rows.iter().map(|r| r.1).sum();
    let total_profit: i128 = rows
        .iter()
        .map(|r| {
            r.4.as_deref()
                .unwrap_or("0")
                .parse::<i128>()
                .unwrap_or(0)
        })
        .sum();

    let types: Vec<TypeAnalysisEntry> = rows
        .into_iter()
        .map(|(t, count, sim, exec, profit)| {
            let profit_wei = profit
                .as_deref()
                .unwrap_or("0")
                .parse::<i128>()
                .unwrap_or(0);
            let profit_eth = wei_to_eth(profit_wei);
            TypeAnalysisEntry {
                opportunity_type: t,
                count,
                percentage: if total_count > 0 {
                    (count as f64 / total_count as f64) * 100.0
                } else {
                    0.0
                },
                total_profit_eth: profit_eth,
                avg_profit_eth: if count > 0 {
                    profit_eth / count as f64
                } else {
                    0.0
                },
                success_rate: if count > 0 {
                    (exec as f64 / count as f64) * 100.0
                } else {
                    0.0
                },
                simulated_count: sim,
                executed_count: exec,
            }
        })
        .collect();

    Ok(Json(TypeAnalysis {
        types,
        total_count,
        total_profit_eth: wei_to_eth(total_profit),
    }))
}

/// Get analysis breakdown by token pair.
pub async fn analysis_by_pair_handler(
    State(state): State<AppState>,
) -> Result<Json<PairAnalysis>, ApiError> {
    let rows: Vec<(String, String, i64, Option<String>)> = sqlx::query_as(
        "SELECT token_pairs, protocol_venues, COUNT(*),
         SUM(CAST(estimated_net_profit_wei AS INTEGER))
         FROM opportunities GROUP BY token_pairs ORDER BY COUNT(*) DESC LIMIT 50",
    )
    .fetch_all(state.db.pool())
    .await
    .map_err(|e| ApiError {
        error: e.to_string(),
        code: 500,
    })?;

    let total_count: i64 = rows.iter().map(|r| r.2).sum();

    let pairs: Vec<PairAnalysisEntry> = rows
        .into_iter()
        .map(|(pair, venues, count, profit)| {
            let profit_wei = profit
                .as_deref()
                .unwrap_or("0")
                .parse::<i128>()
                .unwrap_or(0);
            let profit_eth = wei_to_eth(profit_wei);
            let protocols: Vec<String> = serde_json::from_str(&venues).unwrap_or_default();
            let pair_display: Vec<String> = serde_json::from_str(&pair).unwrap_or_default();
            PairAnalysisEntry {
                token_pair: pair_display.join(", "),
                count,
                percentage: if total_count > 0 {
                    (count as f64 / total_count as f64) * 100.0
                } else {
                    0.0
                },
                total_profit_eth: profit_eth,
                avg_profit_eth: if count > 0 {
                    profit_eth / count as f64
                } else {
                    0.0
                },
                protocols,
            }
        })
        .collect();

    Ok(Json(PairAnalysis { pairs, total_count }))
}

/// Get hourly analysis for heatmap visualization.
pub async fn hourly_analysis_handler(
    State(state): State<AppState>,
) -> Result<Json<HourlyAnalysis>, ApiError> {
    // Get opportunities grouped by hour (UTC)
    // SQLite: strftime('%H', datetime(timestamp/1000, 'unixepoch'))
    let rows: Vec<(String, i64, Option<String>)> = sqlx::query_as(
        "SELECT strftime('%H', datetime(timestamp/1000, 'unixepoch')) as hour,
         COUNT(*), SUM(CAST(estimated_net_profit_wei AS INTEGER))
         FROM opportunities
         WHERE timestamp >= ?
         GROUP BY hour ORDER BY hour",
    )
    .bind(chrono::Utc::now().timestamp_millis() - 7 * 86_400_000) // Last 7 days
    .fetch_all(state.db.pool())
    .await
    .map_err(|e| ApiError {
        error: e.to_string(),
        code: 500,
    })?;

    let max_count = rows.iter().map(|r| r.1).max().unwrap_or(1);
    let total_count: i64 = rows.iter().map(|r| r.1).sum();

    // Build hours 0-23
    let mut hours_map: std::collections::HashMap<i32, (i64, f64)> = std::collections::HashMap::new();
    for (hour_str, count, profit) in &rows {
        if let Ok(hour) = hour_str.parse::<i32>() {
            let profit_eth = wei_to_eth(
                profit
                    .as_deref()
                    .unwrap_or("0")
                    .parse::<i128>()
                    .unwrap_or(0),
            );
            hours_map.insert(hour, (*count, profit_eth));
        }
    }

    let hours: Vec<HourlyEntry> = (0..24)
        .map(|h| {
            let (count, profit) = hours_map.get(&h).copied().unwrap_or((0, 0.0));
            HourlyEntry {
                hour: h,
                count,
                total_profit_eth: profit,
                intensity: if max_count > 0 {
                    count as f64 / max_count as f64
                } else {
                    0.0
                },
            }
        })
        .collect();

    let peak_hour = hours
        .iter()
        .max_by_key(|h| h.count)
        .map(|h| h.hour)
        .unwrap_or(0);

    Ok(Json(HourlyAnalysis {
        hours,
        peak_hour,
        total_count,
    }))
}

/// Get list of executions.
pub async fn executions_handler(
    State(state): State<AppState>,
    Query(params): Query<ExecutionQuery>,
) -> Result<Json<Vec<ExecutionResponse>>, ApiError> {
    let mut query = String::from(
        "SELECT id, opportunity_id, tx_hash, block_number, tx_index, status,
         gas_price_wei, gas_limit, gas_used, gas_cost_wei, gross_profit_wei,
         net_profit_wei, slippage_bps, error_message, submitted_at, confirmed_at
         FROM executions WHERE 1=1",
    );

    if let Some(opp_id) = params.opportunity_id {
        query.push_str(&format!(" AND opportunity_id = {}", opp_id));
    }

    if let Some(ref status) = params.status {
        query.push_str(&format!(" AND status = '{}'", status));
    }

    if let Some(from_ts) = params.from_timestamp {
        query.push_str(&format!(" AND submitted_at >= {}", from_ts));
    }

    if let Some(to_ts) = params.to_timestamp {
        query.push_str(&format!(" AND submitted_at <= {}", to_ts));
    }

    query.push_str(&format!(
        " ORDER BY submitted_at DESC LIMIT {} OFFSET {}",
        params.limit, params.offset
    ));

    let rows: Vec<ExecutionRow> = sqlx::query_as(&query)
        .fetch_all(state.db.pool())
        .await
        .map_err(|e| ApiError {
            error: e.to_string(),
            code: 500,
        })?;

    let executions: Vec<ExecutionResponse> =
        rows.into_iter().map(exec_row_to_response).collect();

    Ok(Json(executions))
}

/// Get single execution detail.
pub async fn execution_detail_handler(
    State(state): State<AppState>,
    Path(id): Path<i64>,
) -> Result<Json<ExecutionResponse>, ApiError> {
    let row: Option<ExecutionRow> = sqlx::query_as(
        "SELECT id, opportunity_id, tx_hash, block_number, tx_index, status,
         gas_price_wei, gas_limit, gas_used, gas_cost_wei, gross_profit_wei,
         net_profit_wei, slippage_bps, error_message, submitted_at, confirmed_at
         FROM executions WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(state.db.pool())
    .await
    .map_err(|e| ApiError {
        error: e.to_string(),
        code: 500,
    })?;

    let row = row.ok_or(ApiError {
        error: "Execution not found".to_string(),
        code: 404,
    })?;

    Ok(Json(exec_row_to_response(row)))
}

// ============================================================================
// Helper Types and Functions
// ============================================================================

/// Database row for opportunity queries.
#[derive(Debug, Clone, sqlx::FromRow)]
struct OpportunityRow {
    id: i64,
    timestamp: i64,
    opportunity_type: String,
    token_pairs: String,
    protocol_venues: String,
    estimated_gross_profit_wei: String,
    estimated_gas_cost_wei: String,
    estimated_net_profit_wei: String,
    block_number_detected: i64,
    block_number_disappeared: Option<i64>,
    blocks_persisted: Option<i64>,
    captured_by_competitor: bool,
    competitor_tx_hash: Option<String>,
    simulated: bool,
    simulation_profitable: Option<bool>,
    #[sqlx(default)]
    simulation_result: Option<String>,
    executed: bool,
    execution_tx_hash: Option<String>,
    execution_profit_wei: Option<String>,
}

/// Database row for execution queries.
#[derive(Debug, Clone, sqlx::FromRow)]
struct ExecutionRow {
    id: i64,
    opportunity_id: i64,
    tx_hash: String,
    block_number: Option<i64>,
    tx_index: Option<i32>,
    status: String,
    gas_price_wei: String,
    gas_limit: i64,
    gas_used: Option<i64>,
    gas_cost_wei: Option<String>,
    gross_profit_wei: Option<String>,
    net_profit_wei: Option<String>,
    slippage_bps: Option<i32>,
    error_message: Option<String>,
    submitted_at: i64,
    confirmed_at: Option<i64>,
}

/// Convert wei to ETH.
fn wei_to_eth(wei: i128) -> f64 {
    wei as f64 / 1e18
}

/// Convert ETH to wei.
fn eth_to_wei(eth: f64) -> i128 {
    (eth * 1e18) as i128
}

/// Format timestamp as human-readable string.
fn format_timestamp(ts_millis: i64) -> String {
    chrono::DateTime::from_timestamp_millis(ts_millis)
        .map(|dt| dt.format("%Y-%m-%d %H:%M:%S UTC").to_string())
        .unwrap_or_else(|| "Unknown".to_string())
}

/// Convert database row to opportunity response.
fn row_to_response(row: OpportunityRow) -> OpportunityResponse {
    let token_pairs: Vec<String> = serde_json::from_str(&row.token_pairs).unwrap_or_default();
    let protocol_venues: Vec<String> =
        serde_json::from_str(&row.protocol_venues).unwrap_or_default();

    OpportunityResponse {
        id: row.id,
        timestamp: row.timestamp,
        timestamp_formatted: format_timestamp(row.timestamp),
        opportunity_type: row.opportunity_type,
        token_pairs,
        protocol_venues,
        estimated_gross_profit_eth: wei_to_eth(
            row.estimated_gross_profit_wei.parse::<i128>().unwrap_or(0),
        ),
        estimated_gas_cost_eth: wei_to_eth(
            row.estimated_gas_cost_wei.parse::<i128>().unwrap_or(0),
        ),
        estimated_net_profit_eth: wei_to_eth(
            row.estimated_net_profit_wei.parse::<i128>().unwrap_or(0),
        ),
        block_number_detected: row.block_number_detected,
        block_number_disappeared: row.block_number_disappeared,
        blocks_persisted: row.blocks_persisted,
        captured_by_competitor: row.captured_by_competitor,
        competitor_tx_hash: row.competitor_tx_hash,
        simulated: row.simulated,
        simulation_profitable: row.simulation_profitable,
        executed: row.executed,
        execution_tx_hash: row.execution_tx_hash,
        execution_profit_eth: row
            .execution_profit_wei
            .as_ref()
            .and_then(|s| s.parse::<i128>().ok())
            .map(wei_to_eth),
    }
}

/// Convert database row to execution response.
fn exec_row_to_response(row: ExecutionRow) -> ExecutionResponse {
    ExecutionResponse {
        id: row.id,
        opportunity_id: row.opportunity_id,
        tx_hash: row.tx_hash,
        block_number: row.block_number,
        tx_index: row.tx_index,
        status: row.status,
        gas_price_gwei: row.gas_price_wei.parse::<f64>().unwrap_or(0.0) / 1e9,
        gas_limit: row.gas_limit,
        gas_used: row.gas_used,
        gas_cost_eth: row
            .gas_cost_wei
            .as_ref()
            .and_then(|s| s.parse::<i128>().ok())
            .map(wei_to_eth),
        gross_profit_eth: row
            .gross_profit_wei
            .as_ref()
            .and_then(|s| s.parse::<i128>().ok())
            .map(wei_to_eth),
        net_profit_eth: row
            .net_profit_wei
            .as_ref()
            .and_then(|s| s.parse::<i128>().ok())
            .map(wei_to_eth),
        slippage_bps: row.slippage_bps,
        error_message: row.error_message,
        submitted_at: row.submitted_at,
        submitted_at_formatted: format_timestamp(row.submitted_at),
        confirmed_at: row.confirmed_at,
        confirmed_at_formatted: row.confirmed_at.map(format_timestamp),
    }
}
