# HANDOFF

> Dokumen serah-terima kerja. **Wajib diperbarui setiap selesai satu unit kerja**
> (fitur, fix, keputusan, atau sesi berakhir). Entri terbaru di atas.
> Baca ini dulu sebelum mulai kerja; lalu [PLAN.md](PLAN.md) dan [SPEC.md](SPEC.md).

## Status saat ini

- **Fase aktif**: Fase 4 — **scheduler selesai** (`scheduled_sends`, worker, `/send/schedules*`, semua `/send/*` menerima `scheduled_at` dst.; termasuk forward terjadwal) dan `/newsletter/follow|unfollow|info|messages|messages/{id}/download` selesai + `live-updates` & event `newsletter.message` (sisa: message-updates, directory, event `newsletter.joined|left|mute`). Sebelumnya: Fase 3 — chat storage, `/chats`, `/chat/{jid}/*`, `/message/{id}/*`, `/group/*`, `/user/my/groups|privacy|contacts[/detailed]`, `/user/pushname|avatar|presence/subscribe` selesai; event grup `group.participants|info|joined` selesai; `/send/sticker|presence|chat-presence|poll` selesai; `/user/*`, `/call/reject`, `call.offer` + auto-reject selesai; auto reply + auto mark read selesai; event `presence` + presence on-connect/pulse selesai; semua event webhook Fase 3 + error 463 selesai → **Fase 3 tuntas (kode)**; berikutnya verifikasi live, lalu Fase 4.
- **Rilis**: versi crate `0.1.0-rc.2` (rc.1 sudah dirilis dan workflow terbukti hijau di GitHub: CI, tarball amd64+arm64, image GHCR); `Dockerfile` (+ `.dockerignore`), `.github/workflows/ci.yml` dan `release.yml` ada; image sudah dibangun & di-smoke-test lokal (health, `/app/metrics`, SIGTERM 1 dtk, ffmpeg, uid 10001; 258MB). Image arm64 belum pernah dijalankan.
- **Build**: `cargo test` (239 test: 188 unit + 51 HTTP/WS; 1 test jaringan `#[ignore]`),
  `cargo clippy --all-targets` → hijau. Test video/PTT memakai ffmpeg sungguhan bila terpasang (di-skip bila tidak).
- **Terverifikasi manual oleh user (2026-11-23, via gowa-ui)**: pair 2 akun (QR), kirim pesan teks.
- **Terverifikasi live oleh agent (tanpa akun)**: handshake + QR, multi-device + restart, link preview ke internet,
  gowa-ui terunduh & dirender, indikator `/ws` hijau di headless Chrome.
- **Belum terverifikasi live**: kirim media (upload ke CDN WhatsApp), webhook pesan masuk/media/receipt/presence,
  auto-download, `/user/*`, logout dari HP (`device.logged_out`), temp ban, **chat storage** (history sync setelah
  pair, pesan masuk/keluar tersimpan, reply setelah restart), `/chat/{jid}/pin|archive|disappearing|history`
  (app-state/protocol ke WhatsApp), nama dari `ContactUpdate`, **backfill nama** (nama grup placeholder terisi, replay kontak `CriticalUnblockLow`), semua `/message/{id}/*` (revoke/edit/reaction/read/
  played/star/delete-for-me/forward/download), **semua `/group/*`, `/user/my/groups|privacy|contacts[/detailed]` `/user/pushname`, `/user/avatar` dan `/user/presence/subscribe`** (butuh akun yang ada di grup; admin untuk
  name/locked/announce/topic/photo/participants). Cek via gowa-ui menu Chats/Groups atau `GET /chats`.
- **whatsapp-rust rev**: `94877bedb6cd473864f9f6b332a5c13d5fbef789`
- **GOWA acuan**: `3744c92a24fa0806c68df9bf6a34db4340016e10` (snapshot di `docs/upstream/`)

## Langkah berikutnya

0. **Jalankan [MANUAL-TEST.md](MANUAL-TEST.md)** (checklist verifikasi live, 9 bagian) sebelum Fase 4; salin temuan ke "Catatan / jebakan".
1. **Uji manual M1+M2 lewat gowa-ui** (paling mudah): `APP_HOST=127.0.0.1 cargo run -- rest` dari direktori kerja
   terpisah, buka `http://localhost:3000/` → kartu device → **Pair** → scan QR dari HP. Lalu menu **Messaging**
   untuk kirim teks/gambar/file/audio/video/kontak/lokasi/link, **Add device** untuk multi-device.
   Halaman Channels/sebagian Account akan error (endpoint Fase 3–4); Chats dan Groups kini tersedia. **User sudah berhasil pair 2 akun dan kirim pesan lewat UI (2026-11-23).**
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
2. Fase 4 tinggal newsletter `message-updates|directory` + event `newsletter.joined|left|mute` (lihat catatan newsletter). Rilis stabil: naikkan `version` ke `0.1.0` lalu push tag `v0.1.0`. Fase 3 tuntas di sisi kode. Verifikasi live (daftar "Belum terverifikasi" di atas), lalu Fase 4 (newsletter, scheduler), auto reply/mark read/reject call.

## Keputusan yang sudah diambil

