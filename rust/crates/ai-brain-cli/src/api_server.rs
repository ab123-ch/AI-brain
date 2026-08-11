use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Path, Query as AxumQuery, Request, State};
use axum::http::{HeaderMap, StatusCode, Uri};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Json;
use axum::Router;
use brain_llm::config::LlmConfig;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use crate::orchestrator::Orchestrator;
use crate::web::collaboration::{
    default_runtime_dir, CollaborationConfig, CollaborationRepository,
};
use crate::web::collaboration_runtime::CollaborationRuntime;
use crate::web::session_manager::SessionManager;
use crate::web::ws_handler::{ws_upgrade, AppState};

// ─── 内嵌静态文件 ─────────────────────────────────────────────────
static INDEX_HTML: &str = include_str!("web/static/index.html");
static STYLE_CSS: &str = include_str!("web/static/style.css");
static APP_JS: &str = include_str!("web/static/app.js");
static MENTIONS_JS: &str = include_str!("web/static/mentions.js");
static MODEL_CATALOG_JS: &str = include_str!("web/static/model_catalog.js");
static ROOM_REPLY_JS: &str = include_str!("web/static/room_reply.js");

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
pub async fn serve_web(orch: Orchestrator, addr: &str) -> Result<(), String> {
    serve_web_with_policy(orch, addr, None).await
}

/// 启动只接受 Tailscale Serve 请求的 Web UI。
pub async fn serve_web_remote(
    orch: Orchestrator,
    addr: &str,
    expected_host: &str,
) -> Result<(), String> {
    serve_web_with_policy(orch, addr, Some(expected_host.to_string())).await
}

fn capture_workspace_root<C, K>(
    current_dir: C,
    canonicalize: K,
) -> Result<std::path::PathBuf, String>
where
    C: FnOnce() -> std::io::Result<std::path::PathBuf>,
    K: FnOnce(&std::path::Path) -> std::io::Result<std::path::PathBuf>,
{
    current_dir()
        .and_then(|path| canonicalize(&path))
        .map_err(|error| format!("读取服务启动工作目录失败: {error}"))
}

