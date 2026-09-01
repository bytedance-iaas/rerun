mod layout;
mod model;
mod paint;

use self::layout::Layout;
use self::model::{Model, ModelFilter, build_transform_cache_model};
use self::paint::{draw_transform_cache_contents, scene_legend_ui};

use re_chunk_store::LatestAtQuery;
use re_ui::UiExt as _;
use re_viewer_context::external::re_entity_db::EntityDb;
use re_viewer_context::external::re_tf::transform_cache_snapshot;
use re_viewer_context::{StorageContext, TransformDatabaseStoreCache};

const NODE_SIZE: egui::Vec2 = egui::Vec2 { x: 220.0, y: 52.0 };

// Zooming in too far can cause blurred text, see: https://github.com/emilk/egui/issues/5691
const SCENE_MAX_ZOOM: f32 = 1.0;
const SCENE_MIN_ZOOM: f32 = 0.05;

/// Compared to [`transform_cache_snapshot::FrameFilter`],
/// this adds a category for unlinked named frames.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum FrameVisibilityFilter {
    Implicit,
    #[default]
    Named,
    Unlinked,
    All,
}

/// Orientation of the rendered transform-cache graph.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum LayoutDirection {
    Horizontal,
    #[default]
    Vertical,
}

/// Persistent UI state for the transform-cache dev panel.
#[derive(Default)]
pub(super) struct TransformCacheUiState {
    frame_filter: FrameVisibilityFilter,
    edge_filter: transform_cache_snapshot::EdgeFilter,
    layout_direction: LayoutDirection,
    scene_rect: Option<egui::Rect>,
    user_interacted: bool,
}

/// Draws the transform-cache dev-panel tab for the active recording and latest-at time.
pub(super) fn ui(
    ui: &mut egui::Ui,
    recording: &EntityDb,
    storage_context: &StorageContext<'_>,
    query: Option<LatestAtQuery>,
    state: &mut TransformCacheUiState,
) {
    re_tracing::profile_function!();

    let Some(query) = query else {
        ui.warning_label("变换缓存没有选中活动的时间轴。");
        return;
    };

    let Some(caches) = storage_context.hub.store_caches(recording.store_id()) else {
        ui.label("这个录制文件还没有可用的变换缓存。");
        return;
    };

    let (model, settings_changed) = ui
        .allocate_ui_with_layout(
            egui::vec2(ui.available_width(), ui.spacing().interact_size.y),
            egui::Layout::left_to_right(egui::Align::TOP),
            |ui| {
                let previous_settings = (
                    state.frame_filter,
                    state.edge_filter,
                    state.layout_direction,
                );

                frame_filter_ui(ui, state);
                ui.separator();

                edge_filter_ui(ui, state);
                ui.separator();

                layout_direction_ui(ui, state);
                ui.separator();

                let filter = ModelFilter {
                    frame_filter: state.frame_filter,
                    edge_filter: state.edge_filter,
                };
                let current_model = caches.memoizer(|cache: &mut TransformDatabaseStoreCache| {
                    build_transform_cache_model(recording, cache, &query, filter)
                });

                if current_model.snapshot.frames.is_empty() {
                    ui.warning_label("没有符合当前筛选条件的变换坐标系。");
                } else {
                    ui.info_label(format!(
                        "{} 棵树、{} 个坐标系、{} 个变换",
                        current_model.num_trees(),
                        current_model.snapshot.frames.len(),
                        current_model.snapshot.edges.len()
                    ));
                }
                ui.separator();

                // Center the recording name label horizontally to align it with the widgets' text.
                ui.horizontal_centered(|ui| {
                    let store_id = recording.store_id();
                    ui.label(format!(
                        "{} ({})",
                        store_id.application_id(),
                        store_id.recording_id()
                    ));

                    if current_model.any_missing_chunks {
                        ui.warning_label("有部分 chunk 缺失");
                    }
                });

                let settings_changed = previous_settings
                    != (
                        state.frame_filter,
                        state.edge_filter,
                        state.layout_direction,
                    );
                (current_model, settings_changed)
            },
        )
        .inner;

    draw_transform_cache(ui, &model, state, settings_changed);
}

