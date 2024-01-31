use base64::{prelude::BASE64_STANDARD, Engine};
use dotenv;
use futures_lite::stream::StreamExt;
use lapin::{options::*, types::FieldTable, Connection, ConnectionProperties};
use serde::Deserialize;
use std::collections::HashMap;
use std::str;
use std::sync::Arc;
use tokio::sync::Mutex;
use websocket::{ws::dataframe::DataFrame, ClientBuilder, Message};

#[derive(Default, Debug)]
struct MessageResponse {
    tags: Option<Tags>,
    source: Source,
    command: Option<Command>,
    parameters: Option<String>,
}

#[derive(Default, Debug)]
struct Tags {
    badges: Option<HashMap<String, String>>,
    emote_sets: Option<HashMap<String, String>>,
    emotes: Option<HashMap<String, Vec<Emote>>>,
    other: Option<HashMap<String, String>>,
}

#[derive(Default, Debug)]
#[allow(dead_code)]
struct Emote {
    start_position: Option<String>,
    end_position: Option<String>,
}
#[derive(Default, Debug)]
struct Source {
    nick: Option<String>,
    host: Option<String>,
}

#[derive(Default, Debug, Clone)]
struct Command {
    command: Option<String>,
    channel: Option<String>,
    is_cap_request_enabled: Option<bool>,
    bot_command: Option<String>,
    bot_command_params: Option<String>,
}

#[tokio::main]
async fn main() {
    let addr = dotenv::var("AMQP_ADDR").unwrap_or_else(|_| "amqp://localhost:5672".into());
    let mut client = ClientBuilder::new("ws://irc-ws.chat.twitch.tv:80")
        .unwrap()
        .connect_insecure()
        .unwrap();

    let cap_req = Message::text("CAP REQ :twitch.tv/tags twitch.tv/commands");
    let pass = Message::text(format!(
        "PASS {}",
        dotenv::var("TWITCH_OAUTH_TOKEN").unwrap()
    ));
    let nick = Message::text(format!(
        "NICK {}",
        dotenv::var("TWITCH_DISPLAY_NAME").unwrap()
    ));
    let join = Message::text(format!(
        "JOIN #{}",
        dotenv::var("TWITCH_DISPLAY_NAME").unwrap()
    ));
    let _ = client.send_message(&cap_req);
    let _ = client.send_message(&pass);
    let _ = client.send_message(&nick);
    let _ = client.send_message(&join);

    let conn = Connection::connect(&addr, ConnectionProperties::default())
        .await
        .unwrap();

    let channel_a = conn.create_channel().await.unwrap();
    let (reciever, sender) = client.split().unwrap();
    let rec = Arc::new(Mutex::new(reciever));
    let sen = Arc::new(Mutex::new(sender));

    for _ in 0..1 {
        let reciever = Arc::clone(&rec);
        let sender = Arc::clone(&sen);
        tokio::spawn(async move {
            for message in reciever.lock().await.incoming_messages() {
                match message {
                    Ok(owned_message) => {
                        if owned_message.is_data() {
                            let payload = &Message::from(owned_message).take_payload();
                            let message_as_str = str::from_utf8(&payload).unwrap();
                            let messages: Vec<&str> =
                                message_as_str.trim_end().split("\r\n").collect();
                            for m in messages.iter() {
                                let parsed_message = parse_message(m).unwrap();
                                println!("{:?}", parsed_message);
                                let response = generate_response(parsed_message).await;
                                if let Ok(r) = response {
                                    let reply = Message::text(r);
                                    let _ = sender.lock().await.send_message(&reply);
                                }
                            }
                        }
                    }
                    Err(e) => {
                        println!("An error occured: {:?}", e);
                    }
                }
            }
        });
    }

    let mut consumer = channel_a
        .basic_consume(
            "send",
            "bot",
            BasicConsumeOptions::default(),
            FieldTable::default(),
        )
        .await
        .unwrap();

    for _ in 0..1 {
        let sender = Arc::clone(&sen);
        while let Some(delivery) = consumer.next().await {
            let delivery = delivery.expect("error in consumer");
            let data = str::from_utf8(&delivery.data).unwrap();
            let msg = Message::text(data);
            let _ = sender.lock().await.send_message(&msg);
            delivery.ack(BasicAckOptions::default()).await.unwrap();
        }
    }
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
            let reply = reply_message(message.as_str()).await?;
            return Ok(format!(
                "@reply-parent-msg-id={} PRIVMSG {} :{}",
                message_id, channel, reply
            ));
        }
        _ => return Err(()),
    }
}

