use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use anyhow::{Context as _, Result};
use clap::{Parser, Subcommand};
use futures_lite::StreamExt as _;
use iroh::{EndpointAddr, protocol::Router};
use iroh_blobs::BlobsProtocol;
use iroh_gossip::{
    Gossip, TopicId,
    api::{Event, GossipReceiver},
};
use tokio::fs;
use tracing::error;

use crate::{
    channel::{Message, MessageBody, MessageEnvelope, Ticket},
    command::Command,
    context::Context,
};

mod channel;
mod command;
mod context;

#[derive(Parser, Debug)]
struct Args {
    #[arg(long, short, help = "Data directory", default_value = "./test-data")]
    data_dir: PathBuf,
    #[arg(long, short, alias = "name", help = "User name", required = true)]
    identity: String,
    #[arg(long, short, help = "Enforce creation of new topic", action = clap::ArgAction::SetTrue)]
    new_topic: bool,
    #[command(subcommand)]
    command: CliCommand,
}

#[derive(Subcommand, Debug)]
enum CliCommand {
    Start,
    Join { ticket: String },
}

async fn load_topic(data_dir: &Path, new_topic: bool) -> Result<TopicId> {
    let file = data_dir.join("topic").with_extension("bin");
    if fs::try_exists(&file).await? && !new_topic {
        let topic_data = fs::read(file).await?;
        let topic_arr: [u8; 32] = topic_data[..].try_into()?;
        Ok(TopicId::from_bytes(topic_arr))
    } else {
        let topic = TopicId::from_bytes(rand::random());
        fs::write(&file, topic.as_bytes()).await?;
        Ok(topic)
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();
    let args = Args::parse();

    let (topic, endpoints) = match args.command {
        CliCommand::Start => {
            let topic = load_topic(&args.data_dir, args.new_topic).await?;
            let endpoints: Vec<EndpointAddr> = vec![];

            (topic, endpoints)
        }
        CliCommand::Join { ref ticket } => {
            let ticket: Ticket = ticket.parse().context("Invalid ticket")?;
            (ticket.topic, ticket.endpoints)
        }
    };

    let context = Context::new(&args, topic, endpoints).await?;

    run(context).await?;
    Ok(())
}

//TODO - extract to funcions
// CliCommand::Send { file } => {
//             let tag = store.add_path(path::absolute(&file)?).await?;
//             let ticket = BlobTicket::new(endpoint.id().into(), tag.hash, tag.format);
//             let ticket = ticket.to_string();
//             let file = tokio::fs::File::create("./test-data/file_ticket.txt").await?;
//             let mut file = tokio::io::BufWriter::new(file);
//             file.write_all(ticket.as_bytes()).await?;
//             file.flush().await?;

//             println!("Ticket: \n{}", ticket);
//             let router = Router::builder(endpoint).spawn();
//             tokio::signal::ctrl_c().await?;
//             router.shutdown().await?;
//         }
//         CliCommand::Receive { ticket, file } => {
//             let store = MemStore::new();
//             let ticket: BlobTicket = ticket.parse()?;
//             let output_file = path::absolute(&file)?;
//             let downloader = store.downloader(&endpoint);
//             downloader
//                 .download(ticket.hash(), Some(ticket.addr().id))
//                 .await?;
//             store.blobs().export(ticket.hash(), output_file).await?;
//             endpoint.close().await;
//         }

async fn run(context: Context) -> Result<()> {
    let endpoint = context.endpoint();

    // Blobs config

    let blobs = BlobsProtocol::new(context.store(), None);

    // Gossip config
    let gossip = Gossip::builder().spawn(endpoint.clone());
    let router = Router::builder(endpoint.clone())
        .accept(iroh_gossip::ALPN, gossip.clone())
        .accept(iroh_blobs::ALPN, blobs)
        .spawn();

    let (sender, receiver) = start_chat(&context, gossip).await?;
    let (input_sender, mut input_receiver) = tokio::sync::mpsc::channel::<Command>(1);

    tokio::spawn(message_loop(
        receiver,
        input_sender.clone(),
        context.clone(),
    ));

    std::thread::spawn(move || input_loop(input_sender));

    while let Some(cmd) = input_receiver.recv().await {
        match cmd {
            Command::Share { file } => todo!(),
            Command::Download {
                ticket,
                output_file,
            } => todo!(),
            Command::Message(message) => {
                let data: Vec<u8> = message.sign_and_encode(endpoint.secret_key())?;
                sender.broadcast(data.into()).await?;
            }
            Command::Quit => {
                println!("!! Goodbye");
                break;
            }
        }
    }
    router.shutdown().await?;
    Ok(())
}

async fn start_chat(
    context: &Context,
    gossip: Gossip,
) -> Result<(iroh_gossip::api::GossipSender, GossipReceiver), anyhow::Error> {
    let topic = *context.topic_id();
    let topic_endpoints = context.topic_endpoints();
    let endpoint = context.endpoint();
    let ticket = Ticket {
        topic: topic,
        endpoints: vec![endpoint.id().into()],
    };
    let ticket = ticket.to_string();
    println!("Ticket: \n{}", ticket);
    fs::write(context.data_dir().join("chat_ticket.txt"), ticket).await?;
    if topic_endpoints.is_empty() {
        println!("Waiting somebody joins our channel")
    } else {
        println!("Waiting to join channel")
    }
    let endpoint_ids: Vec<_> = topic_endpoints.iter().map(|ep| ep.id).collect();
    let (sender, receiver) = gossip
        .subscribe_and_join(topic, endpoint_ids)
        .await?
        .split();
    println!("Connected - start chat below");
    println!("------------------------------------");
    let data: Vec<u8> =
        Message::new_intro(context.identity().into()).sign_and_encode(endpoint.secret_key())?;
    sender.broadcast(data.into()).await?;
    Ok((sender, receiver))
}

async fn message_loop(
    mut receiver: GossipReceiver,
    input_sender: tokio::sync::mpsc::Sender<Command>,
    context: Context,
) -> Result<()> {
    let mut directory: HashMap<_, String> = HashMap::new();
    while let Some(event) = receiver.next().await {
        let event = event?;
        match event {
            Event::Received(msg) => {
                let (from, message) = match MessageEnvelope::decode_and_verify(&msg.content) {
                    Ok(res) => res,
                    Err(e) => {
                        error!("Failed to decode message: {}", e);
                        continue;
                    }
                };
                match message.body {
                    MessageBody::Message { text } => {
                        let short_id = from.fmt_short().to_string();
                        let name = directory
                            .get(&from)
                            .map(|name| format!("[{name}:{short_id}]"))
                            .unwrap_or_else(|| format!("[{short_id}]"));
                        println!("<< {}: {}", name, text);
                    }
                    MessageBody::Intro { name } => {
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
                let intro = Message::new_intro(context.identity().into());
                let intro = Command::Message(intro);
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

fn input_loop(sender: tokio::sync::mpsc::Sender<Command>) -> Result<()> {
    let stdin = std::io::stdin();
    let mut buf = String::new();
    loop {
        // print!("\n>> ");
        stdin.read_line(&mut buf)?;
        let text = buf.trim_end().to_string();
        if !text.is_empty() {
            let parsed: Result<Command, _> = text.parse();
            match parsed {
                Ok(cmd) => sender.blocking_send(cmd)?,
                Err(e) => println!("!! Invalid command {text}, error: {e}"),
            }
        }
        buf.clear();
    }
}
