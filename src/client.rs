//! ACMEDNS compatibility for cert-manager, forwarding to an existing acme-dns server.

use crate::{ApiError, base64url, read_json};
use axum::{
    Router,
    extract::{Request, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use reqwest::{Client, Url};
use serde::Deserialize;
use serde_json::json;
use std::time::Duration;

#[derive(Clone)]
struct Upstream {
    http: Client,
    endpoint: Url,
}

pub fn router(server_url: &str) -> Result<Router, Box<dyn std::error::Error>> {
    Ok(app(Upstream {
        http: http_client(Duration::from_secs(20))?,
        endpoint: endpoint(server_url)?,
    }))
}

fn endpoint(server_url: &str) -> Result<Url, &'static str> {
    let mut url = Url::parse(server_url).map_err(|_| "Invalid ACME_DNS_SERVER_URL")?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("ACME_DNS_SERVER_URL must be HTTPS without credentials, query or fragment");
    }
    url.set_path(&format!(
        "{}/v1/challenges",
        url.path().trim_end_matches('/')
    ));
    Ok(url)
}

fn http_client(timeout: Duration) -> Result<Client, reqwest::Error> {
    Client::builder()
        .timeout(timeout)
        .connect_timeout(Duration::from_secs(3))
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .build()
}

fn app(upstream: Upstream) -> Router {
    Router::new()
        .route("/healthz", get(|| async { StatusCode::OK }))
        .route("/update", post(update))
        .with_state(upstream)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Update {
    #[serde(alias = "SubDomain")]
    subdomain: String,
    #[serde(alias = "Txt")]
    txt: String,
}

#[derive(Deserialize)]
struct Acknowledgement {
    ok: bool,
}

async fn update(State(upstream): State<Upstream>, request: Request) -> Result<Response, ApiError> {
    if request.uri().query().is_some() {
        return Err(ApiError(StatusCode::BAD_REQUEST, "query_not_allowed"));
    }
    let token = request
        .headers()
        .get("x-api-key")
        .and_then(|h| h.to_str().ok())
        .filter(|token| base64url(token))
        .ok_or(ApiError(StatusCode::UNAUTHORIZED, "unauthorized"))?
        .to_owned();
    let user = request
        .headers()
        .get("x-api-user")
        .and_then(|h| h.to_str().ok())
        .filter(|user| {
            !user.is_empty()
                && user.len() <= 32
                && user
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
        .ok_or(ApiError(StatusCode::UNAUTHORIZED, "unauthorized"))?
        .to_owned();
    let challenge: Update = read_json(request).await?;
    if challenge.subdomain != user {
        return Err(ApiError(StatusCode::FORBIDDEN, "invalid_subdomain"));
    }
    if !base64url(&challenge.txt) {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_value"));
    }

    // The upstream authenticates the key and chooses its one configured DNS name.
    // Never accept an upstream URL or DNS target from the request, or follow redirects.
    let mut response = upstream
        .http
        .post(upstream.endpoint)
        .bearer_auth(token)
        .json(&json!({"value":challenge.txt}))
        .send()
        .await
        .map_err(transport_error)?;
    match response.status() {
        StatusCode::OK => {}
        status @ (StatusCode::UNAUTHORIZED
        | StatusCode::FORBIDDEN
        | StatusCode::CONFLICT
        | StatusCode::TOO_MANY_REQUESTS
        | StatusCode::GATEWAY_TIMEOUT) => {
            return Err(ApiError(status, "upstream_rejected"));
        }
        _ => return Err(ApiError(StatusCode::BAD_GATEWAY, "upstream_error")),
    }
    // Bound the reply and require a positive acknowledgement before reporting success.
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(transport_error)? {
        if body.len() + chunk.len() > 1024 {
            return Err(ApiError(
                StatusCode::BAD_GATEWAY,
                "upstream_invalid_response",
            ));
        }
        body.extend_from_slice(&chunk);
    }
    let ack: Acknowledgement = serde_json::from_slice(&body)
        .map_err(|_| ApiError(StatusCode::BAD_GATEWAY, "upstream_invalid_response"))?;
    if !ack.ok {
        return Err(ApiError(
            StatusCode::BAD_GATEWAY,
            "upstream_invalid_response",
        ));
    }
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        axum::Json(json!({"txt":challenge.txt})),
    )
        .into_response())
}

fn transport_error(error: reqwest::Error) -> ApiError {
    // Do not expose upstream URLs, credentials or response bodies in errors.
    if error.is_timeout() {
        ApiError(StatusCode::GATEWAY_TIMEOUT, "upstream_timeout")
    } else {
        ApiError(StatusCode::BAD_GATEWAY, "upstream_unavailable")
    }
}

#[cfg(test)]
mod tests;