| Tanggal | Keputusan | Alasan |
|---|---|---|
| 2026-11-24 | Update gowa-ui jalan sebagai loop: cek pertama saat start; gagal → ulang 30 dtk, ×2, maks 30 mnt (dan tak melebihi interval); sukses → tiap `APP_UI_UPDATE_INTERVAL` (3h) ±10%. Log gagal memakai rantai error penuh (`{error:#}`) + URL | sebelumnya sekali coba saat start, kegagalan sesaat = tak ada dashboard sampai restart, dan pesan error tak menyebut penyebab |
| 2026-11-24 | `APP_UI_ASSET_SHA256` (tolak digest rilis/unduhan/cache yang beda; menerima awalan `sha256:`, huruf besar), `APP_UI_GITHUB_TOKEN` (bearer ke API + unduhan) | sama dengan GOWA `uiasset`; tanpa ETag kondisional (cukup bandingkan digest sebelum mengunduh) |
| 2026-11-24 | Rilis = binary statis musl (`--features bundled`: SQLite tertanam via `whatsapp-rust/sqlite-storage-bundled`) untuk amd64+arm64, dibangun **native** di runner masing-masing (`ubuntu-24.04`, `ubuntu-24.04-arm`); image multi-arch merakit binary itu lewat target Dockerfile `prebuilt` (tanpa kompilasi di bawah QEMU). `docker build .` biasa memakai target `source` (kompilasi di dalam) | build dev tanpa `bundled` menaut `libsqlite3` sistem (dicek `ldd`); QEMU untuk compile arm64 ~jam-an |
| 2026-11-24 | Tag harus sama dengan `version` di `Cargo.toml` (job `version` menolak bila beda); tag berisi `-` = pre-release (GitHub prerelease, tanpa tag `latest` di GHCR) | cegah binary berlabel salah |
| 2026-11-24 | Image: alpine + ffmpeg + tini, user uid 10001, `WORKDIR /data` (volume: `storages/` + `statics/` relatif cwd), `HEALTHCHECK` ke `/health` | path data server relatif ke cwd |
| 2026-11-24 | Server menangani SIGTERM (selain Ctrl-C) → `docker stop` menghentikan device dengan rapi | sebelumnya hanya SIGINT, PID 1 mengabaikan SIGTERM |
| 2026-11-24 | `/app/metrics` mempertahankan semua key GOWA; arti di Rust: `go_version` = versi rustc, `heap_alloc_bytes` = byte hidup dari `CountingAlloc` (global allocator di `main.rs`), `runtime_sys_bytes` = `VmSize`, `goroutines` = task tokio hidup, `gc_cycles` = 0; `session_store` selalu `sqlite`; `started_at` diinisialisasi di awal `main` | klien (gowa-ui) membaca key GOWA; Rust tak punya GC |
| 2026-11-24 | Scheduler: validasi payload memakai **jalur kirim yang sama** dengan mode "dry run" (`DRY_RUN` task-local; `must_login` mengembalikan penanda `SCHEDULE_DRY_RUN` setelah validasi lolos) | satu sumber validasi dengan kirim langsung; tanpa menduplikasi `ValidateSend*` GOWA |
| 2026-11-24 | Identitas occurrence di-pin lewat `PINNED` task-local (`message_id` + poll secret) dan `deliver` memakai `SendOptions::with_message_id` | retry/pemulihan crash tidak membuat pesan kedua (GOWA `ContextWithSendMessageID`) |
| 2026-11-24 | Media jadwal disimpan di `storages/scheduled/<id>/`, bukan `statics/media/scheduled/` | GOWA menaruhnya di folder yang disajikan publik lewat `/statics` |
| 2026-11-24 | Waktu `scheduled_sends` = unix detik (BIGINT); zona waktu IANA lewat `chrono-tz`; celah DST didorong maju (`02:30` NY → `03:30`), overlap pakai instan pertama | port `occurrenceOn` GOWA |
| 2026-11-24 | Opsi jadwal dibaca dari body yang sama oleh extractor `Payload` hanya bila body memuat `scheduled_at` | field jadwal tak pernah menggagalkan request yang tidak dijadwalkan; request tipe tak ditambah 7 field |
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
| 2026-11-23 | Chat storage di `storages/ruwa.db` (migrasi `000002_chat_storage`); partisi `device_id` = JID akun (non-AD), fallback id slot | sama dengan GOWA (`deviceIDFromContext`) |
| 2026-11-23 | Waktu di chat storage disimpan unix detik (BIGINT), bukan TIMESTAMP teks GOWA | internal; konversi ke RFC3339 di layer HTTP |
| 2026-11-23 | Hanya query device-scoped yang diport (tanpa `GetChat`/`GetMessageByID` global GOWA) | AGENTS.md GOWA melarang lookup global untuk flow user |
| 2026-11-23 | Cache `device/recent.rs` dihapus; reply mengutip dari chat storage | GOWA `mergeReplyContext` |
| 2026-11-23 | `/send/message` tanpa `duration` mengirim `expiration` = timer chat tersimpan (atau 0) | GOWA `getDefaultEphemeralExpiration` (selalu set field) |
| 2026-11-23 | Tabel chatwoot/poll/scheduled belum dibuat | dibuat saat fiturnya (poll Fase 3, schedule Fase 4; chatwoot di luar scope) |
| 2026-11-23 | Tabel `contacts` sendiri (migrasi `000003`) sebagai pengganti contact store whatsmeow | whatsapp-rust tidak menyimpan kontak, hanya event `ContactUpdate`; GOWA memakai contact store untuk nama chat & sender |
| 2026-11-23 | Kebijakan nama di `usecase/names.rs` (fungsi murni) dipakai `/chats`, `/chat/*/messages`, dan `sender_display_name` webhook | satu sumber, port `ChatDisplayNameResolver` + `SenderDisplayNameResolver` |
| 2026-11-23 | Endpoint `/chat/*`: body gagal bind → 400 `BAD_REQUEST` `Invalid request body`; query tak valid → default | perilaku handler GOWA `ui/rest/chat.go` + Fiber `Query[T]` (beda dengan `/send/*`) |
| 2026-11-23 | Timestamp response chat = RFC3339 UTC; unix 0 → `0001-01-01T00:00:00Z` | GOWA `formatTimestamp` (zero time); `APP_TIMEZONE` belum didukung |
| 2026-11-23 | `/message/*/delete` & `star`: `from_me`/author diambil dari salinan tersimpan, heuristik GOWA (id > 22 char = bukan dari saya) hanya bila tidak tersimpan | heuristik GOWA salah untuk id pendek dari klien lain; index app-state mengikuti whatsapp-rust (WA Web) |
| 2026-11-23 | Read/played receipt 1:1 tanpa `participant` | API whatsapp-rust (`sender` hanya untuk grup); whatsmeow juga mengabaikan sender di DM |
| 2026-11-23 | `/group/participants/{add,remove,promote,demote}`: error per peserta selain 403+`add_request` dilaporkan `status:"error"` (`Action failed (code N)`) | GOWA melaporkan semua itu `success` (menyesatkan); kasus 403 tetap persis GOWA |
| 2026-11-23 | `/group/*` memakai `Body`/`Payload` (gagal bind → 500) dan `GET` list peserta/request/export menolak `group_id` kosong dengan 400 `INVALID_GROUP_ID` sebelum cek login | perilaku handler GOWA `ui/rest/group.go` |
| 2026-11-23 | Peserta di body (`participants`) selalu ditempeli `@s.whatsapp.net` dan dicek `is_on_whatsapp` tanpa memedulikan `WHATSAPP_ACCOUNT_VALIDATION`; tak terdaftar → 400 `INVALID_JID` `user is not registered` | GOWA `participantToJID` |
| 2026-11-24 | `/user/my/contacts*` membaca tabel `contacts` kita (pengganti contact store whatsmeow), diurutkan per JID (GOWA: urutan map Go acak); `/user/my/privacy` selalu mengosongkan `last_seen` seperti GOWA | tabel `contacts` terisi dari `ContactUpdate` + replay kontak, jadi daftarnya bergantung backfill |
| 2026-11-24 | Nama grup placeholder (`Group <id>`) diisi antrean di `device/backfill.rs`: `fetch_metadata_batch` 20 grup/5 dtk, backoff 30 dtk ×2 (7 percobaan), `Forbidden`/`NotFound`/tanpa subjek final, rename hanya bila nama masih placeholder (`rename_chat_if`). Pemicu: `Connected`, selesai history sync, pertama kali grup terlihat di pesan | pola zapfast `pump_group_info`; burst query kena rate limit server dan grup tersangkut bernama "Group" |
| 2026-11-24 | Kontak diputar ulang dari HP sekali per link lewat `resync_app_state([CriticalUnblockLow], Snapshot)`; marker file `contacts-resynced` di direktori slot (ditulis saat `PairSuccess` karena link baru sudah menerima kontak, dihapus di `wipe_session`) | pola zapfast `contact_names`; HP hanya mengirim kontak saat berubah, jadi sesi lama tak punya kontak |
| 2026-11-24 | `/user/my/groups` = `list_participating` (id) + `fetch_metadata_batch` (chunk 200) → `GroupInfo` lengkap, 2 IQ berapa pun jumlah grup; tanpa grup → `data: null` | whatsmeow `GetJoinedGroups` mengembalikan peserta penuh; `list_participating` SDK hanya overview tanpa peserta, per-grup `fetch_metadata` = N IQ (risiko rate limit) |
| 2026-11-23 | Foto grup: validasi (tipe/ukuran ≤10MB/format) → 400 `INVALID_IMAGE_FORMAT`; diproses crop persegi tengah, ≤640px, JPEG q80; tanpa file `photo` = hapus foto | GOWA `ValidateGroupPhotoFormat` + `ProcessGroupPhoto` |

