//! Peer-to-peer workspace replication over Iroh Documents.
//!
//! One document per workspace. Every grid, card, folder, calendar, date link,
//! template, settings view and attachment is a record under a stable key, so a
//! change publishes only what changed. Records carry a Lamport clock and the
//! device id; the highest `(clock, device)` wins. Deletions are tombstones, so
//! a stale peer cannot resurrect an object. Losing values are kept in
//! `backups/`.
//!
//! The GUI owns the live `Workspace`; this module only sees snapshots and
//! hands applied records back through a callback. The network runtime and the
//! Iroh endpoint live in a dedicated thread so the Qt thread never blocks.

use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::model::{CalendarDef, Card, DateLink, Folder, Grid, Settings, Template, Workspace};
use crate::storage;

const SCHEMA: u32 = 1;
const STATE_FILE: &str = "sync-state.json";
const ENDPOINT_KEY_FILE: &str = "endpoint.key";
const DEBOUNCE: Duration = Duration::from_secs(2);
const SETUP_PREFIX: &str = "corcho1:";
const SETUP_PREFIX_V2: &str = "corcho2:";
const BUILD_SHA: &str = env!("CORCHO_BUILD_SHA");
const KEY_LEN: usize = 32;

pub type SnapshotFn = Arc<dyn Fn() -> Option<Workspace> + Send + Sync>;
pub type ApplyFn = Arc<dyn Fn(Vec<(String, Record)>) -> Option<bool> + Send + Sync>;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Record {
    #[serde(default = "default_schema")]
    pub v: u32,
    pub device: String,
    pub clock: u64,
    #[serde(default)]
    pub deleted: bool,
    #[serde(default)]
    pub value: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sealed: Option<Sealed>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Sealed {
    pub n: String,
    pub c: String,
}

fn default_schema() -> u32 {
    SCHEMA
}

impl Record {
    fn newer_than(&self, clock: u64, device: &str) -> bool {
        (self.clock, self.device.as_str()) > (clock, device)
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Applied {
    clock: u64,
    device: String,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct LocalState {
    #[serde(default)]
    device: String,
    #[serde(default)]
    clock: u64,
    #[serde(default)]
    write_ticket: Option<String>,
    #[serde(default)]
    read_ticket: Option<String>,
    #[serde(default)]
    applied: HashMap<String, Applied>,
    #[serde(default)]
    last_published: HashMap<String, String>,
    #[serde(default)]
    last_sync: Option<String>,
    #[serde(default)]
    attachments: HashMap<String, String>,
    #[serde(default)]
    attachment_plain: HashMap<String, String>,
    #[serde(default)]
    key: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IrohConfig {
    pub enabled: bool,
    pub include_attachments: bool,
}

impl Default for IrohConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            include_attachments: true,
        }
    }
}

#[derive(Clone, Debug, Default)]
struct Status {
    ready: bool,
    configured: bool,
    namespace: Option<String>,
    write_ticket: bool,
    read_ticket: bool,
    device: Option<String>,
    endpoint_id: Option<String>,
    relay: Option<String>,
    clock: u64,
    peers: usize,
    last_sync: Option<String>,
    last_error: Option<String>,
    pending: bool,
}

struct Inner {
    tx: Mutex<Option<Sender<Command>>>,
    status: Mutex<Status>,
    config: Mutex<IrohConfig>,
    snapshot: Mutex<Option<SnapshotFn>>,
    apply: Mutex<Option<ApplyFn>>,
    state: Mutex<LocalState>,
    dirty_since: Mutex<Option<Instant>>,
    pull_requested: AtomicBool,
    peers: AtomicUsize,
    stop: AtomicBool,
    data_dir: PathBuf,
}

enum Command {
    Wake,
    Enable(bool),
    Create {
        reply: Sender<Result<String>>,
    },
    Join {
        ticket: String,
        reply: Sender<Result<String>>,
    },
    Share {
        read_only: bool,
        include_key: bool,
        reply: Sender<Result<String>>,
    },
    SyncNow {
        reply: Sender<Result<String>>,
    },
    Relay {
        reply: Sender<Result<String>>,
    },
    Leave {
        reply: Sender<Result<String>>,
    },
    Shutdown,
}

#[derive(Clone)]
pub struct IrohSync {
    inner: Arc<Inner>,
}

impl IrohSync {
    /// A handle with no networking, used by headless MCP clients and tests.
    pub fn disabled() -> Self {
        Self {
            inner: Arc::new(Inner {
                tx: Mutex::new(None),
                status: Mutex::new(Status::default()),
                config: Mutex::new(IrohConfig::default()),
                snapshot: Mutex::new(None),
                apply: Mutex::new(None),
                state: Mutex::new(LocalState::default()),
                dirty_since: Mutex::new(None),
                pull_requested: AtomicBool::new(false),
                peers: AtomicUsize::new(0),
                stop: AtomicBool::new(false),
                data_dir: storage::default_data_dir(),
            }),
        }
    }

    /// Start the Iroh endpoint and the document worker for this data dir.
    pub fn start(data_dir: PathBuf, config: IrohConfig) -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        let inner = Arc::new(Inner {
            tx: Mutex::new(Some(tx)),
            status: Mutex::new(Status::default()),
            config: Mutex::new(config),
            snapshot: Mutex::new(None),
            apply: Mutex::new(None),
            state: Mutex::new(load_local_state(&data_dir)),
            dirty_since: Mutex::new(None),
            pull_requested: AtomicBool::new(false),
            peers: AtomicUsize::new(0),
            stop: AtomicBool::new(false),
            data_dir,
        });
        let worker_inner = inner.clone();
        let _ = std::thread::Builder::new()
            .name("corcho-iroh".into())
            .spawn(move || worker(worker_inner, rx));
        Self { inner }
    }

    pub fn attach(&self, snapshot: SnapshotFn, apply: ApplyFn) {
        *self.inner.snapshot.lock().unwrap() = Some(snapshot);
        *self.inner.apply.lock().unwrap() = Some(apply);
        self.wake();
    }

    pub fn configure(&self, config: IrohConfig) {
        let enabled = config.enabled;
        *self.inner.config.lock().unwrap() = config;
        let _ = self.send(Command::Enable(enabled));
    }

    pub fn enabled(&self) -> bool {
        self.inner.config.lock().map(|c| c.enabled).unwrap_or(false)
    }

    pub fn mark_dirty(&self) {
        if !self.enabled() {
            return;
        }
        *self.inner.dirty_since.lock().unwrap() = Some(Instant::now());
        self.wake();
    }

    pub fn create_workspace(&self) -> Result<String> {
        let (reply, rx) = std::sync::mpsc::channel();
        self.send(Command::Create { reply })?;
        rx.recv_timeout(Duration::from_secs(10))
            .map_err(|_| anyhow!("timed out creating the Iroh workspace"))?
    }

    pub fn join(&self, ticket: &str) -> Result<String> {
        let (reply, rx) = std::sync::mpsc::channel();
        self.send(Command::Join {
            ticket: ticket.to_string(),
            reply,
        })?;
        rx.recv_timeout(Duration::from_secs(20))
            .map_err(|_| anyhow!("timed out joining the Iroh workspace"))?
    }

    pub fn share(&self, read_only: bool) -> Result<String> {
        let (reply, rx) = std::sync::mpsc::channel();
        self.send(Command::Share {
            read_only,
            include_key: true,
            reply,
        })?;
        rx.recv_timeout(Duration::from_secs(10))
            .map_err(|_| anyhow!("timed out creating the share ticket"))?
    }

    pub fn cache_ticket(&self) -> Result<String> {
        let (reply, rx) = std::sync::mpsc::channel();
        self.send(Command::Share {
            read_only: true,
            include_key: false,
            reply,
        })?;
        rx.recv_timeout(Duration::from_secs(10))
            .map_err(|_| anyhow!("timed out creating the cache ticket"))?
    }

    pub fn sync_now(&self) -> Result<String> {
        let (reply, rx) = std::sync::mpsc::channel();
        self.send(Command::SyncNow { reply })?;
        rx.recv_timeout(Duration::from_secs(60))
            .map_err(|_| anyhow!("timed out syncing the Iroh workspace"))?
    }

    pub fn reload_relay(&self) -> Result<String> {
        let (reply, rx) = std::sync::mpsc::channel();
        self.send(Command::Relay { reply })?;
        rx.recv_timeout(Duration::from_secs(15))
            .map_err(|_| anyhow!("timed out reloading the Iroh relay"))?
    }

    pub fn leave(&self) -> Result<String> {
        let (reply, rx) = std::sync::mpsc::channel();
        self.send(Command::Leave { reply })?;
        rx.recv_timeout(Duration::from_secs(10))
            .map_err(|_| anyhow!("timed out leaving the Iroh workspace"))?
    }

    pub fn status_json(&self) -> String {
        let config = self
            .inner
            .config
            .lock()
            .map(|c| c.clone())
            .unwrap_or_default();
        let mut status = self
            .inner
            .status
            .lock()
            .map(|s| s.clone())
            .unwrap_or_default();
        status.peers = self.inner.peers.load(Ordering::Relaxed);
        status.clock = self
            .inner
            .state
            .lock()
            .map(|s| s.clock)
            .unwrap_or(status.clock);
        status.pending = self
            .inner
            .dirty_since
            .lock()
            .map(|d| d.is_some())
            .unwrap_or(false);
        let state = self.inner.state.lock().ok();
        let encrypted = state.as_ref().map(|s| s.key.is_some()).unwrap_or(false);
        json!({
            "enabled": config.enabled,
            "includeAttachments": config.include_attachments,
            "ready": status.ready,
            "configured": status.configured,
            "namespace": status.namespace,
            "writeTicket": status.write_ticket,
            "readTicket": status.read_ticket,
            "device": status.device,
            "endpointId": status.endpoint_id,
            "relay": status.relay,
            "encrypted": encrypted,
            "fingerprint": sync_fingerprint(),
            "clock": status.clock,
            "peers": status.peers,
            "lastSync": status.last_sync,
            "lastError": status.last_error,
            "pending": status.pending,
            "savedWorkspace": state
                .as_ref()
                .map(|s| s.write_ticket.is_some() || s.read_ticket.is_some())
                .unwrap_or(false),
            "remoteAttachments": state.as_ref().map(|s| s.attachments.len()).unwrap_or(0),
        })
        .to_string()
    }

    pub fn shutdown(&self) {
        self.inner.stop.store(true, Ordering::Relaxed);
        let _ = self.send(Command::Shutdown);
    }

    fn send(&self, command: Command) -> Result<()> {
        let tx = self
            .inner
            .tx
            .lock()
            .map_err(|_| anyhow!("sync lock"))?
            .clone()
            .ok_or_else(|| anyhow!("Iroh sync is not running"))?;
        tx.send(command).map_err(|_| anyhow!("Iroh sync stopped"))
    }

    fn wake(&self) {
        let _ = self.send(Command::Wake);
    }
}

// ---------------------------------------------------------------------------
// Worker
// ---------------------------------------------------------------------------

struct Network {
    endpoint: iroh::Endpoint,
    router: iroh::protocol::Router,
    blobs: iroh_blobs::store::fs::FsStore,
    docs: iroh_docs::protocol::Docs,
    author: iroh_docs::AuthorId,
}

fn worker(inner: Arc<Inner>, rx: Receiver<Command>) {
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(2)
        .build()
    {
        Ok(rt) => rt,
        Err(err) => {
            set_error(&inner, format!("cannot start sync runtime: {err}"));
            return;
        }
    };

    let mut network = match runtime.block_on(start_network(&inner)) {
        Ok(net) => net,
        Err(err) => {
            set_error(&inner, format!("Iroh unavailable: {err}"));
            {
                let mut status = inner.status.lock().unwrap();
                status.ready = false;
            }
            // Keep serving commands so the UI can report the error and retry
            // after e.g. a network change.
            for command in rx.iter() {
                match command {
                    Command::Shutdown => break,
                    Command::Wake => {}
                    Command::Create { reply }
                    | Command::Join { reply, .. }
                    | Command::Share { reply, .. }
                    | Command::SyncNow { reply }
                    | Command::Relay { reply }
                    | Command::Leave { reply } => {
                        let _ = reply.send(Err(anyhow!(
                            "Iroh is not available: {}",
                            status_error(&inner)
                        )));
                    }
                    Command::Enable(_) => {}
                }
            }
            return;
        }
    };

    {
        let mut status = inner.status.lock().unwrap();
        status.ready = true;
        status.device = Some(network.endpoint.id().to_string());
        status.last_error = None;
    }
    {
        let mut state = inner.state.lock().unwrap();
        if state.device.is_empty() {
            state.device = network.endpoint.id().to_string();
            save_state(&inner, &state);
        }
    }

    let enabled = inner.config.lock().map(|c| c.enabled).unwrap_or(false);
    let mut doc = if enabled {
        match runtime.block_on(open_document(&inner, &network)) {
            Ok(doc) => doc,
            Err(err) => {
                set_error(&inner, format!("cannot open Iroh workspace: {err}"));
                None
            }
        }
    } else {
        None
    };
    if doc.is_some() {
        start_subscription(
            inner.clone(),
            doc.clone().unwrap(),
            runtime.handle().clone(),
        );
    }

    loop {
        match rx.recv_timeout(Duration::from_millis(300)) {
            Ok(Command::Wake) => {}
            Ok(Command::Shutdown) => break,
            Ok(Command::Enable(enabled)) => {
                if enabled && doc.is_none() {
                    if let Ok(Some(opened)) = runtime.block_on(open_document(&inner, &network)) {
                        doc = Some(opened.clone());
                        start_subscription(inner.clone(), opened.clone(), runtime.handle().clone());
                        inner.pull_requested.store(true, Ordering::Relaxed);
                    }
                }
                let mut status = inner.status.lock().unwrap();
                status.configured = enabled && status.write_ticket;
            }
            Ok(Command::Create { reply }) => {
                let result = runtime.block_on(create_workspace(&inner, &network));
                if let Ok((ticket, new_doc)) = &result {
                    doc = Some(new_doc.clone());
                    start_subscription(inner.clone(), new_doc.clone(), runtime.handle().clone());
                    *inner.dirty_since.lock().unwrap() = Some(Instant::now() - DEBOUNCE);
                    let _ = reply.send(Ok(ticket.clone()));
                } else if let Err(err) = result {
                    let _ = reply.send(Err(err));
                }
            }
            Ok(Command::Join { ticket, reply }) => {
                let result = runtime.block_on(join_workspace(&inner, &network, &ticket));
                match result {
                    Ok(joined) => {
                        doc = Some(joined.clone());
                        start_subscription(inner.clone(), joined.clone(), runtime.handle().clone());
                        inner.pull_requested.store(true, Ordering::Relaxed);
                        let _ = reply.send(Ok(namespace_id(&inner)));
                    }
                    Err(err) => {
                        let _ = reply.send(Err(err));
                    }
                }
            }
            Ok(Command::Share {
                read_only,
                include_key,
                reply,
            }) => {
                let result = if let Some(current) = doc.clone() {
                    runtime
                        .block_on(share_document(&inner, &current, read_only, include_key))
                        .map_err(|e| anyhow!("{e}"))
                } else {
                    match runtime.block_on(open_document(&inner, &network)) {
                        Ok(Some(opened)) => {
                            doc = Some(opened.clone());
                            start_subscription(
                                inner.clone(),
                                opened.clone(),
                                runtime.handle().clone(),
                            );
                            runtime
                                .block_on(share_document(&inner, &opened, read_only, include_key))
                                .map_err(|e| anyhow!("{e}"))
                        }
                        Ok(None) => Err(anyhow!("no Iroh workspace configured")),
                        Err(err) => Err(err),
                    }
                };
                let _ = reply.send(result);
            }
            Ok(Command::SyncNow { reply }) => {
                let result = if doc.is_some() {
                    *inner.dirty_since.lock().unwrap() = Some(Instant::now() - DEBOUNCE);
                    inner.pull_requested.store(true, Ordering::Relaxed);
                    Ok(namespace_id(&inner))
                } else {
                    Err(anyhow!("no Iroh workspace configured"))
                };
                let _ = reply.send(result);
            }
            Ok(Command::Relay { reply }) => {
                if let Some(doc) = &doc {
                    let _ = runtime.block_on(doc.close());
                }
                doc = None;
                runtime.block_on(async {
                    let _ = network.router.shutdown().await;
                    let _ = network.blobs.shutdown().await;
                    network.endpoint.close().await;
                });
                match runtime.block_on(start_network(&inner)) {
                    Ok(fresh) => {
                        network = fresh;
                        if inner.config.lock().map(|c| c.enabled).unwrap_or(false) {
                            match runtime.block_on(open_document(&inner, &network)) {
                                Ok(Some(opened)) => {
                                    doc = Some(opened.clone());
                                    start_subscription(
                                        inner.clone(),
                                        opened,
                                        runtime.handle().clone(),
                                    );
                                    inner.pull_requested.store(true, Ordering::Relaxed);
                                }
                                Ok(None) => {}
                                Err(err) => {
                                    set_error(&inner, format!("cannot open Iroh workspace: {err}"))
                                }
                            }
                        }
                        let _ = reply.send(Ok("reloaded".into()));
                    }
                    Err(err) => {
                        set_error(&inner, format!("Iroh unavailable: {err}"));
                        let _ = reply.send(Err(err));
                    }
                }
            }
            Ok(Command::Leave { reply }) => {
                if let Some(doc) = &doc {
                    let _ = runtime.block_on(doc.leave());
                    let _ = runtime.block_on(doc.close());
                }
                doc = None;
                {
                    let mut state = inner.state.lock().unwrap();
                    state.write_ticket = None;
                    state.read_ticket = None;
                    state.applied.clear();
                    state.last_published.clear();
                    save_state(&inner, &state);
                }
                inner.peers.store(0, Ordering::Relaxed);
                {
                    let mut status = inner.status.lock().unwrap();
                    status.configured = false;
                    status.namespace = None;
                    status.write_ticket = false;
                    status.read_ticket = false;
                    status.peers = 0;
                }
                let _ = reply.send(Ok("left".into()));
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }

        if inner.stop.load(Ordering::Relaxed) {
            break;
        }

        if inner.pull_requested.load(Ordering::Relaxed)
            && inner.apply.lock().map(|a| a.is_some()).unwrap_or(false)
        {
            if let Some(doc) = &doc {
                let done = runtime.block_on(async { pull_and_apply(&inner, &network, doc).await });
                if done {
                    inner.pull_requested.store(false, Ordering::Relaxed);
                }
            }
        }

        let publish_due = inner
            .dirty_since
            .lock()
            .map(|d| d.map(|t| t.elapsed() >= DEBOUNCE).unwrap_or(false))
            .unwrap_or(false);
        if publish_due {
            if let Some(doc) = &doc {
                if runtime.block_on(async { publish(&inner, &network, doc).await }) {
                    *inner.dirty_since.lock().unwrap() = None;
                }
            }
        }
    }

    if let Some(doc) = doc {
        let _ = runtime.block_on(doc.close());
    }
    runtime.block_on(async {
        let _ = network.router.shutdown().await;
        let _ = network.blobs.shutdown().await;
        network.endpoint.close().await;
    });
}

fn relay_file_path() -> Result<PathBuf> {
    let base =
        directories::BaseDirs::new().ok_or_else(|| anyhow!("no config directory available"))?;
    Ok(base.config_dir().join("iroh/relay-url"))
}

fn parse_relay_urls(raw: &str) -> Result<Vec<iroh::RelayUrl>> {
    let urls: Vec<iroh::RelayUrl> = raw
        .lines()
        .flat_map(|line| line.split(','))
        .map(str::trim)
        .filter(|item| !item.is_empty() && !item.starts_with('#'))
        .map(|item| {
            item.parse()
                .map_err(|err| anyhow!("invalid relay URL {item}: {err}"))
        })
        .collect::<Result<Vec<_>>>()?;
    if urls.is_empty() {
        return Err(anyhow!("Iroh relay config has no valid URL"));
    }
    Ok(urls)
}

/// The active relay base URL, without a trailing slash.
pub fn relay_base_url() -> Option<String> {
    configured_relays()
        .ok()
        .flatten()?
        .first()
        .map(|url| url.to_string().trim_end_matches('/').to_string())
}

/// The relay list saved by the user, without environment overrides.
pub fn relay_url() -> String {
    let Ok(path) = relay_file_path() else {
        return String::new();
    };
    let Ok(text) = std::fs::read_to_string(path) else {
        return String::new();
    };
    parse_relay_urls(&text)
        .ok()
        .and_then(|urls| urls.first().map(|url| url.to_string()))
        .unwrap_or_default()
}

/// Save the relay list locally. An empty value removes the override and
/// restores the public n0 relays on the next endpoint bind.
pub fn set_relay_url(text: &str) -> Result<()> {
    let path = relay_file_path()?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(anyhow!("remove {}: {err}", path.display())),
        }
        return Ok(());
    }
    let urls = parse_relay_urls(trimmed)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let body = urls
        .iter()
        .map(|url| url.to_string())
        .collect::<Vec<_>>()
        .join("\n");
    write_private(&path, body.as_bytes())
}

