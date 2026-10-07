//! The browser's two ways of reading a Lance-format `LeRobot` dataset, neither of which
//! decodes Lance (the `lance` crate is native-only):
//!
//! 1. [`LanceManifestStore`] — full direct reading via the *dataset index*
//!    ([`crate::lance_index`]): videos come straight off the dataset bucket as byte ranges
//!    (Lance blobs are raw MP4 bytes), curves from small exported parquet files, metadata
//!    embedded in the manifest. When the index is missing or stale, the catalog server is
//!    asked to (re)build it (`/api/ensure-manifest`, same-origin like ensure-cors).
//! 2. [`LanceLiteStore`] — artifacts-only playback for the `LeRobotMetaDir` layout: `meta/`
//!    is readable without Lance, and the artifact fingerprints need only it plus the Lance
//!    manifest versions (derivable from the `_versions/` file names) — episodes a desktop
//!    viewer converted play, everything else fails with clear guidance.
//!
//! Both are [`DatasetStore`] decorators: the regular v2/v3 streaming driver (and its
//! per-episode rrd-artifacts read-through) runs on top unchanged.

use std::collections::{BTreeSet, HashMap};
use std::ops::Range;
use std::sync::Arc;

use bytes::Bytes;
use re_i18n::trf;
use re_lerobot::lance::{LanceDatasetLayout, data_files_from_episode_parquets};

use crate::lance_index::{
    IndexSource, LanceIndex, ServerIndexParams, dataset_salt, export_key, manifest_key,
};
use crate::lerobot_remote::{DatasetStore, HttpStatusError, ListedFile, PauseState, fetch_full};
use crate::tos::TosClient;

// ----------------------------------------------------------------------------
// Full direct reading via the dataset index.

/// One virtual file, resolved for browser reading.
enum WasmFile {
    /// Bytes carried in the manifest (synthesized metadata).
    Embedded(Bytes),

    /// An export object next to the manifest in the artifacts store.
    Export { size: u64 },

    /// A byte range of a real dataset file (videos).
    Range {
        source_path: String,
        offset: u64,
        size: u64,
    },
}

/// A [`DatasetStore`] presenting a Lance dataset as a classic one, from its dataset index.
pub struct LanceManifestStore<S> {
    inner: Arc<S>,

    /// Reads the manifest's export objects.
    artifacts: TosClient,
    index_dir: String,

    files: HashMap<String, WasmFile>,

    /// Physically existing `meta/**` files (the `LeRobotMetaDir` layout), minus the paths the
    /// manifest overrides.
    meta_listing: Vec<ListedFile>,

    dataset_version: String,
}

impl<S: DatasetStore + 'static> LanceManifestStore<S> {
    /// Open the dataset's index; ask the server to build it when missing or stale.
    ///
    /// `Ok(None)`: no usable index (and the server could not provide one) — fall back.
    pub async fn open(
        inner: Arc<S>,
        layout: LanceDatasetLayout,
        artifacts_config: &crate::rrd_artifacts::RrdArtifactsConfig,
    ) -> anyhow::Result<Option<Self>> {
        let listing = inner.list().await?;
        let Some(salt) = dataset_salt(&listing, layout) else {
            anyhow::bail!("No Lance `_versions/` manifests found in the dataset listing");
        };

        let artifacts = TosClient::new(
            artifacts_config.credentials.clone(),
            artifacts_config.location.bucket.clone(),
        );
        let index_dir =
            crate::lance_index::index_dir(&artifacts_config.location.prefix, &inner.url());
        let key = manifest_key(&index_dir);

        let mut manifest = fetch_fresh_manifest(&artifacts, &key, &salt).await;
        if manifest.is_none()
            && let Some(params) = inner.server_index_params()
            && ensure_index_via_server(&params).await
        {
            manifest = fetch_fresh_manifest(&artifacts, &key, &salt).await;
        }
        let Some(manifest) = manifest else {
            return Ok(None);
        };

        let mut files = HashMap::new();
        for file in manifest.files {
            let resolved = match file.source {
                IndexSource::Embedded { text } => WasmFile::Embedded(Bytes::from(text)),
                IndexSource::Export => WasmFile::Export { size: file.size },
                IndexSource::Range {
                    source_path,
                    offset,
                } => WasmFile::Range {
                    source_path,
                    offset,
                    size: file.size,
                },
            };
            files.insert(file.path, resolved);
        }

        let meta_listing = listing
            .into_iter()
            .filter(|file| {
                file.rel_path.starts_with("meta/") && !files.contains_key(&file.rel_path)
            })
            .collect();

        Ok(Some(Self {
            inner,
            artifacts,
            index_dir,
            files,
            meta_listing,
            dataset_version: salt,
        }))
    }

    fn virtual_stat(&self, rel_path: &str, size: u64) -> ListedFile {
        ListedFile {
            rel_path: rel_path.to_owned(),
            size,
            content_id: Some(format!("{}#{rel_path}", self.dataset_version)),
        }
    }

    fn size_of(file: &WasmFile) -> u64 {
        match file {
            WasmFile::Embedded(bytes) => bytes.len() as u64,
            WasmFile::Export { size } | WasmFile::Range { size, .. } => *size,
        }
    }
}

