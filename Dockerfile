FROM rust:1-slim-bookworm AS builder

RUN apt-get update && apt-get install -y --no-install-recommends musl-tools && rm -rf /var/lib/apt/lists/*

WORKDIR /build

# Detect architecture and add appropriate musl target
RUN ARCH=$(uname -m) && \
    if [ "$ARCH" = "x86_64" ]; then \
        echo "x86_64-unknown-linux-musl" > /target.txt; \
    elif [ "$ARCH" = "aarch64" ]; then \
        echo "aarch64-unknown-linux-musl" > /target.txt; \
    fi && \
    rustup target add $(cat /target.txt)

COPY Cargo.toml Cargo.lock* ./
RUN TARGET=$(cat /target.txt) && \
    mkdir src && echo "fn main(){}" > src/main.rs && \
    cargo build --release --target "$TARGET" && \
    rm -rf src target/$TARGET/release/deps/gd_iptv_panel* target/$TARGET/release/gd-iptv-panel*

COPY src ./src
COPY static ./static
RUN TARGET=$(cat /target.txt) && \
    touch src/main.rs && \
    cargo build --release --target "$TARGET" && \
    cp target/$TARGET/release/gd-iptv-panel /gd-iptv-panel && \
    mkdir -p /empty-data

FROM scratch

ENV IPTV_CONFIG=/data/config.json \
    RUST_LOG=info,hyper=warn,retina=warn

COPY --from=builder /empty-data /data
COPY --from=builder /gd-iptv-panel /usr/local/bin/gd-iptv-panel

EXPOSE 4022
HEALTHCHECK --interval=30s --timeout=5s --retries=3 \
  CMD ["/usr/local/bin/gd-iptv-panel", "--healthcheck", "127.0.0.1:4022"]

ENTRYPOINT ["/usr/local/bin/gd-iptv-panel"]
CMD ["-c", "/data/config.json"]
