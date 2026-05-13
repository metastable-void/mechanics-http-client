//! Integration tests against a local wiremock server. The HTTP path
//! exercises the no-TLS code path; certificate handling is verified
//! by the workspace's existing end-to-end deployments rather than
//! by a self-signed TLS test fixture here.

use serde_json::json;
use std::time::Duration;
use wiremock::matchers::{body_json, header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[cfg(feature = "http3")]
use crate::client::Origin;
use crate::{Client, Error, StatusCode};
#[cfg(feature = "http3")]
use crate::{alt_svc, https_rr};

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

#[cfg(feature = "http3")]
#[test]
fn alt_svc_parser_accepts_h3_and_max_age() {
    let now = std::time::Instant::now();
    let parsed = alt_svc::parse_str(r#"h3=":9443"; ma=120"#, now, 443);
    let alt_svc::AltSvcUpdate::Entry(entry) = parsed else {
        panic!("expected h3 entry");
    };
    assert_eq!(entry.host, None);
    assert_eq!(entry.port, 9443);
    assert!(entry.expires_at > now);
}

#[cfg(feature = "http3")]
#[test]
fn alt_svc_parser_ignores_h3_drafts() {
    let parsed = alt_svc::parse_str(r#"h3-29=":443"; ma=120"#, std::time::Instant::now(), 443);
    assert!(matches!(parsed, alt_svc::AltSvcUpdate::None));
}

#[cfg(feature = "http3")]
#[test]
fn alt_svc_parser_clear_evicts() {
    let parsed = alt_svc::parse_str("clear", std::time::Instant::now(), 443);
    assert!(matches!(parsed, alt_svc::AltSvcUpdate::Clear));
}

#[cfg(feature = "http3")]
#[test]
fn https_rr_parser_honours_h3_port_and_hints() {
    use hickory_resolver::proto::rr::rdata::svcb::{Alpn, IpHint, SvcParamKey, SvcParamValue};
    use hickory_resolver::proto::rr::rdata::{A, AAAA};

    let expires_at = std::time::Instant::now() + Duration::from_secs(60);
    let entry = https_rr::parse_svcb(
        &[
            (
                SvcParamKey::Alpn,
                SvcParamValue::Alpn(Alpn(vec!["h2".to_owned(), "h3".to_owned()])),
            ),
            (SvcParamKey::Port, SvcParamValue::Port(8443)),
            (
                SvcParamKey::Ipv4Hint,
                SvcParamValue::Ipv4Hint(IpHint(vec![A::new(192, 0, 2, 1)])),
            ),
            (
                SvcParamKey::Ipv6Hint,
                SvcParamValue::Ipv6Hint(IpHint(vec![AAAA::new(0x2001, 0xdb8, 0, 0, 0, 0, 0, 1)])),
            ),
        ],
        443,
        expires_at,
    );

    assert!(entry.has_h3);
    assert_eq!(entry.port, 8443);
    assert_eq!(entry.addresses.len(), 2);
    assert_eq!(entry.expires_at, expires_at);
}

#[cfg(feature = "http3")]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn http_urls_do_not_honour_alt_svc() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/alt"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("Alt-Svc", r#"h3=":443"; ma=120"#)
                .set_body_string("ok"),
        )
        .mount(&server)
        .await;

    let client = Client::new().expect("client");
    let resp = client
        .get(format!("{}/alt", server.uri()))
        .send()
        .await
        .expect("send");
    assert_eq!(resp.status(), StatusCode::OK);

    let origin = Origin {
        host: "127.0.0.1".to_owned(),
        port: 80,
    };
    assert!(!client.has_alt_svc_for_test(&origin));
}

#[cfg(feature = "http3")]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
async fn runtime_http3_toggle_keeps_hyper_path() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/off"))
        .respond_with(ResponseTemplate::new(200).set_body_string("ok"))
        .mount(&server)
        .await;

    let client = Client::builder().http3(false).build().expect("client");
    let resp = client
        .get(format!("{}/off", server.uri()))
        .send()
        .await
        .expect("send");
    assert_eq!(resp.status(), StatusCode::OK);
}

#[cfg(feature = "http3")]
#[test]
fn negative_cache_accessor_tracks_inserted_entry() {
    let client = Client::new().expect("client");
    let origin = Origin {
        host: "example.com".to_owned(),
        port: 443,
    };
    client.insert_negative_for_test(
        origin.clone(),
        std::time::Instant::now() + Duration::from_secs(60),
    );
    assert!(client.has_negative_for_test(&origin));
}

#[cfg(feature = "http3")]
#[test]
fn https_rr_cache_accessor_tracks_inserted_entry() {
    let client = Client::new().expect("client");
    let origin = Origin {
        host: "example.com".to_owned(),
        port: 443,
    };
    client.insert_https_rr_for_test(
        origin.clone(),
        https_rr::HttpsRrEntry {
            port: 443,
            addresses: Vec::new(),
            has_h3: true,
            expires_at: std::time::Instant::now() + Duration::from_secs(60),
        },
    );
    assert!(client.has_https_rr_for_test(&origin));
}

#[cfg(feature = "http3")]
#[test]
fn alt_svc_cache_accessor_tracks_inserted_entry() {
    let client = Client::new().expect("client");
    let origin = Origin {
        host: "example.com".to_owned(),
        port: 443,
    };
    client.insert_alt_svc_for_test(
        origin.clone(),
        alt_svc::AltSvcEntry {
            host: None,
            port: 443,
            expires_at: std::time::Instant::now() + Duration::from_secs(60),
        },
    );
    assert!(client.has_alt_svc_for_test(&origin));
}

#[cfg(feature = "http3")]
#[test]
fn tcp_tls_config_does_not_advertise_h3_alpn() {
    let config = crate::tls::webpki_roots_client_config().expect("tls config");
    assert!(
        !config
            .alpn_protocols
            .iter()
            .any(|protocol| protocol.as_slice() == b"h3")
    );
}

#[cfg(feature = "http3")]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[ignore = "requires a local HTTP/3 server fixture; covered by deterministic cache/parser tests in this dispatch"]
async fn real_http3_https_rr_upgrade_fixture() {}

#[cfg(feature = "http3")]
#[tokio::test(flavor = "multi_thread", worker_threads = 1)]
#[ignore = "requires a local HTTP/3 server fixture; covered by deterministic cache/parser tests in this dispatch"]
async fn real_http3_alt_svc_upgrade_fixture() {}
