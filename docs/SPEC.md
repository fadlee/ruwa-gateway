# ruwa-gateway — Spesifikasi

> REST API WhatsApp yang kompatibel dengan **GOWA** (`aldinokemal/go-whatsapp-web-multidevice`),
> ditulis dalam Rust di atas **whatsapp-rust** (`oxidezap/whatsapp-rust`).

Status: draft v0.1 — 2026-11-23

## 1. Tujuan

1. **Drop-in replacement GOWA** untuk client yang ada (n8n node `@aldinokemal2104/n8n-nodes-gowa`,
   gowa-ui, integrasi custom berbasis webhook): path, method, request body, response envelope,
   header, dan payload webhook harus identik.
2. Footprint kecil: satu binary statis, memori idle rendah, multi-device dalam satu proses.
3. Konfigurasi memakai nama flag/env yang sama dengan GOWA (`APP_PORT`, `APP_BASIC_AUTH`,
   `WHATSAPP_WEBHOOK`, dst.) supaya file `.env` / docker-compose lama bisa dipakai ulang.

### Non-goal (setidaknya sampai v1)

- Integrasi Chatwoot (`/chatwoot/*`, `CHATWOOT_*`).
- MCP server (`/mcp`) dan MCP OAuth.
- Migrasi otomatis sesi dari database whatsmeow (lihat §9 — dieksplorasi terpisah).
- Panggilan suara/video.

## 2. Referensi kontrak

Sumber kebenaran kompatibilitas (GOWA `main`, whatsmeow `v0.0.0-20261105212532-307a2edf117d`):

| Dokumen | Isi |
|---|---|
| `docs/openapi.yaml` (GOWA) | 99 path REST, schema request/response |
| `docs/webhook-payload.md` (GOWA) | 21 event webhook, format payload, HMAC |
| `src/.env.example` (GOWA) | nama konfigurasi |

Salinan snapshot disimpan di `docs/upstream/` (lihat PLAN fase 0) dan dipakai sebagai fixture contract test.

## 3. Arsitektur

```
┌──────────────────────────── ruwa-gateway (1 proses) ────────────────────────────┐
│ HTTP (axum)                                                                     │
│   middleware: base-path → basic-auth → CORS → device-scope (X-Device-Id)        │
│   handlers: app / devices / send / message / user / chat / group / newsletter   │
│        │                                                                        │
│ Usecase layer (logika GOWA: validasi, normalisasi JID, mention, media prep)     │
│        │                                                                        │
│ DeviceManager ── Arc<DeviceInstance> per slot                                   │
│        │            ├─ whatsapp_rust::Bot / Client  (protokol, E2E)             │
│        │            ├─ SqliteStore  storages/devices/<id>/session.db            │
│        │            └─ event pump → EventMapper → Webhook dispatcher / WS hub    │
│        │                                                                        │
│ Registry DB (storages/ruwa.db): device slots, webhook per-device, chat storage, │
│   scheduled sends                                                               │
└─────────────────────────────────────────────────────────────────────────────────┘
```

### 3.1 Crate / modul

Satu binary crate `ruwa-gateway` dengan modul:

| Modul | Tanggung jawab |
|---|---|
| `config` | CLI (clap) + env, nama mengikuti GOWA |
| `http` | router axum, middleware (auth, device scope, CORS), response envelope |
| `http::routes::*` | satu file per grup endpoint GOWA (handler tipis) |
| `usecase` | logika GOWA `usecase/`: validasi, resolusi JID, builder pesan |
| `error` | `ApiError` dengan kode/pesan GOWA (lihat `docs/error-codes.md`) |
| `device` | `DeviceManager`, `DeviceInstance`, lifecycle (login, logout, reconnect), event pump |
| `wa` | adapter tipis ke whatsapp-rust (konversi JID, builder pesan, upload media) |
| `events` | mapping `whatsapp_rust::Event` → event GOWA |
| `webhook` | dispatcher, filter, HMAC, retry |
| `storage` | registry + chat storage (SQLite via `diesel`) |
| `media` | sticker WebP, kompresi gambar/video (ffmpeg opsional) |

