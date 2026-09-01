//! Basic ui elements & interaction for most `re_viewer_context::Item`.
//!
//! TODO(andreas): This is not a `data_ui`, can this go somewhere else, shouldn't be in `re_data_ui`.

use re_i18n::tr;
use re_entity_db::entity_db::EntityDbClass;
use re_entity_db::{EntityTree, InstancePath};
use re_format::format_uint;
use re_log_types::{ApplicationId, EntityPath, TableId, TimeInt, TimeType, TimelineName};
use re_sdk_types::archetypes::RecordingInfo;
use re_sdk_types::components::{Name, Timestamp};
use re_ui::list_item::ListItemContentButtonsExt as _;
use re_ui::{SyntaxHighlighting as _, UiExt as _, icons, list_item};
use re_viewer_context::open_url::ViewerOpenUrl;
use re_viewer_context::{
    AppContext, DataResultInteractionAddress, HoverHighlight, Item, Route, StoreViewContext,
    SystemCommand, SystemCommandSender as _, TimeControlCommand, UiLayout, ViewId, ViewerContext,
};

use crate::AppUi as _;

use super::DataUi as _;

// TODO(andreas): This is where we want to go, but we need to figure out how get the [`re_viewer_context::ViewClass`] from the `ViewId`.
// Simply pass in optional icons?
//
// Show a button to an [`Item`] with a given text.
// pub fn item_button_to(
//     ctx: &ViewerContext<'_>,
//     ui: &mut egui::Ui,
//     item: &Item,
//     text: impl Into<egui::WidgetText>,
// ) -> egui::Response {
//     match item {
//         Item::ComponentPath(component_path) => {
//             component_path_button_to(ctx, ui, text, component_path)
//         }
//         Item::View(view_id) => {
//             view_button_to(ctx, ui, text, *view_id, view_category)
//         }
//         Item::InstancePath(view_id, instance_path) => {
//             instance_path_button_to(ctx, ui, *view_id, instance_path, text)
//         }
//     }
// }

/// Show an entity path and make it selectable.
pub fn entity_path_button(
    ctx: &StoreViewContext<'_>,
    ui: &mut egui::Ui,
    view_id: Option<ViewId>,
    entity_path: &EntityPath,
) -> egui::Response {
    instance_path_button_to(
        ctx,
        ui,
        view_id,
        &InstancePath::entity_all(entity_path.clone()),
        entity_path.syntax_highlighted(ui.style()),
    )
}

/// Show the different parts of an entity path and make them selectable.
pub fn entity_path_parts_buttons(
    ctx: &StoreViewContext<'_>,
    ui: &mut egui::Ui,
    view_id: Option<ViewId>,
    entity_path: &EntityPath,
) -> egui::Response {
    let with_individual_icons = false; // too much noise with icons in a path

    ui.horizontal(|ui| {
        {
            ui.spacing_mut().item_spacing.x = 2.0;

            // The last part points to the selected entity, but that's ugly, so remove the highlight:
            let visuals = ui.visuals_mut();
            visuals.selection.bg_fill = egui::Color32::TRANSPARENT;
            visuals.selection.stroke = visuals.widgets.inactive.fg_stroke;
        }

        if !with_individual_icons {
            // Show one single icon up-front instead:
            let instance_path = InstancePath::entity_all(entity_path.clone());
            ui.add(instance_path_icon(ctx, &instance_path).as_image());
        }

        if entity_path.is_root() {
            ui.strong("/");
        } else {
            let mut accumulated = Vec::new();
            for part in entity_path.iter() {
                accumulated.push(part.clone());

                ui.strong("/");
                instance_path_button_to_ex(
                    ctx,
                    ui,
                    view_id,
                    &InstancePath::entity_all(accumulated.clone()),
                    part.syntax_highlighted(ui.style()),
                    with_individual_icons,
                );
            }
        }
    })
    .response
}

/// Show an entity path and make it selectable.
pub fn entity_path_button_to(
    ctx: &StoreViewContext<'_>,
    ui: &mut egui::Ui,
    view_id: Option<ViewId>,
    entity_path: &EntityPath,
    text: impl Into<egui::WidgetText>,
) -> egui::Response {
    instance_path_button_to(
        ctx,
        ui,
        view_id,
        &InstancePath::entity_all(entity_path.clone()),
        text,
    )
}

/// Show an instance id and make it selectable.
pub fn instance_path_button(
    ctx: &StoreViewContext<'_>,
    ui: &mut egui::Ui,
    view_id: Option<ViewId>,
    instance_path: &InstancePath,
) -> egui::Response {
    instance_path_button_to(
        ctx,
        ui,
        view_id,
        instance_path,
        instance_path.syntax_highlighted(ui.style()),
    )
}

