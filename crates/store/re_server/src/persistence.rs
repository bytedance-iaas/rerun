//! Catalog persistence: catalog mutations recorded in `SQLite`, replayed at startup.
//!
//! The native server keeps its catalog purely in memory; a restart loses every dataset entry and
//! registration. This module records each successful catalog mutation (dataset created, sources
//! registered, sources unregistered, entry deleted) as a row in the `catalog_ops` table of a
//! `SQLite` database on the server's data directory, and replays them when the server boots —
//! recreating entries under their original ids, so client-side references stay valid across
//! restarts.
//!
//! Storing *operations* (not materialized state) means replay goes through the server's own
//! handler code: the recovered state is computed by the same logic that produced it, so the
//! database can never disagree with server semantics. Materialized state tables (and skipping
//! replay) are a straightforward later evolution if the catalog ever grows large.
//!
//! The database is owned exclusively by the server process. It also hosts the (phase 2)
//! `conversions` table, which viewers will populate through a server HTTP API with the locations
//! of `LeRobot` episodes they converted to rrd and wrote back to the object store.
//!
//! The data itself was never the volatile part: registered sources live in local files or in the
//! remote object store (with a local cache, see [`crate::cloud_storage`]). Only `memory://`
//! sources (data written into the server via the SDK) cannot be replayed and are skipped with a
//! warning.
//!
//! Enabled by starting the server with `--data-dir` (or `RERUN_SERVER_DATA_DIR`).

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use re_log_types::EntryId;
use re_protos::cloud::v1alpha1::ext;
use re_protos::common::v1alpha1::ext::IfDuplicateBehavior;
use url::Url;

use crate::store::InMemoryStore;

/// The data dir chosen via `--data-dir`, so that other modules (the remote-file cache) share it.
static DATA_DIR: OnceLock<PathBuf> = OnceLock::new();

pub(crate) fn configured_data_dir() -> Option<PathBuf> {
    DATA_DIR.get().cloned()
}

/// One catalog mutation, stored as one JSON value per `catalog_ops` row.
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum LedgerOp {
    CreateDataset {
        id: String,
        name: String,
    },
    Register {
        dataset_id: String,
        on_duplicate: String,
        sources: Vec<LedgerSource>,
    },
    Unregister {
        dataset_id: String,

        /// Empty means "all segments".
        segment_ids: Vec<String>,

        /// Empty means "all layers".
        layers: Vec<String>,
    },
    DeleteEntry {
        id: String,
    },
}

#[derive(serde::Serialize, serde::Deserialize)]
pub struct LedgerSource {
    pub url: String,
    pub layer: String,

    /// The segment this source registered as (from the registration result). Lets tooling
    /// (e.g. the `/catalog/sources` endpoint) reconstruct exact per-segment source lists.
    #[serde(default)]
    pub segment: String,
}

/// The server's persistence database. Sole owner of the `SQLite` file.
pub struct Ledger {
    connection: parking_lot::Mutex<rusqlite::Connection>,
}

impl Ledger {
    pub fn new(data_dir: &Path) -> anyhow::Result<Self> {
        std::fs::create_dir_all(data_dir)?;
        let db_path = data_dir.join("rerun-server.sqlite");
        let connection = rusqlite::Connection::open(&db_path)?;

        // WAL keeps appends cheap and readers (future admin tooling) unblocked.
        connection.pragma_update(None, "journal_mode", "WAL")?;

        connection.execute_batch(
            "CREATE TABLE IF NOT EXISTS catalog_ops (
                 id INTEGER PRIMARY KEY AUTOINCREMENT,
                 created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
                 op TEXT NOT NULL
             );
             -- Phase 2 (viewer rrd write-back): locations of LeRobot episodes converted to rrd
             -- and uploaded to the object store, reported by viewers via the server HTTP API.
             CREATE TABLE IF NOT EXISTS conversions (
                 source_fingerprint TEXT NOT NULL,
                 source_url TEXT NOT NULL,
                 episode INTEGER NOT NULL,
                 converter_version TEXT NOT NULL,
                 rrd_url TEXT NOT NULL,
                 created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
                 PRIMARY KEY (source_fingerprint, episode, converter_version)
             );",
        )?;

