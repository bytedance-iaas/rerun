use re_i18n::tr;
use re_ui::DesignTokens;

pub(super) const DOCS_URL: &str = "https://www.rerun.io/docs";

pub(super) fn welcome_screen_title() -> &'static str {
    tr("Rerun: The data layer for VePAI", "Rerun: VePAI的数据底座")
}

pub(super) fn welcome_screen_bullet_text() -> [&'static str; 3] {
    [
        tr(
            "Log multi-rate, multimodal data with the Rerun SDK in C++, Python, or Rust",
            "用 C++、Python 或 Rust 的 Rerun SDK 记录多频率、多模态数据",
        ),
        tr(
            "Visualize and explore live or recorded data across the pipeline",
            "可视化并探索管线各环节的实时或已录制数据",
        ),
        tr(
            "Query with dataframes or SQL, and stream directly to training",
            "用 dataframe 或 SQL 查询，并直接流式接入训练",
        ),
    ]
}

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
                egui::RichText::new(welcome_screen_title())
                    .strong()
                    .line_height(Some(line_height))
                    .text_style(style),
            )
            .wrap(),
        );
    });
}
