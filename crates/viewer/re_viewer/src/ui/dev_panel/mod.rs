mod chunk_event_stats;
mod memory_history;
mod plot_utils;
mod server_streaming_tab;
mod streaming_history;
mod transform_cache_ui;

use re_i18n::{tr, trf};
use ahash::HashMap;
use egui_plot::HoverPosition;
use plot_utils::history_to_plot;
use re_chunk_store::{ChunkStoreChunkStats, ChunkStoreConfig, ChunkStoreStats};
use re_entity_db::StoreBundle;
use re_format::{format_bytes, format_uint};
use re_log_types::StoreId;
use re_memory::MemoryLimit;
use re_memory::util::sec_since_start;
use re_query::{QueryCacheStats, QueryCachesStats};
use re_renderer::WgpuResourcePoolStatistics;
use re_ui::UiExt as _;
use re_viewer_context::store_hub::StoreHubStats;
use re_viewer_context::{ActiveStoreContext, StorageContext, TimeControl};

use crate::env_vars::RERUN_TRACK_ALLOCATIONS;
use memory_history::MemoryHistory;
use streaming_history::StreamingHistory;

// ----------------------------------------------------------------------------

/// Which view to show in the dev panel.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, strum_macros::EnumIter)]
enum DevPanelTab {
    #[default]
    Flamegraph,

    TimeGraph,

    Stores,

    Streaming,

    AllocationTracking,

    Gpu,

    TransformCache,
}

impl DevPanelTab {
    fn label(&self) -> &'static str {
        match self {
            Self::Flamegraph => tr("Flamegraph", "内存火焰图"),
            Self::TimeGraph => tr("Over time", "随时间变化"),
            Self::Stores => tr("Recordings", "录制文件"),
            Self::Streaming => tr("Server streaming", "服务器流式读取"),
            Self::AllocationTracking => tr("Allocation tracking", "内存分配追踪"),
            Self::Gpu => "GPU",
            Self::TransformCache => tr("Transform cache", "变换缓存"),
        }
    }
}

#[derive(Default)]
pub struct DevPanel {
    history: MemoryHistory,
    streaming_history: StreamingHistory,
    memory_purge_times: Vec<f64>,
    selected_tab: DevPanelTab,
    include_rss_in_flamegraph: bool,
    transform_cache_state: transform_cache_ui::TransformCacheUiState,
}

#[derive(Default)]
pub struct DevPanelResponse {
    pub close_requested: bool,
    pub repaint_requested: bool,
}

impl DevPanel {
    /// Call once per frame
    pub fn update(
        &mut self,
        gpu_resource_stats: &WgpuResourcePoolStatistics,
        store_stats: Option<&StoreHubStats>,
        store_bundle: Option<&StoreBundle>,
    ) {
        re_tracing::profile_function!();

        // Ensure GC counter subscriber is registered (idempotent via OnceLock).
        chunk_event_stats::ChunkEventStats::subscription_handle();

        self.history.capture(Some(gpu_resource_stats), store_stats);
        if let Some(store_bundle) = store_bundle {
            self.streaming_history.capture(store_bundle);
        }
    }

    /// Note that we purged memory at this time, to show in stats.
    #[inline]
    pub fn note_memory_purge(&mut self) {
        self.memory_purge_times.push(sec_since_start());
    }

