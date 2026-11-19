FROM rust:1-alpine AS builder

RUN apk add --no-cache musl-dev git

WORKDIR /build
# On Alpine musl, target binaries link statically by default without breaking host proc-macros
ENV CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_RUSTFLAGS="-C target-feature=+crt-static" \
    CARGO_TARGET_AARCH64_UNKNOWN_LINUX_MUSL_RUSTFLAGS="-C target-feature=+crt-static"
COPY Cargo.toml Cargo.lock* ./
RUN mkdir src && echo "fn main(){}" > src/main.rs \
 && cargo build --release \
 && rm -rf src

COPY src ./src
COPY static ./static
RUN touch src/main.rs \
 && cargo build --release \
 && mkdir -p /empty-data

FROM scratch

ENV IPTV_CONFIG=/data/config.json \
    RUST_LOG=info,hyper=warn,retina=warn

COPY --from=builder /empty-data /data
COPY --from=builder /build/target/release/gd-iptv-panel /usr/local/bin/gd-iptv-panel

EXPOSE 4022
HEALTHCHECK --interval=30s --timeout=5s --retries=3 \
  CMD ["/usr/local/bin/gd-iptv-panel", "--healthcheck", "127.0.0.1:4022"]

ENTRYPOINT ["/usr/local/bin/gd-iptv-panel"]
CMD ["-c", "/data/config.json"]
