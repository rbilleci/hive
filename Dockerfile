# Builds the console and the service, then assembles a runtime image that holds only the binary and
# the static console build. The Fargate task definition under infra/aws/fargate-app runs ARM64:
#   docker buildx build --platform linux/arm64 -t hive-service .

# The editor bundle: CodeMirror behind the small interface the console loads on demand.
FROM node:22-bookworm-slim AS editor
WORKDIR /build
COPY package.json package-lock.json ./
COPY crates/hive-console/editor-js/package.json crates/hive-console/editor-js/package.json
RUN npm ci
COPY crates/hive-console/editor-js crates/hive-console/editor-js
RUN npm run --workspace @hive/console-editor build

# The console: Leptos compiled to WebAssembly by Trunk. cynic checks every operation against
# schema/hive.graphql at compile time, so the schema is a build input.
FROM rust:1-bookworm AS console
RUN cargo install --locked trunk --version 0.21.14
WORKDIR /build
# rust-toolchain.toml names the wasm32 target, so rustup installs it with the toolchain.
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates crates
COPY schema schema
COPY --from=editor /build/crates/hive-console/editor-js/dist crates/hive-console/editor-js/dist
RUN cd crates/hive-console && trunk build --release --cargo-profile wasm-release

# .br and .gz siblings: the server sends whichever the client accepts.
FROM node:22-bookworm-slim AS web
WORKDIR /build
COPY scripts/precompress.mjs scripts/precompress.mjs
COPY --from=console /build/crates/hive-console/dist dist
RUN node scripts/precompress.mjs dist

FROM rust:1-bookworm AS service
WORKDIR /build
COPY Cargo.toml Cargo.lock rust-toolchain.toml ./
COPY crates crates
# The migrator embeds every migration and seed file at compile time (include_str!).
COPY db db
RUN cargo build --release --locked --bin hive

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install --yes --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --uid 10001 --home-dir /srv hive
COPY --from=service /build/target/release/hive /usr/local/bin/hive
COPY --from=web /build/dist /srv/web
ENV HIVE_BIND_ADDRESS=0.0.0.0 \
    HIVE_PORT=8080 \
    HIVE_WEB_DIST=/srv/web
USER hive
WORKDIR /srv
EXPOSE 8080
ENTRYPOINT ["hive"]
CMD ["serve"]
