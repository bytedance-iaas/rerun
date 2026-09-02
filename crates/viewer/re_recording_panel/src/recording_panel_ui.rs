use re_i18n::{tr, trf};
use std::sync::Arc;

use egui::collapsing_header::CollapsingState;
use egui::{RichText, WidgetInfo, WidgetType};
use re_data_ui::AppUi as _;
use re_data_ui::item_ui::{entity_db_button_ui, table_id_button_ui};
use re_log_channel::LogSource;
use re_log_types::TableId;
use re_redap_browser::{EXAMPLES_ORIGIN, RedapServers};
use re_ui::list_item::{LabelContent, ListItemContentButtonsExt as _};
use re_ui::{ContextExt as _, OnResponseExt as _, UiExt as _, UiLayout, icons, list_item};
use re_uri::dataset_hierarchy_leaf_name;
use re_viewer_context::open_url::ViewerOpenUrl;
use re_viewer_context::{
    AppContext, Item, RecordingOrTable, RedapEntryKind, Route, SystemCommand,
    SystemCommandSender as _,
};

use crate::RecordingPanelCommand;
use crate::data::{
    AppIdData, DatasetData, EntryData, EntryTreeNode, FailedEntryData, RecordingData,
    RecordingPanelData, RemoteTableData, SegmentData, ServerData, ServerEntriesData,
    ServerLeafEntry,
};

#[derive(Debug, Clone, Default)]
pub struct RecordingPanel {
    commands: Vec<RecordingPanelCommand>,
}

impl RecordingPanel {
    pub fn send_command(&mut self, command: RecordingPanelCommand) {
        self.commands.push(command);
    }

    pub fn show_panel(
        &mut self,
        ctx: &AppContext<'_>,
        ui: &mut egui::Ui,
        servers: &RedapServers,
        hide_examples: bool,
    ) {
        re_tracing::profile_function!();
        let recording_panel_data = RecordingPanelData::new(ctx, servers, hide_examples);

        for command in self.commands.drain(..) {
            match command {
                RecordingPanelCommand::SelectNextRecording => {
                    shift_through_recordings(ctx, &recording_panel_data, 1);
                }
                RecordingPanelCommand::SelectPreviousRecording => {
                    shift_through_recordings(ctx, &recording_panel_data, -1);
                }
            }
        }

        for item in ctx.selection().iter_items() {
            expand_parents_for_item(ui.ctx(), &recording_panel_data, item);
        }

        ui.panel_content(|ui| {
            ui.panel_title_bar_with_buttons(
                tr("Sources", "数据来源"),
                Some(tr("Your connected servers, opened recordings and tables.", "已连接的服务器、已打开的 episode 和表格。")),
                |ui| {
                    add_button_ui(ctx, ui, &recording_panel_data);
                },
            );
        });

        egui::ScrollArea::both()
            .id_salt("recordings_scroll_area")
            .auto_shrink([false, false]) // shrinking forces to limit maximum height of the recording panel
            .show(ui, |ui| {
                ui.panel_content(|ui| {
                    re_ui::list_item::list_item_scope(ui, "recording panel", |ui| {
                        all_sections_ui(ctx, ui, &recording_panel_data);
                    })
                    .response
                    .widget_info(|| {
                        WidgetInfo::labeled(WidgetType::Panel, true, "_recording_panel")
                    });
                });
            });
    }
}

fn shift_through_recordings(
    ctx: &AppContext<'_>,
    recording_panel_data: &RecordingPanelData<'_>,
    direction: isize,
) {
    let Some(store_context) = ctx.active_store_context else {
        return;
    };
    let current_store_id = store_context.recording.store_id();

    #[expect(clippy::cast_possible_wrap)]
    if let Some((idx, store_collection)) =
        recording_panel_data.collection_from_recording(current_store_id)
    {
        let len = store_collection.len() as isize;
        let new_idx = ((idx as isize + direction + len) % len) as usize;

        // TODO(#11792): this whole feature would be massively more useful if we left the selection
        // alone and tried to maintain viewer state when switching recording (including current
        // timeline, time point, selection, etc.)
        ctx.command_sender()
            .send_system(SystemCommand::SetSelection(
                Item::StoreId(store_collection[new_idx].store_id().clone()).into(),
            ));
    }
}

