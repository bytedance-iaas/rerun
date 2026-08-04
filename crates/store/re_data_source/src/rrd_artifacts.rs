//! The rrd artifacts store: converted `.rrd` files kept in a TOS/S3 bucket.
//!
//! Converting a remote `LeRobot` episode is expensive (several source fetches + parsing +
//! transcoding) but deterministic: same sources + same converter = same rrd. So the viewer
//! uploads each converted episode to the artifacts bucket, and later opens fetch the ready-made
//! rrd instead of re-converting — across all three viewer packagings, connecting to the bucket
//! directly (no relay server). The bucket is the ground truth for converted output: artifacts
//! are never evicted, and their addresses can be shared and fetched directly.
//!
//! Correctness rests on the *fingerprint*: a hash over the source files' listing metadata
//! (paths, sizes, ETags/oids — never their content, which would require downloading the very
//! sources the artifact replaces) plus the converter revision. It is stored as S3 user metadata
//! on the artifact itself, so the store is self-describing: one HEAD request decides
//! up-to-date or stale.

use crate::tos::{TosCredentials, TosLocation};

/// Where converted rrds go unless configured otherwise (`tos_rrd_artifacts_url`).
pub const DEFAULT_RRD_ARTIFACTS_URL: &str = "tos://physical-ai-rerun-test/rrd-data/";

/// Setting `tos_rrd_artifacts_url` (or `TOS_RRD_ARTIFACTS_URL`) to this disables the artifacts store.
pub const RRD_ARTIFACTS_OFF: &str = "off";

/// Serde default for `tos_rrd_artifacts_url` config fields: an *absent* key means the default
/// bucket (the artifacts store is on by default); an explicit `""`/`"off"` disables it.
pub fn default_artifacts_url() -> String {
    DEFAULT_RRD_ARTIFACTS_URL.to_owned()
}

/// Bump when the conversion output changes (importer fixes, layout changes, …):
/// every artifact produced by older revisions becomes stale at once.
const ARTIFACT_FORMAT_REV: u32 = 1;

/// The S3 user-metadata key holding the fingerprint (`x-amz-meta-` + this).
pub const FINGERPRINT_METADATA_KEY: &str = "rerun-fingerprint";

/// The S3 user-metadata key holding the source dataset URL (provenance, for humans).
pub const SOURCE_URL_METADATA_KEY: &str = "rerun-source-url";

/// A resolved, enabled rrd-artifacts target.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RrdArtifactsConfig {
    /// Bucket + key prefix the artifacts live under.
    pub location: TosLocation,

    /// Credentials for the artifacts bucket (the regular TOS credentials).
    pub credentials: TosCredentials,

    /// Upload freshly converted episodes (reading the store is always on when this is `Some`).
    pub write_back: bool,

    /// How many artifacts to prefetch at once; `0` = automatic.
    /// See [`resolve_prefetch_items`].
    pub prefetch_items: usize,
}

/// A "delete artifacts" request, waiting for the user to confirm it in the viewer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArtifactDeletionRequest {
    /// The dataset URL (= application id) whose artifacts are affected.
    pub dataset_url: String,

    /// `Some(index)` deletes that one episode's artifact; `None` deletes the whole
    /// dataset's artifacts directory.
    pub episode: Option<usize>,

    /// What will be deleted, for display and for key derivation:
    /// the object URL (episode) or the directory URL (dataset), `tos://bucket/…`.
    pub target_url: String,
}

static PENDING_DELETION: parking_lot::Mutex<Option<ArtifactDeletionRequest>> =
    parking_lot::Mutex::new(None);

/// Deletions currently running in the background: `(dataset_url, episode)`.
static DELETIONS_IN_FLIGHT: parking_lot::Mutex<Vec<(String, Option<usize>)>> =
    parking_lot::Mutex::new(Vec::new());

