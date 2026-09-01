use re_i18n::{tr, trf};
use std::fmt::Write as _;

use egui::NumExt as _;
use jiff::SignedDuration;
use jiff::fmt::friendly::{FractionalUnit, SpanPrinter};
use re_chunk_store::ChunkStoreConfig;
use re_entity_db::{EntityDb, entity_db::RedapConnectionState};
use re_format::{format_bytes, format_uint};
use re_log_channel::LogSource;
use re_log_types::StoreKind;
use re_ui::UiExt as _;
use re_viewer_context::{AppContext, UiLayout};

use crate::item_ui::{app_id_button_ui, data_source_button_ui};

impl crate::AppUi for EntityDb {
    fn app_ui(&self, ctx: &AppContext<'_>, ui: &mut egui::Ui, ui_layout: UiLayout) {
        re_tracing::profile_function!();

        if ui_layout.is_single_line() {
            // TODO(emilk): standardize this formatting with that in `entity_db_button_ui` (this is
            // probably dead code, as `entity_db_button_ui` is actually used in all single line
            // contexts).
            let mut string = self.store_id().recording_id().to_string();
            if let Some(data_source) = &self.data_source {
                write!(string, ", {data_source}").ok();
            }
            write!(string, ", {}", self.store_id().application_id()).ok();

            ui.label(string);
            return;
        }

        egui::Grid::new("entity_db").num_columns(2).show(ui, |ui| {
            grid_content_ui(ctx, self, ui, ui_layout);
        });

        let hub = ctx.store_hub();

        match self.store_kind() {
            StoreKind::Recording => {}

            StoreKind::Blueprint => {
                if let Some(active_app_id) = ctx.active_store_context.map(|sc| sc.application_id())
                {
                    let is_active_app_id = self.application_id() == active_app_id;

                    if is_active_app_id {
                        let is_default = hub.default_blueprint_id_for_app(active_app_id)
                            == Some(self.store_id());
                        let is_active =
                            hub.active_blueprint_id_for_app(active_app_id) == Some(self.store_id());

                        match (is_default, is_active) {
                            (false, false) => {}
                            (true, false) => {
                                ui.add_space(8.0);
                                ui.label(
                                    tr("This is the default blueprint for the current application.", "这是当前应用的默认 blueprint。"),
                                );

                                if let Some(active_blueprint) =
                                    hub.active_blueprint_for_app(active_app_id)
                                    && active_blueprint.cloned_from() == Some(self.store_id())
                                {
                                    // The active blueprint is a clone of the selected blueprint.
                                    if self.latest_row_id() == active_blueprint.latest_row_id() {
                                        ui.label(
                                            tr("The active blueprint is a clone of this blueprint.", "当前生效的 blueprint 是这个 blueprint 的克隆。"),
                                        );
                                    } else {
                                        ui.label(tr("The active blueprint is a modified clone of this blueprint.", "当前生效的 blueprint 是这个 blueprint 的克隆，并有改动。"));
                                    }
                                }
                            }
                            (false, true) => {
                                ui.add_space(8.0);
                                ui.label(trf!("This is the active blueprint for the current application, '{active_app_id}'", "这是当前应用 '{active_app_id}' 正在生效的 blueprint"));
                            }
                            (true, true) => {
                                ui.add_space(8.0);
                                ui.label(trf!("This is both the active and default blueprint for the current application, '{active_app_id}'", "这既是当前应用 '{active_app_id}' 正在生效的 blueprint，也是其默认 blueprint"));
                            }
                        }
                    } else {
                        ui.add_space(8.0);
                        ui.label(tr("This blueprint is not for the active application", "这个 blueprint 不属于当前活跃的应用"));
                    }
                }
            }
        }

        #[cfg(debug_assertions)]
        if !ctx.is_test {
            let title = re_ui::debug_only::with_debug_only_badge(ui.style(), "Debug info");
            ui.collapsing_header(title, true, |ui| {
                debug_ui(ui, self);
            });
        }
    }
}

