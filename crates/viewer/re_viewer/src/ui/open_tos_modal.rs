use std::sync::Arc;

use parking_lot::Mutex;
use re_data_source::LogDataSource;
use re_data_source::tos::{TosCredentials, TosDatasetSource, TosLocation};
use re_ui::UiExt as _;
use re_ui::modal::{ModalHandler, ModalWrapper};
use re_viewer_context::{CommandSender, SystemCommand, SystemCommandSender as _};

/// Server-side default TOS settings, served by the deployment at `/tos-config.json`.
///
/// The deployment can hold default credentials (injected as docker secrets); those are used
/// unless the user opts into providing their own. The credential fields are never shown in the
/// dialog unless the user explicitly asks to override them.
#[derive(Clone, serde::Deserialize)]
#[serde(default)]
struct ServerTosConfig {
    tos_endpoint: String,
    tos_region: String,
    tos_access_key: String,
    tos_secret_key: String,

    /// Where converted rrds are stored; an absent key means the default bucket,
    /// `""`/`"off"` disables the artifacts store.
    #[serde(default = "re_data_source::rrd_artifacts::default_artifacts_url")]
    tos_rrd_artifacts_url: String,

    /// How many artifacts to prefetch at once; `0` (or absent) = automatic.
    rrd_artifacts_prefetch: usize,
}

impl Default for ServerTosConfig {
    fn default() -> Self {
        Self {
            tos_endpoint: String::new(),
            tos_region: String::new(),
            tos_access_key: String::new(),
            tos_secret_key: String::new(),
            // The artifacts store is on by default, even with no config file at all.
            tos_rrd_artifacts_url: re_data_source::rrd_artifacts::default_artifacts_url(),
            rrd_artifacts_prefetch: 0,
        }
    }
}

impl ServerTosConfig {
    fn has_credentials(&self) -> bool {
        !self.tos_access_key.is_empty() && !self.tos_secret_key.is_empty()
    }
}

/// Volcengine TOS regions, for the endpoint/region dropdowns. The bool marks regions that
/// also have an S3-compatible *internal* endpoint (`…ivolces.com`, reachable only from
/// inside the Volcengine network). Verified against live DNS + HTTPS probes of
/// `tos-s3-<region>.[i]volces.com` (2026-08-07); manual entry stays available for
/// anything not listed here.
const TOS_REGIONS: &[(&str, bool)] = &[
    ("cn-beijing", true),
    ("cn-beijing2", false),
    ("cn-shanghai", true),
    ("cn-guangzhou", true),
    ("cn-hongkong", false),
    ("ap-southeast-1", true),
    ("ap-southeast-2", false),
    ("ap-southeast-3", false),
];

fn public_endpoint(region: &str) -> String {
    format!("https://tos-s3-{region}.volces.com")
}

fn internal_endpoint(region: &str) -> String {
    format!("https://tos-s3-{region}.ivolces.com")
}

/// Dialog for opening a `LeRobot` dataset stored in Volcengine TOS.
#[derive(Default)]
pub struct OpenTosModal {
    modal: ModalHandler,
    just_opened: bool,

    url: String,
    endpoint: String,
    region: String,

    /// Show the AK/SK inputs and use them instead of the server-side default credentials.
    use_custom_credentials: bool,
    access_key: String,
    secret_key: String,

    /// Inverted so the derived `Default` (false) means "upload converted rrds" — on by default.
    artifact_upload_disabled: bool,

    /// Filled asynchronously from the server's `/tos-config.json` (web only).
    /// `Err` holds why the fetch failed — shown in the dialog, because a silently missing
    /// config looks exactly like "this deployment has no credentials" and is undebuggable.
    server_config: Arc<Mutex<Option<Result<ServerTosConfig, String>>>>,
    server_config_requested: bool,
    server_config_applied: bool,
}

impl OpenTosModal {
    pub fn open(&mut self) {
        self.modal.open();
        self.just_opened = true;
        self.fetch_server_config();
    }

