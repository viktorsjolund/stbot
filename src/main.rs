mod message_parser;
use futures::stream::{SplitSink, SplitStream};
use futures::{SinkExt, StreamExt};
use lapin::Channel;
use lapin::{options::*, types::FieldTable, Connection, ConnectionProperties};
use message_parser::{parse_message, MessageResponse};
use reqwest::header::CONTENT_TYPE;
use reqwest::StatusCode;
use serde::Deserialize;
use std::collections::HashMap;
use std::str;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;
use tokio::net::TcpStream;
use tokio::sync::mpsc::{self, Receiver, Sender};
use tokio::task::JoinHandle;
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

        let (ws_rx, ws_tx) = tokio::join!(start_ws(tx.clone(), ws_rx), start_reader(rx, ws_tx));

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
    let access_token = get_twitch_access_token().await.unwrap();
    tx.send(Some(format!("PASS oauth:{}", access_token).into()))
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
    let mut skip_channels = HashMap::<String, Arc<Mutex<Vec<String>>>>::new();
    let mut handles: Vec<JoinHandle<()>> = Vec::new();
    let mut last_skip = SystemTime::now();

    while let Some(result) = ws_rx.next().await {
        match result {
            Ok(message) => {
                if message.is_text() {
                    let message_str = message.to_string();
                    let messages: Vec<&str> = message_str.trim_end().split("\r\n").collect();
                    for m in messages.iter() {
                        let parsed_message = parse_message(m);
                        let response = generate_response(parsed_message).await;
                        match response {
                            Ok(gr) => {
                                if let Some(r) = gr {
                                    match r.event {
                                        ResponseEvent::Message => {
                                            println!("[INFO] Message: {}", m);
                                            let response_message = r.message.unwrap();
                                            let _ = tx
                                                .send(response_message.into())
                                                .await
                                                .unwrap_or_else(|e| {
                                                    eprintln!("[ERROR] Sender Error: {:?}", e)
                                                });
                                        }
                                        ResponseEvent::Reconnect => {
                                            tx.send(None).await.unwrap();
                                            return ws_rx;
                                        }
                                        ResponseEvent::Skip => {
                                            let min_secs_between_skips = Duration::from_secs(10);
                                            if last_skip.elapsed().unwrap()
                                                >= min_secs_between_skips
                                            {
                                                handle_skip(
                                                    &mut skip_channels,
                                                    r,
                                                    &mut handles,
                                                    &mut last_skip,
                                                    &tx,
                                                )
                                                .await;
                                            }
                                        }
                                    };
                                }
                            }
                            Err(e) => eprintln!("[ERROR] {}", e),
                        }
                    }
                }
            }
            Err(e) => {
                eprintln!("[ERROR] Websocket Error: {:?}", e);
                return ws_rx;
            }
        }
    }

    return ws_rx;
}

#[derive(Deserialize, Debug)]
#[allow(dead_code)]
struct TwitchTokenResponse {
    access_token: String,
}

async fn get_twitch_access_token() -> Result<String, Box<dyn std::error::Error>> {
    let params = [
        ("grant_type", "refresh_token"),
        ("client_id", &dotenv::var("TWITCH_CLIENT_ID").unwrap()),
        (
            "client_secret",
            &dotenv::var("TWITCH_CLIENT_SECRET").unwrap(),
        ),
        (
            "refresh_token",
            &dotenv::var("TWITCH_REFRESH_TOKEN").unwrap(),
        ),
    ];
    let client = reqwest::Client::new();
    let res = client
        .post("https://id.twitch.tv/oauth2/token")
        .header(CONTENT_TYPE, "application/x-www-form-urlencoded")
        .form(&params)
        .send()
        .await?
        .json::<TwitchTokenResponse>()
        .await?;

    return Ok(res.access_token);
}

