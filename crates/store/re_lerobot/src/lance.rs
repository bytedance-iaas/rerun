//! Detection of Lance-format `LeRobot` datasets.
//!
//! Two layouts exist in the wild (so far):
//!
//! * [`LanceDatasetLayout::LeRobotMetaDir`] — written by `lerobot-lancedb`'s `lerobot-lance-convert`:
//!   `frames.lance`/`videos.lance`/`meta.lance` tables at the dataset root, next to a verbatim
//!   `LeRobot` v3 `meta/` directory (whose `info.json` carries `"storage_format": "lance"`).
//! * [`LanceDatasetLayout::EpisodeTables`] — the `lance-format` three-table layout:
//!   `frames`/`episodes`/`videos` tables (usually under `data/`), with all metadata inside the
//!   `episodes` table and no `meta/` directory.
//!
//! This module only *recognizes* the layouts; reading them is handled elsewhere.

#[cfg(not(target_arch = "wasm32"))]
use std::path::Path;

/// The on-disk/remote layout of a Lance-format `LeRobot` dataset.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LanceDatasetLayout {
    /// `lerobot-lancedb` output: Lance tables next to a verbatim `LeRobot` v3 `meta/` directory.
    LeRobotMetaDir,

    /// `lance-format` three-table layout: `frames`/`episodes`/`videos` tables, self-contained.
    EpisodeTables,
}

impl std::fmt::Display for LanceDatasetLayout {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LeRobotMetaDir => f.write_str("Lance (LeRobot meta directory)"),
            Self::EpisodeTables => f.write_str("Lance (episode tables)"),
        }
    }
}

/// Decide the Lance layout from the names of the immediate subdirectories of the dataset root.
///
/// Trailing slashes in the names are ignored. Returns `None` when the root alone is not
/// conclusive — notably when the tables sit under `data/`, which a one-level listing cannot see
/// into; callers should then fall back to [`layout_from_file_paths`] over a full listing.
pub fn layout_from_root_entries<'a>(
    subdirs: impl IntoIterator<Item = &'a str>,
) -> Option<LanceDatasetLayout> {
    let (mut meta, mut frames, mut videos, mut episodes) = (false, false, false, false);
    for name in subdirs {
        match name.trim_end_matches('/') {
            "meta" => meta = true,
            "frames.lance" => frames = true,
            "videos.lance" => videos = true,
            "episodes.lance" => episodes = true,
            _ => {}
        }
    }

    if frames && videos && meta {
        Some(LanceDatasetLayout::LeRobotMetaDir)
    } else if frames && videos && episodes {
        Some(LanceDatasetLayout::EpisodeTables)
    } else {
        None
    }
}

/// Decide the Lance layout from a full listing of dataset-relative file paths
/// (e.g. `data/frames.lance/_versions/1.manifest`).
pub fn layout_from_file_paths<'a>(
    rel_paths: impl IntoIterator<Item = &'a str>,
) -> Option<LanceDatasetLayout> {
    let (mut meta, mut frames, mut videos, mut episodes) = (false, false, false, false);
    let (mut d_frames, mut d_videos, mut d_episodes) = (false, false, false);
    for path in rel_paths {
        if let Some(nested) = path.strip_prefix("data/") {
            d_frames |= nested.starts_with("frames.lance/");
            d_videos |= nested.starts_with("videos.lance/");
            d_episodes |= nested.starts_with("episodes.lance/");
        } else {
            meta |= path.starts_with("meta/");
            frames |= path.starts_with("frames.lance/");
            videos |= path.starts_with("videos.lance/");
            episodes |= path.starts_with("episodes.lance/");
        }
    }

    if d_frames && d_videos && d_episodes {
        Some(LanceDatasetLayout::EpisodeTables)
    } else if frames && videos && meta {
        Some(LanceDatasetLayout::LeRobotMetaDir)
    } else if frames && videos && episodes {
        Some(LanceDatasetLayout::EpisodeTables)
    } else {
        None
    }
}

