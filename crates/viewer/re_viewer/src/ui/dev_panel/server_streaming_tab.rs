use egui_plot::HoverPosition;
use re_byte_size::SizeBytes as _;
use re_chunk_store::Chunk;
use re_format::{format_bytes, format_uint};
use re_log_types::EntityPath;
use re_ui::UiExt as _;
use re_ui::list_item;
use re_viewer_context::StorageContext;
use std::collections::BTreeSet;

use super::plot_utils::history_to_plot;
use super::streaming_history::StreamingHistory;

pub fn server_streaming_tab_ui(
    ui: &mut egui::Ui,
    storage_context: &StorageContext<'_>,
    streaming_history: &StreamingHistory,
) {
    ui.request_repaint();

    let streaming_recordings: Vec<_> = storage_context
        .bundle
        .recordings()
        .filter(|r| r.can_fetch_chunks_from_redap())
        .collect();

    // Two-column layout: details on left, plots on right
    let available = ui.available_size();
    let left_width = (available.x * 0.2).max(150.0);

    ui.horizontal(|ui| {
        // Left column: per-recording details
        ui.allocate_ui_with_layout(
            egui::vec2(left_width, available.y),
            egui::Layout::top_down(egui::Align::LEFT),
            |ui| {
                egui::ScrollArea::vertical()
                    .auto_shrink(false)
                    .show(ui, |ui| {
                        if streaming_recordings.is_empty() {
                            ui.label("没有活跃的服务器流式读取连接。");
                        } else {
                            list_item::list_item_scope(ui, "streaming_recordings", |ui| {
                                for recording in &streaming_recordings {
                                    recording_ui(ui, recording, storage_context.hub);
                                }
                            });
                        }
                    });
            },
        );

        ui.separator();

        // Right column: three side-by-side plots (always shown for history)
        ui.vertical(|ui| {
            streaming_plots(ui, streaming_history);
        });
    });
}

fn recording_ui(
    ui: &mut egui::Ui,
    recording: &re_entity_db::EntityDb,
    hub: &re_viewer_context::StoreHub,
) {
    let store_id = recording.store_id();
    let chunk_requests = recording.rrd_manifest_index().chunk_requests();
    let is_active = chunk_requests.has_pending();

    let header = list_item::LabelContent::new(store_id.recording_id().to_string()).with_icon_fn(
        |ui, rect, visuals| {
            if is_active {
                re_ui::loading_indicator::paint_loading_indicator_inside(
                    ui,
                    egui::Align2::CENTER_CENTER,
                    rect,
                    1.0,
                    Some(visuals.text_color()),
                    "正在下载 chunk",
                );
            } else {
                ui.painter()
                    .circle_filled(rect.center(), rect.width() / 4.0, visuals.text_color());
            }
        },
    );

    ui.list_item()
        .interactive(false)
        .show_hierarchical_with_children(
            ui,
            ui.make_persistent_id(store_id),
            false,
            header,
            |ui| {
                recording_details_ui(ui, recording, hub.was_preview(store_id));
            },
        );
}

fn recording_details_ui(ui: &mut egui::Ui, recording: &re_entity_db::EntityDb, is_preview: bool) {
    status_ui(ui, recording);
    progress_ui(ui, recording);
    chunks_removed_ui(ui, recording);
    manifest_ui(ui, recording);
    prioritization_ui(ui, recording, is_preview);
    pending_requests_ui(ui, recording);
    in_flight_entities_ui(ui, recording);
}

fn status_ui(ui: &mut egui::Ui, recording: &re_entity_db::EntityDb) {
    ui.list_item_flat_noninteractive(
        list_item::PropertyContent::new("连接")
            .value_text(format!("{:?}", recording.redap_connection_state())),
    )
    .on_hover_text("与 Redap 服务器的连接状态");
}

#[derive(Default)]
struct Progress {
    loaded_bytes: u64,
    total_bytes: u64,
    loaded_count: usize,
    total_count: usize,
}

impl Progress {
    fn add(&mut self, size: u64, is_loaded: bool) {
        self.total_bytes += size;
        self.total_count += 1;
        if is_loaded {
            self.loaded_bytes += size;
            self.loaded_count += 1;
        }
    }

