//! Remote reading of Lance-format `LeRobot` datasets, presented as classic v2/v3 datasets.
//!
//! Two layers:
//!
//! 1. [`DatasetStoreObjectStore`] adapts a [`DatasetStore`] (TOS, Hugging Face, …) to the
//!    [`object_store::ObjectStore`] interface lance reads through — so lance's own ranged reads
//!    go through the backend's credentials, proxy handling, and retries.
//! 2. [`LanceVirtualStore`] wraps a [`DatasetStore`] and presents the Lance dataset as a plain
//!    `LeRobot` v3 ([`LanceDatasetLayout::LeRobotMetaDir`]) or v2
//!    ([`LanceDatasetLayout::EpisodeTables`]) dataset: episode data parquet files are synthesized
//!    from the `frames` table, video "files" are served from the blob columns via ranged reads.
//!    The regular remote streaming driver (pause/prioritize/progress/rrd-artifacts) then runs on
//!    top, unchanged.
//!
//! Native-only: the `lance` crate does not compile to wasm.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;

use anyhow::Context as _;
use async_trait::async_trait;
use bytes::Bytes;
use futures_util::StreamExt as _;
use futures_util::TryStreamExt as _;
use futures_util::stream::BoxStream;
use object_store::{
    GetOptions, GetRange, GetResult, GetResultPayload, ListResult, ObjectMeta,
    path::Path as ObjPath,
};
use parking_lot::Mutex;

use re_lance::Table;
use re_lerobot::lance::LanceDatasetLayout;

use crate::lerobot_remote::{
    DatasetStore, DirListing, ListedFile, PauseState, fetch_full, fetch_range,
};

// ----------------------------------------------------------------------------
// DatasetStore → object_store adapter.

/// A read-only [`object_store::ObjectStore`] over a [`DatasetStore`], rooted at the dataset.
///
/// Paths are dataset-relative (e.g. `frames.lance/_versions/3.manifest`). Writes are rejected.
pub struct DatasetStoreObjectStore<S> {
    store: Arc<S>,

    /// Cached full listing — Lance datasets have few files, and lance lists `_versions/`
    /// on every table open.
    listing: Mutex<Option<Arc<Vec<ListedFile>>>>,
}

impl<S> DatasetStoreObjectStore<S> {
    pub fn new(store: Arc<S>) -> Self {
        Self {
            store,
            listing: Mutex::new(None),
        }
    }
}

impl<S> Clone for DatasetStoreObjectStore<S> {
    fn clone(&self) -> Self {
        Self {
            store: self.store.clone(),
            listing: Mutex::new(self.listing.lock().clone()),
        }
    }
}

impl<S> std::fmt::Display for DatasetStoreObjectStore<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DatasetStoreObjectStore")
    }
}

impl<S> std::fmt::Debug for DatasetStoreObjectStore<S> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DatasetStoreObjectStore")
    }
}

fn os_generic(err: anyhow::Error) -> object_store::Error {
    object_store::Error::Generic {
        store: "DatasetStoreObjectStore",
        source: err.into(),
    }
}

fn os_read_only(op: &str) -> object_store::Error {
    object_store::Error::NotSupported {
        source: format!("{op}: the LeRobot dataset store is read-only").into(),
    }
}

fn epoch() -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::from_timestamp(0, 0).unwrap_or_default()
}

fn object_meta(location: ObjPath, size: u64, e_tag: Option<String>) -> ObjectMeta {
    ObjectMeta {
        location,
        // The backends don't surface modification times; lance does not depend on them.
        last_modified: epoch(),
        size,
        e_tag,
        version: None,
    }
}

impl<S: DatasetStore + 'static> DatasetStoreObjectStore<S> {
    async fn listing(&self) -> object_store::Result<Arc<Vec<ListedFile>>> {
        if let Some(listing) = self.listing.lock().clone() {
            return Ok(listing);
        }
        let listing = Arc::new(self.store.list().await.map_err(os_generic)?);
        *self.listing.lock() = Some(listing.clone());
        Ok(listing)
    }
}