fn add_button_ui(
    ctx: &AppContext<'_>,
    ui: &mut egui::Ui,
    _recording_panel_data: &RecordingPanelData<'_>,
) {
    ui.add(
        ui.small_icon_button_widget(&re_ui::icons::ADD, tr("Add…", "添加…"))
            .on_hover_text(tr("Open a file, dataset or connect to a server", "打开文件、数据集，或连接服务器"))
            .on_menu(|ui| {
                if re_ui::UICommand::Open
                    .menu_button_ui(ui, ctx.command_sender())
                    .clicked()
                {
                    ui.close();
                }
                if re_ui::UICommand::OpenUrl
                    .menu_button_ui(ui, ctx.command_sender())
                    .clicked()
                {
                    ui.close();
                }
                if re_ui::UICommand::AddRedapServer
                    .menu_button_ui(ui, ctx.command_sender())
                    .clicked()
                {
                    ui.close();
                }

                // Our additions on top of stock rerun, set apart as their own section.
                ui.separator();
                ui.add_enabled(
                    false,
                    egui::Button::new(egui::RichText::new(tr("Extended", "扩展功能")).italics()),
                );
                if re_ui::UICommand::OpenTosDataset
                    .menu_button_ui(ui, ctx.command_sender())
                    .clicked()
                {
                    ui.close();
                }
                if re_ui::UICommand::OpenHfDataset
                    .menu_button_ui(ui, ctx.command_sender())
                    .clicked()
                {
                    ui.close();
                }

                // Show some nice debugging tools in debug builds.
                #[cfg(debug_assertions)]
                {
                    ui.separator();
                    ui.debug_only_badge();

                    if ui.button("Print recording entity DBs").clicked() {
                        let recording_entity_dbs = ctx
                            .store_bundle()
                            .entity_dbs()
                            .filter(|entity_db| entity_db.store_id().is_recording())
                            .collect::<Vec<_>>();
                        println!("Recording entity DBs:\n{recording_entity_dbs:#?}\n");
                    }

                    if ui.button("Print recording panel data").clicked() {
                        println!("Recording panel data:\n{_recording_panel_data:#?}\n");
                    }
                }
            }),
    );
}

fn all_sections_ui(
    ctx: &AppContext<'_>,
    ui: &mut egui::Ui,
    recording_panel_data: &RecordingPanelData<'_>,
) {
    //
    // Welcome and examples
    //

    if recording_panel_data.show_example_section {
        welcome_item_ui(ctx, ui, recording_panel_data);
    }

    //
    // Empty placeholder
    //

    if recording_panel_data.is_empty() {
        ui.add_space(ui.tokens().panel_margin().left as f32);
        ui.weak(tr("Click + to add a recording, connect to a server or drag and drop a file directly to the viewer", "点击 + 添加 episode、连接服务器，或直接把文件拖进 Viewer"));
    }

    //
    // Servers
    //

    for server_data in &recording_panel_data.servers {
        server_section_ui(ctx, ui, server_data);
    }

    //
    // TOS datasets (streamed straight from an object-store bucket)
    //

    if !recording_panel_data.tos_apps.is_empty() {
        let id = egui::Id::new("tos items");
        if ui
            .list_item()
            .header()
            .show_hierarchical_with_children(
                ui,
                id,
                true,
                list_item::LabelContent::header(tr("Volcengine TOS", "火山引擎 TOS")),
                |ui| {
                    for app_id_data in &recording_panel_data.tos_apps {
                        app_id_section_ui(ctx, ui, app_id_data);
                    }
                },
            )
            .item_response
            .clicked()
        {
            let mut state = CollapsingState::load_with_default_open(ui.ctx(), id, true);
            state.toggle(ui);
            state.store(ui.ctx());
        }
    }

    //
    // Hugging Face datasets
    //

    if !recording_panel_data.hf_apps.is_empty() {
        let id = egui::Id::new("hf items");
        if ui
            .list_item()
            .header()
            .show_hierarchical_with_children(
                ui,
                id,
                true,
                list_item::LabelContent::header("Hugging Face"),
                |ui| {
                    for app_id_data in &recording_panel_data.hf_apps {
                        app_id_section_ui(ctx, ui, app_id_data);
                    }
                },
            )
            .item_response
            .clicked()
        {
            let mut state = CollapsingState::load_with_default_open(ui.ctx(), id, true);
            state.toggle(ui);
            state.store(ui.ctx());
        }
    }

    //
    // Local recordings and tables
    //

    if !recording_panel_data.local_apps.is_empty() || !recording_panel_data.local_tables.is_empty()
    {
        let id = egui::Id::new("local items");
        if ui
            .list_item()
            .header()
            .show_hierarchical_with_children(
                ui,
                id,
                true,
                list_item::LabelContent::header(tr("Local", "本地")),
                |ui| {
                    for app_id_data in &recording_panel_data.local_apps {
                        app_id_section_ui(ctx, ui, app_id_data);
                    }

                    for table_id in &recording_panel_data.local_tables {
                        table_item_ui(ctx, ui, table_id);
                    }
                },
            )
            .item_response
            .clicked()
        {
            let mut state = CollapsingState::load_with_default_open(ui.ctx(), id, true);
            state.toggle(ui);
            state.store(ui.ctx());
        }
    }

    //
    // Loading receivers
    //

    loading_receivers_ui(ctx, ui, &recording_panel_data.loading_receivers);

    // Add space at the end of the recordings panel
    ui.add_space(8.0);
}

