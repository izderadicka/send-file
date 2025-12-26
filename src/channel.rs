use std::{fmt::Display, str::FromStr};

use anyhow::{Context as _, Result};
use iroh::{EndpointAddr, EndpointId, PublicKey, SecretKey, Signature};
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
        Ok(postcard::from_bytes(bytes)?)
    }
}

impl Into<Vec<u8>> for &Ticket {
    fn into(self) -> Vec<u8> {
        postcard::to_stdvec(self).unwrap()
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
    pub ts: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub enum MessageBody {
    Intro { name: String },
    Message { text: String },
}

impl Message {
    pub fn new_intro(name: String) -> Self {
        Message {
            body: MessageBody::Intro { name },
            id: uuid::Uuid::new_v4(),
            ts: now(),
        }
    }

    pub fn new_message(text: impl Into<String>) -> Self {
        let text = text.into();
        Message {
            body: MessageBody::Message { text },
            id: uuid::Uuid::new_v4(),
            ts: now(),
        }
    }

    pub fn sign_and_encode(&self, key: &SecretKey) -> Result<Vec<u8>> {
        let data = postcard::to_stdvec(self)?;
        let signature = key.sign(&data);
        let envelope = MessageEnvelope {
            from: key.public(),
            data,
            signature,
        };
        Ok(postcard::to_stdvec(&envelope)?)
    }
}

impl TryFrom<&[u8]> for Message {
    type Error = anyhow::Error;
    fn try_from(bytes: &[u8]) -> Result<Self, Self::Error> {
        Ok(postcard::from_bytes(bytes)?)
    }
}

impl Into<Vec<u8>> for &Message {
    fn into(self) -> Vec<u8> {
        postcard::to_stdvec(self).unwrap()
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub struct MessageEnvelope {
    pub from: EndpointId,
    pub data: Vec<u8>,
    pub signature: Signature,
}

const MESSAGE_TTL: u64 = 60 * 60 * 1000;
impl MessageEnvelope {
    pub fn decode_and_verify(data: &[u8]) -> Result<(PublicKey, Message)> {
        let envelope: MessageEnvelope = postcard::from_bytes(data).unwrap();
        let public_key = envelope.from;
        public_key
            .verify(&envelope.data, &envelope.signature)
            .context("Invalid signature")?;

        let message: Message = postcard::from_bytes(&envelope.data)?;
        if message.ts < now() - MESSAGE_TTL {
            anyhow::bail!("Message expired");
        }
        Ok((public_key, message))
    }
}

fn now() -> u64 {
    u64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .expect("TS bigger than u64")
}
