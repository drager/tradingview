use std::{env, io};

use futures::StreamExt;
use tradingview::{ClientOptions, Currency, Timeframe, TradingView};

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
        .await?;

    let ticker = env::args().nth(1).expect("No ticker provided");
    let range: u32 = env::args().nth(2).expect("No range provided").parse()?;
    let currency = Currency::from(env::args().nth(3).expect("No currency provided").as_str());

    let mut data = trading_view
        .historical_data(&ticker, Timeframe::FiveMinutes, range, &currency)
        .await?
        .collect::<Vec<_>>()
        .await;

    for ohlc in data.iter_mut() {
        let date = ohlc.get_timestamp();

        println!("date: {}", date);
        println!(
            "{} {} {} {} {} {}",
            &ohlc.get_timestamp(),
            &ohlc.get_open(),
            &ohlc.get_high(),
            &ohlc.get_low(),
            &ohlc.get_close(),
            &ohlc.get_volume()
        );
    }

    Ok(())
}