    fn value_text(&self) -> String {
        let pct = if self.total_bytes > 0 {
            format!(
                "{:.1}%",
                self.loaded_bytes as f64 / self.total_bytes as f64 * 100.0
            )
        } else {
            "—".to_owned()
        };
        format!(
            "{} / {}（{pct}，{} / {} 个 chunk）",
            format_bytes(self.loaded_bytes as _),
            format_bytes(self.total_bytes as _),
            format_uint(self.loaded_count),
            format_uint(self.total_count),
        )
    }
}

fn progress_ui(ui: &mut egui::Ui, recording: &re_entity_db::EntityDb) {
    let manifest_index = recording.rrd_manifest_index();

    if let Some(manifest) = manifest_index.manifest() {
        let uncompressed_sizes = manifest.col_chunk_byte_size_uncompressed();

        let mut overall = Progress::default();
        for info in manifest_index.root_chunks() {
            overall.add(uncompressed_sizes[info.row_id], info.is_fully_loaded());
        }
        ui.list_item_flat_noninteractive(
            list_item::PropertyContent::new("总体").value_text(overall.value_text()),
        )
        .on_hover_text(
            "已完整加载的根 chunk 相对于 manifest 声明的整个录制文件的占比。",
        );

        let protected_roots = &manifest_index.chunk_prioritizer().protected_chunks().roots;
        if !protected_roots.is_empty() {
            let mut target = Progress::default();
            #[expect(clippy::iter_over_hash_type, reason = "order-independent sum")]
            for id in protected_roots {
                if let Some(info) = manifest_index.root_chunk_info(id) {
                    target.add(uncompressed_sizes[info.row_id], info.is_fully_loaded());
                }
            }
            ui.list_item_flat_noninteractive(
                list_item::PropertyContent::new("已加载的受保护根 chunk")
                    .value_text(target.value_text()),
            )
            .on_hover_text("受保护的根 chunk 不会被垃圾回收。chunk 通常在被活跃使用时受保护。");
        }
    }

    let bw_text = manifest_index
        .chunk_requests()
        .bandwidth()
        .map_or_else(|| "—".to_owned(), |bw| format!("{}/s", format_bytes(bw)));
    ui.list_item_flat_noninteractive(
        list_item::PropertyContent::new("带宽").value_text(bw_text),
    )
    .on_hover_text("近期平均下载速度（按传输中的压缩字节计）");
}

fn chunks_removed_ui(ui: &mut egui::Ui, recording: &re_entity_db::EntityDb) {
    use super::chunk_event_stats::ChunkEventStats;
    let stats = ChunkEventStats::for_store(recording.store_id());
    let total_removed = stats.num_chunks_gc
        + stats.num_chunks_split_cleanup
        + stats.num_chunks_compacted
        + stats.num_chunks_overwritten;
    ui.list_item()
        .interactive(false)
        .show_hierarchical_with_children(
            ui,
            ui.make_persistent_id(("chunks_removed", recording.store_id())),
            false,
            list_item::PropertyContent::new("已移除的 chunk")
                .value_text(format_uint(total_removed)),
            |ui| {
                ui.list_item_flat_noninteractive(
                    list_item::PropertyContent::new("垃圾回收")
                        .value_text(format_uint(stats.num_chunks_gc)),
                )
                .on_hover_text("因内存压力被清理");

                ui.list_item_flat_noninteractive(
                    list_item::PropertyContent::new("拆分清理")
                        .value_text(format_uint(stats.num_chunks_split_cleanup)),
                )
                .on_hover_text("根 chunk 重新下载后，被移除的旧拆分 chunk");

                ui.list_item_flat_noninteractive(
                    list_item::PropertyContent::new("压实（compaction）")
                        .value_text(format_uint(stats.num_chunks_compacted)),
                )
                .on_hover_text("chunk 被压实后的版本替换");

                ui.list_item_flat_noninteractive(
                    list_item::PropertyContent::new("覆盖")
                        .value_text(format_uint(stats.num_chunks_overwritten)),
                )
                .on_hover_text("静态 chunk 被更新的值覆盖");
            },
        );
}

