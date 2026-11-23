# HANDOFF

> Dokumen serah-terima kerja. **Wajib diperbarui setiap selesai satu unit kerja**
> (fitur, fix, keputusan, atau sesi berakhir). Entri terbaru di atas.
> Baca ini dulu sebelum mulai kerja; lalu [PLAN.md](PLAN.md) dan [SPEC.md](SPEC.md).

## Status saat ini

- **Fase aktif**: Fase 2 (M2) kode selesai. Fase 1+2 **belum diuji manual dengan HP** (ditunda atas
  permintaan user) → lanjut Fase 3 boleh, tapi uji manual tetap prioritas sebelum rilis.
- **Build**: `cargo test` (90 test: 67 unit + 23 HTTP; 1 test jaringan `#[ignore]`),
  `cargo clippy --all-targets` → hijau. Test video/PTT memakai ffmpeg sungguhan bila terpasang (di-skip bila tidak).
- **Terverifikasi live tanpa akun**: handshake + QR (`/app/login`, `/devices/{id}/login`), multi-device,
  registry bertahan setelah restart (termasuk webhook per device), link preview ke internet.
- **Belum terverifikasi live**: semua yang butuh akun ter-pair (kirim teks/media, upload ke CDN WhatsApp,
  webhook pesan masuk/media/receipt/presence, auto-download, `/user/*`, logout remote, temp ban).
- **whatsapp-rust rev**: `94877bedb6cd473864f9f6b332a5c13d5fbef789`
- **GOWA acuan**: `3744c92a24fa0806c68df9bf6a34db4340016e10` (snapshot di `docs/upstream/`)

## Langkah berikutnya

1. **Uji manual M1+M2 lewat gowa-ui** (paling mudah): `APP_HOST=127.0.0.1 cargo run -- rest` dari direktori kerja
   terpisah, buka `http://localhost:3000/` → kartu device → **Pair** → scan QR dari HP. Lalu menu **Messaging**
   untuk kirim teks/gambar/file/audio/video/kontak/lokasi/link, **Add device** untuk multi-device.
   Halaman Chats/Groups/Channels/sebagian Account akan error (endpoint Fase 3–4). **User sudah berhasil pair 2 akun dan kirim pesan lewat UI (2026-11-23).**
   Alternatif lewat curl (butuh HP dengan akun test):
   ```sh
   # terminal 1: penerima webhook (atau https://webhook.site)
   python3 -m http.server 9000
   # terminal 2 (jalankan dari direktori kerja terpisah; server menulis storages/ & statics/ di cwd)
   APP_HOST=127.0.0.1 WHATSAPP_WEBHOOK=http://127.0.0.1:9000/hook cargo run -- rest
   curl localhost:3000/app/login                      # buka qr_link, scan dari HP
   curl localhost:3000/app/status                     # is_logged_in: true
   curl -XPOST localhost:3000/send/message -H 'content-type: application/json' \
        -d '{"phone":"62xxx","message":"halo dari ruwa"}'
   curl -XPOST localhost:3000/send/image -F phone=62xxx -F caption=tes -F image=@foto.jpg
   curl -XPOST localhost:3000/send/audio -F phone=62xxx -F ptt=true -F audio=@suara.mp3
   curl "localhost:3000/user/check?phone=62xxx"
   ```
   Cek: pesan/media sampai di HP; balasan teks & gambar dari HP → webhook `message` (gambar tersimpan di
   `statics/media/`); receipt → `message.ack`; restart → session pulih; device kedua via
   `POST /devices` + `GET /devices/{id}/login`; unlink dari HP → `device.logged_out`.
2. Fase 3: chat storage (diesel) → `/chats`, `/chat/{jid}/*`, `/message/{id}/*`, `/group/*`, sisa `/user/*`,
   sticker/poll/presence, event grup/label/call. Chat storage juga menutup beberapa gap di bawah
   (nama kontak, reply setelah restart, default ephemeral).

## Keputusan yang sudah diambil

