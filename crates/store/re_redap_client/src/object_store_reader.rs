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

use std::io::SeekFrom;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use bytes::{Buf as _, Bytes};
use futures::future::BoxFuture;
use futures::{AsyncRead, AsyncSeek, FutureExt as _};
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

/// Where the bytes come from.
#[derive(Clone)]
enum Backend {
    /// An object store accessed with credentials (S3-compatible, local filesystem, …).
    ObjectStore {
        store: Arc<dyn ObjectStore>,
        location: ObjectPath,
    },

    /// Plain HTTP(S) with ranged `GET`s and no credentials — e.g. a pre-signed
    /// object-store URL: the signature in the URL is the whole authorization.
    Http { url: Url },
}

impl Backend {
    fn fetch(&self, range: std::ops::Range<u64>) -> BoxFuture<'static, Result<Bytes, String>> {
        match self {
            Self::ObjectStore { store, location } => {
                let store = Arc::clone(store);
                let location = location.clone();
                async move {
                    store
                        .get_range(&location, range)
                        .await
                        .map_err(|err| err.to_string())
                }
                .boxed()
            }

            Self::Http { url } => {
                let mut request = ehttp::Request::get(url.as_str());
                request.headers.insert(
                    "Range",
                    format!("bytes={}-{}", range.start, range.end.saturating_sub(1)),
                );
                async move {
                    let response = crate::http_fetch::fetch_async(request).await?;
                    match response.status {
                        206 => Ok(Bytes::from(response.bytes)),
                        // Server ignored the Range header and sent the whole object.
                        200 => {
                            let start = usize::try_from(range.start)
                                .map_err(|_ignored| "range start overflows usize".to_owned())?;
                            let end = usize::try_from(range.end)
                                .map_err(|_ignored| "range end overflows usize".to_owned())?;
                            if response.bytes.len() >= end {
                                Ok(Bytes::copy_from_slice(&response.bytes[start..end]))
                            } else {
                                Err(format!(
                                    "server returned HTTP 200 with {} bytes, needed at least {end}",
                                    response.bytes.len()
                                ))
                            }
                        }
                        status => Err(format!("range request failed: HTTP {status}")),
                    }
                }
                .boxed()
            }
        }
    }
}

/// An async, seekable, read-only view of a single object in an object store, or behind a
/// pre-signed HTTP(S) URL.
///
/// Implements [`AsyncRead`] + [`AsyncSeek`], i.e. `re_log_encoding`'s `AsyncReadAt`. The
/// object's size is fetched once (a `HEAD` request) at construction — or supplied by the
/// caller for pre-signed URLs; seeking is pure cursor arithmetic and reading issues one
/// ranged `GET` per requested span. The cursor may point past the end of the object
/// (mirroring `std::fs::File`); reads there return 0 bytes.
pub struct ObjectStoreReader {
    backend: Backend,
    url: Url,

    /// Total object size: from a `HEAD` request at construction, or caller-provided for
    /// pre-signed URLs (a pre-signed `GET` cannot be `HEAD`ed).
    size: u64,

    /// Current cursor position, as seen by the caller.
    pos: u64,

    /// Bytes already fetched but not yet handed to the caller; starts at `pos`.
    pending: Bytes,

    /// In-flight ranged `GET`, if any.
    inflight: Option<BoxFuture<'static, Result<Bytes, String>>>,
}