fn configured_relays() -> Result<Option<Vec<iroh::RelayUrl>>> {
    let mut raw = std::env::var("CORCHO_IROH_RELAY")
        .or_else(|_| std::env::var("IROH_RELAY_URL"))
        .ok();
    if raw.as_deref().map(str::trim).is_some_and(str::is_empty) {
        raw = None;
    }
    let raw = match raw {
        Some(raw) => raw,
        None => {
            let path = relay_file_path()?;
            match std::fs::read_to_string(&path) {
                Ok(text) => text,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(err) => return Err(anyhow!("read {}: {err}", path.display())),
            }
        }
    };
    Ok(Some(parse_relay_urls(&raw)?))
}

async fn start_network(inner: &Arc<Inner>) -> Result<Network> {
    let iroh_dir = inner.data_dir.join("iroh");
    std::fs::create_dir_all(&iroh_dir)?;
    std::fs::create_dir_all(iroh_dir.join("blobs"))?;
    std::fs::create_dir_all(iroh_dir.join("docs"))?;

    let key = load_endpoint_key(&iroh_dir)?;
    let relays = configured_relays()?;
    let relay = relays
        .as_ref()
        .and_then(|relays| relays.first())
        .map(|url| url.to_string());
    let mut builder = iroh::Endpoint::builder(iroh::endpoint::presets::N0);
    if let Some(relays) = relays {
        builder = builder.relay_mode(iroh::endpoint::RelayMode::custom(relays));
    }
    let endpoint = builder
        .secret_key(key)
        .bind()
        .await
        .map_err(|e| anyhow!("bind endpoint: {e}"))?;
    {
        let mut status = inner.status.lock().unwrap();
        status.endpoint_id = Some(endpoint.id().to_string());
        status.relay = relay;
    }

    let blobs = iroh_blobs::store::fs::FsStore::load(iroh_dir.join("blobs"))
        .await
        .map_err(|e| anyhow!("blob store: {e}"))?;
    let gossip = iroh_gossip::net::Gossip::builder().spawn(endpoint.clone());
    let blobs_store: iroh_blobs::api::Store = blobs.clone().into();
    let docs = iroh_docs::protocol::Docs::persistent(iroh_dir.join("docs"))
        .spawn(endpoint.clone(), blobs_store, gossip.clone())
        .await
        .map_err(|e| anyhow!("docs store: {e}"))?;
    let author = docs
        .author_default()
        .await
        .map_err(|e| anyhow!("author: {e}"))?;

    let router = iroh::protocol::Router::builder(endpoint.clone())
        .accept(
            iroh_blobs::ALPN,
            iroh_blobs::BlobsProtocol::new(&blobs, None),
        )
        .accept(iroh_gossip::ALPN, gossip)
        .accept(iroh_docs::ALPN, docs.clone())
        .spawn();

    Ok(Network {
        endpoint,
        router,
        blobs,
        docs,
        author,
    })
}