## Catatan / jebakan

- `/app/metrics` (`src/sysmetrics.rs`): folder `media` (= `statics/media`) dan `storages` dijalankan walk di `spawn_blocking` dan di-cache 1 menit **tanpa memedulikan** daftar folder (seperti GOWA); symlink dihitung ukuran dirinya, tak diikuti. Nilai `null` bila tak tersedia (non-Linux, limit cgroup `max`). Envelope kita tak punya field `status` (hanya `code`/`message`/`results`, sama untuk semua endpoint). Clippy `unnecessary_cast` di `statvfs` di-allow demi portabilitas tipe.
- CI memakai toolchain `1.97` (sama dengan `rust:1.97-alpine`); naikkan keduanya bersamaan. CI memakai `clippy -D warnings`. Job `musl-build` di CI membangun profil rilis agar kerusakan musl ketahuan di PR. Build Docker dari nol ~12 menit.
- `ubuntu-24.04-arm` gratis hanya untuk repo publik; repo privat butuh runner arm berbayar atau ganti ke QEMU.
- Scheduler (`usecase/schedule.rs`, `schedule_time.rs`, `schedule_dispatch.rs`, `storage/schedule.rs`): worker tiap 5 dtk (`Worker<Backend>`; `DeviceBackend` untuk produksi, backend palsu untuk test) — pulihkan lease kedaluwarsa (120 dtk; occurrence yang sudah di-pin dikenai 1 percobaan), lalu klaim satu per satu job `active` yang jatuh tempo. Percobaan maks 10; retry `15 dtk · 2^percobaan` (tanpa kenaikan, jadi 15, 30, 60 … maks 5 mnt); device offline → coba lagi 30 dtk **tanpa** biaya percobaan; validasi/payload rusak → `failed` langsung; kehabisan percobaan → job berulang **melewati** occurrence itu (slot berikut, jatah baru), job sekali jalan `failed`; `end_at` lewat → `completed` tanpa kirim. `resume`: job sekali jalan yang terlambat dikirim segera, job berulang lanjut di slot berikut (id pin lama dibuang) atau `completed`. Beda dari GOWA: kolom `legacy_attempt_unknown` tidak ada (tak ada baris lama); `/send/chat-presence` dengan field jadwal → 400 `chat presence cannot be scheduled`; `send/presence` tak dijadwalkan. `deliver` kini menunggu ack ≤5 dtk (lihat catatan nack) sehingga satu occurrence bisa lebih lama — masih jauh di bawah batas 120 dtk. Hapus device (`DELETE /devices/{id}`) menghapus jadwal & medianya; media yatim dibersihkan saat start. `phone` jadwal tersimpan seperti GOWA setelah `sanitize_phone` (`+62…@s.whatsapp.net`).
- Forward terjadwal: `ForwardRequest` kini membawa `message_id` (diisi dari path, menimpa body) agar payload tersimpan mandiri; `validate_scheduled_forward` (wajib `message_id`/`phone`, nomor, `duration`, pesan ada di storage device → kalau tidak **400** `message with ID X not found`, bukan 500 seperti forward langsung; tipe tak didukung → 400) dijalankan saat membuat **dan** saat jatuh tempo (sumber terhapus → `failed` permanen). Cek jadwal (`timezone`, dst.) dijalankan sebelum cek storage. Summary `Forwarded message`.
- Belum di scheduler: wake-up segera saat job baru jatuh tempo (worker polling 5 dtk), metrik.
- `cargo test`/`clippy` penuh bisa gagal `ld … signal 9` (kehabisan memori); pakai `-j 1`. Kompilasi ulang `-j 1` ~4–8 menit — jalankan di latar belakang.
- Newsletter live-updates (`device/live_updates.rs`): langganan di memori per device (hilang saat restart), diperpanjang di 80% durasi grant (min 5 dtk), gagal → backoff 30s×2 maks 5m dan tak melewati kedaluwarsa; `Connected` memicu renew semua; logout/hapus device (`wipe_session`) menghentikan semua; akun slot berganti → langganan dihapus. `GET` selalu array terurut; `DELETE` tanpa langganan → 400 `no live updates subscription for …`. Beda dari GOWA: tak ada penanda "pending" saat subscribe bersamaan unsubscribe. Event `newsletter.message`: hanya counter (`server_id`, `forwards_count>0`, `reaction_counts`); `views_count`/`message_id`/`type`/`timestamp` tak ada di event SDK; timestamp = waktu proses. **Event `newsletter.joined|left|mute` tidak dikirim** — SDK tidak punya event-nya (whatsmeow `NewsletterJoin/Leave/MuteChange`); bisa ditiru hanya untuk aksi lewat API, tapi tidak untuk aksi dari HP.
- Newsletter: `follow`/`unfollow` = `newsletter().join/leave`; `info` lewat id atau `invite` (awalan `https://whatsapp.com/channel/` dibuang); `messages` = `get_messages(count, before)`; `download` mengambil 1 pesan (`before = server_id+1`) lalu `client.download` — **belum pasti** SDK mengunduh media saluran tanpa `media_key` (plaintext) dengan benar, uji live. `file_url` selalu ada (path `statics/...`). Validasi: `newsletter_id` wajib + harus berakhiran `@newsletter` (unfollow hanya wajib), `count` 1–100 (default 50), `before` ≥ 0, int query tak valid → 500 (Fiber). Belum ada: `message-updates` (SDK tak punya IQ-nya; perlu IQ sendiri), `directory` (query WA Web tak terdokumentasi), `live-updates` (+ renew 90 dtk) dan event `newsletter.*`. Fallback info tanpa viewer metadata (405 untuk saluran yang tak diikuti) belum ada — bergantung perilaku `get_metadata`. `status` pesan `/send/*` ke saluran masih upload biasa.
- Nack kirim (463 dst.): whatsapp-rust kembali dari `send` sebelum server menjawab; jawabannya datang sebagai `Event::ServerAck` (`class=message`, `error=Some(code)`). `device/acks.rs` (`AckBook`, 512 id terakhir per device) menyimpannya dan `usecase::send::deliver` menunggu verdict id-nya **maks 5 dtk** (whatsmeow menunggu ack penuh): `463` → 429 `WA_REACHOUT_TIMELOCK` (teks GOWA), kode lain → 500 `server returned error N`; nack → pesan **tidak** disimpan. Tanpa ack dalam 5 dtk → dianggap sukses. Efek samping: tiap kirim kini menunggu ack (biasanya ratusan ms). Ack juga membawa timestamp server (belum dipakai untuk `status` kirim — gap lama).
- `device.reachout_timelock`: `ends_at` dari `time_enforcement_ends` (detik unix, string), dikirim juga ke WS `DEVICE_REACHOUT_TIMELOCK`.
- Label: `label.edit` dari `LabelEditUpdate`, `label.association` dari `LabelAssociationUpdate` (hanya level chat; per-pesan `MessageLabelAssociationUpdate` sengaja tak diteruskan, sama dengan GOWA). Hanya field yang dikirim WhatsApp yang muncul. `predefined_id` dikirim sebagai **angka** (int32, perilaku kode GOWA) walau contoh dokumen menulis string `"1"`. `timestamp` = `action_timestamp` bila ada, selain itu waktu proses.
- `message.deleted` (dari `DeleteMessageForMeUpdate`): seperti GOWA, hanya dikirim bila salinan pesan ada di storage device itu (dicari per id, lalu baris dihapus); tanpa `timestamp` top-level (ada di payload, = waktu proses). `from` = peserta bila disebut, selain itu akun sendiri bila `from_me`, selain itu chat (SDK tak punya `SenderJID` whatsmeow persis). `chat_id` = chat tersimpan, `chat_lid` bila event menyebut chat dengan LID.
- Presence sendiri (`device/presence.rs`): saat `Connected` dikirim `WHATSAPP_PRESENCE_ON_CONNECT` (`available`/`unavailable`/`none`, default `unavailable`, nilai lain = unavailable seperti GOWA). Pulse (default aktif): tiap menit tiap device yang connected & login dicek; bila ≥ `WHATSAPP_PRESENCE_PULSE_INTERVAL` (24h) sejak pulse sukses terakhir → available, tidur `..._DURATION` (5m), lalu unavailable. Pulse gagal dicoba lagi pada cek berikutnya. Durasi memakai subset `time.ParseDuration` Go (`h m s ms`, bisa digabung); nilai 0/negatif **ditolak** saat start (GOWA diam-diam memakai default). Efek samping (dari dokumen GOWA): akun *available* menekan notifikasi di HP — makanya default `unavailable`. Pulse jalan per proses, state `last` hanya di memori (restart → pulse langsung).
- Event `presence`: `timestamp` = waktu proses (GOWA `time.Now()`), `last_seen` hanya bila SDK memberi nilai. WhatsApp hanya mengirimnya untuk kontak yang sudah di-subscribe (`/user/presence/subscribe`) dan selama akun berstatus *available*; presence pulse/`WHATSAPP_PRESENCE_ON_CONNECT` belum ada, jadi event bisa jarang muncul (lihat catatan `chat_presence`).
- Auto reply/mark read ada di `device/processor.rs` (`auto_reply`, `auto_mark_read`), dijalankan setelah pesan masuk disimpan dan sebelum webhook (urutan GOWA). Auto-reply hanya untuk chat 1:1 (`@s.whatsapp.net`/`@lid`), teks ketikan asli (bukan caption; edit teks dihitung), bukan pesan sendiri; balasan dikirim ke `sender` dan disimpan ke chat storage. Mark read memakai waktu sekarang dan, di grup, menyebut pengirim. Balasan otomatis tidak memakai chat-presence/jeda apa pun; tidak ada pencegahan loop selain filter di atas (dua akun dengan auto-reply saling membalas bisa berputar — GOWA sama).
- Call: `call.offer` hanya dari `CallAction::Offer` (`MissedCall` dari offline queue tidak dikirim; GOWA/whatsmeow juga hanya `CallOffer`). `from` = `call_creator`; auto-reject memakai `voip().reject_call(call_id, from, creator)` dengan timeout 10 dtk, gagal → `auto_rejected:false`. `/call/reject` memakai caller_jid sebagai `peer` dan `creator` (SDK memisahkan keduanya untuk companion-device). Panggilan masuk belum disimpan sebagai baris `media_type = call`.
- `/user/business-profile`: jam buka `HH:MM` dihitung dari **menit** sejak tengah malam (540 → `09:00`); GOWA membagi 100 (`FormatBusinessHourTime`) sehingga 540 jadi `05:40` — kami sengaja tidak meniru bug itu. Waktu kosong → `00:00` (nilai nol Go). `profile_options` berisi `description`/`website` (SDK tak mengekspos flag commerce whatsmeow), `null` bila kosong. Bukan akun bisnis → 500 `business profile not found`.
- `/user/my/newsletters`: JSON meniru skema `Newsletter` OpenAPI; field yang tak diekspos SDK (`name.id`, `update_time`, `picture.id/direct_path`, `reaction_codes`) berupa string kosong. Jumlah pelanggan kosong diisi `get_metadata` per saluran (batch 5, timeout 5 dtk; GOWA sliding window). Tanpa saluran → `data: null`.
- Poll: pesan dibangun sendiri (SDK `polls().create` tak bisa menambah `expiration`): v3 bila `max_answer == 1`, v1 selain itu, `message_secret` 32 byte acak, dikirim lewat `deliver`. Belum: tabel `poll_definitions` (GOWA menyimpannya untuk meresolusi vote) dan event/endpoint vote; batas SDK (2–12 opsi) tidak divalidasi di sisi kita — server yang menolak.
- Sticker: WebP animasi dikenali dari header (VP8X flag, tanpa `webpmux`) → wajib 512x512 & <500KB, dikirim apa adanya; selain itu didecode (GIF = frame pertama), dipas ke 512x512, dikonversi WebP lossy q60 lewat ffmpeg (GOWA sama), atau **lossless** lewat crate `image` bila ffmpeg tak ada (file lebih besar; GOWA gagal 500 tanpa ffmpeg/cwebp). WebP animasi invalid-dimensi → 400. `/send/presence` dengan `type` kosong → 400 (GOWA lolos validasi lalu gagal di whatsmeow). Chat-presence tidak ada media (`composing`/`paused` saja).
- Event grup: `Add` → `group.participants` join (+ `group.joined` bila akun sendiri ada di daftar), `Remove` → leave, `Promote`/`Demote`; `Subject` → `group.info` + nama chat tersimpan diganti (`rename_chat`, hanya bila chat sudah ada). Belum: `Create` (raw node) tidak memicu `group.joined`; `reason` hanya `invite`/kosong (whatsmeow punya jenis lain); `Modify`, description, locked/announce tidak dikirim (GOWA juga tidak). `group.joined` dikirim berdampingan dengan `group.participants` untuk akun sendiri — belum dicek apakah whatsmeow melakukan hal sama.
- Cold build pertama ~4,5 menit (whatsapp-rust besar). Clippy cold ~2 menit.
- Server menolak start bila `APP_BASIC_AUTH` kosong dan host bukan loopback (perilaku GOWA).
  Untuk dev lokal pakai `APP_HOST=127.0.0.1`.
