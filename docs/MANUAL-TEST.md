# Checklist uji manual (verifikasi live)

> Hampir seluruh Fase 3 lulus test otomatis tetapi **belum pernah dijalankan dengan akun WhatsApp nyata**
> (daftar lengkap: HANDOFF → "Belum terverifikasi live"). Dokumen ini mengurutkan uji supaya satu sesi
> mencakup sebanyak mungkin fitur. Centang `[x]` yang lulus; catat yang gagal di bagian **Temuan** paling bawah
> (lalu salin ke HANDOFF).

## 0. Persiapan

**Yang dibutuhkan**

- **Akun A** (akun yang diuji, di-pair ke server) dan **HP B** (akun/nomor lain untuk lawan bicara).
- Satu **grup test** berisi A + B, dengan **A sebagai admin** (untuk `/group/*` yang butuh admin). Grup kedua
  tempat A *bukan* admin berguna untuk uji penolakan.
- Penerima webhook: <https://webhook.site> (paling mudah) atau `python3 -m http.server 9000` (tanpa body).
  Dengan webhook.site, ambil URL uniknya.
- `ffmpeg` terpasang (uji video/PTT/sticker); `jq` dan `curl`.

**Jalankan server** dari direktori kerja terpisah (server menulis `storages/` dan `statics/` di cwd):

```sh
mkdir -p ~/ruwa-test && cd ~/ruwa-test
APP_HOST=127.0.0.1 \
WHATSAPP_WEBHOOK=https://webhook.site/<uuid> \
WHATSAPP_PRESENCE_ON_CONNECT=available \
RUST_LOG=info \
cargo run --manifest-path ~/Projects/ruwa-gateway/Cargo.toml -- rest
```

Variabel bantu (shell lain):

```sh
B=http://127.0.0.1:3000
A=62xxxxxxxxxx      # nomor akun A
PB=62yyyyyyyyyy     # nomor HP B
G=120363xxxxxxxxxx@g.us   # JID grup test (dari GET /user/my/groups)
j() { curl -s -H 'content-type: application/json' "$@" | jq; }
```

Sambil menguji, buka **gowa-ui** (`http://127.0.0.1:3000/`) — menu Chats/Groups memudahkan melihat hasil — dan
amati log server untuk `WARN`/`ERROR`.

> **Catatan:** `WHATSAPP_PRESENCE_ON_CONNECT=available` sengaja dipakai agar event `presence`/`chat_presence`
> bisa muncul, tetapi akun *available* menekan notifikasi di HP A. Kembalikan ke default setelah sesi uji.

---

## 1. Pair & koneksi (sudah pernah lulus — cek ulang cepat)

- [ ] `GET /app/login` → buka `qr_link`, scan dari A → `GET /app/status` → `is_logged_in: true`.
- [ ] Restart server → sesi pulih tanpa scan; `GET /app/devices` menampilkan JID.
- [ ] Log menyebut `own presence set` (presence on connect) tanpa error.
- [ ] **History sync** setelah pair: beberapa menit kemudian `GET /chats` berisi chat dengan nama (bukan hanya nomor).

## 2. Chat storage & backfill nama

- [ ] `GET /chats?limit=50` → chat 1:1 bernama kontak; **nama grup bukan `Group <id>`** (backfill nama grup;
      beri waktu hingga beberapa menit setelah connect, lihat log).
- [ ] `GET /user/my/contacts` terisi (replay kontak `CriticalUnblockLow` — log `contacts replayed from the phone`).
- [ ] B kirim pesan ke A → webhook `message` + `GET /chat/$PB@s.whatsapp.net/messages` memuat pesan itu.
- [ ] A mengirim pesan (bagian 3) → muncul di `/chat/.../messages` dengan `is_from_me: true`.
- [ ] **Reply setelah restart**: restart server, lalu `reply_message_id` ke pesan lama masih mengutip benar.
- [ ] `POST /chat/$PB@s.whatsapp.net/pin {"pinned":true}` → chat tersemat di HP A; `false` melepas.
- [ ] `POST /chat/.../archive {"archived":true}` → terarsip di HP A (dan event arsip dari HP mengubah flag di `/chats`).
- [ ] `POST /chat/.../disappearing {"timer_seconds":86400}` → timer aktif di HP; `0` mematikan.
- [ ] `POST /chat/.../history {"count":20}` → pesan lebih lama masuk (history on-demand).

## 3. Kirim pesan (`/send/*`)

Kirim ke B. Verifikasi tiap pesan **sampai di HP B** dan mengembalikan `message_id`.

