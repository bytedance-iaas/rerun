use re_i18n::tr;
use std::str::FromStr as _;

use egui::{NumExt as _, Ui};
use re_entity_db::FetchStage;
use re_log_types::{Timestamp, TimestampFormat};
use re_memory::MemoryLimit;
use re_ui::syntax_highlighting::SyntaxHighlightedBuilder;
use re_ui::{DesignTokens, UiExt as _};
use re_viewer_context::{AppOptions, ExperimentalAppOptions, VideoOptions};

pub fn settings_screen_ui(ui: &mut egui::Ui, app_options: &mut AppOptions, keep_open: &mut bool) {
    egui::Frame {
        inner_margin: egui::Margin::same(5),
        ..Default::default()
    }
    .show(ui, |ui| {
        const MAX_WIDTH: f32 = 600.0;
        const MIN_WIDTH: f32 = 300.0;

        let centering_margin = ((ui.available_width() - MAX_WIDTH) / 2.0).at_least(0.0);
        let max_rect = ui.max_rect().expand2(-centering_margin * egui::Vec2::X);
        let mut child_ui = ui.new_child(egui::UiBuilder::new().max_rect(max_rect));

        egui::ScrollArea::both()
            .auto_shrink(false)
            .show(&mut child_ui, |ui| {
                ui.set_min_width(MIN_WIDTH);
                settings_screen_ui_impl(ui, app_options, keep_open);
            });

        if ui.input_mut(|ui| ui.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
            *keep_open = false;
        }
    });
}

fn settings_screen_ui_impl(ui: &mut egui::Ui, app_options: &mut AppOptions, keep_open: &mut bool) {
    //
    // Title
    //

    ui.add_space(40.0);

    ui.horizontal(|ui| {
        ui.add(egui::Label::new(
            egui::RichText::new(tr("Settings", "设置"))
                .strong()
                .line_height(Some(32.0))
                .text_style(DesignTokens::welcome_screen_h2()),
        ));

        ui.allocate_ui_with_layout(
            egui::Vec2::X * ui.available_width(),
            egui::Layout::right_to_left(egui::Align::Center),
            |ui| {
                if ui
                    .small_icon_button(&re_ui::icons::CLOSE, tr("Close", "关闭"))
                    .clicked()
                {
                    *keep_open = false;
                }
            },
        )
    });

    //
    // General
    //

    separator_with_some_space(ui);

    ui.strong(tr("General", "通用"));

    ui.horizontal(|ui| {
        ui.label(tr("Theme", "主题"));
        egui::global_theme_preference_buttons(ui);
    });

    let AppOptions {
        experimental,
        warn_e2e_latency: _, // not yet exposed
        show_metrics,
        show_notification_toasts,
        custom_window_decorations,
        language: _, // switched via the top-panel 中/EN toggle
        include_rerun_examples_button_in_recordings_panel,
        show_picking_debug_overlay: _, // not yet exposed
        inspect_blueprint_timeline: _, // not yet exposed
        blueprint_gc: _,               // not yet exposed
        visualizer_limits_enabled,
        timestamp_format,
        video,
        mapbox_access_token,
        memory_limit,
        max_fetch_stage,

        #[cfg(not(target_arch = "wasm32"))]
            cache_directory: _, // not yet exposed
    } = app_options;

    ui.add_space(8.0);

    egui::Grid::new("prefetcher").num_columns(2).show(ui, |ui| {
        ui.label(tr("Memory budget", "内存预算"));
        memory_budget_section_ui(ui, memory_limit);
        ui.help_button(|ui| {
            ui.label(tr("When this limit is reached we start purging data from RAM", "达到这个上限后，会开始从内存中清理数据"));
        });
        ui.end_row();

        ui.label(tr("Prefetch", "预取"));
        prefetch_stage_combo_box_ui(ui, max_fetch_stage);
        ui.help_button(|ui| {
            ui.label(
                "控制在必需数据之外预取 chunk 的激进程度。\n\n\
                • 仅必需：只加载渲染当前时间标记所必需的 chunk。\n\
                • 相似：额外预取与必需 chunk 相同组件路径上、给定实际时长内的 chunk。\n\
                • 全部：额外预取录制文件中的所有 chunk。",
            );
        });
        ui.end_row();
    });

    ui.add_space(8.0);

    ui.re_checkbox(
        include_rerun_examples_button_in_recordings_panel,
        "显示“Rerun 示例”按钮",
    );

    ui.re_checkbox(
        visualizer_limits_enabled,
        "限制单个视图中的图元数量",
    )
    .on_hover_text(
        "限制每个可视化器处理的元素数量\
             （例如 3D 形状的实例上限、时间序列的线条上限）。\
             关闭后，数据量特别大时 Viewer 可能会卡死无响应。",
    );

    ui.collapsing_header("时间戳格式", false, |ui| {
        time_format_section_ui(ui, timestamp_format);
    });

    separator_with_some_space(ui);
    ui.strong("标题栏");

    if re_ui::supports_custom_decorations(ui.os()) {
        ui.re_checkbox(custom_window_decorations, "使用自定义窗口装饰")
            .on_hover_text(
                "隐藏系统原生标题栏，把 Rerun 的顶部栏画成窗口边框。\n\n\
             如果窗口行为出现异常，请关闭这个选项。",
            );
    }

    ui.re_checkbox(show_metrics, "显示性能指标")
        .on_hover_text("在顶部栏显示每帧耗时（毫秒）和内存占用");

    ui.re_checkbox(show_notification_toasts, "显示通知弹窗")
        .on_hover_text("以弹窗形式显示日志消息和其他通知");

    separator_with_some_space(ui);
    ui.strong("地图视图");
    map_view_section_ui(ui, mapbox_access_token);

    separator_with_some_space(ui);
    ui.strong("视频");
    video_section_ui(ui, video);

    #[cfg(target_arch = "wasm32")]
    if experimental.use_internal_catalog {
        separator_with_some_space(ui);
        ui.strong("浏览器源私有文件系统（OPFS）");
        origin_private_filesystem_section_ui(ui);
    }

    {
        let ExperimentalAppOptions {
            table_cards_and_blueprints,
            gamepad_navigation,
            point_cloud_transparency,
            use_internal_catalog,
        } = experimental;
        separator_with_some_space(ui);
        ui.strong("实验功能");
        ui.re_checkbox(table_cards_and_blueprints, "表格卡片与 blueprint")
            .on_hover_text(
                "为服务器提供的表格启用已注册的表格 blueprint 和卡片布局。\n\n\
                 启用后，表格可以用已注册的视图定义来预览 segment，表格标题栏中会出现列表/网格切换按钮。",
            );
        ui.re_checkbox(point_cloud_transparency, "点云透明度")
            .on_hover_text(
                "对半透明点云做 alpha 混合，并按从后到前排序。\n\n\
                 排序每帧都在 CPU 上进行，点云很大时会非常慢。",
            );
        ui.re_checkbox(use_internal_catalog, "通过 Viewer catalog 加载文件")
            .on_hover_text(
                "通过 Viewer catalog 加载 .rrd 文件，而不是作为实时录制文件导入。\
                 对启用之后打开的文件生效。",
            );
        cfg_select! {
            target_arch = "wasm32" => {
                let _ = gamepad_navigation;
            }
            _ => {
                let gamepad_navigation_response = ui
                    .re_checkbox(gamepad_navigation, "手柄操控")
                    .on_hover_text("在 3D 空间视图中启用手柄操控。");
                if gamepad_navigation_response.changed() && !*gamepad_navigation {
                    re_gamepad::clear_event_waker();
                }
            }
        }
    }
}