- Server menulis ke `storages/` dan `statics/` **relatif terhadap cwd**. Untuk eksperimen jalankan dari
  direktori lain (di-gitignore, tapi tetap rapi).
- Perbedaan perilaku dari GOWA yang disadari (belum ditutup):
  - Tabel `contacts` hanya terisi dari `ContactUpdate` (app state), push name pesan/history, dan verified name —
    whatsmeow juga mengisi dari sumber lain (mis. `contacts` history sync, usync). Nama bisa lebih jarang terisi.
  - Nama chat 1:1 dari history sync memakai `displayName` percakapan saat ingest; GOWA lebih dulu tanya contact store
    (di read path, `/chats` tetap memakai contact store bila nama tersimpan berupa nomor/JID).
  - `APP_TIMEZONE` (GOWA `AppLocation`) belum ada; timestamp selalu UTC.
  - Pesan error parse `start_time`/`end_time` meniru format Go tetapi detail error-nya dari chrono.
  - Teks chat storage belum meringkas pesan bisnis (template/interactive/list/buttons/product/order) — GOWA ya.
  - Panggilan masuk belum disimpan sebagai baris `media_type = call` (`CreateIncomingCallRecord`) → bersama `call.offer`.
  - History sync tidak ditulis ke file JSON `storages/history-*.json` seperti GOWA (hanya ke DB).
  - Media kedaluwarsa (403/404/410) tidak diminta ulang dari HP (`ExpiredMediaRecovery`, `UpdateMessageMedia`) —
    download/forward re-upload gagal dengan pesan error biasa.
  - `/message/*/download`: `file_url` selalu `http://<Host>`; GOWA memakai skema Fiber (https bila TLS/proxy tepercaya).
  - Forward: field jadwal (`scheduled_at`, dll.) diabaikan → Fase 4.
  - `allow_reshare` divalidasi tapi flag reshare belum dipasang ke pesan status.
  - Status message `/send/message` memakai waktu lokal server (UTC), bukan timestamp ack server.
  - `qr_duration` = setengah timeout QR dari whatsapp-rust (hasil live: 29), GOWA/whatsmeow biasanya 30.
  - Link preview: hanya `jpegThumbnail` inline; GOWA juga meng-upload thumbnail resolusi tinggi (`MediaLinkThumbnail`).
  - Kirim media ke newsletter memakai upload biasa (GOWA: `UploadNewsletter`) → kemungkinan gagal; Fase 4.
  - Thumbnail dokumen hanya untuk file gambar (GOWA juga PDF).
  - `/user/info`: `name` kosong (butuh chat storage); daftar device dari usync device list.
  - `/group/info`: JSON meniru `whatsmeow types.GroupInfo` dari ingatan (nama field Go: `JID`, `Name`, `Topic`, `Participants`, ...);
    versi whatsmeow acuan GOWA lebih baru dari yang bisa diverifikasi → bandingkan dengan output GOWA asli bila ada konsumen yang bergantung.
    `DefaultMembershipApprovalMode`/`AnnounceVersionID` selalu kosong (whatsapp-rust tidak mengeksposnya).
  - `/group/participants|participant-requests`: `display_name` hanya dari tabel `contacts` (GOWA juga memakai verified name dari usync).
  - `/user/avatar` GET: SDK memberi `NotFound`/`NotAuthorized` (bukan error) → `AVATAR_NOT_SET` 404 / `AVATAR_HIDDEN` 403 dengan pesan whatsmeow; timeout 5 dtk per percobaan → 408 `CONTEXT_ERROR`; `is_community` dipaksa false untuk JID user dan, bila gagal, jatuh ke rute biasa (hanya diklasifikasi bila kedua percobaan sepakat, selain itu 500). Boolean query tak valid → 500 (Fiber `Bind().Query`). `type` = `preview`/`image` menurut `is_preview` (GOWA: atribut node). POST: tanpa file → 500 sebelum cek login; foto diproses seperti foto grup (crop persegi, ≤640px, JPEG q80) lalu `profile().set_profile_picture`.
  - `/user/presence/subscribe`: LID yang pemetaannya diketahui diubah ke nomor oleh `resolve_recipient` (GOWA menolaknya sebagai non-user); peringatan `WHATSAPP_PRESENCE_ON_CONNECT` belum ada karena presence-on-connect belum diport.
  - `/user/pushname`: nama kosong ditolak whatsapp-rust → 500 (GOWA mengirim app-state kosong tanpa validasi); `SyncPending` (presence terkirim tapi app-state gagal) dipetakan ke 500 seperti GOWA. `/user/pushname` memakai scope device (GOWA memakai `c.Context()` tanpa device).
  - `/user/my/contacts/detailed`: `lid` dari `get_lid_pn_entry` (PN→LID) — hanya terisi bila pemetaan sudah dipelajari SDK; filter bot memakai prefix `1313555`/`131655500` (whatsmeow `IsBot`).
  - `/user/my/groups`: grup yang daftar pesertanya dipotong server (`Truncated`) keluar dengan `Participants: []` + `ParticipantCount` benar dan field lain kosong; grup `Forbidden`/`NotFound` di antara dua IQ dilewati. `ParticipantCount` kini juga ada di `/group/info` (struct Go sama). `truncated_group_json` belum punya unit test (`GroupOverview` `non_exhaustive`, tak bisa dibangun di luar SDK).
  - `/group/info-from-link`: `participant_count` = jumlah peserta di respons (atau atribut `size` bila daftar kosong).
  - `/group/join-with-link` pada grup yang butuh persetujuan tetap mengembalikan `group_id` (GOWA/whatsmeow sama), tanpa penanda "pending".
  - `/group/topic` memakai `PreviousDescription::Resolve` (satu IQ tambahan untuk membaca id deskripsi sebelumnya).
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

