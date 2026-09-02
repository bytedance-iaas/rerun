//! Minimal runtime i18n for this fork's viewer.
//!
//! The whole UI is immediate-mode: every frame re-queries the strings it draws.
//! So switching language is just flipping [`set_language`] and requesting a repaint —
//! the next frame renders entirely in the new language, with no cached text to
//! invalidate. A single global holds the current language so that both the viewer UI
//! and the lower-level crates (data sources, importers) can read it without threading
//! a parameter through every function.

use std::sync::atomic::{AtomicU8, Ordering};

/// The languages the UI can display.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize)]
pub enum Language {
    /// 简体中文 — the default for this fork.
    #[default]
    Chinese,

    /// English — the upstream Rerun language.
    English,
}

impl Language {
    fn from_u8(value: u8) -> Self {
        match value {
            1 => Self::English,
            _ => Self::Chinese,
        }
    }

    fn as_u8(self) -> u8 {
        match self {
            Self::Chinese => 0,
            Self::English => 1,
        }
    }

    /// The label to show on a toggle that switches to the *other* language.
    pub fn toggle_label(self) -> &'static str {
        match self {
            Self::Chinese => "EN",
            Self::English => "中",
        }
    }

    /// The language you get by toggling away from this one.
    pub fn toggled(self) -> Self {
        match self {
            Self::Chinese => Self::English,
            Self::English => Self::Chinese,
        }
    }
}

/// 0 = Chinese (default), 1 = English.
static LANGUAGE: AtomicU8 = AtomicU8::new(0);

/// The language the UI is currently rendering in.
#[inline]
pub fn language() -> Language {
    Language::from_u8(LANGUAGE.load(Ordering::Relaxed))
}

/// Set the current language. Call [`egui::Context::request_repaint`] afterwards so the
/// change shows up immediately.
#[inline]
pub fn set_language(language: Language) {
    LANGUAGE.store(language.as_u8(), Ordering::Relaxed);
}

/// Is the UI currently in Chinese? (Used by the [`trf!`] macro; prefer [`tr`] elsewhere.)
#[inline]
pub fn is_chinese() -> bool {
    language() == Language::Chinese
}

/// Pick between an English and a Chinese string based on the current language.
///
/// ```
/// # use re_i18n::tr;
/// let label = tr("Open file…", "打开文件…");
/// ```
#[inline]
pub fn tr(english: &'static str, chinese: &'static str) -> &'static str {
    if is_chinese() {
        chinese
    } else {
        english
    }
}

/// Like [`tr`], but for `format!`-style templates with interpolation. Both templates
/// must be string literals; the arguments are shared.
///
/// ```
/// # use re_i18n::trf;
/// let err = "boom";
/// let msg = trf!("Failed to open: {err}", "打开失败：{err}");
/// ```
#[macro_export]
macro_rules! trf {
    ($english:literal, $chinese:literal $(, $($arg:tt)*)?) => {
        if $crate::is_chinese() {
            format!($chinese $(, $($arg)*)?)
        } else {
            format!($english $(, $($arg)*)?)
        }
    };
}
