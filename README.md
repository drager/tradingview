# TradingView

This project provides Rust bindings for leveraging TradingView functionalities, allowing Rust applications to interact with TradingView for financial data fetching, realtime subscription, and more.

## Getting Started
Check out the [examples](./examples) folder for a quick start on how to use the library.

Run the examples with the following commands:

```bash
cargo run --features native-tls --example fetch_historical_data NDQ 20425 USD
cargo run --features native-tls --example fetch_instruments
cargo run --features native-tls --example realtime

# Or use rustls instead of OpenSSL (no native-tls dependency - right choice
# for static/musl builds):
cargo run --features rustls --example realtime

## TLS backends

HTTPS and the WebSocket need a TLS backend, selected by feature:

- `native-tls` - OpenSSL via the system (default on most glibc setups)
- `rustls` - rustls with the ring provider, no OpenSSL, so it works in static
  musl builds. The WebSocket uses the bundled webpki roots; HTTPS uses the
  platform verifier, so the system CA store (e.g. `ca-certificates`) must be
  present at runtime

There is no TLS by default: build fails to reach `wss://`/`https://` unless
one of the two features is enabled.
```

### Installation

Add the following to your `Cargo.toml` file:

```toml
[dependencies]
tradingview = "0.1.0"
