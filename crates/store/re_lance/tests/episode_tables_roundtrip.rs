//! Round-trip test of the episode-tables virtual file system over a freshly written Lance dataset.

#![expect(clippy::unwrap_used)] // tests may panic

use std::collections::HashMap;
use std::sync::Arc;

use arrow::array::{
    FixedSizeListArray, Float32Array, Int32Array, Int64Array, LargeBinaryArray, RecordBatch,
    RecordBatchIterator,
};
use arrow::datatypes::{DataType, Field, Schema};

use re_lance::episode_tables::LanceEpisodeTablesFs;
use re_lerobot::vfs::Blob;

fn write_lance_table(path: &std::path::Path, batch: RecordBatch) {
    let schema = batch.schema();
    let reader = RecordBatchIterator::new([Ok(batch)], schema);
    tokio::runtime::Runtime::new()
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

fn fixture(root: &std::path::Path) {
    let tables = root.join("data");

    // frames: episodes 0 (2 frames) and 1 (1 frame).
    let state_values = Float32Array::from(vec![0.0, 0.1, 1.0, 1.1, 2.0, 2.1]);
    let state = FixedSizeListArray::new(
        Arc::new(Field::new("item", DataType::Float32, true)),
        2,
        Arc::new(state_values),
        None,
    );
    let frames = RecordBatch::try_from_iter([
        (
            "observation_state",
            Arc::new(state) as arrow::array::ArrayRef,
        ),
        ("episode_index", Arc::new(Int64Array::from(vec![0, 0, 1]))),
        ("frame_index", Arc::new(Int64Array::from(vec![0, 1, 0]))),
        ("index", Arc::new(Int64Array::from(vec![0, 1, 2]))),
        ("task_index", Arc::new(Int64Array::from(vec![0, 0, 1]))),
    ])
    .unwrap();
    write_lance_table(&tables.join("frames.lance"), frames);

    // episodes: one MP4 segment blob per episode.
    let episodes_schema = Arc::new(Schema::new_with_metadata(
        vec![
            Field::new("episode_index", DataType::Int64, false),
            Field::new("task_index", DataType::Int64, false),
            Field::new("fps", DataType::Int32, false),
            blob_field("cam_video_blob"),
        ],
        HashMap::default(),
    ));
    let episodes = RecordBatch::try_new_with_options(
        episodes_schema.clone(),
        vec![
            Arc::new(Int64Array::from(vec![0, 1])),
            Arc::new(Int64Array::from(vec![0, 1])),
            Arc::new(Int32Array::from(vec![10, 10])),
            Arc::new(LargeBinaryArray::from_opt_vec(vec![
                Some(b"fake-mp4-segment-0".as_slice()),
                Some(b"fake-mp4-segment-1".as_slice()),
            ])),
        ],
        &arrow::array::RecordBatchOptions::default().with_row_count(Some(2)),
    )
    .unwrap();
    write_lance_table(&tables.join("episodes.lance"), episodes);

    // A videos table exists too in real datasets, but the fs only reads frames + episodes.
    let videos = RecordBatch::try_from_iter([(
        "video_key",
        Arc::new(arrow::array::StringArray::from(vec!["cam"])) as arrow::array::ArrayRef,
    )])
    .unwrap();
    write_lance_table(&tables.join("videos.lance"), videos);
}

fn read_full(fs: &dyn re_lerobot::vfs::LeRobotFs, path: &str) -> Vec<u8> {
    match fs.read(path).unwrap() {
        Blob::Full(bytes) => bytes.to_vec(),
        Blob::Sparse(_) => panic!("expected a full blob for {path}"),
    }
}

#[test]
fn episode_tables_fs_serves_synthesized_v2_dataset() {
    let dir = tempfile::tempdir().unwrap();
    fixture(dir.path());

    let fs = LanceEpisodeTablesFs::open_local(dir.path()).unwrap();

    // info.json parses and describes the synthesized v2 dataset.
    let info: serde_json::Value =
        serde_json::from_slice(&read_full(fs.as_ref(), "meta/info.json")).unwrap();
    assert_eq!(info["total_episodes"], 2);
    assert_eq!(info["total_frames"], 3);
    assert_eq!(info["fps"], 10.0);
    assert_eq!(info["features"]["observation_state"]["dtype"], "float32");
    assert_eq!(info["features"]["observation_state"]["shape"][0], 2);
    assert_eq!(info["features"]["cam"]["dtype"], "video");
    assert_eq!(info["features"]["task_index"]["dtype"], "int64");

    // episodes.jsonl: one line per episode, with lengths from the frames table.
    let episodes = String::from_utf8(read_full(fs.as_ref(), "meta/episodes.jsonl")).unwrap();
    let lines: Vec<&str> = episodes.lines().collect();
    assert_eq!(lines.len(), 2);
    assert!(lines[0].contains("\"length\":2"), "{}", lines[0]);
    assert!(lines[1].contains("\"length\":1"), "{}", lines[1]);

    // tasks.jsonl: both task indices present.
    let tasks = String::from_utf8(read_full(fs.as_ref(), "meta/tasks.jsonl")).unwrap();
    assert_eq!(tasks.lines().count(), 2);

    // Episode data parquet: episode 0 has exactly its 2 frames, with all columns.
    let parquet_bytes = read_full(fs.as_ref(), "data/chunk-000/episode_000000.parquet");
    let reader = parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(
        bytes::Bytes::from(parquet_bytes),
    )
    .unwrap()
    .build()
    .unwrap();
    let batches: Vec<RecordBatch> = reader.collect::<Result<_, _>>().unwrap();
    assert_eq!(batches.len(), 1, "one row group, read as one batch");
    assert_eq!(batches[0].num_rows(), 2);
    assert!(batches[0].column_by_name("observation_state").is_some());
    assert!(batches[0].column_by_name("frame_index").is_some());

    // Videos come straight from the episodes table's blob column.
    assert_eq!(
        read_full(fs.as_ref(), "videos/chunk-000/cam/episode_000001.mp4"),
        b"fake-mp4-segment-1"
    );

    // Listing and existence behave like a real directory tree.
    assert!(fs.exists("meta/tasks.jsonl"));
    assert!(!fs.exists("data/chunk-000/episode_000002.parquet"));
    assert_eq!(fs.list_files("videos").unwrap().len(), 2);
}
