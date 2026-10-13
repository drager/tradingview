# TradingView

This project provides Rust bindings for leveraging TradingView functionalities, allowing Rust applications to interact with TradingView for financial data fetching, realtime subscription, and more.

## Installation

Add the following to your `Cargo.toml` file, picking one of the TLS backends
described below:

```toml
[dependencies]
tradingview = { version = "0.2", features = ["rustls"] } # or "native-tls"
```

## TLS backends

HTTPS and the WebSocket need a TLS backend, selected by feature:

- `native-tls`: OpenSSL via the system (default on most glibc setups)
- `rustls`: rustls with the ring provider, no OpenSSL, so it works in static
  musl builds. The WebSocket uses the bundled webpki roots. HTTPS uses the
  platform verifier, so the system CA store (e.g. `ca-certificates`) must be
  present at runtime

No TLS backend is enabled by default. Without one, the crate still builds, but
connecting to `wss://` and `https://` fails at runtime.

## Getting Started
Check out the [examples](./examples) folder for a quick start on how to use the library.

Run the examples with the following commands:

```bash
cargo run --features native-tls --example fetch_historical_data 300 USD NASDAQ:AAPL NASDAQ:MSFT
cargo run --features native-tls --example fetch_instruments
cargo run --features native-tls --example realtime

# Or use rustls instead of OpenSSL (no native-tls dependency, the right choice
# for static/musl builds):
cargo run --features rustls --example realtime
```
