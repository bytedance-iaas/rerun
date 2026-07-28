use std::net::SocketAddr;

use anyhow::Context as _;
use re_protos::common::v1alpha1::ext;
#[cfg(unix)]
use tokio::signal::unix::{SignalKind, signal};
#[cfg(windows)]
use tokio::signal::windows::{ctrl_break, ctrl_close};
use tracing::{info, warn};

use crate::{NamedPath, NamedPathCollection, ServerBuilder, ServerHandle};

// ---

#[derive(Clone, Debug, clap::Parser)]
#[clap(author, version, about)]
pub struct Args {
    /// IP address to listen on.
    #[clap(long, default_value = "0.0.0.0")]
    pub host: String,

    /// Port to bind to.
    #[clap(long, short = 'p', default_value_t = 51234)]
    pub port: u16,

    // TODO(ab): expose this to the CLI
    /// Load a set of RRDs as a dataset (can be specified multiple times).
    ///
    /// All the paths in the path collections must point at RRD files. Directories are not
    /// supported.
    #[clap(skip)]
    pub datasets: Vec<NamedPathCollection>,

    /// Load a directory of RRD as dataset (can be specified multiple times).
    /// You can specify only a path or provide a name such as
    /// `-d my_dataset=./path/to/files`
    #[clap(long = "dataset", short = 'd', value_name = "[NAME=]DIR_PATH")]
    pub dataset_prefixes: Vec<NamedPath>,

    /// Load a lance file as a table (can be specified multiple times).
    /// You can specify only a path or provide a name such as
    /// `-t my_table=./path/to/table`
    #[clap(long = "table", short = 't', value_name = "[NAME=]TABLE_PATH")]
    pub tables: Vec<NamedPath>,

    /// Artificial latency to add to each request (in milliseconds).
    #[clap(long, default_value_t = 0)]
    pub latency_ms: u16,

    /// Artificial bandwidth limit for responses (e.g. '10MB' for 10 megabytes per second).
    #[clap(long, value_parser = parse_bandwidth_limit)]
    pub bandwidth_limit: Option<u64>,

    /// Additional origin patterns allowed to make cross-origin requests to the server
    /// (can be specified multiple times).
    ///
    /// By default, only `localhost`, `127.0.0.1`, and `rerun.io` are allowed.
    /// Patterns are matched against the full `Origin` header value,
    /// using glob-style matching where `*` matches any sequence of characters.
    #[clap(long = "cors-allow-origin")]
    pub cors_allow_origin: Vec<String>,

    /// Directory for server persistence: the catalog database and the cache of remote
    /// (`tos://`, `s3://`) files.
    ///
    /// When set, the catalog survives server restarts: registered datasets are restored
    /// under their original ids at startup. Without it, the catalog is in-memory only.
    #[clap(long = "data-dir", env = "RERUN_SERVER_DATA_DIR")]
    pub data_dir: Option<std::path::PathBuf>,
}

fn parse_bandwidth_limit(s: &str) -> Result<u64, String> {
    re_format::parse_bytes(s)
        .and_then(|b| u64::try_from(b).ok())
        .ok_or_else(|| format!("expected a bandwidth like '10MB' or '1GiB', got {s:?}"))
}

impl Default for Args {
    fn default() -> Self {
        Self {
            host: "0.0.0.0".into(),
            port: 51234,
            datasets: vec![],
            dataset_prefixes: vec![],
            tables: vec![],
            latency_ms: 0,
            bandwidth_limit: None,
            cors_allow_origin: Vec::new(),
            data_dir: None,
        }
    }
}