/// Check whether the provided local path contains a Lance-format `LeRobot` dataset,
/// and if so which layout.
#[cfg(not(target_arch = "wasm32"))]
pub fn find_lance_layout(path: impl AsRef<Path>) -> Option<LanceDatasetLayout> {
    let path = path.as_ref();
    if !path.is_dir() {
        return None;
    }

    // A subdirectory counts only when it exists and is non-empty.
    let dir = |rel: &str| {
        let sub = path.join(rel);
        sub.is_dir()
            && sub
                .read_dir()
                .is_ok_and(|mut contents| contents.next().is_some())
    };

    if dir("meta") && dir("frames.lance") && dir("videos.lance") {
        Some(LanceDatasetLayout::LeRobotMetaDir)
    } else if (dir("data/frames.lance") && dir("data/episodes.lance") && dir("data/videos.lance"))
        || (dir("frames.lance") && dir("episodes.lance") && dir("videos.lance"))
    {
        Some(LanceDatasetLayout::EpisodeTables)
    } else {
        None
    }
}

/// Whether a full listing of dataset-relative paths contains Lance tables at all.
///
/// Recognized layout or not — this lets callers tell "an unsupported Lance variant" apart
/// from "not a Lance dataset", for a clear error instead of a confusing fallback.
pub fn file_paths_have_lance_tables<'a>(rel_paths: impl IntoIterator<Item = &'a str>) -> bool {
    rel_paths.into_iter().any(|path| path.contains(".lance/"))
}

/// Local variant of [`file_paths_have_lance_tables`].
#[cfg(not(target_arch = "wasm32"))]
pub fn has_lance_tables(path: impl AsRef<Path>) -> bool {
    fn dir_has_lance_table(dir: &Path) -> bool {
        std::fs::read_dir(dir).is_ok_and(|entries| {
            entries.flatten().any(|entry| {
                entry.path().is_dir() && entry.file_name().to_string_lossy().ends_with(".lance")
            })
        })
    }

    let path = path.as_ref();
    dir_has_lance_table(path) || dir_has_lance_table(&path.join("data"))
}

// ----------------------------------------------------------------------------
// Pure helpers shared by the native readers and the wasm artifacts fallback.

/// Where one synthesized episode-data parquet file comes from: a row range of `frames.lance`.
pub struct DataFileRows {
    pub offset: u64,
    pub len: u64,
}

/// Substitute the `{chunk_index:03d}` / `{file_index:03d}` / `{video_key}` tokens of the v3 path
/// templates.
#[expect(clippy::literal_string_with_formatting_args)] // the tokens are LeRobot's, not Rust's
pub fn render_v3_path(template: &str, video_key: Option<&str>, chunk: i64, file: i64) -> String {
    let mut path = template
        .replace("{chunk_index:03d}", &format!("{chunk:03}"))
        .replace("{file_index:03d}", &format!("{file:03}"));
    if let Some(video_key) = video_key {
        path = path.replace("{video_key}", video_key);
    }
    path
}

/// Map each synthesized data-parquet path (from the `data_path` template) to the `frames.lance`
/// rows it covers.
///
/// The mapping comes from the real per-episode metadata parquet files under `meta/episodes/`:
/// the episodes of one (chunk, file) span the rows
/// `[min(dataset_from_index), max(dataset_to_index))`.
pub fn data_files_from_episode_parquets(
    parquet_files: impl IntoIterator<Item = bytes::Bytes>,
    data_path_template: &str,
) -> anyhow::Result<std::collections::HashMap<String, DataFileRows>> {
    use anyhow::Context as _;
    use arrow::array::{Array as _, Int64Array};

    fn int64_column<'a>(
        batch: &'a arrow::array::RecordBatch,
        name: &str,
    ) -> anyhow::Result<&'a Int64Array> {
        batch
            .column_by_name(name)
            .and_then(|c| c.as_any().downcast_ref::<Int64Array>())
            .with_context(|| format!("Expected an int64 column named `{name}`"))
    }

    let mut ranges: std::collections::HashMap<(i64, i64), (u64, u64)> =
        std::collections::HashMap::new();

    for bytes in parquet_files {
        let reader = parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(bytes)?
            .build()?;
        for batch in reader {
            let batch = batch?;
            let chunk = int64_column(&batch, "data/chunk_index")?;
            let file_idx = int64_column(&batch, "data/file_index")?;
            let from = int64_column(&batch, "dataset_from_index")?;
            let to = int64_column(&batch, "dataset_to_index")?;
            for i in 0..batch.num_rows() {
                let entry = ranges
                    .entry((chunk.value(i), file_idx.value(i)))
                    .or_insert((u64::MAX, 0));
                entry.0 = entry.0.min(from.value(i) as u64);
                entry.1 = entry.1.max(to.value(i) as u64);
            }
        }
    }

    Ok(ranges
        .into_iter()
        .map(|((chunk, file), (from, to))| {
            (
                render_v3_path(data_path_template, None, chunk, file),
                DataFileRows {
                    offset: from,
                    len: to.saturating_sub(from),
                },
            )
        })
        .collect())
}

