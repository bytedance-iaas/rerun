//! The Lance virtual store must present a freshly written Lance dataset as a classic
//! v2/v3 `LeRobot` dataset, over a mock "remote" store serving a local directory.

#![cfg(not(target_arch = "wasm32"))]
#![expect(clippy::unwrap_used)] // tests may panic

use std::collections::HashMap;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use arrow::array::{
    FixedSizeListArray, Float32Array, Int64Array, LargeBinaryArray, RecordBatch,
    RecordBatchIterator, StringArray,
};
use arrow::datatypes::{DataType, Field, Schema};
use bytes::Bytes;

use re_data_source::lance_remote::LanceVirtualStore;
use re_data_source::lerobot_remote::{DatasetStore, HttpStatusError, ListedFile};
use re_lerobot::lance::LanceDatasetLayout;

// ---- A local directory served as a "remote" dataset store. ---------------------------------

struct LocalDirStore {
    root: PathBuf,
}

impl DatasetStore for LocalDirStore {
    fn url(&self) -> String {
        format!("mock://{}", self.root.display())
    }

    async fn list(&self) -> anyhow::Result<Vec<ListedFile>> {
        fn walk(root: &Path, dir: &Path, out: &mut Vec<ListedFile>) -> std::io::Result<()> {
            for entry in std::fs::read_dir(dir)? {
                let entry = entry?;
                let path = entry.path();
                if path.is_dir() {
                    walk(root, &path, out)?;
                } else if let Ok(rel) = path.strip_prefix(root) {
                    out.push(ListedFile {
                        rel_path: rel.to_string_lossy().replace('\\', "/"),
                        size: entry.metadata()?.len(),
                        content_id: None,
                    });
                }
            }
            Ok(())
        }

        let mut out = Vec::new();
        walk(&self.root, &self.root, &mut out)?;
        Ok(out)
    }

    async fn file_size(&self, rel_path: &str) -> anyhow::Result<u64> {
        std::fs::metadata(self.root.join(rel_path))
            .map(|meta| meta.len())
            .map_err(|err| {
                anyhow::Error::new(HttpStatusError(404))
                    .context(err)
                    .context(format!("no such file: {rel_path}"))
            })
    }

    async fn get_range_once(&self, rel_path: &str, range: Range<u64>) -> anyhow::Result<Vec<u8>> {
        let contents = std::fs::read(self.root.join(rel_path))?;
        let start = usize::try_from(range.start)?.min(contents.len());
        let end = usize::try_from(range.end)?.min(contents.len());
        Ok(contents[start..end].to_vec())
    }
}

// ---- Fixture building. ---------------------------------------------------------------------

fn write_lance_table(path: &Path, batch: RecordBatch) {
    let schema = batch.schema();
    let reader = RecordBatchIterator::new([Ok(batch)], schema);
    tokio::runtime::Runtime::new() // NOLINT: a test owns its process
        .unwrap()
        .block_on(lance::Dataset::write(reader, path.to_str().unwrap(), None))
        .unwrap();
}

fn blob_field(name: &str) -> Field {
    Field::new(name, DataType::LargeBinary, true).with_metadata(HashMap::from([(
        "lance-encoding:blob".to_owned(),
        "true".to_owned(),
    )]))
}

fn state_column(values: Vec<f32>) -> FixedSizeListArray {
    FixedSizeListArray::new(
        Arc::new(Field::new("item", DataType::Float32, true)),
        2,
        Arc::new(Float32Array::from(values)),
        None,
    )
}