impl Args {
    /// Waits for the server to start, and return a handle to it.
    ///
    /// Use [`ServerHandle::connect_addr`] for the address to connect to.
    pub async fn create_server_handle(self) -> anyhow::Result<ServerHandle> {
        let Self {
            host: ip,
            port,
            datasets,
            dataset_prefixes,
            tables,
            latency_ms,
            bandwidth_limit,
            cors_allow_origin,
            data_dir,
        } = self;

        let handler = {
            let mut builder = crate::RerunCloudHandlerBuilder::new();

            // First: restore the previous catalog, so the -d/-t preload flags below behave
            // the same as on a fresh server (duplicates error out).
            if let Some(data_dir) = &data_dir {
                builder = builder.with_persistence(data_dir).await?;
            }

            for NamedPathCollection { name, paths } in datasets {
                builder = builder
                    .with_rrds_as_dataset(
                        name,
                        paths,
                        ext::IfDuplicateBehavior::Error,
                        crate::OnError::Continue,
                    )
                    .await?;
            }

            for dataset_prefix in &dataset_prefixes {
                builder = builder
                    .with_directory_as_dataset(
                        dataset_prefix,
                        ext::IfDuplicateBehavior::Error,
                        crate::OnError::Continue,
                    )
                    .await?;
            }

            #[cfg_attr(not(feature = "lance"), expect(clippy::never_loop))]
            for table in &tables {
                cfg_select! {
                    feature = "lance" => {
                        builder = builder
                            .with_directory_as_table(
                                table,
                                ext::IfDuplicateBehavior::Error,
                            )
                            .await?;
                    }
                    _ => {
                        _ = table;
                        anyhow::bail!("re_server was not compiled with the 'lance' feature");
                    }
                }
            }

            builder
        };
        let ledger = handler.ledger();
        let handler = handler.build();

        let rerun_cloud_server =
            re_protos::cloud::v1alpha1::rerun_cloud_service_server::RerunCloudServiceServer::new(
                handler,
            )
            .max_decoding_message_size(re_grpc_server::MAX_DECODING_MESSAGE_SIZE)
            .max_encoding_message_size(re_grpc_server::MAX_ENCODING_MESSAGE_SIZE);

        let ip = ip.parse().with_context(|| format!("IP: {ip:?}"))?;
        let ip_port = SocketAddr::new(ip, port);

        let server_builder = ServerBuilder::default()
            .with_address(ip_port)
            .with_service(rerun_cloud_server)
            .with_http_route(
                "/version",
                axum::routing::get(async move || re_build_info::build_info!().to_string()),
            )
            // Read-only source listing of a dataset (original tos://s3://file:// URLs).
            // Lets training-side tooling mirror the data straight from the object store
            // instead of streaming every batch through this server.
            .with_http_route(
                "/catalog/sources",
                axum::routing::get({
                    let ledger = ledger.clone();
                    move |query: axum::extract::Query<std::collections::HashMap<String, String>>| {
                        let ledger = ledger.clone();
                        async move {
                            use axum::http::StatusCode;
                            let Some(ledger) = ledger else {
                                return (
                                    StatusCode::NOT_IMPLEMENTED,
                                    axum::Json(serde_json::json!({
                                        "error": "catalog persistence is not enabled \
                                                  (start the server with --data-dir)",
                                    })),
                                );
                            };
                            let Some(name) = query.0.get("dataset") else {
                                return (
                                    StatusCode::BAD_REQUEST,
                                    axum::Json(
                                        serde_json::json!({"error": "missing ?dataset=<name>"}),
                                    ),
                                );
                            };
                            match ledger.dataset_sources(name) {
                                Some((dataset_id, sources)) => (
                                    StatusCode::OK,
                                    axum::Json(serde_json::json!({
                                        "dataset": name,
                                        "dataset_id": dataset_id,
                                        "sources": sources,
                                    })),
                                ),
                                None => (
                                    StatusCode::NOT_FOUND,
                                    axum::Json(serde_json::json!({
                                        "error": format!("unknown dataset: {name}"),
                                    })),
                                ),
                            }
                        }
                    }
                }),
            )
            .with_artificial_latency(std::time::Duration::from_millis(latency_ms as _))
            .with_bandwidth_limit(bandwidth_limit)
            .with_cors_allowed_origins(cors_allow_origin);

        let server = server_builder.build();
        let async_runtime =
            re_async::AsyncRuntimeHandle::from_current_tokio_runtime_or_wasmbindgen()?;

        let server_handle = server.start(&async_runtime).await?;

        Ok(server_handle)
    }

    pub async fn run_async(self) -> anyhow::Result<()> {
        let mut server_handle = self.create_server_handle().await?;

        #[cfg(unix)]
        let mut term_signal = signal(SignalKind::terminate())?;
        #[cfg(windows)]
        let mut term_signal = ctrl_close()?;

        #[cfg(unix)]
        let mut int_signal = signal(SignalKind::interrupt())?;
        #[cfg(windows)]
        let mut int_signal = ctrl_break()?;

        tokio::select! {
            _ = term_signal.recv() => {
                info!("received SIGTERM, gracefully shutting down");
            }

            _ = int_signal.recv() => {
                info!("received SIGINT, gracefully shutting down");
            }

            () = server_handle.wait_for_shutdown() => {
                warn!("gRPC endpoint shut down on its own, terminating redap-server");
            }
        }

        Ok(())
    }
}