fn load_endpoint_key(dir: &Path) -> Result<iroh::SecretKey> {
    let path = dir.join(ENDPOINT_KEY_FILE);
    if let Ok(bytes) = std::fs::read(&path) {
        if bytes.len() == 32 {
            let mut raw = [0u8; 32];
            raw.copy_from_slice(&bytes);
            return Ok(iroh::SecretKey::from_bytes(&raw));
        }
    }
    let key = iroh::SecretKey::generate();
    write_private(&path, &key.to_bytes())?;
    Ok(key)
}

pub fn endpoint_id(data_dir: &Path) -> Result<String> {
    let dir = data_dir.join("iroh");
    std::fs::create_dir_all(&dir)?;
    Ok(load_endpoint_key(&dir)?.public().to_string())
}

async fn open_document(
    inner: &Arc<Inner>,
    network: &Network,
) -> Result<Option<iroh_docs::api::Doc>> {
    let ticket = {
        let state = inner.state.lock().unwrap();
        state
            .write_ticket
            .clone()
            .or_else(|| state.read_ticket.clone())
    };
    let Some(ticket) = ticket else {
        return Ok(None);
    };
    let ticket = parse_ticket(&ticket)?;
    let (doc, _events) = network
        .docs
        .import_and_subscribe(ticket)
        .await
        .map_err(|e| anyhow!("open ticket: {e}"))?;
    let namespace = doc.id().to_string();
    {
        let mut status = inner.status.lock().unwrap();
        status.configured = true;
        status.namespace = Some(namespace);
        let state = inner.state.lock().unwrap();
        status.write_ticket = state.write_ticket.is_some();
        status.read_ticket = state.read_ticket.is_some();
        status.last_sync = state.last_sync.clone();
    }
    Ok(Some(doc))
}

async fn create_workspace(
    inner: &Arc<Inner>,
    network: &Network,
) -> Result<(String, iroh_docs::api::Doc)> {
    let doc = network
        .docs
        .create()
        .await
        .map_err(|e| anyhow!("create document: {e}"))?;
    let write = doc
        .share(
            iroh_docs::api::protocol::ShareMode::Write,
            iroh_docs::api::protocol::AddrInfoOptions::RelayAndAddresses,
        )
        .await
        .map_err(|e| anyhow!("share document: {e}"))?;
    let read = doc
        .share(
            iroh_docs::api::protocol::ShareMode::Read,
            iroh_docs::api::protocol::AddrInfoOptions::RelayAndAddresses,
        )
        .await
        .ok();
    let write = encode_ticket(&write);
    let key: [u8; KEY_LEN] = rand::random();
    let write = wrap_ticket(&write, Some(&key));
    {
        let mut state = inner.state.lock().unwrap();
        state.key = Some(encode_key(&key));
        state.write_ticket = Some(write.clone());
        state.read_ticket = read
            .as_ref()
            .map(|ticket| wrap_ticket(&encode_ticket(ticket), Some(&key)));
        state.clock = state.clock.max(1);
        state.applied.clear();
        state.last_published.clear();
        state.attachments.clear();
        state.attachment_plain.clear();
        state.last_sync = Some("created".into());
        save_state(inner, &state);
    }
    {
        let mut status = inner.status.lock().unwrap();
        status.configured = true;
        status.namespace = Some(doc.id().to_string());
        status.write_ticket = true;
        status.read_ticket = true;
        status.last_error = None;
    }
    Ok((write, doc))
}

async fn join_workspace(
    inner: &Arc<Inner>,
    network: &Network,
    ticket: &str,
) -> Result<iroh_docs::api::Doc> {
    let (parsed, incoming_key, fingerprint) = parse_setup_code(ticket)?;
    let Some(fingerprint) = fingerprint else {
        return Err(anyhow!(
            "this setup code came from an older Corcho; update both devices and share a new code"
        ));
    };
    if fingerprint != sync_fingerprint() {
        return Err(anyhow!(
            "setup code uses a different build; update Corcho on both devices"
        ));
    }
    let writable = matches!(parsed.capability, iroh_docs::Capability::Write(_));
    let raw_ticket = encode_ticket(&parsed);
    let (doc, _events) = network
        .docs
        .import_and_subscribe(parsed)
        .await
        .map_err(|e| anyhow!("join document: {e}"))?;
    {
        let mut state = inner.state.lock().unwrap();
        if let Some(key) = incoming_key {
            state.key = Some(encode_key(&key));
        }
        let effective = state.key.as_deref().and_then(|text| decode_key(text).ok());
        let stored = wrap_ticket(&raw_ticket, effective.as_ref());
        if writable {
            state.write_ticket = Some(stored);
        } else {
            state.read_ticket = Some(stored);
        }
        state.last_sync = Some("joined".into());
        save_state(inner, &state);
    }
    {
        let mut status = inner.status.lock().unwrap();
        status.configured = true;
        status.namespace = Some(doc.id().to_string());
        status.write_ticket = writable;
        status.read_ticket = !writable;
        status.last_error = None;
    }
    Ok(doc)
}

