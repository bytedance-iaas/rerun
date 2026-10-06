//! The browser's view of a Lance-format `LeRobot` dataset: artifacts only.
//!
//! The browser cannot decode Lance (the `lance` crate is native-only), but for the
//! [`re_lerobot::lance::LanceDatasetLayout::LeRobotMetaDir`] layout everything needed to
//! *announce* the episodes and *fingerprint* them is readable without Lance:
//!
//! * `meta/` is a verbatim `LeRobot` v3 metadata directory (plain JSON + parquet);
//! * the Lance table versions — the fingerprint salt — are derivable from the `_versions/`
//!   manifest *file names* ([`re_lerobot::lance::lance_version_from_manifest_paths`]).
//!
//! [`LanceLiteStore`] serves exactly that: `meta/**` passes through, the per-episode artifact
//! fingerprints come out identical to the ones the native [`crate::lance_remote`] reader
//! computes — so episodes converted by a desktop viewer (or `rerun rrd-convert`) load from the
//! rrd-artifacts store, and only an actual conversion attempt fails, with a clear message.

use std::collections::BTreeSet;
use std::ops::Range;
use std::sync::Arc;

use re_i18n::trf;
use re_lerobot::lance::{data_files_from_episode_parquets, lance_version_from_manifest_paths};

use crate::lerobot_remote::{DatasetStore, HttpStatusError, ListedFile, PauseState, fetch_full};

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

        let version_of = |table: &str| {
            let prefix = format!("{table}/_versions/");
            lance_version_from_manifest_paths(
                listing
                    .iter()
                    .filter(|file| file.rel_path.starts_with(&prefix))
                    .map(|file| file.rel_path.as_str()),
            )
        };
        let (Some(frames_version), Some(videos_version)) =
            (version_of("frames.lance"), version_of("videos.lance"))
        else {
            anyhow::bail!("No Lance manifests found under frames.lance/ and videos.lance/");
        };
        // Keep in sync with `LanceVirtualStore`'s `content_version` (native).
        let dataset_version = format!("lance:{frames_version}:{videos_version}");

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
