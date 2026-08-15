//! "Recently opened" section of the welcome screen.
//!
//! Converted data is RAM-only, so a restarted viewer is blank; this section shows which remote
//! datasets were open before (with their metadata) and re-opens one with a click — by bringing
//! up the matching "Open from …" dialog pre-filled, so credential handling stays in one place.

use re_ui::{DesignTokens, UiExt as _, icons};

use crate::recent_datasets::{RecentDataset, RecentKind, now_unix, relative_time_label};

/// What the user did in the section this frame.
pub enum RecentAction {
    /// Re-open this entry (index into the recents list).
    Open(usize),

    /// Forget this entry.
    Remove(usize),
}

/// Lists the recently opened remote datasets; returns what to do about them.
pub fn recent_datasets_ui(ui: &mut egui::Ui, recents: &[RecentDataset]) -> Option<RecentAction> {
    if recents.is_empty() {
        return None;
    }

    let mut action = None;
    let now = now_unix();

    ui.add(egui::Label::new(
        egui::RichText::new("Recently opened")
            .strong()
            .line_height(Some(32.0))
            .text_style(DesignTokens::welcome_screen_example_title()),
    ));
    ui.add(egui::Label::new(
        egui::RichText::new(
            "Remote datasets stream in on demand and are not kept after a restart — \
             click one to open it again.",
        )
        .color(ui.visuals().weak_text_color())
        .text_style(DesignTokens::welcome_screen_body()),
    ));
    ui.add_space(8.0);

    for (index, recent) in recents.iter().enumerate() {
        let source_label = match recent.kind {
            RecentKind::Tos => "TOS",
            RecentKind::Hf => "Hugging Face",
        };
        let mut meta = format!(
            "{source_label} · {}",
            relative_time_label(recent.last_opened_unix, now)
        );
        if let Some(count) = recent.item_count {
            use std::fmt::Write as _;
            write!(meta, " · {count} items").ok();
        }

        ui.horizontal(|ui| {
            let open_response = ui
                .selectable_label_with_icon(
                    &icons::DATASET,
                    format!("{}  ·  {}", recent.display_name(), recent.url),
                    false,
                    re_ui::LabelStyle::Normal,
                )
                .on_hover_text(&meta);
            if open_response.clicked() {
                action = Some(RecentAction::Open(index));
            }

            ui.add(egui::Label::new(
                egui::RichText::new(meta.clone())
                    .color(ui.visuals().weak_text_color())
                    .text_style(DesignTokens::welcome_screen_body()),
            ));

            // TOS datasets can be sent to the Daft curation console. Only LeRobot v2/v3
            // makes it through the TOS loader, and `item_count` is set the first time a
            // stream reports progress — so "has an item count" doubles as "format confirmed",
            // and the button stays greyed out until then.
            if matches!(recent.kind, RecentKind::Tos)
                && let Some(url) = re_viewer_context::daft_link::diagnose_url(&recent.url)
            {
                let confirmed = recent.item_count.is_some();
                let response = ui
                    .add_enabled(confirmed, egui::Button::new("Diagnose").small())
                    .on_hover_text(format!("Run data curation on this dataset in Daft\n{url}"))
                    .on_disabled_hover_text(
                        "Dataset format not confirmed yet — open the dataset once first \
                         (only LeRobot v2/v3 can be diagnosed)",
                    );
                if response.clicked() {
                    ui.open_url(egui::OpenUrl::new_tab(url));
                }
            }

            if ui
                .small_icon_button(&icons::CLOSE, "Remove from this list")
                .clicked()
            {
                action = Some(RecentAction::Remove(index));
            }
        });
        ui.add_space(2.0);
    }

    ui.add_space(24.0);

    action
}