async fn share_document(
    inner: &Arc<Inner>,
    doc: &iroh_docs::api::Doc,
    read_only: bool,
    include_key: bool,
) -> Result<String> {
    let mode = if read_only {
        iroh_docs::api::protocol::ShareMode::Read
    } else {
        iroh_docs::api::protocol::ShareMode::Write
    };
    let ticket = doc
        .share(
            mode,
            iroh_docs::api::protocol::AddrInfoOptions::RelayAndAddresses,
        )
        .await
        .map_err(|e| anyhow!("share: {e}"))?;
    let text = encode_ticket(&ticket);
    let mut state = inner.state.lock().unwrap();
    let key = state
        .key
        .as_deref()
        .and_then(|value| decode_key(value).ok());
    let text = if include_key {
        wrap_ticket(&text, key.as_ref())
    } else {
        text
    };
    if include_key {
        if read_only {
            state.read_ticket = Some(text.clone());
        } else {
            state.write_ticket = Some(text.clone());
        }
        save_state(inner, &state);
    }
    Ok(text)
}

fn start_subscription(inner: Arc<Inner>, doc: iroh_docs::api::Doc, handle: tokio::runtime::Handle) {
    use futures_util::StreamExt;
    let tx = inner.tx.lock().unwrap().clone();
    let peers = inner.clone();
    handle.spawn(async move {
        let Ok(mut events) = doc.subscribe().await else {
            return;
        };
        while let Some(event) = events.next().await {
            match event {
                Ok(iroh_docs::engine::LiveEvent::InsertRemote { .. })
                | Ok(iroh_docs::engine::LiveEvent::ContentReady { .. })
                | Ok(iroh_docs::engine::LiveEvent::PendingContentReady)
                | Ok(iroh_docs::engine::LiveEvent::SyncFinished(_)) => {
                    peers.pull_requested.store(true, Ordering::Relaxed);
                    if let Some(tx) = &tx {
                        let _ = tx.send(Command::Wake);
                    }
                }
                Ok(iroh_docs::engine::LiveEvent::NeighborUp(_)) => {
                    peers.peers.fetch_add(1, Ordering::Relaxed);
                }
                Ok(iroh_docs::engine::LiveEvent::NeighborDown(_)) => {
                    let current = peers.peers.load(Ordering::Relaxed);
                    peers
                        .peers
                        .store(current.saturating_sub(1), Ordering::Relaxed);
                }
                _ => {}
            }
        }
    });
}

async fn publish(inner: &Arc<Inner>, network: &Network, doc: &iroh_docs::api::Doc) -> bool {
    if !inner.config.lock().map(|c| c.enabled).unwrap_or(false) {
        return true;
    }
    let Some(snapshot) = inner.snapshot.lock().unwrap().clone() else {
        return false;
    };
    let Some(ws) = snapshot() else {
        return false;
    };
    let mut records = split_workspace(&ws);

    if inner
        .config
        .lock()
        .map(|c| c.include_attachments)
        .unwrap_or(false)
    {
        publish_attachments(inner, network, doc, &mut records).await;
    }

    let mut plan: Vec<(String, Vec<u8>)> = Vec::new();
    let mut tombstones: Vec<(String, Vec<u8>)> = Vec::new();
    let mut journal: Option<(String, Record)> = None;
    {
        let state_key = inner.state.lock().unwrap().key.clone();
        let crypto_key = match state_key.as_deref() {
            Some(text) => match decode_key(text) {
                Ok(key) => Some(key),
                Err(err) => {
                    set_error(inner, format!("invalid workspace key: {err}"));
                    return false;
                }
            },
            None => None,
        };
        let mut state = inner.state.lock().unwrap();
        let device = state.device.clone();
        for (record_key, value) in &records {
            let hash = value_hash(value);
            if state.last_published.get(record_key) == Some(&hash) {
                continue;
            }
            state.clock = state.clock.saturating_add(1);
            let record = match &crypto_key {
                Some(key) if !record_key.starts_with("attachment/") => {
                    let plain = match serde_json::to_vec(value) {
                        Ok(plain) => plain,
                        Err(_) => continue,
                    };
                    match seal_random(key, record_key.as_bytes(), &plain) {
                        Ok(sealed) => Record {
                            v: SCHEMA,
                            device: device.clone(),
                            clock: state.clock,
                            deleted: false,
                            value: None,
                            sealed: Some(sealed),
                        },
                        Err(_) => continue,
                    }
                }
                _ => Record {
                    v: SCHEMA,
                    device: device.clone(),
                    clock: state.clock,
                    deleted: false,
                    value: Some(value.clone()),
                    sealed: None,
                },
            };
            match serde_json::to_vec(&record) {
                Ok(bytes) => plan.push((record_key.clone(), bytes)),
                Err(_) => continue,
            }
        }
        let missing: Vec<String> = state
            .last_published
            .keys()
            .filter(|key| !records.contains_key(*key))
            .cloned()
            .collect();
        for key in missing {
            state.clock = state.clock.saturating_add(1);
            let record = Record {
                v: SCHEMA,
                device: device.clone(),
                clock: state.clock,
                deleted: true,
                value: None,
                sealed: None,
            };
            if let Ok(bytes) = serde_json::to_vec(&record) {
                tombstones.push((key, bytes));
            }
        }
        if !plan.is_empty() || !tombstones.is_empty() {
            let mut changes = serde_json::Map::new();
            for (key, _) in &plan {
                if let Some(value) = records.get(key) {
                    changes.insert(key.clone(), value.clone());
                }
            }
            for (key, _) in &tombstones {
                changes.insert(key.clone(), json!({"deleted": true}));
            }
            let entry = json!({
                "at": now(),
                "direction": "publish",
                "device": device,
                "changes": changes,
            });
            append_history(&inner.data_dir, &entry);
            let journal_key = format!("journal/{}-{}-{}", now(), device, state.clock);
            let record = match &crypto_key {
                Some(key) => match serde_json::to_vec(&entry)
                    .map_err(|err| anyhow!("{err}"))
                    .and_then(|plain| seal_random(key, journal_key.as_bytes(), &plain))
                {
                    Ok(sealed) => Record {
                        v: SCHEMA,
                        device: device.clone(),
                        clock: state.clock,
                        deleted: false,
                        value: None,
                        sealed: Some(sealed),
                    },
                    Err(_) => Record {
                        v: SCHEMA,
                        device: device.clone(),
                        clock: state.clock,
                        deleted: false,
                        value: Some(entry),
                        sealed: None,
                    },
                },
                None => Record {
                    v: SCHEMA,
                    device: device.clone(),
                    clock: state.clock,
                    deleted: false,
                    value: Some(entry),
                    sealed: None,
                },
            };
            journal = Some((journal_key, record));
        }
    }

    let mut applied_keys: Vec<String> = Vec::new();
    for (key, bytes) in plan {
        if doc
            .set_bytes(network.author, key.clone(), bytes)
            .await
            .is_ok()
        {
            applied_keys.push(key);
        }
    }
    for (key, bytes) in tombstones {
        if doc
            .set_bytes(network.author, key.clone(), bytes)
            .await
            .is_ok()
        {
            applied_keys.push(key);
        }
    }
    if let Some((journal_key, record)) = journal {
        if let Ok(bytes) = serde_json::to_vec(&record) {
            let _ = doc.set_bytes(network.author, journal_key, bytes).await;
        }
    }

    let mut state = inner.state.lock().unwrap();
    for key in &applied_keys {
        if let Some(value) = records.remove(key) {
            state.last_published.insert(key.clone(), value_hash(&value));
        } else {
            state.last_published.remove(key);
        }
    }
    if !applied_keys.is_empty() {
        state.last_sync = Some(now());
        let mut status = inner.status.lock().unwrap();
        status.last_sync = state.last_sync.clone();
        status.last_error = None;
    }
    save_state(inner, &state);
    true
}

