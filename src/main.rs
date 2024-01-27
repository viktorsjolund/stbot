use base64::{prelude::BASE64_STANDARD, Engine};
use dotenv;
use serde::Deserialize;
use std::collections::HashMap;
use std::str;
use websocket::{ws::dataframe::DataFrame, ClientBuilder, Message};

#[tokio::main]
async fn main() {
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

    let (mut reciever, mut sender) = client.split().unwrap();

    for message in reciever.incoming_messages() {
        match message {
            Ok(owned_message) => {
                if owned_message.is_data() {
                    let payload = &Message::from(owned_message).take_payload();
                    let message_as_str = str::from_utf8(&payload).unwrap();
                    let messages: Vec<&str> = message_as_str.trim_end().split("\r\n").collect();
                    for m in messages.iter() {
                        let parsed_message = parse_message(m).unwrap();
                        println!("{:?}", parsed_message);
                        let response = generate_response(parsed_message).await;
                        if let Ok(r) = response {
                            let reply = Message::text(r);
                            let _ = sender.send_message(&reply);
                        }
                    }
                }
            }
            Err(e) => {
                println!("An error occured: {:?}", e);
            }
        }
    }
}

async fn generate_response(
    parsed_message: HashMap<String, HashMap<String, HashMap<String, Vec<HashMap<String, String>>>>>,
) -> Result<String, ()> {
    let default_str = "".to_string();
    let command = parsed_message
        .get("command")
        .and_then(|m| m.get("data"))
        .and_then(|m| m.get("data"))
        .and_then(|v| v.get(0))
        .and_then(|m| m.get("command"))
        .unwrap_or(&default_str)
        .as_str();

    let channel = parsed_message
        .get("command")
        .and_then(|m| m.get("data"))
        .and_then(|m| m.get("data"))
        .and_then(|v| v.get(0))
        .and_then(|m| m.get("channel"))
        .unwrap_or(&default_str)
        .as_str();

    let parameter_message = parsed_message
        .get("parameters")
        .and_then(|m| m.get("data"))
        .and_then(|m| m.get("data"))
        .and_then(|v| v.get(0))
        .and_then(|m| m.get("message"))
        .unwrap_or(&default_str)
        .as_str();

    let message_id = parsed_message
        .get("tags")
        .and_then(|m| m.get("id"))
        .and_then(|m| m.get("data"))
        .and_then(|v| v.get(0))
        .and_then(|m| m.get("tagValue"))
        .unwrap_or(&default_str)
        .as_str();

    match command {
        "PING" => {
            return Ok(format!("PONG {}", parameter_message));
        }
        "PRIVMSG" => {
            let reply = reply_message(parameter_message).await?;
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
                    println!("--error-- {:?}", e);
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

fn parse_message(
    irc_message: &str,
) -> Option<HashMap<String, HashMap<String, HashMap<String, Vec<HashMap<String, String>>>>>> {
    println!("raw msg: {:?}", irc_message);
    let mut parsed_message = HashMap::new();
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

    let mut parsed_command = parse_command(raw_command_component).unwrap_or_else(|| HashMap::new());
    let command_map = create_nesting(&mut parsed_command);
    parsed_message.insert("command".to_string(), command_map);
    if let Some(m) = parsed_message.get("command") {
        if m.len() > 0 {
            if raw_tags_component != "" {
                let parsed_tags = parse_tags(raw_tags_component);
                parsed_message.insert("tags".to_string(), parsed_tags);
            }

            let mut parsed_source =
                parse_source(raw_source_component).unwrap_or_else(|| HashMap::new());
            let source_map = create_nesting(&mut parsed_source);
            parsed_message.insert("source".to_string(), source_map);

            let mut param_m = HashMap::new();
            param_m.insert("message".to_string(), raw_parameters_component.to_string());
            let param_map = create_nesting(&mut param_m);
            parsed_message.insert("parameters".to_string(), param_map);

            if raw_parameters_component == "!" {
                let mut parsed_params =
                    parse_parameters(raw_parameters_component, &mut parsed_command);
                let parameter_map = create_nesting(&mut parsed_params);
                parsed_message.insert("command".to_string(), parameter_map);
            }
        } else {
            return None;
        }
    }

    return Some(parsed_message);
}

fn parse_tags(tags: &str) -> HashMap<String, HashMap<String, Vec<HashMap<String, String>>>> {
    let tags_to_ignore = vec!["client-nonce", "flags"];
    let mut dict_parsed_tags = HashMap::new();
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
                    let mut dict = HashMap::new();
                    let badges: Vec<&str> = tag_value.split(",").collect();
                    for b in badges.iter() {
                        let badge_parts: Vec<&str> = b.split("/").collect();
                        dict.insert(badge_parts[0].to_string(), badge_parts[1].to_string());
                    }
                    let mut v = Vec::new();
                    v.push(dict);
                    let mut m = HashMap::new();
                    m.insert("dict".to_string(), v);
                    dict_parsed_tags.insert(parsed_tag[0].to_string(), m);
                } else {
                    let mut v = Vec::new();
                    v.push(HashMap::new());
                    let mut m = HashMap::new();
                    m.insert("dict".to_string(), v);
                    dict_parsed_tags.insert(parsed_tag[0].to_string(), m);
                }
            }
            "emotes" => {
                if tag_value != "" {
                    let mut dict_emotes = HashMap::new();
                    let emotes: Vec<&str> = tag_value.split("/").collect();
                    for e in emotes.iter() {
                        let emote_parts: Vec<&str> = e.split(":").collect();
                        let mut text_positions = Vec::new();
                        let positions: Vec<&str> = emote_parts[1].split(",").collect();
                        for p in positions.iter() {
                            let position_parts: Vec<&str> = p.split("-").collect();
                            let mut pos_map = HashMap::new();
                            pos_map.insert(
                                String::from("startPosition"),
                                position_parts[0].to_string(),
                            );
                            pos_map
                                .insert(String::from("endPosition"), position_parts[1].to_string());
                            text_positions.push(pos_map);
                        }
                        dict_emotes.insert(emote_parts[0].to_string(), text_positions);
                    }
                    dict_parsed_tags.insert(parsed_tags[0].to_string(), dict_emotes);
                } else {
                    let mut v = Vec::new();
                    v.push(HashMap::new());
                    let mut m = HashMap::new();
                    m.insert("data".to_string(), v);
                    dict_parsed_tags.insert(parsed_tag[0].to_string(), m);
                }
            }
            "emote-sets" => {
                let mut m1 = HashMap::new();
                let emote_set_ids: Vec<&str> = tag_value.split(",").collect();
                for (i, es) in emote_set_ids.iter().enumerate() {
                    m1.insert(i.to_string(), es.to_owned().to_string());
                }
                let mut v = Vec::new();
                v.push(m1);
                let mut m = HashMap::new();
                m.insert("emoteSets".to_string(), v);
                dict_parsed_tags.insert(parsed_tag[0].to_string(), m);
            }
            _ => {
                if !tags_to_ignore.contains(&parsed_tag[0]) {
                    let mut m = HashMap::new();
                    let mut v = Vec::new();
                    let mut m1 = HashMap::new();
                    m1.insert("tagValue".to_string(), tag_value.to_string());
                    v.push(m1);
                    m.insert("data".to_string(), v);
                    dict_parsed_tags.insert(parsed_tag[0].to_string(), m);
                }
            }
        }
    }

    return dict_parsed_tags;
}

