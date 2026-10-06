//! A virtual `LeRobot` v2 file system over a `lance-format` three-table dataset
//! ([`re_lerobot::lance::LanceDatasetLayout::EpisodeTables`]).
//!
//! These datasets have no `meta/` directory — everything lives in the `frames`/`episodes`/`videos`
//! tables. The metadata files are synthesized from the tables, each episode's data parquet from
//! the `frames` rows, and each episode's video from the `episodes` table's self-contained MP4
//! segment blobs (columns named `<camera>_video_blob`). This maps naturally onto the `LeRobot`
//! **v2** model of one data file and one video per episode.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write as _;
use std::sync::Arc;

use anyhow::Context as _;
use arrow::array::{Array as _, Int64Array, RecordBatch};
use arrow::datatypes::DataType;
use bytes::Bytes;
use parking_lot::Mutex;

use re_lerobot::LeRobotError;
use re_lerobot::vfs::{Blob, LeRobotFs};

use crate::meta_dir::write_parquet;
use crate::tables::Table;

/// Suffix of the per-episode MP4 segment blob columns of the `episodes` table.
const VIDEO_BLOB_SUFFIX: &str = "_video_blob";

pub enum SynthFile {
    /// A synthesized metadata file.
    Static(Bytes),

    /// One episode's data parquet: a row range of the `frames` table.
    EpisodeData { offset: u64, len: u64 },

    /// One episode's video: an MP4 segment blob of the `episodes` table, at (row, column).
    EpisodeVideo { row: u64, column: String },
}

/// A [`LeRobotFs`] presenting a `lance-format` three-table dataset as a `LeRobot` v2 directory.
pub struct LanceEpisodeTablesFs {
    frames: Table,
    episodes: Table,
    files: HashMap<String, SynthFile>,
    parquet_cache: Mutex<HashMap<String, Bytes>>,
}

impl LanceEpisodeTablesFs {
    /// Open the dataset rooted at `path` (must be the
    /// [`re_lerobot::lance::LanceDatasetLayout::EpisodeTables`] layout).
    pub fn open_local(path: &std::path::Path) -> anyhow::Result<Arc<dyn LeRobotFs>> {
        let tables_dir = if path.join("data/frames.lance").is_dir() {
            path.join("data")
        } else {
            path.to_path_buf()
        };

        let frames = Table::open(&tables_dir.join("frames.lance"))?;
        let episodes = Table::open(&tables_dir.join("episodes.lance"))?;

        // One row of each table tells us the schemas (blob columns scan as descriptor structs,
        // which is fine — only their names matter here).
        let frames_sample = {
            let columns = frames.column_names();
            let column_refs: Vec<&str> = columns.iter().map(|s| s.as_str()).collect();
            frames.read_rows(&column_refs, 0, 1)?
        };

        let video_columns = video_blob_columns(&episodes.column_names());

        let frame_episode_indices = frames.read_all(&["episode_index"])?;

        let episodes_projection = episodes_meta_projection(&episodes.column_names());
        let episodes_projection_refs: Vec<&str> =
            episodes_projection.iter().map(|s| s.as_str()).collect();
        let episodes_meta = episodes.read_all(&episodes_projection_refs)?;

        let files = plan_v2_files(&V2PlanInputs {
            frames_sample,
            episodes_meta,
            frame_episode_indices,
            video_columns,
            total_frames: frames.num_rows()?,
        })?;

        Ok(Arc::new(Self {
            frames,
            episodes,
            files,
            parquet_cache: Mutex::new(HashMap::new()),
        }))
    }

    fn synthesize_episode_parquet(
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
        let batch = self.frames.read_rows(&column_refs, offset, len)?;
        let bytes = write_parquet(&batch)?;

        self.parquet_cache
            .lock()
            .insert(rel_path.to_owned(), bytes.clone());
        Ok(bytes)
    }
}

impl LeRobotFs for LanceEpisodeTablesFs {
    fn read(&self, rel_path: &str) -> Result<Blob, LeRobotError> {
        match self.files.get(rel_path) {
            Some(SynthFile::Static(bytes)) => Ok(Blob::Full(bytes.clone())),
            Some(&SynthFile::EpisodeData { offset, len }) => self
                .synthesize_episode_parquet(rel_path, offset, len)
                .map(Blob::Full)
                .map_err(LeRobotError::Other),
            Some(SynthFile::EpisodeVideo { row, column }) => self
                .episodes
                .read_blob(*row, column)
                .map(Blob::Full)
                .map_err(LeRobotError::Other),
            None => Err(LeRobotError::io(
                std::io::Error::new(std::io::ErrorKind::NotFound, "no such virtual file"),
                rel_path,
            )),
        }
    }

