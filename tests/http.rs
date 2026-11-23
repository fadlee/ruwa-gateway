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
    storage::{
        Db,
        registry::{DeviceRecord, Registry},
    },
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
    let registry = Registry::new(Db::open(&dir.path().join("ruwa.db").to_string_lossy()).unwrap());
    for (i, slot) in slots.iter().enumerate() {
        registry
            .insert(DeviceRecord::new(slot, slot, i as i64))
            .await
            .unwrap();
    }
    let qr_dir = dir.path().join("statics/qrcode");
    let devices = DeviceManager::bootstrap(
        DeviceOptions {
            root,
            qr_dir: qr_dir.clone(),
            os: "Chrome".into(),
            media_dir: dir.path().join("statics/media"),
            auto_download_media: true,
        },
        registry,
        Dispatcher::new(WebhookConfig::default()).unwrap(),
    )
    .await
    .unwrap();
    std::fs::write(qr_dir.join("probe.png"), b"png").unwrap();
    let state = AppState {
        ui: ruwa_gateway::ui::UiAsset::new(ruwa_gateway::ui::UiConfig {
            enabled: true,
            auto_update: false,
            repo: "aldinokemal/gowa-ui".into(),
            asset_name: "gowa-ui.html".into(),
            cache_dir: dir.path().join("storages/ui"),
        }),
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

fn req(method: &str, uri: &str, body: &str) -> Request<Body> {
    Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .body(Body::from(body.to_owned()))
        .unwrap()
}

#[tokio::test]
async fn devices_crud() {
    let h = harness().await;
    let (_, body) = call(&h.app, get("/devices")).await;
    assert_eq!(body["message"], "List devices");
    assert_eq!(body["results"][0]["id"], "default");
    assert_eq!(body["results"][0]["state"], "disconnected");
    assert!(body["results"][0].get("jid").is_none(), "jid is omitempty");

    let (status, body) = call(&h.app, req("POST", "/devices", r#"{"device_id":"sales"}"#)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["message"], "Device added");
    assert_eq!(body["results"]["id"], "sales");
    assert!(body["results"].get("webhook_url").is_none());

    let (status, body) = call(&h.app, req("POST", "/devices", r#"{"device_id":"sales"}"#)).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body["message"], "device sales already exists");

    let (_, body) = call(&h.app, req("POST", "/devices", "{}")).await;
    let generated = body["results"]["id"].as_str().unwrap().to_owned();
    assert_eq!(generated.len(), 36, "uuid id");

    let (_, body) = call(&h.app, get("/devices/sales")).await;
    assert_eq!(body["message"], "Device info");
    assert_eq!(body["results"]["id"], "sales");

    let (_, body) = call(&h.app, get("/devices/sales/status")).await;
    assert_eq!(
        body["results"],
        serde_json::json!({"device_id":"sales","is_connected":false,"is_logged_in":false})
    );

    // With three slots, device-scoped routes need an id.
    assert_eq!(
        call(&h.app, get("/app/status")).await.0,
        StatusCode::BAD_REQUEST
    );

    let (status, body) = call(&h.app, req("DELETE", "/devices/sales", "")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["message"], "Device removed");
    assert!(body.get("results").is_none());

    let (status, body) = call(&h.app, req("DELETE", "/devices/sales", "")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(body["message"], "device not found");

    let (status, body) = call(&h.app, get("/devices/sales")).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body["message"], "device sales not found");
}

#[tokio::test]
async fn devices_add_with_webhook_and_bad_body() {
    let h = harness().await;
    let (_, body) = call(
        &h.app,
        req(
            "POST",
            "/devices",
            r#"{"device_id":"a","webhook_url":"https://x","webhook_events":"message"}"#,
        ),
    )
    .await;
    assert_eq!(body["results"]["webhook_url"], "https://x");
    assert_eq!(body["results"]["webhook_secret"], "");
    assert_eq!(body["results"]["webhook_insecure_skip_verify"], false);

    let (status, body) = call(&h.app, req("POST", "/devices", "{nope")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "BAD_REQUEST");
    assert_eq!(body["message"], "Invalid request body");
}

#[tokio::test]
async fn device_webhook_roundtrip() {
    let h = harness().await;
    let (status, body) = call(&h.app, req("PATCH", "/devices/default/webhook", "{}")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["message"], "webhook_url is required");

    let (status, body) = call(
        &h.app,
        req(
            "PATCH",
            "/devices/default/webhook",
            r#"{"webhook_url":"https://h","webhook_secret":"s"}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["message"], "Device webhook updated");

    let (_, body) = call(&h.app, get("/devices/default/webhook")).await;
    assert_eq!(body["message"], "Device webhook retrieved");
    assert_eq!(body["results"]["webhook_url"], "https://h");
    assert_eq!(body["results"]["webhook_secret"], "s");
    assert_eq!(body["results"]["webhook_events"], "");

    let (status, _) = call(&h.app, get("/devices/missing/webhook")).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn device_ids_with_path_characters_are_contained() {
    let h = harness().await;
    let (status, body) = call(
        &h.app,
        req("POST", "/devices", r#"{"device_id":"../escape"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["results"]["id"], "../escape");
    let (status, _) = call(&h.app, get("/devices/..%2Fescape")).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn device_reconnect_and_logout_messages() {
    let h = harness().await;
    let (status, body) = call(&h.app, req("POST", "/devices/default/reconnect", "")).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        body["message"],
        "device default is not logged in (session deleted)"
    );
    let (status, body) = call(&h.app, req("POST", "/devices/default/logout", "")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["message"], "Logout requested");
}

fn multipart(
    uri: &str,
    fields: &[(&str, &str)],
    file: Option<(&str, &str, &str, &[u8])>,
) -> Request<Body> {
    let boundary = "XBOUNDARYX";
    let mut body = Vec::new();
    for (k, v) in fields {
        body.extend_from_slice(
            format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{k}\"\r\n\r\n{v}\r\n")
                .as_bytes(),
        );
    }
    if let Some((name, filename, ctype, bytes)) = file {
        body.extend_from_slice(
            format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"; filename=\"{filename}\"\r\nContent-Type: {ctype}\r\n\r\n").as_bytes(),
        );
        body.extend_from_slice(bytes);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    Request::post(uri)
        .header(
            "content-type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(Body::from(body))
        .unwrap()
}

async fn expect_validation(h: &Harness, req: Request<Body>, message: &str) {
    let (status, body) = call(&h.app, req).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["code"], "VALIDATION_ERROR");
    assert_eq!(body["message"], message);
}

#[tokio::test]
async fn send_media_validation() {
    let h = harness().await;
    expect_validation(
        &h,
        multipart("/send/image", &[("phone", "628")], None),
        "either Image or ImageURL must be provided",
    )
    .await;
    expect_validation(
        &h,
        multipart(
            "/send/image",
            &[("phone", "628")],
            Some(("image", "a.gif", "image/gif", b"GIF89a")),
        ),
        "your image is not allowed. please use jpg/jpeg/png",
    )
    .await;
    expect_validation(
        &h,
        post_json("/send/image", r#"{"phone":"628","image_url":"nope"}"#),
        "ImageURL must be a valid URL",
    )
    .await;
    expect_validation(
        &h,
        post_json("/send/file", r#"{"phone":"628"}"#),
        "either File or FileURL must be provided",
    )
    .await;
    expect_validation(
        &h,
        multipart(
            "/send/video",
            &[("phone", "628")],
            Some(("video", "a.webm", "video/webm", b"x")),
        ),
        "your video type is not allowed. please use mp4/mkv/avi/x-msvideo",
    )
    .await;
    expect_validation(
        &h,
        post_json("/send/audio", r#"{"phone":"628"}"#),
        "either Audio or AudioURL must be provided",
    )
    .await;
    expect_validation(
        &h,
        post_json("/send/contact", r#"{"phone":"628"}"#),
        "contact_name: cannot be blank; contact_phone: cannot be blank.",
    )
    .await;
    expect_validation(&h, post_json("/send/contact", r#"{"phone":"628","contact_name":"A","contact_phone":"0812"}"#),
        "contact phone number must be in international format (should not start with 0). For Indonesian numbers, use 62xxx format instead of 08xxx").await;
    expect_validation(
        &h,
        post_json(
            "/send/location",
            r#"{"phone":"628","latitude":"200","longitude":"1"}"#,
        ),
        "latitude: must be a valid latitude.",
    )
    .await;
    expect_validation(
        &h,
        post_json("/send/link", r#"{"phone":"628","link":"x","caption":"c"}"#),
        "link: must be a valid URL.",
    )
    .await;
    expect_validation(
        &h,
        multipart(
            "/send/image",
            &[("phone", "628"), ("duration", "5")],
            Some(("image", "a.png", "image/png", b"x")),
        ),
        "duration must be one of: 0 (no expiry), 86400 (24h), 604800 (7d), 7776000 (90d)",
    )
    .await;
}

#[tokio::test]
async fn valid_media_send_needs_a_session() {
    let h = harness().await;
    let req = multipart(
        "/send/image",
        &[("phone", "628123"), ("caption", "hi")],
        Some(("image", "a.png", "image/png", b"x")),
    );
    let (status, body) = call(&h.app, req).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], "AUTHENTICATION_ERROR");
    let (status, _) = call(
        &h.app,
        post_json(
            "/send/location",
            r#"{"phone":"628","latitude":"-6.2","longitude":"106.8"}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn user_endpoints_require_login() {
    let h = harness().await;
    let (status, body) = call(&h.app, get("/user/check?phone=628123")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], "AUTHENTICATION_ERROR");
    let (status, body) = call(&h.app, get("/user/info")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["message"], "phone: cannot be blank.");
}

#[tokio::test]
async fn dashboard_falls_back_when_not_downloaded() {
    let h = harness().await;
    let (status, body) = raw(&h.app, get("/")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("not available yet"));
}
