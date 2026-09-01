use re_i18n::{tr, trf};
use re_data_source::rrd_artifacts::ArtifactDeletionRequest;
use re_ui::modal::{ModalHandler, ModalWrapper};

/// Confirmation dialog for deleting rrd artifacts from the store.
///
/// Deletion requests are queued by the context-menu actions
/// (`rrd_artifacts::request_deletion`); this modal picks them up once per frame,
/// asks the user, and only a confirmed request actually deletes anything.
#[derive(Default)]
pub struct DeleteArtifactsModal {
    modal: ModalHandler,
    request: Option<ArtifactDeletionRequest>,
}

impl DeleteArtifactsModal {
    pub fn ui(&mut self, ui: &egui::Ui) {
        if let Some(request) = re_data_source::rrd_artifacts::take_deletion_request() {
            self.request = Some(request);
            self.modal.open();
        }
        let Some(request) = self.request.clone() else {
            return;
        };

        self.modal.ui(
            ui.ctx(),
            || ModalWrapper::new(tr("Delete rrd artifacts?", "删除 rrd 制品？")),
            |ui| {
                if request.episode.is_some() {
                    ui.label(
                        "这会从制品库中删除这一集转换出的 rrd：",
                    );
                } else {
                    let count = re_data_source::lerobot_remote::dataset_artifact_count(
                        &request.application_id,
                    );
                    ui.label(trf!(
                        "This deletes ALL converted rrds of this dataset ({count} known) \
                         from the artifacts store:",
                        "这会从制品库中删除这个数据集转换出的全部 rrd（已知 {count} 个）：",
                    ));
                }
                ui.add_space(4.0);
                ui.monospace(&request.target_url);
                ui.add_space(4.0);
                ui.label(
                    "查看不受影响：受影响的集下次打开时会从源数据重新转换\
                    （若开启了上传，还会重新上传）。此操作无法撤销。",
                );
                ui.add_space(12.0);

                ui.horizontal(|ui| {
                    let delete = ui.add(egui::Button::new(
                        egui::RichText::new("删除").color(ui.visuals().error_fg_color),
                    ));
                    if delete.clicked() {
                        match re_data_source::lerobot_remote::dataset_artifacts_config(
                            &request.application_id,
                        ) {
                            Some(config) => {
                                re_data_source::rrd_artifacts::spawn_deletion(
                                    config,
                                    request.clone(),
                                );
                            }
                            None => {
                                re_log::warn!(
                                    "{}",
                                    trf!(
                                        "Cannot delete rrd artifacts: the dataset's stream is no \
                                         longer active\nDataset: {}",
                                        "无法删除 rrd 制品：这个数据集的流式读取已不再活跃\n数据集：{}",
                                        request.dataset_url
                                    )
                                );
                            }
                        }
                        self.request = None;
                        ui.close();
                    }
                    if ui.button("取消").clicked() {
                        self.request = None;
                        ui.close();
                    }
                });
            },
        );
    }
}