/// Return the instance path icon.
///
/// The choice of icon is based on whether the instance is "empty" as in hasn't any logged component
/// _on the current timeline_.
pub fn instance_path_icon(
    ctx: &StoreViewContext<'_>,
    instance_path: &InstancePath,
) -> &'static icons::Icon {
    if instance_path.is_all() {
        let timeline = ctx.timeline_name();

        // It is an entity path
        if ctx
            .db
            .storage_engine()
            .store()
            .entity_has_physical_data_on_timeline(&timeline, &instance_path.entity_path)
        {
            if instance_path.entity_path.is_reserved() {
                &icons::ENTITY_RESERVED
            } else {
                &icons::ENTITY
            }
        } else if instance_path.entity_path.is_reserved() {
            &icons::ENTITY_RESERVED_EMPTY
        } else {
            &icons::ENTITY_EMPTY
        }
    } else {
        // An instance path
        &icons::ENTITY
    }
}

pub fn guess_instance_path_icon(
    ctx: &ViewerContext<'_>,
    instance_path: &InstancePath,
) -> &'static icons::Icon {
    let ctx = ctx.guess_store_view_context_for_entity(&instance_path.entity_path);
    instance_path_icon(&ctx, instance_path)
}

/// Show an instance id and make it selectable.
pub fn instance_path_button_to(
    ctx: &StoreViewContext<'_>,
    ui: &mut egui::Ui,
    view_id: Option<ViewId>,
    instance_path: &InstancePath,
    text: impl Into<egui::WidgetText>,
) -> egui::Response {
    instance_path_button_to_ex(ctx, ui, view_id, instance_path, text, true)
}

/// Show an instance id and make it selectable.
fn instance_path_button_to_ex(
    ctx: &StoreViewContext<'_>,
    ui: &mut egui::Ui,
    view_id: Option<ViewId>,
    instance_path: &InstancePath,
    text: impl Into<egui::WidgetText>,
    with_icon: bool,
) -> egui::Response {
    let item = if let Some(view_id) = view_id {
        Item::DataResult(DataResultInteractionAddress {
            view_id,
            instance_path: instance_path.clone(),
            visualizer: None,
        })
    } else {
        Item::InstancePath(instance_path.clone())
    };

    let response = if with_icon {
        ui.selectable_label_with_icon(
            instance_path_icon(ctx, instance_path),
            text,
            ctx.is_selected_or_loading(&item),
            re_ui::LabelStyle::Normal,
        )
    } else {
        ui.selectable_label(ctx.is_selected_or_loading(&item), text)
    };

    let response = response.on_hover_ui(|ui| {
        let include_subtree = false;
        instance_hover_card_ui(ui, ctx, instance_path, include_subtree);
    });

    cursor_interact_with_selectable(ctx, response, item)
}

/// Show the different parts of an instance path and make them selectable.
pub fn instance_path_parts_buttons(
    ctx: &StoreViewContext<'_>,
    ui: &mut egui::Ui,
    view_id: Option<ViewId>,
    instance_path: &InstancePath,
) -> egui::Response {
    let with_icon = false; // too much noise with icons in a path

    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 2.0;

        // Show one single icon up-front instead:
        ui.add(instance_path_icon(ctx, instance_path).as_image());

        let mut accumulated = Vec::new();
        for part in instance_path.entity_path.iter() {
            accumulated.push(part.clone());

            ui.strong("/");
            instance_path_button_to_ex(
                ctx,
                ui,
                view_id,
                &InstancePath::entity_all(accumulated.clone()),
                part.syntax_highlighted(ui.style()),
                with_icon,
            );
        }

        if !instance_path.instance.is_all() {
            ui.weak("[");
            instance_path_button_to_ex(
                ctx,
                ui,
                view_id,
                instance_path,
                instance_path.instance.syntax_highlighted(ui.style()),
                with_icon,
            );
            ui.weak("]");
        }
    })
    .response
}