### 3.2 Dependensi utama

- `whatsapp-rust` — **di-pin ke git rev** (versi crates.io 0.7.0 mengaktifkan `simd` yang butuh
  nightly). Fitur: `sqlite-storage, tokio-transport, tokio-runtime, ureq-client, tokio-native`.
- `tokio`, `axum`, `tower-http` (CORS, trace), `serde`/`serde_json`, `clap` (derive + env),
- `diesel` 2.4 (sqlite) untuk registry & chat storage — **bukan sqlx**: whatsapp-rust sudah membawa diesel + `libsqlite3-sys` 0.40, dan dua versi `libsqlite3-sys` akan bentrok (`links = "sqlite3"`), `reqwest` (rustls) untuk webhook,
- `hmac` + `sha2` untuk signature, `qrcode` + `image` untuk PNG QR, `tracing`.
- Media: `image` (thumbnail/kompresi), `infer` + `mime_guess` (MIME), `serde_html_form` (form/multipart),
  ffmpeg/ffprobe eksternal (opsional; wajib untuk video dan konversi PTT, sama seperti GOWA).
- URL dari user (`*_url`, link preview) diambil lewat `media::fetch` yang menolak alamat non-publik
  (loopback, privat, link-local, CGNAT) di setiap hop redirect.

Toolchain: Rust stable ≥ 1.95 (MSRV whatsapp-rust).

## 4. Kontrak HTTP

### 4.1 Envelope response

Semua response JSON:

```json
{ "code": "SUCCESS", "message": "Success", "results": { ... } }
```

- `results` di-omit bila kosong (`omitempty`).
- Error validasi: HTTP 400, `code: "VALIDATION_ERROR"`, pesan gaya ozzo (`phone: cannot be blank.`).
- Error lain / gagal bind body: HTTP 500, `code: "INTERNAL_SERVER_ERROR"`.
- Daftar lengkap kode: [error-codes.md](error-codes.md).
- **Bukan JSON**: basic auth gagal → 401 teks `Unauthorized` + `WWW-Authenticate`;
  `GET /health` → teks `OK`.

### 4.2 Autentikasi

- `APP_BASIC_AUTH=user1:pass1,user2:pass2` → HTTP Basic.
- Kosong + bind non-loopback → **refuse to start** kecuali `APP_ALLOW_UNAUTHENTICATED=true`.
- WebSocket (`/ws`): `?authorization=<base64(user:pass)>` diterima sebagai alternatif header.
- Tanpa auth: `/health` (selalu di root, mengabaikan base path) dan `{base}/statics/*`
  (QR & media; GOWA mendaftarkannya sebelum middleware auth).
- `/app/info` butuh auth tapi tidak butuh device; route lain butuh auth + device scope
  (termasuk `/app/devices`, sama seperti GOWA).

### 4.3 Device scoping

- Endpoint device-scoped membaca `X-Device-Id` header, lalu fallback query `device_id`.
- Jika tidak ada dan hanya satu device terdaftar → pakai device itu.
- Jika tidak ada dan >1 device → 400 `device_id is required via X-Device-Id header or device_id query`.
- Device tidak dikenal → 404 `device not found; create a device first from /api/devices or provide a valid X-Device-Id`.
- `X-Device-Id` juga boleh berisi JID akun; header di-URL-decode.
- Bila belum ada slot sama sekali, slot `default` dibuat otomatis (pengganti instance transient GOWA).

### 4.4 Cakupan endpoint per milestone