fn manifest_ui(ui: &mut egui::Ui, recording: &re_entity_db::EntityDb) {
    let manifest_index = recording.rrd_manifest_index();
    let Some(manifest) = manifest_index.manifest() else {
        return;
    };

    ui.list_item_collapsible_noninteractive_label("Manifest（chunk 清单）", false, |ui| {
        let num_chunks = manifest.num_chunks();
        let num_static = manifest.col_chunk_is_static().filter(|s| *s).count();
        let num_temporal = num_chunks.saturating_sub(num_static);
        ui.list_item()
            .interactive(false)
            .show_hierarchical_with_children(
                ui,
                ui.make_persistent_id(("manifest_chunks", recording.store_id())),
                false,
                list_item::PropertyContent::new("Chunk 数").value_text(format_uint(num_chunks)),
                |ui| {
                    ui.list_item_flat_noninteractive(
                        list_item::PropertyContent::new("静态")
                            .value_text(format_uint(num_static)),
                    );
                    ui.list_item_flat_noninteractive(
                        list_item::PropertyContent::new("时序")
                            .value_text(format_uint(num_temporal)),
                    );
                },
            );

        let num_entities = manifest
            .col_chunk_entity_path_raw()
            .iter()
            .flatten()
            .collect::<BTreeSet<_>>()
            .len();
        ui.list_item_flat_noninteractive(
            list_item::PropertyContent::new("实体数").value_text(format_uint(num_entities)),
        );

        ui.list_item_flat_noninteractive(
            list_item::PropertyContent::new("Manifest 大小（内存中）")
                .value_text(format_bytes(manifest.total_size_bytes() as _)),
        )
        .on_hover_text("manifest（所有 chunk 的索引）在内存中的大小");
        ui.list_item_flat_noninteractive(
            list_item::PropertyContent::new("索引大小（内存中）")
                .value_text(format_bytes(manifest_index.total_size_bytes() as _)),
        )
        .on_hover_text(
            "manifest 加上派生索引（排序后的 chunk、已加载范围、优先级器状态等）在内存中的大小",
        );

        let compressed: u64 = manifest.col_chunk_byte_size().iter().sum();
        let uncompressed: u64 = manifest.col_chunk_byte_size_uncompressed().iter().sum();
        ui.list_item_flat_noninteractive(
            list_item::PropertyContent::new("录制文件未压缩/压缩大小").value_text(
                format!(
                    "{} / {}",
                    format_bytes(uncompressed as _),
                    format_bytes(compressed as _)
                ),
            ),
        )
        .on_hover_text(
            "manifest 声明的 chunk 大小之和，未压缩与存储中大小的对比。\
             后端不压缩 chunk 时，存储中大小等于未压缩大小。",
        );
    });
}

fn prioritization_ui(ui: &mut egui::Ui, recording: &re_entity_db::EntityDb, is_preview: bool) {
    let manifest_index = recording.rrd_manifest_index();

    ui.list_item_collapsible_noninteractive_label("优先级", false, |ui| {
        ui.list_item_flat_noninteractive(
            list_item::PropertyContent::new("预览").value_bool(is_preview),
        )
        .on_hover_text(
            "上一帧中这个录制文件是否以预览形式渲染 — 预览使用更严格的垃圾回收预算",
        );

        if let Some(prio) = manifest_index.chunk_prioritizer().latest_result() {
            ui.list_item_flat_noninteractive(
                list_item::PropertyContent::new("传输预算已满")
                    .value_text(prio.transit_budget_filled.to_string()),
            )
            .on_hover_text(
                "这个录制文件的在途传输预算已耗尽 — 无法再增加并发下载",
            );

            ui.list_item_flat_noninteractive(
                list_item::PropertyContent::new("内存预算已满")
                    .value_text(prio.memory_budget_filled.to_string()),
            )
            .on_hover_text(
                "这个录制文件的内存预算已耗尽 — 不清理就无法加载更多 chunk",
            );

            let all_required = match prio.all_required_are_loaded {
                Some(v) => v.to_string(),
                None => "未知".to_owned(),
            };
            ui.list_item_flat_noninteractive(
                list_item::PropertyContent::new("必需项已全部加载").value_text(all_required),
            )
            .on_hover_text(
                "所有必需 chunk（静态、缺失、高优先级）是否都已加载或在传输中",
            );
        } else {
            ui.list_item_flat_noninteractive(
                list_item::LabelContent::new("还没有获取数据").weak(true),
            );
        }

        let protected = manifest_index.chunk_prioritizer().protected_chunks();

        let roots_text = if let Some(manifest) = manifest_index.manifest() {
            let sizes = manifest.col_chunk_byte_size_uncompressed();
            let roots_bytes: u64 = protected
                .roots
                .iter()
                .filter_map(|id| manifest_index.root_chunk_info(id))
                .map(|info| sizes[info.row_id])
                .sum();
            Progress {
                loaded_bytes: roots_bytes,
                total_bytes: manifest_index.full_uncompressed_size(),
                loaded_count: protected.roots.len(),
                total_count: manifest_index.root_chunks().count(),
            }
            .value_text()
        } else {
            format_uint(protected.roots.len())
        };
        ui.list_item_flat_noninteractive(
            list_item::PropertyContent::new("受保护的根 chunk").value_text(roots_text),
        )
        .on_hover_text(
            "不会被取消下载的根 chunk（当前查询需要）相对于 manifest 中所有根 chunk 的占比。\
             按未压缩的 Arrow 字节计。",
        );

        let store = recording.storage_engine();
        let store = store.store();
        let (num_physical, physical_bytes) = protected
            .physical
            .iter()
            .filter_map(|id| store.physical_chunk(id))
            .fold((0_usize, 0), |(count, bytes), chunk| {
                (count + 1, bytes + Chunk::total_size_bytes(chunk.as_ref()))
            });
        let physical_progress = Progress {
            loaded_bytes: physical_bytes,
            total_bytes: recording.byte_size_of_physical_chunks(),
            loaded_count: num_physical,
            total_count: recording.num_physical_chunks(),
        };
        ui.list_item_flat_noninteractive(
            list_item::PropertyContent::new("受保护的物理 chunk")
                .value_text(physical_progress.value_text()),
        )
        .on_hover_text(
            "不会被垃圾回收的物理 chunk（正被查询使用）相对于内存中所有已加载物理 chunk 的占比。\
             按内存中 Chunk 大小计。",
        );
    });
}

