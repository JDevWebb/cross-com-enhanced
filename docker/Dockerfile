# The Cross-Com server in a container.
#
#   docker compose -f docker/compose.yaml up -d      (settings in docker/.env; docs/hosting.md)
#
# Build context: the repository root. Base images are pinned by digest; update
# them together (docker buildx imagetools inspect <image>).

FROM rust:1-bookworm@sha256:9181b56c2a21ab1d5e73cffe04fc762426d30aa7fac3c91141dd033f49a7fc8b AS build
WORKDIR /src
# The toolchain pinned in rust-toolchain.toml.
COPY rust-toolchain.toml .
RUN rustup show active-toolchain || rustup toolchain install
COPY . .
RUN cargo build --locked --release -p crosscom-server \
 && cp target/release/crosscom-server /crosscom-server

FROM debian:bookworm-slim@sha256:99c0b25c46d0214d5a2091092e7908907dcaecd6a962cde0ac700acee222b6f3
RUN useradd --system --uid 10001 --user-group --home-dir /srv/crosscom --no-create-home --shell /usr/sbin/nologin crosscom \
 && install -d -o crosscom -g crosscom -m 750 /srv/crosscom
COPY --from=build /crosscom-server /app/crosscom-server
# The database and the optional crosscom-server.toml live in the volume.
WORKDIR /srv/crosscom
VOLUME /srv/crosscom
USER crosscom
# 80 online config (and sign-up, when on), 21030 sign-in and 21031 game services (UDP).
# Port 80 needs net.ipv4.ip_unprivileged_port_start=80 (compose.yaml sets it).
EXPOSE 80/tcp 21030/udp 21031/udp
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s \
  CMD bash -c 'exec 3<>/dev/tcp/127.0.0.1/80' || exit 1
ENTRYPOINT ["/app/crosscom-server", "--config", "/srv/crosscom/crosscom-server.toml"]
