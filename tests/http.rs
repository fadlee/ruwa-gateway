//! HTTP contract tests against a real `DeviceManager` with no paired
//! sessions (nothing here touches the network).

use std::sync::Arc;

use axum::{
    Router,
    body::Body,
    http::{Request, StatusCode},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use http_body_util::BodyExt;
use ruwa_gateway::{
    config::Credential,
    device::{DeviceManager, DeviceOptions},
    http::{AppState, Settings, router},
    webhook::{Dispatcher, WebhookConfig},
};
use serde_json::Value;
use tempfile::TempDir;
use tower::ServiceExt;

struct Harness {
    app: Router,
    _dir: TempDir,
}

async fn harness_with(creds: Vec<Credential>, base: &str, slots: &[&str]) -> Harness {
    let dir = TempDir::new().unwrap();
    let root = dir.path().join("storages/devices");
    for slot in slots {
        std::fs::create_dir_all(root.join(slot)).unwrap();
    }
    let qr_dir = dir.path().join("statics/qrcode");
    let devices = DeviceManager::bootstrap(
        DeviceOptions {
            root,
            qr_dir: qr_dir.clone(),
            os: "Chrome".into(),
        },
        Dispatcher::new(WebhookConfig::default()).unwrap(),
    )
    .await
    .unwrap();
    std::fs::write(qr_dir.join("probe.png"), b"png").unwrap();
    let state = AppState {
        credentials: Arc::new(creds),
        devices,
        settings: Arc::new(Settings {
            account_validation: true,
            os: "Chrome".into(),
            base_path: base.into(),
            cors_allowed_origins: vec![],
        }),
    };
    Harness {
        app: router(state),
        _dir: dir,
    }
}

async fn harness() -> Harness {
    harness_with(vec![], "", &[]).await
}

fn cred() -> Vec<Credential> {
    vec![Credential {
        user: "u".into(),
        pass: "p".into(),
    }]
}

async fn raw(app: &Router, req: Request<Body>) -> (StatusCode, String) {
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

async fn call(app: &Router, req: Request<Body>) -> (StatusCode, Value) {
    let (status, body) = raw(app, req).await;
    (
        status,
        serde_json::from_str(&body).unwrap_or_else(|_| panic!("not json: {body}")),
    )
}

fn get(uri: &str) -> Request<Body> {
    Request::get(uri).body(Body::empty()).unwrap()
}

fn post_json(uri: &str, body: &str) -> Request<Body> {
    Request::post(uri)
        .header("content-type", "application/json")
        .body(Body::from(body.to_owned()))
        .unwrap()
}

fn authed(mut req: Request<Body>) -> Request<Body> {
    req.headers_mut().insert(
        "authorization",
        format!("Basic {}", STANDARD.encode("u:p")).parse().unwrap(),
    );
    req
}

#[tokio::test]
async fn health_is_plain_ok_at_root_even_with_base_path() {
    let h = harness_with(cred(), "/gowa", &[]).await;
    assert_eq!(
        raw(&h.app, get("/health")).await,
        (StatusCode::OK, "OK".into())
    );
}

#[tokio::test]
async fn unauthorized_is_plain_text_with_challenge() {
    let h = harness_with(cred(), "", &[]).await;
    let res = h.app.clone().oneshot(get("/app/info")).await.unwrap();
    assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    assert!(
        res.headers()["www-authenticate"]
            .to_str()
            .unwrap()
            .starts_with("Basic")
    );
    let body = res.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&body[..], b"Unauthorized");
}

#[tokio::test]
async fn wrong_password_is_rejected() {
    let h = harness_with(cred(), "", &[]).await;
    let req = Request::get("/app/info")
        .header("authorization", format!("Basic {}", STANDARD.encode("u:x")))
        .body(Body::empty())
        .unwrap();
    assert_eq!(raw(&h.app, req).await.0, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn app_info_with_auth() {
    let h = harness_with(cred(), "", &[]).await;
    let (status, body) = call(&h.app, authed(get("/app/info"))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["code"], "SUCCESS");
    assert_eq!(body["message"], "Fetch app info success");
    assert_eq!(body["results"]["os"], "Chrome");
}

#[tokio::test]
async fn statics_need_no_auth_and_honour_base_path() {
    let h = harness_with(cred(), "/gowa", &[]).await;
    let (status, body) = raw(&h.app, get("/gowa/statics/qrcode/probe.png")).await;
    assert_eq!((status, body.as_str()), (StatusCode::OK, "png"));
}

#[tokio::test]
async fn single_default_device_is_implicit() {
    let h = harness().await;
    let (status, body) = call(&h.app, get("/app/status")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["message"], "Connection status retrieved");
    assert_eq!(body["results"]["device_id"], "default");
    assert_eq!(body["results"]["is_connected"], false);
    assert_eq!(body["results"]["is_logged_in"], false);
}

#[tokio::test]
async fn unknown_device_is_404_with_id() {
    let h = harness().await;
    let req = Request::get("/app/status")
        .header("X-Device-Id", "nope")
        .body(Body::empty())
        .unwrap();
    let (status, body) = call(&h.app, req).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["code"], "DEVICE_NOT_FOUND");
    assert_eq!(body["results"]["device_id"], "nope");
}

#[tokio::test]
async fn several_devices_require_an_id() {
    let h = harness_with(vec![], "", &["a", "b"]).await;
    let (status, body) = call(&h.app, get("/app/status")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "DEVICE_ID_REQUIRED");
    let (status, body) = call(&h.app, get("/app/status?device_id=b")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["results"]["device_id"], "b");
}

#[tokio::test]
async fn devices_lists_slots() {
    let h = harness().await;
    let (_, body) = call(&h.app, get("/app/devices")).await;
    assert_eq!(body["message"], "Fetch device success");
    assert_eq!(body["results"][0]["device"], "default");
    assert_eq!(body["results"][0]["jid"], "");
}

#[tokio::test]
async fn send_message_validates_before_login() {
    let h = harness().await;
    let (status, body) = call(&h.app, post_json("/send/message", "{}")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "VALIDATION_ERROR");
    assert_eq!(
        body["message"],
        "message: cannot be blank; phone: cannot be blank."
    );
}

#[tokio::test]
async fn send_message_without_session_is_not_connected() {
    let h = harness().await;
    let (status, body) = call(
        &h.app,
        post_json("/send/message", r#"{"phone":"628123","message":"hi"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], "AUTHENTICATION_ERROR");
    assert_eq!(
        body["message"],
        "you are not connect to services server, please reconnect"
    );
}

#[tokio::test]
async fn send_message_accepts_form_bodies() {
    let h = harness().await;
    let req = Request::post("/send/message")
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Body::from("phone=0812&message=hi"))
        .unwrap();
    let (status, body) = call(&h.app, req).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .contains("international format")
    );
}

#[tokio::test]
async fn reconnect_without_session_fails() {
    let h = harness().await;
    let (status, body) = call(&h.app, get("/app/reconnect")).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        body["message"],
        "device default is not logged in (session deleted)"
    );
}

#[tokio::test]
async fn login_with_code_validates_phone() {
    let h = harness().await;
    let (status, body) = call(&h.app, get("/app/login-with-code?phone=08-12")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        body["message"],
        "phone_number(08-12): must be in a valid format"
    );
}

#[tokio::test]
async fn logout_keeps_slot() {
    let h = harness().await;
    let (status, body) = call(&h.app, get("/app/logout")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["message"], "Success logout");
    let (_, body) = call(&h.app, get("/app/devices")).await;
    assert_eq!(body["results"].as_array().unwrap().len(), 1);
}
