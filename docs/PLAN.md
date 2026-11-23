# ruwa-gateway — Rencana Implementasi

Lihat [SPEC.md](SPEC.md) untuk kontrak. Dokumen ini urutan kerja dan definisi selesai per fase.

## Fase 0 — Fondasi (scaffold)

- [x] `git init`, `.gitignore`, `README.md`
- [x] Cargo project, pin `whatsapp-rust` git rev, toolchain stable
- [x] Modul kosong sesuai SPEC §3.1, `config` (clap + env), `tracing`
- [x] axum server: `/health`, envelope response `{code,message,results}`, error type
- [x] Middleware basic auth + aturan refuse-to-start
- [x] Snapshot kontrak upstream ke `docs/upstream/` (`openapi.yaml`, `webhook-payload.md`, `.env.example`) + catat commit GOWA
- [x] Ekstrak daftar kode error GOWA ke `docs/error-codes.md`

**Selesai bila**: `cargo run -- rest` menyala, `GET /health` OK, `cargo test` & `cargo clippy` hijau.

## Fase 1 — M1: satu device, kirim teks, webhook dasar

- [x] `DeviceManager` dengan satu slot default; `SqliteStore` per slot
- [x] `/app/login` (QR PNG di `/statics/qrcode/`), `/app/login-with-code`, `/app/logout`, `/app/reconnect`, `/app/status`, `/app/devices`, `/app/info` — QR terverifikasi live; pair code belum dites dengan HP
- [x] Middleware device scope (`X-Device-Id` / `device_id` / default tunggal)
- [x] Normalisasi JID (port `pkg/utils` GOWA) + unit test
- [x] `/send/message` (teks, reply, mentions, `@everyone`, duration) — belum dites kirim ke HP sungguhan
- [x] Event pump → `message`, `message.ack`, `device.logged_out` (+ `message.reaction|revoked|edited` lebih awal dari rencana M2)
- [x] Webhook dispatcher: multi URL, HMAC, filter event, ignore JIDs, retry
- [x] Golden test untuk contoh `message` (text, reply, mentions) & `message.ack`

- [ ] **Uji manual dengan akun test** (lihat HANDOFF): scan QR, kirim teks, terima webhook

**Selesai bila**: pairing via QR berhasil dengan akun test, kirim teks dari API sampai di HP,
pesan masuk memicu webhook dengan payload identik contoh GOWA.

## Fase 2 — M2: multi-device + media

- [x] Registry `storages/ruwa.db` (diesel migrations), tabel `devices`
- [x] Semua `/devices/*` termasuk webhook per device
- [x] Restore semua device saat startup
- [ ] Upload media: `/send/image|file|audio|video`, `/send/location|contact|link`
- [ ] `/user/check`, `/user/info`
- [ ] Event: `message.reaction|revoked|edited`, `chat_presence`, `device.temporary_ban`
- [ ] Payload media di webhook + auto-download media ke `storages/media/`

**Selesai bila**: dua akun aktif paralel, n8n node GOWA bisa kirim teks & gambar ke keduanya.

## Fase 3 — M3: chat storage, message ops, grup, user

- [ ] Chat storage (tabel meniru GOWA), isi dari pesan live + history sync
- [ ] `/chats`, `/chat/{jid}/*`
- [ ] `/message/{id}/*` (revoke, delete, reaction, update, read, played, star, forward, download)
- [ ] Semua `/group/*`
- [ ] Sisa `/user/*`, `/send/sticker|poll|presence|chat-presence`, `/call/reject`
- [ ] Event: `message.deleted`, `presence`, `group.*`, `label.*`, `call.offer`, `device.reachout_timelock`
- [ ] Auto reply, auto mark read, auto reject call

## Fase 4 — M4: newsletter, scheduler, UI

- [ ] `/newsletter/*` + event `newsletter.*`
- [ ] Scheduler persisten (`scheduled_sends`), `/send/schedules*`, retry dengan message id sama
- [ ] `/ws` hub, serve gowa-ui (download release + verifikasi SHA-256)
- [ ] `/app/metrics`
- [ ] Docker image + release binary (amd64/arm64)

## Fase 5 — Opsional

- [ ] Passkey login, `/send/interactive`
- [ ] Spike migrasi sesi whatsmeow (fitur `legacy-session-interop`)
- [ ] Backend Postgres
- [ ] MCP server

## Konvensi kerja

- Satu PR/commit per endpoint group; tiap endpoint wajib punya contract test.
- Adapter `wa/` adalah satu-satunya modul yang meng-import `whatsapp_rust` langsung,
  supaya update rev library terlokalisasi.
- Update rev whatsapp-rust: cek changelog, jalankan seluruh test + smoke test pairing.
