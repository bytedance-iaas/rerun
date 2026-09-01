use egui::os::OperatingSystem;
use egui::{Key, KeyboardShortcut, Modifiers};
use smallvec::{SmallVec, smallvec};

use crate::context_ext::ContextExt as _;

/// Interface for sending [`UICommand`] messages.
pub trait UICommandSender {
    fn send_ui(&self, command: UICommand);
}

/// All the commands we support.
///
/// Most are available in the GUI,
/// some have keyboard shortcuts,
/// and all are visible in the [`crate::CommandPalette`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, strum_macros::EnumIter)]
pub enum UICommand {
    // Listed in the order they show up in the command palette by default!
    Open,
    OpenUrl,
    OpenTosDataset,
    OpenHfDataset,
    Import,

    CloseAllEntries,

    NextRecording,
    PreviousRecording,

    NavigateBack,
    NavigateForward,

    #[cfg(not(target_arch = "wasm32"))]
    Quit,

    OpenWebsite,
    OpenWebHelp,
    OpenRerunDiscord,

    ResetViewer,

    #[cfg(not(target_arch = "wasm32"))]
    OpenProfiler,

    #[cfg(not(target_arch = "wasm32"))]
    CaptureProfileTrace,

    TogglePanelStateOverrides,
    ToggleDevPanel,
    ToggleTopPanel,
    ToggleBlueprintPanel,
    ExpandBlueprintPanel,
    ToggleSelectionPanel,
    ExpandSelectionPanel,
    Settings,

    #[cfg(debug_assertions)]
    ToggleEguiDebugPanel,

    ToggleFullscreen,
    #[cfg(not(target_arch = "wasm32"))]
    ZoomIn,
    #[cfg(not(target_arch = "wasm32"))]
    ZoomOut,
    #[cfg(not(target_arch = "wasm32"))]
    ZoomReset,

    ToggleCommandPalette,

    // Dev-tools:
    #[cfg(not(target_arch = "wasm32"))]
    ScreenshotWholeApp,

    #[cfg(debug_assertions)]
    ResetEguiMemory,

    Share,
    CopyDirectLink,

    CopyTimeSelectionLink,

    CopyEntityHierarchy,

    // Graphics options:
    #[cfg(target_arch = "wasm32")]
    RestartWithWebGl,
    #[cfg(target_arch = "wasm32")]
    RestartWithWebGpu,

    // Redap commands
    AddRedapServer,
}