### 2026-11-24 — Update gowa-ui berkala + retry (rc.2)
- `src/ui.rs`: `update_loop` (retry/backoff, jitter), `Outcome`, pin SHA-256, token, `api_base` (untuk test), `{error:#}`; `config.rs`: `APP_UI_UPDATE_INTERVAL|GITHUB_TOKEN|ASSET_SHA256` + `parse_sha256_pin`. Pemicu: container user mencetak `dashboard update failed … error sending request` (jaringan, tak ada retry).
- Test: 8 unit `ui::tests` (GitHub palsu lewat axum: unduh+cache+up-to-date, digest salah, pin sebelum/sesudah unduh, token, cache melanggar pin, rantai error, backoff, jitter) + `sha256_pin_is_empty_or_64_hex`. Loop `update_loop` sendiri tak punya test (bagian murninya ya).

### 2026-11-24 — Fase 4: `/app/metrics`, rilis (Docker + GitHub Actions)
- `src/sysmetrics.rs` (+`build.rs` untuk versi rustc), route `/app/metrics` (tanpa device, di balik auth), SIGTERM, feature `bundled`, profil release (thin LTO, strip), `Dockerfile`, `.dockerignore`, `ci.yml`, `release.yml`, README (bagian Docker/Rilis). Versi → `0.1.0-rc.1`.
- Test: 6 unit `sysmetrics`, HTTP `app_metrics_reports_devices_and_system`, `app_metrics_honours_base_path_and_needs_no_device`. Docker: build dari source sukses, smoke test OK. Belum diuji: workflow di GitHub, image arm64.

