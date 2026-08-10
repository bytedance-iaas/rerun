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

    /// Token-signing secret (base64). When set, every request must carry a valid access
    /// token; without it the server is completely open.
    ///
    /// Generate a secret with `rerun server generate-secret`, then mint tokens for users
    /// with `rerun server generate-token`.
    #[clap(long = "token-secret", env = "RERUN_SERVER_TOKEN_SECRET")]
    pub token_secret: Option<String>,

    /// Token management utilities; without a subcommand, runs the server.
    #[clap(subcommand)]
    pub command: Option<ServerCommand>,
}

#[derive(Clone, Debug, clap::Subcommand)]
pub enum ServerCommand {
    /// Generate a fresh random token-signing secret (base64) and print it.
    ///
    /// Give it to the server via `--token-secret` / `RERUN_SERVER_TOKEN_SECRET`, and keep it
    /// where you mint tokens. Anyone who has it can mint valid tokens.
    GenerateSecret,

    /// Mint an access token, signed with the given secret.
    GenerateToken(GenerateTokenArgs),
}

#[derive(Clone, Debug, clap::Parser)]
pub struct GenerateTokenArgs {
    /// The token-signing secret (base64), same value the server runs with.
    #[clap(long, env = "RERUN_SERVER_TOKEN_SECRET")]
    pub secret: String,

    /// Who this token is for. Recorded inside the token; shows up in server logs and
    /// permission errors.
    #[clap(long)]
    pub user: String,

    /// How long the token stays valid, e.g. `90d`, `12h`, `30m`.
    #[clap(long, default_value = "90d", value_parser = parse_duration)]
    pub expiration: std::time::Duration,

    /// `read` (query only) or `read-write` (may also register/update/delete).
    #[clap(long, default_value = "read")]
    pub permission: re_auth::Permission,

    /// Server host(s) the token may be sent to, e.g. a public IP and/or the in-cluster
    /// DNS name (can be specified multiple times). A leading dot allows a whole domain's
    /// subdomains (`.example.com`).
    ///
    /// Clients refuse to send a token to hosts outside this list — it limits the damage
    /// of a token leaking to the wrong server.
    #[clap(long = "server-host", required = true)]
    pub server_hosts: Vec<String>,
}

