# Builds the gh-token server as a static musl binary and packages it in a
# distroless image. Set RUST_TARGET to cross-compile (e.g.
# aarch64-unknown-linux-musl).
ARG RUST_TARGET=x86_64-unknown-linux-musl

FROM rust:stable-slim AS builder
ARG RUST_TARGET
RUN apt-get update \
    && apt-get install -y --no-install-recommends musl-tools \
    && rm -rf /var/lib/apt/lists/* \
    && rustup target add ${RUST_TARGET}

WORKDIR /src
COPY . .
ARG RUST_TARGET
ENV RUSTFLAGS="-C target-feature=+crt-static"
RUN cargo build --release --locked --package gh-token-server --target ${RUST_TARGET}

FROM gcr.io/distroless/static:nonroot
ARG RUST_TARGET
# reqwest uses the platform certificate store (rustls-platform-verifier),
# so bundle the CA certificates.
COPY --from=builder /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
COPY --from=builder /src/target/${RUST_TARGET}/release/gh-token-server /usr/local/bin/gh-token-server
USER 65532:65532
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/gh-token-server"]