### 2026-11-24 — Fase 4: forward terjadwal
- `message::validate_scheduled_forward`, `ForwardRequest` (+`message_id`, `Clone`, `Serialize`), handler forward memakai `Payload` dan `routes::send::schedule`, arm `"forward"` di `DeviceBackend`, `send::dry_run_ok`. Test: HTTP `forward_can_be_scheduled_after_the_storage_checks`, unit round-trip payload forward. Belum diuji dengan akun live.

### 2026-11-24 — Fase 4: scheduler (`/send/schedules*`, jadwal pada semua `/send/*`)
- Bagian 1 (`973fc0f`): `usecase/schedule_time.rs` (opsi + `next_occurrence`, port GOWA, test DST/bulanan/mingguan), `storage/schedule.rs` + migrasi `000004_scheduled_sends` (claim berlease, pin id, transisi status terjaga).
- Bagian 2: `usecase/schedule.rs` (`ScheduleService`: create/list/get/pause/resume/cancel; `Worker<Backend>`), `schedule_dispatch.rs` (`DeviceBackend` + klasifikasi `Permanent`/`Transient`), task-local `DRY_RUN`/`PINNED` di `send.rs`, `SendResponse` + `schedule_id`/`scheduled_at`/`next_run_at`, derive `Clone`+`Serialize` pada request kirim, extractor `Payload.schedule`, handler `/send/*` bercabang ke `schedule(...)`, route `/send/schedules*`, `AppState.scheduler`, worker dijalankan di `server.rs`.
- Test: 15 unit mesin status (kirim sekali, berulang+limit, retry id sama, kehabisan percobaan, permanen, device offline, `end_at`, pemulihan, secret poll, pause/resume/cancel, resume job terlambat, media, GC, list), 6 storage, 8 waktu, 2 dispatch, + HTTP `scheduling_a_send_validates_then_stores_without_a_session`, `scheduling_accepts_forms_with_repeated_weekdays_and_uploads`. Belum diuji dengan akun live (cek: jadwalkan teks 2 menit ke depan → terkirim; harian; matikan server di antara → pulih tanpa duplikat; gambar terjadwal; pause/resume/cancel).

### 2026-11-24 — Fase 4: newsletter live-updates + `newsletter.message`
- `device/live_updates.rs` (`LiveUpdates`, loop renew, fungsi murni `next_renew_delay`/`backoff`/`retry_delay`), `usecase::newsletter::live_updates_*`, route GET/POST/DELETE, `events::newsletter_message_envelope`, handler `Event::NewsletterLiveUpdate`. Test: golden `newsletter_message_event`, unit delay/backoff/state, `newsletter_live_updates_contract`. Catatan: link test sempat gagal `ld … signal 9` (kehabisan memori) — ulang dengan `cargo test -j 1`. Belum diuji dengan akun live (cek: subscribe saluran ikutan → reaksi/forward berubah → webhook; renew tiap ~72 dtk di log; unsubscribe).

### 2026-11-24 — Fase 4: `/newsletter/*` (bagian 1)
- `usecase/newsletter.rs`, `http/routes/newsletter.rs`; `user::newsletter_value` diekstrak dari `my_newsletters` agar dipakai bersama. Test: `newsletter_contract` (validasi, 500 int, 401), unit `id_validation_messages`, `query_ints_like_fiber`. Belum diuji dengan akun live (cek: info via id & invite, messages, download media saluran, follow/unfollow).

### 2026-11-24 — Checklist uji manual
- `docs/MANUAL-TEST.md`: persiapan (2 akun, grup test, webhook.site), 9 bagian berurutan (pair, chat storage, kirim, webhook masuk, `/message/*`, `/group/*`, `/user/*`, event lain, multi-device/logout) + tabel temuan. Belum dijalankan.

