use re_i18n::{tr, trf};
use std::ops::Sub as _;

use re_format::format_plural_s;

use re_log_types::{Timestamp, TimestampFormat};

/// Formats a duration in a short, readable format, e.g. ("1 hour ago" or "2 minutes ago")
///
/// 0-10 seconds: "just now"
/// 10-60 seconds: "less than a minute ago"
/// 1-60 minutes: "X minutes ago"
/// 1-24 hours: "X hours ago"
/// 1-7 days: "X days ago"
/// Over 7 days ago: formats the timestamp using the provided `TimestampFormat`.
pub fn format_duration_short(timestamp: Timestamp, fallback_format: TimestampFormat) -> String {
    let duration = Timestamp::now().sub(timestamp);
    let seconds = duration.as_secs_f64() as u64;

    // English pluralizes the unit ("2 minutes ago"); Chinese does not ("2 分钟前").
    let format_plural = |n: u64, unit_en: &'static str, unit_zh: &'static str| {
        if re_i18n::is_chinese() {
            format!("{n} {unit_zh}前")
        } else {
            format!("{} ago", format_plural_s(n, unit_en))
        }
    };

    if seconds < 10 {
        tr("just now", "刚刚").to_owned()
    } else if seconds < 60 {
        tr("less than a minute ago", "不到一分钟前").to_owned()
    } else if seconds < 3600 {
        let minutes = seconds / 60;
        format_plural(minutes, "minute", "分钟")
    } else if seconds < 24 * 3600 {
        let hours = seconds / 3600;
        format_plural(hours, "hour", "小时")
    } else if seconds < 7 * 24 * 3600 {
        let days = seconds / 86400;
        format_plural(days, "day", "天")
    } else {
        timestamp.format(fallback_format)
    }
}

/// Shows a timestamp as a duration from now, in a short format.
///
/// E.g. "1 hour ago", "2 minutes ago", or "just now".
/// Shows the full timestamp on hover.
pub fn short_duration_ui(
    ui: &mut egui::Ui,
    timestamp: Timestamp,
    format: TimestampFormat,
    show: impl FnOnce(&mut egui::Ui, String) -> egui::Response,
) -> egui::Response {
    // Remember to update the ui so it doesn't say "just now" forever:
    let age = timestamp.elapsed().as_secs_f64();
    let repaint_in_sec = if age < 60.0 {
        1
    } else if age < 3600.0 {
        60
    } else {
        3600
    };
    ui.request_repaint_after(std::time::Duration::from_secs(repaint_in_sec));

    let short = format_duration_short(timestamp, format);
    show(ui, short).on_hover_text(timestamp.format(format))
}