/// If `include_subtree=true`, stats for the entire entity subtree will be shown.
fn entity_tree_stats_ui(
    ui: &mut egui::Ui,
    timeline: &TimelineName,
    db: &re_entity_db::EntityDb,
    tree: &EntityTree,
    include_subtree: bool,
) {
    use re_format::format_bytes;

    let subtree_caveat = if tree.children.is_empty() {
        ""
    } else if include_subtree {
        tr(" (including subtree)", "（含子树）")
    } else {
        tr(" (excluding subtree)", "（不含子树）")
    };

    let engine = db.storage_engine();

    let (static_stats, timeline_stats) = if include_subtree {
        (
            re_entity_db::EntityDb::subtree_stats_static(&engine, &tree.path),
            re_entity_db::EntityDb::subtree_stats_on_timeline(&engine, &tree.path, timeline),
        )
    } else {
        (
            engine.store().entity_stats_static(&tree.path),
            engine
                .store()
                .entity_stats_on_timeline(&tree.path, timeline),
        )
    };

    let total_stats = static_stats + timeline_stats;

    if total_stats.num_rows == 0 {
        return;
    } else if timeline_stats.num_rows == 0 {
        ui.label(format!(
            "{} 行静态数据{subtree_caveat}",
            format_uint(total_stats.num_rows)
        ));
    } else if static_stats.num_rows == 0 {
        ui.label(format!(
            "时间轴 '{timeline}' 上有 {} 行{subtree_caveat}",
            format_uint(total_stats.num_rows),
        ));
    } else {
        ui.label(format!(
            "共 {} 行 = {} 行静态数据 + 时间轴 '{timeline}' 上 {} 行{subtree_caveat}",
            format_uint(total_stats.num_rows),
            format_uint(static_stats.num_rows),
            format_uint(timeline_stats.num_rows),
        ));
    }

    let num_temporal_rows = timeline_stats.num_rows;

    let mut data_rate = None;

    if 0 < timeline_stats.total_size_bytes && 1 < num_temporal_rows {
        // Try to estimate data-rate:
        if let Some(time_range) = engine.store().entity_time_range(timeline, &tree.path) {
            let min_time = time_range.min();
            let max_time = time_range.max();
            if min_time < max_time {
                // Let's do our best to avoid fencepost errors.
                // If we log 1 MiB once every second, then after three
                // events we have a span of 2 seconds, and 3 MiB,
                // but the data rate is still 1 MiB/s.
                //
                //          <-----2 sec----->
                // t:       0s      1s      2s
                // data:   1MiB    1MiB    1MiB

                let duration = max_time.as_f64() - min_time.as_f64();

                let mut bytes_per_time = timeline_stats.total_size_bytes as f64 / duration;

                // Fencepost adjustment:
                bytes_per_time *= (num_temporal_rows - 1) as f64 / num_temporal_rows as f64;

                let typ = db.timeline_type(timeline);

                data_rate = Some(match typ {
                    TimeType::Sequence => {
                        format!("{} / {}", format_bytes(bytes_per_time), timeline)
                    }

                    TimeType::DurationNs | TimeType::TimestampNs => {
                        let bytes_per_second = 1e9 * bytes_per_time;

                        format!("{}/s in '{}'", format_bytes(bytes_per_second), timeline)
                    }
                });
            }
        }
    }

    if let Some(data_rate) = data_rate {
        ui.label(format!(
            "占用约 {}{subtree_caveat} ≈ {}",
            format_bytes(total_stats.total_size_bytes as f64),
            data_rate
        ));
    } else {
        ui.label(format!(
            "占用约 {}{subtree_caveat}",
            format_bytes(total_stats.total_size_bytes as f64)
        ));
    }
}

pub fn data_blueprint_button_to(
    ctx: &StoreViewContext<'_>,
    ui: &mut egui::Ui,
    text: impl Into<egui::WidgetText>,
    view_id: ViewId,
    entity_path: &EntityPath,
) -> egui::Response {
    let item = Item::DataResult(DataResultInteractionAddress::from_entity_path(
        view_id,
        entity_path.clone(),
    ));
    let response = ui
        .selectable_label(ctx.is_selected_or_loading(&item), text)
        .on_hover_ui(|ui| {
            let include_subtree = false;
            entity_hover_card_ui(ui, ctx, entity_path, include_subtree);
        });
    cursor_interact_with_selectable(ctx, response, item)
}

pub fn time_button(
    ctx: &ViewerContext<'_>,
    ui: &mut egui::Ui,
    timeline_name: &TimelineName,
    value: TimeInt,
) -> egui::Response {
    let is_selected = ctx.time_ctrl.is_time_selected(timeline_name, value);

    let typ = ctx.recording().timeline_type(timeline_name);

    let response = ui.selectable_label(
        is_selected,
        typ.format(value, ctx.app_options().timestamp_format),
    );
    if response.clicked() {
        ctx.send_time_commands([
            TimeControlCommand::SetActiveTimeline(*timeline_name),
            TimeControlCommand::SetTime(value.into()),
            TimeControlCommand::Pause,
        ]);
    }
    response
}

pub fn timeline_button(
    ctx: &AppContext<'_>,
    ui: &mut egui::Ui,
    timeline: &TimelineName,
) -> egui::Response {
    timeline_button_to(ctx, ui, timeline.to_string(), timeline)
}

