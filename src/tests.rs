use super::*;
use axum::{
    body::Body,
    extract::State,
    http::{Method, Request as HttpRequest, Uri},
    routing::any,
};
use serde_json::Value;
use tokio::task::JoinHandle;
use tower::ServiceExt;

const ZONE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const SITE_A: &str = "_acme-challenge.site-a.example.com";
const SITE_B: &str = "_acme-challenge.site-b.example.com";

fn config_json() -> Value {
    json!({"zoneId":ZONE, "zoneName":"example.com", "clients":[
        {"id":"site-a", "recordName":SITE_A, "tokenSha256":[format!("{:x}", Sha256::digest("k".repeat(43)))]},
        {"id":"site-b", "recordName":SITE_B, "tokenSha256":[format!("{:x}", Sha256::digest("y".repeat(43)))]}
    ]})
}

fn config() -> Config {
    Config::parse(&config_json().to_string()).unwrap()
}

#[derive(Default)]
struct MockDns {
    records: Vec<Value>,
    calls: Vec<(Method, String, Value)>,
    next_id: u32,
    page_size: usize,
    fail: Option<StatusCode>,
    malformed: bool,
    fail_after_create: bool,
    bad_write_response: bool,
}

struct Harness {
    app: Router,
    dns: Arc<Mutex<MockDns>>,
    server: JoinHandle<()>,
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.server.abort();
    }
}

