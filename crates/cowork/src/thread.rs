use crate::{
    catalog::{CATALOG_STALE_AFTER, Catalog, CatalogEntry, ModelRef, POPULAR_PROVIDERS, Support},
    cowork_settings::CoworkSettings,
    provider::{Message, Role},
};
use anyhow::{Context as _, Result};
use db::kvp::KeyValueStore;
use collections::{HashMap, HashSet};
use editor::Editor;
use gpui::Focusable as _;
use gpui::{
    App, AppContext as _, Context, Entity, EventEmitter, Global, SharedString, Task, TaskExt as _,
    Window,
};
use http_client::HttpClient;
use serde::{Deserialize, Serialize};
use settings::{Settings as _, SettingsStore};
use std::{
    cmp::Reverse,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use util::ResultExt as _;

const KVP_NAMESPACE: &str = "cowork";
const INDEX_KEY: &str = "index";
const CATALOG_KEY: &str = "catalog";
const STORED_KEYS_KEY: &str = "providers_with_keys";
const PREVIEW_LENGTH: usize = 120;
const TITLE_LENGTH: usize = 48;

#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ThreadId(Arc<str>);

impl ThreadId {
    /// Thread ids only have to be unique within one user's local history, and they double as the
    /// suffix of the key the thread is stored under, so a sortable timestamp plus a counter is
    /// enough and keeps the keys human-readable when inspecting the database.
    fn new(sequence: u64) -> Self {
        Self(format!("{:013}-{sequence}", now_millis()).into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ThreadMetadata {
    pub id: ThreadId,
    pub title: String,
    pub model: ModelRef,
    pub created_at: u64,
    pub updated_at: u64,
    pub message_count: usize,
    pub preview: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Thread {
    pub metadata: ThreadMetadata,
    pub messages: Vec<Message>,
}

impl Thread {
    /// Keeps the summary that the panel lists in sync with the messages the thread view holds.
    pub fn refresh_metadata(&mut self) {
        self.metadata.message_count = self.messages.len();
        self.metadata.updated_at = now_seconds();

        let first_prompt = self
            .messages
            .iter()
            .find(|message| message.role == Role::User)
            .map(|message| message.text.as_str())
            .unwrap_or_default();

        if !first_prompt.trim().is_empty() {
            self.metadata.title = summarize(first_prompt, TITLE_LENGTH);
        }

        self.metadata.preview = self
            .messages
            .last()
            .map(|message| summarize(&message.text, PREVIEW_LENGTH))
            .unwrap_or_default();
    }
}

/// A provider as the settings UI draws it.
///
/// Every field is resolved once, when the catalog or the environment is read, and never during
/// `render`. The catalog carries 213 providers; recomputing display names, joining environment
/// variable lists and probing the process environment on every frame is what made the old panel
/// roster stutter.
#[derive(Clone, Debug)]
pub struct ProviderRow {
    pub id: SharedString,
    pub name: SharedString,
    /// The environment variables that would connect this provider, already joined for display.
    pub env_label: SharedString,
    pub model_count: usize,
    pub connected: bool,
    /// The credential came from the OS credential store rather than the environment.
    pub stored: bool,
    pub supported: bool,
}

/// A model of a connected provider, resolved once for the same reason as [`ProviderRow`].
#[derive(Clone, Debug)]
pub struct ModelRow {
    pub model: ModelRef,
    pub name: SharedString,
    pub provider_name: SharedString,
    pub detail: SharedString,
    /// Whether the model is offered in the model selector.
    pub enabled: bool,
}

/// An in-flight "set the API key for this provider" dialog.
///
/// The settings window is not a `Workspace`, so it has no modal layer to push onto. The dialog is
/// therefore drawn by the settings page itself, and its state lives here because the store is the
/// one thing both the panel and the settings window can reach.
pub struct PendingApiKey {
    pub provider_id: SharedString,
    pub provider_name: SharedString,
    pub env_label: SharedString,
    pub mode: ApiKeyMode,
    pub editor: Entity<Editor>,
    pub error: Option<SharedString>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ApiKeyMode {
    /// No key is stored for this provider yet.
    Connect,
    /// A key is stored; the dialog confirms removing it.
    Disconnect,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct CachedCatalog {
    fetched_at: u64,
    catalog: Catalog,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CatalogState {
    Idle,
    Loading,
    Loaded,
    Failed(String),
}

pub enum CoworkStoreEvent {
    ThreadsChanged,
    CatalogChanged,
}

pub struct CoworkStore {
    threads: Vec<ThreadMetadata>,
    catalog: Catalog,
    catalog_state: CatalogState,
    connected: HashSet<String>,
    /// API keys the user typed into Cowork, mirrored from the OS credential store. Keys read from
    /// the environment are never copied here.
    stored_keys: HashMap<String, String>,
    disabled_models: HashSet<String>,
    pending_api_key: Option<PendingApiKey>,
    provider_list: Arc<[ProviderRow]>,
    model_list: Arc<[ModelRow]>,
    key_value_store: KeyValueStore,
    http_client: Arc<dyn HttpClient>,
    next_sequence: u64,
    _load: Task<()>,
    _refresh: Task<()>,
    _settings: gpui::Subscription,
}

struct GlobalCoworkStore(Entity<CoworkStore>);

impl Global for GlobalCoworkStore {}

impl EventEmitter<CoworkStoreEvent> for CoworkStore {}

impl CoworkStore {
    pub fn global(cx: &App) -> Option<Entity<Self>> {
        cx.try_global::<GlobalCoworkStore>()
            .map(|global| global.0.clone())
    }

    pub fn set_global(store: Entity<Self>, cx: &mut App) {
        cx.set_global(GlobalCoworkStore(store));
    }

    pub fn new(cx: &mut Context<Self>) -> Self {
        let key_value_store = KeyValueStore::global(cx);
        let http_client = cx.http_client();

        let load = cx.spawn({
            let key_value_store = key_value_store.clone();
            async move |this, cx| {
                let loaded = cx
                    .background_spawn(async move { read_index(&key_value_store) })
                    .await;

                this.update(cx, |this, cx| {
                    this.load_stored_keys(cx);
                    this.threads = loaded;
                    this.next_sequence = this.threads.len() as u64;
                    cx.emit(CoworkStoreEvent::ThreadsChanged);
                    cx.notify();
                })
                .log_err();
            }
        });

        let settings_subscription = cx.observe_global::<SettingsStore>(|this: &mut Self, cx| {
            this.rebuild_rows(cx);
            cx.emit(CoworkStoreEvent::CatalogChanged);
            cx.notify();
        });

        Self {
            threads: Vec::new(),
            catalog: Catalog::default(),
            catalog_state: CatalogState::Idle,
            connected: HashSet::default(),
            stored_keys: HashMap::default(),
            disabled_models: HashSet::default(),
            pending_api_key: None,
            provider_list: Arc::from([]),
            model_list: Arc::from([]),
            key_value_store,
            http_client,
            next_sequence: 0,
            _load: load,
            _refresh: Task::ready(()),
            _settings: settings_subscription,
        }
    }

    pub fn threads(&self) -> &[ThreadMetadata] {
        &self.threads
    }

    pub fn catalog(&self) -> &Catalog {
        &self.catalog
    }

    pub fn catalog_state(&self) -> &CatalogState {
        &self.catalog_state
    }

    pub fn catalog_entries(&self) -> Vec<CatalogEntry> {
        self.catalog
            .entries(|provider_id| self.connected.contains(provider_id))
            .into_iter()
            .filter(|entry| !self.disabled_models.contains(&entry.model_ref.qualified()))
            .collect()
    }

    /// The credential to authenticate a request with. A key typed into Cowork wins over the
    /// environment, so setting one in the UI takes effect without restarting Wu.
    pub fn api_key(&self, provider_id: &str) -> Option<String> {
        if let Some(key) = self.stored_keys.get(provider_id) {
            return Some(key.clone());
        }
        let provider = self.catalog.providers.get(provider_id)?;
        provider.env_vars().iter().find_map(|name| credential(name))
    }

    pub fn pending_api_key(&self) -> Option<&PendingApiKey> {
        self.pending_api_key.as_ref()
    }

    /// A provider with a stored key opens a confirmation to remove it; anything else opens the
    /// dialog to add one.
    pub fn begin_api_key(&mut self, row: &ProviderRow, window: &mut Window, cx: &mut Context<Self>) {
        let mode = if row.stored {
            ApiKeyMode::Disconnect
        } else {
            ApiKeyMode::Connect
        };

        let editor = cx.new(|cx| {
            let mut editor = Editor::single_line(window, cx);
            editor.set_masked(true, cx);
            editor.set_placeholder_text("Paste the API key", window, cx);
            editor
        });
        if mode == ApiKeyMode::Connect {
            window.focus(&editor.focus_handle(cx), cx);
        }

        self.pending_api_key = Some(PendingApiKey {
            provider_id: row.id.clone(),
            provider_name: row.name.clone(),
            env_label: row.env_label.clone(),
            mode,
            editor,
            error: None,
        });
        cx.notify();
    }

    /// Forgets the stored key for the provider named by the open dialog.
    pub fn remove_api_key(&mut self, cx: &mut Context<Self>) {
        let Some(pending) = self.pending_api_key.as_ref() else {
            return;
        };
        let provider_id = pending.provider_id.to_string();
        self.stored_keys.remove(&provider_id);
        cx.delete_credentials(&credential_url(&provider_id)).detach();

        self.pending_api_key = None;
        self.persist_stored_key_index(cx);
        self.rebuild_rows(cx);
        cx.emit(CoworkStoreEvent::CatalogChanged);
        cx.notify();
    }

    /// Shows or hides a model in the selector by writing `cowork.disabled_models`.
    pub fn set_model_enabled(
        &mut self,
        model: &ModelRef,
        enabled: bool,
        fs: Arc<dyn fs::Fs>,
        cx: &mut Context<Self>,
    ) {
        let qualified = model.qualified();
        settings::update_settings_file(fs, cx, move |settings, _| {
            let disabled = settings
                .cowork
                .get_or_insert_default()
                .disabled_models
                .get_or_insert_default();
            if enabled {
                disabled.retain(|entry| entry != &qualified);
            } else if !disabled.contains(&qualified) {
                disabled.push(qualified);
            }
        });
    }

    pub fn cancel_api_key(&mut self, cx: &mut Context<Self>) {
        self.pending_api_key = None;
        cx.notify();
    }

    /// Saves the typed key to the OS credential store, or removes it when the field is empty.
    pub fn submit_api_key(&mut self, cx: &mut Context<Self>) {
        let Some(pending) = self.pending_api_key.as_ref() else {
            return;
        };
        let provider_id = pending.provider_id.to_string();
        let key = pending.editor.read(cx).text(cx).trim().to_owned();
        let url = credential_url(&provider_id);

        if key.is_empty() {
            self.stored_keys.remove(&provider_id);
            cx.delete_credentials(&url).detach();
        } else {
            self.stored_keys.insert(provider_id.clone(), key.clone());
            let write = cx.write_credentials(&url, &provider_id, key.as_bytes());
            cx.spawn(async move |this, cx| {
                if let Err(error) = write.await {
                    log::warn!("cowork: could not store the API key: {error:#}");
                    this.update(cx, |this, cx| {
                        // The key still works for this session; only persistence failed.
                        if let Some(pending) = this.pending_api_key.as_mut() {
                            pending.error = Some(format!("{error:#}").into());
                            cx.notify();
                        }
                    })
                    .log_err();
                }
            })
            .detach();
        }

        self.pending_api_key = None;
        self.persist_stored_key_index(cx);
        self.rebuild_rows(cx);
        cx.emit(CoworkStoreEvent::CatalogChanged);
        cx.notify();
    }

    fn persist_stored_key_index(&self, cx: &mut Context<Self>) {
        let ids = self.stored_keys.keys().cloned().collect::<Vec<_>>();
        let key_value_store = self.key_value_store.clone();
        cx.background_spawn(async move {
            let raw = serde_json::to_string(&ids)?;
            key_value_store
                .scoped(KVP_NAMESPACE)
                .write(STORED_KEYS_KEY.to_owned(), raw)
                .await
        })
        .detach_and_log_err(cx);
    }

    /// Reads back the keys typed in previous sessions. Only the providers listed in the index are
    /// touched, so this is a handful of credential-store reads rather than one per catalog entry.
    fn load_stored_keys(&mut self, cx: &mut Context<Self>) {
        let key_value_store = self.key_value_store.clone();
        cx.spawn(async move |this, cx| {
            let ids = cx
                .background_spawn(async move {
                    key_value_store
                        .scoped(KVP_NAMESPACE)
                        .read(STORED_KEYS_KEY)
                        .ok()
                        .flatten()
                        .and_then(|raw| serde_json::from_str::<Vec<String>>(&raw).ok())
                        .unwrap_or_default()
                })
                .await;

            for id in ids {
                let read = cx.update(|cx| cx.read_credentials(&credential_url(&id)));
                let Ok(Some((_, key))) = read.await else {
                    continue;
                };

                let Ok(key) = String::from_utf8(key) else {
                    continue;
                };
                if this
                    .update(cx, |this, _| {
                        this.stored_keys.insert(id.clone(), key);
                    })
                    .is_err()
                {
                    return;
                }
            }

            this.update(cx, |this, cx| {
                this.rebuild_rows(cx);
                cx.emit(CoworkStoreEvent::CatalogChanged);
                cx.notify();
            })
            .log_err();
        })
        .detach();
    }

    pub fn is_connected(&self, provider_id: &str) -> bool {
        self.connected.contains(provider_id)
    }

    pub fn connected_count(&self) -> usize {
        self.connected.len()
    }

    /// Provider rows ordered connected first, then the curated popular set, then the rest.
    pub fn provider_list(&self) -> Arc<[ProviderRow]> {
        self.provider_list.clone()
    }

    /// Model rows grouped by provider. Only connected providers appear.
    pub fn model_list(&self) -> Arc<[ModelRow]> {
        self.model_list.clone()
    }

    /// Re-reads the environment and rebuilds the cached rows. Cheap enough to call when the
    /// settings window opens, and never called from `render`.
    pub fn refresh_connections(&mut self, cx: &mut Context<Self>) {
        self.rebuild_rows(cx);
        cx.emit(CoworkStoreEvent::CatalogChanged);
        cx.notify();
    }

    fn rebuild_rows(&mut self, cx: &App) {
        self.disabled_models = CoworkSettings::get_global(cx)
            .disabled_models
            .iter()
            .cloned()
            .collect();

        self.connected = self
            .catalog
            .providers
            .iter()
            .filter(|(id, provider)| {
                self.stored_keys.contains_key(*id)
                    || provider
                        .env_vars()
                        .iter()
                        .any(|name| credential_is_present(name))
            })
            .map(|(id, _)| id.clone())
            .collect();

        let popular_rank = |id: &str| POPULAR_PROVIDERS.iter().position(|entry| *entry == id);

        let mut rows = self
            .catalog
            .providers
            .iter()
            .filter(|(_, provider)| !provider.models.is_empty())
            .map(|(id, provider)| ProviderRow {
                id: id.clone().into(),
                name: provider.display_name(id).into(),
                env_label: if provider.env_vars().is_empty() {
                    SharedString::new_static("no key required")
                } else {
                    provider.env_vars().join(" or ").into()
                },
                model_count: provider.models.len(),
                connected: self.connected.contains(id),
                stored: self.stored_keys.contains_key(id),
                supported: provider.support() == Support::Supported,
            })
            .collect::<Vec<_>>();
        rows.sort_by_key(|row| row.name.to_lowercase());

        let (connected, rest): (Vec<_>, Vec<_>) = rows.into_iter().partition(|row| row.connected);
        let (mut popular, other): (Vec<_>, Vec<_>) =
            rest.into_iter().partition(|row| popular_rank(&row.id).is_some());
        popular.sort_by_key(|row| popular_rank(&row.id).unwrap_or(usize::MAX));

        let mut list = connected;
        list.extend(popular);
        list.extend(other);
        self.provider_list = Arc::from(list);

        let mut models = Vec::new();
        let mut provider_names = self
            .connected
            .iter()
            .filter_map(|id| {
                let provider = self.catalog.providers.get(id)?;
                (provider.support() == Support::Supported)
                    .then(|| (id.clone(), provider.display_name(id)))
            })
            .collect::<Vec<_>>();
        provider_names.sort_by_key(|(_, name)| name.to_lowercase());

        for (id, name) in provider_names {
            let Some(provider) = self.catalog.providers.get(&id) else {
                continue;
            };
            let mut entries = provider
                .models
                .iter()
                .filter(|(_, model)| !model.is_deprecated())
                .map(|(model_key, model)| ModelRow {
                    model: ModelRef {
                        provider_id: id.clone(),
                        model_id: model_key.clone(),
                    },
                    name: model.display_name(model_key).into(),
                    provider_name: name.clone().into(),
                    detail: describe_model(model).into(),
                    enabled: !self
                        .disabled_models
                        .contains(&format!("{id}/{model_key}")),
                })
                .collect::<Vec<_>>();
            if entries.is_empty() {
                continue;
            }
            entries.sort_by_key(|row| row.name.to_lowercase());
            models.extend(entries);
        }
        self.model_list = Arc::from(models);
    }

    /// The model a new thread starts on: the one named in settings when the catalog knows it,
    /// otherwise the first entry whose credential is actually present in the environment.
    pub fn default_model(&self, cx: &App) -> Option<ModelRef> {
        let configured = CoworkSettings::get_global(cx).default_model.clone();
        // A configured model is only offered when its provider is actually connected; otherwise
        // the panel would name a model that every request would fail on.
        if let Some(model) = ModelRef::parse(&configured)
            && self.connected.contains(&model.provider_id)
            && self.catalog.model(&model).is_some()
        {
            return Some(model);
        }

        self.catalog_entries()
            .into_iter()
            .next()
            .map(|entry| entry.model_ref)
    }

    pub fn load_catalog(&mut self, force_refresh: bool, cx: &mut Context<Self>) {
        if self.catalog_state == CatalogState::Loading {
            return;
        }

        let url = CoworkSettings::get_global(cx).catalog_url.clone();
        let key_value_store = self.key_value_store.clone();
        let http_client = self.http_client.clone();

        self.catalog_state = CatalogState::Loading;
        cx.emit(CoworkStoreEvent::CatalogChanged);
        cx.notify();

        self._refresh = cx.spawn(async move |this, cx| {
            let cached = cx
                .background_spawn({
                    let key_value_store = key_value_store.clone();
                    async move { read_cached_catalog(&key_value_store) }
                })
                .await;

            let is_fresh = cached
                .as_ref()
                .is_some_and(|cached| age_of(cached.fetched_at) < CATALOG_STALE_AFTER);

            if let Some(cached) = cached.clone() {
                this.update(cx, |this, cx| {
                    this.catalog = cached.catalog;
                    this.catalog_state = CatalogState::Loaded;
                    this.rebuild_rows(cx);
                    cx.emit(CoworkStoreEvent::CatalogChanged);
                    cx.notify();
                })
                .log_err();

                if is_fresh && !force_refresh {
                    return;
                }
            }

            let fetched = crate::catalog::fetch(http_client, &url).await;

            match fetched {
                Ok(catalog) => {
                    let cache = CachedCatalog {
                        fetched_at: now_seconds(),
                        catalog: catalog.clone(),
                    };
                    cx.background_spawn(
                        async move { write_cached_catalog(&key_value_store, &cache).await },
                    )
                    .await
                    .log_err();

                    this.update(cx, |this, cx| {
                        this.catalog = catalog;
                        this.catalog_state = CatalogState::Loaded;
                        this.rebuild_rows(cx);
                        cx.emit(CoworkStoreEvent::CatalogChanged);
                        cx.notify();
                    })
                    .log_err();
                }
                Err(error) => {
                    log::warn!("cowork: failed to load the models.dev catalog: {error:#}");
                    this.update(cx, |this, cx| {
                        // A stale cache is still usable, so a failed refresh must not discard it.
                        if cached.is_none() {
                            this.catalog_state = CatalogState::Failed(format!("{error:#}"));
                        }
                        cx.emit(CoworkStoreEvent::CatalogChanged);
                        cx.notify();
                    })
                    .log_err();
                }
            }
        });
    }

    pub fn create_thread(&mut self, model: ModelRef, cx: &mut Context<Self>) -> Thread {
        self.next_sequence = self.next_sequence.wrapping_add(1);
        let now = now_seconds();
        let thread = Thread {
            metadata: ThreadMetadata {
                id: ThreadId::new(self.next_sequence),
                title: "New thread".to_owned(),
                model,
                created_at: now,
                updated_at: now,
                message_count: 0,
                preview: String::new(),
            },
            messages: Vec::new(),
        };

        self.threads.insert(0, thread.metadata.clone());
        cx.emit(CoworkStoreEvent::ThreadsChanged);
        cx.notify();

        thread
    }

    pub fn load_thread(&self, id: ThreadId, cx: &App) -> Task<Result<Thread>> {
        let key_value_store = self.key_value_store.clone();
        cx.background_spawn(async move { read_thread(&key_value_store, &id) })
    }

    pub fn save_thread(&mut self, thread: Thread, cx: &mut Context<Self>) {
        let metadata = thread.metadata.clone();
        match self
            .threads
            .iter_mut()
            .find(|existing| existing.id == metadata.id)
        {
            Some(existing) => *existing = metadata,
            None => self.threads.insert(0, metadata),
        }
        self.sort_threads();

        let key_value_store = self.key_value_store.clone();
        let index = self.threads.clone();
        cx.background_spawn(async move {
            write_thread(&key_value_store, &thread).await?;
            write_index(&key_value_store, &index).await
        })
        .detach_and_log_err(cx);

        cx.emit(CoworkStoreEvent::ThreadsChanged);
        cx.notify();
    }

    pub fn delete_thread(&mut self, id: ThreadId, cx: &mut Context<Self>) {
        self.threads.retain(|thread| thread.id != id);

        let key_value_store = self.key_value_store.clone();
        let index = self.threads.clone();
        cx.background_spawn(async move {
            delete_thread(&key_value_store, &id).await?;
            write_index(&key_value_store, &index).await
        })
        .detach_and_log_err(cx);

        cx.emit(CoworkStoreEvent::ThreadsChanged);
        cx.notify();
    }

    fn sort_threads(&mut self) {
        self.threads
            .sort_by_key(|thread| Reverse(thread.updated_at));
    }
}

fn thread_key(id: &ThreadId) -> String {
    format!("thread/{}", id.as_str())
}

fn read_index(key_value_store: &KeyValueStore) -> Vec<ThreadMetadata> {
    let Some(raw) = key_value_store
        .scoped(KVP_NAMESPACE)
        .read(INDEX_KEY)
        .context("reading the cowork thread index")
        .log_err()
        .flatten()
    else {
        return Vec::new();
    };

    match serde_json::from_str::<Vec<ThreadMetadata>>(&raw) {
        Ok(mut threads) => {
            threads.sort_by_key(|thread| Reverse(thread.updated_at));
            threads
        }
        Err(error) => {
            log::warn!("cowork: discarding an unreadable thread index: {error:#}");
            Vec::new()
        }
    }
}

async fn write_index(key_value_store: &KeyValueStore, index: &[ThreadMetadata]) -> Result<()> {
    let raw = serde_json::to_string(index).context("serializing the cowork thread index")?;
    key_value_store
        .scoped(KVP_NAMESPACE)
        .write(INDEX_KEY.to_owned(), raw)
        .await
        .context("writing the cowork thread index")
}

fn read_thread(key_value_store: &KeyValueStore, id: &ThreadId) -> Result<Thread> {
    let raw = key_value_store
        .scoped(KVP_NAMESPACE)
        .read(&thread_key(id))
        .context("reading a cowork thread")?
        .with_context(|| format!("cowork thread {} is no longer stored", id.as_str()))?;

    serde_json::from_str(&raw).context("parsing a cowork thread")
}

async fn write_thread(key_value_store: &KeyValueStore, thread: &Thread) -> Result<()> {
    let raw = serde_json::to_string(thread).context("serializing a cowork thread")?;
    key_value_store
        .scoped(KVP_NAMESPACE)
        .write(thread_key(&thread.metadata.id), raw)
        .await
        .context("writing a cowork thread")
}

async fn delete_thread(key_value_store: &KeyValueStore, id: &ThreadId) -> Result<()> {
    key_value_store
        .scoped(KVP_NAMESPACE)
        .delete(thread_key(id))
        .await
        .context("deleting a cowork thread")
}

fn read_cached_catalog(key_value_store: &KeyValueStore) -> Option<CachedCatalog> {
    let raw = key_value_store
        .scoped(KVP_NAMESPACE)
        .read(CATALOG_KEY)
        .context("reading the cached models.dev catalog")
        .log_err()
        .flatten()?;

    serde_json::from_str(&raw)
        .context("parsing the cached models.dev catalog")
        .log_err()
}

async fn write_cached_catalog(
    key_value_store: &KeyValueStore,
    cache: &CachedCatalog,
) -> Result<()> {
    let raw = serde_json::to_string(cache).context("serializing the models.dev catalog cache")?;
    key_value_store
        .scoped(KVP_NAMESPACE)
        .write(CATALOG_KEY.to_owned(), raw)
        .await
        .context("writing the models.dev catalog cache")
}

/// A one-line summary of a model's capabilities for the settings list.
fn describe_model(model: &crate::catalog::Model) -> String {
    let mut parts = Vec::new();
    if let Some(context) = model.limit.and_then(|limit| limit.context) {
        parts.push(format!("{}k context", context / 1000));
    }
    if model.reasoning {
        parts.push("reasoning".to_owned());
    }
    if model.tool_call {
        parts.push("tools".to_owned());
    }
    parts.join(" · ")
}

/// Where a provider's key lives in the OS credential store.
fn credential_url(provider_id: &str) -> String {
    format!("cowork://{provider_id}")
}

/// Reads a credential from the environment. They are read from the environment variable the
/// catalog declares for the provider, which is the same contract the AI SDK uses.
pub fn credential(env_var: &str) -> Option<String> {
    std::env::var(env_var)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

pub fn credential_is_present(env_var: &str) -> bool {
    credential(env_var).is_some()
}

pub fn now_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
}

fn age_of(timestamp: u64) -> Duration {
    Duration::from_secs(now_seconds().saturating_sub(timestamp))
}

/// Collapses a message down to a single line that fits in the panel's list.
fn summarize(text: &str, max_length: usize) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= max_length {
        return collapsed;
    }

    let truncated = collapsed
        .char_indices()
        .nth(max_length)
        .map(|(index, _)| &collapsed[..index])
        .unwrap_or(&collapsed);
    format!("{}…", truncated.trim_end())
}

/// A compact "when did this last change" label for the thread list.
pub fn format_age(timestamp: u64) -> String {
    let seconds = now_seconds().saturating_sub(timestamp);
    match seconds {
        0..=59 => "now".to_owned(),
        60..=3599 => format!("{}m", seconds / 60),
        3600..=86399 => format!("{}h", seconds / 3600),
        86400..=2591999 => format!("{}d", seconds / 86400),
        _ => format!("{}mo", seconds / 2592000),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thread_with(messages: Vec<Message>) -> Thread {
        Thread {
            metadata: ThreadMetadata {
                id: ThreadId("test".into()),
                title: "New thread".to_owned(),
                model: ModelRef {
                    provider_id: "anthropic".to_owned(),
                    model_id: "claude-sonnet-4-5".to_owned(),
                },
                created_at: 0,
                updated_at: 0,
                message_count: 0,
                preview: String::new(),
            },
            messages,
        }
    }

    #[test]
    fn metadata_titles_a_thread_from_its_first_prompt() {
        let mut thread = thread_with(vec![
            Message {
                role: Role::User,
                text: "  Explain   the  borrow checker\n".to_owned(),
            },
            Message {
                role: Role::Assistant,
                text: "It tracks lifetimes.".to_owned(),
            },
        ]);

        thread.refresh_metadata();

        assert_eq!(thread.metadata.title, "Explain the borrow checker");
        assert_eq!(thread.metadata.preview, "It tracks lifetimes.");
        assert_eq!(thread.metadata.message_count, 2);
    }

    #[test]
    fn metadata_keeps_the_placeholder_title_until_a_prompt_arrives() {
        let mut thread = thread_with(Vec::new());

        thread.refresh_metadata();

        assert_eq!(thread.metadata.title, "New thread");
        assert_eq!(thread.metadata.preview, "");
    }

    #[test]
    fn summaries_are_truncated_on_character_boundaries() {
        let summary = summarize("ação ação ação ação ação ação", 10);

        assert!(summary.ends_with('…'), "got: {summary}");
        assert!(summary.chars().count() <= 11, "got: {summary}");
    }

    #[test]
    fn ages_read_as_compact_units() {
        let now = now_seconds();

        assert_eq!(format_age(now), "now");
        assert_eq!(format_age(now.saturating_sub(120)), "2m");
        assert_eq!(format_age(now.saturating_sub(7200)), "2h");
        assert_eq!(format_age(now.saturating_sub(172800)), "2d");
    }

    #[test]
    fn thread_ids_are_unique_per_sequence() {
        assert_ne!(ThreadId::new(1), ThreadId::new(2));
    }
}