async fn reply_message(user_msg: &str) -> Result<String, ()> {
    let tokens: Vec<&str> = user_msg.split(" ").collect();

    match tokens.get(0) {
        Some(&"?song") => {
            let song = get_spotify_song().await;
            match song {
                Ok(s) => {
                    if s.is_playing {
                        return Ok(format!("{} - {}", s.item.artists[0].name, s.item.name));
                    } else {
                        return Ok("No song currently playing.".to_string());
                    }
                }
                Err(e) => {
                    println!("{:?}", e);
                    return Err(());
                }
            };
        }
        Some(&"?slink") => {
            let song = get_spotify_song().await;
            match song {
                Ok(s) => {
                    if s.is_playing {
                        return Ok(s.item.external_urls.spotify);
                    } else {
                        return Ok("No song currently playing.".to_string());
                    }
                }
                Err(e) => {
                    println!("--error-- {:?}", e);
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

async fn get_spotify_song() -> Result<SongResponse, Box<dyn std::error::Error>> {
    let client = reqwest::Client::new();
    let access_token = get_spotify_access_token().await?;
    let res = client
        .get("https://api.spotify.com/v1/me/player/currently-playing")
        .bearer_auth(access_token)
        .send()
        .await?
        .json::<SongResponse>()
        .await?;

    return Ok(res);
}

#[derive(Deserialize, Debug)]
struct SpotifyAccessTokenResponse {
    access_token: String,
}

async fn get_spotify_access_token() -> Result<String, Box<dyn std::error::Error>> {
    let client = reqwest::Client::new();
    let spotify_client_id = dotenv::var("SPOTIFY_CLIENT_ID").unwrap();
    let spotify_client_secret = dotenv::var("SPOTIFY_CLIENT_SECRET").unwrap();
    let spotify_refresh_token = dotenv::var("SPOTIFY_REFRESH_TOKEN").unwrap();
    let encoded_client_details = BASE64_STANDARD
        .encode(format!("{}:{}", spotify_client_id, spotify_client_secret).as_bytes());
    let mut params = HashMap::new();
    params.insert("grant_type", "refresh_token".to_owned());
    params.insert("refresh_token", spotify_refresh_token);
    let res = client
        .post("https://accounts.spotify.com/api/token")
        .form(&params)
        .header(
            "Authorization",
            "Basic ".to_owned() + encoded_client_details.as_str(),
        )
        .send()
        .await?
        .json::<SpotifyAccessTokenResponse>()
        .await?;

    return Ok(res.access_token);
}

fn parse_message(irc_message: &str) -> Option<MessageResponse> {
    println!("raw msg: {:?}", irc_message);
    let mut pm = MessageResponse {
        ..Default::default()
    };
    let mut idx = 0;
    let mut raw_tags_component: &str = "";
    let mut raw_source_component: &str = "";
    let mut raw_parameters_component: &str = "";

    if irc_message.chars().nth(idx).unwrap() == '@' {
        let end_idx = irc_message.chars().position(|c| c == ' ').unwrap();
        raw_tags_component = &irc_message[1..end_idx];
        idx = end_idx + 1;
    }

    if irc_message.chars().nth(idx).unwrap() == ':' {
        idx += 1;
        let end_idx = irc_message[idx..].chars().position(|c| c == ' ').unwrap() + idx;
        raw_source_component = &irc_message[idx..end_idx];
        idx = end_idx + 1;
    }

    let end_idx = irc_message[idx..]
        .chars()
        .position(|c| c == ':')
        .unwrap_or_else(|| irc_message.len() - idx)
        + idx;

    let raw_command_component = irc_message[idx..end_idx].trim();

    if end_idx != irc_message.len() {
        idx = end_idx + 1;
        raw_parameters_component = &irc_message[idx..];
    }

    let parsed_command = parse_command(raw_command_component);
    pm.command = parsed_command;
    if let Some(c) = pm.command.clone() {
        if raw_tags_component != "" {
            let parsed_tags = parse_tags(raw_tags_component);
            pm.tags = Some(parsed_tags);
        }

        let parsed_source = parse_source(raw_source_component);
        pm.source = parsed_source;

        pm.parameters = Some(raw_parameters_component.to_string());

        if raw_parameters_component == "!" {
            let parsed_params = parse_parameters(raw_parameters_component, c);
            pm.command = Some(parsed_params);
        }
    }

    return Some(pm);
}

fn parse_tags(tags: &str) -> Tags {
    let tags_to_ignore = vec!["client-nonce", "flags"];
    let mut ta = Tags {
        ..Default::default()
    };
    let parsed_tags: Vec<&str> = tags.split(";").collect();
    for t in parsed_tags.iter() {
        let parsed_tag: Vec<&str> = t.split("=").collect();
        let mut tag_value = "";
        if let Some(&v) = parsed_tag.get(1) {
            if v != "" {
                tag_value = v;
            }
        }

        match parsed_tag[0] {
            "badges" | "badge-info" => {
                if tag_value != "" {
                    let mut badges_map = HashMap::new();
                    if let Some(prev) = ta.badges {
                        badges_map.extend(prev);
                    }
                    let badges: Vec<&str> = tag_value.split(",").collect();
                    for b in badges.iter() {
                        let badge_parts: Vec<&str> = b.split("/").collect();
                        badges_map.insert(badge_parts[0].to_string(), badge_parts[1].to_string());
                    }
                    ta.badges = Some(badges_map);
                }
            }
            "emotes" => {
                if tag_value != "" {
                    let mut dict_emotes = HashMap::new();
                    if let Some(prev) = ta.emotes {
                        dict_emotes.extend(prev);
                    }
                    let emotes: Vec<&str> = tag_value.split("/").collect();
                    for e in emotes.iter() {
                        let emote_parts: Vec<&str> = e.split(":").collect();
                        let mut text_positions: Vec<Emote> = Vec::new();
                        let positions: Vec<&str> = emote_parts[1].split(",").collect();
                        for p in positions.iter() {
                            let position_parts: Vec<&str> = p.split("-").collect();
                            text_positions.push(Emote {
                                start_position: match position_parts.get(0) {
                                    Some(s) => Some(s.to_string()),
                                    None => None,
                                },
                                end_position: match position_parts.get(1) {
                                    Some(s) => Some(s.to_string()),
                                    None => None,
                                },
                            });
                        }
                        dict_emotes.insert(emote_parts[0].to_string(), text_positions);
                    }
                    ta.emotes = Some(dict_emotes);
                }
            }
            "emote-sets" => {
                let mut emote_sets = HashMap::new();
                let emote_set_ids: Vec<&str> = tag_value.split(",").collect();
                for (i, es) in emote_set_ids.iter().enumerate() {
                    emote_sets.insert(i.to_string(), es.to_owned().to_string());
                }
                ta.emote_sets = Some(emote_sets);
            }
            _ => {
                if !tags_to_ignore.contains(&parsed_tag[0]) {
                    let mut other = HashMap::new();
                    other.insert(parsed_tag[0].to_string(), tag_value.to_string());
                    if let Some(prev) = ta.other {
                        other.extend(prev);
                    }
                    ta.other = Some(other);
                }
            }
        }
    }

    return ta;
}

fn parse_command(raw_command_component: &str) -> Option<Command> {
    let mut pc = Command {
        ..Default::default()
    };
    let command_parts: Vec<&str> = raw_command_component.split(" ").collect();

    match command_parts[0] {
        "JOIN" | "PART" | "NOTICE" | "CLEARCHAT" | "HOSTTARGET" | "PRIVMSG" => {
            pc.command = match command_parts.get(0) {
                Some(s) => Some(s.to_string()),
                None => None,
            };
            pc.channel = match command_parts.get(1) {
                Some(s) => Some(s.to_string()),
                None => None,
            };
        }
        "PING" => {
            pc.command = match command_parts.get(0) {
                Some(s) => Some(s.to_string()),
                None => None,
            };
        }
        "CAP" => {
            pc.command = match command_parts.get(0) {
                Some(s) => Some(s.to_string()),
                None => None,
            };
            let is_enabled = match command_parts.get(2) {
                Some(s) => Some(s == &"ACK"),
                None => None,
            };
            pc.is_cap_request_enabled = is_enabled;
        }
        "GLOBALUSERSTATE" => {
            pc.command = match command_parts.get(0) {
                Some(s) => Some(s.to_string()),
                None => None,
            };
        }
        "USERSTATE" | "ROOMSTATE" => {
            pc.command = match command_parts.get(0) {
                Some(s) => Some(s.to_string()),
                None => None,
            };
            pc.channel = match command_parts.get(1) {
                Some(s) => Some(s.to_string()),
                None => None,
            };
        }
        "RECONNECT" => {
            println!("The Twitch IRC server is about to terminate the connection for maintenance.");
            pc.command = match command_parts.get(0) {
                Some(s) => Some(s.to_string()),
                None => None,
            };
        }
        "421" => {
            println!("Unsupported IRC command: {:?}", command_parts[2]);
            return None;
        }
        "001" => {
            pc.command = match command_parts.get(0) {
                Some(s) => Some(s.to_string()),
                None => None,
            };
            pc.channel = match command_parts.get(1) {
                Some(s) => Some(s.to_string()),
                None => None,
            };
        }
        "002" | "003" | "004" | "353" | "366" | "372" | "375" | "376" => {
            println!("numeric message: {:?}", command_parts[0]);
            return None;
        }
        _ => {
            println!("\nUnexpected command: {:?}\n", command_parts[0]);
            return None;
        }
    }

    return Some(pc);
}

fn parse_source(raw_source_component: &str) -> Source {
    let mut s = Source {
        ..Default::default()
    };
    if raw_source_component == "" {
        return s;
    } else {
        let source_parts: Vec<&str> = raw_source_component.split("!").collect();
        if source_parts.len() == 2 {
            s.nick = match source_parts.get(0) {
                Some(s) => Some(s.to_string()),
                None => None,
            };
            s.host = match source_parts.get(1) {
                Some(s) => Some(s.to_string()),
                None => None,
            };
        } else {
            s.nick = None;
            s.host = match source_parts.get(0) {
                Some(s) => Some(s.to_string()),
                None => None,
            };
        }

        return s;
    }
}

fn parse_parameters(raw_parameters_component: &str, command: Command) -> Command {
    let mut command = command.clone();
    let idx = 0;
    let command_parts = raw_parameters_component[idx + 1..].trim();
    let params_idx = command_parts.chars().position(|c| c == ' ');
    if let Some(p) = params_idx {
        command.bot_command = Some(command_parts[0..p].to_string());
        command.bot_command_params = Some(command_parts[p..].trim().to_string());
    } else {
        command.bot_command = Some(command_parts.to_string());
    }

    return command;
}
