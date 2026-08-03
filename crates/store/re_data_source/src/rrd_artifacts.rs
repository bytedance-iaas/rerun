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
