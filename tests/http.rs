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
    db: Db,
    _dir: TempDir,
}

async fn harness_with(creds: Vec<Credential>, base: &str, slots: &[&str]) -> Harness {
    let dir = TempDir::new().unwrap();
    let root = dir.path().join("storages/devices");
    let db = Db::open(&dir.path().join("ruwa.db").to_string_lossy()).unwrap();
    let registry = Registry::new(db.clone());
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
            auto_reject_call: false,
            auto_mark_read: false,
            auto_reply: String::new(),
            presence: Default::default(),
        },
        db.clone(),
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
        db,
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
async fn user_my_groups_requires_login() {
    let h = harness().await;
    let (status, body) = call(&h.app, get("/user/my/groups")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], "AUTHENTICATION_ERROR");
}

#[tokio::test]
async fn user_profile_endpoints_require_login() {
    let h = harness().await;
    for path in [
        "/user/my/privacy",
        "/user/my/contacts",
        "/user/my/contacts/detailed",
    ] {
        let (status, body) = call(&h.app, get(path)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{path}");
        assert_eq!(body["code"], "AUTHENTICATION_ERROR", "{path}");
    }
    let (status, body) = call(
        &h.app,
        post_json("/user/pushname", r#"{"push_name":"Ruwa"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], "AUTHENTICATION_ERROR");
    // GOWA binds the body first: a broken one is a 500, before the login check.
    let (status, body) = call(&h.app, post_json("/user/pushname", "{not json")).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body["code"], "INTERNAL_SERVER_ERROR");
}

#[tokio::test]
async fn presence_subscribe_validates_then_needs_login() {
    let h = harness().await;
    let (status, body) = call(&h.app, post_json("/user/presence/subscribe", "{}")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "VALIDATION_ERROR");
    assert_eq!(body["message"], "phone: cannot be blank.");
    let (status, body) = call(
        &h.app,
        post_json("/user/presence/subscribe", r#"{"phone":"628123456789"}"#),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], "AUTHENTICATION_ERROR");
    let (status, _) = call(&h.app, post_json("/user/presence/subscribe", "{not json")).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
}

#[tokio::test]
async fn avatar_endpoints_validate_then_need_login() {
    let h = harness().await;
    let (status, body) = call(&h.app, get("/user/avatar")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["message"], "phone: cannot be blank.");
    let (status, body) = call(
        &h.app,
        get("/user/avatar?phone=628123456789&is_preview=true"),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], "AUTHENTICATION_ERROR");
    // Fiber's Bind().Query fails on a malformed boolean: 500.
    let (status, _) = call(&h.app, get("/user/avatar?phone=628&is_preview=yes")).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);

    // POST: no file is a 500 before the login check; a file reaches the login check.
    let (status, _) = call(&h.app, multipart("/user/avatar", &[], None)).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    let (status, body) = call(
        &h.app,
        multipart(
            "/user/avatar",
            &[],
            Some(("avatar", "a.png", "image/png", b"x")),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], "AUTHENTICATION_ERROR");
}

#[tokio::test]
async fn dashboard_falls_back_when_not_downloaded() {
    let h = harness().await;
    let (status, body) = raw(&h.app, get("/")).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("not available yet"));
}

mod websocket {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;

    use super::*;

    async fn serve(h: &Harness) -> std::net::SocketAddr {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let app = h.app.clone();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        addr
    }

    async fn next_json<S>(ws: &mut S) -> Value
    where
        S: futures_util::Stream<Item = Result<Message, tokio_tungstenite::tungstenite::Error>>
            + Unpin,
    {
        loop {
            let msg = tokio::time::timeout(std::time::Duration::from_secs(5), ws.next())
                .await
                .expect("ws message in time")
                .unwrap()
                .unwrap();
            if let Message::Text(t) = msg {
                return serde_json::from_str(&t).unwrap();
            }
        }
    }

    #[tokio::test]
    async fn fetch_devices_and_device_events() {
        let h = harness_with(cred(), "", &[]).await;
        let addr = serve(&h).await;
        let auth = STANDARD.encode("u:p");
        let (mut ws, _) = tokio_tungstenite::connect_async(format!(
            "ws://{addr}/ws?device_id=default&authorization={auth}"
        ))
        .await
        .unwrap();

        ws.send(Message::Text(r#"{"code":"FETCH_DEVICES"}"#.into()))
            .await
            .unwrap();
        let ev = next_json(&mut ws).await;
        assert_eq!(ev["code"], "LIST_DEVICES");
        assert_eq!(ev["message"], "Device found");
        assert_eq!(ev["result"][0]["device"], "default");

        let (status, _) = call(
            &h.app,
            authed(req("POST", "/devices", r#"{"device_id":"b"}"#)),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let (status, _) = call(&h.app, authed(req("DELETE", "/devices/b", ""))).await;
        assert_eq!(status, StatusCode::OK);
        let ev = next_json(&mut ws).await;
        assert_eq!(ev["code"], "DEVICE_REMOVED");
        assert_eq!(ev["result"]["device_id"], "b");
        assert_eq!(ev["result"]["devices"].as_array().unwrap().len(), 1);

        let (status, _) = call(&h.app, authed(get("/app/logout"))).await;
        assert_eq!(status, StatusCode::OK);
        let ev = next_json(&mut ws).await;
        assert_eq!(ev["code"], "DEVICE_LOGGED_OUT");
        assert_eq!(ev["message"], "Device default logged out (slot kept)");
    }

    #[tokio::test]
    async fn requires_auth_and_upgrade() {
        let h = harness_with(cred(), "", &[]).await;
        let addr = serve(&h).await;
        assert!(
            tokio_tungstenite::connect_async(format!("ws://{addr}/ws?device_id=default"))
                .await
                .is_err()
        );
        let auth = STANDARD.encode("u:p");
        let (status, _) = raw(
            &h.app,
            get(&format!("/ws?device_id=default&authorization={auth}")),
        )
        .await;
        assert_eq!(status, StatusCode::UPGRADE_REQUIRED);
    }
}

// ---- /chats, /chat/{jid}/* ----

async fn seed_chats(h: &Harness) {
    use ruwa_gateway::storage::chat::{Chat, ChatStore, ContactName, Message, Reaction};
    let s = ChatStore::new(h.db.clone());
    // Unpaired slot `default` → partition "default".
    let dev = "default";
    let chat = |jid: &str, name: &str, last: i64, archived: bool| Chat {
        jid: jid.into(),
        device_id: dev.into(),
        name: name.into(),
        last_message_time: last,
        ephemeral_expiration: if archived { 86400 } else { 0 },
        archived,
        ..Default::default()
    };
    s.store_chat(chat(
        "628222@s.whatsapp.net",
        "628222",
        1_700_000_200,
        false,
    ))
    .await
    .unwrap();
    s.store_chat(chat("1203@g.us", "", 1_700_000_100, true))
        .await
        .unwrap();
    s.store_chat(chat("628333@s.whatsapp.net", "Carol", 0, false))
        .await
        .unwrap();
    s.set_contact_name(
        dev,
        "628222@s.whatsapp.net",
        ContactName::Saved {
            first: "Bob".into(),
            full: "Bob Smith".into(),
        },
    )
    .await
    .unwrap();
    let msg = |id: &str, ts: i64, from_me: bool, content: &str| Message {
        id: id.into(),
        chat_jid: "628222@s.whatsapp.net".into(),
        device_id: dev.into(),
        sender: if from_me {
            "628111@s.whatsapp.net".into()
        } else {
            "628222@s.whatsapp.net".into()
        },
        content: content.into(),
        timestamp: ts,
        is_from_me: from_me,
        ..Default::default()
    };
    let mut reply = msg("M2", 1_700_000_200, true, "reply");
    reply.context_metadata = r#"{"replied_to_id":"M1"}"#.into();
    let mut image = msg("M3", 1_700_000_150, false, "");
    image.media_type = "image".into();
    image.filename = "image_x.jpg".into();
    image.file_length = 10;
    s.store_messages(vec![msg("M1", 1_700_000_000, false, "hello"), reply, image])
        .await
        .unwrap();
    s.store_reaction(Reaction {
        message_id: "M1".into(),
        chat_jid: "628222@s.whatsapp.net".into(),
        device_id: dev.into(),
        reactor_jid: "628222@s.whatsapp.net".into(),
        emoji: "👍".into(),
        reaction_timestamp: 1_700_000_050,
        ..Default::default()
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn chats_list_contract() {
    let h = harness().await;
    seed_chats(&h).await;
    let (status, body) = call(&h.app, get("/chats")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["code"], "SUCCESS");
    assert_eq!(body["message"], "Success get chat list");
    let r = &body["results"];
    assert_eq!(
        r["pagination"],
        serde_json::json!({"limit": 25, "offset": 0, "total": 3})
    );
    let data = r["data"].as_array().unwrap();
    assert_eq!(data.len(), 3);
    // Newest first; JID-like stored name replaced by the saved contact name.
    assert_eq!(data[0]["jid"], "628222@s.whatsapp.net");
    assert_eq!(data[0]["name"], "Bob Smith");
    assert_eq!(data[0]["last_message_time"], "2023-11-14T22:16:40Z");
    assert_eq!(data[1]["name"], "Group 1203");
    assert_eq!(data[1]["archived"], true);
    assert_eq!(data[1]["ephemeral_expiration"], 86400);
    assert_eq!(data[2]["last_message_time"], "0001-01-01T00:00:00Z");
    let keys: Vec<_> = data[0].as_object().unwrap().keys().cloned().collect();
    assert_eq!(
        keys,
        [
            "archived",
            "created_at",
            "ephemeral_expiration",
            "jid",
            "last_message_time",
            "name",
            "updated_at"
        ]
    );

    let (_, body) = call(&h.app, get("/chats?archived=true&limit=abc")).await;
    assert_eq!(body["results"]["pagination"]["total"], 1);
    assert_eq!(body["results"]["pagination"]["limit"], 25);
    let (_, body) = call(&h.app, get("/chats?search=Car&offset=0")).await;
    assert_eq!(body["results"]["data"][0]["name"], "Carol");
    let (_, body) = call(&h.app, get("/chats?has_media=true")).await;
    assert_eq!(body["results"]["pagination"]["total"], 1);

    let (status, body) = call(&h.app, get("/chats?limit=101&offset=-1")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "VALIDATION_ERROR");
    assert_eq!(
        body["message"],
        "limit: must be no greater than 100; offset: must be no less than 0."
    );
}

#[tokio::test]
async fn chat_messages_contract() {
    let h = harness().await;
    seed_chats(&h).await;
    let (status, body) = call(&h.app, get("/chat/628222%40s.whatsapp.net/messages")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["message"], "Success get chat messages");
    let r = &body["results"];
    assert_eq!(
        r["pagination"],
        serde_json::json!({"limit": 50, "offset": 0, "total": 3})
    );
    assert_eq!(r["chat_info"]["name"], "Bob Smith");
    let data = r["data"].as_array().unwrap();
    assert_eq!(
        data.iter()
            .map(|m| m["id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["M2", "M3", "M1"]
    );
    assert_eq!(data[0]["context_metadata"], r#"{"replied_to_id":"M1"}"#);
    assert_eq!(data[0]["sender_display_name"], "628111");
    assert!(data[0].get("reactions").is_none());
    assert!(data[0].get("call_metadata").is_none());
    assert_eq!(data[1]["media_type"], "image");
    assert_eq!(data[1]["file_length"], 10);
    assert_eq!(data[2]["sender_display_name"], "Bob Smith");
    assert_eq!(data[2]["timestamp"], "2023-11-14T22:13:20Z");
    assert_eq!(
        data[2]["reactions"],
        serde_json::json!([{
            "emoji": "👍",
            "sender_jid": "628222@s.whatsapp.net",
            "sender_display_name": "Bob Smith",
            "is_from_me": false,
            "timestamp": "2023-11-14T22:14:10Z"
        }])
    );

    // Filters; total still counts the whole chat.
    let (_, body) = call(
        &h.app,
        get("/chat/628222@s.whatsapp.net/messages?is_from_me=false&media_only=true"),
    )
    .await;
    assert_eq!(body["results"]["data"].as_array().unwrap().len(), 1);
    assert_eq!(body["results"]["pagination"]["total"], 3);
    let (_, body) = call(
        &h.app,
        get(
            "/chat/628222@s.whatsapp.net/messages?start_time=2023-11-14T22:14:00Z&limit=1&offset=1",
        ),
    )
    .await;
    assert_eq!(body["results"]["data"][0]["id"], "M3");
    let (_, body) = call(
        &h.app,
        get("/chat/628222@s.whatsapp.net/messages?search=HELL"),
    )
    .await;
    assert_eq!(body["results"]["data"][0]["id"], "M1");

    // Unknown chat: empty, not an error.
    let (status, body) = call(&h.app, get("/chat/628999@s.whatsapp.net/messages")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["results"]["data"], serde_json::json!([]));
    assert_eq!(body["results"]["chat_info"]["name"], "628999");
    assert_eq!(body["results"]["chat_info"]["last_message_time"], "");

    let (status, body) = call(
        &h.app,
        get("/chat/628222@s.whatsapp.net/messages?start_time=yesterday"),
    )
    .await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .starts_with("invalid start_time format")
    );
}

#[tokio::test]
async fn chat_actions_validate_then_need_login() {
    let h = harness().await;
    seed_chats(&h).await;
    let jid = "628222@s.whatsapp.net";
    for path in ["pin", "archive", "disappearing"] {
        // Empty body or bad JSON → 400 BAD_REQUEST.
        let (status, body) = call(&h.app, post_json(&format!("/chat/{jid}/{path}"), "")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{path}");
        assert_eq!(body["code"], "BAD_REQUEST");
        assert_eq!(body["message"], "Invalid request body");
        let (status, _) = call(&h.app, post_json(&format!("/chat/{jid}/{path}"), "{")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }
    let (status, body) = call(
        &h.app,
        post_json(
            &format!("/chat/{jid}/disappearing"),
            r#"{"timer_seconds":5}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        body["message"],
        "timer_seconds: timer_seconds must be one of: 0 (off), 86400 (24h), 604800 (7d), 7776000 (90d)."
    );
    let (status, body) = call(
        &h.app,
        post_json(&format!("/chat/{jid}/pin"), r#"{"pinned":true}"#),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], "AUTHENTICATION_ERROR");

    // History: count range, then an anchor, then login.
    let (status, body) = call(
        &h.app,
        post_json(&format!("/chat/{jid}/history"), r#"{"count":501}"#),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["message"], "count: must be no greater than 500.");
    let (status, body) = call(&h.app, post_json("/chat/628999@s.whatsapp.net/history", "")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        body["message"],
        "chat has no stored messages yet; nothing to anchor the history sync request on"
    );
    let (status, _) = call(&h.app, post_json(&format!("/chat/{jid}/history"), "")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

// ---- /message/{id}/* ----

#[tokio::test]
async fn message_endpoints_validate_then_need_login() {
    let h = harness().await;
    seed_chats(&h).await;
    for action in [
        "reaction", "revoke", "delete", "update", "read", "played", "star", "unstar", "forward",
    ] {
        let (status, body) = call(&h.app, post_json(&format!("/message/M1/{action}"), "{}")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{action}");
        assert_eq!(body["code"], "VALIDATION_ERROR");
        let expected = if action == "update" {
            "message: cannot be blank; phone: cannot be blank."
        } else {
            "phone: cannot be blank."
        };
        assert_eq!(body["message"], expected, "{action}");
    }
    let (status, _) = call(&h.app, get("/message/M1/download")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // Valid requests reach MustLogin.
    for action in [
        "reaction", "revoke", "delete", "read", "played", "star", "unstar",
    ] {
        let (status, body) = call(
            &h.app,
            post_json(
                &format!("/message/M1/{action}"),
                r#"{"phone":"628222","emoji":"👍"}"#,
            ),
        )
        .await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{action}");
        assert_eq!(body["code"], "AUTHENTICATION_ERROR");
    }
    let (status, _) = call(&h.app, get("/message/M1/download?phone=628222")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // A malformed body is a 500, like GOWA's panic on Bind().Body.
    let (status, body) = call(&h.app, post_json("/message/M1/revoke", "{")).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body["code"], "INTERNAL_SERVER_ERROR");
}

#[tokio::test]
async fn message_update_validation() {
    let h = harness().await;
    let cases = [
        (
            r#"{"phone":"628","message":"x","link":"nope"}"#,
            "link: must be a valid URL.",
        ),
        (
            r#"{"phone":"628","message":"x","media_type":"video"}"#,
            "media_type: must be a valid value.",
        ),
        (
            r#"{"phone":"628","message":"x","media_type":"image","link":"https://a.b"}"#,
            "link: cannot be combined with media_type.",
        ),
    ];
    for (body, message) in cases {
        let (status, res) = call(&h.app, post_json("/message/M1/update", body)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(res["message"], message);
    }
}

#[tokio::test]
async fn message_forward_checks_storage_first() {
    use ruwa_gateway::storage::chat::{ChatStore, Message};
    let h = harness().await;
    seed_chats(&h).await;
    ChatStore::new(h.db.clone())
        .store_message(Message {
            id: "C1".into(),
            chat_jid: "628222@s.whatsapp.net".into(),
            device_id: "default".into(),
            sender: "628222@s.whatsapp.net".into(),
            content: "Contact: Bob (62)".into(),
            timestamp: 1,
            ..Default::default()
        })
        .await
        .unwrap();
    let forward = |id: &str, body: &str| post_json(&format!("/message/{id}/forward"), body);

    let (status, body) = call(&h.app, forward("NOPE", r#"{"phone":"628333"}"#)).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(body["message"], "message with ID NOPE not found");
    let (status, body) = call(&h.app, forward("C1", r#"{"phone":"628333"}"#)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["message"], "unsupported message type for forward");
    let (status, body) = call(&h.app, forward("M1", r#"{"phone":"628333","duration":5}"#)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .starts_with("duration must be one of")
    );
    let (status, _) = call(&h.app, forward("M1", r#"{"phone":"628333"}"#)).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

// ---- /group/* ----

async fn expect_login(h: &Harness, req: Request<Body>) {
    let (status, body) = call(&h.app, req).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "{body}");
    assert_eq!(body["code"], "AUTHENTICATION_ERROR");
}

#[tokio::test]
async fn group_validation_then_login() {
    let h = harness().await;

    // Create: both fields, then each participant, then login.
    expect_validation(
        &h,
        post_json("/group", "{}"),
        "participants: cannot be blank; title: cannot be blank.",
    )
    .await;
    expect_validation(
        &h,
        post_json("/group", r#"{"title":"T","participants":["62",""]}"#),
        "participants: (1: cannot be blank.).",
    )
    .await;
    expect_login(
        &h,
        post_json("/group", r#"{"title":"T","participants":["62"]}"#),
    )
    .await;

    // Link based endpoints.
    expect_validation(
        &h,
        post_json("/group/join-with-link", "{}"),
        "link: cannot be blank.",
    )
    .await;
    expect_validation(&h, get("/group/info-from-link"), "link: cannot be blank.").await;
    expect_login(&h, get("/group/info-from-link?link=abc")).await;
    expect_login(&h, post_json("/group/join-with-link", r#"{"link":"abc"}"#)).await;

    // group_id required everywhere else.
    expect_validation(&h, get("/group/info"), "group_id: cannot be blank.").await;
    expect_validation(&h, get("/group/invite-link"), "group_id: cannot be blank.").await;
    for path in ["leave", "locked", "announce", "topic"] {
        expect_validation(
            &h,
            post_json(&format!("/group/{path}"), "{}"),
            "group_id: cannot be blank.",
        )
        .await;
        expect_login(
            &h,
            post_json(
                &format!("/group/{path}"),
                r#"{"group_id":"1203632782168851111"}"#,
            ),
        )
        .await;
    }
    expect_login(&h, get("/group/info?group_id=1203632782168851111")).await;
    expect_login(
        &h,
        get("/group/invite-link?group_id=1203632782168851111&reset=true"),
    )
    .await;

    // Participant management.
    for path in [
        "participants",
        "participants/remove",
        "participants/promote",
        "participants/demote",
        "participant-requests/approve",
        "participant-requests/reject",
    ] {
        expect_validation(
            &h,
            post_json(&format!("/group/{path}"), "{}"),
            "group_id: cannot be blank; participants: cannot be blank.",
        )
        .await;
        expect_login(
            &h,
            post_json(
                &format!("/group/{path}"),
                r#"{"group_id":"1203632782168851111","participants":["62811"]}"#,
            ),
        )
        .await;
    }

    // Name: required and at most 25 characters.
    expect_validation(
        &h,
        post_json("/group/name", r#"{"group_id":"1203632782168851111"}"#),
        "name: cannot be blank.",
    )
    .await;
    expect_validation(
        &h,
        post_json(
            "/group/name",
            r#"{"group_id":"1203632782168851111","name":"abcdefghijklmnopqrstuvwxyz"}"#,
        ),
        "name: the length must be between 1 and 25.",
    )
    .await;
    expect_login(
        &h,
        post_json(
            "/group/name",
            r#"{"group_id":"1203632782168851111","name":"Team"}"#,
        ),
    )
    .await;

    // Form bodies bind the same way (bool as 1/true).
    let form = Request::post("/group/locked")
        .header("content-type", "application/x-www-form-urlencoded")
        .body(Body::from("group_id=1203632782168851111&locked=1"))
        .unwrap();
    expect_login(&h, form).await;

    // A body that cannot be bound is a 500, as in GOWA.
    let (status, _) = call(&h.app, post_json("/group/locked", "{")).await;
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
}

#[tokio::test]
async fn group_listing_endpoints_reject_empty_id_before_login() {
    let h = harness().await;
    for path in [
        "/group/participants",
        "/group/participants/export",
        "/group/participant-requests",
    ] {
        let (status, body) = call(&h.app, get(path)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{path}");
        assert_eq!(body["code"], "INVALID_GROUP_ID");
        assert_eq!(body["message"], "Group ID cannot be empty");
        expect_login(&h, get(&format!("{path}?group_id=1203632782168851111"))).await;
    }
}

#[tokio::test]
async fn group_photo_validation() {
    let h = harness().await;
    let png = {
        let mut out = std::io::Cursor::new(Vec::new());
        image::DynamicImage::new_rgb8(8, 8)
            .write_to(&mut out, image::ImageFormat::Png)
            .unwrap();
        out.into_inner()
    };
    let group = ("group_id", "1203632782168851111");

    let (status, body) = call(
        &h.app,
        multipart(
            "/group/photo",
            &[group],
            Some(("photo", "a.gif", "image/gif", &png)),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "INVALID_IMAGE_FORMAT");
    assert_eq!(
        body["message"],
        "Image validation failed: unsupported image format: image/gif (supported: JPEG, PNG, WebP)"
    );
    let (status, body) = call(
        &h.app,
        multipart(
            "/group/photo",
            &[group],
            Some(("photo", "a.png", "image/png", b"nope")),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "INVALID_IMAGE_FORMAT");
    assert!(
        body["message"]
            .as_str()
            .unwrap()
            .starts_with("Image validation failed: invalid image file")
    );

    // Valid photo and photo removal both reach the login check.
    expect_login(
        &h,
        multipart(
            "/group/photo",
            &[group],
            Some(("photo", "a.png", "image/png", &png)),
        ),
    )
    .await;
    expect_login(&h, multipart("/group/photo", &[group], None)).await;
    expect_validation(
        &h,
        multipart("/group/photo", &[], None),
        "group_id: cannot be blank.",
    )
    .await;
}

#[tokio::test]
async fn send_presence_contract() {
    let h = harness().await;
    expect_validation(
        &h,
        post_json("/send/presence", r#"{"type":"away"}"#),
        "type: must be a valid value.",
    )
    .await;
    expect_validation(
        &h,
        post_json("/send/chat-presence", "{}"),
        "action: cannot be blank; phone: cannot be blank.",
    )
    .await;
    expect_validation(
        &h,
        post_json(
            "/send/chat-presence",
            r#"{"phone":"628123","action":"jump"}"#,
        ),
        "action: must be a valid value.",
    )
    .await;
    for (path, body) in [
        ("/send/presence", r#"{"type":"available"}"#),
        (
            "/send/chat-presence",
            r#"{"phone":"628123","action":"start"}"#,
        ),
    ] {
        let (status, resp) = call(&h.app, post_json(path, body)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{path}");
        assert_eq!(resp["code"], "AUTHENTICATION_ERROR");
    }
}

#[tokio::test]
async fn send_sticker_contract() {
    let h = harness().await;
    expect_validation(
        &h,
        post_json("/send/sticker", r#"{"phone":"628"}"#),
        "either Sticker or StickerURL must be provided",
    )
    .await;
    expect_validation(
        &h,
        multipart(
            "/send/sticker",
            &[("phone", "628")],
            Some(("sticker", "a.bmp", "image/bmp", b"BM")),
        ),
        "your sticker is not allowed. please use jpg/jpeg/png/webp/gif",
    )
    .await;
    expect_validation(
        &h,
        multipart(
            "/send/sticker",
            &[
                ("phone", "628"),
                ("sticker_url", "https://example.com/a.png"),
            ],
            Some(("sticker", "a.png", "image/png", b"x")),
        ),
        "cannot provide both Sticker file and StickerURL",
    )
    .await;
    expect_validation(
        &h,
        post_json("/send/sticker", r#"{"phone":"628","sticker_url":"nope"}"#),
        "StickerURL must be a valid URL",
    )
    .await;
}

#[tokio::test]
async fn send_poll_contract() {
    let h = harness().await;
    for (body, msg) in [
        (r#"{"phone":"628"}"#, "options: cannot be blank."),
        (
            r#"{"options":["a","b"]}"#,
            "max_answer: cannot be blank; phone: cannot be blank; question: cannot be blank.",
        ),
        (
            r#"{"phone":"628","question":"q","options":["a",""],"max_answer":1}"#,
            "options: (1: cannot be blank.).",
        ),
        (
            r#"{"phone":"628","question":"q","options":["a","b"],"max_answer":3}"#,
            "max_answer: must be no greater than 2.",
        ),
        (
            r#"{"phone":"628","question":"q","options":["a","b"],"max_answer":-1}"#,
            "max_answer: must be no less than 1.",
        ),
        (
            r#"{"phone":"628","question":"q","options":["a","a"],"max_answer":1}"#,
            "options should be unique",
        ),
    ] {
        expect_validation(&h, post_json("/send/poll", body), msg).await;
    }
    let (status, resp) = call(
        &h.app,
        post_json(
            "/send/poll",
            r#"{"phone":"628123","question":"q","options":["a","b"],"max_answer":1}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(resp["code"], "AUTHENTICATION_ERROR");
}

#[tokio::test]
async fn user_business_and_newsletters_contract() {
    let h = harness().await;
    let (status, body) = call(&h.app, get("/user/business-profile")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["code"], "VALIDATION_ERROR");
    assert_eq!(body["message"], "phone: cannot be blank.");
    for path in [
        "/user/business-profile?phone=628123",
        "/user/my/newsletters",
    ] {
        let (status, body) = call(&h.app, get(path)).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{path}");
        assert_eq!(body["code"], "AUTHENTICATION_ERROR", "{path}");
    }
}

#[tokio::test]
async fn call_reject_contract() {
    let h = harness().await;
    expect_validation(
        &h,
        post_json("/call/reject", "{}"),
        "call_id: cannot be blank; caller_jid: cannot be blank.",
    )
    .await;
    expect_validation(
        &h,
        post_json("/call/reject", r#"{"caller_jid":"  ","call_id":"A"}"#),
        "caller_jid: cannot be blank.",
    )
    .await;
    let (status, body) = call(
        &h.app,
        post_json(
            "/call/reject",
            r#"{"caller_jid":"628987654321@s.whatsapp.net","call_id":"A"}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], "AUTHENTICATION_ERROR");
}

#[tokio::test]
async fn newsletter_contract() {
    let h = harness().await;
    expect_validation(
        &h,
        post_json("/newsletter/follow", "{}"),
        "newsletter_id: cannot be blank.",
    )
    .await;
    expect_validation(
        &h,
        post_json(
            "/newsletter/follow",
            r#"{"newsletter_id":"628@s.whatsapp.net"}"#,
        ),
        "newsletter_id: must end with @newsletter.",
    )
    .await;
    expect_validation(
        &h,
        post_json("/newsletter/unfollow", "{}"),
        "newsletter_id: cannot be blank.",
    )
    .await;
    for (path, msg) in [
        (
            "/newsletter/info",
            "exactly one of newsletter_id or invite must be provided.",
        ),
        (
            "/newsletter/info?newsletter_id=1@newsletter&invite=abc",
            "exactly one of newsletter_id or invite must be provided.",
        ),
        (
            "/newsletter/info?newsletter_id=628@s.whatsapp.net",
            "newsletter_id: must end with @newsletter.",
        ),
        ("/newsletter/messages", "newsletter_id: cannot be blank."),
        (
            "/newsletter/messages?newsletter_id=1@newsletter&count=101",
            "count: must be no greater than 100.",
        ),
        (
            "/newsletter/messages?newsletter_id=1@newsletter&count=-1",
            "count: must be no less than 1.",
        ),
        (
            "/newsletter/messages/0/download?newsletter_id=1@newsletter",
            "server_id: must be no less than 1.",
        ),
        (
            "/newsletter/messages/abc/download?newsletter_id=1@newsletter",
            "server_id: must be a valid integer.",
        ),
    ] {
        let (status, body) = call(&h.app, get(path)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{path}");
        assert_eq!(body["message"], msg, "{path}");
    }
    let (status, _) = call(
        &h.app,
        get("/newsletter/messages?newsletter_id=1@newsletter&count=x"),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "bad int binds as 500"
    );
    for (method_get, path) in [
        (true, "/newsletter/info?newsletter_id=1@newsletter"),
        (true, "/newsletter/messages?newsletter_id=1@newsletter"),
        (
            true,
            "/newsletter/messages/5/download?newsletter_id=1@newsletter",
        ),
        (false, "/newsletter/follow"),
        (false, "/newsletter/unfollow"),
    ] {
        let req = if method_get {
            get(path)
        } else {
            post_json(path, r#"{"newsletter_id":"1@newsletter"}"#)
        };
        let (status, body) = call(&h.app, req).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{path}");
        assert_eq!(body["code"], "AUTHENTICATION_ERROR", "{path}");
    }
}

#[tokio::test]
async fn newsletter_live_updates_contract() {
    let h = harness().await;
    let (status, body) = call(&h.app, get("/newsletter/live-updates")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["message"], "Success list newsletter live updates");
    assert_eq!(body["results"]["data"], serde_json::json!([]));

    expect_validation(
        &h,
        post_json("/newsletter/live-updates", "{}"),
        "newsletter_id: cannot be blank.",
    )
    .await;
    let (status, body) = call(
        &h.app,
        post_json(
            "/newsletter/live-updates",
            r#"{"newsletter_id":"1@newsletter"}"#,
        ),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["code"], "AUTHENTICATION_ERROR");

    let delete = |uri: &str| {
        axum::http::Request::builder()
            .method("DELETE")
            .uri(uri)
            .body(axum::body::Body::empty())
            .unwrap()
    };
    let (status, body) = call(&h.app, delete("/newsletter/live-updates")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["message"], "newsletter_id: cannot be blank.");
    let (status, body) = call(
        &h.app,
        delete("/newsletter/live-updates?newsletter_id=1@newsletter"),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        body["message"],
        "newsletter_id: no live updates subscription for 1@newsletter."
    );
}
