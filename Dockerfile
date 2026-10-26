# syntax=docker/dockerfile:1

FROM rust:1.97.0-slim-bookworm AS wallet-wasm
ENV CARGO_BUILD_JOBS=2 CARGO_INCREMENTAL=0 \
    CARGO_PROFILE_DEV_DEBUG=0 CARGO_PROFILE_TEST_DEBUG=0 CARGO_PROFILE_RELEASE_DEBUG=0
RUN apt-get update && apt-get install -y --no-install-recommends \
    build-essential pkg-config libssl-dev ca-certificates util-linux \
    && rm -rf /var/lib/apt/lists/*
RUN rustup target add wasm32-unknown-unknown
# This must match crates/mmx-wallet-wasm and scripts/build-wasm.sh.
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    nice -n 19 ionice -c 3 cargo install wasm-bindgen-cli --version 0.2.127 \
    --locked --jobs 2 --root /opt/wasm-bindgen
ENV PATH="/opt/wasm-bindgen/bin:${PATH}"
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY crates/ crates/
COPY vendor/vnx-rust/ vendor/vnx-rust/
COPY apps/wallet/scripts/build-wasm.sh apps/wallet/scripts/build-wasm.sh
RUN --mount=type=cache,target=/usr/local/cargo/registry,sharing=locked \
    --mount=type=cache,target=/build/target/wallet,sharing=locked \
    bash apps/wallet/scripts/build-wasm.sh

FROM node:24.18.1-bookworm-slim AS web-build
ENV GOMAXPROCS=2 UV_THREADPOOL_SIZE=2
RUN apt-get update && apt-get install -y --no-install-recommends util-linux \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /build/apps/wallet
# Wait for the Rust build before starting frontend work: keep build concurrency modest.
COPY --from=wallet-wasm /build/apps/wallet/src/backend/web/pkg/ src/backend/web/pkg/
COPY apps/wallet/package.json apps/wallet/package-lock.json ./
RUN --mount=type=cache,target=/root/.npm,sharing=locked \
    nice -n 19 ionice -c 3 npm ci --no-audit --no-fund
COPY apps/wallet/ ./
ARG VITE_RPC_URL=https://rpc.mmx.network
ENV VITE_RPC_URL=${VITE_RPC_URL}
RUN nice -n 19 ionice -c 3 npm run build:web
WORKDIR /build/apps/explorer
COPY apps/explorer/package.json apps/explorer/package-lock.json ./
RUN --mount=type=cache,target=/root/.npm,sharing=locked \
    nice -n 19 ionice -c 3 npm ci --no-audit --no-fund
COPY apps/explorer/ ./
RUN nice -n 19 ionice -c 3 npm run build
WORKDIR /build/apps/website
COPY apps/website/package.json apps/website/package-lock.json ./
RUN --mount=type=cache,target=/root/.npm,sharing=locked \
    nice -n 19 ionice -c 3 npm ci --no-audit --no-fund
COPY apps/website/ ./
RUN nice -n 19 ionice -c 3 npm run build
COPY deploy/apps/ /build/deploy/apps/
RUN nice -n 19 ionice -c 3 node /build/deploy/apps/prepare.mjs /build

FROM nginx:1.30.6-alpine AS runtime
LABEL org.opencontainers.image.title="mmx-apps" \
    org.opencontainers.image.description="MMX website, web wallet and explorer HTTP origin" \
    org.opencontainers.image.source="https://github.com/mmx-network/mmx-rust-node"
COPY --from=web-build /build/apps/wallet/dist/web/ /srv/mmx/wallet/
COPY --from=web-build /build/apps/explorer/dist/ /srv/mmx/explorer/
COPY --from=web-build /build/apps/website/dist/ /srv/mmx/website/
COPY --from=web-build /build/deploy/apps/generated/ /etc/nginx/mmx/
COPY deploy/apps/nginx.conf.template deploy/apps/locations.conf /etc/nginx/mmx/
COPY --chmod=755 deploy/apps/entrypoint.sh /usr/local/bin/mmx-apps-entrypoint
ENV PORT=8080 WALLET_HOST=wallet.mmx.network EXPLORER_HOST=explore.mmx.network \
    WEBSITE_HOST=mmx.network WEBSITE_WWW_HOST=www.mmx.network
USER nginx
EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=3s --start-period=5s --retries=3 \
    CMD wget -q -O /dev/null "http://127.0.0.1:${PORT}/healthz" || exit 1
ENTRYPOINT ["/usr/local/bin/mmx-apps-entrypoint"]
CMD ["nginx", "-e", "stderr", "-c", "/tmp/mmx-apps/nginx.conf", "-g", "daemon off;"]
