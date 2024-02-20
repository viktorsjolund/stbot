mod message_parser;
use futures::stream::{SplitSink, SplitStream};
use futures::{SinkExt, StreamExt};
use lapin::Channel;
use lapin::{options::*, types::FieldTable, Connection, ConnectionProperties};
use message_parser::{parse_message, MessageResponse};
use serde::Deserialize;
use std::str;
use tokio::net::TcpStream;
use tokio::sync::mpsc::{self, Receiver, Sender};
use tokio::time::{sleep, Duration};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

#[tokio::main]
async fn main() {
    let amqp_addr = dotenv::var("AMQP_ADDR").unwrap_or_else(|_| "amqp://localhost:5672".into());
    let amqp_conn = Connection::connect(&amqp_addr, ConnectionProperties::default())
        .await
        .unwrap();
    let channel = amqp_conn.create_channel().await.unwrap();

    let mut connection_attempts = 0;
    let mut delay = 0;
    let max_connection_attempts = 12;

    while connection_attempts < max_connection_attempts {
        sleep(Duration::from_secs(delay)).await;
        if delay == 0 {
            delay += 1;
        } else {
            delay *= 2;
        }

        println!("[INFO] Connecting... (Attempt #{})", connection_attempts);
        let ws_connection_result = connect_async("ws://irc-ws.chat.twitch.tv:80").await;
        if let Err(e) = ws_connection_result {
            eprintln!("[ERROR] Connection failed: {:?}", e);
            connection_attempts += 1;
            continue;
        }

        connection_attempts = 0;
        delay = 0;

        println!("[INFO] Connected");
        let (tx, rx) = mpsc::channel::<Option<String>>(100);
        let (ws_stream, _) = ws_connection_result.unwrap();
        let (ws_tx, ws_rx) = ws_stream.split();
        let consumer_th = tokio::spawn(start_consumer(channel.clone(), tx.clone()));

        let (ws_rx, ws_tx) =
            tokio::join!(start_ws(tx.clone(), ws_rx), start_reader(rx, ws_tx));

        println!("[INFO] Aborting consumer thread...");
        consumer_th.abort();

        println!("[INFO] Closing websocket connection...");
        ws_tx.reunite(ws_rx).unwrap().close(None).await.unwrap();
    }
}

async fn start_reader(
    mut rx: Receiver<Option<String>>,
    mut ws_tx: SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message>,
) -> SplitSink<WebSocketStream<MaybeTlsStream<TcpStream>>, Message> {
    while let Some(m) = rx.recv().await {
        match m {
            Some(message) => {
                println!("[INFO] Response: {}", message);
                ws_tx.send(message.into()).await.unwrap()
            }
            None => return ws_tx,
        }
    }

    return ws_tx;
}

async fn start_consumer(channel: Channel, tx: Sender<Option<String>>) {
    let mut consumer = channel
        .basic_consume(
            "send",
            "bot",
            BasicConsumeOptions::default(),
            FieldTable::default(),
        )
        .await
        .unwrap();
    while let Some(delivery) = consumer.next().await {
        let delivery = delivery.expect("[ERROR] Error in consumer");
        let data = str::from_utf8(&delivery.data).unwrap();
        tx.send(Some(data.into())).await.unwrap();
        delivery.ack(BasicAckOptions::default()).await.unwrap();
    }
}

async fn start_ws(
    tx: Sender<Option<String>>,
    mut ws_rx: SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>>,
) -> SplitStream<WebSocketStream<MaybeTlsStream<TcpStream>>> {
    tx.send(Some("CAP REQ :twitch.tv/tags twitch.tv/commands".into()))
        .await
        .unwrap();
    tx.send(Some(
        format!("PASS {}", dotenv::var("TWITCH_OAUTH_TOKEN").unwrap()).into(),
    ))
    .await
    .unwrap();
    tx.send(Some(
        format!("NICK {}", dotenv::var("TWITCH_BOT_NICK").unwrap()).into(),
    ))
    .await
    .unwrap();
    let twitch_ch_name = dotenv::var("TWITCH_CHANNEL_NAME");
    if let Ok(ch) = twitch_ch_name {
        tx.send(Some(format!("JOIN #{}", ch).into())).await.unwrap();
    }
    let mut active_users = get_active_users().await.unwrap().users;
    for username in active_users.iter_mut() {
        *username = format!("#{}", username)
    }
    if active_users.len() > 0 {
        tx.send(Some(format!("JOIN {}", active_users.join(",")).into()))
            .await
            .unwrap();
    }

    while let Some(result) = ws_rx.next().await {
        match result {
            Ok(message) => {
                if message.is_text() {
                    let message_str = message.to_string();
                    let messages: Vec<&str> = message_str.trim_end().split("\r\n").collect();
                    for m in messages.iter() {
                        println!("[INFO] Message: {}", m);
                        let parsed_message = parse_message(m);
                        let response = generate_response(parsed_message).await;
                        if let Ok(r) = response {
                            match r.event {
                                ResponseEvent::Message => {
                                    let response_message = r.message.unwrap();
                                    let _ = tx.send(response_message.into()).await.unwrap_or_else(
                                        |e| eprintln!("[ERROR] Sender Error: {:?}", e),
                                    );
                                }
                                ResponseEvent::Reconnect => {
                                    tx.send(None).await.unwrap();
                                    return ws_rx;
                                }
                            }
                        }
                    }
                }
            }
            Err(e) => panic!("[ERROR] Websocket Error: {:?}", e),
        }
    }

    return ws_rx;
}

