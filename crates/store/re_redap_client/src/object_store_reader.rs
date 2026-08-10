//! Async byte-range access to single objects in object storage.
//!
//! This is the client-side bridge that lets RRDs living in object storage (volcengine TOS,
//! AWS S3, `MinIO`, …) flow through the existing reader-generic decoding stack —
//! [`re_log_encoding::read_rrd_footer`] and [`re_log_encoding::RrdChunkProvider`] — without
//! modifying it: each read translates to one ranged `GET` against the store.
//!
//! Callers are expected to batch their reads. The RRD chunk reader already does: it sorts
//! requested chunks by byte offset and coalesces nearby spans into single reads, so one
//! `load_chunks` batch typically costs a handful of range requests.
//!
//! ## Configuration
//!
//! For `tos://` and `s3://` URLs, credentials and endpoint come from the environment. The
//! standard AWS variables are resolved first ([`object_store::aws::AmazonS3Builder::from_env`]:
//! `AWS_ACCESS_KEY_ID`, `AWS_SECRET_ACCESS_KEY`, `AWS_DEFAULT_REGION`, `AWS_ENDPOINT`), then the
//! `TOS_*` overrides used across our cloud deployment take precedence: `TOS_ENDPOINT`,
//! `TOS_REGION`, `TOS_ACCESS_KEY`, `TOS_SECRET_KEY`, and `TOS_S3_PATH_STYLE` (set to `1` for
//! path-style stores like `MinIO`; the default is virtual-hosted style, which is what TOS
//! expects). This mirrors the server's `tos://` registration path so the same environment works
//! on both ends.
//!
//! `file://` URLs read from the local filesystem, mainly so the whole direct-read path can be
//! exercised end-to-end without network access.

use std::sync::Arc;

use bytes::Bytes;
use object_store::path::Path as ObjectPath;
use object_store::{ObjectStore, ObjectStoreExt as _};
use url::Url;

/// Construction/access errors for [`ObjectStoreReader`].
#[derive(thiserror::Error, Debug)]
pub enum ObjectStoreReaderError {
    #[error("unsupported object-store URL scheme '{scheme}': {url}")]
    UnsupportedScheme { scheme: String, url: Url },

    #[error("invalid object-store URL '{url}': {reason}")]
    InvalidUrl { url: Url, reason: String },

    #[error("object not found: {url}")]
    NotFound { url: Url },

    #[error("failed to access object store: {source}\nURL: {url}")]
    Access {
        url: Url,
        source: Box<object_store::Error>,
    },
}

/// An async, read-only view of a single object in an object store.
///
/// Implements [`re_async::AsyncReadAt`]: stateless positional reads, one ranged `GET` per
/// requested span. The object's size is fetched once (a `HEAD` request) at construction.
pub struct ObjectStoreReader {
    store: Arc<dyn ObjectStore>,
    location: ObjectPath,
    url: Url,

    /// Total object size, from the `HEAD` request at construction.
    size: u64,
}

impl std::fmt::Debug for ObjectStoreReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ObjectStoreReader")
            .field("url", &self.url.as_str())
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

impl ObjectStoreReader {
    /// Whether `scheme` is served by this reader.
    pub fn handles_scheme(scheme: &str) -> bool {
        ["tos", "s3", "file"]
            .iter()
            .any(|s| scheme.eq_ignore_ascii_case(s))
    }

    /// Open `url`, verifying that the object exists and fetching its size (one `HEAD` request).
    pub async fn open(url: &Url) -> Result<Self, ObjectStoreReaderError> {
        let (store, location) = build_store(url)?;
        Self::open_in(store, location, url.clone()).await
    }

    /// Open a location in an already-built store. Mainly for tests (e.g. an in-memory store).
    pub async fn open_in(
        store: Arc<dyn ObjectStore>,
        location: ObjectPath,
        url: Url,
    ) -> Result<Self, ObjectStoreReaderError> {
        let head = store.head(&location).await.map_err(|source| match source {
            object_store::Error::NotFound { .. } => {
                ObjectStoreReaderError::NotFound { url: url.clone() }
            }
            source => ObjectStoreReaderError::Access {
                url: url.clone(),
                source: Box::new(source),
            },
        })?;

        Ok(Self {
            store,
            location,
            url,
            size: head.size,
        })
    }

