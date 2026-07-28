//! Streaming of remote `LeRobot` datasets into the viewer, generic over the storage backend
//! (Volcengine TOS / S3-compatible buckets, Hugging Face, …).
//!
//! The flow ("metadata first, then episodes stream in"):
//! 1. Fetch `meta/info.json` first and route by `codebase_version` (v2 and v3 are supported).
//!    No full repo listing up front — huge datasets (`LeRobot` v2 stores one file per episode
//!    per camera) would take minutes to list. Locations without `meta/info.json` fall back to
//!    "repo of data files" mode: every supported file (`.mcap`, `.rrd`, images, …) becomes its
//!    own recording. Everything else is rejected with a clear, persistent error notification.
//! 2. Announce episodes/files as their own (still empty) recordings so the panel fills
//!    immediately, each named from metadata alone. Huge datasets are announced in batches of
//!    [`ANNOUNCE_BATCH`]; a trailing "… N more" entry loads the next batch when clicked.
//! 3. Fetch items one by one — v3 videos via byte-range requests covering just that episode's
//!    samples, v2 files and data files whole — convert, and stream the resulting `LogMsg`s.
//!    Selecting an item in the viewer moves it to the front of the queue, and the whole dataset
//!    can be paused/resumed from the recording panel. Individual items can be paused (parked
//!    until clicked again) and re-downloaded; closing an item or the whole dataset cancels the
//!    corresponding downloads for good.

use std::collections::BTreeSet;
use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock};
use std::task::{Poll, Waker};

use anyhow::Context as _;
use bytes::Bytes;
use parking_lot::Mutex;
use re_log_channel::{LogReceiver, LogSender, LogSource};
use re_log_types::{ApplicationId, StoreId};

use re_importer::lerobot_glue::{
    episode_log_msgs, recording_properties_msg, recording_store_info_msg,
};
use re_lerobot::datasetv2::LeRobotDatasetV2;
use re_lerobot::datasetv3::{LeRobotDatasetV3, episode_sample_range, sample_byte_extent};
use re_lerobot::vfs::{Blob, LeRobotFs, MemFs, SparseBlob};
use re_lerobot::{DType, EpisodeIndex};

/// How many episodes/files are announced at once. Datasets with more get a trailing
/// "… N more" entry that loads the next batch when clicked.
const ANNOUNCE_BATCH: usize = 200;

/// The recording id of the synthetic "… N more" entry.
const MORE_RECORDING_ID: &str = "more";

/// The queue sentinel produced by clicking the "… N more" entry.
const MORE_SENTINEL: usize = usize::MAX;

// ----------------------------------------------------------------------------
// Storage abstraction.

/// One file of a remote dataset.
#[derive(Clone, Debug)]
pub struct ListedFile {
    /// Dataset-relative path, e.g. `meta/info.json`.
    pub rel_path: String,

    pub size: u64,
}

/// Read access to the files of one remote `LeRobot` dataset.
///
/// Implementations only need a listing, a size lookup, and a single-attempt ranged read;
/// resuming of truncated responses is handled by the driver.
///
/// The trait is declared per target because the driver task must be `Send` when spawned on a
/// multi-threaded runtime (native), while browser futures are inherently `!Send` (wasm).
#[cfg(not(target_arch = "wasm32"))]
pub trait DatasetStore: Send + Sync {
    /// The dataset URL; doubles as the application id and display name (e.g. `tos://…`, `hf://…`).
    fn url(&self) -> String;

    /// List all files of the dataset.
    ///
    /// Potentially expensive for huge datasets — only used for v3 layouts (few files) and
    /// file-repo mode.
    fn list(&self) -> impl std::future::Future<Output = anyhow::Result<Vec<ListedFile>>> + Send;

    /// The size of a single file.
    fn file_size(
        &self,
        rel_path: &str,
    ) -> impl std::future::Future<Output = anyhow::Result<u64>> + Send;

    /// One ranged GET attempt of `[start, end)`.
    ///
    /// May legitimately return fewer bytes than requested (e.g. proxies silently truncating large
    /// responses) — the driver resumes from where the response stopped.
    fn get_range_once(
        &self,
        rel_path: &str,
        range: Range<u64>,
    ) -> impl std::future::Future<Output = anyhow::Result<Vec<u8>>> + Send;
}

/// Read access to the files of one remote `LeRobot` dataset (see the native declaration above).
#[cfg(target_arch = "wasm32")]
#[expect(async_fn_in_trait)] // only used with static dispatch
pub trait DatasetStore {
    /// The dataset URL; doubles as the application id and display name (e.g. `tos://…`, `hf://…`).
    fn url(&self) -> String;

    /// List all files of the dataset.
    async fn list(&self) -> anyhow::Result<Vec<ListedFile>>;

    /// The size of a single file.
    async fn file_size(&self, rel_path: &str) -> anyhow::Result<u64>;

    /// One ranged GET attempt of `[start, end)`.
    async fn get_range_once(&self, rel_path: &str, range: Range<u64>) -> anyhow::Result<Vec<u8>>;
}

/// GET `[range.start, range.end)` of a file, resuming until complete.
///
/// Intercepting proxies are known to silently truncate large responses (observed: ~200 KB
/// delivered of a 17 MB range in the browser), so short responses are re-requested from where the
/// previous one stopped. Over-long responses (e.g. a proxy turning a range request into a
/// full-body 200) are trimmed. Respects the dataset's pause switch between requests.
async fn fetch_range<S: DatasetStore>(
    store: &S,
    pause: &PauseState,
    rel_path: &str,
    range: Range<u64>,
) -> anyhow::Result<Vec<u8>> {
    let expected = usize::try_from(range.end.saturating_sub(range.start)).unwrap_or_default();
    let mut out: Vec<u8> = Vec::with_capacity(expected);
    let mut pos = range.start;
    let mut empty_responses = 0;

    while pos < range.end {
        pause.wait_while_paused().await;
        if pause.interrupted() {
            anyhow::bail!("Download interrupted\nFile: {rel_path}");
        }

        let bytes = store.get_range_once(rel_path, pos..range.end).await?;
        if bytes.is_empty() {
            empty_responses += 1;
            if empty_responses > 3 {
                anyhow::bail!(
                    "Empty byte-range response at offset {pos} (wanted {pos}..{})\nFile: {rel_path}",
                    range.end
                );
            }
            continue;
        }
        empty_responses = 0;

        let remaining = usize::try_from(range.end - pos).unwrap_or_default();
        let take = bytes.len().min(remaining);
        out.extend_from_slice(&bytes[..take]);
        pos += take as u64;
        pause.add_item_progress(take as u64);

        if pos < range.end {
            re_log::debug!(
                "Byte-range response truncated ({take} bytes) — resuming at offset {pos} \
                 ({} of {expected} bytes so far)\nFile: {rel_path}",
                out.len(),
            );
        }
    }

    Ok(out)
}

/// The largest file we attempt to load in the browser.
///
/// wasm32 is a 32-bit target: a single allocation beyond ~2.1 GB panics with
/// "capacity overflow", and the importers need additional working copies on top.
#[cfg(target_arch = "wasm32")]
const MAX_BROWSER_FILE_BYTES: u64 = 1_500_000_000;