pub fn timeline_button_to(
    ctx: &AppContext<'_>,
    ui: &mut egui::Ui,
    text: impl Into<egui::WidgetText>,
    timeline_name: &TimelineName,
) -> egui::Response {
    let is_selected = ctx
        .active_time_ctrl()
        .is_some_and(|time_ctr| time_ctr.timeline_name() == timeline_name);

    let response = ui
        .selectable_label(is_selected, text)
        .on_hover_text(tr("Click to switch to this timeline", "点击切换到这条时间轴"));
    if response.clicked() {
        ctx.send_time_commands_to_active_recording([
            TimeControlCommand::SetActiveTimeline(*timeline_name),
            TimeControlCommand::Pause,
        ]);
    }
    response
}

// TODO(andreas): Move elsewhere, this is not directly part of the item_ui.
pub fn cursor_interact_with_selectable(
    ctx: &AppContext<'_>,
    response: egui::Response,
    item: Item,
) -> egui::Response {
    let is_item_hovered =
        ctx.selection_state().highlight_for_ui_element(&item) == HoverHighlight::Hovered;

    ctx.handle_select_hover_drag_interactions(&response, item, false);
    // TODO(andreas): How to deal with shift click for selecting ranges?

    if is_item_hovered {
        response.highlight()
    } else {
        response
    }
}

/// Displays the "hover card" (i.e. big tooltip) for an instance or an entity.
///
/// The entity hover card is displayed if the provided instance path doesn't refer to a specific
/// instance.
///
/// If `include_subtree=true`, stats for the entire entity subtree will be shown.
pub fn instance_hover_card_ui(
    ui: &mut egui::Ui,
    ctx: &StoreViewContext<'_>,
    instance_path: &InstancePath,
    include_subtree: bool,
) {
    if !ctx.db.is_known_entity(&instance_path.entity_path) {
        ui.label(tr("Unknown entity.", "未知实体。"));
        return;
    }

    ui.horizontal(|ui| {
        let subtype_string = if instance_path.instance.is_all() {
            tr("Entity", "实体")
        } else {
            tr("Entity instance", "实体实例")
        };
        ui.strong(subtype_string);
        ui.label(instance_path.syntax_highlighted(ui.style()));
    });

    // TODO(emilk): give data_ui an alternate "everything on this timeline" query?
    // Then we can move the size view into `data_ui`.

    if instance_path.instance.is_all() {
        let db_engine = ctx.db.storage_engine();
        if let Some(subtree) = db_engine
            .store()
            .entity_tree()
            .subtree(&instance_path.entity_path)
        {
            entity_tree_stats_ui(ui, &ctx.timeline_name(), ctx.db, subtree, include_subtree);
        }
    } else {
        // TODO(emilk): per-component stats
    }

    instance_path.data_ui(ctx, ui, UiLayout::Tooltip);
}

/// Displays the "hover card" (i.e. big tooltip) for an entity.
///
/// If `include_subtree=true`, stats for the entire entity subtree will be shown.
pub fn entity_hover_card_ui(
    ui: &mut egui::Ui,
    ctx: &StoreViewContext<'_>,
    entity_path: &EntityPath,
    include_subtree: bool,
) {
    let instance_path = InstancePath::entity_all(entity_path.clone());
    instance_hover_card_ui(ui, ctx, &instance_path, include_subtree);
}

pub fn app_id_button_ui(
    ctx: &AppContext<'_>,
    ui: &mut egui::Ui,
    app_id: &ApplicationId,
) -> egui::Response {
    let item = Item::AppId(app_id.clone());

    let response = ui.selectable_label_with_icon(
        &icons::APPLICATION,
        app_id.to_string(),
        ctx.is_selected_or_loading(&item),
        re_ui::LabelStyle::Normal,
    );

    let response = response.on_hover_ui(|ui| {
        app_id.app_ui(ctx, ui, re_viewer_context::UiLayout::Tooltip);
    });

    cursor_interact_with_selectable(ctx, response, item)
}

pub fn data_source_button_ui(
    ctx: &AppContext<'_>,
    ui: &mut egui::Ui,
    data_source: &re_log_channel::LogSource,
) -> egui::Response {
    let item = Item::DataSource(data_source.clone());

    let response = ui.selectable_label_with_icon(
        &icons::DATA_SOURCE,
        data_source.to_string(),
        ctx.is_selected_or_loading(&item),
        re_ui::LabelStyle::Normal,
    );

    let response = response.on_hover_ui(|ui| {
        data_source.app_ui(ctx, ui, re_viewer_context::UiLayout::Tooltip);
    });

    cursor_interact_with_selectable(ctx, response, item)
}

