FROM rust:1.96.0-bookworm AS rust
WORKDIR /src
RUN apt-get update && apt-get install -y --no-install-recommends clang ca-certificates \
    && rm -rf /var/lib/apt/lists/*
RUN rustup target add wasm32-unknown-unknown \
    && cargo install --locked wasm-bindgen-cli --version 0.2.130
COPY . .
RUN cargo build --locked --release -p nexdesk-signaling \
    && cargo build --locked --release --target wasm32-unknown-unknown -p nexdesk-browser-core \
    && wasm-bindgen --target web --out-dir /web-core --out-name nexdesk_core \
       target/wasm32-unknown-unknown/release/nexdesk_browser_core.wasm

FROM node:24-bookworm-slim AS web
WORKDIR /web
COPY apps/web/package.json apps/web/package-lock.json ./
RUN npm ci --no-audit --no-fund
COPY apps/web ./
COPY --from=rust /web-core ./public/core
RUN npm run build

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && install -d -o 10001 -g 10001 /var/lib/nexdesk
COPY --from=rust /src/target/release/nexdesk-signaling /usr/local/bin/nexdesk-signaling
COPY --from=web /web/dist /srv/nexdesk
ENV NEXDESK_SIGNAL_LISTEN=0.0.0.0:54000 \
    NEXDESK_SIGNAL_DB=/var/lib/nexdesk/directory.sqlite3 \
    NEXDESK_WEB_ROOT=/srv/nexdesk
USER 10001:10001
EXPOSE 54000
ENTRYPOINT ["/usr/local/bin/nexdesk-signaling"]