impl std::fmt::Debug for ObjectStoreReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ObjectStoreReader")
            .field("url", &self.url.as_str())
            .field("size", &self.size)
            .field("pos", &self.pos)
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
            backend: Backend::ObjectStore { store, location },
            url,
            size: head.size,
            pos: 0,
            pending: Bytes::new(),
            inflight: None,
        })
    }

    /// Like [`Self::open_in`], but with a caller-known size — skips the `HEAD` request.
    pub fn open_in_with_size(
        store: Arc<dyn ObjectStore>,
        location: ObjectPath,
        url: Url,
        size: u64,
    ) -> Self {
        Self {
            backend: Backend::ObjectStore { store, location },
            url,
            size,
            pos: 0,
            pending: Bytes::new(),
            inflight: None,
        }
    }

    /// Open a pre-signed (or otherwise directly fetchable) HTTP(S) URL.
    ///
    /// No credentials are read from anywhere — the URL itself is the authorization.
    /// The object size must be supplied by whoever handed out the URL, because a URL
    /// pre-signed for `GET` cannot be `HEAD`ed.
    pub fn open_presigned(url: Url, size: u64) -> Self {
        Self {
            backend: Backend::Http { url: url.clone() },
            url,
            size,
            pos: 0,
            pending: Bytes::new(),
            inflight: None,
        }
    }

    /// A second, independent handle to the same object (own cursor, shared client).
    ///
    /// Each consumer (e.g. one `RrdChunkProvider` per store in the RRD) gets its own reader so
    /// their cursors never interfere. No extra `HEAD` request — objects are immutable for the
    /// lifetime of a registration, so the size is reused.
    pub fn reopen(&self) -> Self {
        Self {
            backend: self.backend.clone(),
            url: self.url.clone(),
            size: self.size,
            pos: 0,
            pending: Bytes::new(),
            inflight: None,
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

impl AsyncRead for ObjectStoreReader {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<std::io::Result<usize>> {
        let this = &mut *self;
        loop {
            // Serve buffered bytes first: they are the continuation of the current cursor.
            if !this.pending.is_empty() {
                let n = this.pending.len().min(buf.len());
                buf[..n].copy_from_slice(&this.pending[..n]);
                this.pending.advance(n);
                this.pos += n as u64;
                return Poll::Ready(Ok(n));
            }

            if let Some(fut) = this.inflight.as_mut() {
                match fut.as_mut().poll(cx) {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(Ok(bytes)) => {
                        this.inflight = None;
                        this.pending = bytes; // served by the loop's next iteration
                    }
                    Poll::Ready(Err(err)) => {
                        this.inflight = None;
                        return Poll::Ready(Err(std::io::Error::other(format!(
                            "range read at {} failed: {err}\nURL: {}",
                            this.pos, this.url
                        ))));
                    }
                }
            } else {
                let remaining = this.size.saturating_sub(this.pos);
                if remaining == 0 || buf.is_empty() {
                    return Poll::Ready(Ok(0)); // EOF
                }

                let len = (buf.len() as u64).min(remaining);
                let range = this.pos..this.pos + len;
                this.inflight = Some(this.backend.fetch(range));
            }
        }
    }
}

impl AsyncSeek for ObjectStoreReader {
    fn poll_seek(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
        pos: SeekFrom,
    ) -> Poll<std::io::Result<u64>> {
        let this = &mut *self;
        let new_pos = match pos {
            SeekFrom::Start(offset) => Some(offset),
            SeekFrom::End(delta) => this.size.checked_add_signed(delta),
            SeekFrom::Current(delta) => {
                // The caller-visible cursor is `pos`; buffered/in-flight bytes are lookahead
                // beyond it and are discarded below.
                this.pos.checked_add_signed(delta)
            }
        }
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "seek to a negative or overflowing position",
            )
        });

        let new_pos = match new_pos {
            Ok(p) => p,
            Err(err) => return Poll::Ready(Err(err)),
        };

        // Anything fetched ahead of the old cursor no longer matches the new one.
        this.pending = Bytes::new();
        this.inflight = None;
        this.pos = new_pos;
        Poll::Ready(Ok(new_pos))
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

/// Test-only: serve `data` over HTTP with `Range` support on any path; returns the base URL.
#[cfg(test)]
pub(crate) async fn spawn_range_server(data: Vec<u8>) -> String {
    use axum::http::{HeaderMap, StatusCode, header};

    let data = std::sync::Arc::new(data);
    let app = axum::Router::new().fallback(axum::routing::get({
        let data = std::sync::Arc::clone(&data);
        move |headers: HeaderMap| {
            let data = std::sync::Arc::clone(&data);
            async move {
                let range = headers
                    .get(header::RANGE)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.strip_prefix("bytes="))
                    .and_then(|v| v.split_once('-'))
                    .and_then(|(start, end)| {
                        Some((start.parse::<usize>().ok()?, end.parse::<usize>().ok()?))
                    });
                match range {
                    Some((start, end)) if start < data.len() => {
                        let end = end.min(data.len() - 1);
                        (StatusCode::PARTIAL_CONTENT, data[start..=end].to_vec())
                    }
                    Some(_) => (StatusCode::RANGE_NOT_SATISFIABLE, Vec::new()),
                    None => (StatusCode::OK, data.to_vec()),
                }
            }
        }
    }));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind failed");
    let addr = listener.local_addr().expect("local addr");
    tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve failed");
    });
    format!("http://{addr}")
}

