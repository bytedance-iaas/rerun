use std::sync::Arc;

use parking_lot::Mutex;
use re_data_source::LogDataSource;
use re_data_source::tos::{TosCredentials, TosDatasetSource, TosLocation};
use re_ui::UiExt as _;
use re_ui::modal::{ModalHandler, ModalWrapper};
use re_viewer_context::{CommandSender, SystemCommand, SystemCommandSender as _};

/// The deployment's TOS connection settings, served at `/config.json`.
///
/// Endpoint and credentials (injected as docker secrets) come exclusively from here —
/// the dialog itself only asks for the dataset URL.
#[derive(Clone, serde::Deserialize)]
#[serde(default)]
struct ServerTosConfig {
    tos_endpoint: String,
    tos_access_key: String,
    tos_secret_key: String,

    /// Where converted rrds are stored; absent/`""`/`"off"` disables the artifacts store.
    tos_rrd_artifacts_url: String,

    /// How many artifacts to prefetch at once; `0` (or absent) = automatic.
    rrd_artifacts_prefetch: usize,
}

impl Default for ServerTosConfig {
    fn default() -> Self {
        Self {
            tos_endpoint: String::new(),
            tos_access_key: String::new(),
            tos_secret_key: String::new(),
            tos_rrd_artifacts_url: String::new(),
            rrd_artifacts_prefetch: 0,
        }
    }
}

impl ServerTosConfig {
    fn has_credentials(&self) -> bool {
        !self.tos_access_key.is_empty() && !self.tos_secret_key.is_empty()
    }
}

/// Volcengine TOS regions, for the Region dropdown — the list and order mirror the
/// volcengine console's create-bucket region picker (2026-08-18). The list may lag
/// behind newly opened regions, so free-text entry stays available.
const TOS_REGIONS: &[&str] = &[
    "cn-beijing",
    "ap-southeast-1",
    "ap-southeast-3",
    "cn-guangzhou",
    "cn-hongkong",
    "cn-shanghai",
];

/// Dialog for opening a `LeRobot` dataset stored in Volcengine TOS.
#[derive(Default)]
pub struct OpenTosModal {
    modal: ModalHandler,
    just_opened: bool,

    url: String,

    /// The bucket's region. Defaults to the deployment endpoint's region; the endpoint is
    /// derived from this (`re_data_source::tos::endpoint_for_region`), so this is the only
    /// connection field the user ever touches.
    region: String,

    /// Inverted so the derived `Default` (false) means "upload converted rrds" — on by default.
    artifact_upload_disabled: bool,

    /// Filled asynchronously from the server's `/config.json` (web only).
    /// `Err` holds why the fetch failed — shown in the dialog, because a silently missing
    /// config looks exactly like "this deployment has no credentials" and is undebuggable.
    server_config: Arc<Mutex<Option<Result<ServerTosConfig, String>>>>,
    server_config_requested: bool,
}

impl OpenTosModal {
    pub fn open(&mut self) {
        self.modal.open();
        self.just_opened = true;
        self.fetch_server_config();
    }

    /// Open with the dataset URL (and its remembered region) pre-filled, e.g. from the
    /// welcome screen's "recently opened" list.
    pub fn open_prefilled(&mut self, url: &str, region: &str) {
        self.url = url.to_owned();
        if !region.is_empty() {
            self.region = region.to_owned();
        }
        self.open();
    }