/// This uses [`list_item::ListItem::show_hierarchical`], meaning it comes with built-in
/// indentation.
pub fn store_id_button_ui(
    ctx: &AppContext<'_>,
    ui: &mut egui::Ui,
    store_id: &re_log_types::StoreId,
    ui_layout: UiLayout,
) {
    if let Some(entity_db) = ctx.store_bundle().get(store_id) {
        entity_db_button_ui(ctx, entity_db, ui, ui_layout, true);
    } else {
        ui_layout.label(ui, "<unknown store>").on_hover_ui(|ui| {
            ui.label(format!("{store_id}"));
        });
    }
}

/// Show button for a store (recording or blueprint).
///
/// You can set `include_app_id` to hide the App Id, but usually you want to show it.
///
/// This uses [`list_item::ListItem::show_hierarchical`], meaning it comes with built-in
/// indentation.
pub fn entity_db_button_ui(
    ctx: &AppContext<'_>,
    entity_db: &re_entity_db::EntityDb,
    ui: &mut egui::Ui,
    ui_layout: UiLayout,
    include_app_id: bool,
) -> egui::Response {
    re_tracing::profile_function!();

    use re_viewer_context::{SystemCommand, SystemCommandSender as _};

    let app_id_prefix = if include_app_id {
        format!("{} - ", entity_db.application_id())
    } else {
        String::default()
    };

    // We try to use a name that has the most chance to be familiar to the user:
    // - The recording name has to be explicitly set by the user, so use it if it exists.
    // - For remote data, segment id have a lot of visibility too, so good fall-back.
    // - Lacking anything better, the start time is better than a random id and caters to the local
    //   workflow where the same logging process is run repeatedly.
    let recording_name = if let Some(recording_name) =
        entity_db.recording_info_property::<Name>(RecordingInfo::descriptor_name().component)
    {
        Some(recording_name.to_string())
    } else if let EntityDbClass::DatasetSegment(url) = entity_db.store_class() {
        Some(url.segment_id.to_string())
    } else {
        entity_db
            .recording_info_property::<Timestamp>(RecordingInfo::descriptor_start_time().component)
            .map(|started| {
                re_log_types::Timestamp::from(started.0)
                    .to_jiff_zoned(ctx.app_options.timestamp_format)
                    .strftime("%H:%M:%S")
                    .to_string()
            })
    }
    .unwrap_or_else(|| "<unknown>".to_owned());

    let size = re_format::format_bytes(entity_db.byte_size_of_physical_chunks() as _);
    let mut title = format!("{app_id_prefix}{recording_name} - {size}");

    let store_id = entity_db.store_id().clone();
    let item = re_viewer_context::Item::StoreId(store_id.clone());

    let icon = match entity_db.store_kind() {
        re_log_types::StoreKind::Recording => &icons::RECORDING,
        re_log_types::StoreKind::Blueprint => &icons::BLUEPRINT,
    };

    let episode_loading = re_data_source::lerobot_remote::is_episode_loading(&store_id);
    let episode_failure = re_data_source::lerobot_remote::episode_failure(&store_id);
    let download_progress = if episode_loading {
        re_data_source::lerobot_remote::episode_download_progress(&store_id)
    } else {
        None
    };

    // Announced but not yet downloaded — such episodes hold only their ~1 KB properties
    // chunk until real data arrives. Dimming them makes the ready-to-view ones stand out,
    // instead of every row looking equally "done".
    let episode_queued = !episode_loading
        && episode_failure.is_none()
        && re_data_source::lerobot_remote::is_dataset_streaming(store_id.application_id().as_str())
        && !re_data_source::lerobot_remote::is_more_placeholder(&store_id)
        && entity_db.byte_size_of_physical_chunks() <= 16 * 1024;

    // Live progress in the row itself: big downloads are agony without a sense of how far
    // along they are. The full name would push the numbers past the panel's truncation, so
    // while loading the row shows a compact "<name> · downloading mp4 43% · ~12s left" instead
    // (the full name returns when the item finishes). The phase word matters: a bare
    // percentage reads as stalled during the conversion that follows the download.
    if let Some(progress) = &download_progress {
        use re_data_source::lerobot_remote::LoadPhase;
        use std::fmt::Write as _;
        let short_name = recording_name
            .split(" · ")
            .next()
            .unwrap_or(recording_name.as_str());
        title = format!("{app_id_prefix}{short_name}");
        match progress.phase {
            LoadPhase::Converting => {
                write!(title, " · 正在转换…").ok();
            }
            LoadPhase::Downloading => {
                // Name the file type ("downloading parquet"): it tells apart fetching
                // sources for a conversion from fetching a ready-made rrd artifact.
                write!(title, " · 正在下载").ok();
                if let Some(kind) = progress.kind.label() {
                    write!(title, " {kind}").ok();
                }
                if let Some(total) = progress.bytes_total {
                    let pct = (progress.bytes_done as f64 / total.max(1) as f64 * 100.0).min(100.0);
                    write!(title, " {pct:.0}%").ok();
                } else {
                    write!(
                        title,
                        " {}",
                        re_format::format_bytes(progress.bytes_done as _)
                    )
                    .ok();
                }
                if let Some(eta) = progress.eta_secs {
                    if eta >= 90.0 {
                        write!(title, " · 约剩 {:.0} 分钟", (eta / 60.0).ceil()).ok();
                    } else {
                        write!(title, " · 约剩 {eta:.0} 秒").ok();
                    }
                }
            }
        }
    }

    // Hidden episodes live in the panel's collapsed "Hidden episodes" group; the row itself
    // is subdued and its buttons reduce to show + close (see below).
    let is_hidden = re_viewer_context::hidden_recordings::is_hidden(&store_id);

    let mut item_content = if episode_failure.is_some() {
        // Upstream convention for failed entries: red text, reason on hover
        // (see `failed_entry_ui` in the recording panel).
        list_item::LabelContent::new(egui::RichText::new(title).color(ui.visuals().error_fg_color))
            .with_icon(icon)
    } else if episode_loading
        && re_data_source::lerobot_remote::is_dataset_paused(store_id.application_id().as_str())
    {
        // Mid-download, but the whole dataset is paused: the download is frozen in place
        // (it continues from here on resume). An animated indicator would wrongly suggest
        // it is still running.
        list_item::LabelContent::new(title).with_icon(&icons::PAUSE)
    } else if episode_loading {
        // This episode is being downloaded right now — animate instead of the static icon.
        list_item::LabelContent::new(title).with_icon_fn(|ui, rect, _visuals| {
            re_ui::loading_indicator::paint_loading_indicator_inside(
                ui,
                egui::Align2::CENTER_CENTER,
                rect,
                1.0,
                None,
                tr("downloading episode", "正在下载 episode"),
            );
        })
    } else {
        list_item::LabelContent::new(title)
            .with_icon(icon)
            .subdued(episode_queued || is_hidden)
    };

    if ui_layout.is_selection_panel() {
        // Per-item download controls of remote dataset streams (TOS / Hugging Face).
        let streaming = re_data_source::lerobot_remote::is_dataset_streaming(
            store_id.application_id().as_str(),
        );
        let dataset_paused = streaming
            && re_data_source::lerobot_remote::is_dataset_paused(
                store_id.application_id().as_str(),
            );
        let episode_parked =
            streaming && re_data_source::lerobot_remote::is_episode_parked(&store_id);
        let episode_failed =
            streaming && re_data_source::lerobot_remote::is_episode_failed(&store_id);
        // Announced-but-not-downloaded items already hold a small properties chunk (~1 KB),
        // so "has downloaded data" needs a threshold, not just > 0.
        let has_data = entity_db.byte_size_of_physical_chunks() > 16 * 1024;

        // The "…and N more" placeholder row is not an episode; it cannot be hidden.
        let can_hide = streaming && !re_data_source::lerobot_remote::is_more_placeholder(&store_id);

        if is_hidden {
            // The show-button is the whole point of a row in the "Hidden episodes" group —
            // it must be visible without discovering the hover behavior first.
            item_content = item_content.with_always_show_buttons(true);
        }

        let store_id = store_id.clone();
        item_content = item_content.with_buttons(move |ui| {
            // Close-button:
            let resp = ui
                .small_icon_button(&icons::CLOSE_SMALL, tr("Close recording", "关闭录制文件"))
                .on_hover_text(match store_id.kind() {
                    re_log_types::StoreKind::Recording => tr("Close this recording", "关闭这个录制文件"),
                    re_log_types::StoreKind::Blueprint => {
                        tr("Close this blueprint (unsaved data will be lost)", "关闭这个 blueprint（未保存的数据会丢失）")
                    }
                });
            if resp.clicked() {
                ctx.command_sender
                    .send_system(SystemCommand::CloseRecordingOrTable(
                        store_id.clone().into(),
                    ));
            }

            // Hide/show — the eye, like the blueprint tree's visibility toggle. The episode
            // keeps its data and moves into the collapsed "Hidden episodes" group at the
            // bottom of the dataset; closing (×) is what frees memory.
            if can_hide {
                if is_hidden {
                    if ui
                        .small_icon_button(&icons::VISIBLE, tr("Move the episode back to the list", "把这个 episode 移回列表"))
                        .on_hover_text(tr("Move the episode back to the list", "把这个 episode 移回列表"))
                        .clicked()
                    {
                        re_viewer_context::hidden_recordings::unhide(&store_id);
                    }
                } else if ui
                    .small_icon_button(&icons::INVISIBLE, tr("Hide the episode", "隐藏这个 episode"))
                    .on_hover_text(tr("Hide the episode", "隐藏这个 episode"))
                    .clicked()
                {
                    re_viewer_context::hidden_recordings::hide(store_id.clone());
                    // A hidden episode must not hold up the download queue: stop its
                    // download (if running) and keep it out of the auto-download order.
                    // Un-hiding brings it back parked; clicking it downloads it again.
                    if episode_loading || episode_queued {
                        re_data_source::lerobot_remote::park_episode_for_store(&store_id);
                    }
                }
            }

            if streaming && !is_hidden {
                // While the whole dataset is paused, the stream is parked and cannot react
                // to per-episode requests — a re-download would drop the old data and then
                // sit in the queue until resume, looking like the episode was deleted.
                // Keep the state simple: gray the per-episode download controls out.
                ui.add_enabled_ui(!dataset_paused, |ui| {
                    if episode_loading {
                        if ui
                            .small_icon_button(&icons::PAUSE, tr("Pause downloading this episode", "暂停下载这个 episode"))
                            .on_hover_text(
                                "暂停下载这个 episode。\
                             点击该 episode（或它的继续按钮）可重新开始。",
                            )
                            .clicked()
                        {
                            re_data_source::lerobot_remote::pause_current_item(
                                store_id.application_id().as_str(),
                            );
                        }
                    } else if episode_parked {
                        if ui
                            .small_icon_button(&icons::PLAY, "继续下载这个 episode")
                            .on_hover_text("继续下载这个 episode")
                            .clicked()
                        {
                            re_data_source::lerobot_remote::prioritize_episode_for_store(&store_id);
                        }
                    } else if (has_data || episode_failed)
                        && ui
                            .small_icon_button(&icons::RESET, "重新下载这个 episode")
                            .on_hover_text("重新下载这个 episode")
                            .clicked()
                    {
                        // Arm the re-download marker, then close the recording to drop the old
                        // data; the close hook completes the hand-off once the store is gone
                        // (fetching before the close is processed would lose the race).
                        re_data_source::lerobot_remote::redownload_episode_for_store(&store_id);
                        ctx.command_sender
                            .send_system(SystemCommand::CloseRecordingOrTable(
                                store_id.clone().into(),
                            ));
                    }
                });
            }
        });
    }

    let mut list_item = ui
        .list_item()
        .active(
            ctx.active_store_context
                .is_some_and(|sc| sc.is_active(&store_id)),
        )
        .selected(ctx.is_selected_or_loading(&item));

    if ctx.hovered().contains_item(&item) {
        list_item = list_item.force_hovered(true);
    }

    let mut response = list_item::list_item_scope(ui, "entity db button", |ui| {
        list_item
            .show_hierarchical(ui, item_content)
            .on_hover_ui(|ui| {
                entity_db.app_ui(ctx, ui, re_viewer_context::UiLayout::Tooltip);
            })
    })
    .inner;

    if let Some(reason) = &episode_failure {
        response = response.on_hover_text(reason.clone());
    }

    // A converted copy of this episode lives in the rrd artifacts store: show where. The address is
    // copyable via the context menu ("Copy rrd artifact address").
    if episode_queued {
        response = response
            .on_hover_text("尚未下载 — 点击后移到下载队列最前。");
    }

    if let Some(artifact_url) = re_data_source::lerobot_remote::episode_rrd_artifact_url(&store_id)
    {
        response = response.on_hover_text(format!("rrd artifact：{artifact_url}"));
    }

    if let Some(progress) = &download_progress {
        let total = progress.bytes_total.map_or_else(
            || "大小未知".to_owned(),
            |total| re_format::format_bytes(total as _),
        );
        response = response.on_hover_text(format!(
            "正在下载：{} / {} · {}/s",
            re_format::format_bytes(progress.bytes_done as _),
            total,
            re_format::format_bytes(progress.bytes_per_sec),
        ));
    }

    if response.hovered() {
        ctx.selection_state().set_hovered(item.clone());
    }

    let new_entry: re_viewer_context::RecordingOrTable = store_id.clone().into();

    response.context_menu(|ui| {
        let url = ViewerOpenUrl::from_route(ctx.store_hub(), &new_entry.route())
            .and_then(|url| url.sharable_url(None));
        if ui
            .add_enabled(url.is_ok(), egui::Button::new("复制该片段的链接"))
            .on_disabled_hover_text(if let Err(err) = url.as_ref() {
                format!("无法复制该片段的链接：{err}")
            } else {
                "无法复制该片段的链接".to_owned()
            })
            .clicked()
            && let Ok(url) = url
        {
            ctx.command_sender
                .send_system(SystemCommand::CopyViewerUrl(url));
        }

        if ui.button("复制片段名称").clicked() {
            re_log::info!("已复制 {recording_name:?} 到剪贴板");
            ui.copy_text(recording_name);
        }

        // Artifact management for remote-dataset episodes with a converted rrd in the store.
        if let Some(artifact_url) =
            re_data_source::lerobot_remote::episode_rrd_artifact_url(&store_id)
        {
            let app_id_str = store_id.application_id().as_str();
            let episode = re_data_source::lerobot_remote::episode_queue_index(&store_id);
            let deleting = re_data_source::rrd_artifacts::deletion_in_flight(app_id_str, episode);
            let no_permission =
                re_data_source::lerobot_remote::dataset_artifacts_config(app_id_str)
                    .as_ref()
                    .and_then(re_data_source::rrd_artifacts::delete_permission)
                    == Some(false);

            ui.separator();
            if ui.button("复制 rrd artifact 地址").clicked() {
                ui.copy_text(artifact_url.clone());
            }
            if ui
                .add_enabled(
                    !deleting && !no_permission,
                    egui::Button::new("删除 rrd artifact…"),
                )
                .on_disabled_hover_text(if no_permission {
                    "当前凭证没有删除权限"
                } else {
                    "正在删除…"
                })
                .clicked()
            {
                // Only queues a request: the viewer shows a confirmation dialog first.
                let app_id = store_id.application_id();
                re_data_source::rrd_artifacts::request_deletion(
                    re_data_source::rrd_artifacts::ArtifactDeletionRequest {
                        application_id: app_id.to_string(),
                        dataset_url: re_data_source::lerobot_remote::dataset_url_of(
                            app_id.as_str(),
                        )
                        .unwrap_or_else(|| app_id.to_string()),
                        episode,
                        target_url: artifact_url,
                    },
                );
            }
        }
    });

    if response.clicked() {
        // When we click on a recording, we directly activate it. This is safe to do because
        // it's non-destructive and recordings are immutable. Switching back is easy.
        // We don't do the same thing for blueprints as swapping them can be much more disruptive.
        // It is much less obvious how to undo a blueprint switch and what happened to your original
        // blueprint.
        // TODO(jleibs): We should still have an `Activate this Blueprint` button in the selection panel
        // for the blueprint.
        if store_id.is_recording() {
            ctx.command_sender
                .send_system(SystemCommand::SetRoute(new_entry.route()));
        }
    }

    ctx.handle_select_hover_drag_interactions(&response, item.clone(), false);
    response
}