/// Refuse (with a clear error) files that cannot be loaded in the browser.
#[cfg_attr(not(target_arch = "wasm32"), expect(clippy::unnecessary_wraps))] // wasm-only check
fn check_browser_file_size(size: u64, what: &str) -> anyhow::Result<()> {
    #[cfg(target_arch = "wasm32")]
    if size > MAX_BROWSER_FILE_BYTES {
        anyhow::bail!(
            "{what} is {} — too large to load in the browser (limit ~{}).              Use the native Rerun viewer for files this big.",
            re_format::format_bytes(size as _),
            re_format::format_bytes(MAX_BROWSER_FILE_BYTES as _),
        );
    }

    #[cfg(not(target_arch = "wasm32"))]
    let _ = (size, what);

    Ok(())
}

/// Fetch a whole file (size looked up first so truncated responses resume).
async fn fetch_full<S: DatasetStore>(
    store: &S,
    pause: &PauseState,
    rel_path: &str,
) -> anyhow::Result<Vec<u8>> {
    let size = store.file_size(rel_path).await?;
    check_browser_file_size(size, rel_path)?;
    fetch_range(store, pause, rel_path, 0..size).await
}

// ----------------------------------------------------------------------------
// Per-stream registry: prioritization, loading indicator, pause.

/// The queue index encoded in a recording id (`episode_7` or `file_3`).
fn queue_index_from_recording_id(recording_id: &str) -> Option<usize> {
    let n = recording_id
        .strip_prefix("episode_")
        .or_else(|| recording_id.strip_prefix("file_"))?;
    n.parse().ok()
}

/// Live byte counts of the item currently being downloaded.
#[derive(Clone, Copy)]
struct ItemProgress {
    started_nanos: i64,
    bytes_done: u64,

    /// Estimated size of the whole item, when known (v3 episodes, loose files).
    bytes_total: Option<u64>,
}

/// Flow control of one active stream: dataset-level pause, stream cancellation (the user closed
/// the dataset), and aborting the item currently being fetched (per-episode pause/close).
/// Parking is waker-based so it works on wasm too.
#[derive(Default)]
struct PauseState {
    paused: AtomicBool,

    /// The whole stream is cancelled — abort everything and exit.
    cancelled: AtomicBool,

    /// Abort fetching the current item only.
    skip_current: AtomicBool,

    waker: Mutex<Option<Waker>>,

    /// Byte progress of the item currently being fetched (for the UI).
    item_progress: Mutex<Option<ItemProgress>>,
}

impl PauseState {
    fn set_paused(&self, paused: bool) {
        self.paused.store(paused, Ordering::SeqCst);
        if !paused {
            self.wake();
        }
    }

    fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        self.wake();
    }

    fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    fn request_skip(&self) {
        self.skip_current.store(true, Ordering::SeqCst);
        self.wake();
    }

    fn take_skip(&self) -> bool {
        self.skip_current.swap(false, Ordering::SeqCst)
    }

    /// Should the fetch in progress be aborted?
    fn interrupted(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst) || self.skip_current.load(Ordering::SeqCst)
    }

    fn wake(&self) {
        if let Some(waker) = self.waker.lock().take() {
            waker.wake();
        }
    }

    fn begin_item_progress(&self, bytes_total: Option<u64>) {
        *self.item_progress.lock() = Some(ItemProgress {
            started_nanos: re_log_types::Timestamp::now().nanos_since_epoch(),
            bytes_done: 0,
            bytes_total,
        });
    }

    fn add_item_progress(&self, bytes: u64) {
        if let Some(progress) = &mut *self.item_progress.lock() {
            progress.bytes_done += bytes;
        }
    }

    fn end_item_progress(&self) {
        *self.item_progress.lock() = None;
    }

    fn blocked(&self) -> bool {
        self.paused.load(Ordering::SeqCst) && !self.interrupted()
    }

    async fn wait_while_paused(&self) {
        std::future::poll_fn(|cx| {
            if self.blocked() {
                *self.waker.lock() = Some(cx.waker().clone());
                // Re-check to avoid a lost wakeup between the load and the waker registration.
                if self.blocked() {
                    Poll::Pending
                } else {
                    Poll::Ready(())
                }
            } else {
                Poll::Ready(())
            }
        })
        .await;
    }
}

/// The shared state of one active stream.
#[derive(Default)]
struct StreamState {
    /// Episode/file indices the user asked to load first (most recent last).
    requests: Mutex<Vec<usize>>,

    /// Woken whenever a new request lands (used to idle-wait for "load more" clicks).
    requests_waker: Mutex<Option<Waker>>,

    /// Items paused by the user: they stay pending, but auto-advance skips them.
    /// Clicking the item (or its resume button) picks it back up.
    parked: Mutex<BTreeSet<usize>>,

    /// Items given up on after repeated failures, with the failure reason
    /// (shown in the UI); a re-download revives them.
    failed: Mutex<std::collections::BTreeMap<usize, String>>,

    /// Items whose recordings the user closed: forget them entirely.
    cancels: Mutex<Vec<usize>>,

    /// Items to re-download from scratch (also revives failed ones).
    redownloads: Mutex<Vec<usize>>,

    /// A re-download closes the item's old recording first; that close must not be taken for
    /// the user closing the item (which would cancel the re-download). Consumed on close.
    redownload_shield: Mutex<BTreeSet<usize>>,

    pause: PauseState,
}

impl StreamState {
    /// Anything for the item loop to react to?
    fn has_pending_work(&self) -> bool {
        !self.requests.lock().is_empty()
            || !self.cancels.lock().is_empty()
            || !self.redownloads.lock().is_empty()
            || self.pause.is_cancelled()
    }

    fn wake_requests(&self) {
        if let Some(waker) = self.requests_waker.lock().take() {
            waker.wake();
        }
    }

    /// Wait until there is work (returns immediately if there already is some).
    async fn wait_for_request(&self) {
        std::future::poll_fn(|cx| {
            if self.has_pending_work() {
                Poll::Ready(())
            } else {
                *self.requests_waker.lock() = Some(cx.waker().clone());
                if self.has_pending_work() {
                    Poll::Ready(())
                } else {
                    Poll::Pending
                }
            }
        })
        .await;
    }
}

/// All active streams, keyed by application id (the dataset URL).
static ACTIVE_STREAMS: LazyLock<Mutex<ahash::HashMap<String, Arc<StreamState>>>> =
    LazyLock::new(Default::default);

/// Ask the loader of the given recording (if it is a remote dataset item still being loaded)
/// to fetch that item next. Returns true if a matching active stream was found.
pub fn prioritize_episode_for_store(store_id: &StoreId) -> bool {
    let recording_id = store_id.recording_id().as_str();
    let index = if recording_id == MORE_RECORDING_ID {
        MORE_SENTINEL
    } else if let Some(index) = queue_index_from_recording_id(recording_id) {
        index
    } else {
        return false;
    };

    let registry = ACTIVE_STREAMS.lock();
    if let Some(state) = registry.get(store_id.application_id().as_str()) {
        state.requests.lock().push(index);
        state.wake_requests();
        true
    } else {
        false
    }
}

/// Cancel the active stream of this dataset: stop downloading, free its resources.
///
/// Called when the user closes the dataset — without this, the background stream would keep
/// downloading and re-announcing episodes, resurrecting the closed dataset.
pub fn cancel_dataset_stream(application_id: &str) {
    if let Some(state) = ACTIVE_STREAMS.lock().get(application_id) {
        state.pause.cancel();
        state.wake_requests();
    }
}

