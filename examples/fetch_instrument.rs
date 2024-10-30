use std::{env, io};

use futures::StreamExt;
use tradingview::{ClientOptions, Currency, TradingView};

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

    let ticker = env::args().nth(1).expect("No ticker provided");
    let currency = Currency::from(env::args().nth(2).expect("No currency provided").as_str());

    let instrument = trading_view.fetch_instrument(&ticker, &currency).await?;

    println!("{:#?}", instrument);

    Ok(())
}