/// Parse `90d` / `12h` / `30m` / `45s` into a duration.
fn parse_duration(s: &str) -> Result<std::time::Duration, String> {
    let s = s.trim();
    let (num, unit) = s.split_at(s.len().saturating_sub(1));
    let num: u64 = num
        .parse()
        .map_err(|_err| format!("expected e.g. '90d', '12h', '30m', got {s:?}"))?;
    let secs = match unit {
        "d" => num * 24 * 60 * 60,
        "h" => num * 60 * 60,
        "m" => num * 60,
        "s" => num,
        _ => return Err(format!("expected a d/h/m/s suffix, got {s:?}")),
    };
    Ok(std::time::Duration::from_secs(secs))
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
            token_secret: None,
            command: None,
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
            token_secret,
            command: _,
        } = self;

        // Token authentication: with a secret every request must present a valid token;
        // without one the server is open (only acceptable on trusted networks).
        let auth_provider = if let Some(secret) = &token_secret {
            let provider = re_auth::RedapProvider::from_secret_key_base64(secret).context(
                "invalid --token-secret (expected base64 from `rerun server generate-secret`)",
            )?;
            info!("token authentication enabled: requests must carry a valid access token");
            Some(provider)
        } else {
            warn!(
                "token authentication DISABLED (no --token-secret / RERUN_SERVER_TOKEN_SECRET): \
                 anyone who can reach this server has full access"
            );
            None
        };

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

        let server_builder = ServerBuilder::default().with_address(ip_port);
        let server_builder = if let Some(provider) = &auth_provider {
            server_builder.with_service(tonic::service::interceptor::InterceptedService::new(
                rerun_cloud_server,
                crate::auth::RequireAuth::new(provider.clone()),
            ))
        } else {
            server_builder.with_service(rerun_cloud_server)
        };
        let server_builder = server_builder
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
                    let auth_provider = auth_provider.clone();
                    move |headers: axum::http::HeaderMap,
                          query: axum::extract::Query<std::collections::HashMap<String, String>>| {
                        let ledger = ledger.clone();
                        let auth_provider = auth_provider.clone();
                        async move {
                            use axum::http::StatusCode;
                            if let Some(provider) = &auth_provider
                                && let Err((code, msg)) =
                                    crate::auth::verify_http_bearer(provider, &headers)
                            {
                                return (
                                    code,
                                    axum::Json(serde_json::json!({"error": msg})),
                                );
                            }
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
        match &self.command {
            Some(ServerCommand::GenerateSecret) => {
                let secret = re_auth::SecretKey::generate(rand::rng());
                println!("{}", secret.to_base64());
                eprintln!(
                    "\nStore this secret safely. Start the server with it \
                     (--token-secret / RERUN_SERVER_TOKEN_SECRET) and use it to mint \
                     tokens with `rerun server generate-token`."
                );
                return Ok(());
            }
            Some(ServerCommand::GenerateToken(args)) => {
                let provider = re_auth::RedapProvider::from_secret_key_base64(&args.secret)
                    .context(
                        "invalid --secret (expected base64 from `rerun server generate-secret`)",
                    )?;
                let token = provider.token_with_hosts(
                    args.expiration,
                    "rerun-oss-server",
                    &args.user,
                    args.permission.clone(),
                    args.server_hosts.clone(),
                )?;
                println!("{token}");
                eprintln!(
                    "\nToken for '{}' ({:?}), valid {}s, usable against host(s) {:?}.\n\
                     Use it as: CatalogClient(\"rerun+http://<host>:51234\", token=\"<token>\")",
                    args.user,
                    args.permission,
                    args.expiration.as_secs(),
                    args.server_hosts,
                );
                return Ok(());
            }
            None => {}
        }

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

#[cfg(test)]
mod tests {
    use clap::Parser as _;

    use super::*;

    #[test]
    fn test_parse_duration() {
        assert_eq!(
            parse_duration("90d").unwrap(),
            std::time::Duration::from_hours(2160)
        );
        assert_eq!(
            parse_duration("12h").unwrap(),
            std::time::Duration::from_hours(12)
        );
        assert_eq!(
            parse_duration("30m").unwrap(),
            std::time::Duration::from_mins(30)
        );
        assert_eq!(
            parse_duration("45s").unwrap(),
            std::time::Duration::from_secs(45)
        );
        assert!(parse_duration("90").is_err());
        assert!(parse_duration("d").is_err());
        assert!(parse_duration("1y").is_err());
    }

    #[test]
    fn test_cli_parsing() {
        // Plain server run, with a token secret.
        let args = Args::try_parse_from(["server", "--token-secret", "c2VjcmV0"]).unwrap();
        assert_eq!(args.token_secret.as_deref(), Some("c2VjcmV0"));
        assert!(args.command.is_none());

        // generate-secret subcommand.
        let args = Args::try_parse_from(["server", "generate-secret"]).unwrap();
        assert!(matches!(args.command, Some(ServerCommand::GenerateSecret)));

        // generate-token requires --server-host.
        assert!(
            Args::try_parse_from([
                "server",
                "generate-token",
                "--secret",
                "x",
                "--user",
                "alice",
            ])
            .is_err()
        );

        let args = Args::try_parse_from([
            "server",
            "generate-token",
            "--secret",
            "x",
            "--user",
            "alice",
            "--permission",
            "read-write",
            "--expiration",
            "7d",
            "--server-host",
            "1.2.3.4",
            "--server-host",
            "rerun-cloud.rerun.svc.cluster.local",
        ])
        .unwrap();
        let Some(ServerCommand::GenerateToken(token_args)) = args.command else {
            panic!("expected generate-token");
        };
        assert_eq!(token_args.user, "alice");
        assert_eq!(token_args.permission, re_auth::Permission::ReadWrite);
        assert_eq!(token_args.expiration, std::time::Duration::from_hours(168));
        assert_eq!(
            token_args.server_hosts,
            vec!["1.2.3.4", "rerun-cloud.rerun.svc.cluster.local"]
        );
    }

    #[test]
    fn test_minted_token_roundtrip() {
        let secret = re_auth::SecretKey::generate(rand::rng());
        let provider = re_auth::RedapProvider::from_secret_key_base64(&secret.to_base64()).unwrap();

        let token = provider
            .token_with_hosts(
                std::time::Duration::from_hours(1),
                "rerun-oss-server",
                "alice",
                re_auth::Permission::Read,
                vec!["1.2.3.4".to_owned(), "10.0.0.1".to_owned()],
            )
            .unwrap();

        // The server-side verification accepts it…
        let claims = provider
            .verify(&token, re_auth::VerificationOptions::default())
            .unwrap();
        assert_eq!(claims.sub(), "alice");

        // …and the client-side host gate allows exactly the listed hosts.
        assert!(token.for_host("1.2.3.4").is_ok());
        assert!(token.for_host("10.0.0.1").is_ok());
        assert!(token.for_host("evil.example.com").is_err());
    }
}