async fn pull_and_apply(inner: &Arc<Inner>, network: &Network, doc: &iroh_docs::api::Doc) -> bool {
    use futures_util::StreamExt;
    let query = iroh_docs::store::Query::single_latest_per_key().build();
    let entries = match doc.get_many(query).await {
        Ok(entries) => entries,
        Err(err) => {
            set_error(inner, format!("read document: {err}"));
            return true;
        }
    };
    let mut entries = Box::pin(entries);

    let mut latest: HashMap<String, (Record, String)> = HashMap::new();
    while let Some(entry) = entries.next().await {
        let Ok(entry) = entry else { continue };
        let key = String::from_utf8_lossy(entry.key()).to_string();
        let Ok(bytes) = network.blobs.get_bytes(entry.content_hash()).await else {
            // Content is still downloading; retry on the next event.
            inner.pull_requested.store(true, Ordering::Relaxed);
            continue;
        };
        let Ok(record) = serde_json::from_slice::<Record>(&bytes) else {
            continue;
        };
        let hash = if record.sealed.is_some() {
            String::new()
        } else {
            value_hash(&record_json(&record))
        };
        match latest.get(&key) {
            Some((existing, _)) if !record.newer_than(existing.clock, &existing.device) => {}
            _ => {
                latest.insert(key, (record, hash));
            }
        }
    }

    let state_key = inner.state.lock().unwrap().key.clone();
    let crypto_key = match state_key.as_deref() {
        Some(text) => match decode_key(text) {
            Ok(key) => Some(key),
            Err(err) => {
                set_error(inner, format!("invalid workspace key: {err}"));
                None
            }
        },
        None => None,
    };
    let mut decrypted: HashMap<String, (Record, String)> = HashMap::new();
    for (key, (mut record, _hash)) in latest {
        if key.starts_with("journal/") {
            continue;
        }
        if record.sealed.is_some() {
            let Some(crypto_key) = crypto_key.as_ref() else {
                set_error(
                    inner,
                    "workspace is encrypted; join it with a setup code".into(),
                );
                continue;
            };
            if let Err(err) = decrypt_record(crypto_key, &key, &mut record) {
                set_error(inner, format!("cannot decrypt {key}: {err}"));
                continue;
            }
        }
        let hash = value_hash(&record_json(&record));
        decrypted.insert(key, (record, hash));
    }

    let local_device = inner
        .state
        .lock()
        .map(|state| state.device.clone())
        .unwrap_or_default();
    let mut fingerprint_seen = false;
    if let Some((record, _)) = decrypted.get("sync/fingerprint") {
        if let Some(value) = &record.value {
            if let Some(fingerprint) = value.get("fp").and_then(|value| value.as_str()) {
                fingerprint_seen = true;
                if fingerprint != sync_fingerprint() && record.device != local_device {
                    set_error(
                        inner,
                        "workspace was written by a different Corcho build; update both devices"
                            .into(),
                    );
                    return true;
                }
            }
        }
    }
    if crypto_key.is_some() && !fingerprint_seen {
        set_error(
            inner,
            "workspace is missing a sync fingerprint; update Corcho and share a new setup code"
                .into(),
        );
        return true;
    }

    let mut to_apply: Vec<(String, Record)> = Vec::new();
    {
        let state = inner.state.lock().unwrap();
        for (key, (record, _hash)) in &decrypted {
            let applied = state.applied.get(key);
            let stale = applied
                .map(|a| !record.newer_than(a.clock, &a.device))
                .unwrap_or(false);
            if !stale {
                to_apply.push((key.clone(), record.clone()));
            }
        }
    }
    if to_apply.is_empty() {
        return true;
    }
    to_apply.sort_by(|a, b| a.0.cmp(&b.0));

    let changed = match inner
        .apply
        .lock()
        .unwrap()
        .as_ref()
        .map(|apply| apply(to_apply.clone()))
    {
        Some(Some(changed)) => changed,
        Some(None) => return false,
        None => return false,
    };

    let mut attachments = Vec::new();
    if changed {
        {
            let mut state = inner.state.lock().unwrap();
            for (key, record) in &to_apply {
                let hash = value_hash(&record_json(record));
                state.applied.insert(
                    key.clone(),
                    Applied {
                        clock: record.clock,
                        device: record.device.clone(),
                    },
                );
                state.clock = state.clock.max(record.clock);
                if record.deleted {
                    state.last_published.remove(key);
                } else {
                    state.last_published.insert(key.clone(), hash);
                }
            }
            state.last_sync = Some(now());
            save_state(inner, &state);
        }
        {
            let mut status = inner.status.lock().unwrap();
            status.last_sync = inner.state.lock().ok().and_then(|s| s.last_sync.clone());
            status.last_error = None;
        }
        attachments.extend(
            to_apply
                .iter()
                .filter(|(key, record)| key.starts_with("attachment/") && !record.deleted)
                .cloned(),
        );
    }
    for (key, record) in attachments {
        export_attachment(inner, network, &key, &record).await;
    }
    true
}

async fn publish_attachments(
    inner: &Arc<Inner>,
    network: &Network,
    _doc: &iroh_docs::api::Doc,
    records: &mut BTreeMap<String, Value>,
) {
    let dir = storage::attachments_dir(&inner.data_dir);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return;
    };
    let state_key = inner.state.lock().unwrap().key.clone();
    let crypto_key = state_key.as_deref().and_then(|text| decode_key(text).ok());
    for entry in entries.flatten() {
        if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
        let Ok(bytes) = std::fs::read(entry.path()) else {
            continue;
        };
        let plain_hash = sha256_hex(&bytes);
        {
            let state = inner.state.lock().unwrap();
            if state.attachment_plain.get(&name) == Some(&plain_hash) {
                continue;
            }
        }
        let (hash, nonce) = match &crypto_key {
            Some(key) => {
                let nonce = attachment_nonce(key, &name, &bytes);
                let aad = format!("attachment/{name}");
                let Ok(ciphertext) = seal_bytes(key, nonce, aad.as_bytes(), &bytes) else {
                    continue;
                };
                let Ok(tag) = network.blobs.add_bytes(ciphertext).await else {
                    continue;
                };
                (tag.hash.to_string(), Some(URL_SAFE_NO_PAD.encode(nonce)))
            }
            None => {
                let Ok(tag) = network.blobs.add_bytes(bytes).await else {
                    continue;
                };
                (tag.hash.to_string(), None)
            }
        };
        {
            let mut state = inner.state.lock().unwrap();
            state.attachments.insert(name.clone(), hash.clone());
            state.attachment_plain.insert(name.clone(), plain_hash);
            save_state(inner, &state);
        }
        let mut value = json!({"hash": hash, "name": name, "size": size});
        if let Some(nonce) = nonce {
            value["enc"] = json!(true);
            value["nonce"] = json!(nonce);
        }
        records.insert(format!("attachment/{name}"), value);
    }
}

async fn export_attachment(inner: &Arc<Inner>, network: &Network, key: &str, record: &Record) {
    let Some(value) = &record.value else {
        return;
    };
    let Some(hash) = value.get("hash").and_then(|v| v.as_str()) else {
        return;
    };
    let Ok(hash) = hash.parse::<iroh_blobs::Hash>() else {
        return;
    };
    let name = key.trim_start_matches("attachment/");
    if name.is_empty() || name.contains('/') {
        return;
    }
    let dir = storage::attachments_dir(&inner.data_dir);
    let _ = std::fs::create_dir_all(&dir);
    let encrypted = value.get("enc").and_then(|v| v.as_bool()).unwrap_or(false);
    if encrypted {
        let Some(nonce) = value
            .get("nonce")
            .and_then(|v| v.as_str())
            .and_then(|v| URL_SAFE_NO_PAD.decode(v).ok())
        else {
            return;
        };
        let state_key = inner.state.lock().unwrap().key.clone();
        let Some(crypto_key) = state_key.as_deref().and_then(|text| decode_key(text).ok()) else {
            return;
        };
        let Ok(ciphertext) = network.blobs.get_bytes(hash).await else {
            return;
        };
        let aad = format!("attachment/{name}");
        let Ok(plain) = open_bytes(&crypto_key, &nonce, aad.as_bytes(), &ciphertext) else {
            return;
        };
        let _ = std::fs::write(dir.join(name), plain);
    } else {
        let _ = network.blobs.export(hash, dir.join(name)).await;
    }
}

// ---------------------------------------------------------------------------
// Records: workspace <-> document
// ---------------------------------------------------------------------------

pub fn split_workspace(ws: &Workspace) -> BTreeMap<String, Value> {
    let mut records = BTreeMap::new();
    for grid in &ws.grids {
        records.insert(
            format!("grid/{}", grid.id),
            json!({
                "id": &grid.id,
                "name": &grid.name,
                "icon": &grid.icon,
                "folderId": &grid.folder_id,
            }),
        );
        for card in &grid.cards {
            records.insert(
                format!("card/{}", card.id),
                json!({"grid": &grid.id, "card": card}),
            );
        }
    }
    for folder in &ws.folders {
        if let Ok(value) = serde_json::to_value(folder) {
            records.insert(format!("folder/{}", folder.id), value);
        }
    }
    for calendar in &ws.calendars {
        if let Ok(value) = serde_json::to_value(calendar) {
            records.insert(format!("calendar/{}", calendar.id), value);
        }
    }
    for link in &ws.date_links {
        if let Ok(value) = serde_json::to_value(link) {
            records.insert(format!("date-link/{}", link.id), value);
        }
    }
    for template in &ws.templates {
        if let Ok(value) = serde_json::to_value(template) {
            records.insert(format!("template/{}", template.id), value);
        }
    }
    records.insert("settings".into(), syncable_settings(&ws.settings));
    records.insert(
        "grid-order".into(),
        json!(ws.grids.iter().map(|grid| &grid.id).collect::<Vec<_>>()),
    );
    records.insert(
        "folder-order".into(),
        json!(ws
            .folders
            .iter()
            .map(|folder| &folder.id)
            .collect::<Vec<_>>()),
    );
    records.insert("sync/fingerprint".into(), json!({"fp": sync_fingerprint()}));
    records
}

pub(crate) fn syncable_settings(settings: &Settings) -> Value {
    let mut value = serde_json::to_value(settings).unwrap_or_else(|_| json!({}));
    if let Some(object) = value.as_object_mut() {
        for key in [
            "aiApiKeys",
            "ai_local_server_url",
            "gdrive_client_id",
            "gdrive_client_secret",
            "gdrive_refresh_token",
            "gdrive_email",
            "repo_token_github",
            "repo_token_gitlab",
            "repo_token_forgejo",
            "git_auth",
            "cloud_folder",
            "data_dir",
            "iroh_sync_enabled",
            "iroh_include_attachments",
        ] {
            object.remove(key);
        }
    }
    value
}

fn record_json(record: &Record) -> Value {
    match &record.value {
        Some(value) if !record.deleted => value.clone(),
        _ => json!({"deleted": true}),
    }
}

fn value_hash(value: &Value) -> String {
    let mut hasher = Sha256::new();
    hasher.update(serde_json::to_vec(value).unwrap_or_default());
    format!("{:x}", hasher.finalize())
}