#[async_trait]
impl<S: DatasetStore + 'static> object_store::ObjectStore for DatasetStoreObjectStore<S> {
    async fn put_opts(
        &self,
        _location: &ObjPath,
        _payload: object_store::PutPayload,
        _opts: object_store::PutOptions,
    ) -> object_store::Result<object_store::PutResult> {
        Err(os_read_only("put"))
    }

    async fn put_multipart_opts(
        &self,
        _location: &ObjPath,
        _opts: object_store::PutMultipartOptions,
    ) -> object_store::Result<Box<dyn object_store::MultipartUpload>> {
        Err(os_read_only("put_multipart"))
    }

    async fn get_opts(
        &self,
        location: &ObjPath,
        options: GetOptions,
    ) -> object_store::Result<GetResult> {
        let rel = location.as_ref().to_owned();
        let size = self.store.file_size(&rel).await.map_err(|err| {
            // lance probes optional files (e.g. `_versions/latest_version_hint.json`); a clean
            // NotFound lets it fall back to listing instead of failing the open.
            if crate::lerobot_remote::http_status_of(&err) == Some(404) {
                object_store::Error::NotFound {
                    path: rel.clone(),
                    source: err.into(),
                }
            } else {
                os_generic(err)
            }
        })?;

        let meta = object_meta(location.clone(), size, None);

        if options.head {
            return Ok(GetResult {
                payload: GetResultPayload::Stream(futures_util::stream::empty().boxed()),
                meta,
                range: 0..0,
                attributes: Default::default(),
            });
        }

        let range: Range<u64> = match options.range {
            None => 0..size,
            Some(GetRange::Bounded(range)) => range.start..range.end.min(size),
            Some(GetRange::Offset(offset)) => offset..size,
            Some(GetRange::Suffix(n)) => size.saturating_sub(n)..size,
        };

        let bytes = fetch_range(
            self.store.as_ref(),
            &PauseState::default(),
            &rel,
            range.clone(),
        )
        .await
        .map_err(os_generic)?;

        Ok(GetResult {
            payload: GetResultPayload::Stream(
                futures_util::stream::once(async move { Ok(Bytes::from(bytes)) }).boxed(),
            ),
            meta,
            range,
            attributes: Default::default(),
        })
    }

    fn delete_stream(
        &self,
        locations: BoxStream<'static, object_store::Result<ObjPath>>,
    ) -> BoxStream<'static, object_store::Result<ObjPath>> {
        locations
            .map(|location| {
                location?;
                Err(os_read_only("delete"))
            })
            .boxed()
    }

    fn list(
        &self,
        prefix: Option<&ObjPath>,
    ) -> BoxStream<'static, object_store::Result<ObjectMeta>> {
        let prefix = prefix
            .map(|p| format!("{}/", p.as_ref().trim_end_matches('/')))
            .unwrap_or_default();
        let this = self.clone();
        futures_util::stream::once(async move {
            let listing = this.listing().await?;
            let metas: Vec<object_store::Result<ObjectMeta>> = listing
                .iter()
                .filter(|file| file.rel_path.starts_with(&prefix))
                .map(|file| {
                    Ok(object_meta(
                        ObjPath::from(file.rel_path.as_str()),
                        file.size,
                        file.content_id.clone(),
                    ))
                })
                .collect();
            Ok::<_, object_store::Error>(futures_util::stream::iter(metas))
        })
        .try_flatten()
        .boxed()
    }

    async fn list_with_delimiter(
        &self,
        prefix: Option<&ObjPath>,
    ) -> object_store::Result<ListResult> {
        let prefix = prefix
            .map(|p| format!("{}/", p.as_ref().trim_end_matches('/')))
            .unwrap_or_default();
        let listing = self.listing().await?;

        let mut objects = Vec::new();
        let mut common_prefixes = std::collections::BTreeSet::new();
        for file in listing.iter() {
            let Some(rest) = file.rel_path.strip_prefix(&prefix) else {
                continue;
            };
            match rest.split_once('/') {
                Some((dir, _)) => {
                    common_prefixes.insert(ObjPath::from(format!("{prefix}{dir}")));
                }
                None => objects.push(object_meta(
                    ObjPath::from(file.rel_path.as_str()),
                    file.size,
                    file.content_id.clone(),
                )),
            }
        }

        Ok(ListResult {
            common_prefixes: common_prefixes.into_iter().collect(),
            objects,
        })
    }

    async fn copy_opts(
        &self,
        _from: &ObjPath,
        _to: &ObjPath,
        _options: object_store::CopyOptions,
    ) -> object_store::Result<()> {
        Err(os_read_only("copy"))
    }
}

// ----------------------------------------------------------------------------
// The virtual classic-LeRobot view over the Lance tables.

/// One virtual file of the presented classic dataset.
enum VirtualFile {
    /// Contents known up front (synthesized metadata, pre-synthesized data parquet).
    Static(Bytes),

    /// An episode-data parquet synthesized on demand from the given `frames` rows.
    EpisodeParquet { offset: u64, len: u64 },

