# HANDOFF

> Dokumen serah-terima kerja. **Wajib diperbarui setiap selesai satu unit kerja**
> (fitur, fix, keputusan, atau sesi berakhir). Entri terbaru di atas.
> Baca ini dulu sebelum mulai kerja; lalu [PLAN.md](PLAN.md) dan [SPEC.md](SPEC.md).

## Status saat ini

- **Fase aktif**: Fase 0 hampir selesai → lanjut Fase 1 (M1)
- **Build**: `cargo build`, `cargo test` (11 test), `cargo clippy --all-targets` → hijau
- **Jalan**: `APP_HOST=127.0.0.1 cargo run -- rest` → `GET /health` OK
- **whatsapp-rust rev**: `94877bedb6cd473864f9f6b332a5c13d5fbef789`
- **GOWA acuan**: `3744c92a24fa0806c68df9bf6a34db4340016e10` (snapshot di `docs/upstream/`)

## Langkah berikutnya

1. Ekstrak kode error GOWA (`src/pkg/error`, `src/ui/rest`) ke `docs/error-codes.md` (sisa Fase 0).
2. Fase 1: `device::DeviceManager` satu slot + `SqliteStore` di `storages/devices/<slot>/session.db`.
3. `/app/login` → event `PairingQrCode` → PNG di `statics/qrcode/scan-qr-<id>.png`.
4. Middleware device scope (`X-Device-Id` / `?device_id` / default tunggal).

## Keputusan yang sudah diambil

| Tanggal | Keputusan | Alasan |
|---|---|---|
| 2026-11-23 | whatsapp-rust di-pin via git rev, bukan crates.io | 0.7.0 di crates.io mengaktifkan `simd` (nightly) |
| 2026-11-23 | Storage aplikasi pakai **diesel**, bukan sqlx | whatsapp-rust membawa diesel + `libsqlite3-sys` 0.40; dua versi akan bentrok `links = "sqlite3"` |
| 2026-11-23 | Hanya `src/wa/` yang boleh import `whatsapp_rust` (via `wa::sdk`) | update rev library terlokalisasi |
| 2026-11-23 | Chatwoot, MCP, Postgres, migrasi sesi whatsmeow di luar scope v1 | fokus drop-in REST + webhook dulu |
| 2026-11-23 | `Command::Rest(Box<Config>)` | clippy `large_enum_variant` |

## Catatan / jebakan

- Cold build pertama ~4,5 menit (whatsapp-rust besar). Build incremental cepat.
- Server menolak start bila `APP_BASIC_AUTH` kosong dan host bukan loopback (perilaku GOWA).
  Untuk dev lokal pakai `APP_HOST=127.0.0.1`.
- Header signature webhook GOWA: `X-Hub-Signature-256: sha256=<hex>` (terverifikasi di `webhook.go:55`).
- Fitur `legacy-session-interop` di whatsapp-rust kandidat untuk migrasi sesi whatsmeow (Fase 5 spike).
- Source referensi lokal (via `opensrc`):
  - GOWA: `~/.opensrc/repos/github.com/aldinokemal/go-whatsapp-web-multidevice/main`
  - whatsapp-rust: `~/.opensrc/repos/github.com/oxidezap/whatsapp-rust/main`
  - zapfast (pola multi-akun): `~/.opensrc/repos/github.com/crmne/zapfast/main`

## Log

### 2026-11-23 — Fase 0 scaffold
- Analisa kelayakan GOWA vs whatsapp-rust vs zapfast → feasible.
- Tulis `docs/SPEC.md`, `docs/PLAN.md`, `docs/HANDOFF.md`; snapshot kontrak GOWA ke `docs/upstream/`.
- `git init`; scaffold crate: `config` (flag/env GOWA), `http` (envelope, basic auth + `?authorization=`,
  base path, fallback 404), `/health`, `/app/info`, modul placeholder `device|wa|events|webhook|storage|media`.
- Test: 5 unit (config) + 6 integration (`tests/http.rs`).