/// Result of the delete-permission probe, keyed by access key + bucket.
/// An entry with `None` means the probe is still in flight.
static DELETE_PERMISSION: parking_lot::Mutex<Vec<(String, Option<bool>)>> =
    parking_lot::Mutex::new(Vec::new());

fn permission_key(config: &RrdArtifactsConfig) -> String {
    format!(
        "{}:{}",
        config.credentials.access_key, config.location.bucket
    )
}

/// Whether these credentials may delete from the artifacts bucket, once known.
///
/// `None` until a [`probe_delete_permission`] settles; the UI treats that optimistically
/// (worst case, the actual deletion reports the failure).
pub fn delete_permission(config: &RrdArtifactsConfig) -> Option<bool> {
    let key = permission_key(config);
    DELETE_PERMISSION
        .lock()
        .iter()
        .find(|(cached_key, _)| *cached_key == key)
        .and_then(|(_, allowed)| *allowed)
}

/// Find out — once per credentials+bucket — whether deletion is permitted, so the UI
/// can grey delete entries out up front.
///
/// The probe DELETEs a key that does not exist: that mutates nothing (S3 deletion of an
/// absent key is a no-op success), yet still requires — and thus reveals — the delete
/// permission. Network trouble leaves the answer unknown and the probe re-armed.
pub fn probe_delete_permission(config: &RrdArtifactsConfig) {
    let key = permission_key(config);
    {
        let mut cache = DELETE_PERMISSION.lock();
        if cache.iter().any(|(cached_key, _)| *cached_key == key) {
            return; // Already probed (or probing).
        }
        cache.push((key.clone(), None));
    }

    let config = config.clone();
    crate::data_source::spawn_future(async move {
        let client =
            crate::tos::TosClient::new(config.credentials.clone(), config.location.bucket.clone());
        let probe_key = format!("{}_permission-probe-does-not-exist", config.location.prefix);
        let allowed = match client.delete_object(&probe_key).await {
            Ok(()) => Some(true),
            Err(err) => {
                if format!("{err:#}").contains("403") {
                    Some(false)
                } else {
                    None // Network trouble: leave unknown, retry on the next stream start.
                }
            }
        };

        let mut cache = DELETE_PERMISSION.lock();
        cache.retain(|(cached_key, _)| *cached_key != key);
        if allowed.is_some() {
            cache.push((key, allowed));
        }
    });
}

/// Whether a deletion is currently running that covers this target.
///
/// The UI greys the matching menu entries out. A dataset-wide deletion
/// (`episode == None` in flight) covers every episode of that dataset; asking with
/// `episode == None` matches any deletion of the dataset.
pub fn deletion_in_flight(dataset_url: &str, episode: Option<usize>) -> bool {
    DELETIONS_IN_FLIGHT
        .lock()
        .iter()
        .any(|(in_flight_url, in_flight_episode)| {
            in_flight_url == dataset_url
                && match episode {
                    None => true,
                    Some(index) => in_flight_episode.is_none() || *in_flight_episode == Some(index),
                }
        })
}

/// Ask the viewer to confirm (and then perform) an artifact deletion.
///
/// Deletion is destructive, so it never runs directly from the requesting UI element:
/// the viewer picks the request up via [`take_deletion_request`], shows a confirmation
/// dialog, and only then calls [`spawn_deletion`].
pub fn request_deletion(request: ArtifactDeletionRequest) {
    *PENDING_DELETION.lock() = Some(request);
}

/// The confirmation UI polls this once per frame.
pub fn take_deletion_request() -> Option<ArtifactDeletionRequest> {
    PENDING_DELETION.lock().take()
}