    pub fn ui(
        &mut self,
        ui: &mut egui::Ui,
        limit: &MemoryLimit,
        mem_usage_tree: Option<re_byte_size::NamedMemUsageTree>,
        external_trees: &[re_byte_size::NamedMemUsageTree],
        gpu_resource_stats: &WgpuResourcePoolStatistics,
        store_stats: Option<&StoreHubStats>,
        store_context: Option<&ActiveStoreContext<'_>>,
        time_controls: &HashMap<StoreId, TimeControl>,
        storage_context: &StorageContext<'_>,
    ) -> DevPanelResponse {
        re_tracing::profile_function!();

        // We show realtime stats, so keep showing the latest!
        // Specific dev panel tabs can opt-out of this below, for resource efficiency if it's not necessary.
        let mut request_repaint = true;

        ui.add_space(4.0);

        // Tab selector at the top
        let ((), close_clicked) = egui::Sides::new().shrink_left().show(
            ui,
            |ui| {
                use strum::IntoEnumIterator as _;
                for tab in DevPanelTab::iter() {
                    ui.selectable_value(&mut self.selected_tab, tab, tab.label());
                }
            },
            |ui| {
                ui.small_icon_button(&re_ui::icons::CLOSE, tr("Close dev panel", "关闭开发者面板"))
                    .clicked()
            },
        );

        ui.separator();

        match self.selected_tab {
            DevPanelTab::Flamegraph => {
                memory_tree_ui(
                    ui,
                    mem_usage_tree,
                    external_trees,
                    &mut self.include_rss_in_flamegraph,
                );
            }
            DevPanelTab::TimeGraph => {
                ui.label(tr("🗠 Rerun Viewer memory use over time", "🗠 Rerun Viewer 内存占用随时间的变化"));
                self.plot(ui, limit);
            }
            DevPanelTab::Stores => {
                egui::ScrollArea::vertical()
                    .auto_shrink(false)
                    .show(ui, |ui| {
                        Self::store_stats_ui(ui, store_stats);
                    });
            }
            DevPanelTab::TransformCache => {
                self.transform_cache_ui(ui, store_context, time_controls, storage_context);
                // Normal repaint behavior of the viewer is enough for the transform debugger,
                // no need to constantly trigger an expensive repaint.
                request_repaint = false;
            }
            DevPanelTab::Streaming => {
                server_streaming_tab::server_streaming_tab_ui(
                    ui,
                    storage_context,
                    &self.streaming_history,
                );
            }
            DevPanelTab::AllocationTracking => {
                egui::ScrollArea::vertical()
                    .auto_shrink(false)
                    .show(ui, |ui| {
                        Self::allocation_tracking_ui(ui);
                    });
            }
            DevPanelTab::Gpu => {
                egui::ScrollArea::vertical()
                    .auto_shrink(false)
                    .show(ui, |ui| {
                        Self::gpu_stats(ui, gpu_resource_stats);
                    });
            }
        }

        DevPanelResponse {
            close_requested: close_clicked,
            repaint_requested: request_repaint,
        }
    }

    fn transform_cache_ui(
        &mut self,
        ui: &mut egui::Ui,
        store_context: Option<&ActiveStoreContext<'_>>,
        time_controls: &HashMap<StoreId, TimeControl>,
        storage_context: &StorageContext<'_>,
    ) {
        // Keep the tab from reporting a tiny content height when it only has a warning label,
        // without imposing a fixed minimum size on the resizable dev panel.
        ui.set_min_height(ui.available_height());

        let Some(store_context) = store_context else {
            ui.warning_label(tr("No active recording selected for the transform cache.", "没有选中活跃的录制文件，无法查看变换缓存。"));
            return;
        };

        let query = time_controls
            .get(store_context.recording.store_id())
            .and_then(|time_ctrl| {
                // Pending timelines do not have resolved timeline metadata yet, so avoid issuing a
                // query that cannot match the viewer's current timepoint.
                time_ctrl.timeline()?;
                Some(time_ctrl.current_query())
            });

        transform_cache_ui::ui(
            ui,
            store_context.recording,
            storage_context,
            query,
            &mut self.transform_cache_state,
        );
    }

