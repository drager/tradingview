use chrono::{DateTime, Utc};
use futures::lock::Mutex;
use futures::stream;
use futures::Stream;
use futures::StreamExt;

use reqwest::header::HeaderMap;
use reqwest::header::HeaderValue;
use reqwest::multipart;

use reqwest::StatusCode;
use reqwest::Url;
use serde::Deserializer;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;
use std::str;
use std::sync::Arc;
use thiserror::Error;
use typed_builder::TypedBuilder;
use websocket::WebSocketClient;

mod websocket;

#[derive(Debug, Serialize, Deserialize, TypedBuilder)]
pub struct TickerSymbol {
    symbol: String,
    currency: Currency,
}

#[derive(Debug, Clone, Serialize, Deserialize, TypedBuilder)]
pub struct Ohlc {
    open: f64,
    high: f64,
    low: f64,
    close: f64,
    volume: f64,
    timestamp: u64,
}

impl Ohlc {
    pub fn get_open(&self) -> f64 {
        self.open
    }

    pub fn get_high(&self) -> f64 {
        self.high
    }

    pub fn get_low(&self) -> f64 {
        self.low
    }

    pub fn get_close(&self) -> f64 {
        self.close
    }

    pub fn get_volume(&self) -> f64 {
        self.volume
    }

    pub fn get_timestamp(&self) -> u64 {
        self.timestamp
    }
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "UPPERCASE")]
pub enum Currency {
    Eur,
    Usd,
    Sek,
}

impl From<&str> for Currency {
    fn from(s: &str) -> Self {
        match s {
            "EUR" | "eur" => Currency::Eur,
            "USD" | "usd" => Currency::Usd,
            "SEK" | "sek" => Currency::Sek,
            _ => panic!("Unknown currency: {}", s),
        }
    }
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
pub enum Timeframe {
    OneMinute,
    ThreeMinutes,
    FiveMinutes,
    FifteenMinutes,
    ThirtyMinutes,
    FortyFiveMinutes,
    OneHour,
    TwoHours,
    ThreeHours,
    FourHours,
    OneDay,
    OneWeek,
    OneMonth,
}

impl fmt::Display for Timeframe {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let timeframe = match self {
            Timeframe::OneMinute => "1",
            Timeframe::ThreeMinutes => "3",
            Timeframe::FiveMinutes => "5",
            Timeframe::FifteenMinutes => "15",
            Timeframe::ThirtyMinutes => "30",
            Timeframe::FortyFiveMinutes => "45",
            Timeframe::OneHour => "60",
            Timeframe::TwoHours => "120",
            Timeframe::ThreeHours => "180",
            Timeframe::FourHours => "240",
            Timeframe::OneDay => "1D",
            Timeframe::OneWeek => "1W",
            Timeframe::OneMonth => "1M",
        };

        write!(f, "{}", timeframe)
    }
}

impl fmt::Display for Currency {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let currency = match self {
            Currency::Eur => "EUR",
            Currency::Usd => "USD",
            Currency::Sek => "SEK",
        };

        write!(f, "{}", currency)
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct User {
    id: u32,
    username: String,
    first_name: String,
    last_name: String,
    reputation: f64,
    following: Option<u32>,
    followers: Option<u32>,
    is_pro: bool,
    #[serde(rename = "notification_count")]
    notifications: NotificationCount,
    session_hash: String,
    private_channel: String,
    auth_token: String,

    #[serde(deserialize_with = "deserialize_datetime")]
    date_joined: DateTime<Utc>,
    active_broker: Option<String>,

    session: Option<String>,
    signature: Option<String>,
}

impl User {
    pub fn get_id(&self) -> u32 {
        self.id
    }

    pub fn get_username(&self) -> &str {
        &self.username
    }

    pub fn get_first_name(&self) -> &str {
        &self.first_name
    }

    pub fn get_last_name(&self) -> &str {
        &self.last_name
    }

    pub fn get_reputation(&self) -> f64 {
        self.reputation
    }

    pub fn get_following(&self) -> Option<u32> {
        self.following
    }

    pub fn get_followers(&self) -> Option<u32> {
        self.followers
    }

    pub fn is_pro(&self) -> bool {
        self.is_pro
    }

