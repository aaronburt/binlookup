FROM rust:1-bookworm AS builder
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src assets && \
    printf '' > src/lib.rs && \
    printf 'fn main() {}\n' > src/main.rs && \
    printf '' > assets/index.html && \
    printf '' > assets/service-worker.js && \
    printf '' > assets/favicon.ico && \
    cargo build --release --locked && \
    rm -rf src assets
COPY src ./src
COPY assets ./assets
COPY data/bin-list-data.csv ./data/bin-list-data.csv
RUN touch src/lib.rs src/routes.rs src/main.rs && \
    cargo build --release --locked && \
    /app/target/release/binlookup --compile /app/data/bin-list-data.csv /app/data/bin-list-data.bin

FROM debian:bookworm-slim
RUN useradd --create-home --shell /usr/sbin/nologin appuser
WORKDIR /app
COPY --from=builder /app/target/release/binlookup /usr/local/bin/binlookup
COPY --from=builder /app/data/bin-list-data.bin ./data/bin-list-data.bin
COPY data/LICENSE ./data/LICENSE
ENV HOST=0.0.0.0
ENV PORT=8080
ENV BIN_DATA_PATH=/app/data/bin-list-data.bin
USER appuser
EXPOSE 8080
CMD ["binlookup"]