async fn serve_web_with_policy(
    orch: Orchestrator,
    addr: &str,
    tailscale_host: Option<String>,
) -> Result<(), String> {
    let workspace_root =
        capture_workspace_root(std::env::current_dir, |path| std::fs::canonicalize(path))?;
    let base_dir = dirs::data_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("ai-brain");
    let sessions = Arc::new(Mutex::new(SessionManager::new(&base_dir)));
    let runtime_dir = default_runtime_dir();
    let llm_config = Arc::new(match LlmConfig::load_default() {
        Ok(config) => config,
        Err(error) => {
            tracing::warn!("加载实例模型目录失败，仅保留 main 策略: {error}");
            LlmConfig::default_config()
        }
    });
    let model_policy_details = llm_config.available_instance_model_policies();
    let collaboration_config = CollaborationConfig::load(&runtime_dir.join("config.toml"))
        .map_err(|error| format!("加载协作配置失败: {error}"))?
        .with_available_model_policies(
            model_policy_details
                .iter()
                .map(|policy| policy.policy_id.clone()),
        );
    let collaboration_repository = Arc::new(
        CollaborationRepository::new_with_startup_working_directory(
            &runtime_dir,
            collaboration_config,
            &workspace_root,
        )
        .map_err(|error| format!("初始化协作存储失败: {error}"))?,
    );
    let orch = Arc::new(orch);
    let collaboration = CollaborationRuntime::start(
        Arc::clone(&collaboration_repository),
        Arc::clone(&orch),
        Arc::clone(&llm_config),
        model_policy_details,
    )
    .await
    .map_err(|error| format!("启动协作运行时失败: {error}"))?;
    let state = Arc::new(AppState {
        orch,
        sessions,
        collaboration,
        collaboration_repository,
        workspace_root: workspace_root.clone(),
        local_file_save_lock: Mutex::new(()),
    });

    let app = Router::new()
        .route("/", get(serve_index))
        .route("/style.css", get(serve_css))
        .route("/app.js", get(serve_js))
        .route("/mentions.js", get(serve_mentions_js))
        .route("/model_catalog.js", get(serve_model_catalog_js))
        .route("/room_reply.js", get(serve_room_reply_js))
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

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .map_err(|error| format!("绑定 {addr} 失败: {error}"))?;
    tracing::info!("Web UI 启动于 http://{addr}");
    axum::serve(listener, app)
        .await
        .map_err(|error| format!("Web 服务错误: {error}"))
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

async fn serve_mentions_js() -> impl IntoResponse {
    (
        [(
            axum::http::header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        )],
        MENTIONS_JS,
    )
}

async fn serve_model_catalog_js() -> impl IntoResponse {
    (
        [(
            axum::http::header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        )],
        MODEL_CATALOG_JS,
    )
}

async fn serve_room_reply_js() -> impl IntoResponse {
    (
        [(
            axum::http::header::CONTENT_TYPE,
            "application/javascript; charset=utf-8",
        )],
        ROOM_REPLY_JS,
    )
}

#[derive(Deserialize)]
struct LocalFileQuery {
    path: String,
    #[serde(default)]
    run_id: Option<String>,
}

#[derive(Deserialize)]
struct SaveLocalFileRequest {
    path: String,
    content: String,
    expected_revision: String,
    #[serde(default)]
    run_id: Option<String>,
}

const MAX_LOCAL_FILE_BYTES: usize = 5 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
struct LocalFileSnapshot {
    name: String,
    path: String,
    content: String,
    revision: String,
}

#[derive(Debug)]
enum LocalFileFailure {
    Status(StatusCode, String),
    Conflict(LocalFileSnapshot),
}

impl LocalFileFailure {
    fn into_response(self) -> Response {
        match self {
            Self::Status(status, error) => {
                (status, Json(serde_json::json!({"error": error}))).into_response()
            }
            Self::Conflict(snapshot) => (
                StatusCode::CONFLICT,
                Json(serde_json::json!({
                    "error": "文件已在磁盘上更新，请先处理版本冲突",
                    "name": snapshot.name,
                    "path": snapshot.path,
                    "content": snapshot.content,
                    "revision": snapshot.revision,
                })),
            )
                .into_response(),
        }
    }
}

async fn serve_local_file(
    State(state): State<Arc<AppState>>,
    AxumQuery(query): AxumQuery<LocalFileQuery>,
) -> axum::response::Response {
    let result = authorize_local_file(&state, &query.path, query.run_id.as_deref())
        .and_then(|canonical| read_local_file_snapshot_at(&canonical));
    match result {
        Ok(snapshot) => (StatusCode::OK, Json(snapshot)).into_response(),
        Err(error) => error.into_response(),
    }
}

fn canonicalize_local_file(
    workspace_root: &std::path::Path,
    requested_path: &str,
) -> Result<PathBuf, LocalFileFailure> {
    let requested = PathBuf::from(requested_path.trim_start_matches(r"\\?\"));
    let requested = if requested.is_absolute() {
        requested
    } else {
        workspace_root.join(requested)
    };
    let canonical = fs::canonicalize(&requested)
        .map_err(|_| LocalFileFailure::Status(StatusCode::NOT_FOUND, "文件不存在".into()))?;
    if !canonical.is_file() {
        return Err(LocalFileFailure::Status(
            StatusCode::FORBIDDEN,
            "只允许访问文件".into(),
        ));
    }
    Ok(canonical)
}

fn authorize_local_file(
    state: &AppState,
    requested_path: &str,
    run_id: Option<&str>,
) -> Result<PathBuf, LocalFileFailure> {
    let canonical = canonicalize_local_file(&state.workspace_root, requested_path)?;
    let canonical_workspace = fs::canonicalize(&state.workspace_root).map_err(|error| {
        LocalFileFailure::Status(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("无法读取工作区: {error}"),
        )
    })?;
    if canonical.starts_with(canonical_workspace) {
        return Ok(canonical);
    }
    let authorized = match run_id {
        Some(run_id) => state
            .collaboration_repository
            .run_changed_file_exists(run_id, &canonical)
            .map_err(|error| {
                LocalFileFailure::Status(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("校验运行文件权限失败: {error}"),
                )
            })?,
        None => false,
    };
    if authorized {
        return Ok(canonical);
    }
    Err(LocalFileFailure::Status(
        StatusCode::FORBIDDEN,
        "只允许访问当前工作区或本轮变更文件".into(),
    ))
}

#[cfg(test)]
fn resolve_workspace_local_file(
    workspace_root: &std::path::Path,
    requested_path: &str,
) -> Result<PathBuf, LocalFileFailure> {
    let canonical = canonicalize_local_file(workspace_root, requested_path)?;
    let canonical_workspace = fs::canonicalize(workspace_root).map_err(|error| {
        LocalFileFailure::Status(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("无法读取工作区: {error}"),
        )
    })?;
    if !canonical.starts_with(canonical_workspace) {
        return Err(LocalFileFailure::Status(
            StatusCode::FORBIDDEN,
            "只允许访问当前工作区文件".into(),
        ));
    }
    Ok(canonical)
}

#[cfg(test)]
fn read_local_file_snapshot(
    workspace_root: &std::path::Path,
    requested_path: &str,
) -> Result<LocalFileSnapshot, LocalFileFailure> {
    let canonical = resolve_workspace_local_file(workspace_root, requested_path)?;
    read_local_file_snapshot_at(&canonical)
}

fn read_local_file_snapshot_at(
    canonical: &std::path::Path,
) -> Result<LocalFileSnapshot, LocalFileFailure> {
    let metadata = fs::metadata(&canonical)
        .map_err(|_| LocalFileFailure::Status(StatusCode::NOT_FOUND, "无法读取文件信息".into()))?;
    if metadata.len() > MAX_LOCAL_FILE_BYTES as u64 {
        return Err(LocalFileFailure::Status(
            StatusCode::PAYLOAD_TOO_LARGE,
            "文件超过 5MB，无法在线预览".into(),
        ));
    }
    let bytes = fs::read(&canonical).map_err(|error| {
        LocalFileFailure::Status(StatusCode::NOT_FOUND, format!("无法读取文件: {error}"))
    })?;
    if bytes.len() > MAX_LOCAL_FILE_BYTES {
        return Err(LocalFileFailure::Status(
            StatusCode::PAYLOAD_TOO_LARGE,
            "文件超过 5MB，无法在线预览".into(),
        ));
    }
    let content = String::from_utf8(bytes).map_err(|_| {
        LocalFileFailure::Status(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "当前仅支持文本文件预览".into(),
        )
    })?;
    let name = canonical
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("文件")
        .to_owned();
    let revision = local_file_revision(content.as_bytes());
    Ok(LocalFileSnapshot {
        name,
        path: canonical.to_string_lossy().into_owned(),
        content,
        revision,
    })
}

async fn save_local_file(
    State(state): State<Arc<AppState>>,
    Json(request): Json<SaveLocalFileRequest>,
) -> axum::response::Response {
    let canonical = match authorize_local_file(&state, &request.path, request.run_id.as_deref()) {
        Ok(canonical) => canonical,
        Err(error) => return error.into_response(),
    };
    let _save_guard = state.local_file_save_lock.lock().await;
    match save_local_file_snapshot_at(&canonical, request) {
        Ok(snapshot) => (
            StatusCode::OK,
            Json(serde_json::json!({
                "saved": true,
                "name": snapshot.name,
                "path": snapshot.path,
                "revision": snapshot.revision,
            })),
        )
            .into_response(),
        Err(error) => error.into_response(),
    }
}

#[cfg(test)]
fn save_local_file_snapshot(
    workspace_root: &std::path::Path,
    request: SaveLocalFileRequest,
) -> Result<LocalFileSnapshot, LocalFileFailure> {
    let canonical = resolve_workspace_local_file(workspace_root, &request.path)?;
    save_local_file_snapshot_at(&canonical, request)
}

fn save_local_file_snapshot_at(
    canonical: &std::path::Path,
    request: SaveLocalFileRequest,
) -> Result<LocalFileSnapshot, LocalFileFailure> {
    if request.content.len() > MAX_LOCAL_FILE_BYTES {
        return Err(LocalFileFailure::Status(
            StatusCode::PAYLOAD_TOO_LARGE,
            "文件超过 5MB".into(),
        ));
    }
    if request.expected_revision.trim().is_empty() {
        return Err(LocalFileFailure::Status(
            StatusCode::BAD_REQUEST,
            "保存请求缺少文件版本".into(),
        ));
    }
    let current = read_local_file_snapshot_at(canonical)?;
    if current.revision != request.expected_revision {
        return Err(LocalFileFailure::Conflict(current));
    }
    let parent = canonical.parent().ok_or_else(|| {
        LocalFileFailure::Status(
            StatusCode::INTERNAL_SERVER_ERROR,
            "无法确定文件所在目录".into(),
        )
    })?;
    let permissions = fs::metadata(&canonical)
        .map_err(|error| {
            LocalFileFailure::Status(StatusCode::NOT_FOUND, format!("无法读取文件信息: {error}"))
        })?
        .permissions();
    let mut temporary = tempfile::NamedTempFile::new_in(parent).map_err(|error| {
        LocalFileFailure::Status(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("创建临时文件失败: {error}"),
        )
    })?;
    temporary
        .write_all(request.content.as_bytes())
        .and_then(|()| temporary.flush())
        .map_err(|error| {
            LocalFileFailure::Status(
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("写入临时文件失败: {error}"),
            )
        })?;
    fs::set_permissions(temporary.path(), permissions).map_err(|error| {
        LocalFileFailure::Status(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("保留文件权限失败: {error}"),
        )
    })?;
    temporary.as_file().sync_all().map_err(|error| {
        LocalFileFailure::Status(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("同步临时文件失败: {error}"),
        )
    })?;

    let latest = read_local_file_snapshot_at(canonical)?;
    if latest.revision != request.expected_revision {
        return Err(LocalFileFailure::Conflict(latest));
    }
    temporary.persist(canonical).map_err(|error| {
        LocalFileFailure::Status(
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("原子替换文件失败: {}", error.error),
        )
    })?;
    let revision = local_file_revision(request.content.as_bytes());
    Ok(LocalFileSnapshot {
        name: current.name,
        path: current.path,
        content: request.content,
        revision,
    })
}

fn local_file_revision(content: &[u8]) -> String {
    format!("{:x}", Sha256::digest(content))
}

#[cfg(test)]
mod static_asset_tests {
    use std::fs;

    use axum::body::to_bytes;
    use axum::http::{header::CONTENT_TYPE, StatusCode};
    use axum::response::IntoResponse;

    use super::{
        local_file_revision, read_local_file_snapshot, save_local_file_snapshot,
        serve_model_catalog_js, serve_room_reply_js, LocalFileFailure, SaveLocalFileRequest,
    };

    #[tokio::test]
    async fn collaboration_web_assets_serves_room_reply_script() {
        let response = serve_room_reply_js().await.into_response();

        assert_eq!(
            response
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("application/javascript; charset=utf-8")
        );
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = std::str::from_utf8(&body).unwrap();
        assert!(body.contains("RoomReply"));
    }

    #[test]
    fn local_file_read_returns_content_revision_and_rejects_outside_workspace() {
        let workspace = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let file = workspace.path().join("sample.rs");
        let outside_file = outside.path().join("outside.rs");
        fs::write(&file, "fn sample() {}\n").unwrap();
        fs::write(&outside_file, "fn outside() {}\n").unwrap();

        let snapshot = read_local_file_snapshot(workspace.path(), file.to_str().unwrap()).unwrap();
        assert_eq!(snapshot.name, "sample.rs");
        assert_eq!(snapshot.content, "fn sample() {}\n");
        assert_eq!(
            snapshot.revision,
            local_file_revision(snapshot.content.as_bytes())
        );

        let outside_error =
            read_local_file_snapshot(workspace.path(), outside_file.to_str().unwrap()).unwrap_err();
        assert!(matches!(
            outside_error,
            LocalFileFailure::Status(StatusCode::FORBIDDEN, _)
        ));
    }

    #[test]
    fn local_file_save_is_revision_guarded_and_preserves_permissions() {
        let workspace = tempfile::tempdir().unwrap();
        let file = workspace.path().join("editable.sh");
        fs::write(&file, "before\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            fs::set_permissions(&file, fs::Permissions::from_mode(0o744)).unwrap();
        }
        let initial = read_local_file_snapshot(workspace.path(), file.to_str().unwrap()).unwrap();
        let saved = save_local_file_snapshot(
            workspace.path(),
            SaveLocalFileRequest {
                path: initial.path.clone(),
                content: "saved\n".into(),
                expected_revision: initial.revision,
                run_id: None,
            },
        )
        .unwrap();
        assert_eq!(fs::read_to_string(&file).unwrap(), "saved\n");
        assert_eq!(saved.revision, local_file_revision(b"saved\n"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            assert_eq!(
                fs::metadata(&file).unwrap().permissions().mode() & 0o777,
                0o744
            );
        }

        fs::write(&file, "external\n").unwrap();
        let conflict = save_local_file_snapshot(
            workspace.path(),
            SaveLocalFileRequest {
                path: saved.path,
                content: "local overwrite\n".into(),
                expected_revision: saved.revision,
                run_id: None,
            },
        )
        .unwrap_err();
        let LocalFileFailure::Conflict(disk) = conflict else {
            panic!("expected an optimistic concurrency conflict");
        };
        assert_eq!(disk.content, "external\n");
        assert_eq!(disk.revision, local_file_revision(b"external\n"));
        assert_eq!(fs::read_to_string(&file).unwrap(), "external\n");
    }

    #[tokio::test]
    async fn model_catalog_script_returns_javascript_content_type_and_catalog_api() {
        let response = serve_model_catalog_js().await.into_response();

        assert_eq!(
            response
                .headers()
                .get(CONTENT_TYPE)
                .and_then(|value| value.to_str().ok()),
            Some("application/javascript; charset=utf-8")
        );
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body = std::str::from_utf8(&body).unwrap();
        assert!(body.contains("ModelCatalog"));
        assert!(body.contains("optionText"));
    }
}

#[cfg(test)]
mod startup_working_directory_tests {
    use std::io;
    use std::path::PathBuf;

    use super::capture_workspace_root;

    #[test]
    fn startup_working_directory_errors_are_returned_to_the_caller() {
        let read_error = capture_workspace_root(
            || Err(io::Error::new(io::ErrorKind::NotFound, "cwd missing")),
            |_| Ok(PathBuf::from("unused")),
        )
        .unwrap_err();
        assert!(read_error.contains("读取服务启动工作目录失败"));
        assert!(read_error.contains("cwd missing"));

        let canonicalize_error = capture_workspace_root(
            || Ok(PathBuf::from("missing-workspace")),
            |_| {
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "canonicalize denied",
                ))
            },
        )
        .unwrap_err();
        assert!(canonicalize_error.contains("读取服务启动工作目录失败"));
        assert!(canonicalize_error.contains("canonicalize denied"));
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
