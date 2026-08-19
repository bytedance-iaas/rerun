//! TOS/S3 backend for the remote `LeRobot` dataset streaming in [`crate::lerobot_remote`].

use std::ops::Range;

use re_log_channel::LogReceiver;

use super::TosLocation;
use super::client::{TosClient, TosCredentials};
use crate::lerobot_remote::{DatasetStore, ListedFile};

/// Everything needed to open a `LeRobot` dataset stored in TOS.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TosDatasetSource {
    pub location: TosLocation,
    pub credentials: TosCredentials,

    /// Where to look up / upload converted rrds; `None` disables the artifacts store.
    pub rrd_artifacts: Option<crate::rrd_artifacts::RrdArtifactsConfig>,
}

/// [`DatasetStore`] over a TOS/S3-compatible bucket prefix.
struct TosStore {
    client: TosClient,
    location: TosLocation,
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

    async fn file_size(&self, rel_path: &str) -> anyhow::Result<u64> {
        let key = format!("{}{rel_path}", self.location.prefix);
        let objects = self.client.list_objects(&key).await?;
        objects
            .iter()
            .find(|obj| obj.key == key)
            .map(|obj| obj.size)
            .ok_or_else(|| {
                // The listing itself succeeded, so the object definitively does not exist —
                // callers use the typed 404 to tell this apart from transient fetch trouble.
                anyhow::Error::new(crate::lerobot_remote::HttpStatusError(404))
                    .context(format!("No such object: {key}"))
            })
    }

    async fn get_range_once(&self, rel_path: &str, range: Range<u64>) -> anyhow::Result<Vec<u8>> {
        let key = format!("{}{rel_path}", self.location.prefix);
        self.client.get_object_once(&key, Some(range)).await
    }
}

/// Open a `LeRobot` dataset (or a single data file) in TOS as a streaming log source.
pub fn stream_lerobot_dataset(source: TosDatasetSource) -> LogReceiver {
    let TosDatasetSource {
        mut location,
        credentials,
        rrd_artifacts,
    } = source;
    // Remembered for the Diagnose deep link into the curation console: the console
    // asks for URL + region, and this is the last spot where the region (baked into
    // the credentials by the open dialog) and the final URL are both in hand.
    let region = credentials.region();
    let client = TosClient::new(credentials, location.bucket.clone());

    // A path to a single file (e.g. tos://bucket/path/recording.mcap) is downloaded and run
    // through the regular importers instead of the LeRobot dataset pipeline.
    // The rrd artifacts store only covers LeRobot episodes, not loose files.
    if let Some(file_name) = location.split_off_file_name() {
        let url = format!("{location}{file_name}");
        crate::lerobot_remote::remember_dataset_region(&url, &region);
        return crate::lerobot_remote::stream_remote_file(
            TosStore { client, location },
            file_name,
            url,
        );
    }

    crate::lerobot_remote::remember_dataset_region(&location.to_string(), &region);
    crate::lerobot_remote::stream_lerobot_dataset(
        TosStore { client, location },
        rrd_artifacts,
        crate::lerobot_remote::StreamMode::Viewer,
    )
}

/// Headless pre-conversion of a `LeRobot` dataset in TOS (`rerun rrd-convert`):
/// convert every episode whose artifact is missing or stale, upload, finish.
pub fn convert_lerobot_dataset(source: TosDatasetSource) -> LogReceiver {
    let TosDatasetSource {
        location,
        credentials,
        rrd_artifacts,
    } = source;
    let client = TosClient::new(credentials, location.bucket.clone());
    crate::lerobot_remote::stream_lerobot_dataset(
        TosStore { client, location },
        rrd_artifacts,
        crate::lerobot_remote::StreamMode::ConvertOnly,
    )
}
