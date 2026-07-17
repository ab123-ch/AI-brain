use std::sync::Arc;

use axum::extract::{Path, Query as AxumQuery, Request, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Json;
use axum::Router;
use serde::{Deserialize, Serialize};
use tokio::sync::Mutex;

use crate::orchestrator::Orchestrator;
use crate::web::session_manager::SessionManager;
use crate::web::ws_handler::{ws_upgrade, AppState};

// ─── 内嵌静态文件 ─────────────────────────────────────────────────
static INDEX_HTML: &str = include_str!("web/static/index.html");
static STYLE_CSS: &str = include_str!("web/static/style.css");
static APP_JS: &str = include_str!("web/static/app.js");

// ─── 请求/响应类型 ───────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct QueryRequest {
    query: String,
}

#[derive(Debug, Serialize)]
struct QueryResponse {
    answer: String,
    confidence: f64,
    participating_brains: Vec<String>,
    duration_ms: u64,
}

#[derive(Debug, Serialize)]
struct StatusResponse {
    status: String,
    broadcast_subscribers: usize,
}

#[derive(Debug, Serialize)]
struct BrainInfo {
    name: String,
    weight: f64,
}

#[derive(Debug, Serialize)]
struct WeightsResponse {
    brains: Vec<BrainInfo>,
}

#[derive(Debug, Serialize)]
struct MemoryStatsResponse {
    l0_count: u32,
    l1_count: u32,
    l2_count: u32,
    l3_count: u32,
    total_size_bytes: u64,
}

#[derive(Debug, Serialize)]
struct EvaluateResponse {
    overall_health: f64,
    brain_count: usize,
    slim_instructions: usize,
}

#[derive(Debug, Serialize)]
struct ErrorResponse {
    error: String,
}

// ─── 进化相关请求/响应 ─────────────────────────────────────────────

#[derive(Debug, Serialize)]
struct BrainListResponse {
    active: Vec<BrainEntryResponse>,
    dormant: Vec<BrainEntryResponse>,
}

#[derive(Debug, Serialize)]
struct BrainEntryResponse {
    name: String,
    description: String,
    state: String,
    task_count: u32,
    capabilities: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct CreateBrainRequest {
    template_name: String,
}

#[derive(Debug, Serialize)]
struct CreateBrainResponse {
    brain_id: String,
}

#[derive(Debug, Serialize)]
struct TemplatesResponse {
    templates: Vec<TemplateEntryResponse>,
}

#[derive(Debug, Serialize)]
struct TemplateEntryResponse {
    name: String,
    description: String,
    capabilities: Vec<String>,
}

#[derive(Debug, Serialize)]
struct SuggestResponse {
    suggestions: Vec<SuggestEntryResponse>,
}

#[derive(Debug, Serialize)]
struct SuggestEntryResponse {
    name: String,
    reason: String,
    capabilities: Vec<String>,
    confidence: f64,
}

// ─── 路由 ────────────────────────────────────────────────────────

type SharedOrch = Arc<Mutex<Orchestrator>>;

pub async fn serve(orch: Orchestrator, addr: &str) {
    let shared = Arc::new(Mutex::new(orch));

    let app = Router::new()
        .route("/api/query", post(handle_query))
        .route("/api/status", get(handle_status))
        .route("/api/brains", get(handle_brains))
        .route("/api/brains/list", get(handle_brain_list))
        .route("/api/brains/templates", get(handle_templates))
        .route("/api/brains/create", post(handle_create_brain))
        .route("/api/brains/{id}/dormant", post(handle_dormant_brain))
        .route("/api/brains/{id}/wake", post(handle_wake_brain))
        .route("/api/brains/suggest", get(handle_suggest))
        .route("/api/memory/stats", get(handle_memory_stats))
        .route("/api/evaluate", post(handle_evaluate))
        .with_state(shared);

    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!("绑定 {addr} 失败: {e}");
            return;
        }
    };

    tracing::info!("HTTP API 服务启动于 http://{addr}");
    if let Err(e) = axum::serve(listener, app).await {
        tracing::error!("API 服务错误: {e}");
    }
}

// ─── Handlers ────────────────────────────────────────────────────