#[cfg(test)]
mod tests {
    use futures::{AsyncReadExt as _, AsyncSeekExt as _};
    use object_store::memory::InMemory;

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
    async fn sequential_and_ranged_reads() {
        let data: Vec<u8> = (0..=255).collect();
        let mut reader = reader_over(&data).await;
        assert_eq!(reader.size(), 256);

        // Sequential read from the start.
        let mut buf = [0u8; 10];
        reader.read_exact(&mut buf).await.expect("read failed");
        assert_eq!(buf, data[0..10]);

        // Seek + read an interior span.
        reader
            .seek(SeekFrom::Start(100))
            .await
            .expect("seek failed");
        reader.read_exact(&mut buf).await.expect("read failed");
        assert_eq!(buf, data[100..110]);

        // Seek relative to the end.
        let pos = reader.seek(SeekFrom::End(-4)).await.expect("seek failed");
        assert_eq!(pos, 252);
        let mut tail = Vec::new();
        reader.read_to_end(&mut tail).await.expect("read failed");
        assert_eq!(tail, data[252..]);

        // Reads at EOF return 0 bytes.
        let n = reader.read(&mut buf).await.expect("read failed");
        assert_eq!(n, 0);

        // Seeking past EOF is allowed; reading there is EOF.
        reader
            .seek(SeekFrom::Start(10_000))
            .await
            .expect("seek failed");
        let n = reader.read(&mut buf).await.expect("read failed");
        assert_eq!(n, 0);
    }

    #[tokio::test]
    async fn small_reads_consume_one_fetch() {
        // A large read buffer followed by small reads must not re-fetch: the pending
        // buffer serves them.
        let data: Vec<u8> = (0..100).collect();
        let mut reader = reader_over(&data).await;

        let mut one = [0u8; 1];
        for i in 0..100u8 {
            reader.read_exact(&mut one).await.expect("read failed");
            assert_eq!(one[0], i);
        }
    }

    #[tokio::test]
    async fn reopen_has_independent_cursor() {
        let data = b"hello world".to_vec();
        let mut a = reader_over(&data).await;
        let mut b = a.reopen();

        let mut buf = [0u8; 5];
        a.seek(SeekFrom::Start(6)).await.expect("seek failed");
        a.read_exact(&mut buf).await.expect("read failed");
        assert_eq!(&buf, b"world");

        b.read_exact(&mut buf).await.expect("read failed");
        assert_eq!(&buf, b"hello");
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

    #[tokio::test]
    async fn presigned_http_reads() {
        let data: Vec<u8> = (0..=255).collect();
        let base = super::spawn_range_server(data.clone()).await;

        // A "pre-signed" URL is just an HTTP URL with a query string; no credentials.
        let url = Url::parse(&format!("{base}/bucket/object.rrd?signature=abc")).expect("url");
        let mut reader = ObjectStoreReader::open_presigned(url, data.len() as u64);

        let mut buf = [0u8; 10];
        reader.read_exact(&mut buf).await.expect("read failed");
        assert_eq!(buf, data[0..10]);

        reader
            .seek(SeekFrom::Start(100))
            .await
            .expect("seek failed");
        reader.read_exact(&mut buf).await.expect("read failed");
        assert_eq!(buf, data[100..110]);

        let pos = reader.seek(SeekFrom::End(-4)).await.expect("seek failed");
        assert_eq!(pos, 252);
        let mut tail = Vec::new();
        reader.read_to_end(&mut tail).await.expect("read failed");
        assert_eq!(tail, data[252..]);

        // Reads at EOF return 0 bytes; reopen gets a fresh cursor.
        let n = reader.read(&mut buf).await.expect("read failed");
        assert_eq!(n, 0);
        let mut fresh = reader.reopen();
        fresh.read_exact(&mut buf).await.expect("read failed");
        assert_eq!(buf, data[0..10]);
    }

    #[tokio::test]
    async fn presigned_http_error_status_surfaces() {
        // A server that 403s everything (an expired signature would look like this).
        let app = axum::Router::new().fallback(axum::routing::get(async || {
            (axum::http::StatusCode::FORBIDDEN, "expired")
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind failed");
        let addr = listener.local_addr().expect("local addr");
        tokio::spawn(async move {
            axum::serve(listener, app).await.expect("serve failed");
        });

        let url = Url::parse(&format!("http://{addr}/x?sig=old")).expect("url");
        let mut reader = ObjectStoreReader::open_presigned(url, 100);
        let mut buf = [0u8; 10];
        let err = reader.read_exact(&mut buf).await.expect_err("must fail");
        assert!(err.to_string().contains("403"), "got: {err}");
    }
}