        Ok(Self {
            connection: parking_lot::Mutex::new(connection),
        })
    }

    /// Record one mutation. Failures are logged, not propagated: the client's operation already
    /// succeeded in memory, and failing it retroactively would leave the two sides disagreeing.
    pub fn append(&self, op: &LedgerOp) {
        let result = serde_json::to_string(op)
            .map_err(anyhow::Error::from)
            .and_then(|json| {
                self.connection
                    .lock()
                    .execute("INSERT INTO catalog_ops (op) VALUES (?1)", [&json])
                    .map_err(anyhow::Error::from)
            });
        if let Err(err) = result {
            re_log::error!(
                "Failed to record a catalog mutation — it will be lost on restart: {err:#}"
            );
        }
    }

    fn read_all(&self) -> anyhow::Result<Vec<LedgerOp>> {
        let connection = self.connection.lock();
        let mut statement = connection.prepare("SELECT id, op FROM catalog_ops ORDER BY id")?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?;

        let mut ops = Vec::new();
        for row in rows {
            let (row_id, json) = row?;
            match serde_json::from_str::<LedgerOp>(&json) {
                Ok(op) => ops.push(op),
                Err(err) => {
                    // A malformed row (e.g. written by a future version) skips that mutation only.
                    re_log::warn!("Skipping unreadable catalog_ops row {row_id}: {err:#}");
                }
            }
        }
        Ok(ops)
    }
}

pub fn if_duplicate_to_str(behavior: IfDuplicateBehavior) -> &'static str {
    match behavior {
        IfDuplicateBehavior::Error => "error",
        IfDuplicateBehavior::Skip => "skip",
        IfDuplicateBehavior::Overwrite => "overwrite",
    }
}

fn if_duplicate_from_str(s: &str) -> IfDuplicateBehavior {
    match s {
        "skip" => IfDuplicateBehavior::Skip,
        "overwrite" => IfDuplicateBehavior::Overwrite,
        _ => IfDuplicateBehavior::Error,
    }
}

/// One row of [`Ledger::dataset_sources`]: where a segment layer's data came from.
#[derive(serde::Serialize)]
pub struct SourceRow {
    pub segment: String,
    pub layer: String,
    pub url: String,
}

impl Ledger {
    /// The current external sources of a dataset, by name: exactly the (segment, layer, url)
    /// triples still registered, folded from the mutation log.
    ///
    /// Serves the read-only `/catalog/sources` HTTP endpoint, which training-side tooling uses
    /// to fetch data straight from the object store instead of streaming it through this server.
    /// Returns `None` for unknown dataset names. `memory://` sources are never recorded, so they
    /// never show up here.
    pub fn dataset_sources(&self, dataset_name: &str) -> Option<(String, Vec<SourceRow>)> {
        let ops = match self.read_all() {
            Ok(ops) => ops,
            Err(err) => {
                re_log::error!("Failed to read the catalog database: {err:#}");
                return None;
            }
        };

        // Fold the log: dataset ids by name, live (segment, layer) → url per dataset.
        let mut id_by_name: std::collections::HashMap<String, String> = Default::default();
        let mut sources: std::collections::HashMap<String, Vec<LedgerSource>> = Default::default();

        for op in ops {
            match op {
                LedgerOp::CreateDataset { id, name } => {
                    id_by_name.insert(name, id);
                }
                LedgerOp::Register {
                    dataset_id,
                    sources: new_sources,
                    on_duplicate: _,
                } => {
                    let existing = sources.entry(dataset_id).or_default();
                    for source in new_sources {
                        // Re-registration of the same (segment, layer) replaces the old row
                        // (matches both skip — same url — and overwrite semantics closely
                        // enough for source listing).
                        existing
                            .retain(|s| !(s.segment == source.segment && s.layer == source.layer));
                        existing.push(source);
                    }
                }
                LedgerOp::Unregister {
                    dataset_id,
                    segment_ids,
                    layers,
                } => {
                    if let Some(existing) = sources.get_mut(&dataset_id) {
                        // Empty list means "all" (proto convention).
                        existing.retain(|s| {
                            let segment_hit =
                                segment_ids.is_empty() || segment_ids.contains(&s.segment);
                            let layer_hit = layers.is_empty() || layers.contains(&s.layer);
                            !(segment_hit && layer_hit)
                        });
                    }
                }
                LedgerOp::DeleteEntry { id } => {
                    sources.remove(&id);
                    id_by_name.retain(|_, dataset_id| dataset_id != &id);
                }
            }
        }

        let dataset_id = id_by_name.get(dataset_name)?.clone();
        let rows = sources
            .remove(&dataset_id)
            .unwrap_or_default()
            .into_iter()
            .map(|s| SourceRow {
                segment: s.segment,
                layer: s.layer,
                url: s.url,
            })
            .collect();
        Some((dataset_id, rows))
    }
}