pub fn table_id_button_ui(
    ctx: &AppContext<'_>,
    ui: &mut egui::Ui,
    table_id: &TableId,
    ui_layout: UiLayout,
) {
    let item = re_viewer_context::Item::TableId(table_id.clone());

    let mut item_content = list_item::LabelContent::new(table_id.as_str()).with_icon(&icons::TABLE);

    if ui_layout.is_selection_panel() {
        item_content = item_content.with_buttons(|ui| {
            // Close-button:
            let resp = ui
                .small_icon_button(&icons::CLOSE_SMALL, "关闭表格")
                .on_hover_text("关闭这个表格（所有数据都会丢失）");
            if resp.clicked() {
                ctx.command_sender()
                    .send_system(SystemCommand::CloseRecordingOrTable(
                        table_id.clone().into(),
                    ));
            }
        });
    }

    let mut list_item = ui
        .list_item()
        .selected(ctx.is_selected_or_loading(&item))
        .active(ctx.active_table_id() == Some(table_id));

    if ctx.hovered().contains_item(&item) {
        list_item = list_item.force_hovered(true);
    }

    let response = list_item::list_item_scope(ui, "entity db button", |ui| {
        list_item
            .show_hierarchical(ui, item_content)
            .on_hover_ui(|ui| {
                ui.label(format!("表格：{table_id}"));
            })
    })
    .inner;

    if response.hovered() {
        ctx.selection_state().set_hovered(item.clone());
    }

    if response.clicked() {
        ctx.command_sender()
            .send_system(SystemCommand::SetRoute(Route::LocalTable(table_id.clone())));
    }
    ctx.handle_select_hover_drag_interactions(&response, item, false);
}
