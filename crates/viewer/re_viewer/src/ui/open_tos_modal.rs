use re_i18n::{tr, trf};
use std::sync::Arc;

use parking_lot::Mutex;
use re_data_source::LogDataSource;
use re_data_source::tos::{TosCredentials, TosDatasetSource, TosLocation};
use re_ui::UiExt as _;
use re_ui::modal::{ModalHandler, ModalWrapper};
use re_viewer_context::{CommandSender, SystemCommand, SystemCommandSender as _};

/// The deployment's TOS connection settings, served at `/config.json`.
///
/// Endpoint and credentials (injected as docker secrets) come from here by default;
/// the user can opt into entering their own AK/SK in the dialog instead (e.g. for a
/// bucket the deployment's credentials can't read).
#[derive(Clone, serde::Deserialize)]
#[serde(default)]
struct ServerTosConfig {
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

impl Default for ServerTosConfig {
    fn default() -> Self {
        Self {
            tos_endpoint: String::new(),
            tos_access_key: String::new(),
            tos_secret_key: String::new(),
            tos_rrd_artifacts_url: String::new(),
            tos_rrd_artifacts_region: String::new(),
            rrd_artifacts_prefetch: 0,
        }
    }
}

impl ServerTosConfig {
    fn has_credentials(&self) -> bool {
        !self.tos_access_key.is_empty() && !self.tos_secret_key.is_empty()
    }
}

/// Known TOS regions, for the Region dropdown — the list and order mirror the
/// volcengine console's create-bucket region picker (2026-08-18). Each entry pairs the
/// code sent to TOS with its English and Chinese display names; the UI shows the
/// localized name, the code stays internal.
const TOS_REGIONS: &[(&str, &str, &str)] = &[
    ("cn-beijing", "cn-beijing (Beijing)", "华北2（北京）"),
    (
        "ap-southeast-1",
        "ap-southeast-1 (Johor)",
        "亚太东南（柔佛）",
    ),
    (
        "ap-southeast-3",
        "ap-southeast-3 (Jakarta)",
        "亚太东南（雅加达）",
    ),
    ("cn-guangzhou", "cn-guangzhou (Guangzhou)", "华南1（广州）"),
    ("cn-hongkong", "cn-hongkong (Hong Kong)", "中国香港"),
    ("cn-shanghai", "cn-shanghai (Shanghai)", "华东2（上海）"),
];

/// The localized display name for a region code, falling back to the code itself
/// for anything not in [`TOS_REGIONS`].
fn tos_region_label(code: &str) -> &str {
    TOS_REGIONS
        .iter()
        .find(|(c, _, _)| *c == code)
        .map_or(code, |(_, en, zh)| tr(en, zh))
}

/// How long after the last keystroke the malformed-URL error may turn red.
const URL_ERROR_DELAY: std::time::Duration = std::time::Duration::from_secs(1);

/// Dialog for opening a `LeRobot` dataset — or a data file such as an MCAP — stored in
/// Volcengine TOS.
#[derive(Default)]
pub struct OpenTosModal {
    modal: ModalHandler,
    just_opened: bool,

    url: String,

    /// The bucket's region. Defaults to the deployment endpoint's region; the endpoint is
    /// derived from this (`re_data_source::tos::endpoint_for_region`), so this is the only
    /// connection field the user ever touches.
    region: String,

    /// Show the AK/SK inputs and use them instead of the deployment's credentials —
    /// the TOS counterpart of the HF dialog's "Use non-default token".
    use_custom_credentials: bool,
    access_key: String,
    secret_key: String,

    /// Inverted so the derived `Default` (false) means "upload converted rrds" — on by default.
    artifact_upload_disabled: bool,