/// A layout-A dataset (`lerobot-lancedb` style): `meta/` directory + frames/videos tables.
#[expect(clippy::literal_string_with_formatting_args)] // LeRobot path templates, not Rust formatting
fn meta_dir_fixture(root: &Path) {
    // frames.lance: 3 frames of episode 0, column name sanitized (dot → underscore).
    let frames = RecordBatch::try_from_iter([
        (
            "observation_state",
            Arc::new(state_column(vec![0.0, 0.1, 1.0, 1.1, 2.0, 2.1])) as arrow::array::ArrayRef,
        ),
        ("episode_index", Arc::new(Int64Array::from(vec![0, 0, 0]))),
        ("frame_index", Arc::new(Int64Array::from(vec![0, 1, 2]))),
        ("index", Arc::new(Int64Array::from(vec![0, 1, 2]))),
        ("task_index", Arc::new(Int64Array::from(vec![0, 0, 0]))),
    ])
    .unwrap();
    write_lance_table(&root.join("frames.lance"), frames);

    // videos.lance: one file of one camera, the "mp4" is a recognizable byte string.
    let videos_schema = Arc::new(Schema::new_with_metadata(
        vec![
            Field::new("video_key", DataType::Utf8, false),
            Field::new("chunk_index", DataType::Int64, false),
            Field::new("file_index", DataType::Int64, false),
            blob_field("video_bytes"),
        ],
        HashMap::default(),
    ));
    let videos = RecordBatch::try_new_with_options(
        videos_schema,
        vec![
            Arc::new(StringArray::from(vec!["observation.images.cam"])),
            Arc::new(Int64Array::from(vec![0])),
            Arc::new(Int64Array::from(vec![0])),
            Arc::new(LargeBinaryArray::from_opt_vec(vec![Some(
                b"fake-mp4-bytes-0123456789".as_slice(),
            )])),
        ],
        &arrow::array::RecordBatchOptions::default().with_row_count(Some(1)),
    )
    .unwrap();
    write_lance_table(&root.join("videos.lance"), videos);

    // meta/: verbatim v3 metadata.
    std::fs::create_dir_all(root.join("meta/episodes/chunk-000")).unwrap();
    let info = serde_json::json!({
        "codebase_version": "v3.0",
        "fps": 10,
        "features": {
            "observation.state": {"dtype": "float32", "shape": [2], "names": null},
            "observation.images.cam": {
                "dtype": "video", "shape": [4, 4, 3],
                "names": ["height", "width", "channels"],
            },
        },
        "total_episodes": 1,
        "total_frames": 3,
        "total_tasks": 1,
        "chunks_size": 1000,
        "data_path": "data/chunk-{chunk_index:03d}/file-{file_index:03d}.parquet",
        "video_path": "videos/{video_key}/chunk-{chunk_index:03d}/file-{file_index:03d}.mp4",
        "storage_format": "lance",
    });
    std::fs::write(
        root.join("meta/info.json"),
        serde_json::to_vec(&info).unwrap(),
    )
    .unwrap();

    // meta/episodes parquet: one episode spanning rows 0..3 of data file (0, 0).
    let episodes_meta = RecordBatch::try_from_iter([
        (
            "episode_index",
            Arc::new(Int64Array::from(vec![0])) as arrow::array::ArrayRef,
        ),
        ("data/chunk_index", Arc::new(Int64Array::from(vec![0]))),
        ("data/file_index", Arc::new(Int64Array::from(vec![0]))),
        ("dataset_from_index", Arc::new(Int64Array::from(vec![0]))),
        ("dataset_to_index", Arc::new(Int64Array::from(vec![3]))),
    ])
    .unwrap();
    std::fs::write(
        root.join("meta/episodes/chunk-000/file-000.parquet"),
        re_lance::meta_dir::write_parquet(&episodes_meta).unwrap(),
    )
    .unwrap();
}

fn read_virtual(
    runtime: &tokio::runtime::Runtime,
    store: &LanceVirtualStore<LocalDirStore>,
    rel_path: &str,
) -> Vec<u8> {
    runtime
        .block_on(async {
            let size = store.file_size(rel_path).await?;
            store.get_range_once(rel_path, 0..size).await
        })
        .unwrap()
}

// ---- Tests. --------------------------------------------------------------------------------