```sh
j $B/send/message -d "{\"phone\":\"$PB\",\"message\":\"halo dari ruwa\"}"
```

- [ ] Teks; dengan `reply_message_id`; dengan `is_forwarded:true` (label "Diteruskan"); dengan `duration:86400`.
- [ ] Teks ke **grup** dengan `mentions` (`"phone":"$G"`, `"mentions":["$PB"]`) dan `@everyone`.
- [ ] Gambar: `curl -F phone=$PB -F caption=tes -F image=@foto.jpg $B/send/image` (juga `compress=false`,
      `view_once=true`, `image_url`).
- [ ] File/dokumen, video (`video`, uji `compress`/`gif_playback`), audio biasa dan `ptt=true` (voice note,
      **waveform muncul**).
- [ ] Kontak, lokasi, link (preview muncul).
- [ ] **Sticker**: PNG/JPG statis, GIF, WebP animasi 512×512 (<500KB). Cek tampil sebagai stiker di HP.
- [ ] **Poll**: `{"phone":"$PB","question":"q","options":["a","b","c"],"max_answer":1}` lalu `max_answer:2` —
      keduanya bisa dipilih di HP B.
- [ ] `POST /send/presence {"type":"available"}` / `unavailable`.
- [ ] `POST /send/chat-presence {"phone":"$PB","action":"start"}` → HP B melihat "mengetik…"; `stop` menghentikan.
- [ ] **Latensi kirim** (perubahan baru: kirim menunggu ack ≤5 dtk): ukur `time curl …/send/message` — normalnya
      <1–2 dtk. Catat bila terasa lambat.
- [ ] **Error 463**: kirim ke nomor yang belum pernah chat dengan A (akun baru/terbatasi) → bila ditolak server,
      harus `429 WA_REACHOUT_TIMELOCK`, bukan sukses palsu. (Mungkin tak bisa direproduksi; catat saja.)

## 4. Webhook masuk (cek di webhook.site)

Header `X-Hub-Signature-256` ada bila `WHATSAPP_WEBHOOK_SECRET` diset (uji sekali dengan secret).

- [ ] B kirim teks → `message`; B kirim **gambar** → `message` + file tersimpan di `statics/media/` (auto-download).
- [ ] B kirim voice note, dokumen, stiker, kontak, lokasi → payload `message` sesuai jenisnya.
- [ ] B **membalas** pesan A → `reply_to` / `quoted` terisi.
- [ ] B memberi **reaksi** → `message.reaction`; B **mengedit** → `message.edited`; B **menarik** → `message.revoked`.
- [ ] B membuka pesan A → `message.ack` (`delivered`, lalu `read`).
- [ ] B mengetik → `chat_presence` (butuh A *available*).
- [ ] A subscribe B (`POST /user/presence/subscribe {"phone":"$PB"}`), B buka/tutup WhatsApp → `presence`
      (`available`/`unavailable`, `last_seen` bila tidak disembunyikan).
- [ ] **"Hapus untuk saya"** dari HP A pada satu pesan → `message.deleted` + pesan hilang dari `/chat/.../messages`.
- [ ] B menelepon A → `call.offer` (`auto_rejected:false`). Ulangi dengan `WHATSAPP_AUTO_REJECT_CALL=true`
      → panggilan ditolak, `auto_rejected:true`. `POST /call/reject` dengan `call_id`/`from` dari webhook saat HP berdering.
- [ ] **Auto reply**: jalankan dengan `WHATSAPP_AUTO_REPLY="sibuk"` → B kirim teks → balasan otomatis; kirim di grup
      **tidak** dibalas; gambar dengan caption **tidak** dibalas.
- [ ] **Auto mark read**: `WHATSAPP_AUTO_MARK_READ=true` → pesan B langsung bercentang biru.

## 5. Operasi pesan (`/message/{id}/*`)

Gunakan id dari pesan yang A kirim/terima.

- [ ] `reaction {"phone":"$PB","emoji":"👍"}` (pesan sendiri dan pesan B); emoji kosong menghapus.
- [ ] `update {"phone":"$PB","message":"diedit"}` pada pesan teks A; juga caption gambar.
- [ ] `revoke` pesan A (di 1:1 dan di grup); `revoke` pesan B sebagai admin grup.
- [ ] `delete` ("untuk saya") dan `star`/`unstar` → tercermin di HP A.
- [ ] `read` pesan masuk (1:1 dan grup) → B melihat centang biru.
- [ ] `played` pada voice note masuk.
- [ ] `forward` ke chat lain (teks, gambar; `force_reupload:true`).
- [ ] `download` media masuk → `file_url` + berkas ada di `statics/media/<phone>/…`.