/// Cancel every active dataset stream (e.g. "close all").
pub fn cancel_all_dataset_streams() {
    for state in ACTIVE_STREAMS.lock().values() {
        state.pause.cancel();
        state.wake_requests();
    }
}

/// The user closed a single episode/file recording: forget that item — don't download it,
/// don't announce it again. Aborts its download if it is the one currently being fetched.
pub fn cancel_episode_for_store(store_id: &StoreId) {
    let Some(index) = queue_index_from_recording_id(store_id.recording_id().as_str()) else {
        return;
    };

    let app_id = store_id.application_id().as_str();
    if let Some(state) = ACTIVE_STREAMS.lock().get(app_id) {
        if state.redownload_shield.lock().remove(&index) {
            // This close is part of a re-download of the item, not the user closing it.
            // NOW the old recording is gone, so it is safe to queue the re-download —
            // queueing it any earlier would race: the stream can re-announce within
            // milliseconds, and this close (processed a frame later) would then delete
            // the re-announced recording for good.
            state.redownloads.lock().push(index);
            state.requests.lock().push(index);
            state.wake_requests();
            return;
        }
        state.cancels.lock().push(index);
        if CURRENTLY_LOADING.lock().get(app_id) == Some(&index) {
            state.pause.request_skip();
        }
        state.wake_requests();
    }
}

/// Pause the download of the item currently being fetched of this dataset.
///
/// The partial download is discarded; clicking the item (or its resume button) restarts it.
pub fn pause_current_item(application_id: &str) {
    if let Some(state) = ACTIVE_STREAMS.lock().get(application_id) {
        state.pause.request_skip();
    }
}

/// Whether this recording is an item paused by the user (resumable by clicking it).
pub fn is_episode_parked(store_id: &StoreId) -> bool {
    let Some(index) = queue_index_from_recording_id(store_id.recording_id().as_str()) else {
        return false;
    };
    ACTIVE_STREAMS
        .lock()
        .get(store_id.application_id().as_str())
        .is_some_and(|state| state.parked.lock().contains(&index))
}

/// Whether this recording is an item that was given up on after repeated download failures.
pub fn is_episode_failed(store_id: &StoreId) -> bool {
    let Some(index) = queue_index_from_recording_id(store_id.recording_id().as_str()) else {
        return false;
    };
    ACTIVE_STREAMS
        .lock()
        .get(store_id.application_id().as_str())
        .is_some_and(|state| state.failed.lock().contains_key(&index))
}

/// Live progress of a downloading item, for the UI.
pub struct DownloadProgress {
    pub bytes_done: u64,

    /// Estimated size of the whole item; `None` when it cannot be known up front.
    pub bytes_total: Option<u64>,

    pub bytes_per_sec: f64,

    /// Estimated seconds remaining (needs a known total and a settled speed).
    pub eta_secs: Option<f64>,
}

/// Byte progress of the item currently being downloaded, if this recording is it.
pub fn episode_download_progress(store_id: &StoreId) -> Option<DownloadProgress> {
    let index = queue_index_from_recording_id(store_id.recording_id().as_str())?;
    let app_id = store_id.application_id().as_str();

    if CURRENTLY_LOADING.lock().get(app_id) != Some(&index) {
        return None;
    }

    let registry = ACTIVE_STREAMS.lock();
    let progress = (*registry.get(app_id)?.pause.item_progress.lock())?;

    let elapsed_secs =
        (re_log_types::Timestamp::now().nanos_since_epoch() - progress.started_nanos) as f64 / 1e9;
    // Speed needs a moment to settle before it means anything.
    let bytes_per_sec = if elapsed_secs > 0.5 {
        progress.bytes_done as f64 / elapsed_secs
    } else {
        0.0
    };
    let eta_secs = match progress.bytes_total {
        Some(total) if bytes_per_sec > 1.0 => {
            Some((total.saturating_sub(progress.bytes_done)) as f64 / bytes_per_sec)
        }
        _ => None,
    };

    Some(DownloadProgress {
        bytes_done: progress.bytes_done,
        bytes_total: progress.bytes_total,
        bytes_per_sec,
        eta_secs,
    })
}

/// Why this item's download was given up on, if it was.
///
/// The recording panel shows this on the failed item (upstream-style: red row + hover reason).
pub fn episode_failure(store_id: &StoreId) -> Option<String> {
    let index = queue_index_from_recording_id(store_id.recording_id().as_str())?;
    ACTIVE_STREAMS
        .lock()
        .get(store_id.application_id().as_str())
        .and_then(|state| state.failed.lock().get(&index).cloned())
}

/// Mark this item for a re-download from scratch, overwriting the previous download.
///
/// This only arms the marker: the caller must close the item's recording alongside, and the
/// close hook ([`cancel_episode_for_store`]) completes the hand-off once the old recording is
/// actually gone. Triggering the stream directly from here would race the (frame-delayed)
/// close, which would then delete the freshly re-announced recording for good.
/// Also revives failed items. Returns true if a matching active stream was found.
pub fn redownload_episode_for_store(store_id: &StoreId) -> bool {
    let Some(index) = queue_index_from_recording_id(store_id.recording_id().as_str()) else {
        return false;
    };

    if let Some(state) = ACTIVE_STREAMS
        .lock()
        .get(store_id.application_id().as_str())
    {
        state.redownload_shield.lock().insert(index);
        true
    } else {
        false
    }
}

/// The item currently being downloaded per active stream, keyed by application id.
static CURRENTLY_LOADING: LazyLock<Mutex<ahash::HashMap<String, usize>>> =
    LazyLock::new(Default::default);

/// Whether the given recording is a remote dataset item that is being downloaded right now.
///
/// The recording panel uses this to show a loading indicator on that item.
pub fn is_episode_loading(store_id: &StoreId) -> bool {
    let Some(index) = queue_index_from_recording_id(store_id.recording_id().as_str()) else {
        return false;
    };

    CURRENTLY_LOADING
        .lock()
        .get(store_id.application_id().as_str())
        == Some(&index)
}

/// Whether a remote dataset stream is currently active for this application id.
///
/// The recording panel uses this to decide whether to offer pause/resume controls.
pub fn is_dataset_streaming(application_id: &str) -> bool {
    ACTIVE_STREAMS.lock().contains_key(application_id)
}

/// Whether the active stream of this application id is paused.
pub fn is_dataset_paused(application_id: &str) -> bool {
    ACTIVE_STREAMS
        .lock()
        .get(application_id)
        .is_some_and(|state| state.pause.paused.load(Ordering::SeqCst))
}

/// Pause or resume the active stream of this application id.
pub fn set_dataset_paused(application_id: &str, paused: bool) {
    if let Some(state) = ACTIVE_STREAMS.lock().get(application_id) {
        state.pause.set_paused(paused);
    }
}

/// Registers the stream state for the lifetime of the returned guard.
struct StreamGuard {
    key: String,
    state: Arc<StreamState>,
}

impl StreamGuard {
    fn new(application_id: &ApplicationId) -> Self {
        let state = Arc::new(StreamState::default());
        ACTIVE_STREAMS
            .lock()
            .insert(application_id.to_string(), state.clone());
        Self {
            key: application_id.to_string(),
            state,
        }
    }