fn welcome_item_ui(
    ctx: &AppContext<'_>,
    ui: &mut egui::Ui,
    recording_panel_data: &RecordingPanelData<'_>,
) {
    let item = Item::welcome_page();
    let selected = ctx.is_selected_or_loading(&item);
    let active = matches!(
        ctx.route(),
        Route::RedapServer(origin) if origin == &*EXAMPLES_ORIGIN
    );

    let title = list_item::LabelContent::header(tr("Welcome to rerun", "欢迎使用 Rerun")).with_icon(&icons::HOME);

    let list_item = ui.list_item().header().selected(selected).active(active);

    let response = if recording_panel_data.example_apps.is_empty() {
        list_item.show_flat(ui, title)
    } else {
        list_item
            .show_hierarchical_with_children(
                ui,
                egui::Id::new("example items"),
                true,
                title,
                |ui| {
                    for app_id_data in &recording_panel_data.example_apps {
                        app_id_section_ui(ctx, ui, app_id_data);
                    }
                },
            )
            .item_response
    };

    ctx.handle_select_hover_drag_interactions(&response, item.clone(), false);
    ctx.handle_select_focus_sync(&response, item);

    if response.clicked() {
        re_redap_browser::switch_to_welcome_screen(ctx.command_sender());
    }
}

// ---

fn server_title(ctx: &AppContext<'_>, origin: &re_uri::Origin, is_internal: bool) -> String {
    if is_internal {
        tr("Viewer catalog", "Viewer 目录").to_owned()
    } else {
        let host = origin.format_host();
        if origin.scheme == re_uri::Scheme::RerunHttps && origin.port == 443 {
            host
        } else if ctx.egui_ctx.is_test() {
            format!("{host}:XXXX")
        } else {
            format!("{host}:{}", origin.port)
        }
    }
}

fn server_section_ui(ctx: &AppContext<'_>, ui: &mut egui::Ui, server_data: &ServerData<'_>) {
    let ServerData {
        origin,
        is_active,
        is_selected,
        is_internal,
        entries_data,
    } = server_data;

    // We hide the section for the internal catalog, until we actually have data.
    // This mirrors the behavior of "Local" in the recording panel.
    if *is_internal && entries_data.iter_datasets().is_empty() {
        return;
    }

    let content = list_item::LabelContent::header(server_title(ctx, origin, *is_internal))
        .with_menu_button(&icons::MORE, tr("Actions", "操作"), move |ui| {
            for command in re_ui::RedapServerCommand::all_for_server(origin) {
                if command.requires_editable_server() && *is_internal {
                    continue;
                }
                command.menu_button_ui(ui, ctx.command_sender());
            }
        });

    let item_response = ui
        .list_item()
        .header()
        .selected(*is_selected)
        .active(*is_active)
        .show_hierarchical_with_children(ui, server_item_id(origin), true, content, |ui| {
            server_entries_ui(ctx, ui, entries_data, origin);
        })
        .item_response
        .on_hover_text(origin.to_string());

    ctx.handle_select_hover_drag_interactions(&item_response, server_data.item(), false);
    ctx.handle_select_focus_sync(&item_response, server_data.item());

    if item_response.clicked() {
        ctx.command_sender()
            .send_system(SystemCommand::SetRoute(Route::RedapServer(origin.clone())));
    }
}

fn server_entries_ui(
    ctx: &AppContext<'_>,
    ui: &mut egui::Ui,
    entries_data: &ServerEntriesData<'_>,
    origin: &re_uri::Origin,
) {
    match entries_data {
        ServerEntriesData::Loading => {
            ui.list_item_flat_noninteractive(list_item::CustomContent::new(|ui, _| {
                // TODO(emilk): ideally we should show this loading indicator left of the server name,
                // instead of the expand-chevron.
                ui.loading_indicator("Loading server entries");
            }));
        }

        ServerEntriesData::Error {
            message,
            is_auth_error,
        } => {
            let (label, color) = if *is_auth_error {
                (tr("Authentication required", "需要身份验证"), ui.visuals().weak_text_color())
            } else {
                (tr("Failed to load entries", "加载条目失败"), ui.visuals().error_fg_color)
            };
            ui.list_item_flat_noninteractive(list_item::LabelContent::new(
                egui::RichText::new(label).color(color),
            ))
            .on_hover_text(message);
        }

        ServerEntriesData::Loaded { entry_tree } => {
            for node in entry_tree {
                entry_tree_node_ui(ctx, ui, node, origin);
            }
        }
    }
}

