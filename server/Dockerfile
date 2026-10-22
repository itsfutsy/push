# Needs BuildKit (the default `docker build` on any current Docker with buildx).

FROM rust:1.99-alpine3.24 AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    --mount=type=cache,target=/src/target \
    cargo build --release --locked && cp target/release/push-server /push-server

FROM gcr.io/distroless/static-debian13:nonroot
COPY --from=build /push-server /push-server
USER 65532:65532
# Created after USER, so /data (and a fresh named volume seeded from it) belongs to 65532.
WORKDIR /data
EXPOSE 3000
HEALTHCHECK --interval=10s --timeout=3s --start-period=5s --retries=3 \
    CMD ["/push-server", "healthcheck"]
ENTRYPOINT ["/push-server"]