    /// The most recently requested pending item — or the "load more" sentinel — if any.
    fn take_requested(&self, pending: &BTreeSet<usize>) -> Option<usize> {
        let mut requests = self.state.requests.lock();
        while let Some(candidate) = requests.pop() {
            if candidate == MORE_SENTINEL || pending.contains(&candidate) {
                requests.clear();
                return Some(candidate);
            }
        }
        None
    }
}

impl Drop for StreamGuard {
    fn drop(&mut self) {
        ACTIVE_STREAMS.lock().remove(&self.key);
        CURRENTLY_LOADING.lock().remove(&self.key);
    }
}

// ----------------------------------------------------------------------------
// Single-file streaming.

/// Fetch a single remote file (`.rrd`, `.mcap`, image, mesh, …) and load it via all importers.
///
/// `url` is the user-facing address of the file (used as the source name).
pub fn stream_remote_file<S: DatasetStore + 'static>(
    store: S,
    rel_path: String,
    url: String,
) -> LogReceiver {
    let (tx, rx) = re_log_channel::log_channel(LogSource::HttpStream { url: url.clone() });

    crate::data_source::spawn_future(async move {
        let pause = PauseState::default();
        let filename = rel_path.rsplit('/').next().unwrap_or("downloaded_file");

        let bytes = match fetch_full(&store, &pause, &rel_path).await {
            Ok(bytes) => bytes,
            Err(err) => {
                re_log::error!(?url, "Failed to fetch file: {err:#}");
                tx.quit(Some(err.into())).ok();
                return;
            }
        };

        re_log::debug!(
            "Fetched {url} ({}), loading…",
            re_format::format_bytes(bytes.len() as f64)
        );

        let settings = re_importer::ImporterSettings {
            force_store_info: true,
            ..re_importer::ImporterSettings::recommended(re_log_types::RecordingId::random())
        };

        if let Err(err) = re_importer::import_from_file_contents(
            &settings,
            re_log_types::FileSource::Uri,
            &std::path::PathBuf::from(filename),
            std::borrow::Cow::Owned(bytes),
            &tx,
        ) {
            re_log::error!(?url, "Failed to load file: {err}");
            tx.quit(Some(Box::new(err))).ok();
        }
        // On success the importers call `tx.quit(None)` themselves once done.
    });

    rx
}

// ----------------------------------------------------------------------------
// Dataset streaming.

/// Open a remote `LeRobot` dataset (or a repo of data files) as a streaming log source.
///
/// Returns immediately; fetching and conversion run as a background task feeding the returned
/// receiver. Unsupported locations produce a clear, persistent error notification instead of
/// silently doing nothing.
pub fn stream_lerobot_dataset<S: DatasetStore + 'static>(store: S) -> LogReceiver {
    let url = store.url();
    let (tx, rx) = re_log_channel::log_channel(LogSource::HttpStream { url: url.clone() });

    crate::data_source::spawn_future(async move {
        if let Err(err) = run_stream(&store, &tx).await {
            re_log::error!(?url, "Failed to stream dataset: {err:#}");
            tx.quit(Some(err.into())).ok();
        } else {
            tx.quit(None).ok();
        }
    });

    rx
}

/// Just enough of `meta/info.json` to route on the format version.
#[derive(serde::Deserialize)]
struct MinimalInfo {
    codebase_version: String,
}

async fn run_stream<S: DatasetStore>(store: &S, tx: &LogSender) -> anyhow::Result<()> {
    let dataset_url = store.url();

    // ---- 1. Identify the dataset format ----
    // `meta/info.json` is fetched before anything else: no full repo listing (which can take
    // minutes for large v2 datasets with one file per episode per camera). Locations without
    // it fall back to "repo of data files" mode.
    let pause = PauseState::default();
    let info_bytes = match fetch_full(store, &pause, "meta/info.json").await {
        Ok(bytes) => bytes,
        Err(err) => {
            re_log::debug!(
                "No readable meta/info.json ({err:#}); checking for loose data files…\nDataset: {dataset_url}"
            );
            return run_stream_files(store, &dataset_url, tx).await;
        }
    };

    let info: MinimalInfo = serde_json::from_slice(&info_bytes)
        .map_err(|err| anyhow::anyhow!("Failed to parse meta/info.json: {err}"))?;

    let version = info.codebase_version.trim_start_matches('v');
    let major = version.split('.').next().unwrap_or_default();

    let memfs = Arc::new(MemFs::default());
    memfs.insert("meta/info.json", Blob::Full(Bytes::from(info_bytes)));

    match major {
        "2" => run_stream_v2(store, &memfs, &dataset_url, tx).await,
        "3" => run_stream_v3(store, &memfs, &dataset_url, tx).await,
        "1" => anyhow::bail!(
            "This is a LeRobot v1 dataset ({}), which is not supported. \
             Supported versions: v2 and v3.",
            info.codebase_version
        ),
        _ => anyhow::bail!(
            "Unsupported LeRobot dataset version {:?}. Supported versions: v2 and v3.",
            info.codebase_version
        ),
    }
}

/// The per-flavor state the item loop needs.
enum RemoteDataset {
    V2 {
        dataset: Box<LeRobotDatasetV2>,
    },
    V3 {
        dataset: Box<LeRobotDatasetV3>,

        /// File sizes by dataset-relative path (from the listing; needed for video range math).
        sizes: ahash::HashMap<String, u64>,

        video_indexes: ahash::HashMap<String, VideoIndex>,
    },

    /// A repo of loose data files (`.mcap`, `.rrd`, images, …), one recording per file.
    Files {
        files: Vec<ListedFile>,
    },
}

impl RemoteDataset {
    /// The recording-id prefix of items ("episode_" for datasets, "file_" for file repos).
    fn recording_id_prefix(&self) -> &'static str {
        match self {
            Self::V2 { .. } | Self::V3 { .. } => "episode_",
            Self::Files { .. } => "file_",
        }
    }

    fn item_noun(&self) -> &'static str {
        match self {
            Self::V2 { .. } | Self::V3 { .. } => "episodes",
            Self::Files { .. } => "files",
        }
    }

    /// Expected download size of one item, when it can be known up front.
    fn item_total_bytes(&self, index: usize) -> Option<u64> {
        match self {
            Self::V2 { .. } => None, // per-episode file sizes are not listed up front
            Self::V3 { dataset, sizes, .. } => {
                estimate_episode_download_size(dataset, sizes, EpisodeIndex(index))
            }
            Self::Files { files } => files.get(index).map(|file| file.size),
        }
    }
}