async fn handle_skip(
    skip_channels: &mut HashMap<String, Arc<Mutex<Vec<String>>>>,
    gr: GeneratedResponse,
    handles: &mut Vec<JoinHandle<()>>,
    last_skip: &mut SystemTime,
    tx: &Sender<Option<String>>,
) {
    let channel_name = gr.channel_name.unwrap();
    if let None = skip_channels.get(&channel_name) {
        skip_channels.insert(channel_name.clone(), Arc::new(Mutex::new(Vec::new())));
    }
    let current_skip_users = skip_channels.get_mut(&channel_name).unwrap();
    let username = gr.username.unwrap();
    if !current_skip_users.lock().unwrap().contains(&username) {
        current_skip_users.lock().unwrap().push(username.clone());
        // TODO: change required votes to 5 or sum
        let required_skips = 5;
        if current_skip_users.lock().unwrap().len() >= required_skips {
            let res = skip_current_song(&channel_name.clone()).await;
            match res {
                Ok(s) => {
                    if s.is_success() {
                        let _ = tx
                            .send(
                                format!("PRIVMSG {} :Vote skip passed", channel_name.clone())
                                    .into(),
                            )
                            .await
                            .unwrap_or_else(|e| eprintln!("[ERROR] Sender Error: {:?}", e));
                        *last_skip = SystemTime::now();
                        for h in handles {
                            h.abort();
                        }
                        current_skip_users.lock().unwrap().clear();
                    } else {
                        println!("[INFO] Could not skip song, status code: {}", s);
                    }
                }
                Err(e) => {
                    eprintln!("[ERROR] Error skipping song: {:?}", e);
                }
            }
        } else {
            let current_skip_users = Arc::clone(&current_skip_users);
            let username = username.clone();
            let handle = tokio::spawn(async move {
                sleep(Duration::from_secs(30)).await;
                let i = current_skip_users
                    .lock()
                    .unwrap()
                    .iter()
                    .position(|n| *n == username)
                    .unwrap();
                current_skip_users.lock().unwrap().remove(i);
            });
            handles.push(handle);
        }
    }
}

