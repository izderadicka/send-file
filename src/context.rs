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

use crate::Args;

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

    pub fn add_peer(&self, public_key: PublicKey, name: String) {
        self.inner.write().unwrap().add_peer(public_key, name);
    }

    pub fn find_by_id(&self, public_key: &PublicKey) -> Option<String> {
        self.inner
            .read()
            .unwrap()
            .find_by_id(public_key)
            .map(|s| s.to_string())
    }

    pub fn find_by_name(&self, name: &str) -> Option<PublicKey> {
        self.inner.read().unwrap().find_by_name(name).cloned()
    }
}

struct PeersDirectoryInner {
    peers: HashMap<PublicKey, String>,
    names: HashMap<String, PublicKey>,
}

impl PeersDirectoryInner {
    fn new() -> Self {
        PeersDirectoryInner {
            peers: HashMap::new(),
            names: HashMap::new(),
        }
    }

    fn add_peer(&mut self, public_key: PublicKey, name: String) {
        self.peers.insert(public_key, name.clone());
        self.names.insert(name, public_key);
    }

    fn find_by_id(&self, public_key: &PublicKey) -> Option<&str> {
        self.peers.get(public_key).map(|name| name.as_str())
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

async fn init_endpoint(data_dir: &Path, identity: &str) -> Result<Endpoint> {
    let secret_key = load_identity(data_dir, identity).await?;
    let mdns = MdnsDiscovery::builder();
    // let dht_discovery = iroh::discovery::pkarr::dht::DhtDiscovery::builder()
    //     .secret_key(secret_key.clone())
    //     .include_direct_addresses(true);
    let builder = Endpoint::empty_builder(RelayMode::Disabled)
        .secret_key(secret_key)
        // .discovery(dht_discovery);
        .discovery(mdns);
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
    ) -> Result<Self> {
        let peers = PeersDirectory::new();
        let store = init_store(&args.data_dir, &args.identity).await?;

        let endpoint = init_endpoint(&args.data_dir, &args.identity).await?;

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
}