    fn store_stats_ui(ui: &mut egui::Ui, store_stats: Option<&StoreHubStats>) {
        if let Some(store_stats) = store_stats {
            for (store_id, store_stats) in &store_stats.store_stats {
                let title = format!("{} {}", store_id.kind(), store_id.recording_id());
                ui.collapsing_header(&title, false, |ui| {
                    if let Some(data_source) = &store_stats.store_source {
                        ui.weak(trf!("Source: {data_source}", "数据源：{data_source}"));

                        ui.separator();
                    }
                    ui.collapsing(tr("Datastore Resources", "数据存储资源"), |ui| {
                        Self::chunk_store_stats(
                            ui,
                            &store_stats.store_config,
                            &store_stats.store_stats,
                        );
                    });

                    ui.separator();
                    ui.collapsing(tr("Primary Query Caches", "主查询缓存"), |ui| {
                        Self::caches_stats(ui, &store_stats.query_cache_stats);
                    });

                    ui.separator();
                    ui.collapsing(tr("Viewer Caches", "Viewer 缓存"), |ui| {
                        ui.label(format!(
                            "GPU 内存：{}",
                            format_bytes(store_stats.cache_vram_usage.size_bytes() as f64)
                        ));

                        // TODO(emilk): in the future we could have a VRAM flamegraph here
                    });
                });
            }
        } else {
            ui.label(tr("No store statistics available.", "没有可用的存储统计信息。"));
        }
    }