#[cfg(target_arch = "wasm32")]
fn origin_private_filesystem_section_ui(ui: &mut Ui) {
    if ui
        .button("申请持久化存储")
        .on_hover_text(
            "请求浏览器保护 Viewer catalog 文件不被自动清理。\
             浏览器可能会拒绝这个请求。包括 Firefox 在内的一些浏览器\
             在授予持久化后还会提高该源（origin）的存储配额。",
        )
        .clicked()
    {
        let request = re_web::browser::window().and_then(|window| {
            window
                .navigator()
                .storage()
                .persist()
                .map_err(re_web::Error::from)
        });
        re_async::spawn_local(async move {
            let result = match request {
                Ok(request) => request
                    .await
                    .map_err(re_web::Error::from)
                    .and_then(|value| {
                        value.as_bool().ok_or_else(|| {
                            re_web::Error::new(
                                "persistent storage request returned a non-boolean value",
                            )
                        })
                    }),
                Err(err) => Err(err),
            };

            match result {
                Ok(true) => re_log::info!("浏览器已授予持久化存储"),
                Ok(false) => re_log::warn!("浏览器拒绝了持久化存储请求"),
                Err(err) => {
                    re_log::error!("申请浏览器持久化存储失败：{err}");
                }
            }
        });
    }
}