    /// A second, independent handle to the same object (shared client).
    ///
    /// Reads are stateless, so this is a plain clone. No extra `HEAD` request — objects are
    /// immutable for the lifetime of a registration, so the size is reused.
    pub fn reopen(&self) -> Self {
        Self {
            store: Arc::clone(&self.store),
            location: self.location.clone(),
            url: self.url.clone(),
            size: self.size,
        }
    }

    /// The URL this reader was opened with.
    pub fn url(&self) -> &Url {
        &self.url
    }

    /// Total size of the object in bytes.
    pub fn size(&self) -> u64 {
        self.size
    }
}

/// Stateless positional reads: each call translates to one ranged `GET` against the store,
/// which matches the batching the RRD chunk reader already does upstream of this.
#[async_trait::async_trait]
impl re_async::AsyncReadAt for ObjectStoreReader {
    async fn read_exact_at(&self, offset: u64, len: usize) -> std::io::Result<Bytes> {
        let end = offset.checked_add(len as u64).ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "read range overflows u64")
        })?;
        if end > self.size {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                format!(
                    "read of {len} bytes at {offset} past end of object ({} bytes)\nURL: {}",
                    self.size, self.url
                ),
            ));
        }

        let bytes = self
            .store
            .get_range(&self.location, offset..end)
            .await
            .map_err(|err| {
                std::io::Error::other(format!(
                    "range read at {offset} failed: {err}\nURL: {}",
                    self.url
                ))
            })?;

        if bytes.len() != len {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                format!(
                    "range read at {offset} returned {} bytes, expected {len}\nURL: {}",
                    bytes.len(),
                    self.url
                ),
            ));
        }
        Ok(bytes)
    }

    async fn size(&self) -> std::io::Result<u64> {
        Ok(self.size)
    }
}

// ---

/// Build the [`ObjectStore`] client and in-store path for `url`.
///
/// See the module docs for the supported schemes and the environment variables consulted.
pub fn build_store(
    url: &Url,
) -> Result<(Arc<dyn ObjectStore>, ObjectPath), ObjectStoreReaderError> {
    match url.scheme() {
        // tos://bucket/key… and s3://bucket/key… — same S3-compatible client either way.
        "tos" | "s3" => {
            let bucket = url
                .host_str()
                .filter(|host| !host.is_empty())
                .ok_or_else(|| ObjectStoreReaderError::InvalidUrl {
                    url: url.clone(),
                    reason: "missing bucket name".to_owned(),
                })?;

            let store = s3_compatible_store(bucket, url)?;

            let location =
                ObjectPath::from_url_path(url.path().trim_start_matches('/')).map_err(|err| {
                    ObjectStoreReaderError::InvalidUrl {
                        url: url.clone(),
                        reason: err.to_string(),
                    }
                })?;

            Ok((store, location))
        }

        "file" => {
            let path = url
                .to_file_path()
                .map_err(|()| ObjectStoreReaderError::InvalidUrl {
                    url: url.clone(),
                    reason: "not a valid local file path".to_owned(),
                })?;

            let location = ObjectPath::from_absolute_path(&path).map_err(|err| {
                ObjectStoreReaderError::InvalidUrl {
                    url: url.clone(),
                    reason: err.to_string(),
                }
            })?;

            Ok((
                Arc::new(object_store::local::LocalFileSystem::new()),
                location,
            ))
        }

        scheme => Err(ObjectStoreReaderError::UnsupportedScheme {
            scheme: scheme.to_owned(),
            url: url.clone(),
        }),
    }
}

