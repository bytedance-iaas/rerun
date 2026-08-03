//! Streaming of `LeRobot` datasets stored in Volcengine TOS (or any S3-compatible object store),
//! straight from the browser or native viewer.
//!
//! The flow ("metadata first, then episodes stream in"):
//! 1. List + fetch the small `meta/` files, parse them into a [`re_importer::lerobot::datasetv3::LeRobotDatasetV3`].
//! 2. Announce every episode as its own (still empty) recording so the panel fills immediately.
//! 3. Fetch episodes one by one — data parquet fully, videos via byte-range requests covering just
//!    that episode's samples — convert with the regular `LeRobot` importer code, and stream the
//!    resulting `LogMsg`s. Selecting an episode in the viewer moves it to the front of the queue.

pub(crate) mod client;
mod lerobot_stream;

pub use client::{TosClient, TosCredentials};
pub use lerobot_stream::{TosDatasetSource, stream_lerobot_dataset};

/// A `tos://bucket/prefix/` dataset location.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TosLocation {
    pub bucket: String,

    /// Key prefix of the dataset root; empty or ending in `/`.
    pub prefix: String,
}

impl TosLocation {
    /// Parse `tos://bucket/prefix/` (also accepts `s3://`).
    pub fn parse(url: &str) -> Option<Self> {
        let rest = url
            .strip_prefix("tos://")
            .or_else(|| url.strip_prefix("s3://"))?;
        let (bucket, prefix) = match rest.split_once('/') {
            Some((bucket, prefix)) => (bucket, prefix),
            None => (rest, ""),
        };
        if bucket.is_empty() {
            return None;
        }

        let mut prefix = prefix.trim_start_matches('/').to_owned();
        // Directory locations get a trailing slash; a last segment with an extension is kept
        // as-is and treated as a single-file location.
        let last_segment_is_file = prefix
            .rsplit('/')
            .next()
            .is_some_and(|segment| segment.contains('.'));
        if !prefix.is_empty() && !prefix.ends_with('/') && !last_segment_is_file {
            prefix.push('/');
        }

        Some(Self {
            bucket: bucket.to_owned(),
            prefix,
        })
    }
}

impl TosLocation {
    /// If this location points at a single file, split off and return the file name,
    /// leaving `self` as the containing directory.
    pub fn split_off_file_name(&mut self) -> Option<String> {
        if self.prefix.is_empty() || self.prefix.ends_with('/') {
            return None;
        }

        let (dir, file_name) = match self.prefix.rsplit_once('/') {
            Some((dir, file_name)) => (format!("{dir}/"), file_name.to_owned()),
            None => (String::new(), std::mem::take(&mut self.prefix)),
        };

        if file_name.contains('.') {
            self.prefix = dir;
            Some(file_name)
        } else {
            None
        }
    }
}

impl std::fmt::Display for TosLocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "tos://{}/{}", self.bucket, self.prefix)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(url: &str) -> (String, String) {
        let loc = TosLocation::parse(url).unwrap();
        (loc.bucket, loc.prefix)
    }

    #[test]
    fn parse_directory_locations() {
        assert_eq!(parse("tos://bucket/a/b/"), ("bucket".into(), "a/b/".into()));
        // `s3://` is accepted too.
        assert_eq!(parse("s3://bucket/a/b/"), ("bucket".into(), "a/b/".into()));
        // A directory without a trailing slash gets one.
        assert_eq!(parse("tos://bucket/dir"), ("bucket".into(), "dir/".into()));
        assert_eq!(parse("tos://bucket/a/b"), ("bucket".into(), "a/b/".into()));
        // Bucket-only: empty prefix.
        assert_eq!(parse("tos://bucket"), ("bucket".into(), String::new()));
        assert_eq!(parse("tos://bucket/"), ("bucket".into(), String::new()));
        // Leading slashes in the prefix are trimmed.
        assert_eq!(parse("tos://bucket//a/"), ("bucket".into(), "a/".into()));
    }

    #[test]
    fn parse_keeps_single_file_locations_verbatim() {
        // A last segment with an extension is treated as a file — no trailing slash added.
        assert_eq!(
            parse("tos://bucket/path/file.parquet"),
            ("bucket".into(), "path/file.parquet".into())
        );
    }

    #[test]
    fn parse_rejects_bad_input() {
        assert_eq!(TosLocation::parse("http://x/y"), None);
        assert_eq!(TosLocation::parse("bucket/prefix"), None);
        assert_eq!(TosLocation::parse("tos://"), None); // empty bucket
        assert_eq!(TosLocation::parse("tos:///prefix"), None); // empty bucket
    }

    #[test]
    fn display_prepends_scheme() {
        let loc = TosLocation {
            bucket: "b".into(),
            prefix: "a/".into(),
        };
        assert_eq!(loc.to_string(), "tos://b/a/");
        assert_eq!(
            TosLocation {
                bucket: "b".into(),
                prefix: String::new(),
            }
            .to_string(),
            "tos://b/"
        );
    }

    #[test]
    fn split_off_file_name_splits_only_files() {
        let mut loc = TosLocation::parse("tos://bucket/path/file.parquet").unwrap();
        assert_eq!(loc.split_off_file_name().as_deref(), Some("file.parquet"));
        assert_eq!(loc.prefix, "path/");

        // A directory location has nothing to split off.
        let mut dir = TosLocation::parse("tos://bucket/path/").unwrap();
        assert_eq!(dir.split_off_file_name(), None);
        assert_eq!(dir.prefix, "path/");
    }
}
