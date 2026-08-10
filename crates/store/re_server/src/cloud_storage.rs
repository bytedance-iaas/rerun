//! `tos://` / `s3://` storage support for dataset registration.
//!
//! Remote rrd files are downloaded into a local cache directory on first registration and then
//! loaded like local files (lazily, chunk by chunk). The original remote URL stays the source's
//! identity, so duplicate/unregister semantics are unchanged.
//!
//! Credentials and endpoint come from the environment (the same variables the rest of the
//! deployment already injects from docker/k8s secrets):
//! `TOS_ENDPOINT`, `TOS_REGION`, `TOS_ACCESS_KEY`, `TOS_SECRET_KEY`, and optionally
//! `TOS_S3_PATH_STYLE=1` for path-style buckets (e.g. `MinIO`). Standard `AWS_*` variables are
//! honored as a fallback, so plain `s3://` against AWS or `MinIO` works too.
//!
//! `RERUN_PRESIGN_ENDPOINT` (optional) overrides the endpoint used only for **pre-signing**
//! direct-read URLs, so the host handed to clients can differ from the one the server reads
//! over: sign the public endpoint for out-of-cloud clients while the server keeps reading over
//! the internal one. Unset falls back to `TOS_ENDPOINT`.

use std::path::PathBuf;
use std::sync::Arc;

use futures::StreamExt as _;
use object_store::{ObjectStore as _, ObjectStoreExt as _};
use url::Url;

/// Does this URL scheme refer to an S3-compatible object store?
pub fn is_remote_scheme(scheme: &str) -> bool {
    matches!(scheme, "tos" | "s3")
}

/// The directory holding the ledger and the remote-file cache.
///
/// Resolution order: value set by `--data-dir` → `RERUN_SERVER_DATA_DIR` → `./rerun-server-data`.
pub fn data_dir() -> PathBuf {
    if let Some(dir) = crate::persistence::configured_data_dir() {
        return dir;
    }
    if let Ok(dir) = std::env::var("RERUN_SERVER_DATA_DIR")
        && !dir.is_empty()
    {
        return PathBuf::from(dir);
    }
    PathBuf::from("rerun-server-data")
}

fn cache_dir() -> PathBuf {
    data_dir().join("cache")
}

/// (bucket, key) of a `tos://bucket/key` or `s3://bucket/key` URL.
fn bucket_and_key(url: &Url) -> tonic::Result<(String, String)> {
    let bucket = url
        .host_str()
        .filter(|host| !host.is_empty())
        .ok_or_else(|| {
            tonic::Status::invalid_argument(format!("missing bucket in remote URL: {url}"))
        })?;
    let key = url.path().trim_start_matches('/');
    Ok((bucket.to_owned(), key.to_owned()))
}