impl<S: DatasetStore + 'static> DatasetStore for LanceManifestStore<S> {
    fn url(&self) -> String {
        self.inner.url()
    }

    async fn list(&self) -> anyhow::Result<Vec<ListedFile>> {
        let mut out = self.meta_listing.clone();
        let mut paths: Vec<&String> = self.files.keys().collect();
        paths.sort();
        for path in paths {
            out.push(self.virtual_stat(path, Self::size_of(&self.files[path])));
        }
        out.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
        Ok(out)
    }

    async fn file_size(&self, rel_path: &str) -> anyhow::Result<u64> {
        if let Some(file) = self.files.get(rel_path) {
            return Ok(Self::size_of(file));
        }
        self.inner.file_size(rel_path).await
    }

    async fn file_stat(&self, rel_path: &str) -> anyhow::Result<ListedFile> {
        if let Some(file) = self.files.get(rel_path) {
            return Ok(self.virtual_stat(rel_path, Self::size_of(file)));
        }
        self.inner.file_stat(rel_path).await
    }

    async fn get_range_once(&self, rel_path: &str, range: Range<u64>) -> anyhow::Result<Vec<u8>> {
        match self.files.get(rel_path) {
            Some(WasmFile::Embedded(bytes)) => {
                let len = bytes.len() as u64;
                let start = range.start.min(len) as usize;
                let end = range.end.min(len) as usize;
                Ok(bytes[start..end].to_vec())
            }
            Some(WasmFile::Export { size }) => {
                let clamped = range.start.min(*size)..range.end.min(*size);
                self.artifacts
                    .get_object(&export_key(&self.index_dir, rel_path), Some(clamped))
                    .await
            }
            Some(WasmFile::Range {
                source_path,
                offset,
                size,
            }) => {
                let clamped = range.start.min(*size)..range.end.min(*size);
                self.inner
                    .get_range_once(source_path, offset + clamped.start..offset + clamped.end)
                    .await
            }
            None => self.inner.get_range_once(rel_path, range).await,
        }
    }

    fn dataset_version(&self) -> Option<String> {
        Some(self.dataset_version.clone())
    }
}

/// Fetch and validate the manifest; `None` on miss, staleness, or any error.
async fn fetch_fresh_manifest(
    artifacts: &TosClient,
    key: &str,
    expected_salt: &str,
) -> Option<LanceIndex> {
    let bytes = artifacts.get_object(key, None).await.ok()?;
    LanceIndex::parse_if_fresh(&bytes, expected_salt)
        .ok()
        .flatten()
}

/// Ask the same-origin catalog server to (re)build the dataset's index; `true` on success.
///
/// Best-effort like the ensure-cors call: a 404/501 just means this deployment (or a local
/// static server) has no endpoint.
async fn ensure_index_via_server(params: &ServerIndexParams) -> bool {
    let url = format!(
        "/api/ensure-manifest?dataset={}&region={}",
        crate::tos::client::uri_encode(&params.dataset_url, true),
        crate::tos::client::uri_encode(&params.region, true),
    );
    let body = params
        .credentials
        .as_ref()
        .filter(|creds| !creds.access_key.is_empty() && !creds.secret_key.is_empty())
        .map(|creds| {
            serde_json::json!({
                "access_key": creds.access_key,
                "secret_key": creds.secret_key,
                "session_token": creds.session_token,
            })
            .to_string()
            .into_bytes()
        })
        .unwrap_or_default();

    let mut request = ehttp::Request::post(&url, body);
    request.headers.insert("content-type", "application/json");

    // Building the index reads the dataset's (small) tables server-side — allow it time.
    match crate::http_client::fetch_async_with_timeout(request, std::time::Duration::from_mins(2))
        .await
    {
        Ok(response) if response.ok => {
            // 200 also covers benign refusals ({"status": "skipped"/"disabled"}) — only an
            // actual build counts, or the manifest refetch is a wasted round-trip.
            let built = serde_json::from_slice::<serde_json::Value>(&response.bytes)
                .ok()
                .and_then(|body| {
                    body.get("status")
                        .and_then(|status| status.as_str())
                        .map(|status| status == "ok")
                })
                .unwrap_or(false);
            if built {
                re_log::info!(
                    "{}",
                    trf!(
                        "The server built the dataset index — reading directly",
                        "服务端已生成数据集索引 — 开始直读"
                    )
                );
            } else {
                re_log::debug_once!(
                    "Index self-service declined: {}",
                    String::from_utf8_lossy(&response.bytes[..response.bytes.len().min(200)])
                );
            }
            built
        }
        Ok(response) if response.status == 404 || response.status == 501 => {
            re_log::debug_once!(
                "Index self-service endpoint not available (HTTP {})",
                response.status
            );
            false
        }
        Ok(response) => {
            re_log::warn_once!(
                "{}",
                trf!(
                    "Building the dataset index server-side failed (HTTP {}): {}",
                    "服务端生成数据集索引失败（HTTP {}）：{}",
                    response.status,
                    String::from_utf8_lossy(&response.bytes[..response.bytes.len().min(200)]),
                )
            );
            false
        }
        Err(err) => {
            re_log::debug_once!("Index self-service endpoint unreachable: {err}");
            false
        }
    }
}

