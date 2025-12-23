use std::path::{self, Path, PathBuf};

use anyhow::Result;
use clap::{Parser, Subcommand};
use iroh::{Endpoint, RelayMode, SecretKey, discovery::mdns::MdnsDiscovery, protocol::Router};
use iroh_blobs::{BlobsProtocol, store::mem::MemStore, ticket::BlobTicket};
use tokio::{fs, io::AsyncWriteExt as _};

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
            let router = Router::builder(endpoint)
                .accept(iroh_blobs::ALPN, blobs)
                .spawn();
            tokio::signal::ctrl_c().await?;
            router.shutdown().await?;
        }
        Command::Receive { ticket, file } => {
            let ticket: BlobTicket = ticket.parse()?;
            let output_file = path::absolute(&file)?;
            let downloader = store.downloader(&endpoint);
            downloader
                .download(ticket.hash(), Some(ticket.addr().id))
                .await?;
            store.blobs().export(ticket.hash(), output_file).await?;
            endpoint.close().await;
        }
    }

    Ok(())
}