/// Open (or create) the database in `data_dir` and replay recorded mutations into the store.
///
/// Individual mutations that fail to replay (source gone from the object store, malformed ids, …)
/// are logged and skipped, so one bad entry never takes the whole catalog down.
pub async fn open_and_replay(data_dir: &Path, store: &mut InMemoryStore) -> anyhow::Result<Ledger> {
    DATA_DIR.set(data_dir.to_path_buf()).ok();

    let ledger = Ledger::new(data_dir)?;
    let ops = ledger.read_all()?;
    if ops.is_empty() {
        re_log::info!("Catalog persistence enabled (empty catalog)\nData dir: {data_dir:?}");
        return Ok(ledger);
    }

    let total = ops.len();
    let mut applied = 0usize;
    for op in ops {
        match replay_one(store, op).await {
            Ok(()) => applied += 1,
            Err(err) => re_log::warn!("Skipping catalog mutation that failed to replay: {err:#}"),
        }
    }
    re_log::info!("Catalog restored: {applied}/{total} mutations replayed\nData dir: {data_dir:?}");
    Ok(ledger)
}

async fn replay_one(store: &mut InMemoryStore, op: LedgerOp) -> anyhow::Result<()> {
    match op {
        LedgerOp::CreateDataset { id, name } => {
            let entry_id: EntryId = id
                .parse()
                .map_err(|err| anyhow::anyhow!("invalid dataset id {id:?}: {err}"))?;
            let name = re_log_types::EntryName::new(name)
                .map_err(|err| anyhow::anyhow!("invalid dataset name: {err}"))?;
            store.create_dataset(name, Some(entry_id))?;
        }

        LedgerOp::Register {
            dataset_id,
            on_duplicate,
            sources,
        } => {
            let entry_id: EntryId = dataset_id
                .parse()
                .map_err(|err| anyhow::anyhow!("invalid dataset id {dataset_id:?}: {err}"))?;

            // One rrd can register several segments → several rows with the same (url, layer);
            // for replay each (url, layer) only needs registering once.
            let mut seen = std::collections::HashSet::new();
            let data_sources: Vec<ext::DataSource> = sources
                .into_iter()
                .filter(|source| seen.insert((source.url.clone(), source.layer.clone())))
                .filter_map(|source| {
                    let url = Url::parse(&source.url)
                        .inspect_err(|err| {
                            re_log::warn!(
                                "Skipping recorded source with invalid URL {:?}: {err}",
                                source.url
                            );
                        })
                        .ok()?;
                    let layer = re_types_core::LayerName::try_new(source.layer.clone())
                        .unwrap_or_else(|_| re_types_core::LayerName::base());
                    Some(ext::DataSource {
                        storage_url: url,
                        is_prefix: false,
                        layer,
                        kind: ext::DataSourceKind::Rrd,
                    })
                })
                .collect();

            if data_sources.is_empty() {
                return Ok(());
            }

            // Replay tolerates re-runs over pre-existing state: whatever the original behavior,
            // an already-registered source is fine to skip.
            let on_duplicate = match if_duplicate_from_str(&on_duplicate) {
                IfDuplicateBehavior::Error => IfDuplicateBehavior::Skip,
                other => other,
            };

            crate::rerun_cloud::do_register_with_dataset(
                store,
                entry_id,
                data_sources,
                on_duplicate,
            )
            .await
            .map_err(|err| anyhow::anyhow!("register replay failed: {err}"))?;
        }

        LedgerOp::Unregister {
            dataset_id,
            segment_ids,
            layers,
        } => {
            use std::collections::HashSet;

            use re_protos::common::v1alpha1::ext::SegmentId;
            use re_types_core::LayerName;

            let entry_id: EntryId = dataset_id
                .parse()
                .map_err(|err| anyhow::anyhow!("invalid dataset id {dataset_id:?}: {err}"))?;

            let segment_ids: Vec<SegmentId> = segment_ids.into_iter().map(SegmentId::new).collect();
            let layers: Vec<LayerName> = layers
                .into_iter()
                .filter_map(|layer| LayerName::try_new(layer).ok())
                .collect();

            let segments_to_drop: Option<HashSet<&SegmentId>> =
                (!segment_ids.is_empty()).then(|| segment_ids.iter().collect());
            let layers_to_drop: Option<HashSet<&LayerName>> =
                (!layers.is_empty()).then(|| layers.iter().collect());

            let dataset = store.dataset_mut(entry_id)?;
            dataset
                .remove_layers(segments_to_drop.as_ref(), layers_to_drop.as_ref())
                .await?;
            store.cleanup_store_pool();
        }

        LedgerOp::DeleteEntry { id } => {
            let entry_id: EntryId = id
                .parse()
                .map_err(|err| anyhow::anyhow!("invalid entry id {id:?}: {err}"))?;
            store.delete_entry(entry_id)?;
        }
    }
    Ok(())
}
