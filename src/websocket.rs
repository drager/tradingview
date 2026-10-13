use anyhow::anyhow;
use either::Either;
use futures::lock::Mutex;
use futures::stream::SplitSink;
use futures::stream::SplitStream;
use futures::SinkExt;
use futures::Stream;
use futures::StreamExt;
use rand::RngExt;
use regex::Regex;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet, VecDeque};
use std::str;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio::time::{sleep, timeout};
use tokio_tungstenite::tungstenite;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::handshake::client::Request;
use tokio_tungstenite::tungstenite::http::{HeaderValue, StatusCode};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

use crate::TickerSymbol;
use crate::Timeframe;
use crate::{parse_timescale_update, Ohlc, TradingViewError, User};

/// Handshake attempts (including the first) before giving up on HTTP 429.
const CONNECT_MAX_ATTEMPTS: u32 = 5;
/// First delay after a 429 handshake, doubled on each further attempt.
const CONNECT_INITIAL_BACKOFF: Duration = Duration::from_secs(1);
/// Upper bound for a server-provided `Retry-After`.
const CONNECT_MAX_RETRY_AFTER: Duration = Duration::from_secs(60);
/// Chart sessions kept open at once on one socket by `historical_data`.
const MAX_CONCURRENT_CHART_SESSIONS: usize = 10;
/// Fail outstanding series if the socket stays silent this long.
const HISTORICAL_DATA_IDLE_TIMEOUT: Duration = Duration::from_secs(30);

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct TradingViewPacket {
    #[serde(rename = "m")]
    pub packet_type: String,
    #[serde(rename = "p")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Vec<Value>>,
}

fn generate_session_id(type_: &str) -> String {
    let mut rng = rand::rng();
    let characters: Vec<char> = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789"
        .chars()
        .collect();

    let mut session_id = String::new();

    for _ in 0..12 {
        let random_index = rng.random_range(0..characters.len());
        session_id.push(characters[random_index]);
    }

    format!("{}_{}", type_, session_id)
}