/// Apply a batch of records to a workspace. Returns true when anything changed.
pub fn apply_records(ws: &mut Workspace, records: &[(String, Record)]) -> bool {
    let mut ordered: Vec<&(String, Record)> = records.iter().collect();
    ordered.sort_by_key(|(key, _)| (record_order(key), key.clone()));

    let mut changed = false;
    let before = value_hash(&serde_json::to_value(&*ws).unwrap_or(Value::Null));
    let mut grid_order: Option<Vec<String>> = None;
    let mut folder_order: Option<Vec<String>> = None;

    for (key, record) in ordered {
        if record.v > SCHEMA {
            continue;
        }
        let deleted = record.deleted || record.value.is_none();
        if key == "grid-order" || key == "folder-order" {
            if !deleted {
                if let Some(ids) = record.value.as_ref().and_then(|value| value.as_array()) {
                    let ids = ids
                        .iter()
                        .filter_map(|value| value.as_str().map(str::to_string))
                        .collect::<Vec<_>>();
                    if key == "grid-order" {
                        grid_order = Some(ids);
                    } else {
                        folder_order = Some(ids);
                    }
                }
            }
            continue;
        }
        if key == "settings" {
            if let Some(mut value) = record.value.clone() {
                if !deleted {
                    crate::core::merge_settings_value(&mut ws.settings, &mut value);
                    changed = true;
                }
            }
            continue;
        }
        if let Some(id) = key.strip_prefix("grid/") {
            if deleted {
                let protected = ws
                    .grids
                    .iter()
                    .find(|g| g.id == id)
                    .map(|g| !g.cards.is_empty())
                    .unwrap_or(false);
                if protected {
                    continue;
                }
                if ws.grids.iter().any(|g| g.id == id) {
                    ws.grids.retain(|g| g.id != id);
                    changed = true;
                }
            } else if let Some(value) = &record.value {
                let name = value.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let icon = value
                    .get("icon")
                    .and_then(|v| v.as_str())
                    .unwrap_or("layout-grid");
                let folder = value
                    .get("folderId")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);
                if let Some(grid) = ws.grids.iter_mut().find(|g| g.id == id) {
                    grid.name = name.to_string();
                    grid.icon = icon.to_string();
                    grid.folder_id = folder;
                } else {
                    let mut grid = Grid {
                        id: id.to_string(),
                        ..Default::default()
                    };
                    grid.name = name.to_string();
                    grid.icon = icon.to_string();
                    grid.folder_id = folder;
                    ws.grids.push(grid);
                }
                changed = true;
            }
            continue;
        }
        if let Some(id) = key.strip_prefix("card/") {
            if deleted {
                let protected = ws
                    .grids
                    .iter()
                    .flat_map(|grid| grid.cards.iter())
                    .find(|card| card.id == id)
                    .map(card_has_content)
                    .unwrap_or(false);
                if protected {
                    continue;
                }
                let mut removed = false;
                for grid in &mut ws.grids {
                    let len = grid.cards.len();
                    grid.cards.retain(|c| c.id != id);
                    removed |= grid.cards.len() != len;
                }
                changed |= removed;
            } else if let Some(value) = &record.value {
                let grid_id = value.get("grid").and_then(|v| v.as_str()).unwrap_or("");
                let Some(card_value) = value.get("card") else {
                    continue;
                };
                let Ok(card) = serde_json::from_value::<Card>(card_value.clone()) else {
                    continue;
                };
                for grid in &mut ws.grids {
                    grid.cards.retain(|c| c.id != card.id);
                }
                if let Some(grid) = ws.grids.iter_mut().find(|g| g.id == grid_id) {
                    grid.cards.push(card);
                } else {
                    let mut grid = Grid {
                        id: grid_id.to_string(),
                        name: "Imported".into(),
                        ..Default::default()
                    };
                    grid.cards.push(card);
                    ws.grids.push(grid);
                }
                changed = true;
            }
            continue;
        }
        if key.starts_with("calendar/")
            || key.starts_with("date-link/")
            || key.starts_with("template/")
            || key.starts_with("folder/")
        {
            // Handled below with concrete types to keep serde shapes explicit.
            continue;
        }
    }

    // Folders, calendars, date links and templates are simple id-keyed lists.
    for (key, record) in records {
        let deleted = record.deleted || record.value.is_none();
        if let Some(id) = key.strip_prefix("folder/") {
            if deleted {
                ws.folders.retain(|f| f.id != id);
                changed = true;
            } else if let Some(value) = &record.value {
                if let Ok(folder) = serde_json::from_value::<Folder>(value.clone()) {
                    if let Some(existing) = ws.folders.iter_mut().find(|f| f.id == folder.id) {
                        *existing = folder;
                    } else {
                        ws.folders.push(folder);
                    }
                    changed = true;
                }
            }
        } else if let Some(id) = key.strip_prefix("calendar/") {
            if deleted {
                ws.calendars.retain(|c| c.id != id);
                changed = true;
            } else if let Some(value) = &record.value {
                if let Ok(calendar) = serde_json::from_value::<CalendarDef>(value.clone()) {
                    if let Some(existing) = ws.calendars.iter_mut().find(|c| c.id == calendar.id) {
                        *existing = calendar;
                    } else {
                        ws.calendars.push(calendar);
                    }
                    changed = true;
                }
            }
        } else if let Some(id) = key.strip_prefix("date-link/") {
            if deleted {
                ws.date_links.retain(|l| l.id != id);
                changed = true;
            } else if let Some(value) = &record.value {
                if let Ok(link) = serde_json::from_value::<DateLink>(value.clone()) {
                    if let Some(existing) = ws.date_links.iter_mut().find(|l| l.id == link.id) {
                        *existing = link;
                    } else {
                        ws.date_links.push(link);
                    }
                    changed = true;
                }
            }
        } else if let Some(id) = key.strip_prefix("template/") {
            if deleted {
                ws.templates.retain(|t| t.id != id);
                changed = true;
            } else if let Some(value) = &record.value {
                if let Ok(template) = serde_json::from_value::<Template>(value.clone()) {
                    if let Some(existing) = ws.templates.iter_mut().find(|t| t.id == template.id) {
                        *existing = template;
                    } else {
                        ws.templates.push(template);
                    }
                    changed = true;
                }
            }
        }
    }

    if let Some(ids) = grid_order {
        reorder_by_ids(&mut ws.grids, &ids, |grid| grid.id.as_str());
        changed = true;
    }
    if let Some(ids) = folder_order {
        reorder_by_ids(&mut ws.folders, &ids, |folder| folder.id.as_str());
        changed = true;
    }

    if ws.active_grid_id.is_none()
        || !ws
            .active_grid_id
            .as_ref()
            .map(|id| ws.grids.iter().any(|g| &g.id == id))
            .unwrap_or(false)
    {
        ws.active_grid_id = ws.grids.first().map(|g| g.id.clone());
    }

    let after = value_hash(&serde_json::to_value(&*ws).unwrap_or(Value::Null));
    changed || before != after
}

fn card_has_content(card: &Card) -> bool {
    !card.title.is_empty() || !card.text.is_empty() || !card.payload.is_empty()
}

fn reorder_by_ids<T>(items: &mut Vec<T>, ids: &[String], id_of: impl Fn(&T) -> &str) {
    let mut ordered = Vec::with_capacity(items.len());
    for id in ids {
        if let Some(position) = items.iter().position(|item| id_of(item) == id.as_str()) {
            ordered.push(items.remove(position));
        }
    }
    ordered.append(items);
    *items = ordered;
}

fn record_order(key: &str) -> u8 {
    if key.starts_with("grid/") {
        0
    } else if key.starts_with("folder/") || key.starts_with("calendar/") {
        1
    } else if key.starts_with("card/") {
        2
    } else if key.starts_with("date-link/") || key.starts_with("template/") {
        3
    } else if key == "settings" {
        4
    } else {
        5
    }
}

// ---------------------------------------------------------------------------
// Local state
// ---------------------------------------------------------------------------

fn state_path(data_dir: &Path) -> PathBuf {
    data_dir.join("iroh").join(STATE_FILE)
}

fn load_local_state(data_dir: &Path) -> LocalState {
    std::fs::read_to_string(state_path(data_dir))
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default()
}

fn save_state(inner: &Arc<Inner>, state: &LocalState) {
    let dir = inner.data_dir.join("iroh");
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(raw) = serde_json::to_vec_pretty(state) {
        let _ = write_private(&state_path(&inner.data_dir), &raw);
    }
    if let Ok(mut status) = inner.status.lock() {
        status.clock = state.clock;
        status.last_sync = state.last_sync.clone();
    }
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn append_history(data_dir: &Path, entry: &Value) {
    let dir = data_dir.join("iroh");
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path = dir.join("history.jsonl");
    let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    else {
        return;
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600));
    }
    let _ = writeln!(file, "{}", entry);
}

pub fn backup_conflict(data_dir: &Path, key: &str, local: &Value, remote: &Value) {
    let dir = crate::storage::backups_dir(data_dir);
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let safe: String = key
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let path = dir.join(format!("iroh-conflict-{}-{safe}.json", now()));
    let entry = json!({
        "at": now(),
        "key": key,
        "local": local,
        "remote": remote,
    });
    if let Ok(bytes) = serde_json::to_vec_pretty(&entry) {
        let _ = write_private(&path, &bytes);
    }
}

fn set_error(inner: &Arc<Inner>, message: String) {
    tracing::warn!("{message}");
    if let Ok(mut status) = inner.status.lock() {
        status.last_error = Some(message);
    }
}

fn status_error(inner: &Arc<Inner>) -> String {
    inner
        .status
        .lock()
        .ok()
        .and_then(|s| s.last_error.clone())
        .unwrap_or_else(|| "unknown error".into())
}

fn namespace_id(inner: &Arc<Inner>) -> String {
    inner
        .status
        .lock()
        .ok()
        .and_then(|s| s.namespace.clone())
        .unwrap_or_default()
}

fn now() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "now".into())
}

fn parse_ticket(text: &str) -> Result<iroh_docs::DocTicket> {
    Ok(parse_setup_code(text)?.0)
}

fn decode_key(text: &str) -> Result<[u8; KEY_LEN]> {
    let bytes = URL_SAFE_NO_PAD
        .decode(text.trim())
        .map_err(|e| anyhow!("invalid workspace key: {e}"))?;
    if bytes.len() != KEY_LEN {
        return Err(anyhow!("invalid workspace key length"));
    }
    let mut key = [0u8; KEY_LEN];
    key.copy_from_slice(&bytes);
    Ok(key)
}