| Grup | Endpoint | Milestone |
|---|---|---|
| health/app | `/health`, `/app/login`, `/app/login-with-code`, `/app/logout`, `/app/reconnect`, `/app/devices`, `/app/status`, `/app/info` | M1 |
| devices | `/devices`, `/devices/{id}`, `/devices/{id}/login`, `/login/code`, `/logout`, `/reconnect`, `/status`, `/webhook` | M2 |
| send (dasar) | `/send/message`, `/send/image`, `/send/file`, `/send/audio`, `/send/video`, `/send/location`, `/send/contact`, `/send/link` | M1 (message) / M2 (media) |
| send (lanjut) | `/send/sticker`, `/send/poll`, `/send/presence`, `/send/chat-presence` | M3 |
| user | `/user/check`, `/user/info`, `/user/avatar`, `/user/pushname`, `/user/my/*`, `/user/business-profile`, `/user/presence/subscribe` | M2 (`check`,`info`) / M3 |
| message | `/message/{id}/revoke|delete|reaction|update|read|played|star|unstar|forward|download` | M3 |
| chat | `/chats`, `/chat/{jid}/messages`, `/history`, `/pin`, `/disappearing`, `/archive` | M3 |
| group | semua `/group/*` | M3 |
| newsletter | semua `/newsletter/*` | M4 |
| schedule | `/send/schedules*` + field `scheduled_at` di send | M4 |
| call | `/call/reject` | M3 |
| ws/ui | `/ws`, serve gowa-ui di `/` | M4 |
| app metrics | `/app/metrics` | M4 (selesai) |
| passkey | `/app/passkey*` | M5 (opsional) |
| interactive | `/send/interactive` | M5 (eksperimental) |
| chatwoot, mcp | — | di luar scope v1 |

### 4.5 Login QR

`GET /app/login` → `results: { device_id, qr_link, qr_duration }`. `qr_link` adalah URL PNG yang
disajikan server di `/statics/qrcode/scan-qr-<id>.png` (sama dengan GOWA), dibangkitkan dari
event `PairingQrCode`.

`GET /app/login-with-code?phone=...` → `results: { device_id, pair_code }` dari event `PairingCode`.

### 4.6 Format JID input

Field `phone` menerima: nomor polos (`628xxx`), JID user (`628xxx@s.whatsapp.net`), JID grup
(`...@g.us`), newsletter (`...@newsletter`), `status@broadcast`. Normalisasi mengikuti
`pkg/utils` GOWA (strip `+`, spasi, `-`; tambahkan suffix `@s.whatsapp.net` bila polos).
Validasi `WHATSAPP_ACCOUNT_VALIDATION` → cek `is_on_whatsapp` sebelum kirim.

## 5. Webhook

### 5.1 Envelope

```json
{
  "event": "message",
  "device_id": "628123456789@s.whatsapp.net",
  "session_id": "org_2",
  "timestamp": "2026-10-28T09:00:00Z",
  "payload": { }
}
```

- `device_id` = JID akun; `session_id` = ID slot (selalu ada untuk event `device.*`).
- Header `X-Hub-Signature-256: sha256=<hex HMAC-SHA256(body, secret)>`.
- Konfigurasi global: `WHATSAPP_WEBHOOK` (multi URL dipisah koma), `WHATSAPP_WEBHOOK_SECRET`,
  `WHATSAPP_WEBHOOK_EVENTS`, `WHATSAPP_WEBHOOK_IGNORE_JIDS`, `WHATSAPP_WEBHOOK_INSECURE_SKIP_VERIFY`.
- Per-device override via `POST /devices` / `/devices/{id}/webhook`.
- Delivery: async queue per device, retry exponential (GOWA: 5x), timeout 10 detik.

### 5.2 Mapping event