    /// Open with the URL and non-secret connection fields pre-filled (e.g. from the welcome
    /// screen's "recently opened" list). Credentials are never pre-filled from outside.
    pub fn open_prefilled(&mut self, url: &str, endpoint: &str, region: &str) {
        self.url = url.to_owned();
        if !endpoint.is_empty() {
            self.endpoint = endpoint.to_owned();
        }
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

        // On the web the viewer is served next to `/tos-config.json`; natively there is no
        // server, so read the same file from the user's config dir and let env vars override.
        #[cfg(target_arch = "wasm32")]
        {
            let config = self.server_config.clone();
            // `SameOrigin`: the deployment may sit behind HTTP Basic auth, and ehttp's
            // default (`Omit`) tells the browser to strip the authenticated session,
            // turning every fetch into a 401.
            let request = ehttp::Request::get("tos-config.json")
                .with_credentials(ehttp::Credentials::SameOrigin);
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
                    re_log::warn!(
                        "Failed to load server TOS defaults: {err}\nFile: tos-config.json"
                    );
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
            env_override(&mut parsed.tos_region, "TOS_REGION");
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

    /// Pre-fill still-empty (non-secret) fields once the server defaults arrive.
    fn apply_server_config(&mut self) {
        if self.server_config_applied {
            return;
        }
        let Some(Ok(config)) = self.server_config.lock().clone() else {
            return;
        };
        self.server_config_applied = true;

        if self.endpoint.is_empty() {
            self.endpoint = config.tos_endpoint.clone();
        }
        if self.region.is_empty() {
            self.region = config.tos_region.clone();
        }

        // Without server-side default credentials the user must provide their own.
        if !config.has_credentials() {
            self.use_custom_credentials = true;
        }
        // The AK/SK input fields themselves are never pre-filled.
    }

    pub fn ui(&mut self, ui: &egui::Ui, command_sender: &CommandSender) {
        self.apply_server_config();

        let fetched = self.server_config.lock().clone();
        let config_resolved = fetched.is_some();
        let config_error = fetched.as_ref().and_then(|r| r.as_ref().err().cloned());
        let server_config = fetched.and_then(Result::ok).unwrap_or_default();

        // Once we know there are no server-side credentials (config arrived without them, or
        // the fetch failed), the user's own AK/SK are the only way forward — force the custom
        // fields open instead of leaving both paths disabled.
        if config_resolved && !server_config.has_credentials() {
            self.use_custom_credentials = true;
        }

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

                        // Endpoint and region: free text with a dropdown of known values.
                        // The list may lag behind newly opened regions, so typing always works.
                        ui.label("Endpoint:");
                        ui.horizontal(|ui| {
                            let menu_width = 20.0;
                            egui::TextEdit::singleline(&mut self.endpoint)
                                .hint_text("https://tos-s3-cn-beijing.volces.com")
                                .desired_width(
                                    ui.available_width() - menu_width - ui.spacing().item_spacing.x,
                                )
                                .show(ui);
                            ui.menu_button("⏷", |ui| {
                                for (region, _) in TOS_REGIONS {
                                    if ui.button(public_endpoint(region)).clicked() {
                                        self.endpoint = public_endpoint(region);
                                        self.region = (*region).to_owned();
                                        ui.close();
                                    }
                                }
                                ui.separator();
                                for (region, has_internal) in TOS_REGIONS {
                                    if *has_internal
                                        && ui
                                            .button(format!(
                                                "{} (internal)",
                                                internal_endpoint(region)
                                            ))
                                            .clicked()
                                    {
                                        self.endpoint = internal_endpoint(region);
                                        self.region = (*region).to_owned();
                                        ui.close();
                                    }
                                }
                            });
                        });
                        ui.end_row();

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
                                for (region, _) in TOS_REGIONS {
                                    if ui.button(*region).clicked() {
                                        self.region = (*region).to_owned();
                                        if self.endpoint.is_empty() {
                                            self.endpoint = public_endpoint(region);
                                        }
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
                if self.endpoint.contains(".ivolces.com") {
                    ui.warning_label(
                        "This is an internal endpoint (.ivolces.com), only reachable from \
                         inside the Volcengine network. In a browser you most likely need \
                         the public endpoint (.volces.com).",
                    );
                }

                if let Some(err) = &config_error {
                    ui.warning_label(format!(
                        "Failed to load the server-side defaults (endpoint, credentials): \
                         {err}\nFile: tos-config.json — enter the values manually below.",
                    ));
                }

                // Credentials: the deployment's docker-secret defaults are used unless the user
                // opts into providing their own.
                ui.add_space(2.0);
                ui.add_enabled_ui(server_config.has_credentials(), |ui| {
                    ui.re_checkbox(&mut self.use_custom_credentials, "Use non-default AK/SK");
                });

                if self.use_custom_credentials {
                    egui::Grid::new("tos_credential_fields")
                        .num_columns(2)
                        .spacing([8.0, 6.0])
                        .show(ui, |ui| {
                            ui.label("Access key:");
                            egui::TextEdit::singleline(&mut self.access_key)
                                .desired_width(f32::INFINITY)
                                .show(ui);
                            ui.end_row();

                            ui.label("Secret key:");
                            egui::TextEdit::singleline(&mut self.secret_key)
                                .password(true)
                                .desired_width(f32::INFINITY)
                                .show(ui);
                            ui.end_row();
                        });
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

                let credentials_ok = if self.use_custom_credentials {
                    !self.access_key.trim().is_empty() && !self.secret_key.trim().is_empty()
                } else {
                    server_config.has_credentials()
                };

                let location = TosLocation::parse(&self.url);
                let can_open = location.is_some() && !self.endpoint.is_empty() && credentials_ok;

                if !self.url.is_empty() && location.is_none() {
                    ui.error_label("The dataset URL should look like tos://bucket/prefix/");
                } else if !credentials_ok {
                    if self.use_custom_credentials {
                        ui.label("Enter an access key and a secret key.");
                    } else {
                        ui.label(
                            "This deployment has no default credentials — provide your own AK/SK.",
                        );
                    }
                } else if !can_open {
                    ui.label("Endpoint is required.");
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
                            let (access_key, secret_key) = if self.use_custom_credentials {
                                (
                                    self.access_key.trim().to_owned(),
                                    self.secret_key.trim().to_owned(),
                                )
                            } else {
                                (
                                    server_config.tos_access_key.clone(),
                                    server_config.tos_secret_key.clone(),
                                )
                            };

                            let credentials = TosCredentials {
                                endpoint: self.endpoint.trim().to_owned(),
                                region: self.region.trim().to_owned(),
                                access_key,
                                secret_key,
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