fn memory_budget_section_ui(ui: &mut Ui, memory_limit: &mut MemoryLimit) {
    const BYTES_PER_GIB: u64 = 1024 * 1024 * 1024;
    const UPPER_LIMIT_BYTES: u64 = 1_000 * BYTES_PER_GIB;

    let mut bytes = memory_limit.as_bytes();

    let speed = (0.02 * bytes as f32).clamp(0.01 * BYTES_PER_GIB as f32, BYTES_PER_GIB as f32);

    ui.add(
        egui::DragValue::new(&mut bytes)
            .custom_formatter(|bytes, _| {
                if bytes < UPPER_LIMIT_BYTES as f64 {
                    re_format::format_bytes(bytes)
                } else {
                    "不限".to_owned()
                }
            })
            .custom_parser(|s| {
                let s = s.trim();
                if s.chars().all(|c| c.is_numeric()) {
                    // Assume GB
                    Some(BYTES_PER_GIB as f64 * f64::from_str(s).ok()?)
                } else {
                    Some(re_format::parse_bytes(s)? as f64)
                }
            })
            .update_while_editing(false)
            .range(0..=UPPER_LIMIT_BYTES)
            .speed(speed),
    );

    if bytes < UPPER_LIMIT_BYTES {
        *memory_limit = MemoryLimit::from_bytes(bytes);
    } else {
        *memory_limit = MemoryLimit::UNLIMITED;
    }
}

fn prefetch_stage_combo_box_ui(ui: &mut Ui, max_fetch_stage: &mut FetchStage) {
    fn label(stage: FetchStage) -> &'static str {
        match stage {
            FetchStage::Required | FetchStage::Indicated => "仅必需",
            FetchStage::Similar(_) => "相似",
            FetchStage::Everything => "全部",
        }
    }

    egui::ComboBox::from_id_salt("max_fetch_stage")
        .selected_text(label(*max_fetch_stage))
        .show_ui(ui, |ui| {
            for stage in [
                FetchStage::Indicated,
                FetchStage::default(),
                FetchStage::Everything,
            ] {
                ui.selectable_value(max_fetch_stage, stage, label(stage));
            }
        });

    /// Maps t in [0, 1] to a log scale value in [min, max],
    /// where t=1.0 maps to `f64::INFINITY`.
    fn log_slider_to_value(t: f64, min: f64, max_finite: f64) -> f64 {
        if t >= 1.0 {
            return f64::INFINITY;
        }
        // Remap t in [0, 1) to log scale over [min, max_finite]
        let log_min = min.log10();
        let log_max = max_finite.log10();
        10f64.powf(log_min + t * (log_max - log_min))
    }

    fn value_to_log_slider(value: f64, min: f64, max_finite: f64) -> f64 {
        if value.is_infinite() {
            return 1.0;
        }
        let log_min = min.log10();
        let log_max = max_finite.log10();
        (value.log10() - log_min) / (log_max - log_min)
    }

    match max_fetch_stage {
        FetchStage::Similar(range) => {
            const MIN: f64 = 1.0;
            const MAX_FINITE: f64 = 600.0;

            let seconds = range.map(|d| d.as_secs_f64()).unwrap_or(f64::INFINITY);

            let mut value = value_to_log_slider(seconds, MIN, MAX_FINITE);
            ui.add(egui::Slider::new(&mut value, 0.0..=1.0).show_value(false));

            *range =
                std::time::Duration::try_from_secs_f64(log_slider_to_value(value, MIN, MAX_FINITE))
                    .ok();

            let label = if let Some(range) = range {
                format!("{}s", range.as_secs())
            } else {
                "∞".to_owned()
            };

            ui.label(label);
        }
        FetchStage::Required | FetchStage::Indicated | FetchStage::Everything => {}
    }
}

fn time_format_section_ui(ui: &mut Ui, timestamp_format: &mut TimestampFormat) {
    fn timestamp_example_ui(
        ui: &mut egui::Ui,
        timestamp: Timestamp,
        timestamp_format: TimestampFormat,
    ) {
        ui.horizontal(|ui| {
            ui.add_space(ui.spacing().icon_width + ui.spacing().icon_spacing);
            egui::Frame::new()
                .fill(ui.visuals().text_edit_bg_color())
                .corner_radius(2.0)
                .inner_margin(egui::Margin::symmetric(4, 2))
                .show(ui, |ui| {
                    ui.label(
                        SyntaxHighlightedBuilder::primitive(&timestamp.format(timestamp_format))
                            .into_widget_text(ui.style()),
                    );
                });
        });
    }

    let timestamp = re_log_types::Timestamp::from(
        jiff::Timestamp::from_str("2023-02-14 21:47:18Z").expect("the timestamp is valid"),
    );

    ui.re_radio_value(timestamp_format, TimestampFormat::utc(), "UTC");
    timestamp_example_ui(ui, timestamp, TimestampFormat::utc());
    ui.re_radio_value(
        timestamp_format,
        TimestampFormat::local_timezone(),
        "本地时间（显示时区）",
    );
    timestamp_example_ui(ui, timestamp, TimestampFormat::local_timezone());
    ui.re_radio_value(
        timestamp_format,
        TimestampFormat::local_timezone_implicit(),
        "本地时间（隐藏时区）",
    );
    timestamp_example_ui(ui, timestamp, TimestampFormat::local_timezone_implicit());
    ui.horizontal(|ui| {
        ui.add_space(ui.spacing().icon_width + ui.spacing().icon_spacing);
        ui.label("注意：不带时区的时间戳复制到别处后会产生歧义。");
    });

    ui.re_radio_value(
        timestamp_format,
        TimestampFormat::unix_epoch(),
        "Unix 纪元以来的秒数",
    );
    timestamp_example_ui(ui, timestamp, TimestampFormat::unix_epoch());
}

