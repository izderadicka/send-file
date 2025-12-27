#![allow(dead_code)]

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::{Arc, RwLock},
    time::Duration,
};

use anyhow::Result;
use iroh::{
    Endpoint, EndpointAddr, PublicKey, RelayMode, SecretKey, discovery::mdns::MdnsDiscovery,
};
use iroh_blobs::store::{
    GcConfig,
    fs::{
        FsStore,
        options::{BatchOptions, InlineOptions, Options, PathOptions},
    },
};
use iroh_gossip::TopicId;
use tokio::fs;
use tracing::error;

use crate::{Args, channel::Message, command::Command};

#[derive(Clone)]
pub struct PeersDirectory {
    inner: Arc<RwLock<PeersDirectoryInner>>,
}

impl PeersDirectory {
    pub fn new() -> Self {
        PeersDirectory {
            inner: Arc::new(RwLock::new(PeersDirectoryInner::new())),
        }
    }

    pub fn add_peer(&self, public_key: PublicKey, name: String) -> Option<Arc<str>> {
        self.inner.write().unwrap().add_peer(public_key, name)
    }

    pub fn find_by_id(&self, public_key: &PublicKey) -> Option<Arc<str>> {
        let db = self.inner.read().unwrap();
        db.find_by_id(public_key)
    }

    pub fn find_by_name(&self, name: &str) -> Option<PublicKey> {
        self.inner.read().unwrap().find_by_name(name).cloned()
    }

    pub fn friendly_name(&self, id: &PublicKey) -> String {
        let short_id = id.fmt_short().to_string();
        let name = self
            .find_by_id(&id)
            .map(|name| format!("[{name}:{short_id}]"))
            .unwrap_or_else(|| format!("[{short_id}]"));
        name
    }
}

struct PeersDirectoryInner {
    peers: HashMap<PublicKey, Arc<str>>,
    names: HashMap<String, PublicKey>,
}

impl PeersDirectoryInner {
    fn new() -> Self {
        PeersDirectoryInner {
            peers: HashMap::new(),
            names: HashMap::new(),
        }
    }

    fn add_peer(&mut self, public_key: PublicKey, name: String) -> Option<Arc<str>> {
        let existing = self.peers.insert(public_key, Arc::from(name.as_str()));
        self.names.insert(name, public_key);
        existing
    }

    fn find_by_id(&self, public_key: &PublicKey) -> Option<Arc<str>> {
        self.peers.get(public_key).cloned()
    }

    fn find_by_name(&self, name: &str) -> Option<&PublicKey> {
        self.names.get(name)
    }
}

async fn init_store(data_dir: &Path, identity: &str) -> Result<FsStore> {
    let path = data_dir.join(format!("store-{}", identity));
    let options = Options {
        path: PathOptions::new(&path),
        inline: InlineOptions::default(),
        batch: BatchOptions::default(),
        gc: Some(GcConfig {
            add_protected: None,
            interval: Duration::from_secs(60),
        }),
    };
    let store = FsStore::load_with_opts(path.join("store.db"), options).await?;
    Ok(store)
}

async fn load_identity(data_dir: &Path, name: &str) -> Result<SecretKey> {
    let file = data_dir.join(name).with_extension("id");
    if fs::try_exists(&file).await? {
        let key_data = fs::read(file).await?;
        if key_data.len() != 32 {
            anyhow::bail!("Secret key must be 32 bytes");
        }
        let key_ref: &[u8; 32] = key_data[..].try_into()?;
        Ok(SecretKey::from_bytes(key_ref))
    } else {
        let key = SecretKey::generate(&mut rand::rng());
        fs::write(&file, key.to_bytes()).await?;
        Ok(key)
    }
}

async fn init_endpoint(args: &Args) -> Result<Endpoint> {
    let secret_key = load_identity(&args.data_dir, &args.identity).await?;

    let mut builder = if args.disable_relays {
        Endpoint::empty_builder(RelayMode::Disabled)
    } else {
        Endpoint::builder().relay_mode(RelayMode::Default)
    };

    builder = builder.secret_key(secret_key.clone());

    if !args.disable_mdns {
        let mdns = MdnsDiscovery::builder();
        builder = builder.discovery(mdns);
    }

    if args.enable_dht {
        let dht_discovery = iroh::discovery::pkarr::dht::DhtDiscovery::builder()
            .secret_key(secret_key.clone())
            .include_direct_addresses(args.disable_relays);
        builder = builder.discovery(dht_discovery);
    }

    let endpoint = builder.bind().await?;
    Ok(endpoint)
}

#[derive(Clone)]
pub struct Context {
    inner: Arc<ContextInner>,
}

impl Context {
    pub async fn new(
        args: &Args,
        topic_id: TopicId,
        topic_endpoints: Vec<EndpointAddr>,
        output_sender: tokio::sync::mpsc::Sender<String>,
        input_sender: tokio::sync::mpsc::Sender<Command>,
    ) -> Result<Self> {
        let peers = PeersDirectory::new();
        let store = init_store(&args.data_dir, &args.identity).await?;

        let endpoint = init_endpoint(&args).await?;

        let downloader = store.downloader(&endpoint);
        Ok(Context {
            inner: Arc::new(ContextInner {
                peers,
                store,
                downloader,
                endpoint,
                topic_id,
                topic_endpoints,
                identity: args.identity.clone(),
                data_dir: args.data_dir.clone(),
                output_sender,
                input_sender,
            }),
        })
    }

    pub fn peers(&self) -> &PeersDirectory {
        &self.inner.peers
    }

    pub fn store(&self) -> &FsStore {
        &self.inner.store
    }

    pub fn downloader(&self) -> &iroh_blobs::api::downloader::Downloader {
        &self.inner.downloader
    }

    pub fn endpoint(&self) -> &Endpoint {
        &self.inner.endpoint
    }

    pub fn topic_id(&self) -> &TopicId {
        &self.inner.topic_id
    }

    pub fn topic_endpoints(&self) -> &Vec<EndpointAddr> {
        &self.inner.topic_endpoints
    }

    pub fn identity(&self) -> &str {
        &self.inner.identity
    }

    pub fn data_dir(&self) -> &Path {
        self.inner.data_dir.as_path()
    }

    pub async fn print(&self, msg: String) {
        self.inner
            .output_sender
            .send(msg)
            .await
            .inspect_err(|_| error!("Cannot print"))
            .ok();
    }

    pub fn input_sender(&self) -> tokio::sync::mpsc::Sender<Command> {
        self.inner.input_sender.clone()
    }

    pub async fn send_command(&self, cmd: Command) {
        self.inner
            .input_sender
            .send(cmd)
            .await
            .inspect_err(|_| error!("Cannot send command"))
            .ok();
    }

    pub async fn send_message(&self, msg: Message) {
        self.send_command(Command::Message(msg)).await;
    }
}

struct ContextInner {
    peers: PeersDirectory,
    store: FsStore,
    downloader: iroh_blobs::api::downloader::Downloader,
    endpoint: Endpoint,
    topic_id: TopicId,
    topic_endpoints: Vec<EndpointAddr>,
    identity: String,
    data_dir: PathBuf,
    output_sender: tokio::sync::mpsc::Sender<String>,
    input_sender: tokio::sync::mpsc::Sender<Command>,
}
