use re_i18n::{tr, trf};
use std::sync::Arc;

use parking_lot::Mutex;
use re_data_source::LogDataSource;
use re_data_source::hf::{HfDatasetSource, parse_hf_dataset_input};
use re_ui::UiExt as _;
use re_ui::modal::{ModalHandler, ModalWrapper};
use re_viewer_context::{CommandSender, SystemCommand, SystemCommandSender as _};

/// Server-side Hugging Face defaults, served by the deployment at `/config.json`.
///
/// The deployment can hold a default token (injected as a docker secret); it is used unless the
/// user opts into providing their own. The token is never shown in the dialog.
#[derive(Clone, serde::Deserialize)]
#[serde(default)]
struct ServerHfConfig {
    hf_token: String,

    /// Hub base-URL override (e.g. a mirror); empty = the official huggingface.co.
    hf_endpoint: String,

    // The rrd artifacts store lives in a TOS bucket regardless of the dataset's source, so the HF dialog
    // needs the TOS connection settings too (same keys, same config file).
    tos_endpoint: String,
    tos_access_key: String,
    tos_secret_key: String,

    /// Where converted rrds are stored; absent/`""`/`"off"` disables the artifacts store.
    tos_rrd_artifacts_url: String,

    /// The artifacts bucket's region; empty = the deployment's own region.
    tos_rrd_artifacts_region: String,

    /// How many artifacts to prefetch at once; `0` (or absent) = automatic.
    rrd_artifacts_prefetch: usize,
}

impl Default for ServerHfConfig {
    fn default() -> Self {
        Self {
            hf_token: String::new(),
            hf_endpoint: String::new(),
            tos_endpoint: String::new(),
            tos_access_key: String::new(),
            tos_secret_key: String::new(),
            tos_rrd_artifacts_url: String::new(),
            tos_rrd_artifacts_region: String::new(),
            rrd_artifacts_prefetch: 0,
        }
    }
}

impl ServerHfConfig {
    /// The resolved rrd-artifacts target — `None` when disabled or without TOS credentials.
    fn rrd_artifacts(
        &self,
        write_back: bool,
    ) -> Option<re_data_source::rrd_artifacts::RrdArtifactsConfig> {
        let location =
            re_data_source::rrd_artifacts::parse_artifacts_url(&self.tos_rrd_artifacts_url)?;
        if self.tos_access_key.is_empty() || self.tos_secret_key.is_empty() {
            return None; // No credentials for the artifacts bucket: silently skip.
        }
        Some(re_data_source::rrd_artifacts::RrdArtifactsConfig {
            location,
            credentials: re_data_source::tos::TosCredentials {
                // Empty artifacts region = the deployment's own (returns the endpoint verbatim).
                endpoint: re_data_source::tos::endpoint_for_region(
                    &self.tos_rrd_artifacts_region,
                    &self.tos_endpoint,
                ),
                access_key: self.tos_access_key.clone(),
                secret_key: self.tos_secret_key.clone(),
            },
            write_back,
            prefetch_items: self.rrd_artifacts_prefetch,
        })
    }
}

/// Dialog for opening a `LeRobot` dataset stored on Hugging Face.
#[derive(Default)]
pub struct OpenHfModal {
    modal: ModalHandler,
    just_opened: bool,

    dataset: String,

    /// Show the token input and use it instead of the server-side default token.
    use_custom_token: bool,
    token: String,

    /// Inverted so the derived `Default` (false) means "upload converted rrds" — on by default.
    artifact_upload_disabled: bool,

    /// Filled asynchronously from the server's `/config.json` (web only).
    /// `Err` holds why the fetch failed — shown in the dialog: without this config the
    /// rrd artifacts store has no credentials and silently degrades to mp4 conversion.
    server_config: Arc<Mutex<Option<Result<ServerHfConfig, String>>>>,
    server_config_requested: bool,
}

impl OpenHfModal {
    pub fn open(&mut self) {
        self.modal.open();
        self.just_opened = true;
        self.fetch_server_config();
    }

