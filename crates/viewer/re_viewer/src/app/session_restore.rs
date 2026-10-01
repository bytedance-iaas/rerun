//! Session restore: on startup, re-open the remote datasets that were still open when the
//! viewer last shut down — so a restart looks like the page was never closed.
//!
//! Only entries stamped `open_at_exit` come back (a dataset the user closed stays closed).
//! Credentials are resolved silently from the deployment/user config ([`crate::viewer_config`]);
//! entries whose credentials cannot be resolved are skipped and stay in the welcome screen's
//! recents list for manual opening. A dataset opened from the curation console needs no
//! credentials here: the console signs its reads again.

use re_data_source::LogDataSource;
use re_viewer_context::{StoreHub, SystemCommand, SystemCommandSender as _};

use super::App;

/// How long to wait for the config fetch before restoring without it (seconds).
const CONFIG_WAIT_TIMEOUT: f64 = 5.0;

/// A `tos://` URL open waiting for the deployment/user config ([`SystemCommand::LoadTosDataset`]).
pub(super) struct PendingTosOpen {
    pub location: re_data_source::tos::TosLocation,

    /// The bucket's region; empty = the deployment endpoint's region.
    pub region: String,

    /// The curation console registration, whose reads the console signs.
    pub curator_dataset: Option<String>,
}

