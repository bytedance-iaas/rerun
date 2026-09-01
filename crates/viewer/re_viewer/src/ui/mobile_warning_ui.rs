use re_i18n::tr;
use re_ui::{ContextExt as _, UiExt as _, WindowFrameConfig};

pub fn mobile_warning_ui(ui: &mut egui::Ui, custom_window_decorations: bool) {
    // We have not yet optimized the UI experience for mobile. Show a warning banner
    // with a link to the tracking issue.

    if ui.os() == egui::os::OperatingSystem::IOS || ui.os() == egui::os::OperatingSystem::Android {
        let window_frame = if custom_window_decorations {
            WindowFrameConfig::custom(ui.ctx())
        } else {
            WindowFrameConfig::Native
        };
        let frame = egui::Frame {
            fill: ui.visuals().panel_fill,
            ..ui.tokens().bottom_panel_frame(window_frame)
        };

        egui::Panel::bottom("warning_panel")
            .resizable(false)
            .frame(frame)
            .show(ui, |ui| {
                ui.centered_and_justified(|ui| {
                    let text = ui.ctx().warning_text(tr(
                        "Mobile OSes are not yet supported. Click for details.",
                        "暂不支持移动操作系统。点击查看详情。",
                    ));
                    ui.hyperlink_to(text, "https://github.com/rerun-io/rerun/issues/1672");
                });
            });
    }
}
