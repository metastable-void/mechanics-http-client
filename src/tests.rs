//! Integration tests against a local wiremock server. The HTTP path
//! exercises the no-TLS code path; certificate handling is verified
//! by the workspace's existing end-to-end deployments rather than
//! by a self-signed TLS test fixture here.

use serde_json::json;
use std::time::Duration;
use wiremock::matchers::{body_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::{Client, Error, StatusCode};

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn get_returns_status_and_text() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/hello"))
        .respond_with(ResponseTemplate::new(200).set_body_string("hello world"))
        .mount(&server)
        .await;

    let client = Client::new().expect("client");
    let resp = client
        .get(format!("{}/hello", server.uri()))
        .send()
        .await
        .expect("send");

    assert_eq!(resp.status(), StatusCode::OK);
    let body = resp.text().await.expect("text");
    assert_eq!(body, "hello world");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn post_json_round_trips_with_bearer_auth_and_custom_header() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/echo"))
        .and(header("Authorization", "Bearer t0ken"))
        .and(header("X-Custom", "v"))
        .and(body_json(json!({"q": 1})))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_json(json!({"ok": true})),
        )
        .mount(&server)
        .await;

    let client = Client::new().expect("client");
    let resp = client
        .post(format!("{}/echo", server.uri()))
        .bearer_auth("t0ken")
        .header("X-Custom", "v")
        .json(&json!({"q": 1}))
        .send()
        .await
        .expect("send");

    assert_eq!(resp.status(), StatusCode::OK);
    let value: serde_json::Value = resp.json().await.expect("json");
    assert_eq!(value, json!({"ok": true}));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn timeout_fires_when_server_is_slow() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/slow"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_delay(Duration::from_millis(500))
                .set_body_string(""),
        )
        .mount(&server)
        .await;

    let client = Client::builder()
        .timeout(Duration::from_millis(50))
        .build()
        .expect("client");
    let result = client.get(format!("{}/slow", server.uri())).send().await;

    match result {
        Err(Error::Timeout) => {}
        other => panic!("expected Timeout, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn bytes_with_cap_rejects_oversize_body() {
    let server = MockServer::start().await;
    let big_body = "x".repeat(1024);
    Mock::given(method("GET"))
        .and(path("/big"))
        .respond_with(ResponseTemplate::new(200).set_body_string(big_body))
        .mount(&server)
        .await;

    let client = Client::new().expect("client");
    let resp = client
        .get(format!("{}/big", server.uri()))
        .send()
        .await
        .expect("send");

    match resp.bytes_with_cap(128).await {
        Err(Error::BodyTooLarge { limit: 128, .. }) => {}
        other => panic!("expected BodyTooLarge, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn http_scheme_only_url_is_rejected() {
    let client = Client::new().expect("client");
    let err = client
        .get("ftp://example.com/")
        .send()
        .await
        .expect_err("must reject non-http scheme");
    assert!(matches!(err, Error::InvalidUrl(_)), "got {err:?}");
}