| Event GOWA | Sumber `whatsapp_rust::Event` | Milestone |
|---|---|---|
| `message` | `Messages` (non-reaction/edit/revoke) | M1 |
| `message.ack` | `Receipt` | M1 |
| `message.reaction` | `Messages` dengan `reactionMessage` | M2 |
| `message.revoked` | `Messages` dengan `protocolMessage.REVOKE` | M2 |
| `message.edited` | `Messages` dengan `protocolMessage.MESSAGE_EDIT` | M2 |
| `message.deleted` | `DeleteMessageForMeUpdate` | M3 |
| `chat_presence` | `ChatPresence` | M2 |
| `presence` | `Presence` | M3 |
| `group.participants`, `group.info`, `group.joined` | `GroupUpdate` | M3 |
| `label.edit`, `label.association` | `LabelEditUpdate`, `LabelAssociationUpdate` | M3 |
| `newsletter.*` | `NewsletterLiveUpdate`, notifikasi MEX | M4 |
| `call.offer` | `IncomingCall` (+ `WHATSAPP_AUTO_REJECT_CALL`) | M3 |
| `device.logged_out` | `LoggedOut` | M1 |
| `device.temporary_ban` | `TemporaryBan` | M2 |
| `device.reachout_timelock` | `ReachoutTimelockUpdate` | M3 |

Field payload (`id`, `chat_id`, `from`, `from_lid`, `sender_display_name`, `from_name`,
`timestamp` RFC3339, `is_from_me`, `body`, media fields, `quoted_*`, `mentions`, …) mengikuti
contoh JSON di `webhook-payload.md` persis. Setiap contoh di dokumen itu menjadi golden test.

## 6. Penyimpanan

| File | Isi | Owner |
|---|---|---|
| `storages/devices/<storage_key>/session.db` (`storage_key` = id bila aman sebagai nama file, selain itu `x`+hex) | state protokol (keys, sessions, app state) | whatsapp-rust `SqliteStore` |
| `storages/ruwa.db` | `devices` (slot, `storage_key`, jid, webhook cfg), `chats`, `messages`, `message_reactions`, `message_edits`, `poll_definitions`, `scheduled_sends` | ruwa-gateway (diesel migrations) |
| `statics/media/` | media terunduh (`WHATSAPP_AUTO_DOWNLOAD_MEDIA`), disajikan publik di `/statics/media` seperti GOWA | ruwa-gateway |
| `statics/qrcode/` | PNG QR | ruwa-gateway |

Skema chat storage meniru tabel GOWA `infrastructure/chatstorage/sqlite_repository.go` agar query
`/chats` dan `/chat/{jid}/messages` menghasilkan bentuk response yang sama.

Postgres: tidak di v1. Bila dibutuhkan, implementasi trait `wacore::store::traits::Backend`
sendiri + diesel postgres untuk registry.

## 7. Konfigurasi (subset v1)