fn encode_key(key: &[u8; KEY_LEN]) -> String {
    URL_SAFE_NO_PAD.encode(key)
}

fn sync_fingerprint() -> &'static str {
    static FINGERPRINT: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    FINGERPRINT.get_or_init(|| {
        let mut hasher = Sha256::new();
        hasher.update(b"corcho-sync-fingerprint");
        hasher.update(SCHEMA.to_le_bytes());
        hasher.update(b"record:2\0crypto:xchacha20poly1305\0setup:2\0pair:1\0build:");
        hasher.update(BUILD_SHA.as_bytes());
        format!("{:x}", hasher.finalize())
    })
}

fn wrap_ticket(ticket: &str, key: Option<&[u8; KEY_LEN]>) -> String {
    match key {
        Some(key) => format!(
            "{SETUP_PREFIX_V2}{}:{}:{ticket}",
            encode_key(key),
            sync_fingerprint()
        ),
        None => ticket.to_string(),
    }
}

fn parse_setup_code(
    text: &str,
) -> Result<(iroh_docs::DocTicket, Option<[u8; KEY_LEN]>, Option<String>)> {
    use iroh_tickets::Ticket;
    let text = text.trim();
    if let Some(rest) = text.strip_prefix(SETUP_PREFIX_V2) {
        let mut parts = rest.splitn(3, ':');
        let key_text = parts
            .next()
            .ok_or_else(|| anyhow!("invalid Corcho setup code"))?;
        let fingerprint = parts
            .next()
            .filter(|value| value.len() == 64 && value.chars().all(|c| c.is_ascii_hexdigit()))
            .ok_or_else(|| anyhow!("invalid Corcho setup fingerprint"))?;
        let ticket_text = parts
            .next()
            .ok_or_else(|| anyhow!("invalid Corcho setup code"))?;
        let key = decode_key(key_text)?;
        let ticket = iroh_docs::DocTicket::decode_string(ticket_text)
            .map_err(|e| anyhow!("invalid Iroh ticket: {e}"))?;
        Ok((ticket, Some(key), Some(fingerprint.to_string())))
    } else if let Some(rest) = text.strip_prefix(SETUP_PREFIX) {
        let (key_text, ticket_text) = rest
            .split_once(':')
            .ok_or_else(|| anyhow!("invalid Corcho setup code"))?;
        let key = decode_key(key_text)?;
        let ticket = iroh_docs::DocTicket::decode_string(ticket_text)
            .map_err(|e| anyhow!("invalid Iroh ticket: {e}"))?;
        Ok((ticket, Some(key), None))
    } else {
        let ticket = iroh_docs::DocTicket::decode_string(text)
            .map_err(|e| anyhow!("invalid Iroh ticket: {e}"))?;
        Ok((ticket, None, None))
    }
}

fn seal_bytes(
    key: &[u8; KEY_LEN],
    nonce: [u8; 24],
    aad: &[u8],
    plaintext: &[u8],
) -> Result<Vec<u8>> {
    let cipher = XChaCha20Poly1305::new(chacha20poly1305::Key::from_slice(key));
    cipher
        .encrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .map_err(|_| anyhow!("encrypt data"))
}

fn open_bytes(key: &[u8; KEY_LEN], nonce: &[u8], aad: &[u8], ciphertext: &[u8]) -> Result<Vec<u8>> {
    if nonce.len() != 24 {
        return Err(anyhow!("invalid nonce length"));
    }
    let cipher = XChaCha20Poly1305::new(chacha20poly1305::Key::from_slice(key));
    cipher
        .decrypt(
            XNonce::from_slice(nonce),
            Payload {
                msg: ciphertext,
                aad,
            },
        )
        .map_err(|_| anyhow!("cannot decrypt data"))
}

fn seal(key: &[u8; KEY_LEN], nonce: [u8; 24], aad: &[u8], plaintext: &[u8]) -> Result<Sealed> {
    let ciphertext = seal_bytes(key, nonce, aad, plaintext)?;
    Ok(Sealed {
        n: URL_SAFE_NO_PAD.encode(nonce),
        c: URL_SAFE_NO_PAD.encode(ciphertext),
    })
}

fn seal_random(key: &[u8; KEY_LEN], aad: &[u8], plaintext: &[u8]) -> Result<Sealed> {
    let nonce: [u8; 24] = rand::random();
    seal(key, nonce, aad, plaintext)
}

fn open_sealed(key: &[u8; KEY_LEN], aad: &[u8], sealed: &Sealed) -> Result<Vec<u8>> {
    let nonce = URL_SAFE_NO_PAD
        .decode(&sealed.n)
        .map_err(|e| anyhow!("invalid record nonce: {e}"))?;
    if nonce.len() != 24 {
        return Err(anyhow!("invalid record nonce length"));
    }
    let ciphertext = URL_SAFE_NO_PAD
        .decode(&sealed.c)
        .map_err(|e| anyhow!("invalid record ciphertext: {e}"))?;
    open_bytes(key, &nonce, aad, &ciphertext)
}

fn decrypt_record(key: &[u8; KEY_LEN], record_key: &str, record: &mut Record) -> Result<()> {
    let Some(sealed) = record.sealed.take() else {
        return Ok(());
    };
    let plain = open_sealed(key, record_key.as_bytes(), &sealed)?;
    record.value = Some(
        serde_json::from_slice(&plain).map_err(|err| anyhow!("invalid decrypted record: {err}"))?,
    );
    Ok(())
}