### 2026-11-24 — Fase 3: `device.reachout_timelock` + error 463
- `device/acks.rs` (`AckBook`) + `Event::ServerAck` di `processor.rs`; `deliver` menunggu ack; `nack_error`. `Event::ReachoutTimelockUpdate` → webhook `device.reachout_timelock` + hub WS. Test: unit `AckBook` (3), `nack_463_is_reachout_timelock`, golden `device_reachout_timelock`. Belum diuji dengan akun live (cek: kirim ke nomor yang belum pernah chat dengan akun terbatasi → 429; kirim normal tidak jadi lambat).

### 2026-11-24 — Fase 3: `label.edit`, `label.association`
- `events::label_edit_envelope` (`LabelEditView`), `label_association_envelope`; handler `LabelEditUpdate`/`LabelAssociationUpdate` di `processor.rs`. Test: golden `label_events` (contoh dokumen + sparse + LID). Belum diuji dengan akun live (cek: buat/ubah label & beri label ke chat di WhatsApp Business).

### 2026-11-24 — Fase 3: `message.deleted`
- `events::message_deleted_envelope` + `DeletedView`; handler `Event::DeleteMessageForMeUpdate` di `processor.rs` (hapus baris + webhook). Test: golden `message_deleted_event` (+ media/LID). Belum diuji dengan akun live (cek: "hapus untuk saya" dari HP → webhook + pesan hilang dari `/chat/{jid}/messages`).

### 2026-11-24 — Fase 3: presence on connect + pulse
- `config.rs`: `WHATSAPP_PRESENCE_ON_CONNECT|PULSE_ENABLED|PULSE_INTERVAL|PULSE_DURATION` + `parse_go_duration`; `device/presence.rs` (`send_on_connect`, `PulseClock`, `start_pulse`), dipanggil dari `processor` (Connected) dan `server.rs`. Test: `on_connect_setting`, `pulse_clock_respects_interval_and_in_flight`, `go_durations`. Belum diuji dengan akun live (cek: `presence-on-connect=available` lalu subscribe kontak → webhook `presence`/`chat_presence` muncul).

### 2026-11-24 — Fase 3: event `presence`
- `events::presence_envelope`, handler `Event::Presence` di `processor.rs` (+ `EventKind::Presence`). Test: golden `presence_events` (dua contoh dokumen + LID). Belum diuji dengan akun live (cek: subscribe kontak → buka/tutup WhatsApp di HP kontak).

### 2026-11-24 — Fase 3: auto reply, auto mark read
- `WHATSAPP_AUTO_REPLY` dan `WHATSAPP_AUTO_MARK_READ` mengalir config → `DeviceOptions` → `MediaSettings`; handler di `processor.rs`. Doc-comment `add_session_id` yang tersangkut di atas fungsi lain dirapikan. Test: unit `typed_text_detection`. Belum diuji dengan akun live (cek: kirim teks ke akun → balasan otomatis + centang biru; grup tidak dibalas).

### 2026-11-24 — Fase 3: `/call/reject`, `call.offer`, auto-reject
- `usecase/call.rs`, `http/routes/call.rs`; `events::call_offer_envelope`; `device/processor.rs::incoming_call` (+ `EventKind::IncomingCall`); `auto_reject_call` mengalir config → `DeviceOptions` → `MediaSettings`.
- Test: golden `call_offer_events` (dua contoh dari `webhook-payload.md`), `call_reject_contract`. Belum diuji dengan akun live (cek: telepon masuk → webhook; `auto_rejected:true` saat flag aktif; `POST /call/reject` sebelum diangkat).

### 2026-11-24 — Fase 3: `/user/my/newsletters`, `/user/business-profile`
- `usecase/user.rs`: `my_newsletters`, `newsletter_json`, `business_profile`, `business_profile_json`, `hhmm`. Test: `user_business_and_newsletters_contract`, unit `business_profile_shape`, `newsletter_shape`. Belum diuji dengan akun live (cek: akun yang mengikuti saluran; nomor akun bisnis vs bukan).

### 2026-11-24 — Fase 3: `/send/poll`
- `usecase/send_media.rs`: `validate_poll` (pesan ozzo GOWA), `poll_message`, `send_poll`. Test: `send_poll_contract`, unit `poll_message_shape`. Belum diuji dengan akun live (cek: poll muncul di HP, pilih jawaban, duration/expiry).

### 2026-11-24 — Fase 3: `/send/sticker`, `/send/presence`, `/send/chat-presence`
- `usecase/send_media.rs`: `send_sticker`, `send_presence`, `send_chat_presence`; `media/image.rs`: `webp_info`, `sticker_png`, `png_to_webp_lossless`; `media/ffmpeg.rs`: `png_to_webp`. Route di `http/routes/send.rs`.
- Test: `send_sticker_contract`, `send_presence_contract`, unit `webp_header_info`, `sticker_png_fits_512`, `sticker_webp` (ffmpeg nyata bila ada). Belum diuji dengan akun live (cek: sticker statis/GIF/WebP animasi sampai di HP, indikator mengetik muncul). Field jadwal diabaikan → Fase 4.

### 2026-11-24 — Fase 3: event grup (`GroupUpdate`)
- `events/mod.rs`: `group_participants_envelope`, `group_info_envelope` (`name_set_by` utamakan PN, kosong bila tak diketahui), `group_joined_envelope`. `device/processor.rs`: `group_update` (+ `EventKind::GroupUpdate`). `storage::ChatStore::rename_chat`.
- Test: golden dari `webhook-payload.md` (join/leave/promote/demote/name change; LID di-resolve), bentuk `group.joined`, `rename_chat`. Belum diuji dengan akun live (cek: ganti nama grup dari HP lain → webhook `group.info` + `GET /chats` ikut berubah).

### 2026-11-24 — Fase 3: `/user/presence/subscribe`, `/user/avatar`
- `usecase/user.rs`: `subscribe_presence` (`presence().subscribe`), `avatar` (`pictures().lookup`; `Fetched` + `combine` murni untuk klasifikasi/fallback), `change_avatar`. Route di `http/routes/user.rs`.
- Test: `presence_subscribe_validates_then_needs_login`, `avatar_endpoints_validate_then_need_login`, unit `avatar_*`/`community_fallback_*`. Belum diuji dengan akun live (cek: avatar kontak tanpa foto → 404, foto disembunyikan → 403, ganti avatar sendiri).