fn entry_tree_node_ui(
    ctx: &AppContext<'_>,
    ui: &mut egui::Ui,
    node: &EntryTreeNode<'_>,
    origin: &re_uri::Origin,
) {
    match node {
        EntryTreeNode::Entry(ServerLeafEntry::Dataset(dataset)) => {
            dataset_entry_ui(ctx, ui, dataset);
        }
        EntryTreeNode::Entry(ServerLeafEntry::Table(table)) => {
            remote_table_entry_ui(ctx, ui, table);
        }
        EntryTreeNode::Entry(ServerLeafEntry::Failed(failed_entry)) => {
            failed_entry_ui(ctx, ui, failed_entry);
        }
        EntryTreeNode::Folder {
            name,
            path_prefix,
            children,
        } => {
            let item = Item::RedapEntry {
                origin: origin.clone(),
                kind: re_viewer_context::RedapEntryKind::Folder(path_prefix.clone()),
            };
            let route = Route::from_item(&item); // Will always be `Some`, but easy to be defensive.

            let is_selected = ctx.selection().contains_item(&item);
            let is_active = Some(ctx.route()) == route.as_ref();

            let content = list_item::LabelContent::new(name.as_str());
            let id = dataset_group_id(path_prefix);

            let item_response = ui
                .list_item()
                .selected(is_selected)
                .active(is_active)
                .show_hierarchical_with_children(ui, id, false, content, |ui| {
                    for child in children {
                        entry_tree_node_ui(ctx, ui, child, origin);
                    }
                })
                .item_response;

            ctx.handle_select_hover_drag_interactions(&item_response, item.clone(), false);
            ctx.handle_select_focus_sync(&item_response, item.clone());

            if item_response.clicked() {
                ctx.command_sender()
                    .send_system(SystemCommand::set_selection(item));
                if let Some(route) = route {
                    ctx.command_sender()
                        .send_system(SystemCommand::SetRoute(route));
                }
            }
        }
    }
}

fn dataset_entry_ui(ctx: &AppContext<'_>, ui: &mut egui::Ui, dataset_entry_data: &DatasetData<'_>) {
    let DatasetData {
        entry_data:
            EntryData {
                origin,
                entry_id,
                name,
                icon,
                is_selected,
                is_active,
            },
        displayed_segments,
    } = dataset_entry_data;

    let item = dataset_entry_data.entry_data.item();
    let list_item = ui.list_item().selected(*is_selected).active(*is_active);

    let mut list_item_content =
        re_ui::list_item::LabelContent::new(dataset_hierarchy_leaf_name(name.as_str()))
            .with_icon(icon);

    let id = ui.make_persistent_id(dataset_entry_data.entry_data.id());

    if !displayed_segments.is_empty() {
        list_item_content = list_item_content.with_buttons(|ui| {
            // Close-button:
            let resp = ui
                .small_icon_button(&icons::CLOSE_SMALL, tr("Close dataset", "关闭数据集"));

            if resp.clicked() {
                for db in displayed_segments.iter().filter_map(SegmentData::entity_db) {
                    ctx.command_sender()
                        .send_system(SystemCommand::CloseRecordingOrTable(
                            RecordingOrTable::Recording {
                                store_id: db.store_id().clone(),
                            },
                        ));
                }
            }
        });
    }

    let item_response = if !displayed_segments.is_empty() {
        list_item
            .show_hierarchical_with_children(ui, id, true, list_item_content, |ui| {
                for segment in displayed_segments {
                    match segment {
                        SegmentData::Loading { receiver } => receiver_ui(ctx, ui, receiver, true),

                        SegmentData::Loaded { entity_db } => {
                            let include_app_id = false; // we already show it in the parent item
                            let response = entity_db_button_ui(
                                ctx,
                                entity_db,
                                ui,
                                UiLayout::SelectionPanel,
                                include_app_id,
                            );
                            ctx.handle_select_focus_sync(
                                &response,
                                Item::StoreId(entity_db.store_id().clone()),
                            );
                        }
                    }
                }
            })
            .item_response
    } else {
        list_item.show_hierarchical(ui, list_item_content)
    };

    // Only request the hover card while the row itself is hovered: egui keeps an open
    // tooltip alive by rect-containment, which would otherwise suppress the tooltips of
    // the buttons sitting inside this row (egui `tooltip.rs` "big tooltip" carve-out).
    let item_response = if item_response.hovered() {
        item_response.on_hover_ui(|ui| {
            ui.label(trf!("Dataset: {name}", "数据集：{name}"));
        })
    } else {
        item_response
    };

    let new_route = Route::from(re_uri::EntryUri::new(origin.clone(), *entry_id));

    item_response.context_menu(|ui| {
        let url = ViewerOpenUrl::from_route(ctx.store_hub(), &new_route)
            .and_then(|url| url.sharable_url(None));
        if ui
            .add_enabled(url.is_ok(), egui::Button::new(tr("Copy link to dataset", "复制数据集链接")))
            .on_disabled_hover_text(tr("Can't copy a link to this dataset", "无法复制该数据集的链接"))
            .clicked()
            && let Ok(url) = url
        {
            ctx.command_sender()
                .send_system(SystemCommand::CopyViewerUrl(url));
        }

        if ui.button(tr("Copy dataset name", "复制数据集名称")).clicked() {
            re_log::info!("{}", trf!("Copied {name:?} to clipboard", "已把 {name:?} 复制到剪贴板"));
            ui.copy_text(name.to_string());
        }

        if ui.button(tr("Copy dataset id", "复制数据集 ID")).clicked() {
            let id = entry_id.id.to_string();
            re_log::info!("{}", trf!("Copied {id:?} to clipboard", "已把 {id:?} 复制到剪贴板"));
            ui.copy_text(id);
        }
    });

    ctx.handle_select_hover_drag_interactions(&item_response, item.clone(), false);
    ctx.handle_select_focus_sync(&item_response, item.clone());

    if item_response.clicked() {
        ctx.command_sender()
            .send_system(SystemCommand::set_selection(item));
        ctx.command_sender()
            .send_system(SystemCommand::SetRoute(new_route));
    }
}

