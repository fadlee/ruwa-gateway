# ruwa-gateway

REST API WhatsApp yang kompatibel dengan [GOWA](https://github.com/aldinokemal/go-whatsapp-web-multidevice),
ditulis dalam Rust di atas [whatsapp-rust](https://github.com/oxidezap/whatsapp-rust).

> Status: tahap awal (Fase 0). Lihat [docs/HANDOFF.md](docs/HANDOFF.md).

## Menjalankan

```sh
APP_HOST=127.0.0.1 cargo run -- rest
curl localhost:3000/health
```

Konfigurasi memakai nama env GOWA (`APP_PORT`, `APP_BASIC_AUTH`, `WHATSAPP_WEBHOOK`, …).
Lihat [docs/SPEC.md](docs/SPEC.md) §7.

## Dokumen

- [docs/SPEC.md](docs/SPEC.md): spesifikasi dan kontrak kompatibilitas
- [docs/PLAN.md](docs/PLAN.md): rencana per fase
- [docs/HANDOFF.md](docs/HANDOFF.md): status terkini dan serah-terima

## Disclaimer

Client tidak resmi, tidak berafiliasi dengan WhatsApp/Meta. Penggunaan dapat melanggar ToS WhatsApp
dan berisiko akun diblokir.