    /// A video "file" served from a Lance blob via ranged reads.
    Video {
        blob: Arc<re_lance::BlobHandle>,
        size: u64,
    },
}

/// A [`DatasetStore`] decorator presenting a Lance-format `LeRobot` dataset as a classic one.
pub struct LanceVirtualStore<S: DatasetStore> {
    inner: Arc<S>,

    files: HashMap<String, VirtualFile>,

    /// Synthesized-on-demand parquet contents, by path.
    parquet_cache: Mutex<HashMap<String, Bytes>>,

    frames: Table,

    /// Lance column name → original feature name (dots restored); layout A only.
    column_renames: HashMap<String, String>,

    /// Layout A: `meta/**` physically exists — reads fall through to the inner store,
    /// and these listing entries are real.
    meta_listing: Vec<ListedFile>,

    /// Identifies the dataset contents — the lance manifest versions. Feeds the
    /// per-file content ids (and thereby the rrd-artifacts fingerprints).
    content_version: String,
}

impl<S: DatasetStore + 'static> LanceVirtualStore<S> {
    /// Open the Lance dataset behind `inner` and build the virtual classic view.
    ///
    /// This reads the Lance table metadata (and, for the
    /// [`LanceDatasetLayout::LeRobotMetaDir`] layout, the full — small — `frames` table).
    pub async fn open(inner: Arc<S>, layout: LanceDatasetLayout) -> anyhow::Result<Self> {
        match layout {
            LanceDatasetLayout::LeRobotMetaDir => Self::open_meta_dir(inner).await,
            LanceDatasetLayout::EpisodeTables => Self::open_episode_tables(inner).await,
        }
    }

    /// Layout A (`lerobot-lancedb`): verbatim `meta/`, synthesized v3 data parquet, blob videos.
    async fn open_meta_dir(inner: Arc<S>) -> anyhow::Result<Self> {
        let pause = PauseState::default();

        let info_bytes = fetch_full(inner.as_ref(), &pause, "meta/info.json").await?;
        let mut info: serde_json::Value =
            serde_json::from_slice(&info_bytes).context("Failed to parse meta/info.json")?;
        let data_path_template = info
            .get("data_path")
            .and_then(|v| v.as_str())
            .context("meta/info.json has no data_path")?
            .to_owned();
        let video_path_template = info
            .get("video_path")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_owned();
        let column_renames = re_lance::meta_dir::column_renames_from_info(&info)?;

        let listing = inner.list().await?;
        let meta_listing: Vec<ListedFile> = listing
            .iter()
            .filter(|file| file.rel_path.starts_with("meta/"))
            .cloned()
            .collect();

        let mut episode_parquets = Vec::new();
        for file in &meta_listing {
            if file.rel_path.starts_with("meta/episodes/") && file.rel_path.ends_with(".parquet") {
                let bytes = fetch_full(inner.as_ref(), &pause, &file.rel_path).await?;
                episode_parquets.push(Bytes::from(bytes));
            }
        }
        let data_files = re_lance::meta_dir::data_files_from_episode_parquets(
            episode_parquets,
            &data_path_template,
        )?;

        let adapter = Arc::new(DatasetStoreObjectStore::new(inner.clone()));
        let frames = open_table(adapter.clone(), "frames.lance").await?;
        let videos = open_table(adapter, "videos.lance").await?;

        let videos_meta = videos
            .read_all_async(&["video_key", "chunk_index", "file_index"])
            .await?;
        let video_rows =
            re_lance::meta_dir::video_files_from_batch(&videos_meta, &video_path_template)?;

        let mut files = HashMap::new();

        // The virtual view is a *classic* v3 dataset: serve info.json without `storage_format`,
        // so the routing never tries to re-wrap it as a Lance dataset.
        if let Some(obj) = info.as_object_mut() {
            obj.remove("storage_format");
        }
        files.insert(
            "meta/info.json".to_owned(),
            VirtualFile::Static(Bytes::from(serde_json::to_vec(&info)?)),
        );

        let mut video_rows: Vec<(String, u64)> = video_rows.into_iter().collect();
        video_rows.sort_by(|a, b| a.0.cmp(&b.0));
        for (path, row) in video_rows {
            let blob = videos.blob_handle(row, "video_bytes").await?;
            let size = blob.size();
            files.insert(
                path,
                VirtualFile::Video {
                    blob: Arc::new(blob),
                    size,
                },
            );
        }

        // The data parquet files are synthesized eagerly: the v3 driver needs their exact sizes
        // from the listing before fetching, and the whole frames table is small (MBs) next to
        // the videos it indexes.
        let frame_columns = frames.column_names();
        let frame_column_refs: Vec<&str> = frame_columns.iter().map(|s| s.as_str()).collect();
        let mut data_files: Vec<_> = data_files.into_iter().collect();
        data_files.sort_by(|a, b| a.0.cmp(&b.0));
        for (path, rows) in &data_files {
            let batch = frames
                .read_rows_async(&frame_column_refs, rows.offset, rows.len)
                .await?;
            let batch = re_lance::meta_dir::rename_columns(&batch, &column_renames)?;
            files.insert(
                path.clone(),
                VirtualFile::Static(re_lance::meta_dir::write_parquet(&batch)?),
            );
        }

        let content_version = format!("lance:{}:{}", frames.version(), videos.version());

        Ok(Self {
            inner,
            files,
            parquet_cache: Mutex::new(HashMap::new()),
            frames,
            column_renames,
            meta_listing,
            content_version,
        })
    }

    /// Layout B (`lance-format` episode tables): everything synthesized, presented as v2.
    async fn open_episode_tables(inner: Arc<S>) -> anyhow::Result<Self> {
        use re_lance::episode_tables::{
            SynthFile, V2PlanInputs, episodes_meta_projection, plan_v2_files, video_blob_columns,
        };

        // The tables usually sit under `data/`, but may be at the root.
        let listing = inner.list().await?;
        let prefix = if listing
            .iter()
            .any(|file| file.rel_path.starts_with("data/frames.lance/"))
        {
            "data/"
        } else {
            ""
        };

        let adapter = Arc::new(DatasetStoreObjectStore::new(inner.clone()));
        let frames = open_table(adapter.clone(), &format!("{prefix}frames.lance")).await?;
        let episodes = open_table(adapter, &format!("{prefix}episodes.lance")).await?;

        let frame_columns = frames.column_names();
        let frame_column_refs: Vec<&str> = frame_columns.iter().map(|s| s.as_str()).collect();
        let frames_sample = frames.read_rows_async(&frame_column_refs, 0, 1).await?;
        let frame_episode_indices = frames.read_all_async(&["episode_index"]).await?;
        let total_frames = frames.num_rows_async().await?;

        let episode_columns = episodes.column_names();
        let video_columns = video_blob_columns(&episode_columns);
        let projection = episodes_meta_projection(&episode_columns);
        let projection_refs: Vec<&str> = projection.iter().map(|s| s.as_str()).collect();
        let episodes_meta = episodes.read_all_async(&projection_refs).await?;

        let plan = plan_v2_files(&V2PlanInputs {
            frames_sample,
            episodes_meta,
            frame_episode_indices,
            video_columns,
            total_frames,
        })?;

        let mut plan: Vec<_> = plan.into_iter().collect();
        plan.sort_by(|a, b| a.0.cmp(&b.0));

        let mut files = HashMap::new();
        for (path, synth) in plan {
            let file = match synth {
                SynthFile::Static(bytes) => VirtualFile::Static(bytes),
                SynthFile::EpisodeData { offset, len } => {
                    VirtualFile::EpisodeParquet { offset, len }
                }
                SynthFile::EpisodeVideo { row, column } => {
                    let blob = episodes.blob_handle(row, &column).await?;
                    let size = blob.size();
                    VirtualFile::Video {
                        blob: Arc::new(blob),
                        size,
                    }
                }
            };
            files.insert(path, file);
        }

        let content_version = format!("lance:{}:{}", frames.version(), episodes.version());

        Ok(Self {
            inner,
            files,
            parquet_cache: Mutex::new(HashMap::new()),
            frames,
            column_renames: HashMap::new(),
            meta_listing: Vec::new(),
            content_version,
        })
    }

    /// The synthesized parquet for an [`VirtualFile::EpisodeParquet`] entry.
    async fn episode_parquet(
        &self,
        rel_path: &str,
        offset: u64,
        len: u64,
    ) -> anyhow::Result<Bytes> {
        if let Some(cached) = self.parquet_cache.lock().get(rel_path) {
            return Ok(cached.clone());
        }

        let columns = self.frames.column_names();
        let column_refs: Vec<&str> = columns.iter().map(|s| s.as_str()).collect();
        let batch = self
            .frames
            .read_rows_async(&column_refs, offset, len)
            .await?;
        let batch = re_lance::meta_dir::rename_columns(&batch, &self.column_renames)?;
        let bytes = re_lance::meta_dir::write_parquet(&batch)?;

        self.parquet_cache
            .lock()
            .insert(rel_path.to_owned(), bytes.clone());
        Ok(bytes)
    }

    /// Size (synthesizing if needed) of one virtual file.
    async fn virtual_size(&self, rel_path: &str, file: &VirtualFile) -> anyhow::Result<u64> {
        Ok(match file {
            VirtualFile::Static(bytes) => bytes.len() as u64,
            VirtualFile::Video { size, .. } => *size,
            &VirtualFile::EpisodeParquet { offset, len } => {
                self.episode_parquet(rel_path, offset, len).await?.len() as u64
            }
        })
    }

    fn virtual_stat(&self, rel_path: &str, size: u64) -> ListedFile {
        ListedFile {
            rel_path: rel_path.to_owned(),
            size,
            content_id: Some(format!("{}#{rel_path}", self.content_version)),
        }
    }
}

