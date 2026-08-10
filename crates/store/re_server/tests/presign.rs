//! End-to-end tests for `/catalog/presign`: exchanging a segment id for directly-readable
//! URLs of its layer RRDs, over a real HTTP transport.
//!
//! Uses `file://`-registered data, which the endpoint passes through unsigned (nothing to
//! sign locally) — same response shape as the pre-signed `tos://`/`s3://` case, so the
//! whole flow is exercised without cloud credentials.

use re_auth::{Permission, RedapProvider, SecretKey};

/// Author a small footered RRD; returns its recording (= segment) id.
fn write_rrd(path: &std::path::Path) -> String {
    use re_chunk::{Chunk, RowId, TimePoint, Timeline};
    use re_log_types::example_components::{MyPoint, MyPoints};
    use re_log_types::{
        EntityPath, LogMsg, SetStoreInfo, StoreId, StoreInfo, StoreKind, StoreSource,
    };

    let store_id = StoreId::random(StoreKind::Recording, "presign_test");
    let timeline = Timeline::new_sequence("frame");

    let mut file = std::fs::File::create(path).expect("failed to create test RRD");
    let mut encoder = re_log_encoding::Encoder::new_eager(
        re_build_info::CrateVersion::LOCAL,
        re_log_encoding::EncodingOptions::PROTOBUF_COMPRESSED,
        &mut file,
    )
    .expect("failed to create encoder");
    encoder
        .append(&LogMsg::SetStoreInfo(SetStoreInfo {
            row_id: *RowId::ZERO,
            info: StoreInfo::new(store_id.clone(), StoreSource::Unknown),
        }))
        .expect("failed to write store info");
    for i in 0..3u32 {
        let points = MyPoint::from_iter(i..i + 1);
        let chunk = Chunk::builder(EntityPath::from(format!("/entity_{i}")))
            .with_sparse_component_batches(
                RowId::new(),
                TimePoint::default().with(timeline, i64::from(i)),
                [(MyPoints::descriptor_points(), Some(&points as _))],
            )
            .build()
            .expect("chunk should be valid");
        encoder
            .append(&LogMsg::ArrowMsg(
                store_id.clone(),
                chunk.to_arrow_msg().expect("chunk should encode"),
            ))
            .expect("failed to write chunk");
    }
    encoder.finish().expect("failed to finish RRD");

    store_id.recording_id().to_string()
}

struct TestServer {
    handle: re_server::ServerHandle,
    dataset_id: String,
    segment_id: String,
    _dir: tempfile::TempDir,
}

/// Start a server preloaded with one file-registered dataset; resolve its entry id.
async fn start_server(token_secret: Option<String>) -> TestServer {
    let dir = tempfile::tempdir().expect("tempdir");
    let segment_id = write_rrd(&dir.path().join("seg.rrd"));

    let handle = re_server::Args {
        host: "127.0.0.1".to_owned(),
        port: 0,
        dataset_prefixes: vec![re_server::NamedPath {
            name: Some("presign_ds".to_owned()),
            path: dir.path().to_path_buf(),
        }],
        token_secret: token_secret.clone(),
        ..Default::default()
    }
    .create_server_handle()
    .await
    .expect("failed to start server");

    // Resolve the dataset's entry id over gRPC.
    let channel =
        tonic::transport::Endpoint::from_shared(format!("http://{}", handle.connect_addr()))
            .expect("valid endpoint")
            .connect()
            .await
            .expect("failed to connect");
    let mut client =
        re_protos::cloud::v1alpha1::rerun_cloud_service_client::RerunCloudServiceClient::new(
            channel,
        );
    let mut request =
        tonic::Request::new(re_protos::cloud::v1alpha1::FindEntriesRequest { filter: None });
    if let Some(token) = token_for(token_secret.as_deref()) {
        request.metadata_mut().insert(
            "authorization",
            format!("Bearer {token}").parse().expect("valid header"),
        );
    }
    let entries = client
        .find_entries(request)
        .await
        .expect("find_entries failed")
        .into_inner()
        .entries;
    let dataset_id = entries
        .iter()
        .find(|e| e.name.as_deref() == Some("presign_ds"))
        .and_then(|e| e.id.clone())
        .map(|id| {
            re_log_types::EntryId::try_from(id)
                .expect("valid entry id")
                .to_string()
        })
        .expect("dataset entry should exist");

    TestServer {
        handle,
        dataset_id,
        segment_id,
        _dir: dir,
    }
}

fn token_for(secret: Option<&str>) -> Option<String> {
    let secret = secret?;
    let provider = RedapProvider::from_secret_key_base64(secret).expect("valid secret");
    let token = provider
        .token_with_hosts(
            std::time::Duration::from_secs(3600),
            "test",
            "alice",
            Permission::Read,
            vec!["127.0.0.1".to_owned()],
        )
        .expect("token should mint");
    Some(token.as_str().to_owned())
}

async fn http_get(url: &str, bearer: Option<&str>) -> (u16, serde_json::Value) {
    let mut request = ehttp::Request::get(url);
    if let Some(token) = bearer {
        request
            .headers
            .insert("Authorization", format!("Bearer {token}"));
    }
    let response = ehttp::fetch_async(request).await.expect("fetch failed");
    let body = serde_json::from_slice(&response.bytes).unwrap_or_else(
        |_err| serde_json::json!({"raw": String::from_utf8_lossy(&response.bytes)}),
    );
    (response.status, body)
}

#[tokio::test(flavor = "multi_thread")]
async fn presign_returns_file_passthrough_layers() {
    let server = start_server(None).await;
    let base = format!("http://{}", server.handle.connect_addr());

    let (status, body) = http_get(
        &format!(
            "{base}/catalog/presign?dataset={}&segment={}",
            server.dataset_id, server.segment_id
        ),
        None,
    )
    .await;
    assert_eq!(status, 200, "body: {body}");

    let layers = body["layers"].as_array().expect("layers array");
    assert_eq!(layers.len(), 1, "body: {body}");
    let layer = &layers[0];
    assert_eq!(layer["layer"], "base");
    let url = layer["url"].as_str().expect("url string");
    assert!(url.starts_with("file://"), "got: {url}");
    assert!(layer["size_bytes"].as_u64().expect("size") > 0);
    assert!(layer["expires_at_unix"].is_null());

    // Unknown segment → 404.
    let (status, _body) = http_get(
        &format!(
            "{base}/catalog/presign?dataset={}&segment=nope",
            server.dataset_id
        ),
        None,
    )
    .await;
    assert_eq!(status, 404);

    // Missing params → 400.
    let (status, _body) = http_get(&format!("{base}/catalog/presign"), None).await;
    assert_eq!(status, 400);

    server.handle.shutdown_and_wait().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn presign_requires_token_when_auth_enabled() {
    let secret = SecretKey::generate(rand::rng()).to_base64();
    let server = start_server(Some(secret.clone())).await;
    let base = format!("http://{}", server.handle.connect_addr());
    let url = format!(
        "{base}/catalog/presign?dataset={}&segment={}",
        server.dataset_id, server.segment_id
    );

    // No token → 401.
    let (status, _body) = http_get(&url, None).await;
    assert_eq!(status, 401);

    // Valid (read) token → 200.
    let token = token_for(Some(&secret)).expect("token");
    let (status, body) = http_get(&url, Some(&token)).await;
    assert_eq!(status, 200, "body: {body}");
    assert_eq!(body["layers"].as_array().expect("layers").len(), 1);

    server.handle.shutdown_and_wait().await;
}
