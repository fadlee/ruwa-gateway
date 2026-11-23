# Kode error GOWA

Diekstrak dari GOWA `src/pkg/error/*.go`, `ui/rest/middleware/*.go` dan `cmd/rest.go`
(commit di `docs/upstream/README.md`). Implementasi: `src/error.rs`.

## Envelope

Error dirender `Recovery` middleware GOWA sebagai `{code, message, results?}`:

- Error yang mengimplementasikan `GenericError` → `StatusCode()` + `ErrCode()` + `Error()`.
- Error lain (termasuk gagal bind body JSON/form) → **500 `INTERNAL_SERVER_ERROR`**, message = teks error.
- `context.DeadlineExceeded` → 504 `GATEWAY_TIMEOUT`, "Request timed out waiting for WhatsApp server response".

## Daftar kode

| HTTP | `code` | Message tetap | Sumber | Status di ruwa |
|---|---|---|---|---|
| 400 | `VALIDATION_ERROR` | (bervariasi; format ozzo `field: cannot be blank.`) | `ValidationError` | ✅ |
| 400 | `ALREADY_LOGGED_IN` | `you are already logged in.` | `ErrAlreadyLoggedIn` | ✅ |
| 401 | `AUTHENTICATION_ERROR` | `you are not connect to services server, please reconnect` | `ErrNotConnected` | ✅ |
| 401 | `AUTHENTICATION_ERROR` | `you are not logged in` | `ErrNotLoggedIn` | ✅ |
| 401 | `AUTHENTICATION_ERROR` | `reconnect error` | `ErrReconnect` | — |
| 500 | `QR_CHANNEL_ERROR` | `QR channel error` | `ErrQrChannel` | — |
| 500 | `SESSION_SAVED_ERROR` | `your session have been saved, please wait to connect 2 second and refresh again` | `ErrSessionSaved` | ✅ |
| 404 | `NOT_FOUND` | `device not found` / `scheduled send not found` | `ErrDeviceNotFound`, `ErrScheduledSendNotFound` | sebagian |
| 500 | `INTERNAL_SERVER_ERROR` | (bervariasi) | `InternalServerError`, panic biasa | ✅ |
| 408 | `CONTEXT_ERROR` | (bervariasi) | `ContextError` | — |
| 400 | `INVALID_JID` | `Phone <x> is not on whatsapp`, `user is not registered` | `InvalidJID` | ✅ |
| 500 | `WEBHOOK_ERROR` | (bervariasi) | `WebhookError` | log saja |
| 500 | `INVALID_WA_CLI` | `your WhatsApp CLI is invalid or empty` | `ErrWaCLI` | — (pakai `AUTHENTICATION_ERROR` not connected) |
| 500 | `UPLOAD_MEDIA_ERROR` | (bervariasi) | `WaUploadMediaError` | Fase 2 |
| 429 | `WA_REACHOUT_TIMELOCK` | teks panjang error 463 | `ErrWaReachoutTimelock` | Fase 3 |
| 404 | `AVATAR_NOT_SET` | (bervariasi) | `AvatarNotSetError` | Fase 3 |
| 403 | `AVATAR_HIDDEN` | (bervariasi) | `AvatarHiddenError` | Fase 3 |
| 404 | `DEVICE_NOT_FOUND` | `device not found; create a device first from /api/devices or provide a valid X-Device-Id` + `results.device_id` | `DeviceMiddleware` | ✅ |
| 400 | `DEVICE_ID_REQUIRED` | `device_id is required via X-Device-Id header or device_id query` | `DeviceMiddleware` | ✅ |
| 503 | `DEVICE_MANAGER_UNAVAILABLE` | `Device manager is not initialized` | `DeviceMiddleware` | tidak relevan |
| 504 | `GATEWAY_TIMEOUT` | `Request timed out waiting for WhatsApp server response` | `Recovery` | — |

## Bukan envelope JSON

| Kasus | Respons GOWA |
|---|---|
| Basic auth gagal | 401, body teks `Unauthorized`, header `WWW-Authenticate: Basic realm="Restricted"...` (Fiber `basicauth`) |
| `GET /health` | 200 `OK` (teks), selalu di root, mengabaikan `APP_BASE_PATH` |
