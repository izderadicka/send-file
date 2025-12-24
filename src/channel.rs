use std::{fmt::Display, str::FromStr};

use iroh::{EndpointAddr, EndpointId};
use iroh_gossip::TopicId;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct Ticket {
    pub topic: TopicId,
    pub endpoints: Vec<EndpointAddr>,
}

impl TryFrom<&[u8]> for Ticket {
    type Error = anyhow::Error;
    fn try_from(bytes: &[u8]) -> Result<Self, Self::Error> {
        Ok(serde_json::from_slice(bytes)?)
    }
}

impl Into<Vec<u8>> for &Ticket {
    fn into(self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap()
    }
}

impl Display for Ticket {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let data: Vec<u8> = self.into();
        let mut encoded = data_encoding::BASE32_NOPAD.encode(&data);
        encoded.make_ascii_lowercase();
        write!(f, "{}", encoded)
    }
}

impl FromStr for Ticket {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let data = data_encoding::BASE32_NOPAD.decode(s.to_ascii_uppercase().as_bytes())?;
        Ticket::try_from(data.as_slice())
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Message {
    pub body: MessageBody,
    pub id: uuid::Uuid,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum MessageBody {
    Intro { from: EndpointId, name: String },
    Message { from: EndpointId, text: String },
}

impl Message {
    pub fn new_intro(from: EndpointId, name: String) -> Self {
        Message {
            body: MessageBody::Intro { from, name },
            id: uuid::Uuid::new_v4(),
        }
    }

    pub fn new_message(from: EndpointId, text: String) -> Self {
        Message {
            body: MessageBody::Message { from, text },
            id: uuid::Uuid::new_v4(),
        }
    }
}

impl TryFrom<&[u8]> for Message {
    type Error = anyhow::Error;
    fn try_from(bytes: &[u8]) -> Result<Self, Self::Error> {
        Ok(serde_json::from_slice(bytes)?)
    }
}

impl Into<Vec<u8>> for &Message {
    fn into(self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap()
    }
}
