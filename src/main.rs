use std::{
    collections::HashMap,
    path::{self, Path, PathBuf},
};

use anyhow::{Context as _, Result};
use clap::{Parser, Subcommand};
use futures_lite::StreamExt as _;
use iroh::{
    Endpoint, EndpointAddr, EndpointId, PublicKey, RelayMode, SecretKey,
    discovery::mdns::MdnsDiscovery, protocol::Router,
};
use iroh_blobs::{BlobsProtocol, store::mem::MemStore, ticket::BlobTicket};
use iroh_gossip::{
    Gossip, TopicId,
    api::{Event, GossipReceiver},
};
use tokio::{fs, io::AsyncWriteExt as _};

use crate::channel::{Message, MessageBody, Ticket};

mod channel;

#[derive(Parser, Debug)]
struct Args {
    #[arg(long, short, help = "Data directory", default_value = "./test-data")]
    data_dir: PathBuf,
    #[arg(long, short, help = "Identity name", required = true)]
    identity: String,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    Send {
        #[arg(required = true)]
        file: PathBuf,
    },
    Receive {
        #[arg(required = true)]
        ticket: String,
        #[arg(required = true)]
        file: PathBuf,
    },
    Publish,
    Subscribe {
        ticket: String,
    },
}

async fn load_identity(data_dir: &Path, name: &str) -> Result<SecretKey> {
    let file = data_dir.join(name).with_extension("id");
    if fs::try_exists(&file).await? {
        let key_data = fs::read(file).await?;
        if key_data.len() != 32 {
            anyhow::bail!("Secret key must be 32 bytes");
        }
        let key_ref: &[u8; 32] = key_data[..].try_into().unwrap();
        Ok(SecretKey::from_bytes(key_ref))
    } else {
        let key = SecretKey::generate(&mut rand::rng());
        fs::write(&file, key.to_bytes()).await?;
        Ok(key)
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let args = Args::parse();
    let mdns = MdnsDiscovery::builder();
    let secret_key = load_identity(&args.data_dir, &args.identity).await?;
    // let dht_discovery = iroh::discovery::pkarr::dht::DhtDiscovery::builder()
    //     .secret_key(secret_key.clone())
    //     .include_direct_addresses(true);
    let builder = Endpoint::empty_builder(RelayMode::Disabled)
        .secret_key(secret_key)
        // .discovery(dht_discovery);
        .discovery(mdns);
    let endpoint = builder.bind().await?;

    match args.command {
        Command::Send { file } => {
            let store = MemStore::new();
            let blobs = BlobsProtocol::new(&store, None);
            let tag = store.add_path(path::absolute(&file)?).await?;
            let ticket = BlobTicket::new(endpoint.id().into(), tag.hash, tag.format);
            let ticket = ticket.to_string();
            let file = tokio::fs::File::create("./test-data/file_ticket.txt").await?;
            let mut file = tokio::io::BufWriter::new(file);
            file.write_all(ticket.as_bytes()).await?;
            file.flush().await?;

            println!("Ticket: \n{}", ticket);
            let router = Router::builder(endpoint)
                .accept(iroh_blobs::ALPN, blobs)
                .spawn();
            tokio::signal::ctrl_c().await?;
            router.shutdown().await?;
        }
        Command::Receive { ticket, file } => {
            let store = MemStore::new();
            let ticket: BlobTicket = ticket.parse()?;
            let output_file = path::absolute(&file)?;
            let downloader = store.downloader(&endpoint);
            downloader
                .download(ticket.hash(), Some(ticket.addr().id))
                .await?;
            store.blobs().export(ticket.hash(), output_file).await?;
            endpoint.close().await;
        }

        Command::Publish => {
            let topic = TopicId::from_bytes(rand::random());
            let endpoints: Vec<EndpointAddr> = vec![];

            chat_main(args.identity.clone(), endpoint, topic, endpoints).await?
        }
        Command::Subscribe { ticket } => {
            let ticket: Ticket = ticket.parse().context("Invalid ticket")?;
            chat_main(
                args.identity.clone(),
                endpoint,
                ticket.topic,
                ticket.endpoints,
            )
            .await?
        }
    }

    Ok(())
}

async fn chat_main(
    identity: String,
    endpoint: Endpoint,
    topic: TopicId,
    endpoints: Vec<EndpointAddr>,
) -> Result<()> {
    let gossip = Gossip::builder().spawn(endpoint.clone());
    let router = Router::builder(endpoint.clone())
        .accept(iroh_gossip::ALPN, gossip.clone())
        .spawn();
    let ticket = Ticket {
        topic: topic.clone(),
        endpoints: vec![endpoint.id().into()],
    };
    let ticket = ticket.to_string();
    println!("Ticket: \n{}", ticket);
    fs::write("./test-data/chat_ticket.txt", ticket).await?;

    if endpoints.is_empty() {
        println!("Waiting somebody joins our channel")
    } else {
        println!("Waiting to join channel")
    }

    let endpoint_ids: Vec<_> = endpoints.iter().map(|ep| ep.id).collect();
    let (sender, receiver) = gossip
        .subscribe_and_join(topic, endpoint_ids)
        .await?
        .split();
    println!("Connected - start chat below");
    println!("------------------------------------");
    let message = Message::new_intro(endpoint.id(), identity.clone());
    let data: Vec<u8> = (&message).into();
    sender.broadcast(data.into()).await?;
    let (input_sender, mut input_receiver) = tokio::sync::mpsc::channel::<Message>(1);

    tokio::spawn(message_loop(
        receiver,
        input_sender.clone(),
        endpoint.id(),
        identity,
    ));

    std::thread::spawn(move || input_loop(input_sender, endpoint.id()));
    while let Some(message) = input_receiver.recv().await {
        let data: Vec<u8> = (&message).into();
        sender.broadcast(data.into()).await?;
    }

    router.shutdown().await?;
    Ok(())
}

async fn message_loop(
    mut receiver: GossipReceiver,
    input_sender: tokio::sync::mpsc::Sender<Message>,
    my_id: EndpointId,
    identity: String,
) -> Result<()> {
    let mut directory: HashMap<_, String> = HashMap::new();
    while let Some(event) = receiver.next().await {
        let event = event?;
        match event {
            Event::Received(msg) => {
                let message: Message = Message::try_from(msg.content.as_ref())?;
                match message.body {
                    MessageBody::Message { from, text } => {
                        let short_id = from.fmt_short().to_string();
                        let name = directory
                            .get(&from)
                            .map(|name| format!("[{name}:{short_id}]"))
                            .unwrap_or_else(|| format!("[{short_id}]"));
                        println!("<< {}: {}", name, text);
                    }
                    MessageBody::Intro { from, name } => {
                        let existing = directory.insert(from, name.clone());
                        if let Some(old_name) = existing {
                            if old_name != name {
                                println!(
                                    "<< User {} changed name from {} to {}",
                                    from, old_name, name
                                );
                            }
                        } else {
                            println!("<< User {} joined with name {}", from, name);
                        }
                    }
                }
            }
            Event::NeighborUp(id) => {
                println!("<< New user {} just joined", id.fmt_short());
                let intro = Message::new_intro(my_id, identity.clone());
                input_sender.send(intro).await?;
            }
            Event::NeighborDown(id) => {
                println!("<< User {} just left", id.fmt_short());
            }
            Event::Lagged => {
                println!("<< Lagged - some messages were lost");
            }
        }
    }
    Ok(())
}

fn input_loop(sender: tokio::sync::mpsc::Sender<Message>, id: EndpointId) -> Result<()> {
    let stdin = std::io::stdin();
    let mut buf = String::new();
    loop {
        // print!("\n>> ");
        stdin.read_line(&mut buf)?;
        let text = buf.trim_end().to_string();
        let msg = Message::new_message(id, text);
        sender.blocking_send(msg)?;
        buf.clear();
    }
}
