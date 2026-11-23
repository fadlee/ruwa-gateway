# ruwa-gateway

GOWA-compatible WhatsApp REST API in Rust, built on whatsapp-rust.

## Read first

1. `docs/HANDOFF.md` — current status, next steps, decisions, gotchas.
2. `docs/PLAN.md` — phase checklist.
3. `docs/SPEC.md` — contract (endpoints, envelope, webhook, config).
4. `docs/upstream/` — GOWA contract snapshot (openapi, webhook payloads). Source of truth for compatibility; do not edit by hand.

## Rules

- **Update `docs/HANDOFF.md` after finishing any unit of work** (feature, fix, decision, or end of session):
  refresh "Status saat ini" and "Langkah berikutnya", add new decisions/gotchas, append a dated entry to "Log".
  Also tick the matching items in `docs/PLAN.md`.
- GOWA compatibility wins: path, field names, error codes, and webhook payloads must match `docs/upstream/`.
  Every new endpoint needs a contract test; every webhook event needs a golden test from `webhook-payload.md`.
- Only `src/wa/` imports `whatsapp_rust` (exposed as `wa::sdk`).
- Use diesel for SQLite, never sqlx (`libsqlite3-sys` link conflict with whatsapp-rust).
- Before committing: `cargo fmt && cargo clippy --all-targets && cargo test`.
- Docs are written in Indonesian; code and comments in English.

## Commands

```sh
APP_HOST=127.0.0.1 cargo run -- rest   # dev server on :3000, no auth needed on loopback
cargo test
cargo clippy --all-targets
```
