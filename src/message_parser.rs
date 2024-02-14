use std::collections::HashMap;

#[derive(Default, Debug)]
pub struct MessageResponse {
    pub tags: Option<Tags>,
    pub source: Source,
    pub command: Option<Command>,
    pub parameters: Option<String>,
}

#[derive(Default, Debug)]
pub struct Tags {
    badges: Option<HashMap<String, String>>,
    emote_sets: Option<HashMap<String, String>>,
    emotes: Option<HashMap<String, Vec<Emote>>>,
    pub other: Option<HashMap<String, String>>,
}

#[derive(Default, Debug)]
#[allow(dead_code)]
struct Emote {
    start_position: Option<String>,
    end_position: Option<String>,
}
#[derive(Default, Debug)]
pub struct Source {
    nick: Option<String>,
    host: Option<String>,
}

#[derive(Default, Debug, Clone)]
pub struct Command {
    pub command: Option<String>,
    pub channel: Option<String>,
    is_cap_request_enabled: Option<bool>,
    bot_command: Option<String>,
    bot_command_params: Option<String>,
}

pub fn parse_message(irc_message: &str) -> Option<MessageResponse> {
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
            println!("[INFO] The Twitch IRC server is about to terminate the connection for maintenance.");
            pc.command = match command_parts.get(0) {
                Some(s) => Some(s.to_string()),
                None => None,
            };
        }
        "421" => {
            println!("[INFO] Unsupported IRC command: {:?}", command_parts[2]);
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
            println!("[INFO] Numeric message: {:?}", command_parts[0]);
            return None;
        }
        _ => {
            println!("[INFO] Unexpected command: {:?}", command_parts[0]);
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