fn remote_table_entry_ui(
    ctx: &AppContext<'_>,
    ui: &mut egui::Ui,
    remote_table_data: &RemoteTableData,
) {
    let RemoteTableData {
        entry_data:
            EntryData {
                origin: _,
                entry_id: _,
                name,
                icon,
                is_selected,
                is_active,
            },
    } = remote_table_data;

    let item = remote_table_data.entry_data.item();
    let text = RichText::new(dataset_hierarchy_leaf_name(name.as_str()));

    let list_item = ui.list_item().selected(*is_selected).active(*is_active);
    let list_item_content = LabelContent::new(text).with_icon(icon);
    let item_response = list_item.show_hierarchical(ui, list_item_content);

    ctx.handle_select_hover_drag_interactions(&item_response, item.clone(), false);
    ctx.handle_select_focus_sync(&item_response, item.clone());

    if item_response.clicked() {
        if let Some(route) = Route::from_item(&item) {
            ctx.command_sender()
                .send_system(SystemCommand::SetRoute(route));
        }
        ctx.command_sender()
            .send_system(SystemCommand::set_selection(item));
    }
}

fn failed_entry_ui(ctx: &AppContext<'_>, ui: &mut egui::Ui, failed_entry_data: &FailedEntryData) {
    let FailedEntryData {
        entry_data:
            EntryData {
                origin,
                entry_id,
                name,
                icon,
                is_selected,
                is_active,
            },
        error,
    } = failed_entry_data;

    let item = failed_entry_data.entry_data.item();
    let text = RichText::new(name.as_str()).color(ui.visuals().error_fg_color);

    let list_item = ui.list_item().selected(*is_selected).active(*is_active);
    let list_item_content = LabelContent::new(text).with_icon(icon);
    let item_response = list_item.show_hierarchical(ui, list_item_content);

    if item_response.clicked() {
        ctx.command_sender()
            .send_system(SystemCommand::set_selection(item));
        ctx.command_sender().send_system(SystemCommand::SetRoute(
            re_uri::EntryUri::new(origin.clone(), *entry_id).into(),
        ));
    }

    item_response.on_hover_text(error);
}

// ---