#[derive(Deserialize, Debug)]
struct ActiveUsersResponse {
    users: Vec<String>,
}

async fn get_active_users() -> Result<ActiveUsersResponse, Box<dyn std::error::Error>> {
    let client = reqwest::Client::new();
    let res = client
        .get(dotenv::var("WEB_URI").unwrap() + "/api/active")
        .send()
        .await?
        .json::<ActiveUsersResponse>()
        .await?;

    return Ok(res);
}

struct GeneratedResponse {
    event: ResponseEvent,
    message: Option<String>,
}

enum ResponseEvent {
    Reconnect,
    Message,
}

async fn generate_response(parsed_message: MessageResponse) -> Result<GeneratedResponse, ()> {
    let command = parsed_message
        .command
        .clone()
        .and_then(|c| c.command)
        .unwrap_or_default();
    let channel = parsed_message
        .command
        .clone()
        .and_then(|c| c.channel)
        .unwrap_or_default();
    let message = parsed_message.parameters.unwrap_or_default();
    let message_id = parsed_message
        .tags
        .and_then(|t| t.other)
        .and_then(|o| match o.get("id") {
            Some(s) => Some(s.to_owned()),
            None => None,
        })
        .unwrap_or_default();

    match command.as_str() {
        "PING" => {
            return Ok(GeneratedResponse {
                event: ResponseEvent::Message,
                message: Some(format!("PONG {}", message)),
            });
        }
        "PRIVMSG" => {
            let reply = reply_message(message.as_str(), &channel[1..]).await?;
            return Ok(GeneratedResponse {
                event: ResponseEvent::Message,
                message: Some(format!(
                    "@reply-parent-msg-id={} PRIVMSG {} :{}",
                    message_id, channel, reply
                )),
            });
        }
        "RECONNECT" => {
            return Ok(GeneratedResponse {
                event: ResponseEvent::Reconnect,
                message: None,
            });
        }
        _ => return Err(()),
    }
}

async fn reply_message(user_msg: &str, channel_name: &str) -> Result<String, ()> {
    let tokens: Vec<&str> = user_msg.split(" ").collect();
    match tokens.get(0) {
        Some(command) => match command.to_owned() {
            "?song" => {
                let song = get_spotify_song(channel_name).await;
                match song {
                    Ok(s) => {
                        if s.is_playing {
                            return Ok(format!("{} - {}", s.item.artists[0].name, s.item.name));
                        }
                        return Ok("No song currently playing".to_string());
                    }
                    Err(e) => {
                        eprintln!("[ERROR] Could not get song: {:?}", e);
                        return Err(());
                    }
                };
            }
            "?slink" => {
                let song = get_spotify_song(channel_name).await;
                match song {
                    Ok(s) => {
                        if s.is_playing {
                            return Ok(s.item.external_urls.spotify);
                        }
                        return Ok("No song currently playing".to_string());
                    }
                    Err(e) => {
                        eprintln!("[ERROR] Could not get song: {:?}", e);
                        return Err(());
                    }
                };
            }
            _ => return Err(()),
        },
        None => return Err(()),
    }
}

#[derive(Deserialize, Debug)]
struct SongResponse {
    is_playing: bool,
    item: Item,
}

#[derive(Deserialize, Debug)]
struct Item {
    name: String,
    external_urls: ExternalUrls,
    artists: Vec<Artist>,
}

#[derive(Deserialize, Debug)]
struct ExternalUrls {
    spotify: String,
}

#[derive(Deserialize, Debug)]
struct Artist {
    name: String,
}

async fn get_spotify_song(channel_name: &str) -> Result<SongResponse, Box<dyn std::error::Error>> {
    let client = reqwest::Client::new();
    let url = dotenv::var("WEB_URI").unwrap() + "/api/spotify/song";
    let params = [("channel_name", channel_name)];
    let url = reqwest::Url::parse_with_params(url.as_str(), &params)?;
    let res = client.get(url).send().await?.json::<SongResponse>().await?;

    return Ok(res);
}