    fn allocation_tracking_ui(ui: &mut egui::Ui) {
        let mut is_tracking_callstacks = re_memory::accounting_allocator::is_tracking_callstacks();
        ui.re_checkbox(
            &mut is_tracking_callstacks,
            tr("Enable detailed allocation tracking", "启用详细的内存分配追踪"),
        )
        .on_hover_text(tr("This will slow down the program", "这会拖慢程序"));
        re_memory::accounting_allocator::set_tracking_callstacks(is_tracking_callstacks);

        ui.add_space(8.0);

        if let Some(tracking_stats) = re_memory::accounting_allocator::tracking_stats() {
            ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Extend);
            Self::tracking_stats(ui, tracking_stats);
        } else if !cfg!(target_arch = "wasm32") {
            ui.label(format!(
                "设置 {RERUN_TRACK_ALLOCATIONS}=1 可从启动起就进行详细的内存分配追踪。"
            ));
        }
    }

    fn gpu_stats(ui: &mut egui::Ui, gpu_resource_stats: &WgpuResourcePoolStatistics) {
        ui.strong(tr("GPU Resources", "GPU 资源"));
        ui.separator();

        egui::Grid::new("gpu resource grid")
            .num_columns(2)
            .show(ui, |ui| {
                let WgpuResourcePoolStatistics {
                    num_bind_group_layouts,
                    num_pipeline_layouts,
                    num_render_pipelines,
                    num_samplers,
                    num_shader_modules,
                    num_bind_groups,
                    num_buffers,
                    num_textures,
                    total_buffer_size_in_bytes,
                    total_texture_size_in_bytes,
                } = gpu_resource_stats;

                ui.label(tr("# Bind Group Layouts:", "Bind group layout 数："));
                ui.label(num_bind_group_layouts.to_string());
                ui.end_row();
                ui.label(tr("# Pipeline Layouts:", "Pipeline layout 数："));
                ui.label(num_pipeline_layouts.to_string());
                ui.end_row();
                ui.label(tr("# Render Pipelines:", "渲染管线数："));
                ui.label(num_render_pipelines.to_string());
                ui.end_row();
                ui.label(tr("# Samplers:", "采样器数："));
                ui.label(num_samplers.to_string());
                ui.end_row();
                ui.label(tr("# Shader Modules:", "着色器模块数："));
                ui.label(num_shader_modules.to_string());
                ui.end_row();
                ui.label(tr("# Bind Groups:", "Bind group 数："));
                ui.label(num_bind_groups.to_string());
                ui.end_row();
                ui.label(tr("# Buffers:", "缓冲区数："));
                ui.label(num_buffers.to_string());
                ui.end_row();
                ui.label(tr("# Textures:", "纹理数："));
                ui.label(num_textures.to_string());
                ui.end_row();
                ui.label(tr("Buffer Memory:", "缓冲区内存："));
                ui.label(re_format::format_bytes(*total_buffer_size_in_bytes as _));
                ui.end_row();
                ui.label(tr("Texture Memory:", "纹理内存："));
                ui.label(re_format::format_bytes(*total_texture_size_in_bytes as _));
                ui.end_row();
            });
    }

    fn chunk_store_stats(
        ui: &mut egui::Ui,
        store_config: &ChunkStoreConfig,
        store_stats: &ChunkStoreStats,
    ) {
        // TODO(cmc): this will become useful again once we introduce compaction settings.
        _ = store_config;

        egui::Grid::new("store stats grid 2")
            .num_columns(3)
            .show(ui, |ui| {
                let ChunkStoreStats {
                    static_chunks,
                    temporal_chunks,
                } = *store_stats;

                ui.label(egui::RichText::new(tr("Stats", "统计")).italics());
                ui.label(tr("Chunks", "Chunk 数"));
                ui.label(tr("Rows (total)", "行数（总计）"));
                ui.label(tr("Events (total)", "事件数（总计）"))
                    .on_hover_text(tr("Number of non-null component batches (cells)", "非空组件批次（单元格）的数量"));
                ui.label(tr("Size (total)", "大小（总计）"));
                ui.end_row();

                fn label_chunk_stats(ui: &mut egui::Ui, stats: ChunkStoreChunkStats) {
                    let ChunkStoreChunkStats {
                        num_chunks,
                        total_size_bytes,
                        num_rows,
                        num_events,
                    } = stats;

                    ui.label(re_format::format_uint(num_chunks));
                    ui.label(re_format::format_uint(num_rows));
                    ui.label(re_format::format_uint(num_events));
                    ui.label(re_format::format_bytes(total_size_bytes as _));
                }

                ui.label(tr("Static:", "静态："));
                label_chunk_stats(ui, static_chunks);
                ui.end_row();

                ui.label(tr("Temporal:", "时序："));
                label_chunk_stats(ui, temporal_chunks);
                ui.end_row();

                ui.label(tr("Total:", "总计："));
                label_chunk_stats(ui, static_chunks + temporal_chunks);
                ui.end_row();
            });
    }

    fn caches_stats(ui: &mut egui::Ui, caches_stats: &QueryCachesStats) {
        let QueryCachesStats { latest_at, range } = caches_stats;

        if !latest_at.is_empty() {
            ui.separator();
            ui.strong("LatestAt");
            egui::ScrollArea::vertical()
                .max_height(200.0)
                .id_salt("latest_at")
                .show(ui, |ui| {
                    egui::Grid::new("latest_at cache stats grid")
                        .num_columns(3)
                        .show(ui, |ui| {
                            ui.label(egui::RichText::new(tr("Entity", "实体")).underline());
                            ui.label(egui::RichText::new(tr("Component", "组件")).underline());
                            ui.label(egui::RichText::new(tr("Chunks", "Chunk 数")).underline())
                                .on_hover_text(tr("How many chunks in the cache?", "缓存里有多少个 chunk？"));
                            ui.label(egui::RichText::new(tr("Effective size", "名义大小")).underline())
                                .on_hover_text(tr("What would be the size of this cache in the worst case, i.e. if all chunks had been fully copied?", "最坏情况下（即所有 chunk 都被完整复制时）这个缓存会有多大？"));
                            ui.label(egui::RichText::new(tr("Actual size", "实际大小")).underline())
                                .on_hover_text(tr("What is the actual size of this cache after deduplication?", "去重之后这个缓存实际有多大？"));
                            ui.end_row();

                            for (cache_key, stats) in latest_at {
                                let &QueryCacheStats {
                                    total_chunks,
                                    total_effective_size_bytes,
                                    total_actual_size_bytes,
                                } = stats;

                                ui.label(cache_key.entity_path.ui_string());
                                ui.label(cache_key.component.to_string());
                                ui.label(re_format::format_uint(total_chunks));
                                ui.label(re_format::format_bytes(total_effective_size_bytes as _));
                                ui.label(re_format::format_bytes(total_actual_size_bytes as _));
                                ui.end_row();
                            }
                        });
                });
        }

        if !range.is_empty() {
            ui.separator();
            ui.strong("Range");
            egui::ScrollArea::vertical()
                .max_height(200.0)
                .id_salt("range")
                .show(ui, |ui| {
                    egui::Grid::new("range cache stats grid")
                        .num_columns(4)
                        .show(ui, |ui| {
                            ui.label(egui::RichText::new(tr("Entity", "实体")).underline());
                            ui.label(egui::RichText::new(tr("Component", "组件")).underline());
                            ui.label(egui::RichText::new(tr("Chunks", "Chunk 数")).underline())
                                .on_hover_text(tr("How many chunks in the cache?", "缓存里有多少个 chunk？"));
                            ui.label(egui::RichText::new(tr("Effective size", "名义大小")).underline())
                                .on_hover_text(tr("What would be the size of this cache in the worst case, i.e. if all chunks had been fully copied?", "最坏情况下（即所有 chunk 都被完整复制时）这个缓存会有多大？"));
                            ui.label(egui::RichText::new(tr("Actual size", "实际大小")).underline())
                                .on_hover_text(tr("What is the actual size of this cache after deduplication?", "去重之后这个缓存实际有多大？"));
                            ui.end_row();

                            for (cache_key, stats) in range {
                                let &QueryCacheStats {
                                    total_chunks,
                                    total_effective_size_bytes,
                                    total_actual_size_bytes,
                                } = stats;

                                ui.label(cache_key.entity_path.ui_string());
                                ui.label(cache_key.component.to_string());
                                ui.label(re_format::format_uint(total_chunks));
                                ui.label(re_format::format_bytes(total_effective_size_bytes as _));
                                ui.label(re_format::format_bytes(total_actual_size_bytes as _));
                                ui.end_row();
                            }
                        });
                });
        }
    }

    fn tracking_stats(
        ui: &mut egui::Ui,
        tracking_stats: re_memory::accounting_allocator::TrackingStatistics,
    ) {
        ui.label("counted = fully_tracked + stochastically_tracked + untracked + overhead");
        ui.label(format!(
            "fully_tracked（完整追踪）：{}，共 {} 次分配",
            format_bytes(tracking_stats.fully_tracked.size as _),
            format_uint(tracking_stats.fully_tracked.count),
        ));
        ui.label(format!(
            "stochastically_tracked（随机采样追踪）：{}，共 {} 次分配",
            format_bytes(tracking_stats.stochastically_tracked.size as _),
            format_uint(tracking_stats.stochastically_tracked.count),
        ));
        ui.label(format!(
            "untracked（未追踪）：{}，共 {} 次分配（均小于 {}）",
            format_bytes(tracking_stats.untracked.size as _),
            format_uint(tracking_stats.untracked.count),
            format_bytes(tracking_stats.track_size_threshold as _),
        ));
        ui.label(format!(
            "overhead（开销）：{}，共 {} 次分配",
            format_bytes(tracking_stats.overhead.size as _),
            format_uint(tracking_stats.overhead.count),
        ))
        .on_hover_text(tr("Used for the book-keeping of the allocation tracker", "分配追踪器自身记账所用的内存"));

        egui::CollapsingHeader::new(tr("Top memory consumers", "内存占用大户"))
            .default_open(true)
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .max_height(300.0)
                    .show(ui, |ui| {
                        ui.set_min_width(750.0);
                        for callstack in tracking_stats.top_callstacks {
                            let stochastic_rate = callstack.stochastic_rate;
                            let is_stochastic = stochastic_rate > 1;

                            let text = format!(
                                "{}{}，共 {} 次分配（每次约 {}）{} - {}",
                                if is_stochastic { "≈" } else { "" },
                                format_bytes((callstack.extant.size * stochastic_rate) as _),
                                format_uint(callstack.extant.count * stochastic_rate),
                                format_bytes(
                                    callstack.extant.size as f64 / callstack.extant.count as f64
                                ),
                                if stochastic_rate <= 1 {
                                    String::new()
                                } else {
                                    format!("（{} 个随机采样）", callstack.extant.count)
                                },
                                summarize_callstack(&callstack.readable_backtrace.to_string())
                            );

                            if ui
                                .button(text)
                                .on_hover_text(tr("Click to copy callstack to clipboard", "点击把调用栈复制到剪贴板"))
                                .clicked()
                            {
                                let mut text = callstack.readable_backtrace.to_string();
                                if text.is_empty() {
                                    // This is weird
                                    text = tr("No callstack available", "没有可用的调用栈").to_owned();
                                }
                                ui.copy_text(text);
                            }
                        }
                    });
            });
    }

    fn plot(&self, ui: &mut egui::Ui, limit: &MemoryLimit) {
        re_tracing::profile_function!();

        let ram_purge_color = ui.visuals().warn_fg_color;

        egui_plot::Plot::new("mem_history_plot")
            .min_size(egui::Vec2::splat(200.0))
            .label_formatter(|hover_position| match hover_position {
                HoverPosition::NearDataPoint {
                    plot_name,
                    position,
                    ..
                } => Some(format!("{plot_name}: {}", format_bytes(position.y))),
                HoverPosition::Elsewhere { position } => Some(format_bytes(position.y)),
            })
            .x_axis_formatter(|time, _| format!("{} s", time.value))
            .y_axis_formatter(|bytes, _| format_bytes(bytes.value))
            .show_x(false)
            .legend(egui_plot::Legend::default().position(egui_plot::Corner::LeftTop))
            .include_y(0.0)
            // TODO(emilk): turn off plot interaction, and always do auto-sizing
            .show(ui, |plot_ui| {
                if limit.is_limited() {
                    plot_ui
                        .hline(egui_plot::HLine::new(tr("Limit", "上限"), limit.as_bytes() as f64).width(2.0));
                }

                for &time in &self.memory_purge_times {
                    plot_ui.vline(
                        egui_plot::VLine::new(tr("RAM purge", "内存清理"), time)
                            .color(ram_purge_color)
                            .width(2.0),
                    );
                }

                let MemoryHistory {
                    resident,
                    counted_allocator,
                    counted_vram,
                    counted_blueprints,
                    counted_recordings,
                    counted_query_caches,
                    counted_table_stores,
                } = &self.history;

                plot_ui.line(history_to_plot(tr("Resident", "常驻内存（RSS）"), resident).width(1.5));
                plot_ui.line(history_to_plot(tr("Allocator", "分配器统计"), counted_allocator).width(1.5));
                plot_ui.line(history_to_plot(tr("VRAM", "显存（VRAM）"), counted_vram).width(1.5));
                plot_ui.line(history_to_plot(tr("Recordings", "录制文件"), counted_recordings).width(1.5));

                if false {
                    // Intentionally omitted because they are uninteresting and clutter things up too much
                    plot_ui.line(history_to_plot("Blueprint", counted_blueprints).width(1.5));
                    plot_ui.line(history_to_plot(tr("Query caches", "查询缓存"), counted_query_caches).width(1.5));
                    plot_ui.line(history_to_plot(tr("Table stores", "表格存储"), counted_table_stores).width(1.5));
                }
            });
    }
}

