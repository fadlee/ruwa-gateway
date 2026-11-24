# syntax=docker/dockerfile:1.7
#
# ruwa-gateway image. Two ways to produce it, same runtime:
#
#   docker build -t ruwa-gateway .
#       Compiles from source (target `source`, the default).
#
#   docker buildx build --target prebuilt ...
#       Used by the release workflow: packs the static musl binaries that were
#       already built natively per architecture into dist/<amd64|arm64>/ruwa-gateway,
#       so multi-arch images need no QEMU-emulated compile.
#
# Run:
#   docker run -p 3000:3000 -v ruwa-data:/data \
#     -e APP_BASIC_AUTH=admin:change-me ruwa-gateway
#
# The server refuses to start on a non-loopback host without APP_BASIC_AUTH
# (or APP_ALLOW_UNAUTHENTICATED=true), as GOWA does. /data holds the session
# databases (storages/) and downloaded media (statics/).

# --- Build from source ---
FROM rust:1.97-alpine AS builder
RUN apk add --no-cache musl-dev gcc git
WORKDIR /src
COPY Cargo.toml Cargo.lock build.rs ./
COPY migrations ./migrations
COPY src ./src
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/usr/local/cargo/git \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked --features bundled \
    && cp target/release/ruwa-gateway /ruwa-gateway

# --- Runtime base ---
# ffmpeg: video/PTT transcoding, thumbnails and sticker conversion.
FROM alpine:3.22 AS runtime
RUN apk add --no-cache ca-certificates ffmpeg tini \
    && addgroup -g 10001 -S ruwa \
    && adduser -u 10001 -S -G ruwa -h /data ruwa \
    && mkdir -p /data && chown ruwa:ruwa /data
ENV APP_HOST=0.0.0.0 \
    APP_PORT=3000
WORKDIR /data
VOLUME /data
EXPOSE 3000
USER ruwa
# /health ignores APP_BASE_PATH and needs no auth.
HEALTHCHECK --interval=30s --timeout=5s --start-period=20s --retries=3 \
    CMD wget -q -O /dev/null "http://127.0.0.1:${APP_PORT}/health" || exit 1
ENTRYPOINT ["/sbin/tini", "--", "ruwa-gateway"]
CMD ["rest"]

# --- Final: prebuilt per-architecture binary (release workflow) ---
FROM runtime AS prebuilt
ARG TARGETARCH
COPY dist/${TARGETARCH}/ruwa-gateway /usr/local/bin/ruwa-gateway

# --- Final: compiled here (last stage = default target) ---
FROM runtime AS source
COPY --from=builder /ruwa-gateway /usr/local/bin/ruwa-gateway