    pub fn get_notifications(&self) -> u32 {
        self.notifications.user
    }

    pub fn get_session_hash(&self) -> &str {
        &self.session_hash
    }

    pub fn get_private_channel(&self) -> &str {
        &self.private_channel
    }

    pub fn get_auth_token(&self) -> &str {
        &self.auth_token
    }

    pub fn get_date_joined(&self) -> &DateTime<Utc> {
        &self.date_joined
    }

    pub fn get_active_broker(&self) -> Option<&str> {
        self.active_broker.as_deref()
    }

    pub fn get_session(&self) -> Option<&str> {
        self.session.as_deref()
    }

    pub fn get_signature(&self) -> Option<&str> {
        self.signature.as_deref()
    }
}

fn deserialize_datetime_string(date_str: &str) -> anyhow::Result<DateTime<Utc>> {
    let date_str = if date_str.ends_with('Z') {
        date_str.to_string()
    } else {
        format!("{}Z", date_str)
    };

    Ok(DateTime::parse_from_rfc3339(&date_str).map(|d| d.with_timezone(&Utc))?)
}

fn deserialize_datetime<'de, D>(deserializer: D) -> Result<DateTime<Utc>, D::Error>
where
    D: Deserializer<'de>,
{
    let date_str = String::deserialize(deserializer)?;

    deserialize_datetime_string(&date_str).map_err(serde::de::Error::custom)
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Default)]
struct NotificationCount {
    user: u32,
}

#[derive(Deserialize)]
struct UserResponse {
    user: User,
}

#[derive(Error, Debug)]
pub enum TradingViewError {
    #[error("Failed to authenticate user.")]
    AuthenticationFailed,

    #[error("Invalid username or password.")]
    InvalidCredentials,

    #[error("Internal server error.")]
    InternalServerError,

