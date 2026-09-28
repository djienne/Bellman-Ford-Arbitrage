FROM rust:1.92-bookworm AS build
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY tests ./tests
COPY review/hyperliquid_spot_snapshot.json ./review/hyperliquid_spot_snapshot.json
RUN --mount=type=cache,target=/usr/local/cargo/registry --mount=type=cache,target=/app/target cargo build --release --locked && cp target/release/bellman-arb /usr/local/bin/bellman-arb
RUN sha256sum src/*.rs Cargo.toml Cargo.lock > SOURCE_SHA256

FROM debian:bookworm-slim AS runtime
WORKDIR /app
COPY --from=build /etc/ssl/certs /etc/ssl/certs
COPY --from=build /usr/local/bin/bellman-arb /usr/local/bin/bellman-arb
COPY --from=build /app/SOURCE_SHA256 ./SOURCE_SHA256
COPY config.toml ./config.toml
ENTRYPOINT ["bellman-arb"]
CMD ["run"]