async fn run_stream_v2<S: DatasetStore>(
    store: &S,
    memfs: &Arc<MemFs>,
    dataset_url: &str,
    tx: &LogSender,
) -> anyhow::Result<()> {
    // v2 metadata is three small-ish files; episode files are derived from path templates,
    // so no listing is needed at all.
    let pause = PauseState::default();
    for rel in ["meta/episodes.jsonl", "meta/tasks.jsonl"] {
        let bytes = fetch_full(store, &pause, rel).await?;
        memfs.insert(rel, Blob::Full(Bytes::from(bytes)));
    }

    let dataset = LeRobotDatasetV2::from_fs(memfs.clone() as Arc<dyn LeRobotFs>)
        .map_err(|err| anyhow::anyhow!("Not a readable LeRobot v2 dataset: {err}"))?;

    let mut indices: Vec<usize> = dataset
        .metadata
        .iter_episode_indices()
        .map(|ep| ep.0)
        .collect();
    indices.sort_unstable();

    let names: ahash::HashMap<usize, String> = indices
        .iter()
        .map(|&index| {
            let mut name = format!("Episode {index}");
            if let Some(meta) = dataset.metadata.get_episode(EpisodeIndex(index)) {
                use std::fmt::Write as _;
                if let Some(task) = meta.tasks.first() {
                    write!(name, " · {}", truncate_label(task)).ok();
                }
                write!(name, " · {} frames", meta.length).ok();
            }
            (index, name)
        })
        .collect();

    stream_items(
        store,
        RemoteDataset::V2 {
            dataset: Box::new(dataset),
        },
        memfs,
        indices,
        names,
        dataset_url,
        tx,
    )
    .await
}

async fn run_stream_v3<S: DatasetStore>(
    store: &S,
    memfs: &Arc<MemFs>,
    dataset_url: &str,
    tx: &LogSender,
) -> anyhow::Result<()> {
    // v3 packs many episodes per file, so a full listing is cheap — and needed, both for the
    // episode-metadata parquet paths and for the video byte-range math.
    let pause = PauseState::default();
    let all_files = store.list().await?;

    let sizes: ahash::HashMap<String, u64> = all_files
        .iter()
        .map(|file| (file.rel_path.clone(), file.size))
        .collect();

    for file in &all_files {
        if file.rel_path.starts_with("meta/") && !memfs.exists(&file.rel_path) {
            let bytes = fetch_range(store, &pause, &file.rel_path, 0..file.size).await?;
            memfs.insert(file.rel_path.clone(), Blob::Full(Bytes::from(bytes)));
        }
    }

    let dataset = LeRobotDatasetV3::from_fs(memfs.clone() as Arc<dyn LeRobotFs>)
        .map_err(|err| anyhow::anyhow!("Not a readable LeRobot v3 dataset: {err}"))?;

    let mut indices: Vec<usize> = dataset
        .metadata
        .iter_episode_indices()
        .map(|ep| ep.0)
        .collect();
    indices.sort_unstable();

    let names: ahash::HashMap<usize, String> = indices
        .iter()
        .map(|&index| {
            (
                index,
                episode_display_name(&dataset, &sizes, EpisodeIndex(index)),
            )
        })
        .collect();

    stream_items(
        store,
        RemoteDataset::V3 {
            dataset: Box::new(dataset),
            sizes,
            video_indexes: Default::default(),
        },
        memfs,
        indices,
        names,
        dataset_url,
        tx,
    )
    .await
}

/// "Repo of data files" mode: every supported file becomes its own recording.
async fn run_stream_files<S: DatasetStore>(
    store: &S,
    dataset_url: &str,
    tx: &LogSender,
) -> anyhow::Result<()> {
    let all_files = store.list().await.map_err(|err| {
        anyhow::anyhow!("This does not look like a LeRobot dataset, and listing it failed: {err:#}")
    })?;

    // This location may contain whole LeRobot datasets in subdirectories. Their internal
    // chunk files (per-camera mp4s, data parquets) are meaningless to open standalone, so
    // exclude them and point the user at the dataset directories instead.
    let nested_dataset_roots: Vec<String> = all_files
        .iter()
        .filter_map(|file| file.rel_path.strip_suffix("meta/info.json"))
        .filter(|root| !root.is_empty())
        .map(ToOwned::to_owned)
        .collect();

    let files: Vec<ListedFile> = all_files
        .into_iter()
        .filter(|file| {
            if nested_dataset_roots
                .iter()
                .any(|root| file.rel_path.starts_with(root.as_str()))
            {
                return false;
            }
            let extension = file
                .rel_path
                .rsplit('.')
                .next()
                .unwrap_or_default()
                .to_lowercase();
            // Text files (README.md etc.) are technically importable but never what the user
            // came for. The parquet importer is not part of the browser build, so don't list
            // files the click could never load there.
            if matches!(extension.as_str(), "md" | "txt") {
                return false;
            }
            if cfg!(target_arch = "wasm32") && extension == "parquet" {
                return false;
            }
            re_importer::is_supported_file_extension(&extension)
        })
        .collect();

    if !nested_dataset_roots.is_empty() {
        let base = dataset_url.trim_end_matches('/');
        let listed = nested_dataset_roots
            .iter()
            .map(|root| format!("  {base}/{root}"))
            .collect::<Vec<_>>()
            .join("\n");
        re_log::warn!(
            "This location contains {} LeRobot dataset(s) in subdirectories — open them \
             directly for streaming episodes:\n{listed}",
            nested_dataset_roots.len(),
        );
    }

    if files.is_empty() {
        if !nested_dataset_roots.is_empty() {
            let base = dataset_url.trim_end_matches('/');
            let listed = nested_dataset_roots
                .iter()
                .map(|root| format!("  {base}/{root}"))
                .collect::<Vec<_>>()
                .join("\n");
            anyhow::bail!(
                "This location is not a dataset itself, but it contains LeRobot dataset(s) — \
                 open one of these instead:\n{listed}"
            );
        }
        anyhow::bail!(
            "This does not look like a LeRobot dataset (no meta/info.json), and it contains no \
             supported data files either.\n\
             Supported: LeRobot v2/v3 datasets, or files like .rrd, .mcap, images, meshes."
        );
    }

    let indices: Vec<usize> = (0..files.len()).collect();
    let names: ahash::HashMap<usize, String> = files
        .iter()
        .enumerate()
        .map(|(index, file)| {
            (
                index,
                format!(
                    "{} · {}",
                    truncate_path_label(&file.rel_path),
                    re_format::format_bytes(file.size as _)
                ),
            )
        })
        .collect();

    let memfs = Arc::new(MemFs::default()); // unused in file mode, but keeps the loop uniform
    stream_items(
        store,
        RemoteDataset::Files { files },
        &memfs,
        indices,
        names,
        dataset_url,
        tx,
    )
    .await
}

