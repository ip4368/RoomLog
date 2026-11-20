FROM rust:bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY static ./static
RUN cargo build --release --locked

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --create-home roomlog \
    && mkdir -p /data && chown roomlog:roomlog /data
COPY --from=build /src/target/release/roomlog /usr/local/bin/roomlog
USER roomlog
VOLUME /data
ENV DATABASE_PATH=/data/roomlog.db BIND=0.0.0.0:8080
EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s \
    CMD curl -f http://localhost:8080/healthz || exit 1
ENTRYPOINT ["/usr/local/bin/roomlog"]