fn env_non_empty(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

/// Build an S3-compatible client for the given bucket from the environment.
///
/// Returns the concrete client type because pre-signing ([`object_store::signer::Signer`])
/// is only available on it, not on `dyn ObjectStore`.
///
/// `endpoint_override` replaces the `TOS_ENDPOINT` env for this client only. Pre-signing
/// uses it so the signed URL's host can differ from the endpoint the server reads over
/// (e.g. sign a public host for out-of-cloud clients while the server itself reads over the
/// internal endpoint); all other callers pass `None` and read over `TOS_ENDPOINT`.
fn client_for_bucket(
    bucket: &str,
    endpoint_override: Option<String>,
) -> tonic::Result<Arc<object_store::aws::AmazonS3>> {
    // TOS's S3-compatible endpoint uses virtual-hosted style ({bucket}.{endpoint});
    // path-style is opt-in for MinIO-like setups.
    let path_style = env_non_empty("TOS_S3_PATH_STYLE").is_some_and(|v| v != "0");

    // Start from the standard AWS_* environment, then apply the TOS_* overrides.
    let mut builder = object_store::aws::AmazonS3Builder::from_env().with_bucket_name(bucket);

    if let Some(endpoint) = endpoint_override.or_else(|| env_non_empty("TOS_ENDPOINT")) {
        // In virtual-hosted style `object_store` uses the endpoint as-is (it does not
        // prepend the bucket itself), so splice the bucket into the host here.
        let endpoint = if path_style {
            endpoint
        } else {
            let url = Url::parse(&endpoint).map_err(|err| {
                tonic::Status::invalid_argument(format!("invalid TOS_ENDPOINT {endpoint:?}: {err}"))
            })?;
            let host = url.host_str().unwrap_or_default();
            let port = url.port().map(|p| format!(":{p}")).unwrap_or_default();
            format!("{}://{bucket}.{host}{port}", url.scheme())
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

    builder.build().map(Arc::new).map_err(|err| {
        tonic::Status::invalid_argument(format!(
            "Failed to configure S3-compatible storage (check TOS_ENDPOINT/TOS_REGION/\
             TOS_ACCESS_KEY/TOS_SECRET_KEY): {err:#}"
        ))
    })
}

/// The local cache path of a remote file: stable across restarts for the same URL.
fn cache_path_for(url: &Url) -> PathBuf {
    use std::hash::{Hash as _, Hasher as _};

    // Keyed on the full URL; the readable suffix is only for humans poking at the cache.
    let mut hasher = std::hash::DefaultHasher::new();
    url.as_str().hash(&mut hasher);
    let digest = hasher.finish();

    let filename = url
        .path()
        .rsplit('/')
        .next()
        .unwrap_or("file.rrd")
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') {
                c
            } else {
                '_'
            }
        })
        .collect::<String>();

    cache_dir().join(format!("{digest:016x}-{filename}"))
}

/// Make sure the remote file is present in the local cache, downloading it if needed.
/// Returns the local path.
pub async fn ensure_cached(url: &Url) -> tonic::Result<PathBuf> {
    let local_path = cache_path_for(url);

    if let Ok(metadata) = tokio::fs::metadata(&local_path).await
        && metadata.is_file()
        && metadata.len() > 0
    {
        re_log::debug!("Remote file already cached: {url} -> {local_path:?}");
        return Ok(local_path);
    }

    let (bucket, key) = bucket_and_key(url)?;
    if key.is_empty() {
        return Err(tonic::Status::invalid_argument(format!(
            "remote URL has no object key: {url}"
        )));
    }
    let client = client_for_bucket(&bucket, None)?;

    re_log::info!("Downloading {url}…");
    let object = client
        .get(&object_store::path::Path::from(key.as_str()))
        .await
        .map_err(|err| {
            tonic::Status::not_found(format!("Failed to fetch remote file: {err:#}\nURL: {url}"))
        })?;
    let bytes = object.bytes().await.map_err(|err| {
        tonic::Status::internal(format!(
            "Failed to read remote file body: {err:#}\nURL: {url}"
        ))
    })?;

    if let Some(parent) = local_path.parent() {
        tokio::fs::create_dir_all(parent).await.map_err(|err| {
            tonic::Status::internal(format!(
                "Failed to create cache directory: {err:#}\nPath: {parent:?}"
            ))
        })?;
    }

    // Write via a temp file + rename so a crash mid-download never leaves a truncated
    // file that a later run would mistake for a complete download.
    let tmp_path = local_path.with_extension("part");
    tokio::fs::write(&tmp_path, &bytes).await.map_err(|err| {
        tonic::Status::internal(format!(
            "Failed to write cache file: {err:#}\nPath: {tmp_path:?}"
        ))
    })?;
    tokio::fs::rename(&tmp_path, &local_path)
        .await
        .map_err(|err| {
            tonic::Status::internal(format!(
                "Failed to finalize cache file: {err:#}\nPath: {local_path:?}"
            ))
        })?;

    re_log::info!(
        "Downloaded {url} ({}) to cache",
        re_format::format_bytes(bytes.len() as _)
    );
    Ok(local_path)
}