fn attachment_nonce(key: &[u8; KEY_LEN], name: &str, plaintext: &[u8]) -> [u8; 24] {
    let mut hasher = Sha256::new();
    hasher.update(b"corcho-attachment-nonce");
    hasher.update(key);
    hasher.update(name.as_bytes());
    hasher.update(plaintext);
    let digest = hasher.finalize();
    let mut nonce = [0u8; 24];
    nonce.copy_from_slice(&digest[..24]);
    nonce
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn encode_ticket(ticket: &iroh_docs::DocTicket) -> String {
    use iroh_tickets::Ticket;
    ticket.encode_string()
}

// ---------------------------------------------------------------------------
// Headless archival daemon
// ---------------------------------------------------------------------------

async fn prefetch(network: &Network, doc: &iroh_docs::api::Doc) {
    use futures_util::StreamExt;
    let Ok(entries) = doc
        .get_many(iroh_docs::store::Query::single_latest_per_key().build())
        .await
    else {
        return;
    };
    let mut entries = Box::pin(entries);
    while let Some(entry) = entries.next().await {
        let Ok(entry) = entry else {
            continue;
        };
        let key = String::from_utf8_lossy(entry.key()).to_string();
        let Ok(bytes) = network.blobs.get_bytes(entry.content_hash()).await else {
            continue;
        };
        if !key.starts_with("attachment/") {
            continue;
        }
        let Ok(record) = serde_json::from_slice::<Record>(&bytes) else {
            continue;
        };
        let Some(value) = record.value else {
            continue;
        };
        let Some(hash) = value.get("hash").and_then(|v| v.as_str()) else {
            continue;
        };
        if let Ok(hash) = hash.parse::<iroh_blobs::Hash>() {
            let _ = network.blobs.get_bytes(hash).await;
        }
    }
}

fn wipe_cache(data_dir: &Path) {
    for name in ["docs", "blobs"] {
        let path = data_dir.join("iroh").join(name);
        match std::fs::remove_dir_all(&path) {
            Ok(()) => eprintln!("corcho sync daemon: expired {}", path.display()),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => eprintln!(
                "corcho sync daemon: cannot remove {}: {err}",
                path.display()
            ),
        }
    }
}

/// Join a workspace read-only and keep a persistent copy so other devices can
/// sync even when every writer is offline. Never publishes.
pub fn run_daemon(data_dir: PathBuf, ticket: String, ttl_hours: u64) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .worker_threads(2)
        .build()?;
    runtime.block_on(async move {
        let inner = Arc::new(Inner {
            tx: Mutex::new(None),
            status: Mutex::new(Status::default()),
            config: Mutex::new(IrohConfig {
                enabled: true,
                include_attachments: true,
            }),
            snapshot: Mutex::new(None),
            apply: Mutex::new(None),
            state: Mutex::new(LocalState::default()),
            dirty_since: Mutex::new(None),
            pull_requested: AtomicBool::new(false),
            peers: AtomicUsize::new(0),
            stop: AtomicBool::new(false),
            data_dir: data_dir.clone(),
        });
        let network = start_network(&inner).await?;
        let parsed = parse_ticket(&ticket)?;
        let ttl = (ttl_hours > 0).then(|| Duration::from_secs(ttl_hours * 3600));
        let deadline = ttl.map(|ttl| Instant::now() + ttl);
        let (doc, mut events) = network
            .docs
            .import_and_subscribe(parsed)
            .await
            .map_err(|e| anyhow!("join ticket: {e}"))?;
        eprintln!(
            "corcho sync daemon: storing namespace {} at {}",
            doc.id(),
            data_dir.display()
        );
        use futures_util::StreamExt;
        let mut sigterm =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
        let mut expired = false;
        let mut ticker = tokio::time::interval(Duration::from_secs(30));
        loop {
            if deadline
                .map(|deadline| Instant::now() >= deadline)
                .unwrap_or(false)
            {
                expired = true;
                break;
            }
            tokio::select! {
                _ = tokio::signal::ctrl_c() => break,
                _ = sigterm.recv() => break,
                _ = ticker.tick() => {
                    prefetch(&network, &doc).await;
                }
                event = events.next() => {
                    match event {
                        Some(Ok(iroh_docs::engine::LiveEvent::NeighborUp(id))) => {
                            eprintln!("corcho sync daemon: peer {id}");
                        }
                        Some(_) => {}
                        None => break,
                    }
                }
            }
        }
        // Flush the stores before exiting; FsStore otherwise loses the last
        // seconds of writes.
        let _ = doc.close().await;
        let _ = network.router.shutdown().await;
        let _ = network.blobs.shutdown().await;
        network.endpoint.close().await;
        if expired {
            tokio::time::sleep(Duration::from_millis(500)).await;
            wipe_cache(&inner.data_dir);
        }
        Ok::<(), anyhow::Error>(())
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn card(ws: &mut Workspace, title: &str) -> String {
        let mut card = Card::new("note", 10.0, 20.0);
        card.title = title.into();
        ws.grids[0].cards.push(card);
        ws.grids[0].cards.last().unwrap().id.clone()
    }

    #[test]
    fn split_and_apply_round_trip() {
        let mut ws = Workspace::default();
        card(&mut ws, "hello");
        let records = split_workspace(&ws);
        let mut target = Workspace::default();
        let applied: Vec<(String, Record)> = records
            .into_iter()
            .map(|(key, value)| {
                (
                    key,
                    Record {
                        v: SCHEMA,
                        device: "dev".into(),
                        clock: 1,
                        deleted: false,
                        value: Some(value),
                        sealed: None,
                    },
                )
            })
            .collect();
        assert!(apply_records(&mut target, &applied));
        let imported = target
            .grids
            .iter()
            .flat_map(|g| g.cards.iter())
            .find(|c| c.title == "hello");
        assert!(imported.is_some());
    }

    #[test]
    fn tombstone_removes_empty_card() {
        let mut ws = Workspace::default();
        let note = Card::new("note", 10.0, 20.0);
        let id = note.id.clone();
        ws.grids[0].cards.push(note);
        let records = vec![(
            format!("card/{id}"),
            Record {
                v: SCHEMA,
                device: "dev".into(),
                clock: 2,
                deleted: true,
                value: None,
                sealed: None,
            },
        )];
        assert!(apply_records(&mut ws, &records));
        assert!(ws.grids.iter().all(|g| g.cards.is_empty()));
    }

    #[test]
    fn remote_tombstones_keep_non_empty_cards_and_grids() {
        let mut ws = Workspace::default();
        let id = card(&mut ws, "keep me");
        let grid_id = ws.grids[0].id.clone();
        let records = vec![
            (
                format!("card/{id}"),
                Record {
                    v: SCHEMA,
                    device: "dev".into(),
                    clock: 2,
                    deleted: true,
                    value: None,
                    sealed: None,
                },
            ),
            (
                format!("grid/{grid_id}"),
                Record {
                    v: SCHEMA,
                    device: "dev".into(),
                    clock: 3,
                    deleted: true,
                    value: None,
                    sealed: None,
                },
            ),
        ];
        let _ = apply_records(&mut ws, &records);
        assert!(ws.grids.iter().any(|g| g.id == grid_id));
        assert!(ws
            .grids
            .iter()
            .flat_map(|g| g.cards.iter())
            .any(|c| c.id == id));
    }

    #[test]
    fn settings_never_publish_secrets() {
        let mut ws = Workspace::default();
        ws.settings
            .ai_api_keys
            .insert("claude".into(), "secret".into());
        ws.settings.repo_token_github = "token".into();
        let records = split_workspace(&ws);
        let settings = records.get("settings").unwrap().to_string();
        assert!(!settings.contains("secret"));
        assert!(!settings.contains("token"));
        assert!(!settings.contains("aiApiKeys"));
    }

    #[test]
    fn relay_url_list_parses_and_rejects_garbage() {
        let urls =
            parse_relay_urls("https://relay.example.com\n# comment\nhttps://backup.example.com")
                .unwrap();
        assert_eq!(urls.len(), 2);
        assert!(parse_relay_urls("# only comments").is_err());
        assert!(parse_relay_urls("not a url").is_err());
    }

    #[test]
    fn sealed_roundtrip_binds_aad_and_key() {
        let key: [u8; KEY_LEN] = rand::random();
        let sealed = seal_random(&key, b"card/a", b"hello").unwrap();
        assert_eq!(open_sealed(&key, b"card/a", &sealed).unwrap(), b"hello");
        assert!(open_sealed(&key, b"card/b", &sealed).is_err());
        let other: [u8; KEY_LEN] = rand::random();
        assert!(open_sealed(&other, b"card/a", &sealed).is_err());
    }

    #[test]
    fn cache_wipe_keeps_the_endpoint_key() {
        let dir = tempfile::tempdir().unwrap();
        let iroh = dir.path().join("iroh");
        std::fs::create_dir_all(iroh.join("docs")).unwrap();
        std::fs::create_dir_all(iroh.join("blobs")).unwrap();
        std::fs::write(iroh.join("docs/entry"), b"x").unwrap();
        std::fs::write(iroh.join("blobs/blob"), b"y").unwrap();
        std::fs::write(iroh.join(ENDPOINT_KEY_FILE), b"key").unwrap();
        wipe_cache(dir.path());
        assert!(!iroh.join("docs").exists());
        assert!(!iroh.join("blobs").exists());
        assert!(iroh.join(ENDPOINT_KEY_FILE).exists());
    }

    #[test]
    fn encrypted_record_roundtrips() {
        let key: [u8; KEY_LEN] = rand::random();
        let plain = json!({"grid": "g", "card": {"id": "c", "title": "secret"}});
        let sealed = seal_random(&key, b"card/c", &serde_json::to_vec(&plain).unwrap()).unwrap();
        let mut record = Record {
            v: SCHEMA,
            device: "dev".into(),
            clock: 1,
            deleted: false,
            value: None,
            sealed: Some(sealed),
        };
        decrypt_record(&key, "card/c", &mut record).unwrap();
        assert_eq!(record.value, Some(plain));
        assert!(record.sealed.is_none());
    }

    #[test]
    fn key_encoding_roundtrips() {
        let key: [u8; KEY_LEN] = rand::random();
        assert_eq!(decode_key(&encode_key(&key)).unwrap(), key);
        assert!(decode_key("short").is_err());
    }

    #[test]
    fn attachment_nonce_is_deterministic_per_name() {
        let key: [u8; KEY_LEN] = rand::random();
        assert_eq!(
            attachment_nonce(&key, "a.png", b"data"),
            attachment_nonce(&key, "a.png", b"data")
        );
        assert_ne!(
            attachment_nonce(&key, "a.png", b"data"),
            attachment_nonce(&key, "b.png", b"data")
        );
    }

    #[test]
    fn grid_and_folder_order_round_trips() {
        use crate::model::{Folder, Grid};
        let mut ws = Workspace {
            grids: vec![
                Grid {
                    id: "g1".into(),
                    name: "One".into(),
                    ..Default::default()
                },
                Grid {
                    id: "g2".into(),
                    name: "Two".into(),
                    ..Default::default()
                },
                Grid {
                    id: "g3".into(),
                    name: "Three".into(),
                    ..Default::default()
                },
            ],
            folders: vec![
                Folder {
                    id: "f1".into(),
                    name: "F1".into(),
                    collapsed: false,
                },
                Folder {
                    id: "f2".into(),
                    name: "F2".into(),
                    collapsed: false,
                },
            ],
            ..Default::default()
        };
        ws.grids.swap(0, 2);
        ws.folders.swap(0, 1);
        let records = split_workspace(&ws)
            .into_iter()
            .map(|(key, value)| {
                (
                    key,
                    Record {
                        v: SCHEMA,
                        device: "dev".into(),
                        clock: 1,
                        deleted: false,
                        value: Some(value),
                        sealed: None,
                    },
                )
            })
            .collect::<Vec<_>>();
        let mut target = Workspace::default();
        target.grids.clear();
        target.folders.clear();
        assert!(apply_records(&mut target, &records));
        assert_eq!(
            target
                .grids
                .iter()
                .map(|g| g.id.as_str())
                .collect::<Vec<_>>(),
            vec!["g3", "g2", "g1"]
        );
        assert_eq!(
            target
                .folders
                .iter()
                .map(|f| f.id.as_str())
                .collect::<Vec<_>>(),
            vec!["f2", "f1"]
        );
    }

    #[test]
    fn local_history_and_conflict_backup_are_written() {
        let dir = tempfile::tempdir().unwrap();
        append_history(dir.path(), &json!({"at": "now", "value": 1}));
        let history = dir.path().join("iroh/history.jsonl");
        let text = std::fs::read_to_string(history).unwrap();
        assert!(text.contains(r#""value":1"#));
        backup_conflict(
            dir.path(),
            "card/a",
            &json!({"title": "local"}),
            &json!({"title": "remote"}),
        );
        let backups = std::fs::read_dir(crate::storage::backups_dir(dir.path()))
            .unwrap()
            .count();
        assert_eq!(backups, 1);
    }

    #[test]
    fn newer_record_wins_by_clock_then_device() {
        let mut ws = Workspace::default();
        let id = card(&mut ws, "old");
        let value = json!({"grid": ws.grids[0].id.clone(), "card": &ws.grids[0].cards[0]});
        let newer = Record {
            v: SCHEMA,
            device: "a".into(),
            clock: 5,
            deleted: false,
            value: Some(value.clone()),
            sealed: None,
        };
        let mut newer_card = serde_json::from_value::<Card>(value["card"].clone()).unwrap();
        newer_card.title = "new".into();
        let newer = Record {
            value: Some(json!({"grid": newer_card.id.clone(), "card": newer_card})),
            ..newer
        };
        let older = Record {
            v: SCHEMA,
            device: "z".into(),
            clock: 4,
            deleted: false,
            value: Some(value.clone()),
            sealed: None,
        };
        assert!(!older.newer_than(newer.clock, &newer.device));
        assert!(newer.newer_than(older.clock, &older.device));
        let _ = id;
    }
}
