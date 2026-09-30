FROM rust:1.92-bookworm AS build
ARG FEATURES=""
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY examples/depth_probe.rs ./examples/depth_probe.rs
COPY tests ./tests
COPY review/hyperliquid_spot_snapshot.json ./review/hyperliquid_spot_snapshot.json
RUN --mount=type=cache,target=/usr/local/cargo/registry --mount=type=cache,target=/app/target cargo build --release --locked --features "$FEATURES" --bin bellman-arb --example depth_probe && cp target/release/bellman-arb /usr/local/bin/bellman-arb && cp target/release/examples/depth_probe /usr/local/bin/depth-probe
RUN sha256sum src/*.rs examples/*.rs Cargo.toml Cargo.lock > SOURCE_SHA256

FROM debian:bookworm-slim AS runtime
WORKDIR /app
COPY --from=build /etc/ssl/certs /etc/ssl/certs
COPY --from=build /usr/local/bin/bellman-arb /usr/local/bin/bellman-arb
COPY --from=build /usr/local/bin/depth-probe /usr/local/bin/depth-probe
COPY --from=build /app/SOURCE_SHA256 ./SOURCE_SHA256
COPY config.toml ./config.toml
ENTRYPOINT ["bellman-arb"]
CMD ["run"]
