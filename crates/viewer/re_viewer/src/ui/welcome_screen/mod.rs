mod example_section;
mod intro_section;
mod loading_data_ui;
mod no_data_ui;
mod recent_section;
mod welcome_section;

pub use recent_section::RecentAction;

use std::sync::Arc;

use example_section::{ExampleSection, MIN_COLUMN_WIDTH};
use re_log_channel::LogSource;

use crate::app_state::WelcomeScreenState;

pub use intro_section::{CloudState, LoginState};

/// The uniform section heading of the welcome screen ("Volcengine enhancements",
/// "About the original Rerun", "Recently opened datasets"): same size, underlined.
pub(super) fn section_heading_ui(ui: &mut egui::Ui, text: &str) {
    ui.add(egui::Label::new(
        egui::RichText::new(text).strong().size(18.0).underline(),
    ));
    ui.add_space(10.0);
}
use re_viewer_context::AppContext;

#[derive(Default)]
pub struct WelcomeScreen {
    example_page: ExampleSection,
}

impl WelcomeScreen {
    pub fn set_examples_manifest_url(&mut self, egui_ctx: &egui::Context, url: String) {
        self.example_page.set_manifest_url(egui_ctx, url);
    }

    /// Welcome screen shown in place of the viewport when no data is loaded.
    ///
    /// Returns what the user did in the "recently opened" section, if anything.
    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &AppContext<'_>,
        welcome_screen_state: &WelcomeScreenState,
        log_sources: &[Arc<LogSource>],
        login_state: &CloudState,
        recent_datasets: &[crate::recent_datasets::RecentDataset],
    ) -> Option<RecentAction> {
        if welcome_screen_state.opacity <= 0.0 {
            return None;
        }

        // This is needed otherwise `example_page_ui` bleeds by a few pixels over the timeline panel
        // TODO(ab): figure out why that happens
        ui.set_clip_rect(ui.available_rect_before_wrap());

        let horizontal_scroll = ui.available_width() < 40.0 * 2.0 + MIN_COLUMN_WIDTH;

        let mut recent_action = None;
        let response = egui::ScrollArea::new([horizontal_scroll, true])
            .id_salt("welcome_screen_page")
            .auto_shrink([false, false])
            .show(ui, |ui| {
                egui::Frame {
                    inner_margin: egui::Margin {
                        left: 40,
                        right: 40,
                        top: 50,
                        bottom: 8,
                    },
                    ..Default::default()
                }
                .show(ui, |ui| {
                    if welcome_screen_state.hide_examples {
                        // No cards on this minimal screen, so the recents lead.
                        recent_action = recent_section::recent_datasets_ui(ui, recent_datasets);

                        if let Some(loading_text) =
                            loading_data_ui::loading_text_for_data_sources(log_sources)
                        {
                            loading_data_ui::loading_data_ui(ui, &loading_text);
                        } else {
                            no_data_ui::no_data_ui(ui);
                        }
                    } else {
                        // The full welcome screen draws the recents below the feature cards.
                        recent_action =
                            self.example_page.ui(ui, ctx, login_state, recent_datasets);
                    }
                });
            });

        if welcome_screen_state.opacity < 1.0 {
            let cover_opacity = 1.0 - welcome_screen_state.opacity;
            let fill_color = ui.visuals().panel_fill.gamma_multiply(cover_opacity);
            ui.painter()
                .rect_filled(response.inner_rect, 0.0, fill_color);
        }

        recent_action
    }
}