### 2026-11-24 — Fase 3: `/user/my/privacy|contacts|contacts/detailed`, `/user/pushname`
- `usecase/user.rs`: `my_privacy` (`fetch_privacy_settings`), `change_push_name` (`profile().set_push_name`), `my_contacts` (nama via `names::preferred_contact_name`), `my_contacts_detailed` (`person_contacts` murni: server pn/lid, tanpa bot, satu baris per user, urut JID). `storage::ChatStore::all_contacts`.
- Test: `user_profile_endpoints_require_login` (401 + body rusak → 500), unit `person_contacts_*`, `privacy_values_*`, `all_contacts_sorted_and_device_scoped`. Belum diuji dengan akun live.

### 2026-11-24 — Backfill nama (pola zapfast)
- `device/backfill.rs`: `fill_group_names` (generik atas trait `GroupNames`, dites dengan fake: batch, backoff lalu menyerah, tidak menimpa nama yang terisi di sela, berhenti saat device pergi), `kick_group_names`/`note_group_seen`, `resync_contacts` + marker. `storage/chat.rs`: `group_chats`, `rename_chat_if`. `ingest::placeholder_group_name` dipakai bersama `chat_name`.
- Field baru `DeviceInstance`: `group_names_running`, `contacts_resyncing`, `seen_groups`. Belum diuji dengan akun live: pastikan setelah connect, `GET /chats` menampilkan nama grup asli dan `contacts` terisi (log `contacts replayed from the phone`).
- Belum: event `GroupUpdate` (rename grup oleh orang lain) belum mengubah nama chat; peta LID→nomor dari `ContactUpdate`/`sender_alt` saat ingest (zapfast `learn_pair`) belum diadaptasi; `first_name` sudah tersimpan di `contacts`.

### 2026-11-24 — Fase 3: `/user/my/groups`
- `usecase/group.rs`: `my_groups` (list_participating → fetch_metadata_batch per 200 id), `group_info_values` (dipakai bersama `info`: resolve LID→PN lalu satu query kontak untuk semua grup), `truncated_group_json`; `ParticipantCount` ditambah ke JSON `GroupInfo`. Route di `http/routes/user.rs`.
- Test: `user_my_groups_requires_login`; unit test `group_info` memeriksa `ParticipantCount`. Belum diuji dengan akun live.

### 2026-11-23 — Fase 3: `/group/*`
- `usecase/group.rs` + `http/routes/group.rs`: 19 route (create, join-with-link, info-from-link, info, leave, participants
  + export CSV, add/remove/promote/demote, participant-requests + approve/reject, photo, name, locked, announce, topic,
  invite-link). Validasi gaya ozzo (`participants: (1: cannot be blank.).`, `name: the length must be between 1 and 25.`).
- `media/image.rs`: `validate_group_photo` dan `group_photo` (crop persegi, 640px, JPEG q80). CSV export meniru
  `spreadsheetSafeCell` + quoting `encoding/csv`. `usecase::chat::{ozzo, required}` dibuka `pub(crate)`.
- Test: `group_validation_then_login`, `group_listing_endpoints_reject_empty_id_before_login`, `group_photo_validation`,
  plus unit test (CSV, sel formula, bool form, foto, bentuk JSON `GroupInfo`). Belum diuji dengan akun live.

### 2026-11-23 — Fase 3: `/message/{id}/*`
- `usecase/message.rs` + `http/routes/message.rs`: reaction (key ala `BuildMessageKey`), revoke (sender/admin dari salinan
  tersimpan, lalu hapus lokal), delete-for-me & star/unstar (app state), update (teks, caption image/document, link
  preview; edit dicerminkan ke storage via `ingest::store_own_edit`), read (grup butuh pengirim tersimpan), played
  (hanya audio masuk), forward (CDN blob lama, re-upload bila gagal/`force_reupload`, timer chat tersimpan), download
  (`statics/media/<phone>/<tanggal>/<unix>-<uuid><ext>` + `file_url`).
- `send_media.rs`: `link_preview_message` dan `upload` dibuka untuk dipakai ulang.
- Contract test: validasi semua endpoint, 401 sebelum login, body rusak → 500, forward cek storage dulu.

### 2026-11-23 — Fase 3: `/chats`, `/chat/{jid}/*`
- `usecase/chat.rs` + `http/routes/chat.rs`: list chats (filter search/has_media/archived, pagination total terfilter),
  messages (filter waktu RFC3339, media_only, is_from_me, search; total = semua pesan chat; chat belum tersimpan → kosong),
  pin/unpin & archive (app-state `chat_actions`), disappearing (1:1 protocol message, grup `set_ephemeral`),
  history on-demand (`fetch_message_history` dari pesan tertua). Validasi ozzo + body/query ala Fiber.
- `storage/chat.rs`: tabel `contacts` (+ `oldest_message`), ingest mengisi dari push name, verified name,
  `ContactUpdate`, push name history sync. `usecase/names.rs`: resolver nama chat/sender; webhook
  `sender_display_name` kini memakai kontak tersimpan (gap lama ditutup).
- Contract test: `chats_list_contract`, `chat_messages_contract`, `chat_actions_validate_then_need_login`.

### 2026-11-23 — Fase 3: chat storage
- `storage/chat.rs`: tabel `chats`, `messages`, `message_reactions`, `message_edits` (migrasi `000002`), upsert ala GOWA
  (replay tidak menghapus `context_metadata`; pesan terkirim tidak menimpa edit), list/count/search + reaksi, delete cascade,
  `delete_device_data`.
- `wa/message.rs`: port `ExtractMessageTextFromProto` (tanpa pesan bisnis), `ExtractMediaInfo`, `GenerateMediaFilename`,
  `ExtractContextMetadata`, `ExtractEphemeralExpiration`, `ExtractPhoneFromVCard`, ringkasan kontak/lokasi, revoke/edit/reaction.
- `device/ingest.rs`: `CreateMessage` (nama chat, timer, arsip), reaksi, revoke (author/admin grup/akun sendiri), edit (author check,
  stub hanya untuk edit sendiri), pesan terkirim, history sync (bootstrap/recent/on-demand + push name), `ArchiveUpdate`.
- Processor menyimpan sebelum webhook; `Resolver::is_group_admin` (fetch metadata grup). Purge device menghapus partisi slot + JID
  (kecuali JID dipakai slot lain). `DeviceManager::bootstrap` kini menerima `Db`.
- Reply (`merge_reply`) dan default timer `/send/message` membaca chat storage; `device/recent.rs` dihapus.
- Smoke: server start di direktori kosong → semua tabel termigrasi. Belum diuji dengan akun live.

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
