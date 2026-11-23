//! Envelope and auth behaviour of the HTTP layer.

use std::sync::Arc;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use http_body_util::BodyExt;
use ruwa_gateway::{
    config::Credential,
    http::{AppState, router},
};
use serde_json::Value;
use tower::ServiceExt;

fn app(creds: Vec<Credential>, base: &str) -> axum::Router {
    router(
        AppState {
            credentials: Arc::new(creds),
        },
        base,
    )
}

fn cred() -> Vec<Credential> {
    vec![Credential {
        user: "u".into(),
        pass: "p".into(),
    }]
}

async fn call(app: axum::Router, req: Request<Body>) -> (StatusCode, Value) {
    let res = app.oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, serde_json::from_slice(&bytes).unwrap())
}

fn get(uri: &str) -> Request<Body> {
    Request::get(uri).body(Body::empty()).unwrap()
}

#[tokio::test]
async fn health_is_public_and_uses_envelope() {
    let (status, body) = call(app(cred(), ""), get("/health")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["code"], "SUCCESS");
    assert_eq!(body["results"]["status"], "ok");
}

#[tokio::test]
async fn protected_route_requires_auth() {
    let (status, body) = call(app(cred(), ""), get("/app/info")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["message"], "Unauthorized access");
}

#[tokio::test]
async fn basic_auth_header_is_accepted() {
    let req = Request::get("/app/info")
        .header("Authorization", format!("Basic {}", STANDARD.encode("u:p")))
        .body(Body::empty())
        .unwrap();
    let (status, body) = call(app(cred(), ""), req).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["results"]["name"], "ruwa-gateway");
}

#[tokio::test]
async fn wrong_password_is_rejected() {
    let req = Request::get("/app/info")
        .header("Authorization", format!("Basic {}", STANDARD.encode("u:x")))
        .body(Body::empty())
        .unwrap();
    assert_eq!(call(app(cred(), ""), req).await.0, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn query_authorization_is_accepted() {
    let uri = format!("/app/info?authorization={}", STANDARD.encode("u:p"));
    assert_eq!(call(app(cred(), ""), get(&uri)).await.0, StatusCode::OK);
}

#[tokio::test]
async fn base_path_prefixes_routes() {
    let a = app(vec![], "/gowa");
    assert_eq!(call(a.clone(), get("/gowa/health")).await.0, StatusCode::OK);
    assert_eq!(
        a.oneshot(get("/health")).await.unwrap().status(),
        StatusCode::NOT_FOUND
    );
}