#[test]
fn virtual_store_presents_meta_dir_layout_as_classic_v3() {
    let dir = tempfile::tempdir().unwrap();
    meta_dir_fixture(dir.path());

    let runtime = tokio::runtime::Runtime::new().unwrap(); // NOLINT: a test owns its process
    let store = runtime
        .block_on(LanceVirtualStore::open(
            Arc::new(LocalDirStore {
                root: dir.path().to_path_buf(),
            }),
            LanceDatasetLayout::LeRobotMetaDir,
        ))
        .unwrap();

    // The virtual info.json no longer claims the Lance storage format.
    let info: serde_json::Value =
        serde_json::from_slice(&read_virtual(&runtime, &store, "meta/info.json")).unwrap();
    assert_eq!(info.get("storage_format"), None);
    assert_eq!(info["codebase_version"], "v3.0");

    // The listing contains exactly one entry per path, with the virtual files' exact sizes.
    let listing = runtime.block_on(store.list()).unwrap();
    let mut seen = std::collections::HashSet::new();
    for file in &listing {
        assert!(
            seen.insert(file.rel_path.clone()),
            "duplicate: {}",
            file.rel_path
        );
    }
    let data_entry = listing
        .iter()
        .find(|file| file.rel_path == "data/chunk-000/file-000.parquet")
        .expect("synthesized data parquet missing from listing");
    assert!(
        data_entry.content_id.is_some(),
        "virtual files carry a content id"
    );

    // The synthesized parquet has the episode's rows, with the dotted column name restored.
    let parquet_bytes = read_virtual(&runtime, &store, "data/chunk-000/file-000.parquet");
    assert_eq!(data_entry.size, parquet_bytes.len() as u64);
    let reader = parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(
        Bytes::from(parquet_bytes),
    )
    .unwrap()
    .build()
    .unwrap();
    let batches: Vec<RecordBatch> = reader.collect::<Result<_, _>>().unwrap();
    assert_eq!(batches[0].num_rows(), 3);
    assert!(batches[0].column_by_name("observation.state").is_some());
    assert!(batches[0].column_by_name("observation_state").is_none());

    // The video is served from the blob column, including ranged reads.
    let video_path = "videos/observation.images.cam/chunk-000/file-000.mp4";
    assert_eq!(
        read_virtual(&runtime, &store, video_path),
        b"fake-mp4-bytes-0123456789"
    );
    let range = runtime
        .block_on(store.get_range_once(video_path, 5..13))
        .unwrap();
    assert_eq!(range, b"mp4-byte");

    // Unknown paths fall through to the inner store (meta/) or honestly 404.
    assert!(
        runtime
            .block_on(store.file_size("meta/episodes/chunk-000/file-000.parquet"))
            .is_ok()
    );
    assert!(runtime.block_on(store.file_size("nope.bin")).is_err());

    // The dataset version the native reader reports must be reproducible from the manifest
    // file names alone — that is how the wasm artifacts fallback computes the identical
    // fingerprint salt without decoding Lance.
    let manifest_names = |table: &str| -> Vec<String> {
        std::fs::read_dir(dir.path().join(table).join("_versions"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect()
    };
    let version_of = |table: &str| {
        re_lerobot::lance::lance_version_from_manifest_paths(
            manifest_names(table).iter().map(|s| s.as_str()),
        )
        .unwrap()
    };
    assert_eq!(
        store.dataset_version(),
        Some(format!(
            "lance:{}:{}",
            version_of("frames.lance"),
            version_of("videos.lance")
        ))
    );
}

/// A layout-B dataset (`lance-format` style): frames/episodes tables under `data/`.
fn episode_tables_fixture(root: &Path) {
    let tables = root.join("data");

    let frames = RecordBatch::try_from_iter([
        (
            "observation_state",
            Arc::new(state_column(vec![0.0, 0.1, 1.0, 1.1, 2.0, 2.1])) as arrow::array::ArrayRef,
        ),
        ("episode_index", Arc::new(Int64Array::from(vec![0, 0, 1]))),
        ("frame_index", Arc::new(Int64Array::from(vec![0, 1, 0]))),
        ("index", Arc::new(Int64Array::from(vec![0, 1, 2]))),
        ("task_index", Arc::new(Int64Array::from(vec![0, 0, 1]))),
    ])
    .unwrap();
    write_lance_table(&tables.join("frames.lance"), frames);

    let episodes_schema = Arc::new(Schema::new_with_metadata(
        vec![
            Field::new("episode_index", DataType::Int64, false),
            Field::new("task_index", DataType::Int64, false),
            blob_field("cam_video_blob"),
        ],
        HashMap::default(),
    ));
    let episodes = RecordBatch::try_new_with_options(
        episodes_schema,
        vec![
            Arc::new(Int64Array::from(vec![0, 1])),
            Arc::new(Int64Array::from(vec![0, 1])),
            Arc::new(LargeBinaryArray::from_opt_vec(vec![
                Some(b"fake-mp4-segment-0".as_slice()),
                Some(b"fake-mp4-segment-1".as_slice()),
            ])),
        ],
        &arrow::array::RecordBatchOptions::default().with_row_count(Some(2)),
    )
    .unwrap();
    write_lance_table(&tables.join("episodes.lance"), episodes);
}

/// Layout B: reuse the fixture shape of `re_lance`'s own round-trip test, through the store.
#[test]
fn virtual_store_presents_episode_tables_layout_as_classic_v2() {
    let dir = tempfile::tempdir().unwrap();
    episode_tables_fixture(dir.path());

    let runtime = tokio::runtime::Runtime::new().unwrap(); // NOLINT: a test owns its process
    let store = runtime
        .block_on(LanceVirtualStore::open(
            Arc::new(LocalDirStore {
                root: dir.path().to_path_buf(),
            }),
            LanceDatasetLayout::EpisodeTables,
        ))
        .unwrap();

    let info: serde_json::Value =
        serde_json::from_slice(&read_virtual(&runtime, &store, "meta/info.json")).unwrap();
    assert_eq!(info["codebase_version"], "v2.1");
    assert_eq!(info["total_episodes"], 2);

    let episodes_jsonl =
        String::from_utf8(read_virtual(&runtime, &store, "meta/episodes.jsonl")).unwrap();
    assert_eq!(episodes_jsonl.lines().count(), 2);

    // Per-episode parquet is synthesized lazily with an exact size.
    let parquet_path = "data/chunk-000/episode_000000.parquet";
    let size = runtime.block_on(store.file_size(parquet_path)).unwrap();
    let parquet_bytes = read_virtual(&runtime, &store, parquet_path);
    assert_eq!(size, parquet_bytes.len() as u64);

    assert_eq!(
        read_virtual(&runtime, &store, "videos/chunk-000/cam/episode_000001.mp4"),
        b"fake-mp4-segment-1"
    );
}

// ---- The dataset index (Phase 4: browser direct reading). ----------------------------------

/// The index must describe the virtual dataset byte-exactly: embedded metadata matches the
/// virtual files, exports match the synthesized parquet, and every video Range entry must
/// yield the same bytes when read from the raw dataset file as the virtual video serves.
#[test]
fn dataset_index_is_byte_exact_for_both_layouts() {
    use re_data_source::lance_index::{IndexSource, dataset_salt};
    use re_lerobot::lance::LanceDatasetLayout;

    let runtime = tokio::runtime::Runtime::new().unwrap(); // NOLINT: a test owns its process

    for layout in [
        LanceDatasetLayout::LeRobotMetaDir,
        LanceDatasetLayout::EpisodeTables,
    ] {
        let dir = tempfile::tempdir().unwrap();
        match layout {
            LanceDatasetLayout::LeRobotMetaDir => meta_dir_fixture(dir.path()),
            LanceDatasetLayout::EpisodeTables => episode_tables_fixture(dir.path()),
        }

        let inner = || LocalDirStore {
            root: dir.path().to_path_buf(),
        };
        let store = runtime
            .block_on(LanceVirtualStore::open(Arc::new(inner()), layout))
            .unwrap();
        let (manifest, exports) = runtime.block_on(store.build_index()).unwrap();

        // The browser recomputes the salt from the listing's manifest file names — it must
        // match what the native reader stamped into the index.
        let listing = runtime.block_on(inner().list()).unwrap();
        assert_eq!(
            Some(manifest.dataset_version.clone()),
            dataset_salt(&listing, layout),
            "{layout}: salt mismatch between native and listing-derived"
        );

        let raw = inner();
        let mut videos_checked = 0;
        for file in &manifest.files {
            // Whatever the index claims must byte-match what the virtual store serves.
            let virtual_bytes = read_virtual(&runtime, &store, &file.path);
            assert_eq!(virtual_bytes.len() as u64, file.size, "{}", file.path);

            match &file.source {
                IndexSource::Embedded { text } => {
                    assert_eq!(text.as_bytes(), &virtual_bytes[..], "{}", file.path);
                }
                IndexSource::Export => {
                    let export = exports
                        .iter()
                        .find(|(path, _)| path == &file.path)
                        .unwrap_or_else(|| panic!("missing export bytes for {}", file.path));
                    assert_eq!(&export.1[..], &virtual_bytes[..], "{}", file.path);
                }
                IndexSource::Range {
                    source_path,
                    offset,
                } => {
                    let raw_bytes = runtime
                        .block_on(raw.get_range_once(source_path, *offset..*offset + file.size))
                        .unwrap();
                    assert_eq!(raw_bytes, virtual_bytes, "{}", file.path);
                    videos_checked += 1;
                }
            }
        }
        assert!(videos_checked > 0, "{layout}: no video Range entries");
    }
}
