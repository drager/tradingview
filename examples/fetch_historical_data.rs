use std::{env, io};

use chrono::DateTime;
use tradingview::{ClientOptions, Currency, TickerSymbol, Timeframe, TradingView};

fn on_two_factor() -> String {
    println!("Enter two factor code: ");

    let mut code = String::new();
    io::stdin().read_line(&mut code).unwrap();
    code.trim().to_string()
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let client_options = ClientOptions::builder().build();

    let trading_view = TradingView::new(client_options);

    trading_view
        .login(
            "username",
            "password",
            true,
            Some(tradingview::Either::Right(on_two_factor)),
        )
        .await?;

    // Usage: fetch_historical_data <range> <currency> <ticker>...
    let mut args = env::args().skip(1);
    let range: u32 = args.next().expect("No range provided").parse()?;
    let currency = Currency::from(args.next().expect("No currency provided").as_str());
    let tickers: Vec<TickerSymbol> = args
        .map(|symbol| {
            TickerSymbol::builder()
                .symbol(symbol)
                .currency(currency.clone())
                .build()
        })
        .collect();
    assert!(!tickers.is_empty(), "No tickers provided");

    // One call (and one websocket connection) for all tickers.
    let data = trading_view
        .historical_data(&tickers, Timeframe::FiveMinutes, range)
        .await?;

    for ticker in &tickers {
        println!("== {}", ticker.get_symbol());

        let ohlcs = match &data[ticker] {
            Ok(ohlcs) => ohlcs,
            Err(e) => {
                println!("error: {e}");
                continue;
            }
        };

        for ohlc in ohlcs {
            let date = ohlc.get_timestamp();

            println!(
                "date: {}",
                DateTime::from_timestamp(date as i64, 0)
                    .ok_or_else(|| anyhow::anyhow!("Invalid date"))?
                    .format("%Y-%m-%d %H:%M:%S")
            );
            println!(
                "O: {} H: {} L: {} C: {} V: {}",
                ohlc.get_open(),
                ohlc.get_high(),
                ohlc.get_low(),
                ohlc.get_close(),
                ohlc.get_volume()
            );
        }
    }

    Ok(())
}