    #[error("reqwest error")]
    ReqwestError(#[from] reqwest::Error),

    #[error(transparent)]
    ChronoParseError(#[from] chrono::ParseError),
}

#[derive(Debug, Deserialize, Serialize)]
pub struct TradingViewLastPrice {
    symbol: String,
    price: f64,
    timestamp: u64,
    change: f64,
    change_percent: Option<f64>,
    volume: f64,
}

impl TradingViewLastPrice {
    pub fn get_symbol(&self) -> &str {
        &self.symbol
    }

    pub fn get_price(&self) -> f64 {
        self.price
    }

    pub fn get_timestamp(&self) -> u64 {
        self.timestamp
    }

    pub fn get_change(&self) -> f64 {
        self.change
    }

    pub fn get_change_percent(&self) -> Option<f64> {
        self.change_percent
    }

    pub fn get_volume(&self) -> f64 {
        self.volume
    }
}

pub struct TradingView {
    client_options: ClientOptions,
    user: Arc<Mutex<Option<User>>>,
}

#[derive(TypedBuilder)]
pub struct ClientOptions {
    #[builder(default)]
    server: Option<String>,
    #[builder(default)]
    user_agent: Option<String>,
}

impl TradingView {
    const SERVER_URL: &'static str = "https://www.tradingview.com";

    pub fn new(client_options: ClientOptions) -> Self {
        TradingView {
            client_options,
            user: Arc::new(Mutex::new(None)),
        }
    }

    pub async fn login(
        &self,
        username: &str,
        password: &str,
        remember: bool,
    ) -> anyhow::Result<User> {
        let server_url = self
            .client_options
            .server
            .as_deref()
            .unwrap_or(Self::SERVER_URL);

        let mut form = multipart::Form::new()
            .text("username", username.to_string())
            .text("password", password.to_string());

        if remember {
            form = form.text("remember", "on".to_string());
        }

        let client = reqwest::Client::new();
        let res = client
            .post(&format!("{}/accounts/signin/", server_url))
            .header(
                "User-agent",
                self.client_options.user_agent.as_deref().unwrap_or(concat!(
                    env!("CARGO_PKG_NAME"),
                    " API ",
                    "/",
                    env!("CARGO_PKG_VERSION")
                )),
            )
            .header("Referer", format!("{}/", server_url))
            .multipart(form)
            .send()
            .await?;

        match res.status() {
            StatusCode::UNAUTHORIZED => Err(TradingViewError::InvalidCredentials.into()),
            StatusCode::INTERNAL_SERVER_ERROR => Err(TradingViewError::InternalServerError.into()),
            s if !s.is_success() => Err(TradingViewError::AuthenticationFailed.into()),
            _ => {
                let cookies = res.cookies().collect::<Vec<_>>();

                let session_cookie = cookies
                    .iter()
                    .find(|c| c.name() == "sessionid")
                    .ok_or(TradingViewError::InvalidCredentials)?;
                let session = session_cookie.value().to_string();

                let sign_cookie = cookies
                    .iter()
                    .find(|c| c.name() == "sessionid_sign")
                    .ok_or(TradingViewError::InvalidCredentials)?;
                let signature = sign_cookie.value().to_string();

                let user_response: Value = res.json().await?;

                if let Some(error) = user_response.get("error") {
                    if error == "2FA_required" {
                        log::info!("2FA required...");

                        let user = User {
                            session: Some(session),
                            signature: Some(signature),
                            ..Default::default()
                        };

                        self.user.lock().await.replace(user.clone());

                        return self.two_factor("").await;
                    }
                    return Err(TradingViewError::InvalidCredentials.into());
                }

                self.get_user_from_cookie(&user_response, &session, &signature)
                    .await
            }
        }
    }

    async fn get_user_from_cookie(
        &self,
        res: &Value,
        session: &str,
        signature: &str,
    ) -> anyhow::Result<User> {
        let user_response: UserResponse = serde_json::from_value(res.clone())?;

        let mut user = user_response.user;

        user.session = Some(session.to_owned());
        user.signature = Some(signature.to_owned());

        self.user.lock().await.replace(user.clone());

        Ok(user)
    }

    pub async fn two_factor(&self, code: &str) -> anyhow::Result<User> {
        let server_url = self
            .client_options
            .server
            .as_deref()
            .unwrap_or(Self::SERVER_URL);

        let form = multipart::Form::new().text("code", code.to_string());
        let user = self.user.lock().await;

        let user = user.as_ref().ok_or(TradingViewError::InvalidCredentials)?;

        let session = user
            .get_session()
            .ok_or(TradingViewError::InvalidCredentials)?;

        let signature = user
            .get_signature()
            .ok_or(TradingViewError::InvalidCredentials)?;

        let session_cookie = format!("sessionid={};sessionid_sign={};", session, signature);

        let cookies = vec![HeaderValue::from_str(&session_cookie)?];

        let mut headers = HeaderMap::new();
        headers.insert("Cookie", cookies[0].clone());
        headers.insert(
            "Referer",
            HeaderValue::from_str(&format!("{}/", server_url))?,
        );
        headers.insert(
            "User-agent",
            HeaderValue::from_str(self.client_options.user_agent.as_deref().unwrap_or(concat!(
                env!("CARGO_PKG_NAME"),
                " API ",
                "/",
                env!("CARGO_PKG_VERSION")
            )))?,
        );

        let client = reqwest::Client::new();
        let res = client
            .post(&format!("{}/accounts/two-factor/signin/totp/", server_url))
            .headers(headers)
            .multipart(form)
            .send()
            .await?;

        match res.status() {
            StatusCode::UNAUTHORIZED => Err(TradingViewError::InvalidCredentials.into()),
            StatusCode::INTERNAL_SERVER_ERROR => Err(TradingViewError::InternalServerError.into()),
            s if !s.is_success() => Err(TradingViewError::AuthenticationFailed.into()),
            _ => {
                let cookies = res.cookies().collect::<Vec<_>>();
                let session_cookie = cookies
                    .iter()
                    .find(|c| c.name() == "sessionid")
                    .ok_or(TradingViewError::InvalidCredentials)?;
                let session = session_cookie.value().to_string();

                let sign_cookie = cookies
                    .iter()
                    .find(|c| c.name() == "sessionid_sign")
                    .ok_or(TradingViewError::InvalidCredentials)?;
                let signature = sign_cookie.value().to_string();

                let user_response: Value = res.json().await?;

                if let Some(_error) = user_response.get("error") {
                    return Err(TradingViewError::InvalidCredentials.into());
                }

                self.get_user_from_cookie(&user_response, &session, &signature)
                    .await
            }
        }
    }

    pub async fn provide_user(&self, user: User) {
        self.user.lock().await.replace(user);
    }

    pub async fn get_user(&self, session: &str, signature: &str) -> anyhow::Result<User> {
        let url = Url::parse(
            self.client_options
                .server
                .as_deref()
                .unwrap_or(Self::SERVER_URL),
        )?;

        let client = reqwest::Client::new();

        let session_cookie = format!("sessionid={};sessionid_sign={};", session, signature);

        let cookies = vec![HeaderValue::from_str(&session_cookie)?];

        let mut headers = HeaderMap::new();
        headers.insert("Cookie", cookies[0].clone());

        let response = client.get(url).headers(headers).send().await?;

        let response_body = response.text().await?;

        if response_body.contains("auth_token") {
            let id = response_body
                .split("id\":")
                .nth(1)
                .unwrap()
                .split(',')
                .next()
                .unwrap()
                .parse::<u32>()
                .unwrap();

            let username = response_body
                .split("username\":\"")
                .nth(1)
                .unwrap()
                .split('\"')
                .next()
                .unwrap()
                .to_string();

            let first_name = response_body
                .split("first_name\":\"")
                .nth(1)
                .unwrap()
                .split('\"')
                .next()
                .unwrap()
                .to_string();

            let last_name = response_body
                .split("last_name\":\"")
                .nth(1)
                .unwrap()
                .split('\"')
                .next()
                .unwrap()
                .to_string();

            let reputation = response_body
                .split("reputation\":")
                .nth(1)
                .unwrap()
                .split(',')
                .next()
                .unwrap()
                .parse::<f64>()
                .unwrap();

            let following = response_body
                .split("following\":")
                .nth(1)
                .unwrap()
                .split(',')
                .next()
                .unwrap()
                .parse::<u32>()
                .unwrap();

            let followers = response_body
                .split("followers\":")
                .nth(1)
                .unwrap()
                .split(',')
                .next()
                .unwrap()
                .parse::<u32>()
                .unwrap();

            let is_pro = response_body
                .split("is_pro\":")
                .nth(1)
                .unwrap()
                .split(',')
                .next()
                .unwrap()
                .parse::<bool>()
                .unwrap();

            let notifications = response_body
                .split("notification_count\":")
                .nth(1)
                .unwrap()
                .split(',')
                .nth(1)
                .unwrap()
                .split("user\":")
                .nth(1)
                .unwrap()
                .replace('}', "")
                .parse::<u32>()
                .unwrap();

            let session_hash = response_body
                .split("session_hash\":\"")
                .nth(1)
                .unwrap()
                .split('\"')
                .next()
                .unwrap()
                .to_string();

            let private_channel = response_body
                .split("private_channel\":\"")
                .nth(1)
                .unwrap()
                .split('\"')
                .next()
                .unwrap()
                .to_string();

            let auth_token = response_body
                .split("auth_token\":\"")
                .nth(1)
                .unwrap()
                .split('\"')
                .next()
                .unwrap()
                .to_string();

            let date_joined = response_body
                .split("date_joined\":\"")
                .nth(1)
                .unwrap()
                .split('\"')
                .next()
                .unwrap()
                .to_string();

            Ok(User {
                id,
                username,
                first_name,
                last_name,
                session: Some(session.to_string()),
                signature: Some(signature.to_string()),
                reputation,
                following: Some(following),
                followers: Some(followers),
                is_pro,
                notifications: NotificationCount {
                    user: notifications,
                },
                session_hash,
                private_channel,
                auth_token,
                date_joined: deserialize_datetime_string(&date_joined)?,
                active_broker: None,
            })
        } else {
            Err(TradingViewError::AuthenticationFailed.into())
        }
    }

    pub async fn subscribe_to_symbols(
        &self,
        ticker_symbols: &[TickerSymbol],
    ) -> anyhow::Result<Box<dyn Stream<Item = TradingViewLastPrice> + Unpin + Send>> {
        let websocket = WebSocketClient::new(self.user.clone()).await?;

        Ok(Box::new(
            websocket
                .subscribe(ticker_symbols)
                .await?
                .filter_map(move |tv_packet| {
                    if tv_packet.packet_type == "qsd"
                        && tv_packet.data.is_some()
                        && tv_packet.data.clone().unwrap()[1]
                            .as_object()
                            .unwrap()
                            .get("s")
                            .unwrap()
                            .as_str()
                            .unwrap()
                            == "ok"
                    {
                        futures::future::ready(tv_packet.data.and_then(|mut d| d.pop()).and_then(
                            |data| {
                                data.as_object().and_then(|data| {
                                    let symbol = serde_json::from_str::<Value>(
                                        data.get("n").and_then(|s| s.as_str()).unwrap_or_default(),
                                    )
                                    .map(|s| {
                                        s.get("symbol")
                                            .and_then(|s| s.as_str())
                                            .unwrap_or_default()
                                            .to_string()
                                    })
                                    .ok();
                                    let v = data.get("v").and_then(|v| v.as_object());
                                    let price = v?.get("lp").and_then(|p| p.as_f64());
                                    let change = v?.get("ch").and_then(|c| c.as_f64());
                                    let change_percent = v?.get("chp").and_then(|c| c.as_f64());
                                    let timestamp = v?
                                        .get("lp_time")
                                        .and_then(|t| t.as_u64())
                                        .unwrap_or(chrono::Utc::now().timestamp_millis() as u64);

                                    let volume = v?
                                        .get("volume")
                                        .and_then(|v| v.as_f64())
                                        .unwrap_or_default();

                                    match (symbol, price, change) {
                                        (Some(symbol), Some(price), Some(change)) => {
                                            Some(TradingViewLastPrice {
                                                symbol,
                                                price,
                                                change,
                                                change_percent,
                                                timestamp,
                                                volume,
                                            })
                                        }
                                        _ => None,
                                    }
                                })
                            },
                        ))
                    } else {
                        futures::future::ready(None)
                    }
                }),
        ))
    }

    pub async fn historical_data(
        &self,
        ticker_symbol: &str,
        timeframe: Timeframe,
        range: u32,
        currency: &Currency,
    ) -> anyhow::Result<Box<dyn Stream<Item = Ohlc> + Unpin + Send + '_>> {
        let websocket = WebSocketClient::new(self.user.clone()).await?;

        let mut data = websocket
            .historical_data(ticker_symbol, timeframe, range, currency)
            .await?;

        let mut items = vec![];

        while let Some(tv_packet) = data.next().await {
            if tv_packet.packet_type == "timescale_update" && tv_packet.data.is_some() {
                let prices = tv_packet
                    .data
                    .and_then(|d| d.get(1).cloned())
                    .and_then(|data| {
                        data.as_object()
                            .cloned()
                            .and_then(|prices| prices.get("$prices").cloned())
                            .and_then(|data| data.get("s").map(|s| s.as_array().cloned()))
                            .map(|items| {
                                items
                                    .unwrap()
                                    .into_iter()
                                    .flat_map(|item| {
                                        item.get("v").and_then(|v| v.as_array()).cloned()
                                    })
                                    .collect::<Vec<_>>()
                            })
                    })
                    .unwrap_or_default();

                let prices = prices
                    .iter()
                    .map(|price_row| {
                        Ohlc::builder()
                            .timestamp(
                                price_row
                                    .first()
                                    .and_then(|timestamp| timestamp.as_f64())
                                    .unwrap_or(0.0) as u64,
                            )
                            .open(
                                price_row
                                    .get(1)
                                    .and_then(|open| open.as_f64())
                                    .unwrap_or(0.0),
                            )
                            .high(
                                price_row
                                    .get(2)
                                    .and_then(|open| open.as_f64())
                                    .unwrap_or(0.0),
                            )
                            .low(
                                price_row
                                    .get(3)
                                    .and_then(|open| open.as_f64())
                                    .unwrap_or(0.0),
                            )
                            .close(
                                price_row
                                    .get(4)
                                    .and_then(|open| open.as_f64())
                                    .unwrap_or(0.0),
                            )
                            .volume(
                                price_row
                                    .get(5)
                                    .and_then(|open| open.as_f64())
                                    .unwrap_or(0.0),
                            )
                            .build()
                    })
                    .collect::<Vec<_>>();

                items.extend(prices);

                break;
            }
        }

        Ok(Box::new(Box::pin(stream::iter(items))))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use httpmock::Method::POST;
    use httpmock::MockServer;
    use serde_json::json;

    #[tokio::test]
    async fn test_login_user() {
        let server = MockServer::start();

        server.mock(|when, then| {
            when.method(POST).path("/accounts/signin/");
            then.status(200)
                .header("Set-Cookie", "sessionid=abcd")
                .header("Set-Cookie", "sessionid_sign=efgh")
                .json_body(json!({
                    "user": {
                        "id": 123,
                        "username": "test",
                        "first_name": "Test",
                        "last_name": "User",
                        "reputation": 5.0,
                        "following": 10,
                        "followers": 20,
                        "is_pro": true,
                        "notification_count": {
                            "user": 2
                        },
                        "session_hash": "ijkl",
                        "private_channel": "mnop",
                        "auth_token": "qrst",
                        "date_joined": "2023-06-01T00:00:00Z"
                    }
                }));
        });

        let tv = TradingView::new(
            ClientOptions::builder()
                .server(Some(server.url("")))
                .build(),
        );
        let user = tv.login("username", "password", false).await.unwrap();

        assert_eq!(user.id, 123);
        assert_eq!(user.username, "test");
        assert_eq!(user.first_name, "Test");
        assert_eq!(user.last_name, "User");
        assert_eq!(user.reputation, 5.0);
        assert_eq!(user.following, Some(10));
        assert_eq!(user.followers, Some(20));
        assert_eq!(user.notifications, NotificationCount { user: 2 });
        assert_eq!(user.session, Some("abcd".to_string()));
        assert_eq!(user.signature, Some("efgh".to_string()));
        assert_eq!(user.session_hash, "ijkl");
        assert_eq!(user.private_channel, "mnop");
        assert_eq!(user.auth_token, "qrst");
    }

    #[tokio::test]
    async fn test_login_user_error() {
        let server = MockServer::start();

        // Simulate an incorrect username or password error
        server.mock(|when, then| {
            when.method(POST).path("/accounts/signin/");
            then.status(401) // Unauthorized
                .json_body(json!({
                    "error": "Invalid username or password."
                }));
        });

        let result = TradingView::new(
            ClientOptions::builder()
                .server(Some(server.url("")))
                .build(),
        )
        .login("wrong_username", "wrong_password", true)
        .await;
        assert!(result.is_err());

        let err = result.err().unwrap();
        assert_eq!(format!("{}", err), "Invalid username or password.");
    }

    #[tokio::test]
    async fn test_login_user_missing_cookies() {
        // Create a mock server
        let server = MockServer::start();

        server.mock(|when, then| {
            when.method(POST).path("/accounts/signin/");
            then.status(200).json_body(json!({
                "user": {
                    "id": 123,
                    "username": "test",
                    "first_name": "Test",
                    "last_name": "User",
                    "reputation": 5.0,
                    "following": 10,
                    "followers": 20,
                    "is_pro": true,
                    "notification_count": {
                        "user": 2
                    },
                    "session_hash": "ijkl",
                    "private_channel": "mnop",
                    "auth_token": "qrst",
                    "date_joined": "2023-06-01T00:00:00Z"
                }
            }));
        });

        // Make the login_user request
        let result = TradingView::new(
            ClientOptions::builder()
                .server(Some(server.url("")))
                .build(),
        )
        .login("username", "password", true)
        .await;

        assert!(result.is_err());
    }

    #[test]
    fn test_deserialize_datetime() {
        let json = r#"{
            "id": 123,
            "username": "test",
            "first_name": "Test",
            "last_name": "User",
            "reputation": 5.0,
            "following": 10,
            "followers": 20,
            "is_pro": true,
            "notification_count": {
                "user": 2
            },
            "session_hash": "ijkl",
            "private_channel": "mnop",
            "auth_token": "qrst",
            "date_joined": "2022-02-28T06:16:18.735255"
        }"#;

        let expected_datetime = DateTime::parse_from_rfc3339("2022-02-28T06:16:18.735255Z")
            .unwrap()
            .with_timezone(&Utc);

        let user = serde_json::from_str::<User>(json).unwrap();

        assert_eq!(user.date_joined, expected_datetime);
    }
}