| Tanggal | Keputusan | Alasan |
|---|---|---|
| 2026-11-23 | whatsapp-rust di-pin via git rev, bukan crates.io | 0.7.0 di crates.io mengaktifkan `simd` (nightly) |
| 2026-11-23 | Storage aplikasi pakai **diesel**, bukan sqlx | whatsapp-rust membawa diesel + `libsqlite3-sys` 0.40; dua versi akan bentrok `links = "sqlite3"` |
| 2026-11-23 | Hanya `src/wa/` yang import `whatsapp_rust`; modul lain lewat `crate::wa::sdk` | update rev library terlokalisasi |
| 2026-11-23 | Chatwoot, MCP, Postgres, migrasi sesi whatsmeow di luar scope v1 | fokus drop-in REST + webhook dulu |
| 2026-11-23 | Slot per device di `storages/devices/<id>/` + marker file `jid`; hanya slot bermarker yang auto-connect saat start | slot belum paired tidak boleh menyalakan QR sendiri |
| 2026-11-23 | Tanpa slot sama sekali → slot `default` dibuat otomatis | pengganti instance transient GOWA; client single-device tidak perlu `X-Device-Id` |
| 2026-11-23 | Reply memakai cache memori 2000 pesan terakhir per device (`device/recent.rs`) | GOWA mengutip dari chat storage (Fase 3); sementara cukup id → (chat, sender, teks) |
| 2026-11-23 | Logout lokal men-set flag `local_logout` agar event `LoggedOut` tidak dikirim sebagai webhook | GOWA hanya mengirim `device.logged_out` untuk logout dari sisi WhatsApp |
| 2026-11-23 | `/app/login` selalu membuat bot baru (stop → start) | sama dengan GOWA (#851: QR channel lama mengganggu sesi baru) |
| 2026-11-23 | Gagal bind body → 500 `INTERNAL_SERVER_ERROR` | perilaku GOWA (panic di `Bind().Body` → Recovery) |
| 2026-11-23 | Registry device di `storages/ruwa.db` (diesel, migrasi embedded) menggantikan marker file `jid` Fase 1 | data slot + webhook per device perlu tempat; slot Fase 1 tanpa baris registry diabaikan (belum ada user) |
| 2026-11-23 | Direktori session = `storage_key` (id bila aman, selain itu `x`+hex) | id device GOWA bebas (mis. `../a`); cegah path traversal |
| 2026-11-23 | Request body media **tanpa** `#[serde(flatten)]` | flatten membuat form/multipart gagal parse angka/bool (`duration`, `is_forwarded`) — ada test |
| 2026-11-23 | Fetch URL dari user menolak IP non-publik di tiap hop redirect | setara `newUntrustedHTTPClient` GOWA; cegah SSRF |
| 2026-11-23 | gowa-ui disajikan di `{base}/` (di balik basic auth), diunduh dari release GitHub terbaru saat start, diverifikasi digest SHA-256, cache `storages/ui/index.html` (fitur M4 ditarik maju) | sarana uji manual; sama dengan GOWA v9 (`APP_UI_*`) |
| 2026-11-23 | `/ws` = hub broadcast global (tokio `broadcast`, kapasitas 64; klien lambat diputus) | sama dengan GOWA: tidak di-scope per device; integrasi pakai webhook |
| 2026-11-23 | `compress` gambar default `true` (JSON tanpa field / multipart), video default `false` | GOWA preset `Compress = true` hanya di handler image |

## Catatan / jebakan

- Cold build pertama ~4,5 menit (whatsapp-rust besar). Clippy cold ~2 menit.
- Server menolak start bila `APP_BASIC_AUTH` kosong dan host bukan loopback (perilaku GOWA).
  Untuk dev lokal pakai `APP_HOST=127.0.0.1`.
- Server menulis ke `storages/` dan `statics/` **relatif terhadap cwd**. Untuk eksperimen jalankan dari
  direktori lain (di-gitignore, tapi tetap rapi).
- Perbedaan perilaku dari GOWA yang disadari (belum ditutup):
  - `sender_display_name` sementara = push name / user part JID (GOWA pakai kontak tersimpan dulu) → Fase 3.
  - `duration` tidak dikirim bila tidak diisi; GOWA memakai default ephemeral chat dari storage → Fase 3.
  - Error 463 (reach-out timelock) belum dipetakan ke `WA_REACHOUT_TIMELOCK`; di whatsapp-rust 463 datang
    lewat ack server, bukan `SendError` → perlu riset.
  - `allow_reshare` divalidasi tapi flag reshare belum dipasang ke pesan status.
  - Status message `/send/message` memakai waktu lokal server (UTC), bukan timestamp ack server.
  - `qr_duration` = setengah timeout QR dari whatsapp-rust (hasil live: 29), GOWA/whatsmeow biasanya 30.
  - Link preview: hanya `jpegThumbnail` inline; GOWA juga meng-upload thumbnail resolusi tinggi (`MediaLinkThumbnail`).
  - Kirim media ke newsletter memakai upload biasa (GOWA: `UploadNewsletter`) → kemungkinan gagal; Fase 4.
  - Thumbnail dokumen hanya untuk file gambar (GOWA juga PDF).
  - `/user/info`: `name` kosong (butuh chat storage); daftar device dari usync device list.
  - `chat_presence`: WhatsApp hanya mengirim typing bila akun "online"; GOWA punya presence pulse
    (`presence-pulse-*`) yang belum diport → event typing mungkin jarang/tidak muncul.
  - Belum ada: `WHATSAPP_WEBHOOK_ALL_DEVICE_RECEIPTS`, `WHATSAPP_WEBHOOK_DEVICE_MERGE_GLOBAL`, batas ukuran
    via env (`WHATSAPP_SETTING_MAX_*`); konstanta default GOWA dipakai.
- Header signature webhook GOWA: `X-Hub-Signature-256: sha256=<hex>`; body JSON key tersortir (Go map).
- Event `message` tidak punya `timestamp` top-level; `message.ack` dan `device.*` punya.
- Fitur `legacy-session-interop` di whatsapp-rust kandidat untuk migrasi sesi whatsmeow (Fase 5 spike).
- Source referensi lokal (via `opensrc`):
  - GOWA: `~/.opensrc/repos/github.com/aldinokemal/go-whatsapp-web-multidevice/main`
  - whatsapp-rust (rev yang di-pin): `~/.cargo/git/checkouts/whatsapp-rust-*/94877be`
  - zapfast (pola multi-akun): `~/.opensrc/repos/github.com/crmne/zapfast/main`

## Log

### 2026-11-23 — WebSocket `/ws`
- `device/hub.rs` + `http/routes/ws.rs`: `{code,message,result}`; `FETCH_DEVICES` → `LIST_DEVICES`; event `LOGIN_SUCCESS`,
  `DEVICE_LOGGED_OUT` (lokal & remote), `DEVICE_REMOVED`, `DEVICE_TEMPORARY_BAN`, `DEVICE_STREAM_REPLACED`,
  `DEVICE_WEBHOOK_CONFIG_UPDATED`. Belum: `PASSKEY_*`, `DEVICE_REACHOUT_TIMELOCK`, cek origin browser (`CROSS_SITE_REQUEST_BLOCKED`).
- Test integrasi WS dengan server + tokio-tungstenite. Diverifikasi di headless Chrome: indikator live updates hijau.
- Uji manual user: pair 2 akun + kirim pesan via gowa-ui berhasil.
- Catatan: disk sempat penuh di tengah sesi (build gagal); sudah dibereskan user.

### 2026-11-23 — gowa-ui
- `src/ui.rs`: unduh `gowa-ui.html` (release latest `aldinokemal/gowa-ui`, cek digest), cache, sajikan di `/`; fallback HTML bila belum ada.
  Flag `APP_UI_ENABLED`, `APP_UI_AUTO_UPDATE`, `APP_UI_REPO`, `APP_UI_ASSET_NAME`. Belum: update berkala, pin SHA, `/ws`.
- `/app/info` menambah `max_file_size|max_video_size|max_image_size` (dibaca UI).
- Diverifikasi live: UI v1.9.0 terunduh (sha cocok), dirender headless Chrome, terhubung dan menampilkan device.

### 2026-11-23 — Fase 2 (M2) kode selesai
- 2a (`c913bab`): `storage/` (diesel + migrasi), registry device, `/devices/*` lengkap, webhook per device
  (aturan fallback GOWA), restore semua slot saat start; diuji live (2 slot, restart).
- 2b: `media/` (gambar: thumbnail/compress/HD; ffmpeg: thumbnail video, transcode, Opus PTT, waveform; fetch URL anti-SSRF;
  link preview og-tags; MIME), `/send/image|file|video|audio|contact|location|link`, ekstraktor JSON/form/multipart,
  `/user/check`, `/user/info`, webhook media (auto-download ke `statics/media/` atau `{url}`), kontak, lokasi,
  `chat_presence`, `device.temporary_ban`. Golden test baru untuk semua contoh tersebut di dokumen GOWA.

### 2026-11-23 — Fase 1 (M1) kode selesai
- `wa/`: port `SanitizePhone`/`ParseJID` GOWA, helper proto (`text`, `caption`, `context_info`, builder `extendedTextMessage`).
- `webhook/`: dispatcher multi URL, HMAC `X-Hub-Signature-256`, filter event (case-insensitive), ignore JIDs (`@suffix`), retry 5x backoff 1s→8s, timeout 10s.
- `events/`: payload `message`, `message.reaction|revoked|edited`, `message.ack` (+ aturan forward receipt), `device.logged_out`; LID→PN, rewrite `@lid` di body. Golden test membaca contoh JSON langsung dari `docs/upstream/webhook-payload.md`.
- `device/`: `DeviceManager` (bootstrap dari disk, resolve id/JID/default), `DeviceInstance` (start/stop bot, wipe session, logout), event pump.
- `usecase/`: login QR (PNG 512px, auto-hapus), login-with-code, logout, reconnect; `send_text` lengkap dengan validasi gaya ozzo, `MustLogin`, cek `is_on_whatsapp`, mentions + `@everyone`, reply, forwarded, duration.
- `http/`: `/health` teks di root, `/statics` tanpa auth, auth 401 teks polos ala Fiber, device scope, CORS, semua `/app/*`, `/send/message` (JSON/form).
- `docs/error-codes.md` dibuat; SPEC diperbarui (modul `usecase`/`error`, aturan auth/health/statics).

### 2026-11-23 — Fase 0 scaffold
- Analisa kelayakan GOWA vs whatsapp-rust vs zapfast → feasible.
- Tulis `docs/SPEC.md`, `docs/PLAN.md`, `docs/HANDOFF.md`; snapshot kontrak GOWA ke `docs/upstream/`.
- `git init`; scaffold crate: `config`, `http`, modul placeholder.