impl UICommand {
    pub fn text(self) -> &'static str {
        self.text_and_tooltip().0
    }

    pub fn tooltip(self) -> &'static str {
        self.text_and_tooltip().1
    }

    pub fn text_and_tooltip(self) -> (&'static str, &'static str) {
        match self {
            Self::Open => (
                "打开文件…",
                "在新的录制文件中打开任意支持的文件（.rrd、图片、网格模型等）",
            ),
            Self::OpenUrl => (
                "从 URL 打开…",
                "打开或跳转到任意支持的 URL 数据",
            ),
            Self::OpenTosDataset => (
                "从火山引擎 TOS 打开…",
                "从火山引擎 TOS 桶打开数据集。",
            ),
            Self::OpenHfDataset => (
                "从 Hugging Face 打开…",
                "从 Hugging Face 打开数据集。",
            ),
            Self::Import => (
                "导入到当前录制文件…",
                "把任意支持的文件（.rrd、图片、网格模型等）导入到当前录制文件",
            ),

            Self::CloseAllEntries => (
                "关闭所有录制文件",
                "关闭所有打开的录制文件（未保存的数据会丢失）",
            ),

            Self::NextRecording => ("下一个录制文件", "切换到下一个打开的录制文件"),
            Self::PreviousRecording => (
                "上一个录制文件",
                "切换到上一个打开的录制文件",
            ),

            Self::NavigateBack => ("后退", "回到上一个浏览位置"),
            Self::NavigateForward => ("前进", "去往下一个浏览位置"),

            #[cfg(not(target_arch = "wasm32"))]
            Self::Quit => ("退出", "关闭 Rerun Viewer"),

            Self::OpenWebsite => ("rerun.io", "访问 Rerun 官网"),
            Self::OpenWebHelp => (
                "文档",
                "访问官网文档，包含常见问题排查等内容",
            ),
            Self::OpenRerunDiscord => (
                "Rerun Discord",
                "访问 Rerun 的 Discord 社区，可以提问和寻求帮助",
            ),

            Self::ResetViewer => (
                "重置 Viewer",
                "把 Viewer 恢复到第一次运行时的样子，清除界面状态和所有已保存的 blueprint（从 *.rbl 资源加载的除外）",
            ),

            #[cfg(not(target_arch = "wasm32"))]
            Self::OpenProfiler => (
                "打开性能分析器",
                "启动性能分析器，查看是什么拖慢了 Viewer",
            ),

            #[cfg(not(target_arch = "wasm32"))]
            Self::CaptureProfileTrace => (
                "抓取性能数据…",
                "抓取性能分析数据并保存为 .puffin 文件",
            ),

            Self::ToggleDevPanel => (
                "显示/隐藏开发者面板",
                "查看 Rerun Viewer 的内存占用等开发者统计信息",
            ),

            Self::TogglePanelStateOverrides => (
                "切换面板状态覆盖",
                "在应用 blueprint 与手动覆盖之间切换面板状态",
            ),
            Self::ToggleTopPanel => ("显示/隐藏顶部面板", "显示或隐藏顶部面板"),
            Self::ToggleBlueprintPanel => ("显示/隐藏 Blueprint 面板", "显示或隐藏左侧面板"),
            Self::ExpandBlueprintPanel => ("展开 Blueprint 面板", "展开左侧面板"),
            Self::ToggleSelectionPanel => ("显示/隐藏 Selection 面板", "显示或隐藏右侧面板"),
            Self::ExpandSelectionPanel => ("展开 Selection 面板", "展开右侧面板"),
            Self::Settings => ("设置…", "打开设置页面"),

            #[cfg(debug_assertions)]
            Self::ToggleEguiDebugPanel => (
                "显示/隐藏 egui 调试面板",
                "查看和修改 egui 全局样式设置",
            ),

            #[cfg(not(target_arch = "wasm32"))]
            Self::ToggleFullscreen => (
                "切换全屏",
                "在窗口模式和全屏模式之间切换",
            ),

            #[cfg(target_arch = "wasm32")]
            Self::ToggleFullscreen => (
                "切换全屏",
                "在占满整个页面和初始大小之间切换",
            ),

            #[cfg(not(target_arch = "wasm32"))]
            Self::ZoomIn => ("放大", "放大界面显示"),
            #[cfg(not(target_arch = "wasm32"))]
            Self::ZoomOut => ("缩小", "缩小界面显示"),
            #[cfg(not(target_arch = "wasm32"))]
            Self::ZoomReset => (
                "重置缩放",
                "把界面缩放恢复到操作系统的默认值",
            ),

            Self::ToggleCommandPalette => ("命令面板…", "打开或关闭命令面板"),

            #[cfg(not(target_arch = "wasm32"))]
            Self::ScreenshotWholeApp => (
                "截图",
                "把整个应用的截图复制到剪贴板",
            ),
            #[cfg(debug_assertions)]
            Self::ResetEguiMemory => (
                "重置 egui 内存",
                "重置 egui 内存，用于调试界面代码。",
            ),

            Self::Share => ("分享…", "把当前画面以链接形式分享"),
            Self::CopyDirectLink => (
                "复制直达链接",
                "尝试复制当前画面的分享链接。并非所有数据源和 Viewer 状态都支持。",
            ),

            Self::CopyTimeSelectionLink => (
                "复制选中时间段的链接",
                "复制当前录制文件中循环选区对应时间段的链接。",
            ),

            Self::CopyEntityHierarchy => (
                "复制实体层级",
                "把当前录制文件的完整实体层级树复制到剪贴板。",
            ),

            #[cfg(target_arch = "wasm32")]
            Self::RestartWithWebGl => (
                "用 WebGL 重启",
                "重新加载网页并强制使用 WebGL 渲染。所有数据会丢失。",
            ),
            #[cfg(target_arch = "wasm32")]
            Self::RestartWithWebGpu => (
                "用 WebGPU 重启",
                "重新加载网页并强制使用 WebGPU 渲染。所有数据会丢失。",
            ),

            Self::AddRedapServer => (
                "连接服务器…",
                "连接 Redap 服务器（实验功能）",
            ),
        }
    }

    /// All keyboard shortcuts, with the primary first.
    // `os` is only used by OS-specific shortcuts (e.g. `Quit`), which are all native-only,
    // so it is unused on wasm:
    #[allow(clippy::allow_attributes, unused_variables)]
    pub fn kb_shortcuts(self, os: OperatingSystem) -> SmallVec<[KeyboardShortcut; 2]> {
        fn key(key: Key) -> KeyboardShortcut {
            KeyboardShortcut::new(Modifiers::NONE, key)
        }

        fn cmd(key: Key) -> KeyboardShortcut {
            KeyboardShortcut::new(Modifiers::COMMAND, key)
        }

        fn cmd_shift(key: Key) -> KeyboardShortcut {
            KeyboardShortcut::new(Modifiers::COMMAND | Modifiers::SHIFT, key)
        }

        fn cmd_alt(key: Key) -> KeyboardShortcut {
            KeyboardShortcut::new(Modifiers::COMMAND | Modifiers::ALT, key)
        }

        fn ctrl_shift(key: Key) -> KeyboardShortcut {
            KeyboardShortcut::new(Modifiers::CTRL | Modifiers::SHIFT, key)
        }

        match self {
            Self::Open => smallvec![cmd(Key::O)],
            // Some browsers have a "paste and go" action.
            // But unfortunately there's no standard shortcut for this.
            // Claude however thinks it's this one (it's not). Let's go with that anyways!
            Self::OpenUrl => smallvec![cmd_shift(Key::L)],
            Self::OpenTosDataset => smallvec![],
            Self::OpenHfDataset => smallvec![],
            Self::Import => smallvec![cmd_shift(Key::O)],
            Self::CloseAllEntries => smallvec![],

            Self::NextRecording => smallvec![cmd_alt(Key::ArrowDown)],
            Self::PreviousRecording => smallvec![cmd_alt(Key::ArrowUp)],

            Self::NavigateBack => smallvec![cmd(Key::OpenBracket)],
            Self::NavigateForward => smallvec![cmd(Key::CloseBracket)],

            #[cfg(not(target_arch = "wasm32"))]
            Self::Quit => {
                if os == OperatingSystem::Windows {
                    smallvec![KeyboardShortcut::new(Modifiers::ALT, Key::F4)]
                } else {
                    smallvec![cmd(Key::Q)]
                }
            }

            Self::OpenWebHelp => smallvec![],
            Self::OpenWebsite => smallvec![],
            Self::OpenRerunDiscord => smallvec![],

            Self::ResetViewer => smallvec![ctrl_shift(Key::R)],

            #[cfg(not(target_arch = "wasm32"))]
            Self::OpenProfiler => smallvec![ctrl_shift(Key::P)],
            #[cfg(not(target_arch = "wasm32"))]
            Self::CaptureProfileTrace => smallvec![],
            Self::ToggleDevPanel => smallvec![ctrl_shift(Key::M)],
            Self::TogglePanelStateOverrides => smallvec![],
            Self::ToggleTopPanel => smallvec![],
            Self::ToggleBlueprintPanel => smallvec![ctrl_shift(Key::B)],
            Self::ExpandBlueprintPanel => smallvec![],
            Self::ToggleSelectionPanel => smallvec![ctrl_shift(Key::S)],
            Self::ExpandSelectionPanel => smallvec![],
            Self::Settings => smallvec![cmd(Key::Comma)],

            #[cfg(debug_assertions)]
            Self::ToggleEguiDebugPanel => smallvec![ctrl_shift(Key::U)],

            Self::ToggleFullscreen => {
                if cfg!(target_arch = "wasm32") {
                    smallvec![]
                } else {
                    smallvec![key(Key::F11)]
                }
            }

            #[cfg(not(target_arch = "wasm32"))]
            Self::ZoomIn => smallvec![egui::gui_zoom::kb_shortcuts::ZOOM_IN],
            #[cfg(not(target_arch = "wasm32"))]
            Self::ZoomOut => smallvec![egui::gui_zoom::kb_shortcuts::ZOOM_OUT],
            #[cfg(not(target_arch = "wasm32"))]
            Self::ZoomReset => smallvec![egui::gui_zoom::kb_shortcuts::ZOOM_RESET],

            Self::ToggleCommandPalette => smallvec![cmd(Key::K), cmd(Key::P)],

            #[cfg(not(target_arch = "wasm32"))]
            Self::ScreenshotWholeApp => smallvec![],

            #[cfg(debug_assertions)]
            Self::ResetEguiMemory => smallvec![],

            Self::Share => smallvec![cmd(Key::L)],
            Self::CopyDirectLink => smallvec![],

            Self::CopyTimeSelectionLink => smallvec![],

            Self::CopyEntityHierarchy => smallvec![ctrl_shift(Key::E)],

            #[cfg(target_arch = "wasm32")]
            Self::RestartWithWebGl => smallvec![],
            #[cfg(target_arch = "wasm32")]
            Self::RestartWithWebGpu => smallvec![],

            Self::AddRedapServer => smallvec![],
        }
    }

    /// Primary keyboard shortcut
    pub fn primary_kb_shortcut(self, os: OperatingSystem) -> Option<KeyboardShortcut> {
        self.kb_shortcuts(os).first().copied()
    }

    /// Return the keyboard shortcut for this command, nicely formatted
    // TODO(emilk): use Help/IconText instead
    pub fn formatted_kb_shortcut(self, egui_ctx: &egui::Context) -> Option<String> {
        // Note: we only show the primary shortcut to the user.
        // The fallbacks are there for people who have muscle memory for the other shortcuts.
        self.primary_kb_shortcut(egui_ctx.os())
            .map(|shortcut| egui_ctx.format_shortcut(&shortcut))
    }

    pub fn icon(self) -> Option<&'static crate::Icon> {
        match self {
            Self::OpenWebsite | Self::OpenWebHelp => Some(&crate::icons::EXTERNAL_LINK),
            Self::OpenRerunDiscord => Some(&crate::icons::DISCORD),
            _ => None,
        }
    }

    pub fn is_link(self) -> bool {
        matches!(self, Self::OpenWebHelp | Self::OpenRerunDiscord)
    }

    /// Does this command only exist in debug builds?
    ///
    /// Such commands are marked with an orange "debug only" badge in the UI.
    #[cfg(debug_assertions)]
    pub fn is_debug_only(self) -> bool {
        matches!(self, Self::ToggleEguiDebugPanel | Self::ResetEguiMemory)
    }

    /// Listen for keyboard shortcuts of [`UICommand`]s only.
    ///
    /// The viewer should use [`super::listen_for_kb_shortcuts`] instead,
    /// which also matches recording commands.
    pub fn listen_for_kb_shortcut(egui_ctx: &egui::Context) -> Option<Self> {
        use strum::IntoEnumIterator as _;

        let commands = Self::iter()
            .flat_map(|cmd| {
                cmd.kb_shortcuts(egui_ctx.os())
                    .into_iter()
                    .map(move |kb_shortcut| (kb_shortcut, cmd))
            })
            .collect();

        super::consume_best_shortcut(egui_ctx, commands)
    }

    /// Show this command as a menu-button.
    ///
    /// If clicked, enqueue the command.
    pub fn menu_button_ui(
        self,
        ui: &mut egui::Ui,
        command_sender: &impl UICommandSender,
    ) -> egui::Response {
        self.menu_button_ui_enabled(ui, true, command_sender)
    }

    /// Show this command as a (possibly disabled) menu-button.
    ///
    /// If clicked, enqueue the command.
    pub fn menu_button_ui_enabled(
        self,
        ui: &mut egui::Ui,
        enabled: bool,
        command_sender: &impl UICommandSender,
    ) -> egui::Response {
        let button = self.menu_button(ui.ctx());
        let mut response = ui
            .add_enabled(enabled, button)
            .on_hover_text(self.tooltip());

        if self.is_link() {
            response = response.on_hover_cursor(egui::CursorIcon::PointingHand);
        }

        if response.clicked() {
            command_sender.send_ui(self);
            ui.close();
        }

        response
    }

    pub fn menu_button(self, egui_ctx: &egui::Context) -> egui::Button<'static> {
        let tokens = egui_ctx.tokens();

        let mut button = if let Some(icon) = self.icon() {
            egui::Button::image_and_text(
                icon.as_image()
                    .tint(tokens.label_button_icon_color)
                    .fit_to_exact_size(tokens.small_icon_size),
                self.text(),
            )
        } else {
            cfg_select! {
                debug_assertions => {
                    if self.is_debug_only() {
                        egui::Button::new((
                            self.text(),
                            crate::debug_only::debug_only_rich_text(&egui_ctx.global_style()),
                        ))
                    } else {
                        egui::Button::new(self.text())
                    }
                }
                _ => egui::Button::new(self.text()),
            }
        };

        if let Some(shortcut_text) = self.formatted_kb_shortcut(egui_ctx) {
            button = button.shortcut_text(shortcut_text);
        }

        button
    }

    /// Show name of command and how to activate it
    pub fn tooltip_ui(self, ui: &mut egui::Ui) {
        let os = ui.os();

        let (label, details) = self.text_and_tooltip();

        if let Some(shortcut) = self.primary_kb_shortcut(os) {
            crate::Help::new_without_title()
                .control(label, crate::IconText::from_keyboard_shortcut(os, shortcut))
                .ui(ui);
        } else {
            ui.label(label);
        }

        ui.set_max_width(220.0);
        ui.label(details);
    }
}