/// Perform a confirmed deletion in the background: one object, or everything under the
/// dataset's artifacts directory. Failures only log — nothing in the viewer depends on it.
pub fn spawn_deletion(config: RrdArtifactsConfig, request: ArtifactDeletionRequest) {
    {
        let mut in_flight = DELETIONS_IN_FLIGHT.lock();
        if in_flight
            .iter()
            .any(|(url, episode)| url == &request.dataset_url && *episode == request.episode)
        {
            re_log::debug!(
                "Deletion already in progress\nTarget: {}",
                request.target_url
            );
            return;
        }
        in_flight.push((request.dataset_url.clone(), request.episode));
    }

    crate::data_source::spawn_future(async move {
        let client =
            crate::tos::TosClient::new(config.credentials.clone(), config.location.bucket.clone());
        let bucket_prefix = format!("tos://{}/", config.location.bucket);

        let result = async {
            if request.episode.is_some() {
                let key = request
                    .target_url
                    .strip_prefix(&bucket_prefix)
                    .ok_or_else(|| {
                        anyhow::anyhow!("Artifact URL is not in the configured bucket")
                    })?;
                client.delete_object(key).await?;
                anyhow::Ok(1usize)
            } else {
                let dir = dataset_artifacts_dir(&config.location.prefix, &request.dataset_url);
                let objects = client.list_objects(&dir).await?;
                let mut deleted = 0usize;
                for object in &objects {
                    client.delete_object(&object.key).await?;
                    deleted += 1;
                }
                anyhow::Ok(deleted)
            }
        }
        .await;

        match result {
            Ok(deleted) => {
                re_log::info!(
                    "Deleted {deleted} rrd artifact(s) from the store\nTarget: {}",
                    request.target_url
                );
                crate::lerobot_remote::forget_rrd_artifact_urls(
                    &request.dataset_url,
                    request.episode,
                );
            }
            Err(err) => {
                re_log::warn!(
                    "Failed to delete rrd artifact(s): {err:#}\nTarget: {}",
                    request.target_url
                );
            }
        }

        DELETIONS_IN_FLIGHT
            .lock()
            .retain(|(in_flight_url, in_flight_episode)| {
                !(in_flight_url == &request.dataset_url && *in_flight_episode == request.episode)
            });
    });
}

/// How many artifacts to prefetch concurrently, from the configured value
/// (`rrd_artifacts_prefetch` / `RRD_ARTIFACTS_PREFETCH`).
///
/// `0` (or an absent key) picks the automatic default: 3 in the browser — the ~6
/// connections-per-host budget shared with the per-artifact range parallelism — and 4
/// natively. Explicit values are capped to keep a typo from opening hundreds of
/// connections.
pub fn resolve_prefetch_items(configured: usize) -> usize {
    const AUTO: usize = if cfg!(target_arch = "wasm32") { 3 } else { 4 };
    const MAX: usize = 16;
    match configured {
        0 => AUTO,
        n => n.min(MAX),
    }
}

/// Interpret the configured artifacts-store URL: `""`/`"off"` disables, anything else must parse.
pub fn parse_artifacts_url(configured: &str) -> Option<TosLocation> {
    let trimmed = configured.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case(RRD_ARTIFACTS_OFF) {
        return None;
    }
    let location = TosLocation::parse(trimmed);
    if location.is_none() {
        re_log::warn_once!(
            "Ignoring invalid rrd-artifacts URL (expected tos://bucket/prefix/): {trimmed}"
        );
    }
    location
}

/// The artifacts directory of one source dataset, mirroring its URL for human readability:
/// `tos://src-bucket/data/set-1/` → `<prefix>tos/src-bucket/data/set-1/`.
pub fn dataset_artifacts_dir(artifacts_prefix: &str, source_url: &str) -> String {
    let source_path = source_url
        .replacen("://", "/", 1)
        .trim_matches('/')
        .to_owned();
    format!("{artifacts_prefix}{source_path}/")
}

/// The object key of one item's artifact:
/// `tos://src-bucket/data/set-1/` + `episode_3` → `<prefix>tos/src-bucket/data/set-1/episode_3.rrd`
pub fn object_key(artifacts_prefix: &str, source_url: &str, item_name: &str) -> String {
    format!(
        "{}{item_name}.rrd",
        dataset_artifacts_dir(artifacts_prefix, source_url)
    )
}

