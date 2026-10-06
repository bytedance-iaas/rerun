//! A virtual `LeRobot` v3 file system over a `lerobot-lancedb` dataset
//! ([`re_lerobot::lance::LanceDatasetLayout::LeRobotMetaDir`]).
//!
//! The dataset keeps a verbatim `LeRobot` v3 `meta/` directory, so metadata passes straight
//! through; only the episode data parquet files (synthesized from `frames.lance`) and the video
//! files (served from the `videos.lance` blob column) are virtual.

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Context as _;
use arrow::array::{Array as _, Int64Array, RecordBatch, StringArray};
use arrow::datatypes::{Field, Schema};
use bytes::Bytes;
use parking_lot::Mutex;

use re_lerobot::LeRobotError;
use re_lerobot::vfs::{Blob, LeRobotFs, LocalFs};

use crate::sanitize_column_name;
use crate::tables::Table;

pub use re_lerobot::lance::{DataFileRows, data_files_from_episode_parquets, render_v3_path};

/// A [`LeRobotFs`] presenting a `lerobot-lancedb` dataset as a plain `LeRobot` v3 directory.
pub struct LanceMetaDirFs {
    /// Serves everything under `meta/`.
    meta: LocalFs,

    frames: Table,
    videos: Table,

    /// Synthesized data-parquet path (from the `data_path` template) → `frames.lance` rows.
    data_files: HashMap<String, DataFileRows>,

    /// Video path (from the `video_path` template) → `videos.lance` row.
    video_files: HashMap<String, u64>,

    /// Lance column name → original feature name (dots restored).
    column_renames: HashMap<String, String>,

    /// Synthesized parquet files are cached: v3 loading reads each data file once eagerly, but a
    /// second read (e.g. a retried episode) must not pay the synthesis again.
    parquet_cache: Mutex<HashMap<String, Bytes>>,
}

impl LanceMetaDirFs {
    /// Open the dataset rooted at `path` (must be the
    /// [`re_lerobot::lance::LanceDatasetLayout::LeRobotMetaDir`] layout).
    pub fn open_local(path: &std::path::Path) -> anyhow::Result<Arc<dyn LeRobotFs>> {
        let meta = LocalFs {
            root: path.to_path_buf(),
        };

        let info_bytes = read_full(&meta, "meta/info.json")?;
        let info: serde_json::Value =
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

        let column_renames = column_renames_from_info(&info)?;

        let frames = Table::open(&path.join("frames.lance"))?;
        let videos = Table::open(&path.join("videos.lance"))?;

        let data_files = data_files_from_episode_metadata(&meta, &data_path_template)?;
        let video_files = video_files_from_table(&videos, &video_path_template)?;

        Ok(Arc::new(Self {
            meta,
            frames,
            videos,
            data_files,
            video_files,
            column_renames,
            parquet_cache: Mutex::new(HashMap::new()),
        }))
    }

    fn synthesize_data_parquet(
        &self,
        rel_path: &str,
        rows: &DataFileRows,
    ) -> anyhow::Result<Bytes> {
        if let Some(cached) = self.parquet_cache.lock().get(rel_path) {
            return Ok(cached.clone());
        }

        let columns = self.frames.column_names();
        let column_refs: Vec<&str> = columns.iter().map(|s| s.as_str()).collect();
        let batch = self.frames.read_rows(&column_refs, rows.offset, rows.len)?;
        let batch = rename_columns(&batch, &self.column_renames)?;
        let bytes = write_parquet(&batch)?;

        self.parquet_cache
            .lock()
            .insert(rel_path.to_owned(), bytes.clone());
        Ok(bytes)
    }
}

impl LeRobotFs for LanceMetaDirFs {
    fn read(&self, rel_path: &str) -> Result<Blob, LeRobotError> {
        if let Some(rows) = self.data_files.get(rel_path) {
            return self
                .synthesize_data_parquet(rel_path, rows)
                .map(Blob::Full)
                .map_err(LeRobotError::Other);
        }
        if let Some(row) = self.video_files.get(rel_path) {
            return self
                .videos
                .read_blob(*row, "video_bytes")
                .map(Blob::Full)
                .map_err(LeRobotError::Other);
        }
        self.meta.read(rel_path)
    }

    fn list_files(&self, rel_dir: &str) -> Result<Vec<String>, LeRobotError> {
        let prefix = format!("{}/", rel_dir.trim_end_matches('/'));
        let mut out: Vec<String> = self
            .data_files
            .keys()
            .chain(self.video_files.keys())
            .filter(|path| path.starts_with(&prefix))
            .cloned()
            .collect();
        // `meta/` (and anything else that physically exists, e.g. a kept `data/` directory).
        out.extend(self.meta.list_files(rel_dir).unwrap_or_default());
        out.sort();
        out.dedup();
        Ok(out)
    }

    fn exists(&self, rel_path: &str) -> bool {
        self.data_files.contains_key(rel_path)
            || self.video_files.contains_key(rel_path)
            || self.meta.exists(rel_path)
    }
}