fn app_id_section_ui(ctx: &AppContext<'_>, ui: &mut egui::Ui, local_app_id: &AppIdData<'_>) {
    let AppIdData {
        app_id,
        display_name: _,
        is_active,
        is_selected,
        loaded_recordings,
    } = local_app_id;

    let item = local_app_id.item();
    let list_item = ui.list_item().selected(*is_selected).active(*is_active);

    // Remote dataset streams (TOS / Hugging Face) can be paused and resumed.
    let streaming = re_data_source::lerobot_remote::is_dataset_streaming(app_id.as_str());
    let paused = streaming && re_data_source::lerobot_remote::is_dataset_paused(app_id.as_str());

    // Make the paused state impossible to miss: the row itself says so (the resume button
    // alone only shows on hover, which reads as "downloads just stopped"). Prefixed, because
    // dataset names are long URLs and the panel truncates the tail.
    let name_text = if paused {
        trf!("⏸ paused · {}", "⏸ 已暂停 · {}", local_app_id.name())
    } else {
        local_app_id.name().to_owned()
    };

    let mut list_item_content =
        re_ui::list_item::LabelContent::new(name_text).with_icon(&icons::DATASET);

    let id = ui.make_persistent_id(local_app_id.id());

    // Diagnose in Daft: TOS datasets only (`diagnose_url` is `None` for anything
    // else) — an open TOS dataset is LeRobot v2/v3 by construction, the loader
    // rejects other formats before anything shows up here.
    // The app id is a normalized form of the dataset URL; resolve the real URL for the
    // link, and carry the bucket's region along — the console's connection inputs are
    // exactly URL + region, prefill both and the hand-off is one click.
    let dataset_url = re_data_source::lerobot_remote::dataset_url_of(app_id.as_str());
    let link_url = dataset_url.as_deref().unwrap_or_else(|| app_id.as_str());
    let region = re_data_source::lerobot_remote::dataset_region_of(link_url);
    let diagnose_url = re_viewer_context::daft_link::diagnose_url(link_url, region.as_deref());

    if !local_app_id.loaded_recordings.is_empty() || streaming {
        if paused {
            // Keep the resume button visible without hovering, so a paused dataset
            // shows how to get going again. Everything else (Diagnose, close) is
            // hover-only, like on the HF dataset rows.
            list_item_content = list_item_content.with_always_show_buttons(true);
        }
        list_item_content = list_item_content.with_buttons(move |ui| {
            if streaming {
                let (icon, tooltip) = if paused {
                    (&icons::PLAY, tr("Resume downloading this dataset", "继续下载该数据集"))
                } else {
                    (&icons::PAUSE, tr("Pause downloading this dataset", "暂停下载该数据集"))
                };
                // Secondary (gray) like the Diagnose button, so the row's buttons look alike.
                if ui
                    .add(re_ui::ReButton::icon(icon.clone()).secondary().small())
                    .on_hover_text(tooltip)
                    .clicked()
                {
                    re_data_source::lerobot_remote::set_dataset_paused(app_id.as_str(), !paused);
                }
            }

            // Close-button:
            let resp = ui
                .add(
                    re_ui::ReButton::icon(icons::CLOSE_SMALL.clone())
                        .secondary()
                        .small(),
                )
                .on_hover_text(tr("Close dataset", "关闭数据集"));

            if resp.clicked() {
                ctx.command_sender()
                    .send_system(SystemCommand::CloseApp(app_id.clone()));
            }

            // A labeled, always-visible button (not an icon): this is the entry point
            // into data curation, it should read as a feature, not as row furniture.
            // Buttons lay out right-to-left, so adding it last puts it leftmost —
            // in front of the row furniture, where a feature belongs.
            if let Some(url) = diagnose_url {
                let resp = ui
                    .add(
                        re_ui::ReButton::new(tr("Diagnose", "质检"))
                            .secondary()
                            .small(),
                    )
                    .on_hover_text(tr("Run data curation on this dataset", "对该数据集进行数据质检"));
                if resp.clicked() {
                    ui.open_url(egui::OpenUrl::new_tab(url));
                }
            }
        });
    }

    let mut item_response = if !loaded_recordings.is_empty() {
        list_item
            .show_hierarchical_with_children(ui, id, true, list_item_content, |ui| {
                let include_app_id = false; // we already show it in the parent item
                let episode_row = |ui: &mut egui::Ui, recording_data: &RecordingData<'_>| {
                    let response = entity_db_button_ui(
                        ctx,
                        recording_data.entity_db,
                        ui,
                        UiLayout::SelectionPanel,
                        include_app_id,
                    );
                    ctx.handle_select_focus_sync(
                        &response,
                        Item::StoreId(recording_data.entity_db.store_id().clone()),
                    );
                };

                // Hidden episodes move out of the working list into a collapsed group at
                // the bottom — out of the way, but one eye-click away from coming back.
                let (visible, hidden): (Vec<_>, Vec<_>) =
                    loaded_recordings.iter().partition(|recording_data| {
                        !re_viewer_context::hidden_recordings::is_hidden(
                            recording_data.entity_db.store_id(),
                        )
                    });

                for recording_data in visible {
                    episode_row(ui, recording_data);
                }

                if !hidden.is_empty() {
                    let hidden_id = ui.make_persistent_id(("hidden episodes", local_app_id.id()));
                    ui.list_item().show_hierarchical_with_children(
                        ui,
                        hidden_id,
                        false, // collapsed by default
                        re_ui::list_item::LabelContent::new(trf!(
                            "Hidden episodes ({})",
                            "已隐藏的 episode（{}）",
                            hidden.len()
                        ))
                        .with_icon(&icons::INVISIBLE)
                        .subdued(true),
                        |ui| {
                            for recording_data in hidden {
                                episode_row(ui, recording_data);
                            }
                        },
                    );
                }
            })
            .item_response
    } else {
        list_item.show_hierarchical(ui, list_item_content)
    };

    // See the dataset row above: only request the hover card while actually hovered,
    // so the row buttons' own tooltips are not suppressed.
    if item_response.hovered() {
        item_response = item_response.on_hover_ui(|ui| {
            app_id.app_ui(ctx, ui, UiLayout::Tooltip);
        });
    }

    // Whole-dataset artifact management (per-episode actions live on the episode rows).
    if streaming {
        let artifact_count =
            re_data_source::lerobot_remote::dataset_artifact_count(app_id.as_str());
        if artifact_count > 0
            && let Some(config) =
                re_data_source::lerobot_remote::dataset_artifacts_config(app_id.as_str())
        {
            item_response.context_menu(|ui| {
                let deleting =
                    re_data_source::rrd_artifacts::deletion_in_flight(app_id.as_str(), None);
                let no_permission =
                    re_data_source::rrd_artifacts::delete_permission(&config) == Some(false);
                if ui
                    .add_enabled(
                        !deleting && !no_permission,
                        egui::Button::new(trf!("Delete all rrd artifacts ({artifact_count})…", "删除全部 rrd 转换产物（{artifact_count}）…")),
                    )
                    .on_disabled_hover_text(if no_permission {
                        tr("These credentials have no delete permission", "当前凭证没有删除权限")
                    } else {
                        tr("Deletion in progress…", "正在删除…")
                    })
                    .clicked()
                {
                    // Only queues a request: the viewer shows a confirmation dialog first.
                    // Artifact directories mirror the real source URL, not the app id.
                    let dataset_url =
                        re_data_source::lerobot_remote::dataset_url_of(app_id.as_str())
                            .unwrap_or_else(|| app_id.to_string());
                    let dir = re_data_source::rrd_artifacts::dataset_artifacts_dir(
                        &config.location.prefix,
                        &dataset_url,
                    );
                    re_data_source::rrd_artifacts::request_deletion(
                        re_data_source::rrd_artifacts::ArtifactDeletionRequest {
                            application_id: app_id.to_string(),
                            dataset_url,
                            episode: None,
                            target_url: format!("tos://{}/{dir}", config.location.bucket),
                        },
                    );
                }
            });
        }
    }

    ctx.handle_select_hover_drag_interactions(&item_response, item.clone(), false);
    ctx.handle_select_focus_sync(&item_response, item);
    if list_item::ListItem::gained_focus_via_arrow_key(ui.ctx(), item_response.id) {
        ctx.command_sender()
            .send_system(SystemCommand::ActivateApp(app_id.clone()));
    }

    if item_response.clicked() {
        //TODO(ab): shouldn't this be done by handle_select_hover_drag_interactions?
        ctx.command_sender()
            .send_system(SystemCommand::ActivateApp(app_id.clone()));
    }
}