impl Harness {
    async fn new() -> Self {
        let dns = Arc::new(Mutex::new(MockDns {
            page_size: 100,
            ..Default::default()
        }));
        let mock = Router::new()
            .fallback(any(mock_dns))
            .with_state(dns.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!(
            "http://{}/zones/{ZONE}/dns_records",
            listener.local_addr().unwrap()
        );
        let server = tokio::spawn(async move { axum::serve(listener, mock).await.unwrap() });
        let app = app(
            config(),
            Dns::with_base(base, "synthetic-dns-secret".into()).unwrap(),
        );
        Self { app, dns, server }
    }

    async fn call(&self, value: &str, token: &str) -> Response {
        self.request(
            "POST",
            "/v1/challenges",
            Some(token),
            "application/json",
            json!({"value":value}).to_string(),
        )
        .await
    }

    async fn request(
        &self,
        method: &str,
        uri: &str,
        token: Option<&str>,
        content_type: &str,
        body: String,
    ) -> Response {
        let mut request = HttpRequest::builder()
            .method(method)
            .uri(uri)
            .header(header::CONTENT_TYPE, content_type);
        if let Some(token) = token {
            request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        self.app
            .clone()
            .oneshot(request.body(Body::from(body)).unwrap())
            .await
            .unwrap()
    }
}

fn record(id: u32, name: &str, value: &str) -> Value {
    json!({"id":format!("{id:032x}"), "type":"TXT", "name":name, "content":value})
}

async fn mock_dns(
    State(state): State<Arc<Mutex<MockDns>>>,
    method: Method,
    uri: Uri,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> (StatusCode, axum::Json<Value>) {
    assert_eq!(
        headers.get(header::AUTHORIZATION).unwrap(),
        "Bearer synthetic-dns-secret"
    );
    let mut mock = state.lock().await;
    let body: Value = if body.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&body).unwrap()
    };
    mock.calls
        .push((method.clone(), uri.to_string(), body.clone()));
    if let Some(status) = mock.fail {
        return (
            status,
            axum::Json(json!({"error":"private upstream response synthetic-dns-secret"})),
        );
    }
    if mock.malformed {
        return (
            StatusCode::OK,
            axum::Json(json!({"success":true, "result":null})),
        );
    }
    if method == Method::GET {
        let query = uri.query().unwrap();
        assert!(query.contains("type=TXT"));
        assert!(query.contains("name.exact="));
        let page = query
            .split('&')
            .find_map(|s| s.strip_prefix("page="))
            .unwrap()
            .parse::<usize>()
            .unwrap();
        let records = mock
            .records
            .iter()
            .skip((page - 1) * mock.page_size)
            .take(mock.page_size)
            .cloned()
            .collect::<Vec<_>>();
        // Intentionally ignore the filters: the service must check each result itself.
        return (
            StatusCode::OK,
            axum::Json(json!({"success":true, "result":records,
            "result_info":{"page":page, "total_pages":mock.records.len().div_ceil(mock.page_size)}})),
        );
    }
    assert_eq!(body["type"], "TXT");
    let result = if method == Method::POST {
        mock.next_id += 1;
        let value = record(
            mock.next_id,
            body["name"].as_str().unwrap(),
            body["content"].as_str().unwrap(),
        );
        mock.records.push(value.clone());
        if mock.fail_after_create {
            return (
                StatusCode::BAD_GATEWAY,
                axum::Json(json!({"success":false})),
            );
        }
        value
    } else {
        assert_eq!(method, Method::PUT);
        let id = uri.path().rsplit('/').next().unwrap();
        let record = mock.records.iter_mut().find(|r| r["id"] == id).unwrap();
        record["content"] = body["content"].clone();
        record.clone()
    };
    if mock.bad_write_response {
        return (
            StatusCode::OK,
            axum::Json(json!({"success":true, "result":record(99, SITE_B, "incorrect")})),
        );
    }
    (
        StatusCode::OK,
        axum::Json(json!({"success":true, "result":result})),
    )
}

#[tokio::test]
async fn creates_updates_and_skips_identical_values() {
    let h = Harness::new().await;
    let token = "k".repeat(43);
    assert_eq!(h.call(&"A".repeat(43), &token).await.status(), 200);
    assert_eq!(h.call(&"B".repeat(43), &token).await.status(), 200);
    assert_eq!(h.call(&"B".repeat(43), &token).await.status(), 200);
    let mock = h.dns.lock().await;
    assert_eq!(mock.records.len(), 1);
    assert_eq!(mock.records[0]["content"], "B".repeat(43));
    assert_eq!(
        mock.calls.iter().map(|c| c.0.as_str()).collect::<Vec<_>>(),
        ["GET", "POST", "GET", "PUT", "GET"]
    );
    assert_eq!(mock.calls[1].2["name"], SITE_A);
}

#[tokio::test]
async fn concurrent_requests_do_not_create_duplicates() {
    let h = Harness::new().await;
    let (value, token) = ("A".repeat(43), "k".repeat(43));
    let (a, b, c) = tokio::join!(
        h.call(&value, &token),
        h.call(&value, &token),
        h.call(&value, &token)
    );
    assert_eq!([a.status(), b.status(), c.status()], [StatusCode::OK; 3]);
    let mock = h.dns.lock().await;
    assert_eq!(mock.records.len(), 1);
    assert_eq!(mock.calls.iter().filter(|c| c.0 == Method::POST).count(), 1);
}

#[tokio::test]
async fn later_value_replaces_previous_value() {
    let h = Harness::new().await;
    assert_eq!(h.call(&"A".repeat(43), &"k".repeat(43)).await.status(), 200);
    assert_eq!(h.call(&"B".repeat(43), &"k".repeat(43)).await.status(), 200);
    assert_eq!(
        h.dns.lock().await.records,
        [record(1, SITE_A, &"B".repeat(43))]
    );
}

#[tokio::test]
async fn enforces_cross_client_isolation_even_if_upstream_filters_are_wrong() {
    let h = Harness::new().await;
    h.dns.lock().await.records = vec![record(42, SITE_B, &"A".repeat(43))];
    let mut wrong_type = record(43, SITE_A, "1.2.3.4");
    wrong_type["type"] = json!("A");
    h.dns.lock().await.records.push(wrong_type.clone());
    assert_eq!(h.call(&"B".repeat(43), &"k".repeat(43)).await.status(), 200);
    assert_eq!(h.call(&"C".repeat(43), &"k".repeat(43)).await.status(), 200);
    let mock = h.dns.lock().await;
    assert_eq!(mock.records[0], record(42, SITE_B, &"A".repeat(43)));
    assert_eq!(mock.records[1], wrong_type);
    assert_eq!(mock.records[2]["name"], SITE_A);
    assert_eq!(mock.records[2]["content"], "C".repeat(43));
}

#[tokio::test]
async fn unknown_and_missing_tokens_cannot_call_dns() {
    let h = Harness::new().await;
    for token in [
        None,
        Some("invalid"),
        Some("zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz"),
    ] {
        let response = h
            .request(
                "POST",
                "/v1/challenges",
                token,
                "application/json",
                "{}".into(),
            )
            .await;
        assert_eq!(response.status(), 401);
        assert_eq!(response.headers()[header::WWW_AUTHENTICATE], "Bearer");
    }
    assert!(h.dns.lock().await.calls.is_empty());
}

#[tokio::test]
async fn rejects_caller_supplied_targets_ids_and_invalid_values() {
    let h = Harness::new().await;
    let token = "k".repeat(43);
    for field in [
        "name",
        "recordName",
        "zoneId",
        "recordId",
        "id",
        "type",
        "url",
    ] {
        let mut body = json!({"value":"A".repeat(43)});
        body[field] = json!(SITE_B);
        assert_eq!(
            h.request(
                "POST",
                "/v1/challenges",
                Some(&token),
                "application/json",
                body.to_string()
            )
            .await
            .status(),
            400
        );
    }
    for body in [
        "{}",
        "[]",
        "null",
        "{",
        "{\"value\":null}",
        "{\"value\":\"short\"}",
    ] {
        assert_eq!(
            h.request(
                "POST",
                "/v1/challenges",
                Some(&token),
                "application/json",
                body.into()
            )
            .await
            .status(),
            400
        );
    }
    assert_eq!(h.call(&"!".repeat(43), &token).await.status(), 400);
    assert!(h.dns.lock().await.calls.is_empty());
}

#[tokio::test]
async fn body_size_content_type_and_routes_are_restricted() {
    let h = Harness::new().await;
    let token = "k".repeat(43);
    for (method, path, content_type, body, status) in [
        (
            "POST",
            "/v1/challenges",
            "application/json",
            " ".repeat(513),
            413,
        ),
        ("POST", "/v1/challenges", "text/plain", "{}".into(), 415),
        (
            "POST",
            "/v1/challenges?record=other",
            "application/json",
            "{}".into(),
            400,
        ),
        (
            "POST",
            "/v1/challenges/cleanup",
            "application/json",
            "{}".into(),
            404,
        ),
        ("GET", "/v1/challenges", "application/json", "".into(), 405),
        ("GET", "/healthz", "application/json", "".into(), 200),
    ] {
        assert_eq!(
            h.request(method, path, Some(&token), content_type, body)
                .await
                .status(),
            status
        );
    }
    assert!(h.dns.lock().await.calls.is_empty());
}

#[tokio::test]
async fn pagination_finds_the_exact_record_to_update() {
    let h = Harness::new().await;
    {
        let mut mock = h.dns.lock().await;
        mock.page_size = 1;
        mock.records = vec![record(1, SITE_B, "other"), record(2, SITE_A, "old")];
    }
    assert_eq!(h.call(&"A".repeat(43), &"k".repeat(43)).await.status(), 200);
    let mock = h.dns.lock().await;
    assert_eq!(mock.calls.len(), 3);
    assert!(
        mock.calls[2]
            .1
            .ends_with("/00000000000000000000000000000002")
    );
    assert_eq!(mock.records[0]["content"], "other");
}

#[tokio::test]
async fn duplicate_existing_records_fail_without_mutation_or_deletion() {
    let h = Harness::new().await;
    h.dns.lock().await.records = vec![record(1, SITE_A, "old"), record(2, SITE_A, "also old")];
    assert_eq!(h.call(&"A".repeat(43), &"k".repeat(43)).await.status(), 409);
    let mock = h.dns.lock().await;
    assert_eq!(mock.calls.len(), 1);
    assert_eq!(mock.records[0]["content"], "old");
}

#[tokio::test]
async fn identical_quoted_txt_is_already_satisfied() {
    let h = Harness::new().await;
    h.dns.lock().await.records = vec![record(1, SITE_A, &format!("\"{}\"", "A".repeat(43)))];
    assert_eq!(h.call(&"A".repeat(43), &"k".repeat(43)).await.status(), 200);
    assert_eq!(h.dns.lock().await.calls.len(), 1);
}

#[tokio::test]
async fn upstream_errors_fail_once_and_do_not_leak_response_text() {
    let h = Harness::new().await;
    for status in [
        StatusCode::FORBIDDEN,
        StatusCode::TOO_MANY_REQUESTS,
        StatusCode::INTERNAL_SERVER_ERROR,
    ] {
        h.dns.lock().await.fail = Some(status);
        let before = h.dns.lock().await.calls.len();
        let response = h.call(&"A".repeat(43), &"k".repeat(43)).await;
        assert_eq!(response.status(), 502);
        let body = to_bytes(response.into_body(), 1024).await.unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&body).unwrap(),
            json!({"error":"dns_api_error"})
        );
        assert_eq!(h.dns.lock().await.calls.len(), before + 1);
    }
}

