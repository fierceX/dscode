//! REST + SSE API. Unified `ApiResponse{code,message,data}` envelope.

use crate::bridge::{read_conversation, read_history};
use crate::session::registry::Registry;
use axum::extract::{Path as AxumPath, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;
use std::path::PathBuf;
use std::sync::Arc;

pub struct ApiState {
    pub registry: Arc<Registry>,
    pub cwd: PathBuf,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ApiResponse {
    pub code: u16,
    pub message: String,
    pub data: serde_json::Value,
}

impl ApiResponse {
    fn ok(data: serde_json::Value) -> Self {
        Self {
            code: 200,
            message: String::new(),
            data,
        }
    }

    fn err(code: u16, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: serde_json::Value::Null,
        }
    }
}

impl IntoResponse for ApiResponse {
    fn into_response(self) -> Response {
        let status = if self.code == 200 {
            StatusCode::OK
        } else if self.code == 404 {
            StatusCode::NOT_FOUND
        } else if self.code == 400 {
            StatusCode::BAD_REQUEST
        } else if self.code == 413 {
            StatusCode::PAYLOAD_TOO_LARGE
        } else if self.code == 409 {
            StatusCode::CONFLICT
        } else if self.code == 429 {
            StatusCode::TOO_MANY_REQUESTS
        } else {
            StatusCode::INTERNAL_SERVER_ERROR
        };
        (status, Json(self)).into_response()
    }
}

pub fn router(state: Arc<ApiState>) -> Router {
    Router::new()
        .route("/api/sessions", get(list_sessions).post(create_session))
        .route(
            "/api/sessions/{id}",
            get(get_session).delete(delete_session),
        )
        .route("/api/sessions/{id}/open", post(open_session))
        .route("/api/sessions/{id}/close", post(close_session))
        .route("/api/sessions/{id}/turn", post(turn_session))
        .route("/api/sessions/{id}/interrupt", post(interrupt_session))
        .route("/api/sessions/{id}/events", get(events_history))
        .route("/api/sessions/{id}/conversation", get(conversation_history))
        .route("/api/sessions/{id}/stream", get(stream_events))
        .route("/api/sessions/{id}/plan", get(get_plan))
        .route("/api/sessions/{id}/todo", get(get_todo))
        .route("/api/sessions/{id}/artifacts", get(list_artifacts))
        .route("/api/sessions/{id}/artifacts/{name}", get(get_artifact))
        .route("/api/sessions/{id}/files", get(get_files))
        .route("/health", get(health))
        .with_state(state)
}

#[derive(Deserialize)]
struct CreateSessionReq {
    name: Option<String>,
    #[serde(default)]
    cwd: Option<PathBuf>,
}

#[derive(Deserialize)]
struct TurnReq {
    input: String,
}

#[derive(Default, Deserialize)]
struct ProjectQuery {
    project: Option<String>,
}

#[derive(Deserialize)]
struct HistoryQuery {
    project: Option<String>,
    #[serde(default)]
    from_seq: u64,
    #[serde(default = "default_limit")]
    limit: usize,
    #[serde(default)]
    tail: bool,
    #[serde(default)]
    before_seq: Option<u64>,
}

fn default_limit() -> usize {
    500
}

#[derive(Deserialize)]
struct FilesQuery {
    project: Option<String>,
    path: Option<String>,
    #[serde(default)]
    raw: bool,
}

const FILE_RAW_MAX_BYTES: u64 = 1 << 20; // 1 MiB

/// Resolve a session directory with the registry's typed status mapping.
async fn session_dir_or_err(
    state: &ApiState,
    id: &str,
    project: Option<&str>,
) -> Result<std::path::PathBuf, ApiResponse> {
    state
        .registry
        .session_dir(id, project)
        .await
        .map_err(registry_error)
}

fn registry_error(error: crate::session::registry::RegistryError) -> ApiResponse {
    use crate::session::registry::RegistryError;
    let code = match &error {
        RegistryError::NotFound(_) => 404,
        RegistryError::Ambiguous(_) | RegistryError::Locked(_) | RegistryError::Busy(_) => 409,
        RegistryError::Capacity(_) => 429,
        RegistryError::Internal(_) => 500,
    };
    ApiResponse::err(code, error.to_string())
}
async fn list_sessions(State(state): State<Arc<ApiState>>) -> ApiResponse {
    match state.registry.list().await {
        Ok(sessions) => ApiResponse::ok(json!(sessions)),
        Err(e) => ApiResponse::err(500, e.to_string()),
    }
}

async fn create_session(
    State(state): State<Arc<ApiState>>,
    Json(req): Json<CreateSessionReq>,
) -> ApiResponse {
    let cwd = req.cwd.clone().unwrap_or_else(|| state.cwd.clone());
    let name = req.name.unwrap_or_else(|| "unnamed".to_string());
    match state.registry.create(&name, &cwd).await {
        Ok(summary) => ApiResponse::ok(json!(summary)),
        Err(error) => registry_error(error),
    }
}

async fn get_session(
    State(state): State<Arc<ApiState>>,
    AxumPath(id): AxumPath<String>,
    Query(query): Query<ProjectQuery>,
) -> ApiResponse {
    match state
        .registry
        .session_dir(&id, query.project.as_deref())
        .await
    {
        Ok(_) => {
            let is_open = state
                .registry
                .is_open(&id, query.project.as_deref())
                .unwrap_or(false);
            let running = state
                .registry
                .running(&id, query.project.as_deref())
                .unwrap_or(false);
            ApiResponse::ok(json!({ "id": id, "open": is_open, "running": running }))
        }
        Err(error) => registry_error(error),
    }
}

async fn delete_session(
    State(state): State<Arc<ApiState>>,
    AxumPath(id): AxumPath<String>,
    Query(query): Query<ProjectQuery>,
) -> ApiResponse {
    match state.registry.delete(&id, query.project.as_deref()).await {
        Ok(()) => ApiResponse::ok(json!({ "id": id, "deleted": true })),
        Err(error) => registry_error(error),
    }
}

async fn open_session(
    State(state): State<Arc<ApiState>>,
    AxumPath(id): AxumPath<String>,
    Query(query): Query<ProjectQuery>,
) -> ApiResponse {
    match state.registry.open(&id, query.project.as_deref()).await {
        Ok(_) => ApiResponse::ok(json!({ "id": id, "status": "active" })),
        Err(error) => registry_error(error),
    }
}

async fn close_session(
    State(state): State<Arc<ApiState>>,
    AxumPath(id): AxumPath<String>,
    Query(query): Query<ProjectQuery>,
) -> ApiResponse {
    match state.registry.close(&id, query.project.as_deref()).await {
        Ok(_) => ApiResponse::ok(json!({ "id": id, "status": "free" })),
        Err(error) => registry_error(error),
    }
}

async fn turn_session(
    State(state): State<Arc<ApiState>>,
    AxumPath(id): AxumPath<String>,
    Query(query): Query<ProjectQuery>,
    Json(req): Json<TurnReq>,
) -> ApiResponse {
    let input = req.input.trim().to_string();
    if input.is_empty() {
        return ApiResponse::err(400, "input must not be empty");
    }
    if input.len() > 128 * 1024 {
        return ApiResponse::err(400, "input too large (max 128 KiB)");
    }
    match state
        .registry
        .start_turn(&id, query.project.as_deref(), input)
    {
        Ok(_) => ApiResponse::ok(json!({ "id": id, "status": "running" })),
        Err(error) => registry_error(error),
    }
}

async fn interrupt_session(
    State(state): State<Arc<ApiState>>,
    AxumPath(id): AxumPath<String>,
    Query(query): Query<ProjectQuery>,
) -> ApiResponse {
    match state.registry.interrupt(&id, query.project.as_deref()) {
        Ok(_) => ApiResponse::ok(json!({ "id": id, "interrupted": true })),
        Err(error) => registry_error(error),
    }
}

async fn events_history(
    State(state): State<Arc<ApiState>>,
    AxumPath(id): AxumPath<String>,
    Query(query): Query<HistoryQuery>,
) -> ApiResponse {
    if !(1..=2000).contains(&query.limit) {
        return ApiResponse::err(400, "history limit must be in 1..=2000");
    }
    let dir = match state
        .registry
        .session_dir(&id, query.project.as_deref())
        .await
    {
        Ok(d) => d,
        Err(error) => return registry_error(error),
    };
    let events_path = dir.join("events.jsonl");
    match read_history(
        &events_path,
        query.from_seq,
        query.limit,
        query.tail,
        query.before_seq,
    )
    .await
    {
        Ok(events) => ApiResponse::ok(json!(events)),
        Err(e) => ApiResponse::err(500, e.to_string()),
    }
}
async fn conversation_history(
    State(state): State<Arc<ApiState>>,
    AxumPath(id): AxumPath<String>,
    Query(query): Query<HistoryQuery>,
) -> ApiResponse {
    if !(1..=2000).contains(&query.limit) {
        return ApiResponse::err(400, "history limit must be in 1..=2000");
    }
    let dir = match state
        .registry
        .session_dir(&id, query.project.as_deref())
        .await
    {
        Ok(d) => d,
        Err(error) => return registry_error(error),
    };
    let conv_path = dir.join("conversation.jsonl");
    match read_conversation(
        &conv_path,
        query.from_seq,
        query.limit,
        query.tail,
        query.before_seq,
    )
    .await
    {
        Ok(events) => ApiResponse::ok(json!(events)),
        Err(e) => ApiResponse::err(500, e.to_string()),
    }
}

/// SSE：纯转手——订阅 SessionRuntime 的广播通道（AgentEvent 流），
/// 事件帧直接转发，不做轮次/seq 判断（历史由 conversation 接口提供）。
async fn stream_events(
    State(state): State<Arc<ApiState>>,
    AxumPath(id): AxumPath<String>,
    Query(query): Query<ProjectQuery>,
) -> Response {
    use axum::body::Body;
    use axum::http::header::{CACHE_CONTROL, CONTENT_TYPE};
    use http_body_util::StreamBody;

    let session = match state.registry.active_runtime(&id, query.project.as_deref()) {
        Ok(Some(s)) => s,
        Err(error) => return registry_error(error).into_response(),
        Ok(None) => return ApiResponse::err(404, format!("session {id} not open")).into_response(),
    };
    let mut rx = session.event_receiver();
    let stream = async_stream::stream! {
        // 心跳：30s 无事件时发 `: ping` 注释帧，防止中间代理按空闲超时断开
        let mut heartbeat = tokio::time::interval(std::time::Duration::from_secs(30));
        loop {
            tokio::select! {
                recv = rx.recv() => {
                    match recv {
                        Ok(line) => {
                            let sse = format!("data: {line}\n\n");
                            yield Ok::<http_body::Frame<axum::body::Bytes>, std::convert::Infallible>(
                                http_body::Frame::data(axum::body::Bytes::from(sse)),
                            );
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(missed)) => {
                            let line = serde_json::json!({"type":"stream_gap","missed":missed}).to_string();
                            yield Ok::<http_body::Frame<axum::body::Bytes>, std::convert::Infallible>(
                                http_body::Frame::data(axum::body::Bytes::from(format!("data: {line}\n\n"))),
                            );
                            break;
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
                _ = heartbeat.tick() => {
                    yield Ok::<http_body::Frame<axum::body::Bytes>, std::convert::Infallible>(
                        http_body::Frame::data(axum::body::Bytes::from(": ping\n\n".to_string())),
                    );
                }
            }
        }
    };
    Response::builder()
        .header(CONTENT_TYPE, "text/event-stream")
        .header(CACHE_CONTROL, "no-cache")
        .header("X-Accel-Buffering", "no")
        .body(Body::new(StreamBody::new(stream)))
        .unwrap()
}

async fn health() -> ApiResponse {
    ApiResponse::ok(json!({ "status": "ok" }))
}

async fn get_plan(
    State(state): State<Arc<ApiState>>,
    AxumPath(id): AxumPath<String>,
    Query(query): Query<ProjectQuery>,
) -> ApiResponse {
    let dir = match session_dir_or_err(&state, &id, query.project.as_deref()).await {
        Ok(dir) => dir,
        Err(response) => return response,
    };
    let plan = read_optional(&dir.join("plan.md")).await;
    let draft = read_optional(&dir.join("plan.draft")).await;
    ApiResponse::ok(json!({ "plan": plan, "draft": draft }))
}

async fn get_todo(
    State(state): State<Arc<ApiState>>,
    AxumPath(id): AxumPath<String>,
    Query(query): Query<ProjectQuery>,
) -> ApiResponse {
    let dir = match session_dir_or_err(&state, &id, query.project.as_deref()).await {
        Ok(dir) => dir,
        Err(response) => return response,
    };
    let todo = read_optional(&dir.join("todos.json")).await;
    match todo {
        Some(text) => match serde_json::from_str::<serde_json::Value>(&text) {
            Ok(v) => ApiResponse::ok(json!({ "todos": v })),
            Err(e) => ApiResponse::err(500, format!("todos.json parse error: {e}")),
        },
        None => ApiResponse::ok(json!({ "todos": null })),
    }
}

async fn list_artifacts(
    State(state): State<Arc<ApiState>>,
    AxumPath(id): AxumPath<String>,
    Query(query): Query<ProjectQuery>,
) -> ApiResponse {
    let dir = match session_dir_or_err(&state, &id, query.project.as_deref()).await {
        Ok(dir) => dir,
        Err(response) => return response,
    };
    let artifacts_dir = dir.join("artifacts");
    let index = read_optional(&artifacts_dir.join("index.jsonl")).await;
    let mut records = Vec::new();
    if let Some(index) = index {
        for line in index.lines() {
            if line.trim().is_empty() {
                continue;
            }
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(line) {
                records.push(v);
            }
        }
    }
    ApiResponse::ok(json!({ "artifacts": records }))
}

async fn get_artifact(
    State(state): State<Arc<ApiState>>,
    AxumPath((id, name)): AxumPath<(String, String)>,
    Query(query): Query<ProjectQuery>,
) -> ApiResponse {
    let dir = match session_dir_or_err(&state, &id, query.project.as_deref()).await {
        Ok(dir) => dir,
        Err(response) => return response,
    };
    // `artifact://<id>` maps to artifacts/<id>.txt; also accept the raw
    // filename. Reject anything that could escape the artifacts directory.
    if name.contains('/') || name.contains('\\') || name.contains("..") {
        return ApiResponse::err(400, "invalid artifact name");
    }
    let filename = if name.ends_with(".txt") {
        name.clone()
    } else {
        format!("{name}.txt")
    };
    let path = dir.join("artifacts").join(&filename);
    match read_optional(&path).await {
        Some(text) => ApiResponse::ok(json!({ "name": filename, "content": text })),
        None => ApiResponse::err(404, format!("artifact {name} not found")),
    }
}

async fn get_files(
    State(state): State<Arc<ApiState>>,
    AxumPath(id): AxumPath<String>,
    Query(query): Query<FilesQuery>,
) -> ApiResponse {
    if let Err(response) = session_dir_or_err(&state, &id, query.project.as_deref()).await {
        return response;
    }
    // cwd 是会话工作目录（SessionMetadata.cwd），文件浏览限制在其内。
    let cwd = match state
        .registry
        .session_metadata_cwd(&id, query.project.as_deref())
        .await
    {
        Ok(Some(c)) => std::path::PathBuf::from(c),
        Ok(None) => return ApiResponse::err(500, "session metadata cwd is missing"),
        Err(error) => return ApiResponse::err(500, format!("failed to read session cwd: {error}")),
    };
    let rel = query.path.unwrap_or_default();
    let target = {
        let cwd = cwd.clone();
        let rel = rel.clone();
        match tokio::task::spawn_blocking(move || resolve_within(&cwd, &rel)).await {
            Ok(target) => target,
            Err(error) => return ApiResponse::err(500, format!("path task failed: {error}")),
        }
    };
    let Some(target) = target else {
        return ApiResponse::err(400, "path escapes the workspace");
    };

    if query.raw {
        let meta = match tokio::fs::metadata(&target).await {
            Ok(m) => m,
            Err(_) => return ApiResponse::err(404, "file not found"),
        };
        if !meta.is_file() {
            return ApiResponse::err(400, "not a file");
        }
        if meta.len() > FILE_RAW_MAX_BYTES {
            return ApiResponse::err(
                413,
                format!("file too large ({} bytes, limit 1 MiB)", meta.len()),
            );
        }
        match tokio::fs::read_to_string(&target).await {
            Ok(text) => ApiResponse::ok(json!({ "path": rel, "content": text })),
            Err(e) => ApiResponse::err(500, format!("read failed: {e}")),
        }
    } else {
        let mut entries = match tokio::fs::read_dir(&target).await {
            Ok(entries) => entries,
            Err(_) => return ApiResponse::err(404, "directory not found"),
        };
        let mut items = Vec::new();
        loop {
            let entry = match entries.next_entry().await {
                Ok(Some(entry)) => entry,
                Ok(None) => break,
                Err(error) => {
                    return ApiResponse::err(500, format!("directory read failed: {error}"));
                }
            };
            let name = entry.file_name().to_string_lossy().to_string();
            let is_dir = entry.file_type().await.map(|t| t.is_dir()).unwrap_or(false);
            items.push(json!({ "name": name, "dir": is_dir }));
        }
        items.sort_by(|a, b| {
            let da = a["dir"].as_bool().unwrap_or(false);
            let db = b["dir"].as_bool().unwrap_or(false);
            db.cmp(&da)
                .then_with(|| a["name"].as_str().cmp(&b["name"].as_str()))
        });
        ApiResponse::ok(json!({ "path": rel, "dir": true, "items": items }))
    }
}

async fn read_optional(path: &std::path::Path) -> Option<String> {
    tokio::fs::read_to_string(path).await.ok()
}

/// Join `rel` onto `root`, refusing any path that escapes `root`.
fn resolve_within(root: &std::path::Path, rel: &str) -> Option<std::path::PathBuf> {
    let rel_path = std::path::Path::new(rel);
    let joined = root.join(rel_path);
    let canonical_root = std::fs::canonicalize(root).ok()?;
    // 目标可能不存在（目录浏览场景 target 是已存在目录；raw 场景是文件）。
    // 对已存在路径做 canonicalize 校验；不存在的路径按组件词法校验。
    if let Ok(canonical) = std::fs::canonicalize(&joined) {
        return canonical.starts_with(&canonical_root).then_some(canonical);
    }
    if rel_path.is_absolute() {
        return None;
    }
    let mut components = std::path::PathBuf::from(&canonical_root);
    for comp in rel_path.components() {
        match comp {
            std::path::Component::Normal(c) => components.push(c),
            std::path::Component::CurDir => {}
            _ => return None,
        }
    }
    Some(components)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Method, Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    struct MockRuntimeBackend;

    #[async_trait::async_trait]
    impl mink::runtime::LlmBackend for MockRuntimeBackend {
        fn name(&self) -> &str {
            "server-api-mock"
        }

        async fn stream(
            &self,
            _request: mink::runtime::LlmRequest,
        ) -> anyhow::Result<mink::runtime::LlmResponseStream> {
            Ok(mink::runtime::LlmResponseStream {
                events: Box::pin(futures::stream::iter(vec![
                    Ok(mink::runtime::LlmEvent::Text(mink::runtime::LlmTextEvent {
                        content: "mock api answer".into(),
                    })),
                    Ok(mink::runtime::LlmEvent::Stop(mink::runtime::LlmStopEvent {
                        reason: "end_turn".into(),
                    })),
                ])),
                attempt_count: 1,
            })
        }
    }

    /// 临时 home + 一个含 3 行 conversation 的会话 + 1 个 artifact。
    /// 每个调用使用唯一 home（进程内并行测试共享同一 pid 的固定目录会
    /// 互相覆盖 artifact/会话文件，造成偶发竞态失败）。
    fn test_router() -> Router {
        static SEQ: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let home = std::env::temp_dir().join(format!(
            "mink-server-itest-{}-{}",
            std::process::id(),
            SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        ));
        let sess = home
            .join(".mink")
            .join("projects")
            .join("proj")
            .join("test-session");
        std::fs::create_dir_all(&sess).unwrap();
        std::fs::write(
            sess.join("session.json"),
            serde_json::json!({
                "id": "test-session", "alias": null, "title": "t", "cwd": sess.display().to_string(),
                "created_at": "", "updated_at": "", "parent": null, "first_prompt": null, "summary": null
            }).to_string(),
        ).unwrap();
        std::fs::write(
            sess.join("conversation.jsonl"),
            "{\"role\":\"user\",\"content\":\"one\"}\n{\"role\":\"assistant\",\"content\":[{\"type\":\"text\",\"text\":\"two\"}]}\n{\"role\":\"user\",\"content\":\"three\"}\n",
        ).unwrap();
        std::fs::create_dir_all(sess.join("artifacts")).unwrap();
        std::fs::write(sess.join("artifacts/abc.txt"), "artifact body").unwrap();

        let registry = Arc::new(Registry::new(home, "flash".to_string(), 4));
        let state = Arc::new(ApiState {
            registry,
            cwd: std::env::temp_dir(),
        });
        router(state)
    }

    async fn req(app: &Router, method: Method, uri: &str) -> (StatusCode, serde_json::Value) {
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = resp.status();
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        let json = serde_json::from_slice(&body).unwrap_or(serde_json::Value::Null);
        (status, json)
    }

    #[tokio::test]
    async fn conversation_pagination_and_seq() {
        let app = test_router();
        // tail：返回最后 2 行，注入行号 seq
        let (s, body) = req(
            &app,
            Method::GET,
            "/api/sessions/test-session/conversation?limit=2&tail=true",
        )
        .await;
        assert_eq!(s, StatusCode::OK);
        let rows = body["data"].as_array().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["seq"], serde_json::json!(2));
        assert_eq!(rows[1]["content"], serde_json::json!("three"));
        // before_seq：取 seq 之前的行
        let (_, body) = req(
            &app,
            Method::GET,
            "/api/sessions/test-session/conversation?limit=1&before_seq=3",
        )
        .await;
        let rows = body["data"].as_array().unwrap();
        assert_eq!(rows[0]["seq"], serde_json::json!(2));
        // 不存在的会话 → 404
        let (s, _) = req(&app, Method::GET, "/api/sessions/nope/conversation").await;
        assert_eq!(s, StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn files_path_escape_rejected() {
        let app = test_router();
        let (s, body) = req(
            &app,
            Method::GET,
            "/api/sessions/test-session/files?path=../../../etc",
        )
        .await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        assert!(body["message"].as_str().unwrap_or("").contains("escape"));
    }

    #[tokio::test]
    async fn artifact_name_filtered() {
        let app = test_router();
        let (s, _body) = req(
            &app,
            Method::GET,
            "/api/sessions/test-session/artifacts/..%2F..%2Fpasswd",
        )
        .await;
        assert_eq!(s, StatusCode::BAD_REQUEST);
        // 正常 artifact 可读
        let (s, body) = req(
            &app,
            Method::GET,
            "/api/sessions/test-session/artifacts/abc",
        )
        .await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(body["data"]["content"], serde_json::json!("artifact body"));
    }

    #[tokio::test]
    async fn turn_input_validation() {
        let app = test_router();
        // 空 body → axum JSON 提取器拒绝（415）
        let (s, _) = req(&app, Method::POST, "/api/sessions/test-session/turn").await;
        assert_eq!(s, StatusCode::UNSUPPORTED_MEDIA_TYPE);
        // 带 JSON body 但 input 为空 → 400
        let resp = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/api/sessions/test-session/turn")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"input":""}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn health_and_plan() {
        let app = test_router();
        let (s, body) = req(&app, Method::GET, "/health").await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(body["data"]["status"], serde_json::json!("ok"));
        let (s, body) = req(&app, Method::GET, "/api/sessions/test-session/plan").await;
        assert_eq!(s, StatusCode::OK);
        assert_eq!(body["data"]["plan"], serde_json::Value::Null);
    }

    #[tokio::test]
    async fn mock_runtime_api_streams_authoritative_sse_envelope() {
        let home = std::env::temp_dir().join(format!(
            "mink-server-mock-sse-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let cwd = home.join("project");
        std::fs::create_dir_all(&cwd).unwrap();
        let registry = Arc::new(Registry::with_llm_backend(
            home.clone(),
            "mock".into(),
            1,
            Arc::new(MockRuntimeBackend),
        ));
        let created = registry.create("mock-sse", &cwd).await.unwrap();
        let project = created.project_key.clone();
        let state = Arc::new(ApiState {
            registry: registry.clone(),
            cwd,
        });
        let app = router(state);

        let (status, _) = req(
            &app,
            Method::POST,
            &format!("/api/sessions/{}/open?project={project}", created.id),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let stream_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/api/sessions/{}/stream?project={project}",
                        created.id
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(stream_response.status(), StatusCode::OK);
        let mut body = stream_response.into_body();

        let turn_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri(format!(
                        "/api/sessions/{}/turn?project={project}",
                        created.id
                    ))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"input":"hello"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(turn_response.status(), StatusCode::OK);

        let mut types = Vec::new();
        let mut saw_stream_sequence = false;
        tokio::time::timeout(std::time::Duration::from_secs(3), async {
            while let Some(frame) = body.frame().await {
                let frame = frame.unwrap();
                let Ok(data) = frame.into_data() else {
                    continue;
                };
                for line in String::from_utf8_lossy(&data).lines() {
                    let Some(json) = line.strip_prefix("data: ") else {
                        continue;
                    };
                    let event: serde_json::Value = serde_json::from_str(json).unwrap();
                    saw_stream_sequence |= event["stream_sequence"].is_number();
                    if let Some(kind) = event["type"].as_str() {
                        types.push(kind.to_string());
                        if kind == "turn_final" {
                            return;
                        }
                    }
                }
            }
        })
        .await
        .expect("mock runtime SSE did not reach turn_final");

        assert!(saw_stream_sequence);
        assert!(types.starts_with(&["turn_started".into()]));
        assert!(types.contains(&"text".into()));
        assert!(types.contains(&"stop".into()));
        assert_eq!(types.last().map(String::as_str), Some("turn_final"));
        registry.close(&created.id, Some(&project)).await.unwrap();
        let _ = std::fs::remove_dir_all(home);
    }
}