async fn handle_query(
    State(orch): State<SharedOrch>,
    Json(req): Json<QueryRequest>,
) -> impl IntoResponse {
    let orch = orch.lock().await;
    match orch.query(&req.query).await {
        Ok(output) => Json(QueryResponse {
            answer: output.answer,
            confidence: output.confidence,
            participating_brains: output
                .participating_brains
                .iter()
                .map(std::string::ToString::to_string)
                .collect(),
            duration_ms: output.usage.duration_ms,
        })
        .into_response(),
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse { error: e }),
        )
            .into_response(),
    }
}

async fn handle_status(State(orch): State<SharedOrch>) -> impl IntoResponse {
    let orch = orch.lock().await;
    Json(StatusResponse {
        status: "running".into(),
        broadcast_subscribers: orch.broadcast_subscribers(),
    })
}

async fn handle_brains(State(orch): State<SharedOrch>) -> impl IntoResponse {
    let orch = orch.lock().await;
    let weights = orch.weights_list().await;
    Json(WeightsResponse {
        brains: weights
            .into_iter()
            .map(|(name, weight)| BrainInfo { name, weight })
            .collect(),
    })
}

async fn handle_memory_stats(State(orch): State<SharedOrch>) -> impl IntoResponse {
    let orch = orch.lock().await;
    match orch.memory_stats_raw().await {
        Ok(stats) => Json(MemoryStatsResponse {
            l0_count: stats.l0_count,
            l1_count: stats.l1_count,
            l2_count: stats.l2_count,
            l3_count: stats.l3_count,
            total_size_bytes: stats.total_size_bytes,
        })
        .into_response(),
        Err(e) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse { error: e }),
        )
            .into_response(),
    }
}

async fn handle_evaluate(State(orch): State<SharedOrch>) -> impl IntoResponse {
    let orch = orch.lock().await;
    let result = orch.evaluate_default_raw();
    Json(EvaluateResponse {
        overall_health: result.overall_health,
        brain_count: result.brain_reports.len(),
        slim_instructions: result.slim_instructions.len(),
    })
}

// ─── 进化相关 Handlers ────────────────────────────────────────────

async fn handle_brain_list(State(orch): State<SharedOrch>) -> impl IntoResponse {
    let orch = orch.lock().await;
    let status = orch.brain_status().await;
    Json(BrainListResponse {
        active: status
            .active
            .into_iter()
            .map(|e| BrainEntryResponse {
                name: e.name,
                description: e.description,
                state: "active".into(),
                task_count: e.task_count,
                capabilities: e.capabilities,
            })
            .collect(),
        dormant: status
            .dormant
            .into_iter()
            .map(|e| BrainEntryResponse {
                name: e.name,
                description: e.description,
                state: "dormant".into(),
                task_count: e.task_count,
                capabilities: e.capabilities,
            })
            .collect(),
    })
}

async fn handle_templates(State(orch): State<SharedOrch>) -> impl IntoResponse {
    let orch = orch.lock().await;
    let templates = orch.list_templates().await;
    Json(TemplatesResponse {
        templates: templates
            .into_iter()
            .map(|t| TemplateEntryResponse {
                name: t.name,
                description: t.description,
                capabilities: t.capabilities,
            })
            .collect(),
    })
}

async fn handle_create_brain(
    State(orch): State<SharedOrch>,
    Json(req): Json<CreateBrainRequest>,
) -> impl IntoResponse {
    let orch = orch.lock().await;
    match orch.create_brain(&req.template_name).await {
        Ok(id) => Json(CreateBrainResponse {
            brain_id: id.to_string(),
        })
        .into_response(),
        Err(e) => (
            axum::http::StatusCode::BAD_REQUEST,
            Json(ErrorResponse { error: e }),
        )
            .into_response(),
    }
}

async fn handle_dormant_brain(
    State(orch): State<SharedOrch>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let orch = orch.lock().await;
    let brain_id = brain_core::types::BrainId(id);
    match orch.dormant_brain(&brain_id).await {
        Ok(()) => Json(serde_json::json!({"status": "dormant"})).into_response(),
        Err(e) => (
            axum::http::StatusCode::BAD_REQUEST,
            Json(ErrorResponse { error: e }),
        )
            .into_response(),
    }
}

async fn handle_wake_brain(
    State(orch): State<SharedOrch>,
    Path(id): Path<String>,
) -> impl IntoResponse {
    let orch = orch.lock().await;
    let brain_id = brain_core::types::BrainId(id);
    match orch.wake_brain(&brain_id).await {
        Ok(weight) => {
            Json(serde_json::json!({"status": "active", "weight": weight})).into_response()
        }
        Err(e) => (
            axum::http::StatusCode::BAD_REQUEST,
            Json(ErrorResponse { error: e }),
        )
            .into_response(),
    }
}

