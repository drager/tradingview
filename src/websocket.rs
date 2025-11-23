use anyhow::anyhow;
use either::Either;
use futures::lock::Mutex;
use futures::stream::SplitSink;
use futures::stream::SplitStream;
use futures::SinkExt;
use futures::Stream;
use futures::StreamExt;
use rand::Rng;
use regex::Regex;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::str;
use std::sync::Arc;
use tokio::net::TcpStream;
use tokio::time::{sleep, Duration};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

use crate::TickerSymbol;
use crate::Timeframe;
use crate::{Currency, User};

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
    read_stream: SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>,
    write_stream: SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>,
    session_id: String,
    user: Arc<Mutex<Option<User>>>,
}

impl WebSocketClient {
    /// Create a new instance of the WebSocketClient struct
    pub async fn new(user: Arc<Mutex<Option<User>>>) -> anyhow::Result<Self> {
        let user = user.lock().await.clone();

        match user {
            None => Err(anyhow::anyhow!("User not logged in")),
            Some(user) => {
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
                    HeaderValue::from_static(
                        "Mozilla/5.0 (X11; Linux x86_64; rv:144.0) Gecko/20100101 Firefox/144.0",
                    ),
                );
                headers.insert(
                    "Connection",
                    HeaderValue::from_static("keep-alive, Upgrade"),
                );

                let (ws_stream, _) = connect_async(request).await?;

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

    pub async fn historical_data(
        mut self,
        ticker_symbol: &str,
        timeframe: Timeframe,
        range: u32,
        currency: &Currency,
    ) -> anyhow::Result<Box<dyn Stream<Item = anyhow::Result<TradingViewPacket>> + Unpin + Send>>
    {
        self.send_auth_message().await?;

        self.send(&format_ws_packet(&Either::Left(TradingViewPacket {
            packet_type: "chart_create_session".to_owned(),
            data: Some(vec![Value::String(self.session_id.clone())]),
        })))
        .await?;

        let symbol_key = format!(
            "={}",
            serde_json::json!({ "symbol": ticker_symbol, "adjustment": "splits", "currency-id": currency, "session": "extended" })
        );

        let resolve_package = &format_ws_packet(&Either::Left(TradingViewPacket {
            packet_type: "resolve_symbol".to_owned(),
            data: Some(vec![
                Value::String(self.session_id.clone()),
                Value::String(format!("sds_sym_{}", 1)),
                Value::String(symbol_key),
            ]),
        }));

        self.send(resolve_package).await?;

        let create_series_package = &format_ws_packet(&Either::Left(TradingViewPacket {
            packet_type: "create_series".to_owned(),
            data: Some(vec![
                Value::String(self.session_id.clone()),
                Value::String("$prices".to_string()),
                Value::String("s1".to_string()),
                Value::String(format!("sds_sym_{}", 1)),
                Value::String(timeframe.to_string()),
                Value::Number(range.into()),
            ]),
        }));

        self.send(create_series_package).await?;

        self.listen_to_incoming().await
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
        let mut retry_attempts = 0;
        let max_retries = 5;

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
                                retry_attempts = 0;
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
                        if retry_attempts < max_retries {
                            let backoff = 2u64.pow(retry_attempts);
                            log::warn!("WebSocket stream ended unexpectedly, attempting to reconnect in {} seconds...", backoff);
                            sleep(Duration::from_secs(backoff)).await;
                            retry_attempts += 1;
                            continue;
                        } else {
                            log::error!("Max retry attempts reached. Giving up on reconnecting.");
                            yield Err(anyhow!("Max retry attempts reached. Giving up on reconnecting."));
                            break;
                        }
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
        let msg = Message::Text(msg.to_owned());

        log::debug!("Sending message to websocket: {}", msg);

        self.write_stream.send(msg).await.map_err(|e| e.into())
    }

    async fn ping(
        ping_number: i32,
        write_stream: &mut SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>,
    ) -> anyhow::Result<()> {
        let ping_message = format!("~h~{}", ping_number);
        let packet_string = format_ws_packet(&Either::Right(&ping_message));

        log::debug!("Sending ping: {:?}", packet_string);

        let msg = Message::Text(packet_string.to_owned());

        write_stream.send(msg).await.map_err(|e| e.into())
    }

    async fn handle_ws_package(
        data: &str,
        write_stream: &mut SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>,
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