/// Shared announce + prioritized fetch/convert loop for all dataset flavors.
async fn stream_items<S: DatasetStore>(
    store: &S,
    mut remote: RemoteDataset,
    memfs: &Arc<MemFs>,
    indices: Vec<usize>,
    names: ahash::HashMap<usize, String>,
    dataset_url: &str,
    tx: &LogSender,
) -> anyhow::Result<()> {
    // 0.36.0: application ids are restricted to entry-name characters; URLs get
    // normalized (with a short hash suffix) rather than used verbatim.
    let application_id = ApplicationId::new_or_unknown(dataset_url.to_owned());
    let noun = remote.item_noun();
    let id_prefix = remote.recording_id_prefix();

    // Episodes are small and stream in automatically; loose files can be gigabytes each
    // (far beyond browser memory), so they only load when the user clicks them.
    let auto_advance = !matches!(remote, RemoteDataset::Files { .. });

    let total = indices.len();
    re_log::info!("Loaded dataset metadata: {total} {noun}\nDataset: {dataset_url}");

    let guard = StreamGuard::new(&application_id);

    // ---- Announce the first batch so the recording panel fills up immediately ----
    let has_more_entry = total > ANNOUNCE_BATCH;
    if has_more_entry {
        // The trailing "… N more" entry, announced once; its name is updated per batch.
        if tx
            .send(recording_store_info_msg(&application_id, MORE_RECORDING_ID).into())
            .is_err()
        {
            return Ok(());
        }
    }

    let mut announced = 0usize;
    let mut pending: BTreeSet<usize> = BTreeSet::new();

    let announce_next_batch =
        |pending: &mut BTreeSet<usize>, announced: &mut usize| -> anyhow::Result<bool> {
            let batch = &indices[*announced..(*announced + ANNOUNCE_BATCH).min(total)];
            for &index in batch {
                let recording_id = format!("{id_prefix}{index}");
                if tx
                    .send(recording_store_info_msg(&application_id, &recording_id).into())
                    .is_err()
                {
                    return Ok(false); // Receiver hung up.
                }

                let name = names.get(&index).map_or("", String::as_str);
                match recording_properties_msg(&application_id, &recording_id, name) {
                    Ok(msg) => {
                        if tx.send(msg.into()).is_err() {
                            return Ok(false); // Receiver hung up.
                        }
                    }
                    Err(err) => {
                        re_log::warn!("Failed to build recording properties: {err}");
                    }
                }
                pending.insert(index);
            }
            *announced += batch.len();

            if has_more_entry {
                let remaining = total - *announced;
                let more_name = if remaining > 0 {
                    format!(
                        "⋯ {remaining} more {noun} · click to show the next {}",
                        ANNOUNCE_BATCH.min(remaining)
                    )
                } else {
                    format!("✓ all {total} {noun} shown")
                };
                if let Ok(msg) =
                    recording_properties_msg(&application_id, MORE_RECORDING_ID, &more_name)
                    && tx.send(msg.into()).is_err()
                {
                    return Ok(false);
                }
            }

            Ok(true)
        };

    if !announce_next_batch(&mut pending, &mut announced)? {
        return Ok(());
    }

    // ---- Fetch + convert items, user-selected ones first ----
    // Failed items are retried in later rounds (transient network errors are common through
    // proxies); only after a few attempts is an item given up on, with a visible marker.
    const MAX_ATTEMPTS: u32 = 3;

    let mut deferred: Vec<usize> = Vec::new();
    let mut attempts: ahash::HashMap<usize, u32> = Default::default();

    loop {
        guard.state.pause.wait_while_paused().await;
        if guard.state.pause.is_cancelled() {
            re_log::debug!("Dataset stream cancelled\nDataset: {dataset_url}");
            return Ok(());
        }

        // Items whose recordings the user closed: forget them, never announce them again.
        for index in std::mem::take(&mut *guard.state.cancels.lock()) {
            pending.remove(&index);
            guard.state.parked.lock().remove(&index);
            guard.state.failed.lock().remove(&index);
            deferred.retain(|&i| i != index);
            attempts.remove(&index);
        }

        // Re-download requests: reset the item and queue it again. The caller closed the
        // item's recording (dropping the old data), so re-announce it first.
        for index in std::mem::take(&mut *guard.state.redownloads.lock()) {
            if !names.contains_key(&index) {
                continue;
            }
            guard.state.parked.lock().remove(&index);
            guard.state.failed.lock().remove(&index);
            deferred.retain(|&i| i != index);
            attempts.remove(&index);

            let recording_id = format!("{id_prefix}{index}");
            if tx
                .send(recording_store_info_msg(&application_id, &recording_id).into())
                .is_err()
            {
                return Ok(()); // Receiver hung up.
            }
            let name = names.get(&index).map_or("", String::as_str);
            if let Ok(msg) = recording_properties_msg(&application_id, &recording_id, name)
                && tx.send(msg.into()).is_err()
            {
                return Ok(());
            }
            pending.insert(index);
        }

        let has_loadable = {
            let parked = guard.state.parked.lock();
            pending.iter().any(|index| !parked.contains(index))
        };

        // Idle unless something can be loaded without user input (click-to-load mode never
        // auto-loads, so it always idles here until a request lands).
        if !(auto_advance && has_loadable) {
            if !deferred.is_empty() {
                // Start a retry round with everything that failed this round.
                pending.extend(deferred.drain(..));
                continue;
            }
            // Everything eligible is loaded or parked; idle until the user clicks something
            // ("load more", an unloaded item, resume, or re-download). Deliberately no exit
            // when the dataset is fully loaded: re-downloads must keep working.
            guard.state.wait_for_request().await;
            if guard.state.pause.is_cancelled() {
                return Ok(());
            }
        }

        let next = match guard.take_requested(&pending) {
            Some(MORE_SENTINEL) => {
                if !announce_next_batch(&mut pending, &mut announced)? {
                    return Ok(());
                }
                continue;
            }
            Some(index) => index,
            None if auto_advance => {
                let parked = guard.state.parked.lock();
                match pending
                    .iter()
                    .find(|index| !parked.contains(index))
                    .copied()
                {
                    Some(index) => index,
                    None => continue,
                }
            }
            None => continue, // Nothing eligible right now; loop back to the idle wait.
        };
        pending.remove(&next);

        // Picking a parked item (via click or resume button) un-parks it; restore its name.
        if guard.state.parked.lock().remove(&next) {
            let recording_id = format!("{id_prefix}{next}");
            let name = names.get(&next).map_or("", String::as_str);
            if let Ok(msg) = recording_properties_msg(&application_id, &recording_id, name)
                && tx.send(msg.into()).is_err()
            {
                return Ok(());
            }
        }

        let recording_name = names.get(&next).map_or("", String::as_str);

        guard.state.pause.take_skip(); // Drop any stale skip request before starting.
        guard
            .state
            .pause
            .begin_item_progress(remote.item_total_bytes(next));
        CURRENTLY_LOADING
            .lock()
            .insert(application_id.to_string(), next);

        let result = load_one_item(
            store,
            &guard.state.pause,
            &mut remote,
            memfs,
            &application_id,
            next,
            recording_name,
            tx,
        )
        .await;

        CURRENTLY_LOADING.lock().remove(application_id.as_str());
        guard.state.pause.end_item_progress();

        match result {
            Ok(true) => {}
            Ok(false) => return Ok(()), // Receiver hung up.
            Err(err) => {
                if guard.state.pause.is_cancelled() {
                    return Ok(());
                }

                if guard.state.pause.take_skip() {
                    // Aborted on purpose, not failed. If the item was closed (not paused), the
                    // cancel queued alongside the skip is handled at the loop top; parking it
                    // here would resurrect the closed recording.
                    if !guard.state.cancels.lock().contains(&next) {
                        guard.state.parked.lock().insert(next);
                        pending.insert(next);
                        let parked_name = format!("{recording_name} · ⏸ paused");
                        let recording_id = format!("{id_prefix}{next}");
                        if let Ok(msg) =
                            recording_properties_msg(&application_id, &recording_id, &parked_name)
                            && tx.send(msg.into()).is_err()
                        {
                            return Ok(());
                        }
                    }
                    continue;
                }

                let attempt = attempts.entry(next).or_insert(0);
                *attempt += 1;

                if *attempt < MAX_ATTEMPTS {
                    re_log::warn!(
                        "Failed to load item {next} (attempt {attempt}/{MAX_ATTEMPTS}, will retry): {err:#}\nDataset: {dataset_url}"
                    );
                    deferred.push(next);
                } else {
                    re_log::warn!(
                        "Giving up on item {next} after {MAX_ATTEMPTS} attempts: {err:#}\nDataset: {dataset_url}"
                    );
                    // Make the failure visible in the recording panel; the item's
                    // re-download button can revive it. The browser size limit gets its
                    // own wording — "load failed" would read as a transient error.
                    let err_text = format!("{err:#}");
                    let suffix = if err_text.contains("too large to load in the browser") {
                        "⚠ too large for browser — use the native viewer"
                    } else {
                        "⚠ load failed"
                    };
                    guard.state.failed.lock().insert(next, err_text);
                    let failed_name = format!("{recording_name} · {suffix}");
                    let recording_id = format!("{id_prefix}{next}");
                    if let Ok(msg) =
                        recording_properties_msg(&application_id, &recording_id, &failed_name)
                        && tx.send(msg.into()).is_err()
                    {
                        return Ok(());
                    }
                }
            }
        }
    }
}

