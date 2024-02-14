mod message_parser;
use message_parser::{parse_message, MessageResponse};
use futures::{SinkExt, StreamExt};
use lapin::{options::*, types::FieldTable, Connection, ConnectionProperties};
use serde::Deserialize;
use std::str;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio_tungstenite::connect_async;

#[tokio::main]
async fn main() {
    let amqp_addr = dotenv::var("AMQP_ADDR").unwrap_or_else(|_| "amqp://localhost:5672".into());
    let amqp_conn = Connection::connect(&amqp_addr, ConnectionProperties::default())
        .await
        .unwrap();
    let channel = amqp_conn.create_channel().await.unwrap();

    let (ws_stream, _) = connect_async("ws://irc-ws.chat.twitch.tv:80")
        .await
        .unwrap();
    let (mut ws_tx, ws_rx) = ws_stream.split();

    ws_tx
        .send("CAP REQ :twitch.tv/tags twitch.tv/commands".into())
        .await
        .unwrap();
    ws_tx
        .send(format!("PASS {}", dotenv::var("TWITCH_OAUTH_TOKEN").unwrap()).into())
        .await
        .unwrap();
    ws_tx
        .send(format!("NICK {}", dotenv::var("TWITCH_BOT_NICK").unwrap()).into())
        .await
        .unwrap();
    ws_tx
        .send(format!("JOIN #{}", dotenv::var("TWITCH_CHANNEL_NAME").unwrap()).into())
        .await
        .unwrap();
    let mut active_users = get_active_users().await.unwrap().users;
    for username in active_users.iter_mut() {
        *username = format!("#{}", username)
    }
    if active_users.len() > 0 {
        ws_tx
            .send(format!("JOIN {}", active_users.join(",")).into())
            .await
            .unwrap();
    }

    let sender = Arc::new(Mutex::new(ws_tx));
    let reciever = Arc::new(Mutex::new(ws_rx));

    for _ in 0..1 {
        let reciever = reciever.clone();
        let sender = sender.clone();
        tokio::spawn(async move {
            while let Some(result) = reciever.lock().await.next().await {
                match result {
                    Ok(message) => {
                        if message.is_text() {
                            let message_str = message.to_string();
                            let messages: Vec<&str> =
                                message_str.trim_end().split("\r\n").collect();
                            for m in messages.iter() {
                                println!("[INFO] Message: {}", m);
                                let parsed_message = parse_message(m).unwrap();
                                let response = generate_response(parsed_message).await;
                                if let Ok(r) = response {
                                    println!("[INFO] Response: {}", r);
                                    let _ =
                                        sender.lock().await.send(r.into()).await.unwrap_or_else(
                                            |e| eprintln!("[ERROR] Sender Error: {:?}", e),
                                        );
                                }
                            }
                        }
                    }
                    Err(e) => panic!("[ERROR] Websocket Error: {:?}", e),
                }
            }
        });
    }

    let mut consumer = channel
        .basic_consume(
            "send",
            "bot",
            BasicConsumeOptions::default(),
            FieldTable::default(),
        )
        .await
        .unwrap();

    for _ in 0..1 {
        let sender = sender.clone();
        while let Some(delivery) = consumer.next().await {
            let delivery = delivery.expect("error in consumer");
            let data = str::from_utf8(&delivery.data).unwrap();
            sender.lock().await.send(data.into()).await.unwrap();
            delivery.ack(BasicAckOptions::default()).await.unwrap();
        }
    }
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

async fn generate_response(parsed_message: MessageResponse) -> Result<String, ()> {
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
            return Ok(format!("PONG {}", message));
        }
        "PRIVMSG" => {
            let reply = reply_message(message.as_str(), &channel[1..]).await?;
            return Ok(format!(
                "@reply-parent-msg-id={} PRIVMSG {} :{}",
                message_id, channel, reply
            ));
        }
        _ => return Err(()),
    }
}

async fn reply_message(user_msg: &str, channel_name: &str) -> Result<String, ()> {
    let tokens: Vec<&str> = user_msg.split(" ").collect();

    match tokens.get(0) {
        Some(&"?song") => {
            let song = get_spotify_song(channel_name).await;
            match song {
                Ok(s) => {
                    if s.is_playing {
                        return Ok(format!("{} - {}", s.item.artists[0].name, s.item.name));
                    } 
                    return Ok("No song currently playing.".to_string());
                }
                Err(e) => {
                    eprintln!("[ERROR] Could not get song: {:?}", e);
                    return Err(());
                }
            };
        }
        Some(&"?slink") => {
            let song = get_spotify_song(channel_name).await;
            match song {
                Ok(s) => {
                    if s.is_playing {
                        return Ok(s.item.external_urls.spotify);
                    } 
                    return Ok("No song currently playing.".to_string());
                }
                Err(e) => {
                    eprintln!("[ERROR] Could not get song: {:?}", e);
                    return Err(());
                }
            };
        }
        None => {
            return Err(());
        }
        _ => return Err(()),
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
