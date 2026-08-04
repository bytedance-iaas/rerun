use re_data_source::rrd_artifacts::RrdArtifactsConfig;
use re_data_source::tos::{TosCredentials, TosLocation};
use re_log_channel::SmartMessagePayload;

/// Pre-convert remote `LeRobot` datasets into rrd artifacts, headlessly.
///
/// Walks every episode of the given datasets: episodes whose artifact in the store is
/// already up to date (same source fingerprint) are skipped with a single HEAD request;
/// the rest are downloaded, converted, and uploaded to the artifacts store. Run it again
/// any time — an unchanged dataset costs only the freshness checks.
///
/// Credentials and the artifacts-store location resolve exactly like in the viewer:
/// the `TOS_*` / `HF_TOKEN` / `TOS_RRD_ARTIFACTS_URL` environment variables override
/// `~/.rerun/tos-config.json` (or the file `$RERUN_TOS_CONFIG` points at).
///
/// Examples:
///
///   `rerun rrd-convert tos://bucket/datasets/pick-place/`
///
///   `rerun rrd-convert hf://org/dataset-name`
#[derive(Debug, Clone, clap::Parser)]
pub struct RrdConvertCommand {
    /// Dataset URLs to convert: `tos://bucket/prefix/` or `hf://org/name`.
    #[clap(required = true)]
    urls: Vec<String>,

    /// Override the artifacts-store location (`tos://bucket/prefix/`).
    ///
    /// Defaults to `tos_rrd_artifacts_url` from the config file / environment.
    #[clap(long)]
    artifacts_url: Option<String>,
}

impl RrdConvertCommand {
    pub fn run(self, tokio_runtime: &tokio::runtime::Handle) -> anyhow::Result<()> {
        let _guard = tokio_runtime.enter();

        let config = re_data_source::rrd_artifacts::load_local_config();
        anyhow::ensure!(
            !config.tos_access_key.is_empty() && !config.tos_secret_key.is_empty(),
            "No TOS credentials. Provide TOS_ACCESS_KEY / TOS_SECRET_KEY (or ~/.rerun/tos-config.json) — \
             they are needed both for tos:// sources and for uploading to the artifacts store."
        );

        let credentials = TosCredentials {
            endpoint: config.tos_endpoint.clone(),
            region: config.tos_region.clone(),
            access_key: config.tos_access_key.clone(),
            secret_key: config.tos_secret_key.clone(),
        };

        let artifacts_url = self
            .artifacts_url
            .as_deref()
            .unwrap_or(&config.tos_rrd_artifacts_url);
        let artifacts_location = re_data_source::rrd_artifacts::parse_artifacts_url(artifacts_url)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "No artifacts store to write to (configured URL: {artifacts_url:?}). \
                     Set --artifacts-url or TOS_RRD_ARTIFACTS_URL to tos://bucket/prefix/."
                )
            })?;
        let rrd_artifacts = Some(RrdArtifactsConfig {
            location: artifacts_location,
            credentials: credentials.clone(),
            write_back: true,
            prefetch_items: 0, // Converting never downloads artifacts.
        });

        let mut failures = 0usize;
        for url in &self.urls {
            re_log::info!("Converting dataset: {url}");

            let rx = if let Some(location) = TosLocation::parse(url) {
                re_data_source::tos::convert_lerobot_dataset(
                    re_data_source::tos::TosDatasetSource {
                        location,
                        credentials: credentials.clone(),
                        rrd_artifacts: rrd_artifacts.clone(),
                    },
                )
            } else if let Some((repo, file_path)) = url
                .strip_prefix("hf://")
                .and_then(re_data_source::hf::parse_hf_dataset_input)
            {
                anyhow::ensure!(
                    file_path.is_none(),
                    "Point at a whole dataset, not a single file: {url}"
                );
                re_data_source::hf::convert_lerobot_dataset(re_data_source::hf::HfDatasetSource {
                    repo,
                    file_path: None,
                    token: config.hf_token.clone(),
                    rrd_artifacts: rrd_artifacts.clone(),
                })
            } else {
                anyhow::bail!("Unsupported dataset URL (expected tos://… or hf://…): {url}");
            };

            // Drain the channel until the stream finishes; the convert pipeline logs
            // per-episode progress itself. A Quit with an error marks the dataset failed.
            let mut failed = false;
            while let Ok(msg) = rx.recv() {
                match msg.payload {
                    SmartMessagePayload::Msg(_) => {}
                    SmartMessagePayload::Flush { on_flush_done } => on_flush_done(),
                    SmartMessagePayload::Quit(err) => {
                        if let Some(err) = err {
                            re_log::error!("Conversion failed: {err}\nDataset: {url}");
                            failed = true;
                        }
                        break;
                    }
                }
            }
            if failed {
                failures += 1;
            }
        }

        anyhow::ensure!(
            failures == 0,
            "{failures} of {} dataset(s) failed to convert",
            self.urls.len()
        );
        Ok(())
    }
}