/// Fetch, convert, and send one item. Returns false if the receiver hung up.
#[expect(clippy::too_many_arguments)]
async fn load_one_item<S: DatasetStore>(
    store: &S,
    pause: &PauseState,
    remote: &mut RemoteDataset,
    memfs: &Arc<MemFs>,
    application_id: &ApplicationId,
    index: usize,
    recording_name: &str,
    tx: &LogSender,
) -> anyhow::Result<bool> {
    // Fetch this item's files, remembering which entries to evict afterwards.
    let mut evict = Vec::new();
    let episode = EpisodeIndex(index);

    let msgs = match remote {
        RemoteDataset::V2 { dataset } => {
            // v2 stores one parquet + one mp4 per camera per episode: fetch them whole.
            let mut rels = vec![dataset.metadata.info.episode_data_path(episode)?];
            for (feature_key, feature) in &dataset.metadata.info.features {
                if feature.dtype == DType::Video {
                    rels.push(dataset.metadata.info.video_path(feature_key, episode)?);
                }
            }

            for rel in rels {
                if !memfs.exists(&rel) {
                    let bytes = fetch_full(store, pause, &rel).await?;
                    memfs.insert(rel.clone(), Blob::Full(Bytes::from(bytes)));
                    evict.push(rel);
                }
            }

            episode_log_msgs(dataset.as_ref(), application_id, episode, recording_name)
        }

        RemoteDataset::V3 {
            dataset,
            sizes,
            video_indexes,
        } => {
            let episode_data = dataset
                .metadata
                .get_episode_data(episode)
                .with_context(|| format!("Unknown episode {index}"))?
                .clone();

            // Episode data parquet (shared by all episodes in the same chunk file; fetched once
            // and kept, since siblings need it too).
            let data_rel = dataset.metadata.info.episode_data_path(&episode_data);
            if !memfs.exists(&data_rel) {
                let size = sizes
                    .get(&data_rel)
                    .copied()
                    .with_context(|| format!("Data file not present in listing: {data_rel}"))?;
                let bytes = fetch_range(store, pause, &data_rel, 0..size).await?;
                memfs.insert(data_rel.clone(), Blob::Full(Bytes::from(bytes)));
            }
            dataset.ensure_episode_data_cached(episode)?;

            // Videos: fetch each file's mp4 index once, then only this episode's byte range.
            for (feature_key, feature) in &dataset.metadata.info.features {
                if feature.dtype != DType::Video {
                    continue;
                }
                let video_rel = dataset
                    .metadata
                    .info
                    .video_path(feature_key, &episode_data)?;

                if !video_indexes.contains_key(&video_rel) {
                    let video_index = fetch_video_index(store, pause, sizes, &video_rel).await?;
                    video_indexes.insert(video_rel.clone(), video_index);
                }
                #[expect(clippy::unwrap_used)] // just inserted above
                let video_index = video_indexes.get(&video_rel).unwrap();

                let (from_ts, to_ts) = episode_data
                    .feature_files
                    .get(feature_key)
                    .map(|f| {
                        (
                            f.from_timestamp.unwrap_or(0.0),
                            f.to_timestamp.unwrap_or(0.0),
                        )
                    })
                    .unwrap_or((0.0, 0.0));

                let sample_range = episode_sample_range(&video_index.video, from_ts, to_ts)
                    .with_context(|| format!("Video: {video_rel}"))?;

                let mut sparse = SparseBlob::new(video_index.total_len);
                for (offset, segment) in &video_index.header_segments {
                    sparse.insert(*offset, segment.clone());
                }

                if let Some(extent) = sample_byte_extent(&video_index.video, &sample_range) {
                    let bytes = fetch_range(store, pause, &video_rel, extent.clone()).await?;
                    re_log::debug!(
                        "Episode {index} video fetch: samples {sample_range:?}, bytes {extent:?} ({} received)\nFile: {video_rel}",
                        bytes.len(),
                    );
                    sparse.insert(extent.start, Bytes::from(bytes));
                }

                memfs.insert(video_rel.clone(), Blob::Sparse(Arc::new(sparse)));
                evict.push(video_rel);
            }

            episode_log_msgs(dataset.as_ref(), application_id, episode, recording_name)
        }

        RemoteDataset::Files { files } => {
            let file = files
                .get(index)
                .with_context(|| format!("Unknown file index {index}"))?;

            check_browser_file_size(file.size, &file.rel_path)?;
            let bytes = fetch_range(store, pause, &file.rel_path, 0..file.size).await?;
            let filename = file
                .rel_path
                .rsplit('/')
                .next()
                .unwrap_or("downloaded_file")
                .to_owned();

            // Route the file into its pre-announced recording: importers use
            // `settings.recommended_store_id()` = (application id, recording id).
            let settings = re_importer::ImporterSettings {
                application_id: Some(application_id.clone()),
                ..re_importer::ImporterSettings::recommended(format!("file_{index}"))
            };

            re_importer::import_from_file_contents(
                &settings,
                re_log_types::FileSource::Uri,
                &std::path::PathBuf::from(filename),
                std::borrow::Cow::Owned(bytes),
                tx,
            )
            .map(|()| Vec::new())
        }
    };

    // Drop this item's files again to keep memory bounded.
    for rel in &evict {
        memfs.remove(rel);
    }

    // An interruption that raced with the end of the download must still not send: the
    // recording may just have been closed, and sending would resurrect it.
    if pause.interrupted() {
        anyhow::bail!("Download interrupted");
    }

    for msg in msgs? {
        if tx.send(msg.into()).is_err() {
            return Ok(false);
        }
    }

    Ok(true)
}

/// Shorten a file path to something panel-friendly, keeping the tail (the informative part —
/// same-named files in different directories must stay distinguishable).
fn truncate_path_label(path: &str) -> String {
    const MAX: usize = 48;
    let count = path.chars().count();
    if count > MAX {
        let tail: String = path.chars().skip(count - (MAX - 1)).collect();
        format!("…{tail}")
    } else {
        path.to_owned()
    }
}