async fn handle_suggest(State(orch): State<SharedOrch>) -> impl IntoResponse {
    let orch = orch.lock().await;
    let suggestions = orch.get_suggestions().await;
    Json(SuggestResponse {
        suggestions: suggestions
            .into_iter()
            .map(|s| SuggestEntryResponse {
                name: s.suggested_name,
                reason: s.reason,
                capabilities: s.suggested_capabilities,
                confidence: s.confidence,
            })
            .collect(),
    })
}

// ─── Web UI 服务 ──────────────────────────────────────────────────

/// 启动 Web UI 服务（静态文件 + WebSocket）
pub async fn serve_web(orch: Orchestrator, addr: &str) {
    serve_web_with_policy(orch, addr, None).await;
}

/// 启动只接受 Tailscale Serve 请求的 Web UI。
pub async fn serve_web_remote(orch: Orchestrator, addr: &str, expected_host: &str) {
    serve_web_with_policy(orch, addr, Some(expected_host.to_string())).await;
}

async fn serve_web_with_policy(orch: Orchestrator, addr: &str, tailscale_host: Option<String>) {
    let base_dir = dirs::data_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("ai-brain");
    let sessions = Arc::new(Mutex::new(SessionManager::new(&base_dir)));
    let state = Arc::new(AppState {
        orch: Arc::new(orch),
        sessions,
        active_query_sessions: Arc::new(Mutex::new(std::collections::HashSet::new())),
        workspace_root: std::env::current_dir()
            .and_then(std::fs::canonicalize)
            .unwrap_or_else(|_| std::path::PathBuf::from(".")),
    });

    let app = Router::new()
        .route("/", get(serve_index))
        .route("/style.css", get(serve_css))
        .route("/app.js", get(serve_js))
        .route(
            "/api/local-file",
            get(serve_local_file).post(save_local_file),
        )
        .route("/ws", get(ws_upgrade))
        .with_state(state);
    let app = if let Some(expected_host) = tailscale_host {
        app.layer(middleware::from_fn_with_state(
            TailscaleAccessPolicy { expected_host },
            enforce_tailscale_access,
        ))
    } else {
        app
    };

    let listener = match tokio::net::TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            tracing::error!("绑定 {addr} 失败: {e}");
            return;
        }
    };
    tracing::info!("Web UI 启动于 http://{addr}");
    if let Err(e) = axum::serve(listener, app).await {
        tracing::error!("Web 服务错误: {e}");
    }
}

#[derive(Clone)]
struct TailscaleAccessPolicy {
    expected_host: String,
}

async fn enforce_tailscale_access(
    State(policy): State<TailscaleAccessPolicy>,
    request: Request,
    next: Next,
) -> Response {
    if !has_tailscale_identity(request.headers()) {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": "仅允许通过 Tailscale Serve 访问"})),
        )
            .into_response();
    }
    if !origin_matches_tailscale_host(request.headers(), &policy.expected_host) {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"error": "请求来源与智脑远程地址不匹配"})),
        )
            .into_response();
    }

    next.run(request).await
}

fn has_tailscale_identity(headers: &HeaderMap) -> bool {
    headers
        .get("tailscale-user-login")
        .and_then(|value| value.to_str().ok())
        .is_some_and(|login| !login.trim().is_empty())
}

fn origin_matches_tailscale_host(headers: &HeaderMap, expected_host: &str) -> bool {
    let Some(origin) = headers.get(axum::http::header::ORIGIN) else {
        return true;
    };
    let Ok(origin) = origin.to_str() else {
        return false;
    };
    let Ok(uri) = origin.parse::<Uri>() else {
        return false;
    };

    uri.scheme_str() == Some("https")
        && uri.host().is_some_and(|host| {
            host.trim_end_matches('.')
                .eq_ignore_ascii_case(expected_host.trim_end_matches('.'))
        })
}

async fn serve_index() -> impl IntoResponse {
    (
        [(axum::http::header::CONTENT_TYPE, "text/html; charset=utf-8")],
        INDEX_HTML,
    )
}

async fn serve_css() -> impl IntoResponse {
    (
        [(axum::http::header::CONTENT_TYPE, "text/css; charset=utf-8")],
        STYLE_CSS,
    )
}

