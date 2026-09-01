use re_ui::DesignTokens;

pub(super) const DOCS_URL: &str = "https://www.rerun.io/docs";
pub(super) const WELCOME_SCREEN_TITLE: &str = "VePAI之数据底座";
pub(super) const WELCOME_SCREEN_BULLET_TEXT: &[&str] = &[
    "用 C++、Python 或 Rust 的 Rerun SDK 记录多频率、多模态数据",
    "可视化并探索管线各环节的实时或已录制数据",
    "用 dataframe 或 SQL 查询，并直接流式接入训练",
];

/// Show the welcome section.
pub(super) fn welcome_section_ui(ui: &mut egui::Ui) {
    ui.vertical(|ui| {
        let (style, line_height) = if ui.available_width() > 400.0 {
            (DesignTokens::welcome_screen_h1(), 50.0)
        } else {
            (DesignTokens::welcome_screen_h2(), 36.0)
        };

        ui.add(
            egui::Label::new(
                egui::RichText::new(WELCOME_SCREEN_TITLE)
                    .strong()
                    .line_height(Some(line_height))
                    .text_style(style),
            )
            .wrap(),
        );
    });
}