/// Shorten a dataset-authored label to something panel-friendly.
fn truncate_label(label: &str) -> String {
    if label.chars().count() > 40 {
        format!("{}…", label.chars().take(39).collect::<String>())
    } else {
        label.to_owned()
    }
}

/// Human-facing recording label for one v3 episode, built from metadata only:
/// `Episode 3 · ~26 MB · Grab the red cube · 593 frames`.
fn episode_display_name(
    dataset: &LeRobotDatasetV3,
    sizes: &ahash::HashMap<String, u64>,
    episode: EpisodeIndex,
) -> String {
    use std::fmt::Write as _;

    let mut name = format!("Episode {}", episode.0);
    let Some(episode_data) = dataset.metadata.get_episode_data(episode) else {
        return name;
    };

    // Size first: the recording panel truncates long names, and the download size is the most
    // useful bit when deciding what to click.
    if let Some(bytes) = estimate_episode_download_size(dataset, sizes, episode) {
        write!(name, " · ~{}", re_format::format_bytes(bytes as _)).ok();
    }

    if let Some(task) = episode_data.tasks.first() {
        write!(name, " · {}", truncate_label(task)).ok();
    }

    if let Some(frames) = episode_data.length {
        write!(name, " · {frames} frames").ok();
    }

    name
}

/// Estimate how many bytes need to be downloaded for one v3 episode, from metadata alone.
///
/// Videos dominate; each episode covers `[from_timestamp, to_timestamp)` of a shared video file,
/// so its share is that fraction of the file size (using the file's max `to_timestamp` across
/// episodes as the file duration). The data parquet is split evenly across its episodes.
fn estimate_episode_download_size(
    dataset: &LeRobotDatasetV3,
    sizes: &ahash::HashMap<String, u64>,
    episode: EpisodeIndex,
) -> Option<u64> {
    let episode_data = dataset.metadata.get_episode_data(episode)?;
    let mut total: f64 = 0.0;

    for (feature_key, file_metadata) in &episode_data.feature_files {
        let Ok(video_rel) = dataset.metadata.info.video_path(feature_key, episode_data) else {
            continue;
        };
        let Some(&file_size) = sizes.get(&video_rel) else {
            continue;
        };

        let from_ts = file_metadata.from_timestamp.unwrap_or(0.0);
        let to_ts = file_metadata.to_timestamp.unwrap_or(0.0);

        // File "duration" ≈ the max end timestamp among all episodes stored in this file.
        let file_span = dataset
            .metadata
            .episodes
            .values()
            .filter_map(|other| {
                let other_meta = other.feature_files.get(feature_key)?;
                (other_meta.chunk_index == file_metadata.chunk_index
                    && other_meta.file_index == file_metadata.file_index)
                    .then_some(other_meta.to_timestamp.unwrap_or(0.0))
            })
            .fold(0.0_f64, f64::max);

        if file_span > 0.0 && to_ts > from_ts {
            total += (to_ts - from_ts) / file_span * file_size as f64;
        }
    }

    // The episode's share of its data parquet file.
    let data_rel = dataset.metadata.info.episode_data_path(episode_data);
    if let Some(&data_size) = sizes.get(&data_rel) {
        let episodes_in_file = dataset
            .metadata
            .episodes
            .values()
            .filter(|other| {
                other.data_chunk_index == episode_data.data_chunk_index
                    && other.data_file_index == episode_data.data_file_index
            })
            .count()
            .max(1);
        total += data_size as f64 / episodes_in_file as f64;
    }

    (total > 0.0).then_some(total as u64)
}

/// The parsed index of one remote mp4 file: everything except the bulk sample data.
struct VideoIndex {
    total_len: u64,

    /// The fetched byte segments holding all non-`mdat` top-level boxes (incl. `moov`).
    header_segments: Vec<(u64, Bytes)>,

    video: re_video::VideoDataDescription,
}

/// Fetch the parts of a remote mp4 needed to demux it: all top-level boxes except the bulk
/// `mdat` payload. Usually 2 range requests (a head window + the `moov` box, wherever it is).
async fn fetch_video_index<S: DatasetStore>(
    store: &S,
    pause: &PauseState,
    sizes: &ahash::HashMap<String, u64>,
    video_rel: &str,
) -> anyhow::Result<VideoIndex> {
    let total_len = *sizes
        .get(video_rel)
        .with_context(|| format!("Video file not present in listing: {video_rel}"))?;

    let mut sparse = SparseBlob::new(total_len);

    // Head window: typically covers `ftyp` (+ `moov` for faststart files) in one request.
    let head_len = total_len.min(256 * 1024);
    let head = fetch_range(store, pause, video_rel, 0..head_len).await?;
    sparse.insert(0, Bytes::from(head));

    // Walk the top-level box structure, fetching every non-`mdat` box in full.
    let mut pos: u64 = 0;
    while pos + 8 <= total_len {
        if !sparse.contains(pos, 16.min(total_len - pos)) {
            let fetch_end = (pos + 16).min(total_len);
            let bytes = fetch_range(store, pause, video_rel, pos..fetch_end).await?;
            sparse.insert(pos, Bytes::from(bytes));
        }

        let header = sparse
            .get(pos, 16.min(total_len - pos))
            .with_context(|| format!("Failed to read mp4 box header at {pos}: {video_rel}"))?
            .to_vec();

        let size32 = u32::from_be_bytes([header[0], header[1], header[2], header[3]]);
        let box_type = &header[4..8];
        let box_size = match size32 {
            0 => total_len - pos, // box extends to end of file
            1 => {
                if header.len() < 16 {
                    anyhow::bail!("Truncated 64-bit mp4 box header at {pos}: {video_rel}");
                }
                u64::from_be_bytes([
                    header[8], header[9], header[10], header[11], header[12], header[13],
                    header[14], header[15],
                ])
            }
            s => u64::from(s),
        };
        if box_size < 8 || pos + box_size > total_len {
            anyhow::bail!("Corrupt mp4 box (size {box_size}) at {pos}: {video_rel}");
        }

        if box_type != b"mdat" && !sparse.contains(pos, box_size) {
            let bytes = fetch_range(store, pause, video_rel, pos..pos + box_size).await?;
            sparse.insert(pos, Bytes::from(bytes));
        }

        pos += box_size;
    }

    let header_segments = sparse.segments().to_vec();

    let blob = Blob::Sparse(Arc::new(sparse));
    let mut reader = blob.reader();
    let video =
        re_video::VideoDataDescription::load_mp4_from_reader(&mut reader, total_len, video_rel)
            .with_context(|| format!("Failed to parse mp4 index: {video_rel}"))?;

    Ok(VideoIndex {
        total_len,
        header_segments,
        video,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queue_index_from_recording_id_parses_known_prefixes() {
        assert_eq!(queue_index_from_recording_id("episode_7"), Some(7));
        assert_eq!(queue_index_from_recording_id("file_3"), Some(3));
        assert_eq!(queue_index_from_recording_id("episode_007"), Some(7));
    }

    #[test]
    fn queue_index_from_recording_id_rejects_the_rest() {
        assert_eq!(queue_index_from_recording_id("episode_"), None);
        assert_eq!(queue_index_from_recording_id("foo_1"), None);
        assert_eq!(queue_index_from_recording_id("episode_-1"), None);
        assert_eq!(queue_index_from_recording_id("episode_x"), None);
    }
}
