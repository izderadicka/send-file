use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, anyhow};
use clap::{Parser, Subcommand};
use futures_lite::StreamExt as _;
use iroh::{EndpointAddr, protocol::Router};
use iroh_blobs::{BlobsProtocol, ticket::BlobTicket};
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
    #[arg(long, help = "Disable mdns (local LAN) discovery", action = clap::ArgAction::SetTrue)]
    disable_mdns: bool,
    #[arg(long, help = "Enable DHT discovery (your record will be published in BitTorrent Mainline DHT)", action = clap::ArgAction::SetTrue)]
    enable_dht: bool,
    #[arg(long, help = "Disable public (number-0) relays", action = clap::ArgAction::SetTrue)]
    disable_relays: bool,
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

macro_rules! output {
    ($context:ident, $($arg:tt)*) => {
        {
        let msg = format!($($arg)*);
        $context.print(msg).await;
        }

    };
}

fn init_logging() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();
}

#[tokio::main]
async fn main() -> Result<()> {
    init_logging();
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
    let (output_sender, output_receiver) = tokio::sync::mpsc::channel(8);
    let (input_sender, input_receiver) = tokio::sync::mpsc::channel::<Command>(1);
    let context = Context::new(&args, topic, endpoints, output_sender, input_sender).await?;

    run(context, output_receiver, input_receiver).await?;
    Ok(())
}

async fn share_file(context: Context, file: &str) -> Result<()> {
    let tag = context
        .store()
        .add_path(std::path::absolute(&file)?)
        .await?;
    let ticket = BlobTicket::new(context.endpoint().id().into(), tag.hash, tag.format);
    let ticket = ticket.to_string();
    output!(context, "!! Ticket for {}: {}", file, ticket);
    let file_name = Path::new(file)
        .file_name()
        .ok_or_else(|| anyhow!("Wrong file name"))?
        .to_str()
        .ok_or_else(|| anyhow!("File name is not UTF-8"))?;
    let msg = Message::new_message(format!("Sharing {file_name} with ticket {ticket}"));
    context.send_message(msg).await;

    Ok(())
}

async fn download_ticket(context: Context, ticket: &str, output_file: Option<&str>) -> Result<()> {
    let ticket: BlobTicket = ticket.parse()?;
    let output_file = output_file.map(PathBuf::from).unwrap_or_else(|| {
        context
            .data_dir()
            .join("downloads")
            .join(ticket.hash().to_string())
    });
    let output_file = std::path::absolute(output_file)?;

    let downloader = context.downloader();
    downloader
        .download(ticket.hash(), Some(ticket.addr().id))
        .await?;
    context
        .store()
        .blobs()
        .export(ticket.hash(), &output_file)
        .await?;
    output!(context, "!! Downloaded to {}", output_file.display());
    Ok(())
}

async fn run(
    context: Context,
    output_sync: tokio::sync::mpsc::Receiver<String>,
    mut input_receiver: tokio::sync::mpsc::Receiver<Command>,
) -> Result<()> {
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

    tokio::spawn(message_loop(receiver, context.clone()));

    let mut line_reader = rustyline::DefaultEditor::new()?;
    let printer: Box<dyn rustyline::ExternalPrinter + Send> =
        Box::new(line_reader.create_external_printer()?);

    std::thread::spawn(move || output_loop(output_sync, printer));
    let input_sender = context.input_sender();
    std::thread::spawn(move || input_loop(input_sender, line_reader));

    while let Some(cmd) = input_receiver.recv().await {
        match cmd {
            Command::Share { file } => {
                let context = context.clone();
                tokio::spawn(async move {
                    match share_file(context.clone(), &file).await {
                        Ok(_) => output!(context, "!! File {file} was shared"),
                        Err(e) => output!(context, "!! Error sharing file: {e}"),
                    }
                });
            }
            Command::Download {
                ticket,
                output_file,
            } => {
                let context = context.clone();
                tokio::spawn(async move {
                    match download_ticket(context.clone(), &ticket, output_file.as_deref()).await {
                        Ok(_) => (),
                        Err(e) => output!(context, "!! Error downloading file: {e}"),
                    }
                });
            }
            Command::Message(message) => {
                let data: Vec<u8> = message.sign_and_encode(endpoint.secret_key())?;
                sender.broadcast(data.into()).await?;
            }
            Command::Quit => {
                output!(context, "!! Goodbye\n");
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

async fn message_loop(mut receiver: GossipReceiver, context: Context) -> Result<()> {
    let directory = context.peers();
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
                        let name = directory.friendly_name(&from);
                        output!(context, "<< {}: {}", name, text);
                    }
                    MessageBody::Intro { name } => {
                        let existing = directory.add_peer(from, name.clone());
                        let short_id = from.fmt_short();
                        if let Some(old_name) = existing {
                            if *old_name != name {
                                output!(
                                    context,
                                    "<< User {} changed name from {} to {}",
                                    short_id,
                                    old_name,
                                    name
                                );
                            }
                        } else {
                            output!(context, "<< User {} joined with name {}", short_id, name);
                        }
                    }
                }
            }
            Event::NeighborUp(id) => {
                output!(context, "<< New user {} just joined", id.fmt_short());
                let intro = Message::new_intro(context.identity().into());
                context.send_message(intro).await;
            }
            Event::NeighborDown(id) => {
                let name = directory.friendly_name(&id);
                output!(context, "<< User {} just left", name);
            }
            Event::Lagged => {
                output!(context, "<< Lagged - some messages were lost");
            }
        }
    }
    Ok(())
}

fn output_loop(
    mut output_receiver: tokio::sync::mpsc::Receiver<String>,
    mut printer: Box<dyn rustyline::ExternalPrinter>,
) {
    while let Some(msg) = output_receiver.blocking_recv() {
        printer
            .print(msg)
            .inspect_err(|e| error!("Output error: {e}"))
            .ok();
    }
}

fn input_loop(
    sender: tokio::sync::mpsc::Sender<Command>,
    mut rl: rustyline::DefaultEditor,
) -> Result<()> {
    use rustyline::error::ReadlineError;

    loop {
        let readline = rl.readline(">> ");
        match readline {
            Ok(line) => {
                rl.add_history_entry(line.as_str())?;
                let text = line.trim_end().to_string();
                if !text.is_empty() {
                    let parsed: Result<Command, _> = text.parse();
                    match parsed {
                        Ok(cmd) => {
                            let is_end = matches!(cmd, Command::Quit);
                            sender.blocking_send(cmd)?;
                            if is_end {
                                break;
                            }
                        }
                        Err(e) => println!("!! Invalid command {text}, error: {e}"),
                    }
                }
            }
            Err(ReadlineError::Interrupted) => {
                println!("!! CTRL-C");
                sender.blocking_send(Command::Quit)?;
                break;
            }
            Err(ReadlineError::Eof) => {
                println!("CTRL-D");
                sender.blocking_send(Command::Quit)?;
                break;
            }
            Err(err) => {
                println!("!! Input error: {:?}", err);
                sender.blocking_send(Command::Quit)?;
                break;
            }
        }
    }
    Ok(())
}
