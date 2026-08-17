//! End-to-end tests for the catalog server's token authentication, over a real
//! gRPC transport (the interceptor lives at the transport layer, so in-process
//! handler tests can't cover it).

use re_auth::{Permission, RedapProvider, SecretKey};
use re_protos::cloud::v1alpha1::rerun_cloud_service_client::RerunCloudServiceClient;
use re_protos::cloud::v1alpha1::{CreateDatasetEntryRequest, FindEntriesRequest};
use tonic::Code;
use tonic::metadata::{Ascii, MetadataValue};
use tonic::transport::Channel;

async fn start_server(token_secret: Option<String>) -> re_server::ServerHandle {
    re_server::Args {
        host: "127.0.0.1".to_owned(),
        port: 0,
        token_secret,
        ..Default::default()
    }
    .create_server_handle()
    .await
    .expect("failed to start server")
}

async fn connect(handle: &re_server::ServerHandle) -> RerunCloudServiceClient<Channel> {
    let channel =
        tonic::transport::Endpoint::from_shared(format!("http://{}", handle.connect_addr()))
            .expect("valid endpoint")
            .connect()
            .await
            .expect("failed to connect");
    RerunCloudServiceClient::new(channel)
}

fn bearer(token: &str) -> MetadataValue<Ascii> {
    format!("Bearer {token}")
        .parse()
        .expect("valid header value")
}

fn find_entries_req() -> FindEntriesRequest {
    FindEntriesRequest { filter: None }
}

fn create_dataset_req(name: &str) -> CreateDatasetEntryRequest {
    CreateDatasetEntryRequest {
        name: Some(name.to_owned()),
        id: None,
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn without_secret_the_server_is_open() {
    let handle = start_server(None).await;
    let mut client = connect(&handle).await;

    client
        .find_entries(find_entries_req())
        .await
        .expect("read should succeed on an open server");
    client
        .create_dataset_entry(create_dataset_req("open_ds"))
        .await
        .expect("write should succeed on an open server");

    handle.shutdown_and_wait().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn with_secret_requests_need_a_valid_token() {
    let secret = SecretKey::generate(rand::rng());
    let provider = RedapProvider::from_secret_key(secret.clone());
    let handle = start_server(Some(secret.to_base64())).await;

    // No token → Unauthenticated.
    let mut client = connect(&handle).await;
    let err = client
        .find_entries(find_entries_req())
        .await
        .expect_err("token-less request must be rejected");
    assert_eq!(err.code(), Code::Unauthenticated, "got: {err}");

    // Garbage token → Unauthenticated.
    let mut request = tonic::Request::new(find_entries_req());
    request
        .metadata_mut()
        .insert("authorization", bearer("not-a-jwt"));
    let err = client
        .find_entries(request)
        .await
        .expect_err("garbage token must be rejected");
    assert_eq!(err.code(), Code::Unauthenticated, "got: {err}");

    // Token signed with a DIFFERENT secret → Unauthenticated.
    let other = RedapProvider::from_secret_key(SecretKey::generate(rand::rng()));
    let forged = other
        .token_with_hosts(
            std::time::Duration::from_hours(1),
            "test",
            "mallory",
            Permission::ReadWrite,
            vec!["127.0.0.1".to_owned()],
        )
        .expect("token should mint");
    let mut request = tonic::Request::new(find_entries_req());
    request
        .metadata_mut()
        .insert("authorization", bearer(forged.as_str()));
    let err = client
        .find_entries(request)
        .await
        .expect_err("wrong-secret token must be rejected");
    assert_eq!(err.code(), Code::Unauthenticated, "got: {err}");

    // Valid read token → reads pass, writes are PermissionDenied.
    let read_token = provider
        .token_with_hosts(
            std::time::Duration::from_hours(1),
            "test",
            "alice",
            Permission::Read,
            vec!["127.0.0.1".to_owned()],
        )
        .expect("token should mint");
    let mut request = tonic::Request::new(find_entries_req());
    request
        .metadata_mut()
        .insert("authorization", bearer(read_token.as_str()));
    client
        .find_entries(request)
        .await
        .expect("valid read token must pass reads");

    let mut request = tonic::Request::new(create_dataset_req("read_only_attempt"));
    request
        .metadata_mut()
        .insert("authorization", bearer(read_token.as_str()));
    let err = client
        .create_dataset_entry(request)
        .await
        .expect_err("read token must not pass writes");
    assert_eq!(err.code(), Code::PermissionDenied, "got: {err}");
    assert!(err.message().contains("alice"), "got: {err}");

    // Valid read-write token → writes pass.
    let rw_token = provider
        .token_with_hosts(
            std::time::Duration::from_hours(1),
            "test",
            "bob",
            Permission::ReadWrite,
            vec!["127.0.0.1".to_owned()],
        )
        .expect("token should mint");
    let mut request = tonic::Request::new(create_dataset_req("rw_ds"));
    request
        .metadata_mut()
        .insert("authorization", bearer(rw_token.as_str()));
    client
        .create_dataset_entry(request)
        .await
        .expect("read-write token must pass writes");

    handle.shutdown_and_wait().await;
}

/// Same story through the real SDK client stack (`re_redap_client`, which the Python
/// `CatalogClient(url, token=...)` wraps) — covers the credential plumbing AND the
/// client-side host gate that refuses to send tokens to unlisted hosts.
#[tokio::test(flavor = "multi_thread")]
async fn sdk_client_with_token() {
    let secret = SecretKey::generate(rand::rng());
    let provider = RedapProvider::from_secret_key(secret.clone());
    let handle = start_server(Some(secret.to_base64())).await;

    let origin: re_uri::Origin = format!("rerun+http://{}", handle.connect_addr())
        .parse()
        .expect("valid origin");

    // Token minted for this server's host (exactly what generate-token produces).
    let token = provider
        .token_with_hosts(
            std::time::Duration::from_hours(1),
            "rerun-oss-server",
            "carol",
            Permission::Read,
            vec!["127.0.0.1".to_owned()],
        )
        .expect("token should mint");

    let registry = re_redap_client::ConnectionRegistry::new_without_stored_credentials();
    registry.set_credentials(
        &origin,
        re_redap_client::Credentials::Token(
            re_auth::Jwt::try_from(token.as_str().to_owned()).expect("valid jwt"),
        ),
    );
    let mut client = registry
        .client(origin.clone())
        .await
        .expect("client should connect");

    client
        .find_entries(re_protos::cloud::v1alpha1::EntryFilter::default())
        .await
        .expect("authenticated SDK read must succeed");

    // Without credentials the SDK is rejected — at connect time (the eager server
    // probe) or at the first call, depending on the client version; either way the
    // error must be an authentication error.
    let registry = re_redap_client::ConnectionRegistry::new_without_stored_credentials();
    let err_text = match registry.client(origin).await {
        Err(err) => err.to_string(),
        Ok(mut client) => client
            .find_entries(re_protos::cloud::v1alpha1::EntryFilter::default())
            .await
            .expect_err("token-less SDK request must be rejected")
            .to_string(),
    };
    assert!(
        err_text.contains("missing credentials")
            || err_text.contains("missing token")
            || err_text.to_lowercase().contains("unauthenticated"),
        "got: {err_text}"
    );

    handle.shutdown_and_wait().await;
}