fn summarize_callstack(callstack: &str) -> String {
    let patterns = [
        ("App::receive_messages", "App::receive_messages"),
        ("w_store::store::ComponentBucket>::archive", "archive"),
        ("ChunkStore>::insert", "ChunkStore"),
        ("EntityDb", "EntityDb"),
        ("EntityTree", "EntityTree"),
        ("::LogMsg>::deserialize", "LogMsg"),
        ("::TimePoint>::deserialize", "TimePoint"),
        ("ImageCache", "ImageCache"),
        ("gltf", "gltf"),
        ("tokio::sync::broadcast::channel", "channel"),
        ("grpc", "grpc"),
        ("image::image", "image"),
        ("ImageDecodeCache", "ImageDecodeCache"),
        ("epaint::text::text_layout", "text_layout"),
        ("egui_wgpu", "egui_wgpu"),
        ("decode_arrow", "decode_arrow"),
        ("transform_resolution_cache", "transform_resolution_cache"),
        ("wgpu_hal", "wgpu_hal"),
        ("prepare_staging_buffer", "prepare_staging_buffer"),
        // -----
        // Very general:
        ("crossbeam::channel::Sender", "crossbeam::channel::Sender"),
        ("epaint::texture_atlas", "egui font texture"),
        ("alloc::collections::btree", "BTree"),
        ("std::collections::hash::map::HashMap<K,V,S>", "HashMap"),
    ];

    let mut all_summaries = vec![];

    for (pattern, summary) in patterns {
        if callstack.contains(pattern) {
            all_summaries.push(summary);
        }
    }

    all_summaries.join(", ")
}

