use re_i18n::tr;
use itertools::Itertools as _;
use re_entity_db::EntityDb;
use re_log_types::ApplicationId;
use re_sdk_types::archetypes::RecordingInfo;
use re_sdk_types::components::Timestamp;
use re_viewer_context::{AppContext, UiLayout};

use crate::item_ui::entity_db_button_ui;

impl crate::AppUi for ApplicationId {
    fn app_ui(&self, ctx: &AppContext<'_>, ui: &mut egui::Ui, ui_layout: UiLayout) {
        egui::Grid::new("application_id")
            .num_columns(2)
            .show(ui, |ui| {
                ui.label(tr("Application ID", "应用 ID"));

                let mut label = self.to_string();
                if ctx
                    .active_store_context
                    .is_some_and(|sc| self == sc.application_id())
                {
                    label.push_str(tr(" (active)", "（当前活跃）"));
                }
                UiLayout::List.label(ui, label);
                ui.end_row();
            });

        // Find all recordings with this app id
        let recordings: Vec<&EntityDb> = ctx
            .store_bundle()
            .recordings()
            .filter(|db| db.application_id() == self)
            .sorted_by_key(|entity_db| {
                entity_db.recording_info_property::<Timestamp>(
                    RecordingInfo::descriptor_start_time().component,
                )
            })
            .collect();

        match ui_layout {
            UiLayout::List | UiLayout::Inline => {
                // Too little space for anything else
            }
            UiLayout::Tooltip => {
                if recordings.len() == 1 {
                    ui.label(tr("There is 1 loaded recording for this app.", "该应用已加载 1 个录制文件。"));
                } else {
                    ui.label(format!(
                        "该应用已加载 {} 个录制文件。",
                        re_format::format_uint(recordings.len()),
                    ));
                }
            }
            UiLayout::SelectionPanel => {
                if !recordings.is_empty() {
                    ui.scope(|ui| {
                        ui.spacing_mut().item_spacing.y = 0.0;

                        ui.add_space(8.0);
                        ui.strong(tr("Loaded recordings for this app", "该应用已加载的录制文件"));
                        for entity_db in recordings {
                            entity_db_button_ui(ctx, entity_db, ui, ui_layout, true);
                        }
                    });
                }
            }
        }
    }
}
