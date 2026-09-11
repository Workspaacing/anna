use crate::{
    catalog::{CATALOG_STALE_AFTER, Catalog, CatalogEntry, ModelRef},
    cowork_settings::CoworkSettings,
    provider::{Message, Role},
};
use anyhow::{Context as _, Result};
use db::kvp::KeyValueStore;
use gpui::{App, AppContext as _, Context, Entity, EventEmitter, Global, Task, TaskExt as _};
use http_client::HttpClient;
use serde::{Deserialize, Serialize};
use settings::Settings as _;
use std::{
    cmp::Reverse,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use util::ResultExt as _;

const KVP_NAMESPACE: &str = "cowork";
const INDEX_KEY: &str = "index";
const CATALOG_KEY: &str = "catalog";
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
    key_value_store: KeyValueStore,
    http_client: Arc<dyn HttpClient>,
    next_sequence: u64,
    _load: Task<()>,
    _refresh: Task<()>,
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
                    this.threads = loaded;
                    this.next_sequence = this.threads.len() as u64;
                    cx.emit(CoworkStoreEvent::ThreadsChanged);
                    cx.notify();
                })
                .log_err();
            }
        });

        Self {
            threads: Vec::new(),
            catalog: Catalog::default(),
            catalog_state: CatalogState::Idle,
            key_value_store,
            http_client,
            next_sequence: 0,
            _load: load,
            _refresh: Task::ready(()),
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
        self.catalog.entries()
    }

    /// The model a new thread starts on: the one named in settings when the catalog knows it,
    /// otherwise the first entry whose credential is actually present in the environment.
    pub fn default_model(&self, cx: &App) -> Option<ModelRef> {
        let configured = CoworkSettings::get_global(cx).default_model.clone();
        if let Some(model) = ModelRef::parse(&configured)
            && self.catalog.model(&model).is_some()
        {
            return Some(model);
        }

        self.catalog_entries()
            .into_iter()
            .find(|entry| {
                entry
                    .env_var
                    .as_deref()
                    .is_some_and(|name| credential_is_present(name))
            })
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

/// Cowork never stores provider credentials. They are read from the environment variable the
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