// ----------------------------------------------------------------------------
// Artifacts-only fallback for the `LeRobotMetaDir` layout.

/// A metadata-only [`DatasetStore`] over a `LeRobotMetaDir`-layout Lance dataset.
pub struct LanceLiteStore<S> {
    inner: Arc<S>,

    /// The real listing, restricted to `meta/**`.
    meta_listing: Vec<ListedFile>,

    /// The virtual episode-data parquet paths. Their contents are NOT readable here — only
    /// their existence, so a cache miss fails at the fetch with a clear error instead of a
    /// confusing "file not in listing".
    virtual_data_paths: BTreeSet<String>,

    /// Must match the native reader's [`DatasetStore::dataset_version`] exactly — it feeds the
    /// artifact fingerprints.
    dataset_version: String,
}

impl<S: DatasetStore + 'static> LanceLiteStore<S> {
    pub async fn open(inner: Arc<S>) -> anyhow::Result<Self> {
        let listing = inner.list().await?;

        let Some(dataset_version) = dataset_salt(&listing, LanceDatasetLayout::LeRobotMetaDir)
        else {
            anyhow::bail!("No Lance manifests found under frames.lance/ and videos.lance/");
        };

        let meta_listing: Vec<ListedFile> = listing
            .iter()
            .filter(|file| file.rel_path.starts_with("meta/"))
            .cloned()
            .collect();

        let pause = PauseState::default();
        let info_bytes = fetch_full(inner.as_ref(), &pause, "meta/info.json").await?;
        let info: serde_json::Value = serde_json::from_slice(&info_bytes)?;
        let data_path_template = info
            .get("data_path")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_owned();

        let mut episode_parquets = Vec::new();
        for file in &meta_listing {
            if file.rel_path.starts_with("meta/episodes/") && file.rel_path.ends_with(".parquet") {
                let bytes = fetch_full(inner.as_ref(), &pause, &file.rel_path).await?;
                episode_parquets.push(bytes::Bytes::from(bytes));
            }
        }
        let virtual_data_paths =
            data_files_from_episode_parquets(episode_parquets, &data_path_template)?
                .into_keys()
                .collect();

        Ok(Self {
            inner,
            meta_listing,
            virtual_data_paths,
            dataset_version,
        })
    }

    fn conversion_needed(rel_path: &str) -> anyhow::Error {
        anyhow::Error::new(HttpStatusError(404)).context(trf!(
            "This episode of the Lance-format dataset has no converted artifact yet. \
             Open the dataset once in the desktop viewer (or run `rerun rrd-convert`) to \
             convert it.\nFile: {rel_path}",
            "该 Lance 格式数据集的这一集还没有转换产物。\
             请先用桌面版 viewer 打开一次（或运行 `rerun rrd-convert`）完成转换。\n文件：{rel_path}"
        ))
    }
}

impl<S: DatasetStore + 'static> DatasetStore for LanceLiteStore<S> {
    fn url(&self) -> String {
        self.inner.url()
    }

    async fn list(&self) -> anyhow::Result<Vec<ListedFile>> {
        let mut out = self.meta_listing.clone();
        for path in &self.virtual_data_paths {
            out.push(ListedFile {
                rel_path: path.clone(),
                // The real size would require decoding Lance; any non-zero placeholder works —
                // an actual fetch fails with the clear conversion-needed error regardless.
                size: 1,
                content_id: None,
            });
        }
        out.sort_by(|a, b| a.rel_path.cmp(&b.rel_path));
        Ok(out)
    }

    async fn file_size(&self, rel_path: &str) -> anyhow::Result<u64> {
        if self.virtual_data_paths.contains(rel_path) {
            return Ok(1);
        }
        if rel_path.starts_with("meta/") {
            return self.inner.file_size(rel_path).await;
        }
        Err(Self::conversion_needed(rel_path))
    }

    async fn get_range_once(&self, rel_path: &str, range: Range<u64>) -> anyhow::Result<Vec<u8>> {
        if rel_path.starts_with("meta/") {
            return self.inner.get_range_once(rel_path, range).await;
        }
        Err(Self::conversion_needed(rel_path))
    }

    fn dataset_version(&self) -> Option<String> {
        Some(self.dataset_version.clone())
    }
}