/// One source file that an artifact was converted from.
pub struct FingerprintPart<'a> {
    pub rel_path: &'a str,
    pub size: u64,

    /// `ETag` (TOS) or git blob oid (Hugging Face), when the backend provides one.
    pub content_id: Option<&'a str>,
}

/// Hash the source files' listing metadata + the converter revision into an artifact fingerprint.
///
/// Deliberately *not* a content hash — computing one would require downloading the sources,
/// which is exactly what the artifact spares us. Sizes+ETags/oids are what changes when a dataset is
/// re-uploaded. Parts are sorted internally, so call-site order does not matter.
pub fn fingerprint(parts: &mut [FingerprintPart<'_>]) -> String {
    use sha2::{Digest as _, Sha256};

    parts.sort_by(|a, b| a.rel_path.cmp(b.rel_path));

    let mut hasher = Sha256::new();
    hasher.update(format!("rev={ARTIFACT_FORMAT_REV};v={}", env!("CARGO_PKG_VERSION")).as_bytes());
    for part in parts {
        hasher.update(
            format!(
                "\n{}\x00{}\x00{}",
                part.rel_path,
                part.size,
                part.content_id.unwrap_or("")
            )
            .as_bytes(),
        );
    }
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn object_key_mirrors_the_source_url() {
        assert_eq!(
            object_key("rrd-data/", "tos://src-bucket/data/set-1/", "episode_3"),
            "rrd-data/tos/src-bucket/data/set-1/episode_3.rrd"
        );
        assert_eq!(
            object_key("rrd-data/", "hf://org/name", "episode_0"),
            "rrd-data/hf/org/name/episode_0.rrd"
        );
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn prefetch_resolution() {
        assert_eq!(resolve_prefetch_items(0), 4, "0 = automatic (4 natively)");
        assert_eq!(resolve_prefetch_items(2), 2);
        assert_eq!(resolve_prefetch_items(100), 16, "typo guard: capped at 16");
    }

    #[test]
    fn parse_artifacts_url_off_switch() {
        assert_eq!(parse_artifacts_url(""), None);
        assert_eq!(parse_artifacts_url("off"), None);
        assert_eq!(parse_artifacts_url("OFF"), None);
        assert_eq!(
            parse_artifacts_url(DEFAULT_RRD_ARTIFACTS_URL),
            Some(TosLocation {
                bucket: "physical-ai-rerun-test".to_owned(),
                prefix: "rrd-data/".to_owned(),
            })
        );
    }

    #[test]
    fn fingerprint_properties() {
        let fp = |parts: &mut [FingerprintPart<'_>]| fingerprint(parts);

        let base = fp(&mut [
            FingerprintPart {
                rel_path: "a.parquet",
                size: 10,
                content_id: Some("e1"),
            },
            FingerprintPart {
                rel_path: "b.mp4",
                size: 20,
                content_id: None,
            },
        ]);
        let reordered = fp(&mut [
            FingerprintPart {
                rel_path: "b.mp4",
                size: 20,
                content_id: None,
            },
            FingerprintPart {
                rel_path: "a.parquet",
                size: 10,
                content_id: Some("e1"),
            },
        ]);
        assert_eq!(base, reordered, "part order must not matter");

        let size_changed = fp(&mut [
            FingerprintPart {
                rel_path: "a.parquet",
                size: 11,
                content_id: Some("e1"),
            },
            FingerprintPart {
                rel_path: "b.mp4",
                size: 20,
                content_id: None,
            },
        ]);
        assert_ne!(
            base, size_changed,
            "a size change must change the fingerprint"
        );

        let etag_changed = fp(&mut [
            FingerprintPart {
                rel_path: "a.parquet",
                size: 10,
                content_id: Some("e2"),
            },
            FingerprintPart {
                rel_path: "b.mp4",
                size: 20,
                content_id: None,
            },
        ]);
        assert_ne!(
            base, etag_changed,
            "an ETag change must change the fingerprint"
        );
    }
}
