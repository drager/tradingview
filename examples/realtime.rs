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
    let client_options = ClientOptions::builder().build();

    let trading_view = TradingView::new(client_options);

    trading_view
        .login("username", "password", true, Some(on_two_factor))
        .await
        .unwrap();

    let mut stream = trading_view
        .subscribe_to_symbols(&[
            TickerSymbol::builder()
                .symbol("CME_MINI:ES1!".to_string())
                .currency(Currency::Usd)
                .build(),
            TickerSymbol::builder()
                .symbol("OMXSTO:OMXS30".to_string())
                .currency(Currency::Sek)
                .build(),
            TickerSymbol::builder()
                .symbol("BITSTAMP:BTCUSD".to_string())
                .currency(Currency::Usd)
                .build(),
        ])
        .await?;

    while let Some(data) = stream.next().await {
        println!(
            "{}",
            format!(
                "{} {} ({} | {}%) Volume: {}",
                data.get_symbol(),
                data.get_price(),
                data.get_change(),
                data.get_change_percent().unwrap_or_default(),
                data.get_volume()
            )
        );
    }

    Ok(())
}