    /// Fetch server-side defaults once per app run.
    fn fetch_server_config(&mut self) {
        if self.server_config_requested {
            return;
        }
        self.server_config_requested = true;

        // On the web the viewer is served next to `/config.json`; natively there is no
        // server, so read the same file from the user's config dir and let env vars override.
        #[cfg(target_arch = "wasm32")]
        {
            let config = self.server_config.clone();
            // `SameOrigin`: the deployment may sit behind HTTP Basic auth, and ehttp's
            // default (`Omit`) tells the browser to strip the authenticated session,
            // turning every fetch into a 401.
            let request =
                ehttp::Request::get("config.json").with_credentials(ehttp::Credentials::SameOrigin);
            ehttp::fetch(request, move |result| {
                let outcome = match result {
                    Ok(response) if response.status == 200 => {
                        serde_json::from_slice::<ServerTosConfig>(&response.bytes)
                            .map_err(|err| format!("invalid JSON: {err}"))
                    }
                    Ok(response) => {
                        Err(format!("HTTP {} {}", response.status, response.status_text))
                    }
                    Err(err) => Err(err),
                };
                if let Err(err) = &outcome {
                    re_log::warn!("Failed to load server TOS defaults: {err}\nFile: config.json");
                }
                *config.lock() = Some(outcome);
            });
        }

        #[cfg(not(target_arch = "wasm32"))]
        {
            let mut parsed = super::native_config::load_local_config_bytes()
                .and_then(|bytes| serde_json::from_slice::<ServerTosConfig>(&bytes).ok())
                .unwrap_or_default();

            fn env_override(field: &mut String, key: &str) {
                if let Ok(value) = std::env::var(key)
                    && !value.is_empty()
                {
                    *field = value;
                }
            }
            env_override(&mut parsed.tos_endpoint, "TOS_ENDPOINT");
            env_override(&mut parsed.tos_access_key, "TOS_ACCESS_KEY");
            env_override(&mut parsed.tos_secret_key, "TOS_SECRET_KEY");
            env_override(&mut parsed.tos_rrd_artifacts_url, "TOS_RRD_ARTIFACTS_URL");
            if let Ok(value) = std::env::var("RRD_ARTIFACTS_PREFETCH")
                && let Ok(n) = value.trim().parse()
            {
                parsed.rrd_artifacts_prefetch = n;
            }

            *self.server_config.lock() = Some(Ok(parsed));
        }
    }

