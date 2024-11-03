use std::io;

use futures::StreamExt;
use tradingview::{ClientOptions, Currency, TickerSymbol, TradingView};

fn on_two_factor() -> String {
    println!("Enter two factor code: ");

    let mut code = String::new();
    io::stdin().read_line(&mut code).unwrap();
    code.trim().to_string()
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::init();

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

    let tickers = &[
        TickerSymbol::builder()
            .symbol("OMXSTO:SAND".to_string())
            .currency(Currency::Sek)
            .build(),
        TickerSymbol::builder()
            .symbol("OMXSTO:ERIC_B".to_string())
            .currency(Currency::Sek)
            .build(),
        TickerSymbol::builder()
            .symbol("NASDAQ:AAPL".to_string())
            .currency(Currency::Usd)
            .build(),
        TickerSymbol::builder()
            .symbol("COINBASE:BTCUSD".to_string())
            .currency(Currency::Usd)
            .build(),
        TickerSymbol::builder()
            .symbol("EUREX:FDAX1!".to_string())
            .currency(Currency::Eur)
            .build(),
    ];

    let mut instruments = trading_view.fetch_instruments(tickers).await?;

    while let Some(instrument) = instruments.next().await {
        let instrument = instrument.unwrap();
        println!("{:?}", instrument);
    }

    Ok(())
}