    fn list_files(&self, rel_dir: &str) -> Result<Vec<String>, LeRobotError> {
        let prefix = format!("{}/", rel_dir.trim_end_matches('/'));
        let mut out: Vec<String> = self
            .files
            .keys()
            .filter(|path| path.starts_with(&prefix))
            .cloned()
            .collect();
        out.sort();
        Ok(out)
    }

    fn exists(&self, rel_path: &str) -> bool {
        self.files.contains_key(rel_path)
    }
}

/// The pre-read table excerpts [`plan_v2_files`] needs. The local path reads them with the
/// synchronous [`Table`] API, the remote path asynchronously — the planning itself is pure.
pub struct V2PlanInputs {
    /// One row of the `frames` table, all columns (for the schema).
    pub frames_sample: RecordBatch,

    /// The [`episodes_meta_projection`] columns of the whole `episodes` table.
    pub episodes_meta: RecordBatch,

    /// The `episode_index` column of the whole `frames` table.
    pub frame_episode_indices: RecordBatch,

    /// The [`video_blob_columns`] of the `episodes` table.
    pub video_columns: Vec<String>,

    pub total_frames: usize,
}

/// The `episodes`-table columns needed as [`V2PlanInputs::episodes_meta`].
pub fn episodes_meta_projection(episodes_columns: &[String]) -> Vec<String> {
    let mut projection = vec!["episode_index".to_owned()];
    for optional in ["task_index", "fps"] {
        if episodes_columns.iter().any(|c| c == optional) {
            projection.push(optional.to_owned());
        }
    }
    projection
}

/// The per-episode MP4 segment blob columns of the `episodes` table.
pub fn video_blob_columns(episodes_columns: &[String]) -> Vec<String> {
    episodes_columns
        .iter()
        .filter(|name| name.ends_with(VIDEO_BLOB_SUFFIX))
        .cloned()
        .collect()
}

/// Lay out the virtual `LeRobot` v2 dataset: the synthesized metadata files plus, per episode,
/// where its data parquet rows and video blobs come from.
pub fn plan_v2_files(inputs: &V2PlanInputs) -> anyhow::Result<HashMap<String, SynthFile>> {
    let V2PlanInputs {
        frames_sample,
        episodes_meta,
        frame_episode_indices,
        video_columns,
        total_frames,
    } = inputs;

    let frame_episode_indices = int64_column(frame_episode_indices, "episode_index")?;
    let episode_ranges = contiguous_ranges(frame_episode_indices);

    let episode_indices = int64_column(episodes_meta, "episode_index")?;
    let task_indices = episodes_meta
        .column_by_name("task_index")
        .map(|_| int64_column(episodes_meta, "task_index"))
        .transpose()?;

    let fps = episodes_meta
        .column_by_name("fps")
        .map(|c| {
            use arrow::array::AsArray as _;
            use arrow::datatypes::{Float32Type, Int32Type, Int64Type};
            match c.data_type() {
                DataType::Int32 => c.as_primitive::<Int32Type>().value(0) as f32,
                DataType::Int64 => c.as_primitive::<Int64Type>().value(0) as f32,
                DataType::Float32 => c.as_primitive::<Float32Type>().value(0),
                _ => 30.0,
            }
        })
        .unwrap_or(30.0);

    let total_episodes = episode_indices.len();

    let unique_tasks: BTreeSet<i64> = match &task_indices {
        Some(tasks) => tasks.iter().flatten().collect(),
        None => std::iter::once(0).collect(),
    };

    let features = features_json(frames_sample, video_columns);

    // The `{…}` tokens are LeRobot's path-template syntax, not Rust formatting.
    #[expect(clippy::literal_string_with_formatting_args)]
    let data_path = "data/chunk-{episode_chunk:03d}/episode_{episode_index:06d}.parquet";
    #[expect(clippy::literal_string_with_formatting_args)]
    let video_path = "videos/chunk-{episode_chunk:03d}/{video_key}/episode_{episode_index:06d}.mp4";
    let info = serde_json::json!({
        "codebase_version": "v2.1",
        "robot_type": null,
        "total_episodes": total_episodes,
        "total_frames": total_frames,
        "total_tasks": unique_tasks.len(),
        "total_videos": total_episodes * video_columns.len(),
        "total_chunks": 1,
        "chunks_size": total_episodes.max(1),
        "data_path": data_path,
        "video_path": if video_columns.is_empty() { serde_json::Value::Null } else { video_path.into() },
        "fps": fps,
        "features": features,
    });

    let mut files: HashMap<String, SynthFile> = HashMap::new();
    let mut episodes_jsonl = String::new();
    let mut tasks_jsonl = String::new();

    for task in &unique_tasks {
        writeln!(
            tasks_jsonl,
            "{{\"task_index\":{task},\"task\":\"Task {task}\"}}"
        )
        .ok();
    }

    for row in 0..total_episodes {
        let episode = episode_indices.value(row);
        let Some(&(offset, len)) = episode_ranges.get(&episode) else {
            re_log::warn_once!(
                "Episode {episode} of the Lance dataset has no rows in the frames table"
            );
            continue;
        };
        let task = task_indices.as_ref().map_or(0, |t| t.value(row));

        writeln!(
            episodes_jsonl,
            "{{\"episode_index\":{episode},\"tasks\":[\"Task {task}\"],\"length\":{len}}}"
        )
        .ok();

        files.insert(
            format!("data/chunk-000/episode_{episode:06}.parquet"),
            SynthFile::EpisodeData { offset, len },
        );
        for column in video_columns {
            let video_key = column.trim_end_matches(VIDEO_BLOB_SUFFIX);
            files.insert(
                format!("videos/chunk-000/{video_key}/episode_{episode:06}.mp4"),
                SynthFile::EpisodeVideo {
                    row: row as u64,
                    column: column.clone(),
                },
            );
        }
    }

    files.insert(
        "meta/info.json".to_owned(),
        SynthFile::Static(Bytes::from(serde_json::to_vec(&info)?)),
    );
    files.insert(
        "meta/episodes.jsonl".to_owned(),
        SynthFile::Static(Bytes::from(episodes_jsonl)),
    );
    files.insert(
        "meta/tasks.jsonl".to_owned(),
        SynthFile::Static(Bytes::from(tasks_jsonl)),
    );

    Ok(files)
}

