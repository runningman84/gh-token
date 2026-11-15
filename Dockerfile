# Builds the gh-token server and client as static musl binaries and packages
# them in a distroless image. Set RUST_TARGET to cross-compile (e.g.
# aarch64-unknown-linux-musl).
ARG RUST_TARGET=x86_64-unknown-linux-musl

FROM rust:slim AS builder
ARG RUST_TARGET
RUN apt-get update \
    && apt-get install -y --no-install-recommends musl-tools \
    && rm -rf /var/lib/apt/lists/*

WORKDIR /src
COPY . .
ARG RUST_TARGET
ENV RUSTFLAGS="-C target-feature=+crt-static"
# Install the target for the toolchain that rust-toolchain.toml resolves to
# (it may be auto-installed on first use, missing the musl target).
RUN rustup target add ${RUST_TARGET} \
    && cargo build --release --locked --package gh-token-server --package gh-token-client --target ${RUST_TARGET}

FROM gcr.io/distroless/static:nonroot
ARG RUST_TARGET
# reqwest uses the platform certificate store (rustls-platform-verifier),
# so bundle the CA certificates.
COPY --from=builder /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/ca-certificates.crt
COPY --from=builder /src/target/${RUST_TARGET}/release/gh-token-server /usr/local/bin/gh-token-server
# Client binary for `kubectl exec ... -- gh-token ...` debugging.
COPY --from=builder /src/target/${RUST_TARGET}/release/gh-token /usr/local/bin/gh-token
USER 65532:65532
EXPOSE 8080
ENTRYPOINT ["/usr/local/bin/gh-token-server"]