## 6. Grup (`/group/*`) — akun A admin

```sh
j $B/group -d "{\"title\":\"Uji ruwa\",\"participants\":[\"$PB\"]}"
```

- [ ] `POST /group` (buat) → grup muncul di HP; `GET /group/info?group_id=…` memuat peserta & subjek.
- [ ] `GET /user/my/groups` daftar grup (2 IQ saja, tidak kena rate limit); peserta lengkap.
- [ ] `participants` (daftar) dan `/participants/export` (CSV terunduh, kolom benar).
- [ ] `participants/add`, `remove`, `promote`, `demote` ke B → **webhook `group.participants`** (`join`/`leave`/
      `promote`/`demote`).
- [ ] `name` (ganti nama) → HP berubah, **webhook `group.info`** (+ `name_set_by`), dan `GET /chats` ikut berganti.
      Ganti nama dari HP B → webhook + `/chats` ikut berubah.
- [ ] `topic`, `locked`, `announce`, `photo` (unggah foto; tanpa file = hapus foto).
- [ ] `invite-link` (dan `?reset=true` bila ada) → `info-from-link` / `join-with-link` dari akun lain.
- [ ] `participant-requests` + `approve`/`reject` (butuh grup mode persetujuan).
- [ ] Akun A **ditambahkan** ke grup baru oleh B → **webhook `group.joined`**.
- [ ] `leave` grup uji.
- [ ] Sebagai **bukan admin**: operasi admin ditolak dengan pesan yang masuk akal (bukan crash).

## 7. User & profil (`/user/*`)

- [ ] `check?phone=$PB` (terdaftar) dan nomor ngawur (tidak terdaftar).
- [ ] `info?phone=$PB` — `name` kosong? (diketahui; butuh chat storage) dan daftar device.
- [ ] `avatar?phone=$PB` (ada), kontak tanpa foto → `404 AVATAR_NOT_SET`, foto disembunyikan → `403 AVATAR_HIDDEN`;
      `?is_preview=true`; `POST /user/avatar` (ganti foto A).
- [ ] `my/privacy`, `my/contacts`, `my/contacts/detailed` (nama & `lid`), `my/newsletters` (butuh A mengikuti saluran),
      `business-profile?phone=<nomor bisnis>` (jam buka `09:00` bukan `05:40`; nomor non-bisnis → 500 wajar).
- [ ] `POST /user/pushname {"push_name":"Nama Uji"}` → nama tampil di HP B.

## 8. Event lain

- [ ] **Label** (butuh WhatsApp Business): buat/ubah/hapus label → `label.edit`; beri/lepas label ke chat → `label.association`.
- [ ] **Presence pulse**: set `WHATSAPP_PRESENCE_PULSE_INTERVAL=2m` `…_DURATION=30s` + `PRESENCE_ON_CONNECT=unavailable`
      → log `own presence set` available lalu unavailable sesuai jadwal.
- [ ] `device.temporary_ban` / `device.reachout_timelock`: sulit dipancing — cukup pastikan tidak ada error di log.

## 9. Multi-device & logout

- [ ] `POST /devices {"device_id":"kedua"}` → `GET /devices/kedua/login` → pair akun kedua; kirim pesan dengan
      header `X-Device-Id: kedua`; chat storage tiap akun **terpisah** (`/chats` per device).
- [ ] `PATCH`/webhook per device (`/devices/{id}/webhook`) mengalihkan event akun itu saja.
- [ ] **Logout dari HP** (Perangkat tertaut → keluarkan) → webhook `device.logged_out`, sesi dibersihkan, `/ws`
      menerima `DEVICE_LOGGED_OUT`.
- [ ] `GET /app/logout` (logout dari server) **tidak** mengirim `device.logged_out`.
- [ ] Setelah logout, `GET /app/login` bisa pair ulang tanpa sisa sesi lama.

---

## Temuan

Salin tabel ini ke HANDOFF (bagian "Catatan / jebakan") setelah sesi uji.

| # | Bagian | Endpoint/event | Yang terjadi | Yang diharapkan | Log / bukti |
|---|--------|----------------|--------------|-----------------|-------------|
|   |        |                |              |                 |             |

## Setelah selesai

- Pindahkan fitur yang lulus dari "Belum terverifikasi live" ke "Terverifikasi manual" di HANDOFF (beri tanggal).
- Fitur yang gagal → buat entri di HANDOFF dengan gejala dan log; perbaiki sebelum mulai Fase 4.
- Kembalikan `WHATSAPP_PRESENCE_ON_CONNECT` ke `unavailable`.