fn table_item_ui(ctx: &AppContext<'_>, ui: &mut egui::Ui, table_id: &TableId) {
    table_id_button_ui(ctx, ui, table_id, UiLayout::SelectionPanel);
}

fn loading_receivers_ui(
    ctx: &AppContext<'_>,
    ui: &mut egui::Ui,
    loading_receivers: &Vec<Arc<LogSource>>,
) {
    for receiver in loading_receivers {
        receiver_ui(ctx, ui, receiver, false);
    }
}

fn receiver_ui(
    ctx: &AppContext<'_>,
    ui: &mut egui::Ui,
    receiver: &LogSource,
    show_hierarchal: bool,
) {
    let Some(name) = receiver.loading_name() else {
        return;
    };

    let selected = ctx.is_selected_or_loading(&Item::DataSource(receiver.clone()));

    let label_content = re_ui::list_item::LabelContent::new(&name)
        .with_icon_fn(|ui, rect, visuals| {
            re_ui::loading_indicator::paint_loading_indicator_inside(
                ui,
                egui::Align2::CENTER_CENTER,
                rect,
                1.0,
                Some(visuals.text_color()),
                "Loading data source",
            );
        })
        .with_buttons(|ui| {
            let resp = ui
                .small_icon_button(&re_ui::icons::REMOVE, tr("Disconnect from this source", "断开与该数据来源的连接"));

            if resp.clicked() {
                ctx.connected_receivers.remove(receiver);
            }
        });

    let response = if show_hierarchal {
        ui.list_item()
            .selected(selected)
            .show_hierarchical(ui, label_content)
    } else {
        ui.list_item()
            .selected(selected)
            .show_flat(ui, label_content)
    };

    response.context_menu(|ui| {
        let url = ViewerOpenUrl::from_data_source(receiver).and_then(|url| url.sharable_url(None));
        if ui
            .add_enabled(url.is_ok(), egui::Button::new(tr("Copy link to segment", "复制分段链接")))
            .on_disabled_hover_text(if let Err(err) = url.as_ref() {
                trf!("Can't copy a link to this segment: {err}", "无法复制该分段的链接：{err}")
            } else {
                tr("Can't copy a link to this segment", "无法复制该分段的链接").to_owned()
            })
            .clicked()
            && let Ok(url) = url
        {
            ctx.command_sender()
                .send_system(SystemCommand::CopyViewerUrl(url));
        }

        if ui.button(tr("Copy segment name", "复制分段名称")).clicked() {
            re_log::info!("{}", trf!("Copied {name:?} to clipboard", "已把 {name:?} 复制到剪贴板"));
            ui.copy_text(name);
        }
    });
}

