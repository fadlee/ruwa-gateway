# ruwa-gateway

REST API WhatsApp yang kompatibel dengan [GOWA](https://github.com/aldinokemal/go-whatsapp-web-multidevice),
ditulis dalam Rust di atas [whatsapp-rust](https://github.com/oxidezap/whatsapp-rust).

> Status: Fase 1–4 sebagian besar selesai di sisi kode; verifikasi dengan akun live masih berjalan.
> Lihat [docs/HANDOFF.md](docs/HANDOFF.md).

## Menjalankan

```sh
APP_HOST=127.0.0.1 cargo run -- rest
curl localhost:3000/health
```

Konfigurasi memakai nama env GOWA (`APP_PORT`, `APP_BASIC_AUTH`, `WHATSAPP_WEBHOOK`, …).
Lihat [docs/SPEC.md](docs/SPEC.md) §7.

## Docker

```sh
docker run -d --name ruwa -p 3000:3000 -v ruwa-data:/data \
  -e APP_BASIC_AUTH=admin:ganti-password \
  ghcr.io/<owner>/ruwa-gateway:latest
```

- `/data` menyimpan sesi (`storages/`) dan media (`statics/`); pakai volume bernama agar kepemilikan
  (uid 10001) benar.
- Server menolak start di host non-loopback tanpa `APP_BASIC_AUTH` (atau `APP_ALLOW_UNAUTHENTICATED=true`).
- Dashboard gowa-ui diunduh di latar belakang dan dicoba ulang otomatis bila jaringan belum siap; atur lewat
  `APP_UI_UPDATE_INTERVAL` (default `3h`), `APP_UI_GITHUB_TOKEN`, dan `APP_UI_ASSET_SHA256` (pin build).
- Image berisi ffmpeg (video, voice note, sticker). Healthcheck memakai `GET /health`.
- Build sendiri dari source: `docker build -t ruwa-gateway .`

## Rilis

Binary statis (musl, SQLite tertanam) untuk `amd64` dan `arm64` serta image multi-arch di GHCR dibuat
oleh `.github/workflows/release.yml` saat tag `vX.Y.Z` di-push (harus sama dengan `version` di
`Cargo.toml`). Build manual dari source: `cargo build --release --features bundled`; tanpa fitur
`bundled` binary menaut ke `libsqlite3` sistem. Monitoring: `GET /app/metrics` (memori proses/container,
ukuran `statics/media` dan `storages`, ruang disk, jumlah device).

## Dokumen

- [docs/SPEC.md](docs/SPEC.md): spesifikasi dan kontrak kompatibilitas
- [docs/PLAN.md](docs/PLAN.md): rencana per fase
- [docs/HANDOFF.md](docs/HANDOFF.md): status terkini dan serah-terima

## Disclaimer

Client tidak resmi, tidak berafiliasi dengan WhatsApp/Meta. Penggunaan dapat melanggar ToS WhatsApp
dan berisiko akun diblokir.
