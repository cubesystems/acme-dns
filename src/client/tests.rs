use super::*;
use axum::{
    body::{Body, to_bytes},
    http::{HeaderMap, Method, Request as HttpRequest, Uri},
    routing::any,
};
use serde_json::Value;
use std::sync::Arc;
use tokio::{sync::Mutex, task::JoinHandle};
use tower::ServiceExt;

#[derive(Default)]
struct Mock {
    calls: Vec<(Method, Uri, HeaderMap, Value)>,
    status: StatusCode,
    body: String,
    delay: Duration,
}

struct Harness {
    app: Router,
    mock: Arc<Mutex<Mock>>,
    server: JoinHandle<()>,
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.server.abort();
    }
}

impl Harness {
    async fn new() -> Self {
        let mock = Arc::new(Mutex::new(Mock {
            status: StatusCode::OK,
            body: "{\"ok\":true}".into(),
            ..Default::default()
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = Url::parse(&format!(
            "http://{}/v1/challenges",
            listener.local_addr().unwrap()
        ))
        .unwrap();
        let service = Router::new()
            .fallback(any(mock_upstream))
            .with_state(mock.clone());
        let server = tokio::spawn(async move { axum::serve(listener, service).await.unwrap() });
        // Only tests bypass the HTTPS configuration check to reach a local mock.
        let app = app(Upstream {
            http: http_client(Duration::from_millis(200)).unwrap(),
            endpoint,
        });
        Self { app, mock, server }
    }

    async fn send(
        &self,
        user: Option<&str>,
        key: Option<&str>,
        path: &str,
        content_type: &str,
        body: String,
    ) -> Response {
        let mut request = HttpRequest::builder()
            .method("POST")
            .uri(path)
            .header(header::CONTENT_TYPE, content_type);
        if let Some(user) = user {
            request = request.header("X-Api-User", user);
        }
        if let Some(key) = key {
            request = request.header("X-Api-Key", key);
        }
        self.app
            .clone()
            .oneshot(request.body(Body::from(body)).unwrap())
            .await
            .unwrap()
    }

    async fn update(&self, id: &str, token: &str, value: &str) -> Response {
        self.send(
            Some(id),
            Some(token),
            "/update",
            "application/json",
            json!({"subdomain":id, "txt":value}).to_string(),
        )
        .await
    }
}

async fn mock_upstream(
    State(mock): State<Arc<Mutex<Mock>>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: String,
) -> Response {
    let (status, body, delay) = {
        let mut mock = mock.lock().await;
        mock.calls
            .push((method, uri, headers, serde_json::from_str(&body).unwrap()));
        (mock.status, mock.body.clone(), mock.delay)
    };
    tokio::time::sleep(delay).await;
    (
        status,
        [
            (header::LOCATION, "/must-not-follow"),
            (header::CONTENT_TYPE, "application/json"),
        ],
        body,
    )
        .into_response()
}

#[test]
fn requires_a_fixed_https_server_without_embedded_secrets() {
    assert_eq!(
        endpoint("https://acme.example.com").unwrap().as_str(),
        "https://acme.example.com/v1/challenges"
    );
    assert_eq!(
        endpoint("https://acme.example.com/base/").unwrap().as_str(),
        "https://acme.example.com/base/v1/challenges"
    );
    for url in [
        "",
        "http://acme.example.com",
        "file:///tmp/api",
        "https://user:secret@acme.example.com",
        "https://acme.example.com?token=secret",
        "https://acme.example.com/#fragment",
    ] {
        assert!(endpoint(url).is_err(), "{url}");
    }
}

#[tokio::test]
async fn translates_requests_with_each_clients_own_token() {
    let h = Harness::new().await;
    for (id, key, value) in [
        ("site-a", "k".repeat(43), "A".repeat(43)),
        ("site-b", "y".repeat(43), "B".repeat(43)),
    ] {
        let response = h.update(id, &key, &value).await;
        assert_eq!(response.status(), 200);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        assert_eq!(
            serde_json::from_slice::<Value>(&to_bytes(response.into_body(), 1024).await.unwrap())
                .unwrap(),
            json!({"txt":value})
        );
        let mock = h.mock.lock().await;
        let (method, uri, headers, body) = mock.calls.last().unwrap();
        assert_eq!(*method, Method::POST);
        assert_eq!(uri, "/v1/challenges");
        assert_eq!(headers[header::AUTHORIZATION], format!("Bearer {key}"));
        assert!(!headers.contains_key("x-api-key"));
        assert_eq!(body, &json!({"value":value}));
    }
    assert_eq!(h.mock.lock().await.calls.len(), 2);
}

#[tokio::test]
async fn invalid_requests_never_reach_the_server() {
    let h = Harness::new().await;
    let key = "k".repeat(43);
    for (user, key) in [
        (None, None),
        (Some("site-a"), None),
        (None, Some(key.as_str())),
        (Some("site-a"), Some("invalid")),
        (Some("invalid/user"), Some(key.as_str())),
    ] {
        assert_eq!(
            h.send(user, key, "/update", "application/json", "{}".into())
                .await
                .status(),
            401
        );
    }
    for (path, content_type, body, expected) in [
        (
            "/update",
            "application/json",
            json!({"subdomain":"other", "txt":"A".repeat(43)}).to_string(),
            403,
        ),
        (
            "/update",
            "application/json",
            json!({"subdomain":"site-a", "txt":"A".repeat(43), "url":"https://evil.example"})
                .to_string(),
            400,
        ),
        (
            "/update",
            "application/json",
            json!({"subdomain":"site-a", "txt":"!".repeat(43)}).to_string(),
            400,
        ),
        ("/update", "application/json", "{}".into(), 400),
        ("/update", "application/json", "{".into(), 400),
        ("/update", "application/json", " ".repeat(513), 413),
        ("/update", "text/plain", "{}".into(), 415),
        (
            "/update?url=https://evil.example",
            "application/json",
            "{}".into(),
            400,
        ),
        ("/register", "application/json", "{}".into(), 404),
        ("/v1/challenges", "application/json", "{}".into(), 404),
    ] {
        assert_eq!(
            h.send(Some("site-a"), Some(&key), path, content_type, body)
                .await
                .status(),
            expected
        );
    }
    assert!(h.mock.lock().await.calls.is_empty());
}

#[tokio::test]
async fn upstream_errors_and_redirects_fail_without_retries_or_leaking_details() {
    let h = Harness::new().await;
    for (upstream, expected) in [
        (401, 401),
        (403, 403),
        (409, 409),
        (429, 429),
        (504, 504),
        (500, 502),
        (302, 502),
    ] {
        let before = {
            let mut mock = h.mock.lock().await;
            mock.status = StatusCode::from_u16(upstream).unwrap();
            mock.body = "private server response and credentials".into();
            mock.calls.len()
        };
        let response = h.update("site-a", &"k".repeat(43), &"A".repeat(43)).await;
        assert_eq!(response.status(), expected);
        let body = to_bytes(response.into_body(), 1024).await.unwrap();
        let error: Value = serde_json::from_slice(&body).unwrap();
        assert!(matches!(
            error["error"].as_str(),
            Some("upstream_rejected" | "upstream_error")
        ));
        assert_eq!(h.mock.lock().await.calls.len(), before + 1);
    }
}

#[tokio::test]
async fn success_requires_a_valid_bounded_acknowledgement() {
    let h = Harness::new().await;
    for body in [
        "{}".into(),
        "{\"ok\":false}".into(),
        "not-json".into(),
        " ".repeat(1025),
    ] {
        h.mock.lock().await.body = body;
        assert_eq!(
            h.update("site-a", &"k".repeat(43), &"A".repeat(43))
                .await
                .status(),
            502
        );
    }
}

#[tokio::test]
async fn upstream_timeout_is_reported_and_health_does_not_contact_server() {
    let h = Harness::new().await;
    let health = h
        .app
        .clone()
        .oneshot(
            HttpRequest::builder()
                .uri("/healthz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(health.status(), 200);
    assert!(h.mock.lock().await.calls.is_empty());
    h.mock.lock().await.delay = Duration::from_secs(1);
    assert_eq!(
        h.update("site-a", &"k".repeat(43), &"A".repeat(43))
            .await
            .status(),
        504
    );
    assert_eq!(h.mock.lock().await.calls.len(), 1);
}