fn grid_content_ui(ctx: &AppContext<'_>, db: &EntityDb, ui: &mut egui::Ui, ui_layout: UiLayout) {
    re_tracing::profile_function!();

    {
        ui.grid_left_hand_label(&format!("{} ID", db.store_id().kind()));
        ui.label(db.store_id().recording_id().to_string());
        ui.end_row();
    }

    if let Some(LogSource::RedapGrpcStream {
        uri: re_uri::DatasetSegmentUri { segment_id, .. },
        ..
    }) = &db.data_source
    {
        ui.grid_left_hand_label(tr("Segment ID", "片段 ID"));
        ui.label(segment_id.to_string());
        ui.end_row();
    }

    if let Some(store_info) = db.store_info()
        && ui_layout.is_selection_panel()
    {
        let re_log_types::StoreInfo {
            store_id,
            cloned_from,
            store_source,
            store_version,
        } = store_info;

        if let Some(cloned_from) = cloned_from {
            ui.grid_left_hand_label(tr("Clone of", "克隆自"));
            crate::item_ui::store_id_button_ui(ctx, ui, cloned_from, ui_layout);
            ui.end_row();
        }

        ui.grid_left_hand_label(tr("Application ID", "应用 ID"));
        app_id_button_ui(ctx, ui, store_id.application_id());
        ui.end_row();

        ui.grid_left_hand_label(tr("Source", "来源"));
        ui.label(store_source.to_string());
        ui.end_row();

        if let Some(store_version) = store_version {
            ui.grid_left_hand_label(tr("Source RRD version", "来源 RRD 版本"));
            ui.label(store_version.to_string());
            ui.end_row();
        } else {
            re_log::trace_once!("store version is undefined for this recording, this is a bug");
        }

        ui.grid_left_hand_label(tr("Kind", "类型"));
        ui.label(store_id.kind().to_string());
        ui.end_row();
    }

    let show_last_modified_time = !ctx.is_test;
    // Hide in tests because it is non-deterministic (it's based on `RowId`).
    if show_last_modified_time
        && let Some(latest_row_id) = db.latest_row_id()
        && let Ok(nanos_since_epoch) = i64::try_from(latest_row_id.nanos_since_epoch())
    {
        let time = re_log_types::Timestamp::from_nanos_since_epoch(nanos_since_epoch);
        ui.grid_left_hand_label(tr("Modified", "修改时间"));
        ui.label(time.format(ctx.app_options.timestamp_format));
        ui.end_row();
    }

    if let Some(tl_name) = db
        .timelines()
        .keys()
        .find(|k| **k == re_log_types::TimelineName::log_time())
        && let Some(range) = db.time_range_for(tl_name)
        && let delta_ns = (range.max() - range.min()).as_i64()
        && delta_ns > 0
    {
        let duration = SignedDuration::from_nanos(delta_ns);

        let printer = SpanPrinter::new()
            .fractional(Some(FractionalUnit::Second))
            .precision(Some(2));

        let pretty = printer.duration_to_string(&duration);

        ui.grid_left_hand_label(tr("Duration", "时长"));
        ui.label(pretty)
            .on_hover_text(tr("Duration between earliest and latest log_time.", "最早与最晚 log_time 之间的时长。"));
        ui.end_row();
    }

    {
        ui.grid_left_hand_label(tr("Size", "大小"));

        let current_size_bytes = db.byte_size_of_physical_chunks();
        let full_size_bytes = if db.rrd_manifest_index().has_manifest() {
            db.rrd_manifest_index()
                .full_uncompressed_size()
                .at_least(current_size_bytes)
        } else {
            current_size_bytes
        };

        ui.label(format_bytes(full_size_bytes as _)).on_hover_text(
            "在内存中的大致占用（解压后）。\n\
            在下方 Streams 面板中把鼠标悬停到某个实体上，\
            可以查看单个实体的大小。",
        );
        ui.end_row();

        if db.rrd_manifest_index().has_manifest() {
            ui.grid_left_hand_label("已下载");

            let memory_limit = ctx.app_options.memory_limit;
            let max_downloaded_bytes = if db.rrd_manifest_index().is_fully_loaded() {
                full_size_bytes
            } else {
                u64::min(full_size_bytes, memory_limit.as_bytes())
            };

            let current_size = format_bytes(current_size_bytes as _);
            let max_downloaded = format_bytes(max_downloaded_bytes as _);

            let mut num_root_chunks = 0_usize;
            let mut num_fully_loaded = 0_usize;
            for info in db.rrd_manifest_index().root_chunks() {
                num_root_chunks += 1;
                if info.is_fully_loaded() {
                    num_fully_loaded += 1;
                }
            }

            ui.horizontal(|ui| {
                if db.redap_connection_state() == RedapConnectionState::PartialManifest {
                    ui.label(format!("{current_size} / ?"));
                    ui.label(trf!("({} / ? chunks)", "（{} / ? 个 chunk）", format_uint(num_fully_loaded)));
                    ui.end_row();
                } else if num_fully_loaded == num_root_chunks {
                    ui.label("100%");
                } else {
                    ui.label(format!("{current_size} / {max_downloaded}"));

                    if max_downloaded_bytes < full_size_bytes {
                        let rect =
                            ui.small_icon(&re_ui::icons::INFO, Some(ui.visuals().text_color()));

                        ui.allocate_rect(rect, egui::Sense::hover())
                            .on_hover_text(trf!(
                                "Download limited to {memory_limit} memory budget",
                                "受 {memory_limit} 内存预算限制，不会全部下载"
                            ));
                    }

                    ui.label(trf!(
                        "({} / {} chunks)",
                        "（{} / {} 个 chunk）",
                        format_uint(num_fully_loaded),
                        format_uint(num_root_chunks)
                    ));
                    ui.end_row();
                }
            });

            ui.end_row();

            // ----

            if 0 < num_root_chunks {
                ui.grid_left_hand_label("平均 chunk 大小")
                    .on_hover_text("远端上的大小");
                let avg_chunk_size_bytes = full_size_bytes as f64 / num_root_chunks as f64;
                ui.label(format_bytes(avg_chunk_size_bytes));
                ui.end_row();
            }
        }
    }

    {
        // Stats like number of columns, rows, etc

        let storage_engine = db.storage_engine();
        let store = storage_engine.store();
        let schema = store.schema().chunk_column_descriptors();

        ui.grid_left_hand_label("实体")
            .on_hover_text("位于 ChunkStore 中的实体数");
        ui.label(re_format::format_uint(store.all_entities().len()));
        ui.end_row();

        ui.grid_left_hand_label("时间轴列");
        ui.label(re_format::format_uint(schema.indices.len()));
        ui.end_row();

        ui.grid_left_hand_label("数据列");
        ui.label(re_format::format_uint(schema.components.len()));
        ui.end_row();

        ui.grid_left_hand_label("行数");
        ui.label(re_format::format_uint(store.stats().total().num_rows));
        ui.end_row();
    }

    if ui_layout.is_selection_panel() {
        let &ChunkStoreConfig {
            enable_changelog: _,
            chunk_max_bytes,
            chunk_max_rows,
            chunk_max_rows_if_unsorted,
        } = db.storage_engine().store().config();

        ui.grid_left_hand_label("chunk 合并配置");
        ui.label(trf!(
            "{} rows ({} if unsorted) or {}",
            "{} 行（未排序时 {} 行）或 {}",
            re_format::format_uint(chunk_max_rows),
            re_format::format_uint(chunk_max_rows_if_unsorted),
            re_format::format_bytes(chunk_max_bytes as _),
        ))
            .on_hover_text(
                unindent::unindent(&format!("\
                    当前录制文件的 chunk 合并配置为：不断合并 chunk，\
                    直到达到 {chunk_max_rows} 行（未排序时 {chunk_max_rows_if_unsorted} 行）或 {chunk_max_bytes} 上限，以先到者为准。

                    Viewer 会在数据到达时把 chunk 合并到一起，\
                    以便在存储空间和计算开销之间取得平衡。
                    这与 SDK 的批处理器（batcher）不同：后者在记录端（SDK）做类似的工作，\
                    但目标和约束不一样。
                    这两个功能（SDK 批处理器和 Viewer 合并器）互为补充。

                    阈值越高，通常空间开销越小，但摄入和查询都需要更多计算。
                    阈值越低，通常空间开销越大，但摄入更快、查询响应更及时。
                    以上只是粗略的概括 — 拿不准就用默认值，默认值适合大多数场景。

                    要修改当前配置，请在启动 Viewer 前设置以下环境变量：
                    * {ENV_CHUNK_MAX_ROWS}
                    * {ENV_CHUNK_MAX_ROWS_IF_UNSORTED}
                    * {ENV_CHUNK_MAX_BYTES}

                    这个合并过程只是 Rerun Viewer 在内存中的临时优化，\
                    不会改动录制文件本身：如果想持久化合并结果（让后续打开更快），\
                    请使用 Viewer 的“保存”命令或 `rerun rrd optimize` 命令行工具。
                    ",
                        chunk_max_rows = re_format::format_uint(chunk_max_rows),
                        chunk_max_rows_if_unsorted = re_format::format_uint(chunk_max_rows_if_unsorted),
                        chunk_max_bytes = re_format::format_bytes(chunk_max_bytes as _),
                        ENV_CHUNK_MAX_ROWS = ChunkStoreConfig::ENV_CHUNK_MAX_ROWS,
                        ENV_CHUNK_MAX_ROWS_IF_UNSORTED = ChunkStoreConfig::ENV_CHUNK_MAX_ROWS_IF_UNSORTED,
                        ENV_CHUNK_MAX_BYTES = ChunkStoreConfig::ENV_CHUNK_MAX_BYTES,
                )),
            );
        ui.end_row();
    }

    if let Some(data_source) = &db.data_source
        && ui_layout.is_selection_panel()
    {
        ui.grid_left_hand_label("数据源");
        data_source_button_ui(ctx, ui, data_source);
        ui.end_row();
    }
}

#[cfg(debug_assertions)]
fn debug_ui(ui: &mut egui::Ui, db: &EntityDb) {
    egui::Grid::new("debug-info").show(ui, |ui| {
        if let Some(manifest) = db.rrd_manifest_index().manifest() {
            ui.label("实体");
            ui.label(format_uint(
                manifest.recording_schema().all_entities().len(),
            ));
            ui.end_row();
        }

        ui.label("is_buffering");
        ui.label(db.is_buffering().to_string());
        ui.end_row();

        ui.label("连接");
        ui.label(format!("{:?}", db.redap_connection_state())); // NOLINT: debug-only UI
        ui.end_row();

        ui.label("物理 chunk 数");
        ui.label(format_bytes(db.byte_size_of_physical_chunks() as _));
        ui.end_row();
    });
}
