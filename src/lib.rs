pub mod config;
mod dns;

use axum::{
    Router,
    body::to_bytes,
    extract::{Request, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use config::{ClientConfig, Config};
use dns::{Dns, DnsError};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{sync::Arc, time::Duration};
use subtle::ConstantTimeEq;
use tokio::sync::Mutex;

struct Client {
    config: ClientConfig,
    lock: Mutex<()>,
}

struct App {
    clients: Vec<Client>,
    dns: Dns,
}

pub fn router(config: Config, token: String) -> Result<Router, reqwest::Error> {
    let dns = Dns::new(&config.zone_id, token)?;
    Ok(app(config, dns))
}

fn app(config: Config, dns: Dns) -> Router {
    let state = Arc::new(App {
        clients: config
            .clients
            .into_iter()
            .map(|config| Client {
                config,
                lock: Mutex::new(()),
            })
            .collect(),
        dns,
    });
    Router::new()
        .route("/healthz", get(|| async { StatusCode::OK }))
        .route("/v1/challenges", post(upsert))
        .with_state(state)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Challenge {
    value: String,
}

async fn upsert(State(app): State<Arc<App>>, request: Request) -> Result<Response, ApiError> {
    if request.uri().query().is_some() {
        return Err(ApiError(StatusCode::BAD_REQUEST, "query_not_allowed"));
    }
    let token = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .filter(|token| base64url(token));
    let token = token.ok_or(ApiError(StatusCode::UNAUTHORIZED, "unauthorized"))?;
    let digest = format!("{:x}", Sha256::digest(token.as_bytes()));
    let client = app
        .clients
        .iter()
        .find(|client| {
            client
                .config
                .token_sha256
                .iter()
                .any(|hash| bool::from(hash.as_bytes().ct_eq(digest.as_bytes())))
        })
        .ok_or(ApiError(StatusCode::UNAUTHORIZED, "unauthorized"))?;
    let content_type = request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("");
    if !content_type
        .split(';')
        .next()
        .is_some_and(|s| s.trim().eq_ignore_ascii_case("application/json"))
        || request.headers().contains_key(header::CONTENT_ENCODING)
    {
        return Err(ApiError(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "json_required",
        ));
    }
    let body = tokio::time::timeout(Duration::from_secs(5), to_bytes(request.into_body(), 512))
        .await
        .map_err(|_| ApiError(StatusCode::REQUEST_TIMEOUT, "body_timeout"))?
        .map_err(|_| ApiError(StatusCode::PAYLOAD_TOO_LARGE, "invalid_body"))?;
    let challenge: Challenge = serde_json::from_slice(&body)
        .map_err(|_| ApiError(StatusCode::BAD_REQUEST, "invalid_body"))?;
    if !base64url(&challenge.value) {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_value"));
    }

    // Serialize list-then-write per location in this single container instance.
    // No persistent lock/state and no claim of atomicity across multiple replicas.
    let result = tokio::time::timeout(Duration::from_secs(15), async {
        let _lock = client.lock.lock().await;
        app.dns
            .upsert(&client.config.record_name, &challenge.value)
            .await
    })
    .await;
    let error = match result {
        Ok(Ok(())) => {
            eprintln!("event=upsert client={} status=200", client.config.id);
            return Ok((
                [(header::CACHE_CONTROL, "no-store")],
                axum::Json(json!({"ok":true})),
            )
                .into_response());
        }
        Ok(Err(DnsError::MultipleRecords)) => {
            ApiError(StatusCode::CONFLICT, "multiple_txt_records")
        }
        Ok(Err(DnsError::InvalidResponse)) => {
            ApiError(StatusCode::BAD_GATEWAY, "dns_invalid_response")
        }
        Ok(Err(DnsError::Upstream)) => ApiError(StatusCode::BAD_GATEWAY, "dns_api_error"),
        Err(_) => ApiError(StatusCode::GATEWAY_TIMEOUT, "dns_timeout"),
    };
    eprintln!(
        "event=upsert client={} status={} code={}",
        client.config.id,
        error.0.as_u16(),
        error.1
    );
    Err(error)
}

fn base64url(value: &str) -> bool {
    value.len() == 43
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

struct ApiError(StatusCode, &'static str);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let mut response = (self.0, axum::Json(json!({"error":self.1}))).into_response();
        response
            .headers_mut()
            .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
        if self.0 == StatusCode::UNAUTHORIZED {
            response
                .headers_mut()
                .insert(header::WWW_AUTHENTICATE, "Bearer".parse().unwrap());
        }
        response
    }
}

#[cfg(test)]
mod tests;
