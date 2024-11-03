# TradingView

This project provides Rust bindings for leveraging TradingView functionalities, allowing Rust applications to interact with TradingView for financial data fetching, realtime subscription, and more.

## Getting Started
Check out the [examples](./examples) folder for a quick start on how to use the library.

Run the examples with the following commands:

```bash
cargo run --features native-tls --example fetch_historical_data NDQ 20425 USD
cargo run --features native-tls --example fetch_instruments
cargo run --features native-tls --example realtime
```

### Installation

Add the following to your `Cargo.toml` file:

```toml
[dependencies]
tradingview = "0.1.0"