async fn serve_js() -> impl IntoResponse {
    (
        [(
            axum::http::header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        )],
        APP_JS,
    )
}

#[derive(Deserialize)]
struct LocalFileQuery {
    path: String,
}

#[derive(Deserialize)]
struct SaveLocalFileRequest {
    path: String,
    content: String,
}

async fn serve_local_file(
    State(state): State<Arc<AppState>>,
    AxumQuery(query): AxumQuery<LocalFileQuery>,
) -> axum::response::Response {
    let requested = std::path::PathBuf::from(query.path.trim_start_matches(r"\\?\"));
    let requested = if requested.is_absolute() {
        requested
    } else {
        state.workspace_root.join(requested)
    };
    let Ok(canonical) = std::fs::canonicalize(&requested) else {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "文件不存在"})),
        )
            .into_response();
    };
    if !canonical.starts_with(&state.workspace_root) || !canonical.is_file() {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"error": "只允许预览当前工作区文件"})),
        )
            .into_response();
    }
    let Ok(metadata) = std::fs::metadata(&canonical) else {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "无法读取文件信息"})),
        )
            .into_response();
    };
    if metadata.len() > 5 * 1024 * 1024 {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(serde_json::json!({"error": "文件超过 5MB，无法在线预览"})),
        )
            .into_response();
    }
    let Ok(content) = std::fs::read_to_string(&canonical) else {
        return (
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            Json(serde_json::json!({"error": "当前仅支持文本文件预览"})),
        )
            .into_response();
    };
    let name = canonical
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("文件");
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "name": name,
            "path": canonical.to_string_lossy(),
            "content": content,
        })),
    )
        .into_response()
}

async fn save_local_file(
    State(state): State<Arc<AppState>>,
    Json(request): Json<SaveLocalFileRequest>,
) -> axum::response::Response {
    if request.content.len() > 5 * 1024 * 1024 {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(serde_json::json!({"error": "文件超过 5MB"})),
        )
            .into_response();
    }
    let requested = std::path::PathBuf::from(request.path.trim_start_matches(r"\\?\"));
    let requested = if requested.is_absolute() {
        requested
    } else {
        state.workspace_root.join(requested)
    };
    let Ok(canonical) = std::fs::canonicalize(requested) else {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({"error": "文件不存在"})),
        )
            .into_response();
    };
    if !canonical.starts_with(&state.workspace_root) || !canonical.is_file() {
        return (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"error": "只允许修改当前工作区文件"})),
        )
            .into_response();
    }
    match std::fs::write(canonical, request.content) {
        Ok(()) => (StatusCode::OK, Json(serde_json::json!({"saved": true}))).into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("保存失败: {error}")})),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod remote_policy_tests {
    use axum::http::{HeaderMap, HeaderValue};

    use super::{has_tailscale_identity, origin_matches_tailscale_host};

    #[test]
    fn requires_non_empty_tailscale_login_header() {
        let mut headers = HeaderMap::new();
        assert!(!has_tailscale_identity(&headers));

        headers.insert("tailscale-user-login", HeaderValue::from_static("   "));
        assert!(!has_tailscale_identity(&headers));

        headers.insert(
            "tailscale-user-login",
            HeaderValue::from_static("owner@example.com"),
        );
        assert!(has_tailscale_identity(&headers));
    }

    #[test]
    fn accepts_same_origin_and_headerless_non_browser_requests() {
        let mut headers = HeaderMap::new();
        assert!(origin_matches_tailscale_host(
            &headers,
            "brain-mac.example.ts.net"
        ));

        headers.insert(
            axum::http::header::ORIGIN,
            HeaderValue::from_static("https://brain-mac.example.ts.net"),
        );
        assert!(origin_matches_tailscale_host(
            &headers,
            "BRAIN-MAC.EXAMPLE.TS.NET."
        ));
    }

    #[test]
    fn rejects_cross_origin_and_insecure_browser_requests() {
        let mut headers = HeaderMap::new();
        headers.insert(
            axum::http::header::ORIGIN,
            HeaderValue::from_static("https://attacker.example"),
        );
        assert!(!origin_matches_tailscale_host(
            &headers,
            "brain-mac.example.ts.net"
        ));

        headers.insert(
            axum::http::header::ORIGIN,
            HeaderValue::from_static("http://brain-mac.example.ts.net"),
        );
        assert!(!origin_matches_tailscale_host(
            &headers,
            "brain-mac.example.ts.net"
        ));
    }
}
