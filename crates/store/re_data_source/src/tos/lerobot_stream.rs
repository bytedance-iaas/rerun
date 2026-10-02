//! TOS/S3 backend for the remote `LeRobot` dataset streaming in [`crate::lerobot_remote`].

use re_i18n::trf;
use std::ops::Range;

use re_log_channel::LogReceiver;

use super::TosLocation;
use super::client::{TosAccess, TosClient};
use crate::lerobot_remote::{DatasetStore, ListedFile};

/// Everything needed to open a `LeRobot` dataset stored in TOS.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TosDatasetSource {
    pub location: TosLocation,

    /// Local keys, or reads presigned by the Curator console.
    pub access: TosAccess,

    /// Where to look up / upload converted rrds; `None` disables the artifacts store.
    pub rrd_artifacts: Option<crate::rrd_artifacts::RrdArtifactsConfig>,
}

/// [`DatasetStore`] over a TOS/S3-compatible bucket prefix.
struct TosStore {
    client: TosClient,
    location: TosLocation,

    /// Console-signed opens come from a "Visualize" link whose registration already knows
    /// the dataset's format — a non-LeRobot layout (mcap files) is expected, not a surprise.
    console_signed: bool,
}

impl DatasetStore for TosStore {
    fn url(&self) -> String {
        self.location.to_string()
    }

    async fn list(&self) -> anyhow::Result<Vec<ListedFile>> {
        let objects = self.client.list_objects(&self.location.prefix).await?;
        Ok(objects
            .into_iter()
            .filter_map(|obj| {
                obj.key
                    .strip_prefix(&self.location.prefix)
                    .map(|rel| ListedFile {
                        rel_path: rel.to_owned(),
                        size: obj.size,
                        content_id: obj.etag.clone(),
                    })
            })
            .collect())
    }

    async fn list_dir(&self) -> anyhow::Result<Option<crate::lerobot_remote::DirListing>> {
        let dir = self.client.list_dir(&self.location.prefix).await?;
        Ok(Some(crate::lerobot_remote::DirListing {
            files: dir
                .objects
                .into_iter()
                .filter_map(|obj| {
                    obj.key
                        .strip_prefix(&self.location.prefix)
                        // The prefix itself may be listed as a zero-byte directory marker.
                        .filter(|rel| !rel.is_empty())
                        .map(|rel| ListedFile {
                            rel_path: rel.to_owned(),
                            size: obj.size,
                            content_id: obj.etag.clone(),
                        })
                })
                .collect(),
            subdirs: dir
                .subdirs
                .into_iter()
                .filter_map(|key| {
                    key.strip_prefix(&self.location.prefix)
                        .filter(|rel| !rel.is_empty())
                        .map(ToOwned::to_owned)
                })
                .collect(),
            truncated: dir.truncated,
        }))
    }

    async fn file_size(&self, rel_path: &str) -> anyhow::Result<u64> {
        Ok(self.file_stat(rel_path).await?.size)
    }

    async fn file_stat(&self, rel_path: &str) -> anyhow::Result<ListedFile> {
        let key = format!("{}{rel_path}", self.location.prefix);
        let objects = self.client.list_objects(&key).await?;
        objects
            .iter()
            .find(|obj| obj.key == key)
            .map(|obj| ListedFile {
                rel_path: rel_path.to_owned(),
                size: obj.size,
                content_id: obj.etag.clone(),
            })
            .ok_or_else(|| {
                // The listing itself succeeded, so the object definitively does not exist —
                // callers use the typed 404 to tell this apart from transient fetch trouble.
                anyhow::Error::new(crate::lerobot_remote::HttpStatusError(404))
                    .context(trf!("No such object: {key}", "对象不存在：{key}"))
            })
    }

    async fn get_range_once(&self, rel_path: &str, range: Range<u64>) -> anyhow::Result<Vec<u8>> {
        let key = format!("{}{rel_path}", self.location.prefix);
        self.client.get_object_once(&key, Some(range)).await
    }

    fn loose_files_expected(&self) -> bool {
        self.console_signed
    }
}

/// Open a `LeRobot` dataset (or a single data file) in TOS as a streaming log source.
pub fn stream_lerobot_dataset(source: TosDatasetSource) -> LogReceiver {
    let TosDatasetSource {
        mut location,
        access,
        rrd_artifacts,
    } = source;
    // Remembered for the Diagnose deep link into the curation console: the console
    // asks for URL + region, and this is the last spot where the region (baked into
    // the credentials by the open dialog) and the final URL are both in hand. A
    // console-signed dataset also remembers its registration, for share links.
    let region = access.region();
    let curator_dataset_id = access.curator_dataset_id().map(ToOwned::to_owned);
    let console_signed = curator_dataset_id.is_some();
    let remember = |url: &str| {
        crate::lerobot_remote::remember_dataset_region(url, &region);
        if let Some(dataset_id) = &curator_dataset_id {
            crate::lerobot_remote::remember_dataset_curator_id(url, dataset_id);
        }
    };
    let client = TosClient::new(access, location.bucket.clone());

    // A path to a single file (e.g. tos://bucket/path/recording.mcap) is downloaded and run
    // through the regular importers instead of the LeRobot dataset pipeline. Conversion-heavy
    // formats (MCAP) still go through the rrd artifacts store.
    if let Some(file_name) = location.split_off_file_name() {
        let url = format!("{location}{file_name}");
        remember(&url);
        return crate::lerobot_remote::stream_remote_file(
            TosStore {
                client,
                location,
                console_signed,
            },
            file_name,
            url,
            rrd_artifacts,
        );
    }

    remember(&location.to_string());
    crate::lerobot_remote::stream_lerobot_dataset(
        TosStore {
            client,
            location,
            console_signed,
        },
        rrd_artifacts,
        crate::lerobot_remote::StreamMode::Viewer,
    )
}

/// Headless pre-conversion of a `LeRobot` dataset in TOS (`rerun rrd-convert`):
/// convert every episode whose artifact is missing or stale, upload, finish.
pub fn convert_lerobot_dataset(source: TosDatasetSource) -> LogReceiver {
    let TosDatasetSource {
        location,
        access,
        rrd_artifacts,
    } = source;
    let client = TosClient::new(access, location.bucket.clone());
    crate::lerobot_remote::stream_lerobot_dataset(
        TosStore {
            client,
            location,
            // Conversion only handles LeRobot episodes; the flag is never consulted.
            console_signed: false,
        },
        rrd_artifacts,
        crate::lerobot_remote::StreamMode::ConvertOnly,
    )
}