    /// Open with the dataset field pre-filled (e.g. from the welcome screen's "recently opened"
    /// list). The token is never pre-filled from outside.
    pub fn open_prefilled(&mut self, dataset: &str) {
        self.dataset = dataset.to_owned();
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
                        serde_json::from_slice::<ServerHfConfig>(&response.bytes)
                            .inspect(|parsed| {
                                re_data_source::hf::set_configured_endpoint(&parsed.hf_endpoint);
                            })
                            .map_err(|err| format!("invalid JSON: {err}"))
                    }
                    Ok(response) => {
                        Err(format!("HTTP {} {}", response.status, response.status_text))
                    }
                    Err(err) => Err(err),
                };
                if let Err(err) = &outcome {
                    re_log::warn!("{}", trf!("Failed to load server TOS defaults: {err}\nFile: config.json", "加载服务器端默认配置失败：{err}\n文件：config.json"));
                }
                *config.lock() = Some(outcome);
            });
        }

        #[cfg(not(target_arch = "wasm32"))]
        {
            let mut parsed = super::native_config::load_local_config_bytes()
                .and_then(|bytes| serde_json::from_slice::<ServerHfConfig>(&bytes).ok())
                .unwrap_or_default();

            fn env_override(field: &mut String, key: &str) {
                if let Ok(value) = std::env::var(key)
                    && !value.is_empty()
                {
                    *field = value;
                }
            }
            env_override(&mut parsed.hf_token, "HF_TOKEN");
            env_override(&mut parsed.hf_endpoint, "HF_ENDPOINT");
            env_override(&mut parsed.tos_endpoint, "TOS_ENDPOINT");
            env_override(&mut parsed.tos_access_key, "TOS_ACCESS_KEY");
            env_override(&mut parsed.tos_secret_key, "TOS_SECRET_KEY");
            env_override(&mut parsed.tos_rrd_artifacts_url, "TOS_RRD_ARTIFACTS_URL");
            env_override(
                &mut parsed.tos_rrd_artifacts_region,
                "TOS_RRD_ARTIFACTS_REGION",
            );
            re_data_source::hf::set_configured_endpoint(&parsed.hf_endpoint);
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
        let config_error = fetched.as_ref().and_then(|r| r.as_ref().err().cloned());
        let server_config = fetched.and_then(Result::ok).unwrap_or_default();

        self.modal.ui(
            ui.ctx(),
            || ModalWrapper::new(tr("Open from Hugging Face", "从 Hugging Face 打开")),
            |ui| {
                ui.strong(tr("Stream a LeRobot dataset from Hugging Face.", "从 Hugging Face 流式读取 LeRobot 数据集。"));
                ui.add_space(4.0);

                egui::Grid::new("hf_fields")
                    .num_columns(2)
                    .spacing([8.0, 6.0])
                    .show(ui, |ui| {
                        ui.label(tr("Dataset:", "数据集："));
                        let edit = egui::TextEdit::singleline(&mut self.dataset)
                            .hint_text(tr("org/name, hf://org/name, or the dataset page URL", "org/name、hf://org/name 或数据集页面 URL"))
                            .desired_width(f32::INFINITY)
                            .show(ui);
                        if self.just_opened {
                            edit.response.request_focus();
                        }
                        ui.end_row();
                    });

                // Token: the deployment's docker-secret default is used unless the user opts
                // into providing their own. Public datasets need no token at all.
                ui.add_space(2.0);
                ui.re_checkbox(&mut self.use_custom_token, tr("Use non-default token", "使用自带 token"));

                if self.use_custom_token {
                    egui::Grid::new("hf_token_field")
                        .num_columns(2)
                        .spacing([8.0, 6.0])
                        .show(ui, |ui| {
                            ui.label("Token：");
                            egui::TextEdit::singleline(&mut self.token)
                                .password(true)
                                .desired_width(f32::INFINITY)
                                .show(ui);
                            ui.end_row();
                        });
                }

                if let Some(err) = &config_error {
                    ui.warning_label(format!(
                        "加载服务器端默认配置（HF token、制品库凭证）失败：\
                         {err}\n文件：config.json — 各集将改为本地转换，而不是从制品库加载。",
                    ));
                }

                // Converted episodes are uploaded to a shared rrd artifacts store (a TOS bucket) by
                // default. Needs TOS credentials — without them the upload silently no-ops,
                // but the checkbox still communicates the intent.
                if let Some(artifacts_location) =
                    re_data_source::rrd_artifacts::parse_artifacts_url(&server_config.tos_rrd_artifacts_url)
                {
                    let mut upload = !self.artifact_upload_disabled;
                    ui.re_checkbox(&mut upload, "把转换出的 rrd 上传到制品库")
                        .on_hover_text(format!("{artifacts_location}"));
                    self.artifact_upload_disabled = !upload;
                }

                let parsed = parse_hf_dataset_input(&self.dataset);
                let can_open = parsed.is_some();

                if !self.dataset.is_empty() && parsed.is_none() {
                    ui.error_label(
                        "格式应为 org/name、hf://org/name 或 huggingface.co/datasets/… URL",
                    );
                } else if let Some((repo, file_path)) = &parsed {
                    if let Some(file_path) = file_path {
                        ui.label(format!("将从 hf://{repo} 加载单个文件 {file_path}。"));
                    } else {
                        ui.label(format!(
                            "将流式读取 hf://{repo}；各集（episode）会立即出现，点击某一集可以优先加载它。"
                        ));
                    }
                } else {
                    ui.label("请输入一个 Hugging Face 数据集。");
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let button_width = ui.tokens().modal_button_width;

                    let open_response = ui.add_enabled(
                        can_open,
                        egui::Button::new("打开").min_size(egui::vec2(button_width, 0.0)),
                    );
                    if open_response.clicked()
                        || can_open && ui.input(|i| i.key_pressed(egui::Key::Enter))
                    {
                        if let Some((repo, file_path)) = parsed {
                            let token = if self.use_custom_token {
                                self.token.trim().to_owned()
                            } else {
                                server_config.hf_token.clone()
                            };

                            command_sender.send_system(SystemCommand::LoadDataSource(
                                LogDataSource::HfDataset(HfDatasetSource {
                                    repo,
                                    file_path,
                                    token,
                                    rrd_artifacts: server_config
                                        .rrd_artifacts(!self.artifact_upload_disabled),
                                }),
                            ));
                        }
                        ui.close();
                    }

                    let cancel_response =
                        ui.add(egui::Button::new("取消").min_size(egui::vec2(button_width, 0.0)));
                    if cancel_response.clicked() {
                        ui.close();
                    }
                });
            },
        );

        self.just_opened = false;
    }
}
