# HANDOFF

> Dokumen serah-terima kerja. **Wajib diperbarui setiap selesai satu unit kerja**
> (fitur, fix, keputusan, atau sesi berakhir). Entri terbaru di atas.
> Baca ini dulu sebelum mulai kerja; lalu [PLAN.md](PLAN.md) dan [SPEC.md](SPEC.md).

## Status saat ini

- **Fase aktif**: Fase 1 (M1). Kode selesai, **tinggal uji manual dengan HP/akun test**.
- **Build**: `cargo build`, `cargo test` (50 test: 35 unit + 15 HTTP), `cargo clippy --all-targets` → hijau
- **Terverifikasi live** (tanpa akun): handshake ke server WhatsApp, `/app/login` → PNG QR valid
  dalam ~1 detik, login ulang, logout menghapus session, shutdown Ctrl+C bersih.
- **Belum terverifikasi live**: scan QR sampai paired, `/app/login-with-code`, kirim teks,
  webhook pesan masuk/receipt, `device.logged_out` saat unlink dari HP, restore session saat restart.
- **whatsapp-rust rev**: `94877bedb6cd473864f9f6b332a5c13d5fbef789`
- **GOWA acuan**: `3744c92a24fa0806c68df9bf6a34db4340016e10` (snapshot di `docs/upstream/`)

## Langkah berikutnya

1. **Uji manual M1** (butuh HP dengan akun test):
   ```sh
   # terminal 1: penerima webhook sederhana
   python3 -m http.server 9000   # atau https://webhook.site
   # terminal 2
   APP_HOST=127.0.0.1 WHATSAPP_WEBHOOK=http://127.0.0.1:9000/hook cargo run -- rest
   curl localhost:3000/app/login            # buka qr_link, scan dari HP
   curl localhost:3000/app/status           # is_logged_in: true, jid terisi
   curl -XPOST localhost:3000/send/message -H 'content-type: application/json' \
        -d '{"phone":"62xxx","message":"halo dari ruwa"}'
   ```
   Cek: pesan sampai di HP; balas dari HP → webhook `message`; receipt → `message.ack`;
   restart server → session pulih tanpa scan; unlink dari HP → webhook `device.logged_out`.
2. Setelah lolos: commit, centang item uji manual di PLAN, lanjut Fase 2 (registry diesel + `/devices/*`, media).

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
- Header signature webhook GOWA: `X-Hub-Signature-256: sha256=<hex>`; body JSON key tersortir (Go map).
- Event `message` tidak punya `timestamp` top-level; `message.ack` dan `device.*` punya.
- Fitur `legacy-session-interop` di whatsapp-rust kandidat untuk migrasi sesi whatsmeow (Fase 5 spike).
- Source referensi lokal (via `opensrc`):
  - GOWA: `~/.opensrc/repos/github.com/aldinokemal/go-whatsapp-web-multidevice/main`
  - whatsapp-rust (rev yang di-pin): `~/.cargo/git/checkouts/whatsapp-rust-*/94877be`
  - zapfast (pola multi-akun): `~/.opensrc/repos/github.com/crmne/zapfast/main`

## Log

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
