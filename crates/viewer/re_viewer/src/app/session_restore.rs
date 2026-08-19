//! Session restore: on startup, re-open the remote datasets that were still open when the
//! viewer last shut down — so a restart looks like the page was never closed.
//!
//! Only entries stamped `open_at_exit` come back (a dataset the user closed stays closed).
//! Credentials are resolved silently from the deployment/user config ([`crate::viewer_config`]);
//! entries whose credentials cannot be resolved are skipped and stay in the welcome screen's
//! recents list for manual opening.

use re_data_source::LogDataSource;
use re_viewer_context::{StoreHub, SystemCommand, SystemCommandSender as _};

use super::App;

/// How long to wait for the config fetch before restoring without it (seconds).
const CONFIG_WAIT_TIMEOUT: f64 = 5.0;

impl App {
    /// Runs once, early in the frame loop.
    pub(super) fn maybe_restore_session(&mut self, store_hub: &StoreHub, egui_ctx: &egui::Context) {
        if self.session_restore_attempted {
            return;
        }

        if !self.startup_options.persist_state {
            self.session_restore_attempted = true;
            return;
        }

        if !self
            .state
            .recent_datasets
            .iter()
            .any(|recent| recent.open_at_exit)
        {
            self.session_restore_attempted = true;
            return;
        }

        // Something else is already loading or loaded (a CLI argument, a `?url=` parameter,
        // a dropped file): the user asked for that data, don't butt in with the old session.
        let has_recordings = store_hub
            .store_bundle()
            .entity_dbs()
            .any(|db| db.store_kind() == re_log_types::StoreKind::Recording);
        if has_recordings || !self.rx_log.sources().is_empty() {
            self.session_restore_attempted = true;
            return;
        }

        // Credentials come from the deployment/user config; wait (briefly) for its fetch.
        crate::viewer_config::request();
        let config = if let Some(config) = crate::viewer_config::get() {
            config
        } else {
            let now = egui_ctx.input(|i| i.time);
            let started = *self.session_restore_wait_since.get_or_insert(now);
            if now - started < CONFIG_WAIT_TIMEOUT {
                egui_ctx.request_repaint_after(std::time::Duration::from_millis(100));
                return; // Try again next frame.
            }
            // Config never arrived — restore what works without it (public HF datasets).
            Default::default()
        };
        self.session_restore_attempted = true;

        let to_restore: Vec<crate::recent_datasets::RecentDataset> = self
            .state
            .recent_datasets
            .iter()
            .filter(|recent| recent.open_at_exit)
            .cloned()
            .collect();

        for recent in to_restore {
            let source = match recent.kind {
                crate::recent_datasets::RecentKind::Tos => {
                    let Some(location) = re_data_source::tos::TosLocation::parse(&recent.url)
                    else {
                        continue;
                    };
                    if !config.has_tos_credentials() {
                        re_log::info!(
                            "Not re-opening {} from the last session — no stored credentials; \
                             open it from the welcome screen instead.",
                            recent.url
                        );
                        continue;
                    }
                    LogDataSource::TosDataset(re_data_source::tos::TosDatasetSource {
                        location,
                        credentials: re_data_source::tos::TosCredentials {
                            endpoint: re_data_source::tos::endpoint_for_region(
                                &recent.region,
                                &config.tos_endpoint,
                            ),
                            access_key: config.tos_access_key.clone(),
                            secret_key: config.tos_secret_key.clone(),
                        },
                        rrd_artifacts: config.rrd_artifacts(true),
                    })
                }

                crate::recent_datasets::RecentKind::Hf => {
                    let Some((repo, file_path)) =
                        re_data_source::hf::parse_hf_dataset_input(&recent.url)
                    else {
                        continue;
                    };
                    LogDataSource::HfDataset(re_data_source::hf::HfDatasetSource {
                        repo,
                        file_path,
                        token: config.hf_token.clone(),
                        rrd_artifacts: config.rrd_artifacts(true),
                    })
                }
            };

            re_log::info!(
                "Restoring dataset from the previous session: {}",
                recent.url
            );
            self.command_sender
                .send_system(SystemCommand::LoadDataSource(source));
        }
    }
}