async fn skip_current_song(channel_name: &str) -> Result<StatusCode, Box<dyn std::error::Error>> {
    let client = reqwest::Client::new();
    let url = dotenv::var("WEB_URI").unwrap() + "/api/spotify/skip";
    let params = [("channel_name", channel_name)];
    let url = reqwest::Url::parse_with_params(url.as_str(), &params)?;
    let status = client.post(url).send().await?.status();

    return Ok(status);
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

#[derive(Default)]
struct GeneratedResponse {
    event: ResponseEvent,
    message: Option<String>,
    username: Option<String>,
    channel_name: Option<String>,
}

enum ResponseEvent {
    Reconnect,
    Message,
    Skip,
}

impl Default for ResponseEvent {
    fn default() -> Self {
        Self::Message
    }
}

async fn generate_response(
    parsed_message: MessageResponse,
) -> Result<Option<GeneratedResponse>, String> {
    let command = parsed_message
        .command
        .clone()
        .and_then(|c| c.command)
        .unwrap_or_default();
    // Has a # in front of the channel name
    let channel = parsed_message
        .command
        .clone()
        .and_then(|c| c.channel)
        .unwrap_or_default();
    let message = parsed_message.parameters.unwrap_or_default();
    let tags = parsed_message.tags.and_then(|t| t.other);
    let message_id = tags
        .clone()
        .and_then(|o| match o.get("id") {
            Some(s) => Some(s.to_owned()),
            None => None,
        })
        .unwrap_or_default();
    let display_name = tags
        .clone()
        .and_then(|o| match o.get("display-name") {
            Some(s) => Some(s.to_owned()),
            None => None,
        })
        .unwrap_or_default();
    let is_channel_owner = format!("#{}", display_name.to_lowercase()) == channel.to_lowercase();

    match command.as_str() {
        "PING" => {
            return Ok(Some(GeneratedResponse {
                event: ResponseEvent::Message,
                message: Some(format!("PONG {}", message)),
                ..Default::default()
            }));
        }
        "PRIVMSG" => {
            let reply = reply_message(message.as_str(), &channel[1..], &is_channel_owner).await?;
            if let Some(r) = reply {
                return match r.reply_type {
                    ReplyType::Message => Ok(Some(GeneratedResponse {
                        event: ResponseEvent::Message,
                        message: Some(format!(
                            "@reply-parent-msg-id={} PRIVMSG {} :{}",
                            message_id,
                            channel,
                            r.message.unwrap()
                        )),
                        ..Default::default()
                    })),
                    ReplyType::Skip => Ok(Some(GeneratedResponse {
                        event: ResponseEvent::Skip,
                        username: Some(display_name),
                        message: None,
                        channel_name: Some(channel),
                    })),
                };
            } else {
                return Ok(None);
            }
        }
        "RECONNECT" => {
            return Ok(Some(GeneratedResponse {
                event: ResponseEvent::Reconnect,
                ..Default::default()
            }));
        }
        _ => Ok(None),
    }
}

#[derive(Default)]
struct Reply {
    message: Option<String>,
    reply_type: ReplyType,
}

enum ReplyType {
    Message,
    Skip,
}

impl Default for ReplyType {
    fn default() -> Self {
        ReplyType::Message
    }
}

async fn reply_message(
    user_msg: &str,
    channel_name: &str,
    is_channel_owner: &bool,
) -> Result<Option<Reply>, String> {
    let tokens: Vec<&str> = user_msg.split(" ").collect();
    match tokens.get(0) {
        Some(command) => match command.to_owned() {
            "?song" => {
                let song = get_spotify_song(channel_name).await;
                match song {
                    Ok(s) => {
                        if s.is_playing {
                            return Ok(Some(Reply {
                                message: Some(format!(
                                    "{} - {}",
                                    s.item.artists[0].name, s.item.name
                                )),
                                ..Default::default()
                            }));
                        }
                        return Ok(Some(Reply {
                            message: Some("No song currently playing".to_string()),
                            ..Default::default()
                        }));
                    }
                    Err(e) => {
                        return Err(format!("Could not get song: {:?}", e));
                    }
                };
            }
            "?slink" | "?songlink" => {
                let song = get_spotify_song(channel_name).await;
                match song {
                    Ok(s) => {
                        if s.is_playing {
                            return Ok(Some(Reply {
                                message: Some(s.item.external_urls.spotify),
                                ..Default::default()
                            }));
                        }
                        return Ok(Some(Reply {
                            message: Some("No song currently playing".to_string()),
                            ..Default::default()
                        }));
                    }
                    Err(e) => {
                        return Err(format!("Could not get song: {:?}", e));
                    }
                };
            }
            "?skip" => {
                return Ok(Some(Reply {
                    reply_type: ReplyType::Skip,
                    ..Default::default()
                }));
            }
            "?skipon" => {
                if !is_channel_owner {
                    return Err("User is not the channel owner.".into());
                };
                let res = enable_song_skip(&channel_name).await;
                match res {
                    Ok(s) => {
                        if s.is_success() {
                            return Ok(Some(Reply {
                                reply_type: ReplyType::Message,
                                message: Some("Vote skip is now enabled".into()),
                                ..Default::default()
                            }));
                        }

                        return Err(format!(
                            "Enabling song skip failed with status code {}",
                            s.to_string()
                        ));
                    }
                    Err(e) => Err(format!("Enabling song skip failed: {:?}", e)),
                }
            }
            "?skipoff" => {
                if !is_channel_owner {
                    return Err("User is not the channel owner.".into());
                };
                let res = disable_song_skip(&channel_name).await;
                match res {
                    Ok(s) => {
                        if s.is_success() {
                            return Ok(Some(Reply {
                                reply_type: ReplyType::Message,
                                message: Some("Vote skip is now disabled".into()),
                                ..Default::default()
                            }));
                        }

                        return Err(format!(
                            "Disabling song skip failed with status code {}",
                            s.to_string()
                        ));
                    }
                    Err(e) => Err(format!("Disabling song skip failed: {:?}", e)),
                }
            }
            "?commands" => {
                return Ok(Some(Reply {
                    reply_type: ReplyType::Message,
                    message: Some("?song ?songlink ?slink ?skip ?skipon ?skipoff".into()),
                    ..Default::default()
                }));
            }
            _ => Ok(None),
        },
        None => Err("No token found.".into()),
    }
}

async fn enable_song_skip(
    channel_name: &str,
) -> Result<reqwest::StatusCode, Box<dyn std::error::Error>> {
    let client = reqwest::Client::new();
    let url = dotenv::var("WEB_URI").unwrap() + "/api/commands/skip/add";
    let params = [("channel_name", channel_name)];
    let url = reqwest::Url::parse_with_params(url.as_str(), &params)?;
    let status = client.post(url).send().await?.status();

    return Ok(status);
}

async fn disable_song_skip(
    channel_name: &str,
) -> Result<reqwest::StatusCode, Box<dyn std::error::Error>> {
    let client = reqwest::Client::new();
    let url = dotenv::var("WEB_URI").unwrap() + "/api/commands/skip/remove";
    let params = [("channel_name", channel_name)];
    let url = reqwest::Url::parse_with_params(url.as_str(), &params)?;
    let status = client.post(url).send().await?.status();

    return Ok(status);
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
