mod common;

use axum::{
    Router,
    http::StatusCode,
    routing::{get, head},
};
use common::FakeTransport;
use nexthome::http::{self, HttpTransport, ReqwestTransport};

#[tokio::test]
async fn get_retrying_returns_the_response_after_transient_failures() {
    let transport = FakeTransport::new();
    transport.push_failed_get("dns lookup failed");
    transport.push_failed_get("connection reset");
    transport.push_ok_get(200, "the body");

    let response = http::get_retrying(
        &transport,
        "https://example.test/things",
        &[("q", "berlin".to_owned())],
    )
    .await
    .unwrap();

    assert_eq!(response.status, 200);
    assert_eq!(response.body, "the body");

    let gets = transport.recorded_gets();
    assert_eq!(gets.len(), 3, "each transport failure must be retried");
    assert_eq!(gets[0].url, "https://example.test/things");
    assert_eq!(gets[0].params(), vec![("q", "berlin")]);
}

#[tokio::test]
async fn get_retrying_gives_up_after_three_transport_failures() {
    let transport = FakeTransport::new();
    for _ in 0..3 {
        transport.push_failed_get("connection refused");
    }

    let error = http::get_retrying(&transport, "https://example.test/things", &[])
        .await
        .expect_err("three transport failures in a row exhaust the retries");
    assert_eq!(error.0, "connection refused");
    assert_eq!(transport.recorded_gets().len(), 3);
    assert!(
        transport
            .recorded_gets()
            .iter()
            .all(|get| get.params.is_empty())
    );
}

#[tokio::test]
async fn reqwest_transport_encodes_params_and_does_not_follow_redirects() {
    // A local stand-in for the provider endpoints: /echo answers a GET with
    // the raw query string it received, /moved answers a HEAD with the 302
    // the shared-link expander needs to read itself.
    let app = Router::new()
        .route(
            "/echo",
            get(|uri: axum::http::Uri| async move { uri.query().unwrap_or_default().to_owned() }),
        )
        .route(
            "/moved",
            head(|| async {
                (
                    StatusCode::FOUND,
                    [("location", "https://example.test/next")],
                )
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let transport = ReqwestTransport::new();

    let response = transport
        .get(
            &format!("http://{address}/echo"),
            &[
                ("input", "Berlin Cosmopolitan School".to_owned()),
                ("origins", "52.52,13.405|48.2,16.37".to_owned()),
                ("key", "test-key".to_owned()),
            ],
        )
        .await
        .unwrap();
    assert_eq!(response.status, 200);
    assert_eq!(
        response.body,
        "input=Berlin%20Cosmopolitan%20School&origins=52.52%2C13.405%7C48.2%2C16.37&key=test-key"
    );

    let response = transport
        .head(&format!("http://{address}/moved"))
        .await
        .unwrap();
    assert_eq!(response.status, 302);
    assert_eq!(
        response.location.as_deref(),
        Some("https://example.test/next")
    );
}