fn pending_requests_ui(ui: &mut egui::Ui, recording: &re_entity_db::EntityDb) {
    let chunk_requests = recording.rrd_manifest_index().chunk_requests();
    let pending = chunk_requests.pending_requests();
    let num_chunks: usize = pending.iter().map(|r| r.row_indices.len()).sum();
    let recently_canceled: usize = chunk_requests
        .recently_canceled
        .iter()
        .map(|(_t, count)| count)
        .sum();

    ui.list_item_collapsible_noninteractive_label("待处理请求", false, |ui| {
        ui.list_item_flat_noninteractive(
            list_item::PropertyContent::new("批次数").value_text(format_uint(pending.len())),
        )
        .on_hover_text("发往服务器、仍在途中的请求批次数量");

        ui.list_item_flat_noninteractive(
            list_item::PropertyContent::new("Chunk 数").value_text(format_uint(num_chunks)),
        )
        .on_hover_text("所有待处理批次包含的 chunk 总数");

        ui.list_item_flat_noninteractive(
            list_item::PropertyContent::new("最近取消")
                .value_text(format_uint(recently_canceled)),
        )
        .on_hover_text("最近一秒内取消的批次（例如因为时间标记移动）");
    });
}

fn in_flight_entities_ui(ui: &mut egui::Ui, recording: &re_entity_db::EntityDb) {
    let manifest_index = recording.rrd_manifest_index();
    let pending = manifest_index.chunk_requests().pending_requests();

    let mut entities = BTreeSet::<EntityPath>::new();
    if let Some(manifest) = manifest_index.manifest() {
        let col = manifest.col_chunk_entity_path_raw();
        for request in &pending {
            for &row_idx in &request.row_indices {
                entities.insert(EntityPath::parse_forgiving(col.value(row_idx)));
            }
        }
    }

    // Manual `show_hierarchical_with_children` because label includes entity count —
    // use a stable ID independent of label text.
    ui.list_item()
        .interactive(false)
        .show_hierarchical_with_children(
            ui,
            ui.make_persistent_id(("entities", recording.store_id())),
            false,
            list_item::LabelContent::new(format!("传输中的实体（{}）", entities.len())),
            |ui| {
                for entity in &entities {
                    ui.list_item_flat_noninteractive(list_item::LabelContent::new(
                        entity.to_string(),
                    ));
                }
            },
        );
}