/// Force the server item and all ancestor folder nodes for `item` to be expanded.
///
/// Returns `true` once expansion completed (or the item doesn't need any), `false` if a
/// dependent lookup (entry name) failed because the dataset list isn't loaded yet — in
/// which case the caller should retry next frame.
fn expand_parents_for_item(
    egui_ctx: &egui::Context,
    recording_panel_data: &RecordingPanelData<'_>,
    item: &Item,
) -> bool {
    let force_open = |id| {
        let mut state = CollapsingState::load_with_default_open(egui_ctx, id, true);
        state.set_open(true);
        state.store(egui_ctx);
    };

    let expand_folder_path = |path: &str| {
        for ancestor in ancestor_folder_paths(path) {
            force_open(dataset_group_id(ancestor));
        }
    };

    match item {
        Item::RedapServer(origin) => {
            force_open(server_item_id(origin));
            true
        }
        Item::RedapEntry { origin, kind } => {
            force_open(server_item_id(origin));
            match kind {
                RedapEntryKind::Folder(path) => {
                    expand_folder_path(path);
                    true
                }
                RedapEntryKind::Entry(entry_id) => expand_parent_folders_for_entry(
                    egui_ctx,
                    recording_panel_data,
                    origin,
                    *entry_id,
                ),
            }
        }
        _ => true,
    }
}

fn server_item_id(origin: &re_uri::Origin) -> egui::Id {
    egui::Id::new(origin).with("server_item")
}

fn dataset_group_id(path_prefix: &str) -> egui::Id {
    egui::Id::new("dataset_group").with(path_prefix)
}

fn expand_parent_folders_for_entry(
    egui_ctx: &egui::Context,
    data: &RecordingPanelData<'_>,
    origin: &re_uri::Origin,
    entry_id: re_log_types::EntryId,
) -> bool {
    for server in &data.servers {
        if &server.origin != origin {
            continue;
        }
        let ServerEntriesData::Loaded { entry_tree } = &server.entries_data else {
            return false;
        };

        expand_folder_nodes_containing_entry(egui_ctx, entry_tree, entry_id);
        return true;
    }

    true
}

fn expand_folder_nodes_containing_entry(
    egui_ctx: &egui::Context,
    nodes: &[EntryTreeNode<'_>],
    entry_id: re_log_types::EntryId,
) -> bool {
    for node in nodes {
        match node {
            EntryTreeNode::Entry(entry) => {
                if entry.entry_id() == entry_id {
                    return true;
                }
            }
            EntryTreeNode::Folder {
                path_prefix,
                children,
                ..
            } => {
                if expand_folder_nodes_containing_entry(egui_ctx, children, entry_id) {
                    let mut state = CollapsingState::load_with_default_open(
                        egui_ctx,
                        dataset_group_id(path_prefix),
                        true,
                    );
                    state.set_open(true);
                    state.store(egui_ctx);
                    return true;
                }
            }
        }
    }
    false
}

/// Iterate every ancestor folder path of a dotted hierarchy `path`, shallowest first,
/// including `path` itself. `"a.b.c"` → `"a"`, `"a.b"`, `"a.b.c"`. Allocation-free.
fn ancestor_folder_paths(path: &str) -> impl Iterator<Item = &str> {
    std::iter::chain(
        path.match_indices(re_uri::DATASET_HIERARCHY_SEPARATOR)
            .map(|(idx, _)| &path[..idx]),
        std::iter::once(path),
    )
    .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::ancestor_folder_paths;

    #[test]
    fn ancestor_folder_paths_basic() {
        let collect = |p| ancestor_folder_paths(p).collect::<Vec<_>>();
        assert_eq!(collect("a.b.c"), vec!["a", "a.b", "a.b.c"]);
        assert_eq!(collect("a"), vec!["a"]);
        assert_eq!(collect(""), Vec::<&str>::new());
    }
}
