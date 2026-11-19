# BIN Lookup Service (Rust)

A Rust rewrite of `denobinlookup` built with Axum and Tokio, backed by the [`venelinkochev/bin-list-data`](https://github.com/venelinkochev/bin-list-data) CSV dataset.

## Features

- Loads `data/bin-list-data.csv` (~375k BIN records) or a precompiled `BINLKUP1` binary snapshot (`--compile <input.csv> <output.bin>`) into a compact sorted in-memory table at startup
- Maps CSV columns by header name rather than position
- Serves the single-page web UI (`/`), favicon (`/favicon.ico`), service worker (`/service-worker.js`), and health check (`/healthz`)
- Exposes `GET /api?issuer=<bin>` returning a JSON array of matching BIN metadata (`[]` when not found, `400` JSON error for malformed query strings or `issuer` outside 1–8 numeric digits, `404` when `issuer` is omitted)
- Cloud Run-ready `Dockerfile` that compiles `data/bin-list-data.bin` at build time, supports `PORT`, and handles graceful `SIGTERM` / `Ctrl+C` shutdown

## Running Locally

```sh
cargo run --release
```

Then open `http://localhost:8080` in your browser.

### Configuration

| Variable | Default | Description |
| --- | --- | --- |
| `HOST` | `0.0.0.0` | Bind address |
| `PORT` | `8080` | Listening TCP port |
| `BIN_DATA_PATH` | `data/bin-list-data.csv` | Path to the BIN CSV dataset |
| `RUST_LOG` | `info` | Tracing log filter |

## Docker / Cloud Run

```sh
docker build -t binlookup .
docker run --rm -p 8080:8080 binlookup
```

## Tests

```sh
cargo test
```

## Data Attribution

BIN dataset provided by [`venelinkochev/bin-list-data`](https://github.com/venelinkochev/bin-list-data) under [CC-BY-4.0](data/LICENSE).
