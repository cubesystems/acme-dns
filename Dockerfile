FROM rust:1.95.0-bookworm AS build
RUN apt-get update && apt-get install -y --no-install-recommends cmake && rm -rf /var/lib/apt/lists/*
WORKDIR /build
COPY Cargo.toml Cargo.lock ./
COPY src ./src
RUN cargo build --locked --release

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=build /build/target/release/acme-dns /usr/local/bin/acme-dns
USER 65532:65532
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/acme-dns"]