fn parse_ws_packet(data: &str) -> anyhow::Result<Vec<Either<TradingViewPacket, i32>>> {
    log::debug!("Raw packet from websocket: {}", data);

    let replace_regex = Regex::new(r"~h~")?;
    let split_regex = Regex::new(r"~m~\d+~m~")?;

    // Remove heartbeat messages
    let cleaned = replace_regex.replace_all(data, "");

    // Split the cleaned data into separate packets
    let packets = split_regex.split(&cleaned);

    let data: Vec<Either<TradingViewPacket, i32>> = packets
        .filter(|p| !p.is_empty())
        .filter_map(|p| {
            let clean_packet = if !p.contains(r#""m":"#) && p.contains(r#""p":["#) {
                format!(r#"{{"m":"m",{}}}"#, &p[1..p.len().saturating_sub(1)])
            } else if p.starts_with("={") {
                p.replacen("={", "{", 1)
            } else {
                p.to_string()
            };

            match serde_json::from_str::<TradingViewPacket>(&clean_packet) {
                Ok(mut packet) => {
                    // Post-process `n` field to remove leading `=` if present
                    if let Some(Value::String(n_value)) = packet
                        .data
                        .as_mut()
                        .and_then(|p| p.get_mut(1))
                        .and_then(|v| v.get_mut("n"))
                    {
                        if n_value.starts_with('=') {
                            // Remove the leading `=`
                            *n_value = n_value[1..].to_string();
                        }
                    }
                    Some(Either::Left(packet))
                }
                Err(json_err) => match clean_packet.parse::<i32>() {
                    Ok(number) => Some(Either::Right(number)),
                    Err(err) => {
                        log::debug!(
                            "Failed to parse packet as json or number: {}, {}, {}",
                            clean_packet,
                            json_err,
                            err
                        );
                        None
                    }
                },
            }
        })
        .collect();

    Ok(data)
}

pub fn format_ws_packet(packet: &Either<TradingViewPacket, &str>) -> String {
    let packet_string = match packet {
        Either::Left(packet) => {
            if let Ok(p) = serde_json::to_string(packet) {
                p
            } else {
                return String::new();
            }
        }
        Either::Right(packet) => packet.to_string(),
    };

    format!("~m~{}~m~{}", packet_string.len(), packet_string)
}

pub struct WebSocketClient {
    read_stream: SplitStream<WsStream>,
    write_stream: SplitSink<WsStream, Message>,
    session_id: String,
    user: Arc<Mutex<Option<User>>>,
}

impl WebSocketClient {
    /// Create a new instance of the WebSocketClient struct
    pub async fn new(user: Arc<Mutex<Option<User>>>) -> anyhow::Result<Self> {
        crate::ensure_crypto_provider();
        let user = user.lock().await.clone();

        match user {
            None => Err(anyhow::anyhow!("User not logged in")),
            Some(user) => {
                let ws_stream = Self::connect(&user).await?;

                let (write_stream, read_stream) = ws_stream.split();

                let session_id = generate_session_id("xs");

                Ok(Self {
                    read_stream,
                    write_stream,
                    session_id,
                    user: Arc::new(Mutex::new(Some(user))),
                })
            }
        }
    }

    fn build_request(user: &User) -> anyhow::Result<Request> {
        let mut request = Url::parse(&format!(
            "wss://{}.tradingview.com/socket.io/websocket?type=chart",
            // TODO: More types? Premium?
            if user.is_pro { "prodata" } else { "data" }
        ))?
        .into_client_request()?;

        let headers = request.headers_mut();
        headers.insert(
            "Origin",
            HeaderValue::from_static("https://www.tradingview.com"),
        );
        headers.insert("Pragma", HeaderValue::from_static("no-cache"));
        headers.insert(
            "User-Agent",
            HeaderValue::from_static(crate::DEFAULT_USER_AGENT),
        );
        headers.insert(
            "Connection",
            HeaderValue::from_static("keep-alive, Upgrade"),
        );

        Ok(request)
    }

    /// Open the socket, backing off and retrying when TradingView rejects
    /// the handshake with 429 (it rate-limits new connections per IP).
    async fn connect(user: &User) -> anyhow::Result<WsStream> {
        let mut backoff = CONNECT_INITIAL_BACKOFF;
        let mut attempt = 1;
        loop {
            match connect_async(Self::build_request(user)?).await {
                Ok((ws_stream, _)) => return Ok(ws_stream),
                Err(tungstenite::Error::Http(response))
                    if response.status() == StatusCode::TOO_MANY_REQUESTS
                        && attempt < CONNECT_MAX_ATTEMPTS =>
                {
                    let delay = response
                        .headers()
                        .get("Retry-After")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.trim().parse::<u64>().ok())
                        .map(|secs| Duration::from_secs(secs).min(CONNECT_MAX_RETRY_AFTER))
                        .unwrap_or(backoff);
                    log::warn!(
                        "TradingView websocket handshake rate limited (429); retrying in {delay:?} \
                         (attempt {attempt}/{CONNECT_MAX_ATTEMPTS})"
                    );
                    sleep(delay).await;
                    backoff *= 2;
                    attempt += 1;
                }
                Err(e) => return Err(e.into()),
            }
        }
    }

    /// Subscribe to a ticker symbol
    ///
    /// This function allows subscribing to real-time price updates.
    pub async fn subscribe(
        mut self,
        ticker_symbols: &[TickerSymbol],
    ) -> anyhow::Result<Box<dyn Stream<Item = anyhow::Result<TradingViewPacket>> + Unpin + Send>>
    {
        self.send_auth_message().await?;

        self.send(&format_ws_packet(&Either::Left(TradingViewPacket {
            packet_type: "quote_create_session".to_owned(),
            data: Some(vec![Value::String(self.session_id.clone())]),
        })))
        .await?;

        for ticker_symbol in ticker_symbols {
            let symbol_key = format!(
                "={}",
                serde_json::json!({ "symbol": ticker_symbol.symbol, "currency-id": ticker_symbol.currency })
            );

            self.send(&format_ws_packet(&Either::Left(TradingViewPacket {
                packet_type: "quote_add_symbols".to_owned(),
                data: Some(vec![
                    Value::String(self.session_id.clone()),
                    Value::String(symbol_key),
                ]),
            })))
            .await?;
        }

        self.listen_to_incoming().await
    }

    /// Fetch the last `range` bars at `timeframe` for every symbol over this
    /// one socket, one chart session per symbol, then close the socket.
    ///
    /// The outer error covers failures before any series is requested
    /// (e.g. auth). Afterwards each symbol gets its own result, so a bad
    /// ticker or a dropped connection doesn't discard what already arrived.
    pub async fn historical_data(
        mut self,
        ticker_symbols: &[TickerSymbol],
        timeframe: Timeframe,
        range: u32,
    ) -> anyhow::Result<HashMap<TickerSymbol, anyhow::Result<Vec<Ohlc>>>> {
        // A 0-bar series is a `critical_error` that kills every session on
        // the socket, so refuse it before sending anything.
        if range == 0 {
            return Err(TradingViewError::InvalidRange.into());
        }

        self.send_auth_message().await?;

        let timeframe = timeframe.to_string();
        let mut seen = HashSet::new();
        let mut queue: VecDeque<TickerSymbol> = ticker_symbols
            .iter()
            .filter(|symbol| seen.insert((*symbol).clone()))
            .cloned()
            .collect();
        // Chart session id -> symbol and the bars received so far.
        let mut in_flight: HashMap<String, (TickerSymbol, Vec<Ohlc>)> = HashMap::new();
        let mut results = HashMap::new();

        let failure = loop {
            let mut send_failure = None;
            while in_flight.len() < MAX_CONCURRENT_CHART_SESSIONS {
                let Some(symbol) = queue.pop_front() else {
                    break;
                };
                let chart_session = generate_session_id("cs");
                if let Err(e) = self
                    .start_series(&chart_session, &symbol, &timeframe, range)
                    .await
                {
                    queue.push_front(symbol);
                    send_failure = Some(format!("Failed to request series: {e}"));
                    break;
                }
                in_flight.insert(chart_session, (symbol, Vec::new()));
            }
            if send_failure.is_some() {
                break send_failure;
            }
            if in_flight.is_empty() {
                break None;
            }

            let text = match timeout(HISTORICAL_DATA_IDLE_TIMEOUT, self.read_stream.next()).await {
                Ok(Some(Ok(Message::Text(text)))) => text,
                Ok(Some(Ok(Message::Close(reason)))) => {
                    break Some(format!("WebSocket closed by server: {reason:?}"));
                }
                Ok(Some(Ok(_))) => continue,
                Ok(Some(Err(e))) => break Some(format!("Error reading WebSocket message: {e}")),
                Ok(None) => break Some("WebSocket stream ended".to_owned()),
                Err(_) => {
                    break Some(format!(
                        "Timed out after {HISTORICAL_DATA_IDLE_TIMEOUT:?} waiting for historical data"
                    ));
                }
            };

            let packets = match parse_ws_packet(&text) {
                Ok(packets) => packets,
                Err(e) => break Some(format!("Failed to parse WebSocket message: {e}")),
            };

            let mut fatal = None;
            for packet in packets {
                let packet = match packet {
                    Either::Left(packet) => packet,
                    Either::Right(ping_number) => {
                        if let Err(e) = Self::ping(ping_number, &mut self.write_stream).await {
                            fatal = Some(format!("Failed to answer ping: {e}"));
                            break;
                        }
                        continue;
                    }
                };

                if packet.packet_type == "protocol_error" || packet.packet_type == "critical_error"
                {
                    fatal = Some(format!("Critical error: {:?}", packet.data));
                    break;
                }

                let Some(chart_session) = packet
                    .data
                    .as_ref()
                    .and_then(|data| data.first())
                    .and_then(|session| session.as_str())
                    .map(str::to_owned)
                else {
                    continue;
                };

                match packet.packet_type.as_str() {
                    "timescale_update" => {
                        if let Some((_, bars)) = in_flight.get_mut(&chart_session) {
                            bars.extend(parse_timescale_update(&packet));
                        }
                    }
                    "series_completed" => {
                        if let Some((symbol, mut bars)) = in_flight.remove(&chart_session) {
                            bars.sort_by_key(|bar| bar.get_timestamp());
                            bars.dedup_by_key(|bar| bar.get_timestamp());
                            results.insert(symbol, Ok(bars));
                            let _ = self.delete_chart_session(&chart_session).await;
                        }
                    }
                    "symbol_error" | "series_error" => {
                        if let Some((symbol, _)) = in_flight.remove(&chart_session) {
                            log::warn!("Historical data for {symbol:?} failed: {packet:?}");
                            results.insert(
                                symbol,
                                Err(anyhow!("{}: {:?}", packet.packet_type, packet.data)),
                            );
                            let _ = self.delete_chart_session(&chart_session).await;
                        }
                    }
                    _ => {}
                }
            }
            if fatal.is_some() {
                break fatal;
            }
        };

        if let Some(reason) = failure {
            log::error!("Historical data request aborted: {reason}");
            for (symbol, _) in in_flight
                .into_values()
                .chain(queue.into_iter().map(|s| (s, vec![])))
            {
                results.insert(symbol, Err(anyhow!("{reason}")));
            }
        }

        let _ = self.write_stream.close().await;

        Ok(results)
    }

    async fn start_series(
        &mut self,
        chart_session: &str,
        symbol: &TickerSymbol,
        timeframe: &str,
        range: u32,
    ) -> anyhow::Result<()> {
        self.send(&format_ws_packet(&Either::Left(TradingViewPacket {
            packet_type: "chart_create_session".to_owned(),
            data: Some(vec![Value::String(chart_session.to_owned())]),
        })))
        .await?;

        let symbol_key = format!(
            "={}",
            serde_json::json!({ "symbol": symbol.symbol, "adjustment": "splits", "currency-id": symbol.currency, "session": "extended" })
        );

        self.send(&format_ws_packet(&Either::Left(TradingViewPacket {
            packet_type: "resolve_symbol".to_owned(),
            data: Some(vec![
                Value::String(chart_session.to_owned()),
                Value::String("sds_sym_1".to_owned()),
                Value::String(symbol_key),
            ]),
        })))
        .await?;

        self.send(&format_ws_packet(&Either::Left(TradingViewPacket {
            packet_type: "create_series".to_owned(),
            data: Some(vec![
                Value::String(chart_session.to_owned()),
                Value::String("$prices".to_owned()),
                Value::String("s1".to_owned()),
                Value::String("sds_sym_1".to_owned()),
                Value::String(timeframe.to_owned()),
                Value::Number(range.into()),
            ]),
        })))
        .await
    }

    async fn delete_chart_session(&mut self, chart_session: &str) -> anyhow::Result<()> {
        self.send(&format_ws_packet(&Either::Left(TradingViewPacket {
            packet_type: "chart_delete_session".to_owned(),
            data: Some(vec![Value::String(chart_session.to_owned())]),
        })))
        .await
    }

    pub async fn fetch_instruments(
        mut self,
        ticker_symbols: &[TickerSymbol],
    ) -> anyhow::Result<Box<dyn Stream<Item = anyhow::Result<TradingViewPacket>> + Unpin + Send>>
    {
        self.send_auth_message().await?;

        self.send(&format_ws_packet(&Either::Left(TradingViewPacket {
            packet_type: "quote_create_session".to_owned(),
            data: Some(vec![Value::String(self.session_id.clone())]),
        })))
        .await?;

        let mut data = vec![Value::String(self.session_id.clone())];

        for ticker in ticker_symbols {
            data.push(Value::String(ticker.symbol.clone()));
        }

        let tv_packet = TradingViewPacket {
            packet_type: "quote_add_symbols".to_owned(),
            data: Some(data),
        };

        self.send(&format_ws_packet(&Either::Left(tv_packet)))
            .await?;

        self.listen_to_incoming().await
    }

    pub async fn listen_to_incoming(
        self,
    ) -> anyhow::Result<Box<dyn Stream<Item = anyhow::Result<TradingViewPacket>> + Unpin + Send>>
    {
        let Self {
            mut read_stream,
            write_stream,
            ..
        } = self;

        let write_stream = Arc::new(Mutex::new(write_stream));

        let data_stream = async_stream::stream! {
            loop {
                match read_stream.next().await {
                    Some(Ok(Message::Text(text))) => {
                        let mut write_stream = write_stream.lock().await;

                        match Self::handle_ws_package(&text, &mut write_stream).await {
                            Ok(packets) => {
                                for packet in packets {
                                    log::debug!("Received packet: {:?}", packet);
                                    yield Ok(packet);
                                }
                            },
                            Err(e) => {
                                log::error!("Error handling WebSocket package: {}", e);
                                yield Err(e);
                                break;
                            }
                        }
                    }
                    Some(Ok(Message::Close(reason))) => {
                        log::error!("WebSocket connection closed by server: {:?}", reason);
                        yield Err(anyhow!("WebSocket connection closed by server"));
                        break;
                    }
                    Some(Ok(_)) => {
                        log::warn!("Received unhandled WebSocket message type");
                    }
                    Some(Err(e)) => {
                        log::error!("Error reading WebSocket message: {}", e);
                        yield Err(anyhow!("Error reading WebSocket message"));
                        break;
                    }
                    None => {
                        log::warn!("WebSocket stream ended; caller must reconnect");
                        yield Err(anyhow!("WebSocket stream ended"));
                        break;
                    }
                }
            }
        };

        Ok(Box::new(data_stream.boxed()))
    }

    async fn send_auth_message(&mut self) -> anyhow::Result<()> {
        let packet = TradingViewPacket {
            packet_type: "set_auth_token".to_owned(),
            data: Some(vec![Value::String(
                self.user
                    .lock()
                    .await
                    .clone()
                    .map(|user| user.auth_token)
                    .unwrap_or_default(),
            )]),
        };
        let msg = format_ws_packet(&Either::Left(packet));

        self.send(&msg).await
    }

    async fn send(&mut self, msg: &str) -> anyhow::Result<()> {
        let msg = Message::Text(msg.into());

        log::debug!("Sending message to websocket: {}", msg);

        self.write_stream.send(msg).await.map_err(|e| e.into())
    }

    async fn ping(
        ping_number: i32,
        write_stream: &mut SplitSink<WsStream, Message>,
    ) -> anyhow::Result<()> {
        let ping_message = format!("~h~{}", ping_number);
        let packet_string = format_ws_packet(&Either::Right(&ping_message));

        log::debug!("Sending ping: {:?}", packet_string);

        let msg = Message::Text(packet_string.into());

        write_stream.send(msg).await.map_err(|e| e.into())
    }

    async fn handle_ws_package(
        data: &str,
        write_stream: &mut SplitSink<WsStream, Message>,
    ) -> anyhow::Result<Vec<TradingViewPacket>> {
        let packets = parse_ws_packet(data)?;

        let mut processed_packets = Vec::new();

        for packet in packets {
            match packet {
                Either::Left(mut tv_packet) => {
                    if tv_packet.packet_type == "protocol_error"
                        || tv_packet.packet_type == "critical_error"
                    {
                        let p = tv_packet.data.take().unwrap_or_default();

                        log::error!("Critical error: {:?}. Closing websocket connection...", p);

                        let _ = write_stream.close().await;

                        return Err(anyhow::anyhow!(format!(
                            "Critical error: {:?}. Closing websocket connection...",
                            p
                        )));
                    }

                    if let Some(data) = &tv_packet.data {
                        if let Some(Value::Object(obj)) = data.get(1) {
                            if let Some(Value::String(s)) = obj.get("s") {
                                if s == "error" {
                                    log::error!("Error in packet: {:?}", tv_packet);

                                    return Err(anyhow::anyhow!(
                                        "Error in packet: {:?}",
                                        tv_packet
                                    ));
                                }
                            }
                        }
                    } else {
                        log::warn!("Packet missing expected data field: {:?}", tv_packet);
                    }

                    processed_packets.push(tv_packet);
                }
                Either::Right(ping_number) => {
                    Self::ping(ping_number, write_stream).await?;
                }
            }
        }

        Ok(processed_packets)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_ws_packet() {
        let data = r#"~m~208~m~{"m":"qsd","p":["xs_MNEYASv50aGa",{"n":"={\"currency-id\":\"USD\",\"symbol\":\"BITSTAMP:BTCUSD\"}","s":"ok","v":{"trade_loaded":true,"bid_size":0.39190755,"bid":27161.0,"ask_size":0.19131476,"ask":27162.0}}]}"#;

        let parsed = parse_ws_packet(data).unwrap();

        // Construct the expected TradingViewPacket
        let expected_p = vec![
        Value::String("xs_MNEYASv50aGa".to_string()),
        Value::Object(
            serde_json::from_str(
                r#"{"n":"{\"currency-id\":\"USD\",\"symbol\":\"BITSTAMP:BTCUSD\"}","s":"ok","v":{"trade_loaded":true,"bid_size":0.39190755,"bid":27161.0,"ask_size":0.19131476,"ask":27162.0}}"#
            ).unwrap()
        ),
    ];
        let expected_packet = TradingViewPacket {
            packet_type: "qsd".to_string(),
            data: Some(expected_p),
        };

        // Compare the first packet in parsed with the expected packet
        if let Some(Either::Left(first_packet)) = parsed.first() {
            assert_eq!(first_packet, &expected_packet);
        } else {
            panic!(
                "Parsed data did not contain a TradingViewPacket, but was: {:?}",
                parsed
            );
        }
    }

    #[test]
    fn test_parse_ws_packet_ping_number() {
        let data = r#"~h~1"#;

        let parsed = parse_ws_packet(data).unwrap();

        // Compare the first packet in parsed with the expected packet
        if let Some(Either::Right(ping_number)) = parsed.first() {
            assert_eq!(ping_number, &1);
        } else {
            panic!(
                "Parsed data did not contain a ping number, but was: {:?}",
                parsed
            );
        }
    }

    #[test]
    fn test_parse_ws_packet_empty() {
        let data = "";
        let parsed = parse_ws_packet(data).unwrap();
        assert!(
            parsed.is_empty(),
            "Parsed data should be empty for empty input"
        );
    }

    #[test]
    fn test_parse_ws_packet_invalid_json() {
        let data = r#"~m~208~m~{"m":,,"p":["xs_MNEYASv50aGa",{"n":"={\"currency-id\":\"USD\",\"symbol\":\"BITSTAMP:BTCUSD\"}","s":"ok","v":{"trade_loaded":true,"bid_size":0.39190755,"bid":27161.0,"ask_size":0.19131476,"ask":27162.0}}]}"#;
        let parsed = parse_ws_packet(data);
        assert!(parsed.is_ok(), "Parsing should not fail for invalid JSON");
        assert!(
            parsed.unwrap().is_empty(),
            "Parsed data should be empty for invalid JSON"
        );
    }

    #[test]
    fn test_parse_ws_packet_missing_p_field() {
        let data = r#"~m~208~m~{"m":"qsd"}"#; // missing "p" field
        let parsed = parse_ws_packet(data);
        assert!(parsed.is_ok(), "Parsing should not fail for missing fields");

        // Compare the first packet in parsed with the expected packet
        if let Some(Either::Left(first_packet)) = parsed.unwrap().first() {
            assert_eq!(first_packet.packet_type, "qsd".to_string());
            assert_eq!(first_packet.data, None);
        } else {
            panic!("Parsed data did not contain a TradingViewPacket",);
        }
    }

    #[test]
    fn test_parse_ws_packet_missing_m_field() {
        let data = r#"~m~208~m~{"p":["xs_MNEYASv50aGa",{"n":"={\"currency-id\":\"USD\",\"symbol\":\"BITSTAMP:BTCUSD\"}","s":"ok","v":{"trade_loaded":true,"bid_size":0.39190755,"bid":27161.0,"ask_size":0.19131476,"ask":27162.0}}]}"#; // missing "m" field
        let parsed = parse_ws_packet(data);
        assert!(parsed.is_ok(), "Parsing should not fail for missing fields");

        // Compare the first packet in parsed with the expected packet
        if let Some(Either::Left(first_packet)) = parsed.unwrap().first() {
            assert_eq!(first_packet.packet_type, "m".to_string());
            assert_eq!(
                first_packet.data,
                Some(vec![
                    Value::String("xs_MNEYASv50aGa".to_string()),
                    Value::Object(
                        serde_json::from_str(
                            r#"{"n":"{\"currency-id\":\"USD\",\"symbol\":\"BITSTAMP:BTCUSD\"}","s":"ok","v":{"trade_loaded":true,"bid_size":0.39190755,"bid":27161.0,"ask_size":0.19131476,"ask":27162.0}}"#
                        )
                        .unwrap()
                    ),
                ])
            );
        } else {
            panic!("Parsed data did not contain a TradingViewPacket",);
        }
    }

    #[test]
    fn test_parse_ws_packet_multiple() {
        let data = r#"~m~114~m~{"m":"qsd","p":["xs_kujC8txR2L01",{"n":"OMXSTO:OMXS30","s":"ok","v":{"minute_loaded":true,"lp_time":1730462340}}]}~m~92~m~{"m":"qsd","p":["xs_kujC8txR2L01",{"n":"OMXSTO:OMXS30","s":"ok","v":{"trade_loaded":true}}]}~m~3295~m~{"m":"qsd","p":["xs_kujC8txR2L01",{"n":"OMXSTO:OMXS30","s":"ok","v":{"first_bar_time_1d":528422400,"regular_close":2542.4079}}]}~m~97~m~{"m":"qsd","p":["xs_kujC8txR2L01",{"n":"OMXSTO:OMXS30","s":"ok","v":{"fundamental_data":false}}]}~m~63~m~{"m":"quote_completed","p":["xs_kujC8txR2L01","OMXSTO:OMXS30"]}"#;

        let parsed_packets = parse_ws_packet(data).unwrap();

        // Check that we have 5 packets
        assert_eq!(parsed_packets.len(), 5, "Expected 5 packets");

        // Check the packet type for each parsed packet and confirm key data
        if let Either::Left(packet1) = &parsed_packets[0] {
            assert_eq!(packet1.packet_type, "qsd");
            assert!(packet1.data.is_some());
            let p_data = packet1.data.as_ref().unwrap();
            assert_eq!(p_data[0], Value::String("xs_kujC8txR2L01".to_string()));
        } else {
            panic!("Packet 1 did not parse as expected");
        }

        if let Either::Left(packet2) = &parsed_packets[1] {
            assert_eq!(packet2.packet_type, "qsd");
            assert!(packet2.data.is_some());
        } else {
            panic!("Packet 2 did not parse as expected");
        }

        if let Either::Left(packet3) = &parsed_packets[2] {
            assert_eq!(packet3.packet_type, "qsd");
            assert!(packet3.data.is_some());
        } else {
            panic!("Packet 3 did not parse as expected");
        }

        if let Either::Left(packet4) = &parsed_packets[3] {
            assert_eq!(packet4.packet_type, "qsd");
            assert!(packet4.data.is_some());
        } else {
            panic!("Packet 4 did not parse as expected");
        }

        if let Either::Left(packet5) = &parsed_packets[4] {
            assert_eq!(packet5.packet_type, "quote_completed");
            assert!(packet5.data.is_some());
            let p_data = packet5.data.as_ref().unwrap();
            assert_eq!(p_data[0], Value::String("xs_kujC8txR2L01".to_string()));
            assert_eq!(p_data[1], Value::String("OMXSTO:OMXS30".to_string()));
        } else {
            panic!("Packet 5 did not parse as expected");
        }
    }

    mod format_ws_packet {
        use super::*;

        #[test]
        fn test_format_ws_packet() {
            let packet = TradingViewPacket {
                packet_type: "set_auth_token".to_owned(),
                data: Some(vec![Value::String("xs".to_owned())]),
            };

            let formatted = format_ws_packet(&Either::Left(packet));

            assert_eq!(
                formatted,
                "~m~33~m~{\"m\":\"set_auth_token\",\"p\":[\"xs\"]}"
            );
        }

        #[test]
        fn test_format_ws_packet_ping_number() {
            let ping_number = "1";

            let ping_message = format!("~h~{}", ping_number);
            let formatted = format_ws_packet(&Either::Right(&ping_message));

            assert_eq!(formatted, "~m~4~m~~h~1");
        }
    }
}
