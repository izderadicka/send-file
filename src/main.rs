use std::path::{self, PathBuf};

use anyhow::Result;
use clap::{Parser, Subcommand};
use iroh::{Endpoint, RelayMode, SecretKey, discovery::mdns::MdnsDiscovery, protocol::Router};
use iroh_blobs::{BlobsProtocol, store::mem::MemStore, ticket::BlobTicket};
use tokio::io::AsyncWriteExt as _;

#[derive(Parser, Debug)]
struct Args {
    #[command(subcommand)]
    command: Command
    
}

#[derive(Subcommand, Debug)]
enum Command {
    Send{
        #[arg(required = true)]
        file: PathBuf
    },
    Receive {
        #[arg(required = true)]
        ticket: String,
        #[arg(required = true)]
        file: PathBuf
    }
}

#[tokio::main]
async fn main() -> Result<()>{
    tracing_subscriber::fmt::init();
    let args = Args::parse();
    let mdns = MdnsDiscovery::builder();
    let secret_key = SecretKey::generate(&mut rand::rng());
    let dht_discovery = iroh::discovery::pkarr::dht::DhtDiscovery::builder().secret_key(secret_key.clone());
    let builder = Endpoint::empty_builder(RelayMode::Disabled)
    .secret_key(secret_key)
    // .discovery(dht_discovery);
    .discovery(mdns);
    let endpoint = builder.bind().await?;

    let store = MemStore::new();

    let blobs = BlobsProtocol::new(&store, None);

    match args.command {
        Command::Send { file } => {
            let tag = store.add_path(path::absolute(&file)?).await?;
            let ticket = BlobTicket::new(endpoint.id().into(), tag.hash, tag.format);
            let ticket = ticket.to_string();
            let file = tokio::fs::File::create("./test-data/file_ticket.txt").await?;
            let mut file = tokio::io::BufWriter::new(file);
            file.write_all(ticket.as_bytes()).await?;
            file.flush().await?;

            println!("Ticket: \n{}", ticket);
            let router = Router::builder(endpoint).accept(iroh_blobs::ALPN,blobs).spawn();
            tokio::signal::ctrl_c().await?;
            router.shutdown().await?;

        }
        Command::Receive { ticket, file } => {

            let ticket: BlobTicket = ticket.parse()?;
            let output_file = path::absolute(&file)?;
            let downloader = store.downloader(&endpoint);
            downloader.download(ticket.hash(), Some(ticket.addr().id)).await?;
            store.blobs().export(ticket.hash(), output_file).await?;
            endpoint.close().await;

        }
    }

    

    Ok(())
}