    /// When the URL field was last edited (`ui.input(|i| i.time)`). The malformed-URL
    /// error stays a neutral hint until the user pauses or leaves the field — flashing
    /// red at the first typed character punishes typing that isn't finished yet (#48).
    url_last_edited: Option<f64>,

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
                    re_log::warn!(
                        "{}",
                        trf!(
                            "Failed to load server TOS defaults: {err}\nFile: config.json",
                            "加载服务器端 TOS 默认配置失败：{err}\n文件：config.json"
                        )
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
            env_override(&mut parsed.tos_access_key, "TOS_ACCESS_KEY");
            env_override(&mut parsed.tos_secret_key, "TOS_SECRET_KEY");
            env_override(&mut parsed.tos_rrd_artifacts_url, "TOS_RRD_ARTIFACTS_URL");
            env_override(
                &mut parsed.tos_rrd_artifacts_region,
                "TOS_RRD_ARTIFACTS_REGION",
            );
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
            || ModalWrapper::new(tr("Open from Volcengine TOS", "从火山引擎 TOS 打开")),
            |ui| {
                ui.strong(tr(
                    "Stream a LeRobot dataset — or data files like MCAP and rrd — from a TOS bucket.",
                    "从 TOS 桶流式读取 LeRobot 数据集，或 MCAP、rrd 等数据文件。",
                ));
                ui.add_space(4.0);

                let url_response = egui::Grid::new("tos_fields")
                    .num_columns(2)
                    .spacing([8.0, 6.0])
                    .show(ui, |ui| {
                        ui.label(tr("Dataset URL:", "数据集 URL："));
                        let url_edit = egui::TextEdit::singleline(&mut self.url)
                            .hint_text(tr(
                                "tos://bucket/dataset/ or tos://bucket/path/log.mcap",
                                "tos://bucket/数据集目录/ 或 tos://bucket/path/log.mcap",
                            ))
                            .desired_width(f32::INFINITY)
                            .show(ui);
                        if self.just_opened {
                            url_edit.response.request_focus();
                        }
                        ui.end_row();

                        // Region: a dropdown of known regions shown by their Chinese name.
                        // The code (e.g. "cn-beijing") is what we send to TOS, and the endpoint
                        // is derived from it, so there is nothing else to fill.
                        ui.label(tr("Region:", "地区："));
                        let selected_label = if self.region.trim().is_empty() {
                            tr("Select a region", "请选择地区")
                        } else {
                            tos_region_label(&self.region)
                        };
                        egui::ComboBox::from_id_salt("tos_region")
                            .selected_text(selected_label)
                            .width(ui.available_width())
                            .show_ui(ui, |ui| {
                                for (code, en, zh) in TOS_REGIONS {
                                    ui.selectable_value(
                                        &mut self.region,
                                        (*code).to_owned(),
                                        tr(en, zh),
                                    );
                                }
                            });
                        ui.end_row();

                        url_edit.response
                    })
                    .inner;

                let now = ui.input(|i| i.time);
                if url_response.changed() {
                    self.url_last_edited = Some(now);
                }

                // Credentials: the deployment's (docker secrets on the web, config.json
                // natively) are used unless the user opts into entering their own.
                ui.add_space(2.0);
                ui.re_checkbox(&mut self.use_custom_credentials, tr("Use non-default credentials", "使用自带 AK/SK"));

                if self.use_custom_credentials {
                    egui::Grid::new("tos_credentials_fields")
                        .num_columns(2)
                        .spacing([8.0, 6.0])
                        .show(ui, |ui| {
                            ui.label("Access key：");
                            egui::TextEdit::singleline(&mut self.access_key)
                                .hint_text("AK…")
                                .desired_width(f32::INFINITY)
                                .show(ui);
                            ui.end_row();

                            ui.label("Secret key：");
                            egui::TextEdit::singleline(&mut self.secret_key)
                                .password(true)
                                .desired_width(f32::INFINITY)
                                .show(ui);
                            ui.end_row();
                        });
                }

                // In the browser the requests go out from the user's machine, so internal
                // endpoints (only routable inside the Volcengine network) won't work.
                #[cfg(target_arch = "wasm32")]
                if resolved_endpoint.contains(".ivolces.com") {
                    ui.warning_label(tr(
                        "This is an internal endpoint (.ivolces.com), only reachable from \
                         inside the Volcengine network. In a browser you most likely need \
                         the public endpoint (.volces.com).",
                        "这是内网 endpoint（.ivolces.com），只能在火山引擎内网访问。\
                         在浏览器里你多半需要公网 endpoint（.volces.com）。"
                    ));
                }

                if let Some(err) = &config_error {
                    ui.warning_label(trf!(
                        "Failed to load the deployment's TOS settings (endpoint, credentials): \
                         {err}\nFile: config.json — datasets cannot be opened until this is fixed.",
                        "加载部署的 TOS 设置（endpoint、凭证）失败：\
                         {err}\n文件：config.json — 不修复就无法打开数据集。",
                    ));
                }

                // Converted episodes are uploaded to a shared rrd artifacts store by default, so the next
                // open (by anyone) skips the conversion. Deployment-disabled → no checkbox.
                if let Some(artifacts_location) =
                    re_data_source::rrd_artifacts::parse_artifacts_url(&server_config.tos_rrd_artifacts_url)
                {
                    let mut upload = !self.artifact_upload_disabled;
                    ui.re_checkbox(&mut upload, tr("Upload converted rrd to the artifacts store", "把转换出的 rrd 上传到缓存桶"))
                        .on_hover_text(format!("{artifacts_location}"));
                    self.artifact_upload_disabled = !upload;
                }

                // The endpoint is derived from the chosen region; the signing region in
                // turn is derived from the endpoint (see `TosCredentials::region`).
                let have_credentials = if self.use_custom_credentials {
                    !self.access_key.trim().is_empty() && !self.secret_key.trim().is_empty()
                } else {
                    server_config.has_credentials()
                };
                let connection_ok = !self.region.trim().is_empty() && have_credentials;

                let location = TosLocation::parse(&self.url);
                let can_open = location.is_some() && connection_ok;

                if !self.url.is_empty() && location.is_none() {
                    // Only turn red once the user pauses or leaves the field; mid-typing,
                    // a neutral hint carries the same information without the reprimand.
                    let still_typing = url_response.has_focus()
                        && self
                            .url_last_edited
                            .is_some_and(|t| now - t < URL_ERROR_DELAY.as_secs_f64());
                    if still_typing {
                        ui.label(tr(
                            "The URL should look like tos://bucket/prefix/ (or point at a file, e.g. tos://bucket/path/log.mcap)",
                            "URL 应形如 tos://bucket/prefix/（也可以指向单个文件，如 tos://bucket/path/log.mcap）",
                        ));
                        // Wake up when the pause elapses, so the error can appear
                        // without waiting for the next keystroke or mouse move.
                        ui.ctx().request_repaint_after(URL_ERROR_DELAY);
                    } else {
                        ui.error_label(tr(
                            "The URL should look like tos://bucket/prefix/ (or point at a file, e.g. tos://bucket/path/log.mcap)",
                            "URL 应形如 tos://bucket/prefix/（也可以指向单个文件，如 tos://bucket/path/log.mcap）",
                        ));
                    }
                } else if !connection_ok {
                    ui.label(if !config_resolved {
                        tr(
                            "Loading the deployment's TOS settings…",
                            "正在加载部署的 TOS 设置…",
                        )
                    } else if self.region.trim().is_empty() {
                        tr("Region is required.", "请选择地区。")
                    } else if self.use_custom_credentials {
                        tr(
                            "Enter the access key and secret key.",
                            "请输入 access key 和 secret key。",
                        )
                    } else {
                        tr(
                            "This deployment has no TOS credentials configured (config.json) — \
                             check \"Use non-default credentials\" to enter your own.",
                            "这个部署没有配置 TOS 凭证（config.json）— \
                             勾选“使用自带 AK/SK”后输入你自己的。",
                        )
                    });
                } else {
                    ui.label(tr(
                        "Episodes appear immediately and stream in one by one; \
                         click an episode to load it first.",
                        "各集（episode）会立即出现在列表里并逐个流式加载；\
                         点击某一集可以优先加载它。",
                    ));
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let button_width = ui.tokens().modal_button_width;

                    let open_response = ui.add_enabled(
                        can_open,
                        egui::Button::new(tr("Open", "打开")).min_size(egui::vec2(button_width, 0.0)),
                    );
                    if open_response.clicked()
                        || can_open && ui.input(|i| i.key_pressed(egui::Key::Enter))
                    {
                        if let Some(location) = location {
                            let credentials = if self.use_custom_credentials {
                                TosCredentials {
                                    endpoint: resolved_endpoint.clone(),
                                    access_key: self.access_key.trim().to_owned(),
                                    secret_key: self.secret_key.trim().to_owned(),
                                }
                            } else {
                                TosCredentials {
                                    endpoint: resolved_endpoint.clone(),
                                    access_key: server_config.tos_access_key.clone(),
                                    secret_key: server_config.tos_secret_key.clone(),
                                }
                            };
                            // The artifacts bucket belongs to the deployment, so prefer its
                            // credentials even when the dataset is opened with custom ones
                            // (which likely can't access the artifacts bucket at all).
                            // Same for the endpoint: the artifacts bucket has its own
                            // configured region (empty = the deployment's), independent of
                            // the dataset region the user picked — `resolved_endpoint`
                            // (the dataset's) would answer NoSuchBucket.
                            let artifact_endpoint = re_data_source::tos::endpoint_for_region(
                                &server_config.tos_rrd_artifacts_region,
                                if server_config.tos_endpoint.is_empty() {
                                    &resolved_endpoint
                                } else {
                                    &server_config.tos_endpoint
                                },
                            );
                            let artifact_credentials = if server_config.has_credentials() {
                                TosCredentials {
                                    endpoint: artifact_endpoint,
                                    access_key: server_config.tos_access_key.clone(),
                                    secret_key: server_config.tos_secret_key.clone(),
                                }
                            } else {
                                TosCredentials {
                                    endpoint: artifact_endpoint,
                                    ..credentials.clone()
                                }
                            };
                            let rrd_artifacts = re_data_source::rrd_artifacts::parse_artifacts_url(
                                &server_config.tos_rrd_artifacts_url,
                            )
                            .map(|artifacts_location| {
                                re_data_source::rrd_artifacts::RrdArtifactsConfig {
                                    location: artifacts_location,
                                    credentials: artifact_credentials,
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
                        ui.add(egui::Button::new(tr("Cancel", "取消")).min_size(egui::vec2(button_width, 0.0)));
                    if cancel_response.clicked() {
                        ui.close();
                    }
                });
            },
        );

        self.just_opened = false;
    }
}