fn streaming_plots(ui: &mut egui::Ui, history: &StreamingHistory) {
    re_tracing::profile_function!();

    let cancel_color = ui.visuals().error_fg_color;
    let removed_color = ui.visuals().warn_fg_color;
    let inflight_color = ui.tokens().success_text_color;

    let now = re_memory::util::sec_since_start();

    let following_id = ui.make_persistent_id("streaming_plots_following");
    let mut following = ui.data_mut(|d| *d.get_persisted_mut_or(following_id, true));

    let axis_group = egui::Id::new("streaming_axis");
    let cursor_group = egui::Id::new("streaming_cursor");

    /// Common plot setup shared by all streaming plots.
    fn base_plot(id: &str, axis_group: egui::Id, cursor_group: egui::Id) -> egui_plot::Plot<'_> {
        egui_plot::Plot::new(id)
            .min_size(egui::Vec2::splat(100.0))
            .x_axis_formatter(|time, _| format!("{} s", time.value))
            .show_x(false)
            .legend(egui_plot::Legend::default().position(egui_plot::Corner::LeftTop))
            .include_x(0.0)
            .include_y(0.0)
            .link_axis(axis_group, [true, false])
            .link_cursor(cursor_group, [true, false])
    }

    /// Apply sliding window if following, then check response for user interaction.
    fn show_plot(
        plot: egui_plot::Plot<'_>,
        ui: &mut egui::Ui,
        following: &mut bool,
        now: f64,
        add_lines: impl FnOnce(&mut egui_plot::PlotUi<'_>),
    ) {
        const WINDOW_SECS: f64 = 10.0;

        let resp = plot.show(ui, |plot_ui| {
            if *following {
                plot_ui.set_plot_bounds_x(now - WINDOW_SECS..=now);
            }
            add_lines(plot_ui);
        });
        let r = &resp.response;
        if r.dragged() || (r.hovered() && ui.input(|i| i.smooth_scroll_delta.x != 0.0)) {
            *following = false;
        }
        if r.double_clicked() {
            *following = true;
        }
    }

    ui.columns(3, |columns| {
        columns[0].label("进度");
        show_plot(
            base_plot("streaming_progress", axis_group, cursor_group)
                .label_formatter(|hover_position| match hover_position {
                    HoverPosition::NearDataPoint {
                        plot_name,
                        position,
                        ..
                    } => Some(format!("{plot_name}: {}", format_bytes(position.y))),
                    HoverPosition::Elsewhere { position } => Some(format_bytes(position.y)),
                })
                .y_axis_formatter(|bytes, _| format_bytes(bytes.value)),
            &mut columns[0],
            &mut following,
            now,
            |plot_ui| {
                plot_ui.line(history_to_plot("已加载", &history.loaded_bytes).width(1.5));
                plot_ui.line(
                    history_to_plot("Manifest 声明总量", &history.total_manifest_bytes)
                        .width(1.5)
                        .style(egui_plot::LineStyle::dashed_dense()),
                );
            },
        );

        columns[1].label("吞吐量");
        show_plot(
            base_plot("streaming_throughput", axis_group, cursor_group)
                .label_formatter(|hover_position| match hover_position {
                    HoverPosition::NearDataPoint {
                        plot_name,
                        position,
                        ..
                    } => Some(format!("{plot_name}: {}", format_bytes(position.y))),
                    HoverPosition::Elsewhere { position } => Some(format_bytes(position.y)),
                })
                .y_axis_formatter(|bytes, _| format_bytes(bytes.value)),
            &mut columns[1],
            &mut following,
            now,
            |plot_ui| {
                plot_ui.line(
                    history_to_plot("带宽（B/s）", &history.bandwidth_bytes_per_sec).width(1.5),
                );
                plot_ui.line(history_to_plot("待下载字节", &history.pending_bytes).width(1.5));
            },
        );

        columns[2].label("计数");
        show_plot(
            base_plot("streaming_counts", axis_group, cursor_group).label_formatter(
                |hover_position| match hover_position {
                    HoverPosition::NearDataPoint {
                        plot_name,
                        position,
                        ..
                    } => Some(format!("{plot_name}: {:.0}", position.y)),
                    HoverPosition::Elsewhere { position } => Some(format!("{:.0}", position.y)),
                },
            ),
            &mut columns[2],
            &mut following,
            now,
            |plot_ui| {
                plot_ui.line(
                    history_to_plot("传输中的 chunk", &history.chunks_in_flight)
                        .width(1.5)
                        .color(inflight_color),
                );
                plot_ui.line(
                    history_to_plot("取消数", &history.batch_cancellations)
                        .width(1.5)
                        .color(cancel_color),
                );
                plot_ui.line(
                    history_to_plot("被垃圾回收的 chunk", &history.chunks_gc_per_frame)
                        .width(1.5)
                        .color(removed_color),
                );
            },
        );
    });

    ui.data_mut(|d| d.insert_persisted(following_id, following));
}
