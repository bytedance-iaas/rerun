//! Reading Lance-format `LeRobot` datasets.
//!
//! The entry points build a virtual `LeRobot` file system ([`re_lerobot::vfs::LeRobotFs`]) on top
//! of the Lance tables, so the existing v2/v3 dataset-to-chunk conversion code consumes Lance
//! datasets unchanged:
//!
//! * [`meta_dir::LanceMetaDirFs`] presents a [`re_lerobot::lance::LanceDatasetLayout::LeRobotMetaDir`]
//!   dataset as a `LeRobot` **v3** directory: `meta/` passes through verbatim, episode data
//!   parquet files are synthesized from `frames.lance`, and video files are served from the
//!   `videos.lance` blob column.
//! * [`episode_tables::LanceEpisodeTablesFs`] presents a
//!   [`re_lerobot::lance::LanceDatasetLayout::EpisodeTables`] dataset as a `LeRobot` **v2**
//!   directory: all metadata is synthesized from the `episodes`/`frames` tables, and per-episode
//!   videos are served from the `episodes` table's MP4 segment blobs.
//!
//! This crate is native-only: the `lance` crate does not compile to wasm.

pub mod episode_tables;
pub mod meta_dir;
pub mod tables;

pub use tables::{BlobHandle, Table};

use std::sync::LazyLock;

/// The tokio runtime driving lance's async IO.
///
/// The `LeRobotFs` interface is synchronous, so lance calls are bridged with [`block_on`].
static RUNTIME: LazyLock<tokio::runtime::Runtime> = LazyLock::new(|| {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .thread_name("re_lance")
        .enable_all()
        .build()
        .expect("failed to build the re_lance tokio runtime")
});

/// Run a lance future to completion from synchronous code.
///
/// Safe to call both from plain threads and from within a multi-threaded tokio runtime
/// (via `block_in_place`); calling from a `current_thread` tokio runtime panics.
pub(crate) fn block_on<F: std::future::Future>(future: F) -> F::Output {
    if tokio::runtime::Handle::try_current().is_ok() {
        tokio::task::block_in_place(|| RUNTIME.block_on(future))
    } else {
        RUNTIME.block_on(future)
    }
}

/// Mirror of `lerobot-lancedb`'s column-name sanitization: dots become underscores.
pub(crate) fn sanitize_column_name(feature_key: &str) -> String {
    feature_key.replace('.', "_")
}