/// Draws the implicit/named frame filter controls.
fn frame_filter_ui(ui: &mut egui::Ui, state: &mut TransformCacheUiState) {
    ui.selectable_toggle(|ui| {
        ui.selectable_value(
            &mut state.frame_filter,
            FrameVisibilityFilter::Implicit,
            "隐式",
        )
        .on_hover_text("只显示由实体路径派生出的 tf# 坐标系");
        ui.selectable_value(
            &mut state.frame_filter,
            FrameVisibilityFilter::Named,
            "具名",
        )
        .on_hover_text("显示带变换的、显式命名的坐标系");
        ui.selectable_value(
            &mut state.frame_filter,
            FrameVisibilityFilter::Unlinked,
            "未关联",
        )
        .on_hover_text("显示没有变换的具名坐标系");
        ui.selectable_value(&mut state.frame_filter, FrameVisibilityFilter::All, "全部")
            .on_hover_text("显示隐式、具名和未关联的坐标系");
    });
}

/// Draws the static/temporal edge filter controls.
fn edge_filter_ui(ui: &mut egui::Ui, state: &mut TransformCacheUiState) {
    ui.selectable_toggle(|ui| {
        ui.selectable_value(
            &mut state.edge_filter,
            transform_cache_snapshot::EdgeFilter::Static,
            "静态",
        )
        .on_hover_text("只显示静态变换");
        ui.selectable_value(
            &mut state.edge_filter,
            transform_cache_snapshot::EdgeFilter::Temporal,
            "时序",
        )
        .on_hover_text("只显示时序变换");
        ui.selectable_value(
            &mut state.edge_filter,
            transform_cache_snapshot::EdgeFilter::All,
            "全部",
        )
        .on_hover_text("显示静态和时序变换");
    });
}

/// Draws the horizontal/vertical layout controls.
fn layout_direction_ui(ui: &mut egui::Ui, state: &mut TransformCacheUiState) {
    ui.horizontal_centered(|ui| {
        ui.label("布局：");
        ui.selectable_toggle(|ui| {
            ui.selectable_value(
                &mut state.layout_direction,
                LayoutDirection::Horizontal,
                "▶",
            )
            .on_hover_text("横向排布变换坐标系");
            ui.selectable_value(&mut state.layout_direction, LayoutDirection::Vertical, "▼")
                .on_hover_text("纵向排布变换坐标系");
        })
    });
}

/// Draws the zoomable transform-cache scene and its fixed overlay legend.
fn draw_transform_cache(
    ui: &mut egui::Ui,
    model: &Model,
    state: &mut TransformCacheUiState,
    settings_changed: bool,
) {
    let node_size = NODE_SIZE;
    let layout = (!model.snapshot.frames.is_empty())
        .then(|| Layout::compute(model, state.layout_direction, node_size));
    let content_rect = if let Some(layout) = &layout {
        layout.content_rect(node_size)
    } else {
        // Empty models still need scene bounds; use whatever space the panel currently has.
        ui.available_rect_before_wrap()
    };

    // Auto-adapt the rect to fit the content, unless the user has panned/zoomed.
    // Always redraw if the settings changed.
    if state.scene_rect.is_none() || settings_changed || !state.user_interacted {
        state.scene_rect = Some(content_rect);
    }
    let scene_rect = state.scene_rect.get_or_insert(content_rect);
    let frame_output = egui::Frame::new()
        .fill(ui.tokens().faint_bg_color)
        .show(ui, |ui| {
            egui::Scene::new()
                .zoom_range(SCENE_MIN_ZOOM..=SCENE_MAX_ZOOM)
                .show(ui, scene_rect, |ui| {
                    if let Some(layout) = &layout {
                        draw_transform_cache_contents(ui, model, layout, node_size, content_rect);
                    }
                })
                .response
        });
    let response = frame_output.inner;
    scene_legend_ui(ui, frame_output.response.rect);

    if response.changed() {
        state.user_interacted = true;
    }

    // Reset scene rect on double-click.
    if response.double_clicked() {
        state.scene_rect = Some(content_rect);
        state.user_interacted = false;
    }
}
