//! The in-app user guide.
//!
//! This fork's customer docs, embedded into the viewer so all three viewers (web,
//! local native, cloud native session) show them the same way — no network, no browser
//! and no deployment address needed. The markdown single-sources from
//! `docs/release/user-guide/`; the web deployment additionally serves it as HTML at
//! `/docs/` (see deploy/Dockerfile) for reading or sharing outside the viewer.

use std::sync::OnceLock;

struct Page {
    tab: &'static str,
    markdown: &'static str,
}

const PAGES: &[Page] = &[
    Page {
        tab: "Viewer",
        markdown: include_str!("../../../../../docs/release/user-guide/01-viewer.md"),
    },
    Page {
        tab: "Catalog server",
        markdown: include_str!("../../../../../docs/release/user-guide/02-catalog.md"),
    },
];

/// The screenshots the pages reference, embedded alongside them.
const IMAGES: &[(&str, &[u8])] = &[
    (
        "downloads-sdk-annotated.png",
        include_bytes!("../../../../../docs/release/user-guide/images/downloads-sdk-annotated.png"),
    ),
    (
        "viewer-add-menu-annotated.png",
        include_bytes!(
            "../../../../../docs/release/user-guide/images/viewer-add-menu-annotated.png"
        ),
    ),
    (
        "viewer-open-view-annotated.png",
        include_bytes!(
            "../../../../../docs/release/user-guide/images/viewer-open-view-annotated.png"
        ),
    ),
    (
        "viewer-panel-buttons-annotated.png",
        include_bytes!(
            "../../../../../docs/release/user-guide/images/viewer-panel-buttons-annotated.png"
        ),
    ),
    (
        "viewer-rrd-menu-annotated.png",
        include_bytes!(
            "../../../../../docs/release/user-guide/images/viewer-rrd-menu-annotated.png"
        ),
    ),
    (
        "viewer-tos-dialog-annotated.png",
        include_bytes!(
            "../../../../../docs/release/user-guide/images/viewer-tos-dialog-annotated.png"
        ),
    ),
    (
        "viewer-welcome-annotated.png",
        include_bytes!(
            "../../../../../docs/release/user-guide/images/viewer-welcome-annotated.png"
        ),
    ),
];

/// The markdown with image paths rewritten to the embedded bytes and the one
/// cross-document link (useless inside the viewer) turned into plain text.
fn processed_markdown(page: usize) -> &'static str {
    static CACHE: OnceLock<Vec<String>> = OnceLock::new();
    let pages = CACHE.get_or_init(|| {
        PAGES
            .iter()
            .map(|page| {
                page.markdown
                    .replace("](images/", "](bytes://user-guide/images/")
                    .replace("[01-viewer.md](01-viewer.md)", "Viewer 篇")
            })
            .collect()
    });
    &pages[page]
}

fn open_request_id() -> egui::Id {
    egui::Id::new("rerun_user_guide_open_request")
}

/// Ask the guide modal to open at `page` on the next frame. Callable from anywhere with
/// an [`egui::Context`] (the welcome-screen card uses this) — the modal instance lives
/// in the app state and consumes the request in [`UserGuideModal::ui`].
pub fn request_open(ctx: &egui::Context, page: usize) {
    ctx.data_mut(|data| data.insert_temp(open_request_id(), page));
}

/// The guide window: tab per page, markdown rendered in-app. A resizable, movable
/// [`egui::Window`] (not a modal), so it can sit next to the UI while following along.
#[derive(Default)]
pub struct UserGuideModal {
    open: bool,
    selected: usize,
    images_registered: bool,
    commonmark_cache: egui_commonmark::CommonMarkCache,
}

impl UserGuideModal {
    pub fn ui(&mut self, ui: &egui::Ui) {
        let requested = ui.ctx().data_mut(|data| {
            let requested = data.get_temp::<usize>(open_request_id());
            if requested.is_some() {
                data.remove::<usize>(open_request_id());
            }
            requested
        });
        if let Some(page) = requested {
            self.selected = page.min(PAGES.len() - 1);
            self.open = true;

            if !self.images_registered {
                for (name, bytes) in IMAGES {
                    ui.ctx()
                        .include_bytes(format!("bytes://user-guide/images/{name}"), *bytes);
                }
                self.images_registered = true;
            }
        }
        if !self.open {
            return;
        }

        let selected = &mut self.selected;
        let commonmark_cache = &mut self.commonmark_cache;
        let mut open = self.open;
        let default_size = egui::vec2(840.0, 620.0);
        egui::Window::new("User guide")
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_size(default_size)
            .min_size(egui::vec2(420.0, 320.0))
            .default_pos(ui.ctx().content_rect().center() - 0.5 * default_size)
            .show(ui.ctx(), |ui| {
                ui.horizontal(|ui| {
                    for (index, page) in PAGES.iter().enumerate() {
                        ui.selectable_value(selected, index, page.tab);
                    }
                });
                ui.separator();
                egui::ScrollArea::vertical()
                    .id_salt(*selected) // separate scroll position per page
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        // Cap images to the window width, so every section wraps to the
                        // same line length no matter how wide the screenshots are.
                        let max_image_width = (ui.available_width() - 12.0).max(100.0) as usize;
                        egui_commonmark::CommonMarkViewer::new()
                            .max_image_width(Some(max_image_width))
                            .show(ui, commonmark_cache, processed_markdown(*selected));
                    });
            });
        self.open = open;
    }
}
