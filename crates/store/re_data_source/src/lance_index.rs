//! The *dataset index* that lets the browser read a Lance-format `LeRobot` dataset without
//! decoding Lance.
//!
//! The index describes the virtual classic dataset that the native reader
//! ([`crate::lance_remote::LanceVirtualStore`]) presents, mapping every virtual file to a way
//! of obtaining its bytes that works in a browser:
//!
//! * [`IndexSource::Embedded`] — small synthesized metadata files, carried in the manifest;
//! * [`IndexSource::Export`] — synthesized episode-data parquet, uploaded next to the manifest;
//! * [`IndexSource::Range`] — videos: a byte range of a real dataset file (Lance blobs are raw
//!   MP4 bytes), served by the ordinary ranged reads the dataset backend already supports.
//!
//! Files that physically exist in the dataset (the `meta/` directory of the
//! `LeRobotMetaDir` layout) are not listed: the browser reads them directly.
//!
//! The manifest lives in the rrd-artifacts store under `<dataset mirror>/lance_index/`, and is
//! valid only while [`LanceIndex::dataset_version`] matches the dataset's Lance manifest
//! versions — which the browser derives from the `_versions/` file names, so staleness is
//! detected without decoding anything.

use serde::{Deserialize, Serialize};

/// Bump when the index format changes incompatibly; readers reject unknown revisions.
pub const LANCE_INDEX_FORMAT_REV: u32 = 1;

/// The manifest of one dataset's index.
#[derive(Debug, Serialize, Deserialize)]
pub struct LanceIndex {
    pub format_rev: u32,

    /// The dataset's Lance manifest versions (the same string as
    /// `DatasetStore::dataset_version`); a mismatch means the index is stale.
    pub dataset_version: String,

    /// The virtual files, sorted by path.
    pub files: Vec<IndexFile>,
}

/// One virtual file of the presented classic dataset.
#[derive(Debug, Serialize, Deserialize)]
pub struct IndexFile {
    /// Virtual dataset-relative path (e.g. `data/chunk-000/file-000.parquet`).
    pub path: String,

    pub size: u64,

    #[serde(flatten)]
    pub source: IndexSource,
}

/// Where a virtual file's bytes come from.
#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum IndexSource {
    /// Carried right here (small synthesized metadata; UTF-8).
    Embedded { text: String },

    /// Uploaded next to the manifest, at [`export_key`].
    Export,

    /// `[offset, offset + size)` of a real file of the dataset.
    Range { source_path: String, offset: u64 },
}

/// The dataset's version salt, from the Lance `_versions/` manifest file names in a listing.
///
/// Must produce exactly the native reader's `content_version`
/// (`LanceVirtualStore::dataset_version`) — it feeds fingerprints and index freshness, and is
/// how the browser detects staleness without decoding Lance.
pub fn dataset_salt(
    listing: &[crate::lerobot_remote::ListedFile],
    layout: re_lerobot::lance::LanceDatasetLayout,
) -> Option<String> {
    use re_lerobot::lance::{LanceDatasetLayout, lance_version_from_manifest_paths};

    let version_of = |table_dir: &str| {
        let prefix = format!("{table_dir}/_versions/");
        lance_version_from_manifest_paths(
            listing
                .iter()
                .filter(|file| file.rel_path.starts_with(&prefix))
                .map(|file| file.rel_path.as_str()),
        )
    };

    match layout {
        LanceDatasetLayout::LeRobotMetaDir => Some(format!(
            "lance:{}:{}",
            version_of("frames.lance")?,
            version_of("videos.lance")?
        )),
        LanceDatasetLayout::EpisodeTables => {
            let prefix = if listing
                .iter()
                .any(|file| file.rel_path.starts_with("data/frames.lance/"))
            {
                "data/"
            } else {
                ""
            };
            Some(format!(
                "lance:{}:{}",
                version_of(&format!("{prefix}frames.lance"))?,
                version_of(&format!("{prefix}episodes.lance"))?
            ))
        }
    }
}

/// What the browser sends the catalog server to have a missing index (re)built
/// (`/api/ensure-manifest`) — only TOS-backed datasets support this.
pub struct ServerIndexParams {
    /// The dataset's `tos://bucket/prefix/` url.
    pub dataset_url: String,

    /// The bucket's region — the server cannot reach the bucket through the wrong
    /// region's endpoint.
    pub region: String,

    /// The signing credentials of the open, for zero-credential deployments (the server
    /// itself may hold none). Rides along like the ensure-cors call's.
    pub credentials: Option<crate::tos::TosCredentials>,
}

/// The index directory of one dataset within the artifacts store,
/// e.g. `rrd-data/tos/bucket/path/lance_index/`.
pub fn index_dir(artifacts_prefix: &str, dataset_url: &str) -> String {
    format!(
        "{}lance_index/",
        crate::rrd_artifacts::dataset_artifacts_dir(artifacts_prefix, dataset_url)
    )
}

/// The manifest's object key within the artifacts store.
pub fn manifest_key(index_dir: &str) -> String {
    format!("{index_dir}manifest.json")
}

/// An [`IndexSource::Export`] file's object key within the artifacts store.
pub fn export_key(index_dir: &str, virtual_path: &str) -> String {
    format!("{index_dir}exports/{virtual_path}")
}

impl LanceIndex {
    /// Parse and validate a fetched manifest against the expected dataset version.
    ///
    /// `Ok(None)` = readable but stale or of an unknown revision (regenerate);
    /// `Err` = not a manifest at all.
    pub fn parse_if_fresh(
        bytes: &[u8],
        expected_dataset_version: &str,
    ) -> anyhow::Result<Option<Self>> {
        let manifest: Self = serde_json::from_slice(bytes)?;
        if manifest.format_rev != LANCE_INDEX_FORMAT_REV
            || manifest.dataset_version != expected_dataset_version
        {
            return Ok(None);
        }
        Ok(Some(manifest))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_mirror_the_dataset_url() {
        let dir = index_dir("rrd-data/", "tos://bucket/some/dataset/");
        assert_eq!(dir, "rrd-data/tos/bucket/some/dataset/lance_index/");
        assert_eq!(
            manifest_key(&dir),
            "rrd-data/tos/bucket/some/dataset/lance_index/manifest.json"
        );
        assert_eq!(
            export_key(&dir, "data/chunk-000/file-000.parquet"),
            "rrd-data/tos/bucket/some/dataset/lance_index/exports/data/chunk-000/file-000.parquet"
        );
    }

    #[test]
    fn stale_or_unknown_manifests_are_rejected_but_parse() {
        let manifest = LanceIndex {
            format_rev: LANCE_INDEX_FORMAT_REV,
            dataset_version: "lance:3:3".to_owned(),
            files: vec![IndexFile {
                path: "videos/cam/chunk-000/file-000.mp4".to_owned(),
                size: 10,
                source: IndexSource::Range {
                    source_path: "videos.lance/data/x/1.blob".to_owned(),
                    offset: 0,
                },
            }],
        };
        let bytes = serde_json::to_vec(&manifest).unwrap();

        assert!(
            LanceIndex::parse_if_fresh(&bytes, "lance:3:3")
                .unwrap()
                .is_some()
        );
        assert!(
            LanceIndex::parse_if_fresh(&bytes, "lance:4:3")
                .unwrap()
                .is_none()
        );
        assert!(LanceIndex::parse_if_fresh(b"not json", "lance:3:3").is_err());
    }
}