/// Lance column name → original feature name (dots restored), from the dataset's info.json.
///
/// Dots were sanitized to underscores on conversion; the map turns them back so the columns
/// match the feature keys of info.json again.
pub fn column_renames_from_info(
    info: &serde_json::Value,
) -> anyhow::Result<HashMap<String, String>> {
    let mut column_renames = HashMap::new();
    if let Some(features) = info.get("features").and_then(|v| v.as_object()) {
        for key in features.keys() {
            let sanitized = sanitize_column_name(key);
            if &sanitized != key
                && let Some(previous) = column_renames.insert(sanitized, key.clone())
            {
                anyhow::bail!(
                    "Ambiguous feature names: `{previous}` and `{key}` both sanitize to the same Lance column name"
                );
            }
        }
    }
    Ok(column_renames)
}

/// Build the synthesized-data-parquet map from the real per-episode metadata under
/// `meta/episodes/`: episodes of one (chunk, file) span the `frames.lance` rows
/// `[min(dataset_from_index), max(dataset_to_index))`.
fn data_files_from_episode_metadata(
    meta: &LocalFs,
    data_path_template: &str,
) -> anyhow::Result<HashMap<String, DataFileRows>> {
    let mut parquet_files = Vec::new();
    for file in meta
        .list_files("meta/episodes")
        .map_err(|err| anyhow::anyhow!("{err}"))?
    {
        if file.ends_with(".parquet") {
            parquet_files.push(read_full(meta, &file)?);
        }
    }
    data_files_from_episode_parquets(parquet_files, data_path_template)
}

/// Build the video-path map from the small `videos.lance` table.
fn video_files_from_table(
    videos: &Table,
    video_path_template: &str,
) -> anyhow::Result<HashMap<String, u64>> {
    if video_path_template.is_empty() {
        return Ok(HashMap::new());
    }
    let batch = videos.read_all(&["video_key", "chunk_index", "file_index"])?;
    video_files_from_batch(&batch, video_path_template)
}

/// Video path (from the `video_path` template) → table row, from the already-read
/// `video_key`/`chunk_index`/`file_index` columns of `videos.lance`.
pub fn video_files_from_batch(
    batch: &RecordBatch,
    video_path_template: &str,
) -> anyhow::Result<HashMap<String, u64>> {
    if video_path_template.is_empty() {
        return Ok(HashMap::new());
    }
    let keys = batch
        .column_by_name("video_key")
        .and_then(|c| c.as_any().downcast_ref::<StringArray>())
        .context("videos.lance has no `video_key` string column")?;
    let chunks = int64_column(batch, "chunk_index")?;
    let files = int64_column(batch, "file_index")?;

    let mut out = HashMap::new();
    for row in 0..batch.num_rows() {
        let path = render_v3_path(
            video_path_template,
            Some(keys.value(row)),
            chunks.value(row),
            files.value(row),
        );
        out.insert(path, row as u64);
    }
    Ok(out)
}

fn int64_column<'a>(batch: &'a RecordBatch, name: &str) -> anyhow::Result<&'a Int64Array> {
    batch
        .column_by_name(name)
        .and_then(|c| c.as_any().downcast_ref::<Int64Array>())
        .with_context(|| format!("Expected an int64 column named `{name}`"))
}

/// Rename the batch's columns according to `renames` (lance name → original feature name).
pub fn rename_columns(
    batch: &RecordBatch,
    renames: &HashMap<String, String>,
) -> anyhow::Result<RecordBatch> {
    if renames.is_empty() {
        return Ok(batch.clone());
    }

    let fields: Vec<Field> = batch
        .schema()
        .fields()
        .iter()
        .map(|field| {
            let name = renames
                .get(field.name())
                .cloned()
                .unwrap_or_else(|| field.name().clone());
            field.as_ref().clone().with_name(name)
        })
        .collect();
    let schema = Schema::new_with_metadata(fields, batch.schema().metadata().clone());
    RecordBatch::try_new_with_options(
        Arc::new(schema),
        batch.columns().to_vec(),
        &arrow::array::RecordBatchOptions::default().with_row_count(Some(batch.num_rows())),
    )
    .context("Failed to rename frame columns")
}

/// Serialize a batch to parquet bytes (one row group, so downstream readers see it whole).
pub fn write_parquet(batch: &RecordBatch) -> anyhow::Result<Bytes> {
    let props = parquet::file::properties::WriterProperties::builder()
        .set_max_row_group_row_count(Some(batch.num_rows().max(1)))
        .build();
    let mut out = Vec::new();
    let mut writer = parquet::arrow::ArrowWriter::try_new(&mut out, batch.schema(), Some(props))
        .context("Failed to create parquet writer")?;
    writer.write(batch).context("Failed to write parquet")?;
    writer.close().context("Failed to finish parquet")?;
    Ok(Bytes::from(out))
}

fn read_full(fs: &dyn LeRobotFs, rel_path: &str) -> anyhow::Result<Bytes> {
    match fs.read(rel_path).map_err(|err| anyhow::anyhow!("{err}"))? {
        Blob::Full(bytes) => Ok(bytes),
        Blob::Sparse(_) => anyhow::bail!("Expected a full file\nFile path: {rel_path}"),
    }
}