/// How to reach a dataset: the console's signing when it was opened as a console
/// registration and this viewer can use that (web, same origin), else the deployment keys.
/// `None`: neither is available.
fn tos_access(
    config: &crate::viewer_config::ViewerConfig,
    region: &str,
    curator_dataset: Option<&str>,
    what: &str,
) -> Option<re_data_source::tos::TosAccess> {
    if let Some(dataset_id) = curator_dataset {
        if let Some(access) = config.curator_access(region, dataset_id) {
            return Some(access);
        }
        re_log::info!(
            "Curation console dataset {dataset_id}: only the web viewer on the console's origin \
             can have its reads signed there — using this deployment's own settings instead.\n\
             Url: {what}"
        );
    }
    config.has_tos_credentials().then(|| {
        re_data_source::tos::TosCredentials {
            endpoint: re_data_source::tos::endpoint_for_region(region, &config.tos_endpoint),
            access_key: config.tos_access_key.clone(),
            secret_key: config.tos_secret_key.clone(),
            session_token: config.tos_session_token.clone(),
        }
        .into()
    })
}

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
        if has_recordings || !self.rx_log.sources().is_empty() || !self.pending_tos_opens.is_empty()
        {
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
                crate::recent_datasets::RecentKind::HfCache => {
                    let Some(location) = re_data_source::tos::TosLocation::parse(&recent.url)
                    else {
                        continue;
                    };
                    // The cache bucket is public-read: restorable without any credentials.
                    LogDataSource::TosDataset(re_data_source::tos::hf_cache::source(
                        location,
                        config.rrd_artifacts(true),
                    ))
                }

                crate::recent_datasets::RecentKind::Tos => {
                    let Some(location) = re_data_source::tos::TosLocation::parse(&recent.url)
                    else {
                        continue;
                    };
                    let Some(access) = tos_access(
                        &config,
                        &recent.region,
                        recent.curator_dataset.as_deref(),
                        &recent.url,
                    ) else {
                        re_log::info!(
                            "Not re-opening {} from the last session — no stored credentials; \
                             open it from the welcome screen instead.",
                            recent.url
                        );
                        continue;
                    };
                    LogDataSource::TosDataset(re_data_source::tos::TosDatasetSource {
                        location,
                        access,
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

    /// Finishes `tos://` URL opens ([`SystemCommand::LoadTosDataset`]) once the
    /// deployment/user config — which holds the credentials — has resolved.
    ///
    /// Runs every frame; does nothing while there is no pending open.
    pub(super) fn process_pending_tos_opens(&mut self, egui_ctx: &egui::Context) {
        if self.pending_tos_opens.is_empty() {
            return;
        }

        crate::viewer_config::request();
        let config = if let Some(config) = crate::viewer_config::get() {
            self.pending_tos_wait_since = None;
            config
        } else {
            let now = egui_ctx.input(|i| i.time);
            let started = *self.pending_tos_wait_since.get_or_insert(now);
            if now - started < CONFIG_WAIT_TIMEOUT {
                egui_ctx.request_repaint_after(std::time::Duration::from_millis(100));
                return; // Try again next frame.
            }
            // Config never arrived — fail below with a clear message instead of hanging.
            self.pending_tos_wait_since = None;
            Default::default()
        };

        // Two kinds of opens need no local credentials at all — finish those right away:
        // HF-cache locations (the public ai-infra bucket, read anonymously) and
        // console-signed opens (a curation-console "Visualize" link, signed there).
        // Only the opens that actually need local keys stay to face the credential gate.
        let mut needing_keys = Vec::new();
        for open in std::mem::take(&mut self.pending_tos_opens) {
            if re_data_source::tos::hf_cache::is_cache_location(&open.location) {
                self.command_sender
                    .send_system(SystemCommand::LoadDataSource(LogDataSource::TosDataset(
                        re_data_source::tos::hf_cache::source(
                            open.location,
                            config.rrd_artifacts(true),
                        ),
                    )));
                continue;
            }
            let console_access = open
                .curator_dataset
                .as_deref()
                .and_then(|dataset_id| config.curator_access(&open.region, dataset_id));
            if let Some(access) = console_access {
                self.command_sender
                    .send_system(SystemCommand::LoadDataSource(LogDataSource::TosDataset(
                        re_data_source::tos::TosDatasetSource {
                            location: open.location,
                            access,
                            rrd_artifacts: config.rrd_artifacts(true),
                        },
                    )));
            } else {
                needing_keys.push(open);
            }
        }
        self.pending_tos_opens = needing_keys;
        if self.pending_tos_opens.is_empty() {
            self.credentials_prompt_owned_by_pending = false;
            return;
        }

        // No credentials anywhere (deployment config, env, session slot): prompt for them
        // and keep the opens pending — the next frames re-enter here, and the session slot
        // shows up through `viewer_config::get` once the user saves. Only an outcome of a
        // prompt this flow itself opened may drop the queue: cancelling a startup or
        // menu-opened prompt says nothing about these opens.
        if !config.has_tos_credentials() {
            if self.credentials_prompt_owned_by_pending {
                match self.state.tos_credentials_modal.take_outcome() {
                    Some(crate::ui::CredentialsOutcome::Cancelled) => {
                        self.credentials_prompt_owned_by_pending = false;
                        for open in std::mem::take(&mut self.pending_tos_opens) {
                            re_log::error!(
                                "Can't open {} — no Volcengine credentials were provided.",
                                open.location
                            );
                        }
                    }
                    // `Saved` with still-missing credentials cannot happen (saving fills
                    // the slot); `None` means the prompt is still open.
                    outcome => {
                        if outcome.is_some() {
                            self.credentials_prompt_owned_by_pending = false;
                        }
                    }
                }
            } else if !self.state.tos_credentials_modal.is_open() {
                self.state.tos_credentials_modal.open();
                self.credentials_prompt_owned_by_pending = true;
            }
            // A prompt someone else opened is already on screen: wait for it — a save
            // fills the slot and the next frame proceeds normally.
            return;
        }
        self.credentials_prompt_owned_by_pending = false;

        for open in std::mem::take(&mut self.pending_tos_opens) {
            let Some(access) = tos_access(
                &config,
                &open.region,
                open.curator_dataset.as_deref(),
                &open.location.to_string(),
            ) else {
                // `has_tos_credentials` held above, so the keys fallback always resolves.
                continue;
            };
            self.command_sender
                .send_system(SystemCommand::LoadDataSource(LogDataSource::TosDataset(
                    re_data_source::tos::TosDatasetSource {
                        location: open.location,
                        access,
                        rrd_artifacts: config.rrd_artifacts(true),
                    },
                )));
        }
    }

    /// On startup, once: if no TOS credentials are configured anywhere (deployment config,
    /// env, session slot), open the credentials prompt so the user can enter them up front.
    ///
    /// Dismissable — someone viewing local files or public HF datasets never needs TOS.
    /// Skipping just means the on-demand prompt (or the open dialogs) asks later.
    pub(super) fn maybe_prompt_startup_credentials(&mut self, egui_ctx: &egui::Context) {
        if self.startup_credentials_prompt_attempted {
            return;
        }

        // A pending `tos://` open prompts on its own; don't stack a second prompt.
        if !self.pending_tos_opens.is_empty() {
            self.startup_credentials_prompt_attempted = true;
            return;
        }

        crate::viewer_config::request();
        let config = if let Some(config) = crate::viewer_config::get() {
            config
        } else {
            let now = egui_ctx.input(|i| i.time);
            let started = *self.startup_credentials_wait_since.get_or_insert(now);
            if now - started < CONFIG_WAIT_TIMEOUT {
                egui_ctx.request_repaint_after(std::time::Duration::from_millis(100));
                return; // Try again next frame.
            }
            // Config never arrived — treat as empty, same as the other flows.
            Default::default()
        };
        self.startup_credentials_prompt_attempted = true;

        if !config.has_tos_credentials() && !self.state.tos_credentials_modal.is_open() {
            self.state.tos_credentials_modal.open();
        }
    }
}