#[tokio::test]
async fn malformed_upstream_and_write_acknowledgements_fail() {
    let h = Harness::new().await;
    h.dns.lock().await.malformed = true;
    assert_eq!(h.call(&"A".repeat(43), &"k".repeat(43)).await.status(), 502);
    h.dns.lock().await.malformed = false;
    h.dns.lock().await.bad_write_response = true;
    assert_eq!(h.call(&"A".repeat(43), &"k".repeat(43)).await.status(), 502);
}

#[tokio::test]
async fn failed_write_returns_error_and_next_request_queries_current_dns() {
    let h = Harness::new().await;
    h.dns.lock().await.fail_after_create = true;
    assert_eq!(h.call(&"A".repeat(43), &"k".repeat(43)).await.status(), 502);
    assert_eq!(h.dns.lock().await.calls.len(), 2);
    assert_eq!(h.call(&"A".repeat(43), &"k".repeat(43)).await.status(), 200);
    let mock = h.dns.lock().await;
    assert_eq!(mock.calls.len(), 3);
    assert_eq!(mock.records.len(), 1);
}

#[test]
fn configuration_rejects_cross_zone_names_and_duplicates() {
    for name in [
        "_acme-challenge.evil.test",
        "_acme-challenge.site-a.example.com.evil.test",
        "site-a.example.com",
        "_acme-challenge.*.example.com",
        "_acme-challenge.Site-a.example.com",
    ] {
        let mut data = config_json();
        data["clients"][0]["recordName"] = json!(name);
        assert!(Config::parse(&data.to_string()).is_err());
    }
    for key in ["id", "recordName", "tokenSha256"] {
        let mut data = config_json();
        data["clients"][1][key] = data["clients"][0][key].clone();
        assert!(Config::parse(&data.to_string()).is_err());
    }
    let mut data = config_json();
    data["arbitrary"] = json!("field");
    assert!(Config::parse(&data.to_string()).is_err());
    assert!(Config::parse("{private marker").is_err());
}

#[tokio::test]
async fn rotation_accepts_both_keys_and_revocation_rejects_old_key() {
    let mut h = Harness::new().await;
    let mut data = config_json();
    data["clients"][0]["tokenSha256"] = json!([
        format!("{:x}", Sha256::digest("k".repeat(43))),
        format!("{:x}", Sha256::digest("y".repeat(43)))
    ]);
    data["clients"].as_array_mut().unwrap().pop();
    // Authentication alone: unavailable upstream proves accepted keys reach DNS.
    let make = |data: &Value| {
        app(
            Config::parse(&data.to_string()).unwrap(),
            Dns::with_base("http://127.0.0.1:1".into(), "secret".into()).unwrap(),
        )
    };
    h.app = make(&data);
    assert_eq!(h.call(&"A".repeat(43), &"k".repeat(43)).await.status(), 502);
    assert_eq!(h.call(&"A".repeat(43), &"y".repeat(43)).await.status(), 502);
    data["clients"][0]["tokenSha256"]
        .as_array_mut()
        .unwrap()
        .remove(0);
    h.app = make(&data);
    assert_eq!(h.call(&"A".repeat(43), &"k".repeat(43)).await.status(), 401);
}
