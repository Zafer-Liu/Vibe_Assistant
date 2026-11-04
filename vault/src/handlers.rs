use axum::{
    extract::{DefaultBodyLimit, Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use serde::Deserialize;
use serde_json::json;

use crate::store::{self, CasError, SharedStore};

#[derive(Deserialize)]
pub struct PutBody {
    pub content_hash: String,
    #[serde(with = "serde_bytes_base64")]
    pub payload: Vec<u8>,
    pub updated_at: String,
    pub device_id: String,
}

/// payload 用标准 base64（无填充也接受）传输；为避免引入 base64 crate，
/// 这里手写一个宽松的 serde 辅助。Phase 1 数据量小，性能足够。
mod serde_bytes_base64 {
    use serde::{Deserialize, Deserializer};

    fn decode_b64(s: &str) -> Option<Vec<u8>> {
        const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let filtered: Vec<u8> = s.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
        let trimmed: &[u8] = {
            let end = filtered
                .iter()
                .rposition(|b| *b != b'=')
                .map_or(0, |i| i + 1);
            &filtered[..end]
        };
        if trimmed.len() % 4 == 1 {
            return None;
        }
        let mut out = Vec::with_capacity(trimmed.len() * 3 / 4);
        let mut buf: u32 = 0;
        let mut bits = 0u32;
        for byte in trimmed {
            let v = TABLE.iter().position(|c| *c == *byte)? as u32;
            buf = (buf << 6) | v;
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                out.push((buf >> bits) as u8);
            }
        }
        Some(out)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<u8>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        decode_b64(&s).ok_or_else(|| serde::de::Error::custom("invalid base64 payload"))
    }
}

#[derive(Deserialize)]
struct SinceQuery {
    since_rev: Option<i64>,
}

pub fn router(store: SharedStore) -> Router {
    Router::new()
        .route("/objects", get(list_objects))
        .route(
            "/objects/:key",
            get(get_object)
                .put(put_object)
                .delete(delete_object)
                .layer(DefaultBodyLimit::disable()),
        )
        .route_layer(axum::middleware::from_fn_with_state(
            store.clone(),
            auth,
        ))
        // /health 免认证：连接测试按钮只探测存活，不携带 PAT。
        .route("/health", get(health))
        .with_state(store)
}

async fn auth(
    State(store): State<SharedStore>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let token = req
        .headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    let Some(token) = token else {
        return (StatusCode::UNAUTHORIZED, "missing bearer token").into_response();
    };
    if !store::check_token(&store, token) {
        return (StatusCode::UNAUTHORIZED, "invalid token").into_response();
    }
    if let Some(device) = req.headers().get("x-device-id").and_then(|v| v.to_str().ok()) {
        if !device.is_empty() && device.len() <= 128 {
            store::touch_device(&store, device);
        }
    }
    next.run(req).await
}

async fn health() -> impl IntoResponse {
    Json(json!({ "status": "ok", "service": "agent-manager-vault", "version": env!("CARGO_PKG_VERSION") }))
}

async fn list_objects(
    State(store): State<SharedStore>,
    Query(q): Query<SinceQuery>,
) -> impl IntoResponse {
    let since = q.since_rev.unwrap_or(0);
    let (objects, tombstones, revision) = store::objects_since(&store, since);
    Json(json!({
        "objects": objects,
        "tombstones": tombstones,
        "revision": revision,
    }))
}

async fn get_object(
    State(store): State<SharedStore>,
    Path(key): Path<String>,
) -> Response {
    match store::get_object(&store, &key) {
        Some(obj) => Json(obj).into_response(),
        None => (StatusCode::NOT_FOUND, "object not found").into_response(),
    }
}

async fn put_object(
    State(store): State<SharedStore>,
    Path(key): Path<String>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let parsed: PutBody = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(e) => return (StatusCode::BAD_REQUEST, format!("bad body: {e}")).into_response(),
    };
    if key.is_empty() || key.len() > 256 {
        return (StatusCode::BAD_REQUEST, "invalid key").into_response();
    }
    let base_revision: i64 = headers
        .get("if-match")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse().ok())
        .unwrap_or(-1);
    if base_revision < 0 {
        return (StatusCode::PRECONDITION_FAILED, "missing If-Match revision").into_response();
    }
    match store::put_object(
        &store,
        &key,
        base_revision,
        parsed.content_hash,
        parsed.payload,
        &parsed.updated_at,
        &parsed.device_id,
    ) {
        Ok(rev) => Json(json!({ "revision": rev })).into_response(),
        Err(CasError::Conflict { current_revision }) => (
            StatusCode::CONFLICT,
            Json(json!({ "error": "revision conflict", "current_revision": current_revision })),
        )
            .into_response(),
    }
}

async fn delete_object(
    State(store): State<SharedStore>,
    Path(key): Path<String>,
    headers: HeaderMap,
) -> Response {
    let base_revision: i64 = match headers
        .get("if-match")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse().ok())
    {
        Some(value) if value >= 0 => value,
        _ => return (StatusCode::PRECONDITION_FAILED, "missing If-Match revision").into_response(),
    };
    let device = headers
        .get("x-device-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    match store::delete_object(&store, &key, base_revision, &device) {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => (StatusCode::NOT_FOUND, "object not found").into_response(),
        Err(CasError::Conflict { current_revision }) => (
            StatusCode::CONFLICT,
            Json(json!({ "error": "revision conflict", "current_revision": current_revision })),
        ).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[tokio::test]
    async fn large_object_passes_router_and_payload_validation() {
        let store = Arc::new(store::open(":memory:").unwrap());
        let _ = store::bootstrap_tokens(&store, Some("size-limit-test-token"));
        let payload = "A".repeat(3 * 1024 * 1024);
        let body = json!({
            "content_hash": "large-payload-hash",
            "payload": &payload,
            "updated_at": "2026-11-04T00:00:00Z",
            "device_id": "test-device",
        }).to_string();
        assert!(body.len() > 2 * 1024 * 1024);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let app = router(store.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let response = tokio::time::timeout(std::time::Duration::from_secs(20), async {
            let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
            let headers = format!(
                "PUT /objects/large HTTP/1.1\r\nHost: {address}\r\nAuthorization: Bearer size-limit-test-token\r\nIf-Match: 0\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len(),
            );
            stream.write_all(headers.as_bytes()).await.unwrap();
            stream.write_all(body.as_bytes()).await.unwrap();
            let mut response = Vec::new();
            stream.read_to_end(&mut response).await.unwrap();
            response
        }).await.expect("large object request timed out");
        server.abort();
        assert!(String::from_utf8_lossy(&response).starts_with("HTTP/1.1 200"));
        let stored = store::get_object(&store, "large").unwrap();
        assert_eq!(stored.payload, payload, "payload must not be truncated");
    }
}