/// Build an S3-compatible client for the given bucket from the environment.
///
/// Mirrors the server's `tos://` registration path (`re_server`'s cloud storage module): start
/// from the standard `AWS_*` environment, then apply the `TOS_*` overrides.
fn s3_compatible_store(
    bucket: &str,
    url: &Url,
) -> Result<Arc<dyn ObjectStore>, ObjectStoreReaderError> {
    // TOS's S3-compatible endpoint uses virtual-hosted style ({bucket}.{endpoint});
    // path-style is opt-in for MinIO-like setups.
    let path_style = env_non_empty("TOS_S3_PATH_STYLE").is_some_and(|v| v != "0");

    let mut builder = object_store::aws::AmazonS3Builder::from_env().with_bucket_name(bucket);

    if let Some(endpoint) = env_non_empty("TOS_ENDPOINT") {
        // In virtual-hosted style `object_store` uses the endpoint as-is (it does not
        // prepend the bucket itself), so splice the bucket into the host here.
        let endpoint = if path_style {
            endpoint
        } else {
            let endpoint_url =
                Url::parse(&endpoint).map_err(|err| ObjectStoreReaderError::InvalidUrl {
                    url: url.clone(),
                    reason: format!("invalid TOS_ENDPOINT {endpoint:?}: {err}"),
                })?;
            let host = endpoint_url.host_str().unwrap_or_default();
            let port = endpoint_url
                .port()
                .map(|p| format!(":{p}"))
                .unwrap_or_default();
            format!("{}://{bucket}.{host}{port}", endpoint_url.scheme())
        };
        builder = builder.with_endpoint(endpoint);
    }
    if let Some(region) = env_non_empty("TOS_REGION") {
        builder = builder.with_region(region);
    }
    if let Some(access_key) = env_non_empty("TOS_ACCESS_KEY") {
        builder = builder.with_access_key_id(access_key);
    }
    if let Some(secret_key) = env_non_empty("TOS_SECRET_KEY") {
        builder = builder.with_secret_access_key(secret_key);
    }

    builder = builder.with_virtual_hosted_style_request(!path_style);

    builder
        .build()
        .map(|store| Arc::new(store) as _)
        .map_err(|err| ObjectStoreReaderError::Access {
            url: url.clone(),
            source: Box::new(err),
        })
}

fn env_non_empty(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

#[cfg(test)]
mod tests {
    use object_store::memory::InMemory;
    use re_async::AsyncReadAt as _;

    use super::*;

    async fn reader_over(bytes: &[u8]) -> ObjectStoreReader {
        let store = Arc::new(InMemory::new());
        let location = ObjectPath::from("test/object.bin");
        store
            .put(&location, Bytes::copy_from_slice(bytes).into())
            .await
            .expect("put failed");
        ObjectStoreReader::open_in(
            store,
            location,
            Url::parse("s3://bucket/test").expect("url"),
        )
        .await
        .expect("open failed")
    }

    #[tokio::test]
    async fn positional_reads() {
        let data: Vec<u8> = (0..=255).collect();
        let reader = reader_over(&data).await;
        assert_eq!(reader.size(), 256);

        // Read from the start.
        let buf = reader.read_exact_at(0, 10).await.expect("read failed");
        assert_eq!(&buf[..], &data[0..10]);

        // An interior span.
        let buf = reader.read_exact_at(100, 10).await.expect("read failed");
        assert_eq!(&buf[..], &data[100..110]);

        // The tail.
        let buf = reader.read_exact_at(252, 4).await.expect("read failed");
        assert_eq!(&buf[..], &data[252..]);

        // Reading past EOF is an error.
        let err = reader
            .read_exact_at(250, 10)
            .await
            .expect_err("must not read past EOF");
        assert_eq!(err.kind(), std::io::ErrorKind::UnexpectedEof);
        let err = reader
            .read_exact_at(10_000, 1)
            .await
            .expect_err("must not read past EOF");
        assert_eq!(err.kind(), std::io::ErrorKind::UnexpectedEof);
    }

    #[tokio::test]
    async fn reopen_reads_same_object() {
        let data = b"hello world".to_vec();
        let a = reader_over(&data).await;
        let b = a.reopen();

        let buf = a.read_exact_at(6, 5).await.expect("read failed");
        assert_eq!(&buf[..], b"world");

        let buf = b.read_exact_at(0, 5).await.expect("read failed");
        assert_eq!(&buf[..], b"hello");
    }

    #[tokio::test]
    async fn missing_object_is_not_found() {
        let store: Arc<dyn ObjectStore> = Arc::new(InMemory::new());
        let err = ObjectStoreReader::open_in(
            store,
            ObjectPath::from("nope"),
            Url::parse("s3://bucket/nope").expect("url"),
        )
        .await
        .expect_err("open must fail");
        assert!(
            matches!(err, ObjectStoreReaderError::NotFound { .. }),
            "got: {err}"
        );
    }

    #[test]
    fn scheme_handling() {
        assert!(ObjectStoreReader::handles_scheme("tos"));
        assert!(ObjectStoreReader::handles_scheme("s3"));
        assert!(ObjectStoreReader::handles_scheme("file"));
        assert!(!ObjectStoreReader::handles_scheme("memory"));
        assert!(!ObjectStoreReader::handles_scheme("http"));

        let err = build_store(&Url::parse("memory://x/y").expect("url")).expect_err("must fail");
        assert!(matches!(
            err,
            ObjectStoreReaderError::UnsupportedScheme { .. }
        ));
    }
}