impl<S: DatasetStore + 'static> DatasetStore for LanceVirtualStore<S> {
    fn url(&self) -> String {
        self.inner.url()
    }

    async fn list(&self) -> anyhow::Result<Vec<ListedFile>> {
        // Physically existing files, except where a virtual file overrides them (info.json).
        let mut out: Vec<ListedFile> = self
            .meta_listing
            .iter()
            .filter(|file| !self.files.contains_key(&file.rel_path))
            .cloned()
            .collect();
        let mut virtual_paths: Vec<&String> = self.files.keys().collect();
        virtual_paths.sort();
        for path in virtual_paths {
            let file = &self.files[path];
            let size = self.virtual_size(path, file).await?;
            out.push(self.virtual_stat(path, size));
        }
        out.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
        Ok(out)
    }

    async fn list_dir(&self) -> anyhow::Result<Option<DirListing>> {
        // The virtual view is only ever asked for the full listing (the driver already knows
        // this is a dataset, not a loose-file repo).
        Ok(None)
    }

    async fn file_size(&self, rel_path: &str) -> anyhow::Result<u64> {
        if let Some(file) = self.files.get(rel_path) {
            return self.virtual_size(rel_path, file).await;
        }
        self.inner.file_size(rel_path).await
    }

    async fn file_stat(&self, rel_path: &str) -> anyhow::Result<ListedFile> {
        if let Some(file) = self.files.get(rel_path) {
            let size = self.virtual_size(rel_path, file).await?;
            return Ok(self.virtual_stat(rel_path, size));
        }
        self.inner.file_stat(rel_path).await
    }

    async fn get_range_once(&self, rel_path: &str, range: Range<u64>) -> anyhow::Result<Vec<u8>> {
        match self.files.get(rel_path) {
            Some(VirtualFile::Static(bytes)) => Ok(slice_range(bytes, range)),
            Some(&VirtualFile::EpisodeParquet { offset, len }) => {
                let bytes = self.episode_parquet(rel_path, offset, len).await?;
                Ok(slice_range(&bytes, range))
            }
            Some(VirtualFile::Video { blob, size }) => {
                let clamped = range.start.min(*size)..range.end.min(*size);
                Ok(blob.read_range(clamped).await?.to_vec())
            }
            None => self.inner.get_range_once(rel_path, range).await,
        }
    }

    fn loose_files_expected(&self) -> bool {
        false
    }

    fn dataset_version(&self) -> Option<String> {
        // The Lance manifest versions — the wasm artifacts fallback reproduces this exact
        // string from the `_versions/` manifest file names, so keep the format in sync with
        // [`crate::lerobot_remote::run_stream_lance`]'s wasm side.
        Some(self.content_version.clone())
    }
}

/// `[range.start, range.end)` of `bytes`, clamped to its length.
fn slice_range(bytes: &Bytes, range: Range<u64>) -> Vec<u8> {
    let len = bytes.len() as u64;
    let start = range.start.min(len) as usize;
    let end = range.end.min(len) as usize;
    bytes[start..end].to_vec()
}

/// Open the Lance table at the dataset-relative `table_path` through the adapter.
async fn open_table(
    adapter: Arc<DatasetStoreObjectStore<impl DatasetStore + 'static>>,
    table_path: &str,
) -> anyhow::Result<Table> {
    let url = url::Url::parse(&format!("lance-virtual:///{table_path}"))
        .with_context(|| format!("Invalid table path: {table_path}"))?;
    Table::open_with_object_store(adapter as Arc<dyn object_store::ObjectStore>, url).await
}