pub fn memory_tree_ui(
    ui: &mut egui::Ui,
    tree: Option<re_byte_size::NamedMemUsageTree>,
    external_trees: &[re_byte_size::NamedMemUsageTree],
    include_rss: &mut bool,
) {
    // Add explanation at the top
    ui.horizontal(|ui| {
        ui.label(tr("Memory flamegraph visualizing the memory usage tree.", "内存火焰图，可视化内存占用树。"));
        ui.hyperlink_to(
            tr("Learn more", "了解更多"),
            "https://docs.rs/re_byte_size/latest/re_byte_size/trait.MemUsageTreeCapture.html",
        );

        #[expect(dead_code)]
        fn foo(_: &dyn re_byte_size::MemUsageTreeCapture) {
            // This function is only here so we remember to update the link above if the trait name changes.
        }
    });

    ui.label(tr("Double-click to reset view, scroll to zoom, drag to pan.", "双击重置视图，滚轮缩放，拖动平移。"));

    ui.re_checkbox(include_rss, tr("Include RSS", "包含 RSS"))
        .on_hover_text(tr("Include Resident Set Size (RSS) in the flamegraph. This shows total memory use as reported by the OS. This may be a lot bigger than what is actually _used_ because our allocator (mimalloc) retains pages in case they are needed again.", "在火焰图中包含常驻内存（RSS），即操作系统报告的总内存占用。它可能远大于实际使用量，因为我们的分配器（mimalloc）会保留内存页以备复用。"));

    ui.separator();

    let Some(mut tree) = tree else {
        ui.label(tr("No memory usage tree available.", "没有可用的内存占用树。"));
        return;
    };

    let re_memory::MemoryUse { resident, counted } = re_memory::MemoryUse::capture();

    let include_counted = true; // What our allocator counts. Perfectly accurate.

    if include_counted && let Some(counted) = counted {
        let mut node = re_byte_size::MemUsageNode::new().with_named_child(tree);

        for tree in external_trees {
            node = node.with_named_child(tree.clone());
        }

        tree = re_byte_size::NamedMemUsageTree::new("counted", node.with_total_size_bytes(counted));
    }

    if *include_rss && let Some(resident) = resident {
        tree = re_byte_size::NamedMemUsageTree::new(
            "RSS",
            re_byte_size::MemUsageNode::new()
                .with_named_child(tree)
                .with_total_size_bytes(resident),
        );
    }

    egui::ScrollArea::vertical()
        .auto_shrink(false)
        .show(ui, |ui| {
            re_memory_view::memory_flamegraph_ui(ui, &tree);
        });
}