/// The version of a Lance table, from the file names under its `_versions/` directory.
///
/// Lance names manifests `(u64::MAX - version).manifest` so the newest sorts first; the table
/// version is therefore derivable from a plain file listing, without decoding anything —
/// which is how the wasm artifacts fallback fingerprints a dataset it cannot read.
pub fn lance_version_from_manifest_paths<'a>(
    manifest_paths: impl IntoIterator<Item = &'a str>,
) -> Option<u64> {
    manifest_paths
        .into_iter()
        .filter_map(|path| {
            path.rsplit('/')
                .next()?
                .strip_suffix(".manifest")?
                .parse::<u64>()
                .ok()
        })
        .min()
        .map(|smallest| u64::MAX - smallest)
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    #![expect(clippy::unwrap_used)] // tests may panic

    use super::*;

    /// Create the given relative directories, dropping a marker file into each so they are
    /// non-empty, plus the given (empty) files.
    fn fixture(dirs: &[&str], files: &[&str]) -> tempfile::TempDir {
        let root = tempfile::tempdir().unwrap();
        for dir in dirs {
            let path = root.path().join(dir);
            std::fs::create_dir_all(&path).unwrap();
            std::fs::write(path.join(".marker"), b"x").unwrap();
        }
        for file in files {
            let path = root.path().join(file);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, b"x").unwrap();
        }
        root
    }

    #[test]
    fn local_lerobot_meta_dir_layout() {
        let root = fixture(
            &[
                "meta/episodes",
                "frames.lance/data",
                "videos.lance/data",
                "meta.lance/data",
            ],
            &["meta/info.json"],
        );
        assert_eq!(
            find_lance_layout(root.path()),
            Some(LanceDatasetLayout::LeRobotMetaDir)
        );
        // Not a classic v3 dataset: there is no `data/` directory.
        assert_eq!(
            crate::LeRobotDatasetVersion::find_version(root.path()),
            None
        );
        // …but it still counts as a LeRobot dataset (e.g. the directory importer must skip it).
        assert!(crate::is_lerobot_dataset(root.path()));
    }

    #[test]
    fn local_episode_tables_layout_under_data() {
        let root = fixture(
            &[
                "data/frames.lance/data",
                "data/episodes.lance/data",
                "data/videos.lance/data",
            ],
            &[],
        );
        assert_eq!(
            find_lance_layout(root.path()),
            Some(LanceDatasetLayout::EpisodeTables)
        );
        assert_eq!(
            crate::LeRobotDatasetVersion::find_version(root.path()),
            None
        );
    }

    #[test]
    fn local_episode_tables_layout_at_root() {
        let root = fixture(
            &[
                "frames.lance/data",
                "episodes.lance/data",
                "videos.lance/data",
            ],
            &[],
        );
        assert_eq!(
            find_lance_layout(root.path()),
            Some(LanceDatasetLayout::EpisodeTables)
        );
    }

    #[test]
    fn local_classic_v3_is_not_lance() {
        let root = fixture(&["meta/episodes", "data/chunk-000"], &["meta/info.json"]);
        assert_eq!(find_lance_layout(root.path()), None);
        assert_eq!(
            crate::LeRobotDatasetVersion::find_version(root.path()),
            Some(crate::LeRobotDatasetVersion::V3)
        );
    }

    #[test]
    fn local_negatives() {
        let empty = tempfile::tempdir().unwrap();
        assert_eq!(find_lance_layout(empty.path()), None);
        assert_eq!(find_lance_layout(empty.path().join("nonexistent")), None);

        // Lance table directories that exist but are empty do not count.
        let root = tempfile::tempdir().unwrap();
        for dir in ["meta", "frames.lance", "videos.lance"] {
            std::fs::create_dir_all(root.path().join(dir)).unwrap();
        }
        assert_eq!(find_lance_layout(root.path()), None);
    }

    #[test]
    fn root_entries_detection() {
        assert_eq!(
            layout_from_root_entries(["meta/", "frames.lance/", "videos.lance/", "meta.lance/"]),
            Some(LanceDatasetLayout::LeRobotMetaDir)
        );
        assert_eq!(
            layout_from_root_entries(["frames.lance", "episodes.lance", "videos.lance"]),
            Some(LanceDatasetLayout::EpisodeTables)
        );
        // Tables hidden under `data/`: the root alone is not conclusive.
        assert_eq!(layout_from_root_entries(["data/"]), None);
        assert_eq!(layout_from_root_entries(["meta/", "data/"]), None);
        assert_eq!(layout_from_root_entries([]), None);
    }

    #[test]
    fn file_paths_detection() {
        // so101-style (lerobot-lancedb).
        assert_eq!(
            layout_from_file_paths([
                "meta/info.json",
                "meta/episodes/chunk-000/file-000.parquet",
                "frames.lance/_versions/1.manifest",
                "frames.lance/data/abc.lance",
                "videos.lance/data/abc.lance",
                "meta.lance/data/abc.lance",
            ]),
            Some(LanceDatasetLayout::LeRobotMetaDir)
        );
        // pusht-style (lance-format, tables under `data/`).
        assert_eq!(
            layout_from_file_paths([
                "README.md",
                "data/frames.lance/_versions/1.manifest",
                "data/episodes.lance/data/abc.lance",
                "data/videos.lance/data/abc.lance",
            ]),
            Some(LanceDatasetLayout::EpisodeTables)
        );
        // Classic v3 dataset: parquet + mp4, no Lance tables.
        assert_eq!(
            layout_from_file_paths([
                "meta/info.json",
                "data/chunk-000/file-000.parquet",
                "videos/observation.images.front/chunk-000/file-000.mp4",
            ]),
            None
        );
    }

    #[test]
    fn version_from_manifest_file_names() {
        assert_eq!(
            lance_version_from_manifest_paths([
                "frames.lance/_versions/18446744073709551612.manifest",
                "frames.lance/_versions/18446744073709551613.manifest",
                "frames.lance/_versions/18446744073709551614.manifest",
                "frames.lance/_versions/latest_version_hint.json",
            ]),
            Some(3)
        );
        assert_eq!(
            lance_version_from_manifest_paths(["18446744073709551614.manifest"]),
            Some(1)
        );
        assert_eq!(lance_version_from_manifest_paths([]), None);
        assert_eq!(
            lance_version_from_manifest_paths(["latest_version_hint.json"]),
            None
        );
    }

    #[test]
    fn data_files_from_episode_parquet_bytes() {
        use arrow::array::Int64Array;
        use std::sync::Arc;

        let batch = arrow::array::RecordBatch::try_from_iter([
            (
                "episode_index",
                Arc::new(Int64Array::from(vec![0, 1])) as arrow::array::ArrayRef,
            ),
            ("data/chunk_index", Arc::new(Int64Array::from(vec![0, 0]))),
            ("data/file_index", Arc::new(Int64Array::from(vec![0, 0]))),
            ("dataset_from_index", Arc::new(Int64Array::from(vec![0, 3]))),
            ("dataset_to_index", Arc::new(Int64Array::from(vec![3, 7]))),
        ])
        .unwrap();

        let mut out = Vec::new();
        let mut writer =
            parquet::arrow::ArrowWriter::try_new(&mut out, batch.schema(), None).unwrap();
        writer.write(&batch).unwrap();
        writer.close().unwrap();

        #[expect(clippy::literal_string_with_formatting_args)]
        let template = "data/chunk-{chunk_index:03d}/file-{file_index:03d}.parquet";
        let files = data_files_from_episode_parquets([bytes::Bytes::from(out)], template).unwrap();
        assert_eq!(files.len(), 1);
        let rows = &files["data/chunk-000/file-000.parquet"];
        assert_eq!((rows.offset, rows.len), (0, 7));
    }
}