/// The v2 `features` map of info.json, derived from the frames schema plus the video columns.
fn features_json(
    frames_sample: &RecordBatch,
    video_columns: &[String],
) -> serde_json::Map<String, serde_json::Value> {
    let mut features = serde_json::Map::new();

    for field in frames_sample.schema().fields() {
        let name = field.name();
        if re_lerobot::common::LEROBOT_DATASET_IGNORED_COLUMNS.contains(&name.as_str()) {
            continue;
        }
        let feature = match field.data_type() {
            DataType::Float32 => {
                serde_json::json!({"dtype": "float32", "shape": [1], "names": null})
            }
            DataType::Float64 => {
                serde_json::json!({"dtype": "float64", "shape": [1], "names": null})
            }
            DataType::Int64 if name == "task_index" => {
                serde_json::json!({"dtype": "int64", "shape": [1], "names": null})
            }
            DataType::FixedSizeList(inner, size)
                if matches!(inner.data_type(), DataType::Float32 | DataType::Float64) =>
            {
                let dtype = if inner.data_type() == &DataType::Float32 {
                    "float32"
                } else {
                    "float64"
                };
                serde_json::json!({"dtype": dtype, "shape": [size], "names": null})
            }
            _ => continue, // bools, strings, … — nothing the viewer plots today
        };
        features.insert(name.clone(), feature);
    }

    for column in video_columns {
        let video_key = column.trim_end_matches(VIDEO_BLOB_SUFFIX);
        // Height/width are not knowable without demuxing; nothing reads them for videos.
        features.insert(
            video_key.to_owned(),
            serde_json::json!({"dtype": "video", "shape": [0, 0, 3], "names": null}),
        );
    }

    features
}

/// Map each distinct value of a (sorted, contiguous) `episode_index` column to its row range
/// `(offset, len)`.
fn contiguous_ranges(episode_indices: &Int64Array) -> BTreeMap<i64, (u64, u64)> {
    let mut ranges: BTreeMap<i64, (u64, u64)> = BTreeMap::new();
    let mut current: Option<(i64, u64)> = None;

    for (i, episode) in episode_indices.iter().enumerate() {
        let Some(episode) = episode else { continue };
        if current.is_none_or(|(cur, _)| cur != episode) {
            if let Some((cur, start)) = current {
                ranges.insert(cur, (start, i as u64 - start));
            }
            current = Some((episode, i as u64));
        }
    }
    if let Some((cur, start)) = current {
        ranges.insert(cur, (start, episode_indices.len() as u64 - start));
    }

    ranges
}

fn int64_column<'a>(batch: &'a RecordBatch, name: &str) -> anyhow::Result<&'a Int64Array> {
    batch
        .column_by_name(name)
        .and_then(|c| c.as_any().downcast_ref::<Int64Array>())
        .with_context(|| format!("Expected an int64 column named `{name}`"))
}