/// List all `.rrd` files under a remote prefix, returned as full remote URLs
/// (same scheme as the input).
pub async fn list_rrds(prefix_url: &Url) -> tonic::Result<Vec<Url>> {
    let (bucket, prefix) = bucket_and_key(prefix_url)?;
    let client = client_for_bucket(&bucket, None)?;

    let list_prefix = (!prefix.is_empty()).then(|| object_store::path::Path::from(prefix.as_str()));

    let mut urls = Vec::new();
    let mut stream = client.list(list_prefix.as_ref());
    while let Some(meta) = stream.next().await {
        let meta = meta.map_err(|err| {
            tonic::Status::internal(format!(
                "Failed to list remote prefix: {err:#}\nURL: {prefix_url}"
            ))
        })?;
        if meta.location.as_ref().ends_with(".rrd") {
            let mut url = prefix_url.clone();
            url.set_path(meta.location.as_ref());
            urls.push(url);
        }
    }

    urls.sort_by(|a, b| a.as_str().cmp(b.as_str()));
    Ok(urls)
}

/// Pre-sign a `GET` for a remote object: anyone holding the returned URL can read that one
/// object until the expiry — no credentials needed on their side.
///
/// The signed URL's host comes from `RERUN_PRESIGN_ENDPOINT` when set, else `TOS_ENDPOINT`.
/// Set it to the **public** TOS endpoint (`…volces.com`) so out-of-cloud clients can reach the
/// signed URLs — the deployment default. For clients running inside the same VPC, set it to the
/// internal endpoint (`…ivolces.com`) so their reads stay off the public network. Only the
/// signature host changes; the server's own reads still use `TOS_ENDPOINT`.
pub async fn presign_get(url: &Url, expires_in: std::time::Duration) -> tonic::Result<(Url, i64)> {
    use object_store::signer::Signer as _;

    let (bucket, key) = bucket_and_key(url)?;
    let client = client_for_bucket(&bucket, env_non_empty("RERUN_PRESIGN_ENDPOINT"))?;
    let path = object_store::path::Path::from(key.as_str());

    let signed = client
        .signed_url(http::Method::GET, &path, expires_in)
        .await
        .map_err(|err| {
            tonic::Status::internal(format!(
                "failed to pre-sign object URL: {err:#}\nURL: {url}"
            ))
        })?;

    let expires_at = jiff::Timestamp::now().as_second() + expires_in.as_secs().cast_signed();
    Ok((signed, expires_at))
}

/// Size in bytes of a remote object (one `HEAD` request).
pub async fn object_size(url: &Url) -> tonic::Result<u64> {
    let (bucket, key) = bucket_and_key(url)?;
    let client = client_for_bucket(&bucket, None)?;
    let path = object_store::path::Path::from(key.as_str());

    let head = client.head(&path).await.map_err(|err| {
        tonic::Status::internal(format!("failed to stat object: {err:#}\nURL: {url}"))
    })?;
    Ok(head.size)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bucket_and_key_splits_host_and_path() {
        let cases = [
            (
                "s3://mybucket/path/to/file.rrd",
                "mybucket",
                "path/to/file.rrd",
            ),
            ("tos://mybucket/a/b", "mybucket", "a/b"),
            ("s3://mybucket/", "mybucket", ""), // no key
            ("s3://mybucket", "mybucket", ""),  // no path at all
        ];
        for (url, bucket, key) in cases {
            let parsed = bucket_and_key(&Url::parse(url).unwrap()).unwrap();
            assert_eq!(parsed, (bucket.to_owned(), key.to_owned()), "url: {url}");
        }
    }

    #[test]
    fn bucket_and_key_requires_a_bucket() {
        assert!(bucket_and_key(&Url::parse("s3:///key-only").unwrap()).is_err());
    }
}