fn parse_command(raw_command_component: &str) -> Option<HashMap<String, String>> {
    let mut parsed_command = HashMap::new();
    let command_parts: Vec<&str> = raw_command_component.split(" ").collect();

    match command_parts[0] {
        "JOIN" | "PART" | "NOTICE" | "CLEARCHAT" | "HOSTTARGET" | "PRIVMSG" => {
            parsed_command.insert("command".to_string(), command_parts[0].to_string());
            parsed_command.insert("channel".to_string(), command_parts[1].to_string());
        }
        "PING" => {
            parsed_command.insert("command".to_string(), command_parts[0].to_string());
        }
        "CAP" => {
            parsed_command.insert("command".to_string(), command_parts[0].to_string());
            let is_enabled = command_parts[2] == "ACK";
            parsed_command.insert("isCapRequestEnabled".to_string(), is_enabled.to_string());
        }
        "GLOBALUSERSTATE" => {
            parsed_command.insert("command".to_string(), command_parts[0].to_string());
        }
        "USERSTATE" | "ROOMSTATE" => {
            parsed_command.insert("command".to_string(), command_parts[0].to_string());
            parsed_command.insert("channel".to_string(), command_parts[1].to_string());
        }
        "RECONNECT" => {
            println!("The Twitch IRC server is about to terminate the connection for maintenance.");
            parsed_command.insert("command".to_string(), command_parts[0].to_string());
        }
        "421" => {
            println!("Unsupported IRC command: {:?}", command_parts[2]);
            return None;
        }
        "001" => {
            parsed_command.insert("command".to_string(), command_parts[0].to_string());
            parsed_command.insert("channel".to_string(), command_parts[1].to_string());
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

    return Some(parsed_command);
}

fn parse_source(raw_source_component: &str) -> Option<HashMap<String, String>> {
    if raw_source_component == "" {
        return None;
    } else {
        let source_parts: Vec<&str> = raw_source_component.split("!").collect();
        let mut m = HashMap::new();
        if source_parts.len() == 2 {
            m.insert("host".to_string(), source_parts[1].to_string());
            m.insert("nick".to_string(), source_parts[0].to_string());
        } else {
            m.insert("nick".to_string(), "".to_string());
            m.insert("host".to_string(), source_parts[0].to_string());
        }

        return Some(m);
    }
}

fn parse_parameters(
    raw_parameters_component: &str,
    command: &mut HashMap<String, String>,
) -> HashMap<String, String> {
    let idx = 0;
    let command_parts = raw_parameters_component[idx + 1..].trim();
    let params_idx = command_parts.chars().position(|c| c == ' ');
    if let Some(p) = params_idx {
        command.insert("botCommand".to_string(), command_parts[0..p].to_string());
        command.insert(
            "botCommandParams".to_string(),
            command_parts[p..].trim().to_string(),
        );
    } else {
        command.insert("botCommand".to_string(), command_parts.to_string());
    }

    return command.to_owned();
}

fn create_nesting(
    map: &mut HashMap<String, String>,
) -> HashMap<String, HashMap<String, Vec<HashMap<String, String>>>> {
    let mut v = Vec::new();
    v.push(map.to_owned());
    let mut m1 = HashMap::new();
    let mut m2 = HashMap::new();
    m2.insert("data".to_string(), v);
    m1.insert("data".to_string(), m2.to_owned());

    return m1;
}