fn map_view_section_ui(ui: &mut Ui, mapbox_access_token: &mut String) {
    ui.horizontal(|ui| {
        // TODO(ab): needed for alignment, we should use egui flex instead
        ui.set_height(19.0);

        ui.label("Mapbox access token：").on_hover_ui(|ui| {
            ui.markdown_ui(
                "这个 token 用于启用基于 Mapbox 的地图视图背景。\n\n\
                注意：token 会以明文保存在配置文件里。\
                也可以通过环境变量 `RERUN_MAPBOX_ACCESS_TOKEN` 设置。",
            );
        });

        ui.add(egui::TextEdit::singleline(mapbox_access_token).password(true));
    });
}

fn video_section_ui(ui: &mut Ui, options: &mut VideoOptions) {
    cfg_select! {
        target_arch = "wasm32" => {
            // This affects only the web target, so we don't need to show it on native.
            use re_video::DecodeHardwareAcceleration;

            let hardware_acceleration = &mut options.hw_acceleration;
            ui.horizontal(|ui| {
                ui.label("解码器：");
                egui::ComboBox::from_id_salt("video_decoder_hw_acceleration")
                    .selected_text(hardware_acceleration.to_string())
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            hardware_acceleration,
                            DecodeHardwareAcceleration::Auto,
                            DecodeHardwareAcceleration::Auto.to_string(),
                        );
                        ui.selectable_value(
                            hardware_acceleration,
                            DecodeHardwareAcceleration::PreferSoftware,
                            DecodeHardwareAcceleration::PreferSoftware.to_string(),
                        );
                        ui.selectable_value(
                            hardware_acceleration,
                            DecodeHardwareAcceleration::PreferHardware,
                            DecodeHardwareAcceleration::PreferHardware.to_string(),
                        );
                    });
                // Note that the setting is part of the video's cache key, so, if it changes, the cache
                // entries outdate automatically.
            });
        }
        _ => {
            ui.re_checkbox(
                &mut options.override_ffmpeg_path,
                "自定义 FFmpeg 程序路径",
            )
            .on_hover_ui(|ui| {
                ui.markdown_ui(
                    "默认情况下，Viewer 会在系统的 `PATH` 中自动寻找合适的 FFmpeg 程序。\
                    启用这个选项后可以手动指定 FFmpeg 程序的路径。",
                );
            });

            ui.add_enabled_ui(options.override_ffmpeg_path, |ui| {
                ui.horizontal(|ui| {
                    // TODO(ab): needed for alignment, we should use egui flex instead
                    ui.set_height(19.0);

                    ui.label("路径：");

                    ui.add(egui::TextEdit::singleline(&mut options.ffmpeg_path));
                });
            });

            ffmpeg_path_status_ui(ui, options);
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn ffmpeg_path_status_ui(ui: &mut Ui, options: &VideoOptions) {
    use std::task::Poll;

    use re_video::{FFmpegVersion, FFmpegVersionParseError};

    let path = options
        .override_ffmpeg_path
        .then(|| std::path::Path::new(&options.ffmpeg_path));

    match FFmpegVersion::for_executable_poll(path) {
        Poll::Pending => {
            ui.loading_indicator("正在检查 FFmpeg 版本");
        }

        Poll::Ready(Ok(version)) => {
            if version.is_compatible() {
                ui.success_label(format!("已找到 FFmpeg（版本 {version}）"));
            } else {
                ui.error_label(format!("FFmpeg 版本不兼容：{version}"));
            }
        }
        Poll::Ready(Err(FFmpegVersionParseError::ParseVersion { raw_version })) => {
            // We make this one a warning instead of an error because version parsing is flaky, and
            // it might end up still working.
            ui.warning_label(format!(
                "找到了 FFmpeg 程序，但无法解析其版本：{raw_version}"
            ));
        }

        Poll::Ready(Err(FFmpegVersionParseError::FFmpegNotFound(_path))) => {
            ui.error_label("指定的 FFmpeg 程序路径不存在，或者不是一个文件。");
        }

        Poll::Ready(Err(err)) => {
            ui.error_label(err.to_string());
        }
    }
}

fn separator_with_some_space(ui: &mut egui::Ui) {
    ui.add_space(10.0);
    ui.separator();
    ui.add_space(10.0);
}