| Flag | Env | Default |
|---|---|---|
| `--port` | `APP_PORT` | `3000` |
| `--host` | `APP_HOST` | `0.0.0.0` |
| `--debug` | `APP_DEBUG` | `false` |
| `--os` | `APP_OS` | `Chrome` |
| `--basic-auth` | `APP_BASIC_AUTH` | — |
| — | `APP_ALLOW_UNAUTHENTICATED` | `false` |
| `--base-path` | `APP_BASE_PATH` | — |
| `--cors-allowed-origins` | `APP_CORS_ALLOWED_ORIGINS` | — |
| `--timezone` | `APP_TIMEZONE` | `UTC` (zona IANA untuk timestamp respons REST chat/pesan/newsletter; webhook & storage tetap UTC; nama tak dikenal → gagal start) |
| `--ui-enabled`, `--ui-auto-update` | `APP_UI_ENABLED`, `APP_UI_AUTO_UPDATE` | `true` |
| `--ui-repo`, `--ui-asset-name` | `APP_UI_REPO`, `APP_UI_ASSET_NAME` | `aldinokemal/gowa-ui`, `gowa-ui.html` |
| `--ui-update-interval` | `APP_UI_UPDATE_INTERVAL` | `3h` |
| `--ui-github-token`, `--ui-asset-sha256` | `APP_UI_GITHUB_TOKEN`, `APP_UI_ASSET_SHA256` | — |
| `--webhook` | `WHATSAPP_WEBHOOK` | — |
| `--webhook-secret` | `WHATSAPP_WEBHOOK_SECRET` | `secret` |
| `--webhook-events` | `WHATSAPP_WEBHOOK_EVENTS` | semua |
| `--webhook-ignore-jids` | `WHATSAPP_WEBHOOK_IGNORE_JIDS` | — |
| `--webhook-insecure-skip-verify` | `WHATSAPP_WEBHOOK_INSECURE_SKIP_VERIFY` | `false` |
| `--webhook-include-outgoing` | `WHATSAPP_WEBHOOK_INCLUDE_OUTGOING` | `false` (`true` = webhook `message` juga untuk pesan yang dikirim lewat API; di GOWA env ini hanya ada di `.env.example`, tidak dibaca kode) |
| `--whatsapp-proxy` | `WHATSAPP_PROXY` | — (`socks5://`/`socks5h://`/`http://`/`https://`, boleh `user:pass@`; untuk WebSocket + upload/unduh media; URL dari user tetap langsung; URL tak valid → gagal start) |
| `--presence-on-connect` | `WHATSAPP_PRESENCE_ON_CONNECT` | `unavailable` |
| `--presence-pulse-enabled` | `WHATSAPP_PRESENCE_PULSE_ENABLED` | `true` |
| `--presence-pulse-interval`, `--presence-pulse-duration` | `WHATSAPP_PRESENCE_PULSE_INTERVAL`, `..._DURATION` | `24h`, `5m` |
| `--auto-reply` | `WHATSAPP_AUTO_REPLY` | — |
| `--auto-mark-read` | `WHATSAPP_AUTO_MARK_READ` | `false` |
| `--auto-reject-call` | `WHATSAPP_AUTO_REJECT_CALL` | `false` |
| `--auto-download-media` | `WHATSAPP_AUTO_DOWNLOAD_MEDIA` | `true` |
| `--account-validation` | `WHATSAPP_ACCOUNT_VALIDATION` | `true` |
| `--db-uri` | `DB_URI` | `file:storages/ruwa.db` (hanya sqlite di v1) |

CLI subcommand: `ruwa-gateway rest` (sama dengan GOWA v6+), `ruwa-gateway version`.

## 8. Strategi verifikasi kompatibilitas

1. **Contract test OpenAPI**: setiap route yang diimplementasikan divalidasi terhadap
   `docs/upstream/openapi.yaml` (request & response schema).
2. **Golden webhook test**: setiap contoh JSON di `webhook-payload.md` direproduksi dari
   fixture event whatsapp-rust → serialize → bandingkan (field-by-field, abaikan urutan).
3. **Differential test (manual)**: GOWA dan ruwa-gateway dijalankan berdampingan dengan dua akun
   test; request yang sama dikirim ke keduanya, response dan webhook dibandingkan.
4. Smoke test n8n node GOWA terhadap ruwa-gateway.

## 9. Risiko & keputusan terbuka

| # | Risiko | Mitigasi |
|---|---|---|
| R1 | Detail kontrak (nama field, kode error) melenceng | §8 contract/golden test sejak M1 |
| R2 | whatsapp-rust berubah cepat, API belum stabil | pin git rev, adapter `wa` tipis, update terjadwal |
| R3 | Migrasi sesi whatsmeow → whatsapp-rust | whatsapp-rust punya fitur `legacy-session-interop` untuk "migrating externally produced auth state" — riset terpisah (spike), default: scan ulang QR |
| R4 | Butuh Postgres | implementasi `Backend` trait sendiri, ditunda |
| R5 | Fitur media (kompresi video) butuh ffmpeg | opsional, deteksi saat runtime seperti GOWA |
| R6 | Pesan interactive/native-flow | eksperimental, M5 |
| R7 | ToS WhatsApp / ban akun | sama dengan GOWA; dokumentasikan |