    pub fn ui(&mut self, ui: &egui::Ui, command_sender: &CommandSender) {
        let fetched = self.server_config.lock().clone();
        let config_resolved = fetched.is_some();
        let config_error = fetched.as_ref().and_then(|r| r.as_ref().err().cloned());
        let server_config = fetched.and_then(Result::ok).unwrap_or_default();

        // Default region: wherever the deployment's endpoint lives.
        if self.region.is_empty() && config_resolved {
            self.region = re_data_source::tos::region_from_endpoint(&server_config.tos_endpoint);
        }
        let resolved_endpoint =
            re_data_source::tos::endpoint_for_region(&self.region, &server_config.tos_endpoint);

        self.modal.ui(
            ui.ctx(),
            || ModalWrapper::new("Open from Volcengine TOS"),
            |ui| {
                ui.strong("Stream a LeRobot dataset from a TOS bucket.");
                ui.add_space(4.0);

                egui::Grid::new("tos_fields")
                    .num_columns(2)
                    .spacing([8.0, 6.0])
                    .show(ui, |ui| {
                        ui.label("Dataset URL:");
                        let url_edit = egui::TextEdit::singleline(&mut self.url)
                            .hint_text("tos://bucket/path/to/dataset/")
                            .desired_width(f32::INFINITY)
                            .show(ui);
                        if self.just_opened {
                            url_edit.response.request_focus();
                        }
                        ui.end_row();

                        // Region: free text with a dropdown of known values. The list may lag
                        // behind newly opened regions, so typing always works. The endpoint is
                        // derived from the region, so there is nothing else to fill.
                        ui.label("Region:");
                        ui.horizontal(|ui| {
                            let menu_width = 20.0;
                            egui::TextEdit::singleline(&mut self.region)
                                .hint_text("cn-beijing")
                                .desired_width(
                                    ui.available_width() - menu_width - ui.spacing().item_spacing.x,
                                )
                                .show(ui);
                            ui.menu_button("⏷", |ui| {
                                for region in TOS_REGIONS {
                                    if ui.button(*region).clicked() {
                                        self.region = (*region).to_owned();
                                        ui.close();
                                    }
                                }
                            });
                        });
                        ui.end_row();
                    });

                // In the browser the requests go out from the user's machine, so internal
                // endpoints (only routable inside the Volcengine network) won't work.
                #[cfg(target_arch = "wasm32")]
                if resolved_endpoint.contains(".ivolces.com") {
                    ui.warning_label(
                        "This is an internal endpoint (.ivolces.com), only reachable from \
                         inside the Volcengine network. In a browser you most likely need \
                         the public endpoint (.volces.com).",
                    );
                }

                if let Some(err) = &config_error {
                    ui.warning_label(format!(
                        "Failed to load the deployment's TOS settings (endpoint, credentials): \
                         {err}\nFile: config.json — opening datasets needs this fixed.",
                    ));
                }

                // Converted episodes are uploaded to a shared rrd artifacts store by default, so the next
                // open (by anyone) skips the conversion. Deployment-disabled → no checkbox.
                if let Some(artifacts_location) =
                    re_data_source::rrd_artifacts::parse_artifacts_url(&server_config.tos_rrd_artifacts_url)
                {
                    let mut upload = !self.artifact_upload_disabled;
                    ui.re_checkbox(&mut upload, "Upload converted rrd to the artifacts store")
                        .on_hover_text(format!("{artifacts_location}"));
                    self.artifact_upload_disabled = !upload;
                }

                // Credentials come exclusively from the deployment config (docker secrets
                // on the web, config.json natively); the endpoint is derived from the
                // chosen region. The signing region in turn is derived from the endpoint
                // (see `TosCredentials::region`).
                let connection_ok =
                    !self.region.trim().is_empty() && server_config.has_credentials();

                let location = TosLocation::parse(&self.url);
                let can_open = location.is_some() && connection_ok;

                if !self.url.is_empty() && location.is_none() {
                    ui.error_label("The dataset URL should look like tos://bucket/prefix/");
                } else if !connection_ok {
                    ui.label(if !config_resolved {
                        "Loading the deployment's TOS settings…"
                    } else if self.region.trim().is_empty() {
                        "Region is required."
                    } else {
                        "This deployment has no TOS credentials configured (config.json)."
                    });
                } else {
                    ui.label(
                        "Episodes appear immediately and stream in one by one; \
                         click an episode to load it first.",
                    );
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let button_width = ui.tokens().modal_button_width;

                    let open_response = ui.add_enabled(
                        can_open,
                        egui::Button::new("Open").min_size(egui::vec2(button_width, 0.0)),
                    );
                    if open_response.clicked()
                        || can_open && ui.input(|i| i.key_pressed(egui::Key::Enter))
                    {
                        if let Some(location) = location {
                            let credentials = TosCredentials {
                                endpoint: resolved_endpoint.clone(),
                                access_key: server_config.tos_access_key.clone(),
                                secret_key: server_config.tos_secret_key.clone(),
                            };
                            // The artifacts bucket uses the same credentials as the source.
                            let rrd_artifacts = re_data_source::rrd_artifacts::parse_artifacts_url(
                                &server_config.tos_rrd_artifacts_url,
                            )
                            .map(|artifacts_location| {
                                re_data_source::rrd_artifacts::RrdArtifactsConfig {
                                    location: artifacts_location,
                                    credentials: credentials.clone(),
                                    write_back: !self.artifact_upload_disabled,
                                    prefetch_items: server_config.rrd_artifacts_prefetch,
                                }
                            });

                            command_sender.send_system(SystemCommand::LoadDataSource(
                                LogDataSource::TosDataset(TosDatasetSource {
                                    location,
                                    credentials,
                                    rrd_artifacts,
                                }),
                            ));
                        }
                        ui.close();
                    }

                    let cancel_response =
                        ui.add(egui::Button::new("Cancel").min_size(egui::vec2(button_width, 0.0)));
                    if cancel_response.clicked() {
                        ui.close();
                    }
                });
            },
        );

        self.just_opened = false;
    }
}
