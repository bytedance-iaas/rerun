use re_data_source::LogDataSource;
use re_i18n::tr;
use re_ui::UiExt as _;
use re_ui::modal::{ModalHandler, ModalWrapper};
use re_viewer_context::{CommandSender, SystemCommand, SystemCommandSender as _};

/// Dialog for opening a `LeRobot` dataset from the ByteDance HF cache — hot Hugging Face
/// datasets mirrored into the public-read `ai-infra` TOS bucket (see [`re_data_source::tos::hf_cache`]).
///
/// The one thing the user provides is the dataset name; the bucket is public-read, so no
/// credentials are involved. The catalog of cached datasets lives at
/// <https://huggingface-mirror.bytedance.net/all>.
#[derive(Default)]
pub struct OpenHfCacheModal {
    modal: ModalHandler,
    just_opened: bool,

    dataset_name: String,

    /// Inverted so the derived `Default` (false) means "upload converted rrds" — on by default.
    artifact_upload_disabled: bool,
}

impl OpenHfCacheModal {
    pub fn open(&mut self) {
        self.modal.open();
        self.just_opened = true;
        // The artifacts store (upload checkbox) comes from the resolved viewer config.
        crate::viewer_config::request();
    }

    /// Open with the dataset name pre-filled, e.g. from the welcome screen's recents list.
    pub fn open_prefilled(&mut self, dataset_name: &str) {
        self.dataset_name = dataset_name.to_owned();
        self.open();
    }

    pub fn ui(&mut self, ui: &egui::Ui, command_sender: &CommandSender) {
        let config = crate::viewer_config::get().unwrap_or_default();

        self.modal.ui(
            ui.ctx(),
            || ModalWrapper::new(tr("Open from Volcengine HF Cache", "从火山 HF 缓存打开")),
            |ui| {
                ui.strong(tr(
                    "Stream a LeRobot dataset from the ByteDance Hugging Face cache \
                     (the public ai-infra TOS bucket, Beijing). No credentials needed.",
                    "从字节的 Hugging Face 缓存（公开的 ai-infra TOS 桶，北京）流式读取 \
                     LeRobot 数据集。无需任何凭证。",
                ));
                ui.label(tr(
                    "Cached datasets are listed at huggingface-mirror.bytedance.net/all; \
                     missing ones can be requested there too.",
                    "已缓存的数据集见 huggingface-mirror.bytedance.net/all；\
                     没有的也可以在那里提交缓存请求。",
                ));
                ui.add_space(4.0);

                egui::Grid::new("hf_cache_fields")
                    .num_columns(2)
                    .spacing([8.0, 6.0])
                    .show(ui, |ui| {
                        ui.label(tr("Dataset name:", "数据集名："));
                        let name_edit = egui::TextEdit::singleline(&mut self.dataset_name)
                            .hint_text(tr("e.g. so101-pick-place", "如 so101-pick-place"))
                            .desired_width(f32::INFINITY)
                            .show(ui);
                        if self.just_opened {
                            name_edit.response.request_focus();
                        }
                        ui.end_row();
                    });

                // Converted episodes go to the shared rrd artifacts store when the
                // deployment has one (and credentials for it — the cache itself needs none).
                let rrd_artifacts = config.rrd_artifacts(!self.artifact_upload_disabled);
                if let Some(artifacts) = &rrd_artifacts {
                    let mut upload = !self.artifact_upload_disabled;
                    ui.re_checkbox(
                        &mut upload,
                        tr(
                            "Upload converted rrd to the artifacts store",
                            "把转换出的 rrd 上传到缓存桶",
                        ),
                    )
                    .on_hover_text(format!("{}", artifacts.location));
                    self.artifact_upload_disabled = !upload;
                }

                let name = self.dataset_name.trim().trim_matches('/');
                // One path segment: the cache lays datasets out flat, `dataset/<name>/…`.
                let name_ok = !name.is_empty() && !name.contains('/');

                if !self.dataset_name.trim().is_empty() && !name_ok {
                    ui.error_label(tr(
                        "A dataset name is a single segment without '/' — e.g. so101-pick-place.",
                        "数据集名是不含 '/' 的单段名字，如 so101-pick-place。",
                    ));
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
                        name_ok,
                        egui::Button::new(tr("Open", "打开"))
                            .min_size(egui::vec2(button_width, 0.0)),
                    );
                    if open_response.clicked()
                        || name_ok && ui.input(|i| i.key_pressed(egui::Key::Enter))
                    {
                        let source = re_data_source::tos::hf_cache::source(
                            re_data_source::tos::hf_cache::location(name),
                            config.rrd_artifacts(!self.artifact_upload_disabled),
                        );
                        command_sender.send_system(SystemCommand::LoadDataSource(
                            LogDataSource::TosDataset(source),
                        ));
                        ui.close();
                    }

                    let cancel_response = ui.add(
                        egui::Button::new(tr("Cancel", "取消"))
                            .min_size(egui::vec2(button_width, 0.0)),
                    );
                    if cancel_response.clicked() {
                        ui.close();
                    }
                });
            },
        );

        self.just_opened = false;
    }
}
