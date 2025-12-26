use std::str::FromStr;

use crate::channel::Message;
use anyhow::{anyhow, bail};

pub enum Command {
    Share {
        file: String,
    },
    Download {
        ticket: String,
        output_file: Option<String>,
    },
    Message(Message),
    Quit,
}

impl FromStr for Command {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s.starts_with("#") {
            let mut parts = s.splitn(2, ' ');
            match parts.next().unwrap() {
                "#share" | "#s" => Ok(Command::Share {
                    file: parts
                        .next()
                        .ok_or_else(|| anyhow!("Missing part"))?
                        .to_string(),
                }),
                "#download" | "#d" => {
                    let params = parts.next().ok_or_else(|| anyhow!("Missing part"))?;
                    let mut parts = params.splitn(2, ' ');
                    Ok(Command::Download {
                        ticket: parts
                            .next()
                            .ok_or_else(|| anyhow!("Missing part"))?
                            .to_string(),
                        output_file: parts.next().map(|s| s.to_string()),
                    })
                }
                "#quit" | "#q" => Ok(Command::Quit),
                cmd => bail!("Unknown command {cmd}"),
            }
        } else {
            Ok(Command::Message(Message::new_message(s)))
        }
    }
}
