use egui::os::OperatingSystem;
use egui::{Key, KeyboardShortcut, Modifiers};
use smallvec::{SmallVec, smallvec};

use re_i18n::tr;

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
                tr("Open file…", "打开文件…"),
                tr(
                    "Open any supported files (.rrd, images, meshes, …) in a new recording",
                    "在新的 episode 中打开任意支持的文件（.rrd、图片、网格模型等）",
                ),
            ),
            Self::OpenUrl => (
                tr("Open from URL…", "从 URL 打开…"),
                tr(
                    "Open or navigate to data from any supported URL",
                    "打开或跳转到任意支持的 URL 数据",
                ),
            ),
            Self::OpenTosDataset => (
                tr("Open from Volcengine TOS…", "从火山引擎 TOS 打开…"),
                tr(
                    "Open a dataset from a Volcengine TOS bucket.",
                    "从火山引擎 TOS 桶打开数据集。",
                ),
            ),
            Self::OpenHfDataset => (
                tr("Open from Hugging Face…", "从 Hugging Face 打开…"),
                tr(
                    "Open a dataset from Hugging Face.",
                    "从 Hugging Face 打开数据集。",
                ),
            ),
            Self::Import => (
                tr("Import into current recording…", "导入到当前 episode…"),
                tr(
                    "Import any supported files (.rrd, images, meshes, …) in the current recording",
                    "把任意支持的文件（.rrd、图片、网格模型等）导入到当前 episode",
                ),
            ),

            Self::CloseAllEntries => (
                tr("Close all recordings", "关闭所有 episode"),
                tr(
                    "Close all open current recording (unsaved data will be lost)",
                    "关闭所有打开的 episode（未保存的数据会丢失）",
                ),
            ),

            Self::NextRecording => (
                tr("Next recording", "下一个 episode"),
                tr(
                    "Switch to the next open recording",
                    "切换到下一个打开的 episode",
                ),
            ),
            Self::PreviousRecording => (
                tr("Previous recording", "上一个 episode"),
                tr(
                    "Switch to the previous open recording",
                    "切换到上一个打开的 episode",
                ),
            ),

            Self::NavigateBack => (
                tr("Back in history", "后退"),
                tr("Go back in history", "回到上一个浏览位置"),
            ),
            Self::NavigateForward => (
                tr("Forward in history", "前进"),
                tr("Go forward in history", "去往下一个浏览位置"),
            ),

            #[cfg(not(target_arch = "wasm32"))]
            Self::Quit => (
                tr("Quit", "退出"),
                tr("Close the Rerun Viewer", "关闭 Rerun Viewer"),
            ),

            Self::OpenWebsite => (
                tr("rerun.io", "rerun.io"),
                tr("Visit our homepage", "访问 Rerun 官网"),
            ),
            Self::OpenWebHelp => (
                tr("Docs", "文档"),
                tr(
                    "Visit the docs on our website, with troubleshooting tips and more",
                    "访问官网文档，包含常见问题排查等内容",
                ),
            ),
            Self::OpenRerunDiscord => (
                tr("Rerun Discord", "Rerun Discord"),
                tr(
                    "Visit the Rerun Discord server, where you can ask questions and get help",
                    "访问 Rerun 的 Discord 社区，可以提问和寻求帮助",
                ),
            ),

            Self::ResetViewer => (
                tr("Reset Viewer", "重置 Viewer"),
                tr(
                    "Reset the Viewer to how it looked the first time you ran it, forgetting UI state and all stored blueprints, except the ones loaded from *.rbl resources",
                    "把 Viewer 恢复到第一次运行时的样子，清除界面状态和所有已保存的 blueprint（从 *.rbl 资源加载的除外）",
                ),
            ),

            #[cfg(not(target_arch = "wasm32"))]
            Self::OpenProfiler => (
                tr("Open profiler", "打开性能分析器"),
                tr(
                    "Starts a profiler, showing what makes the viewer run slow",
                    "启动性能分析器，查看是什么拖慢了 Viewer",
                ),
            ),

            #[cfg(not(target_arch = "wasm32"))]
            Self::CaptureProfileTrace => (
                tr("Capture profile trace…", "抓取性能数据…"),
                tr(
                    "Capture profiling data and save them as a .puffin file",
                    "抓取性能分析数据并保存为 .puffin 文件",
                ),
            ),

            Self::ToggleDevPanel => (
                tr("Toggle dev panel", "显示/隐藏开发者面板"),
                tr(
                    "View developer stats like RAM usage inside Rerun Viewer",
                    "查看 Rerun Viewer 的内存占用等开发者统计信息",
                ),
            ),

            Self::TogglePanelStateOverrides => (
                tr("Toggle panel state overrides", "切换面板状态覆盖"),
                tr(
                    "Toggle panel state between app blueprint and overrides",
                    "在应用 blueprint 与手动覆盖之间切换面板状态",
                ),
            ),
            Self::ToggleTopPanel => (
                tr("Toggle top panel", "显示/隐藏顶部面板"),
                tr("Toggle the top panel", "显示或隐藏顶部面板"),
            ),
            Self::ToggleBlueprintPanel => (
                tr("Toggle blueprint panel", "显示/隐藏 Blueprint 面板"),
                tr("Toggle the left panel", "显示或隐藏左侧面板"),
            ),
            Self::ExpandBlueprintPanel => (
                tr("Expand blueprint panel", "展开 Blueprint 面板"),
                tr("Expand the left panel", "展开左侧面板"),
            ),
            Self::ToggleSelectionPanel => (
                tr("Toggle selection panel", "显示/隐藏 Selection 面板"),
                tr("Toggle the right panel", "显示或隐藏右侧面板"),
            ),
            Self::ExpandSelectionPanel => (
                tr("Expand selection panel", "展开 Selection 面板"),
                tr("Expand the right panel", "展开右侧面板"),
            ),
            Self::Settings => (
                tr("Settings…", "设置…"),
                tr("Show the settings screen", "打开设置页面"),
            ),

            #[cfg(debug_assertions)]
            Self::ToggleEguiDebugPanel => (
                tr("Toggle egui debug panel", "显示/隐藏 egui 调试面板"),
                tr(
                    "View and change global egui style settings",
                    "查看和修改 egui 全局样式设置",
                ),
            ),

            #[cfg(not(target_arch = "wasm32"))]
            Self::ToggleFullscreen => (
                tr("Toggle fullscreen", "切换全屏"),
                tr(
                    "Toggle between windowed and fullscreen viewer",
                    "在窗口模式和全屏模式之间切换",
                ),
            ),

            #[cfg(target_arch = "wasm32")]
            Self::ToggleFullscreen => (
                tr("Toggle fullscreen", "切换全屏"),
                tr(
                    "Toggle between full viewport dimensions and initial dimensions",
                    "在占满整个页面和初始大小之间切换",
                ),
            ),

            #[cfg(not(target_arch = "wasm32"))]
            Self::ZoomIn => (
                tr("Zoom in", "放大"),
                tr("Increases the UI zoom level", "放大界面显示"),
            ),
            #[cfg(not(target_arch = "wasm32"))]
            Self::ZoomOut => (
                tr("Zoom out", "缩小"),
                tr("Decreases the UI zoom level", "缩小界面显示"),
            ),
            #[cfg(not(target_arch = "wasm32"))]
            Self::ZoomReset => (
                tr("Reset zoom", "重置缩放"),
                tr(
                    "Resets the UI zoom level to the operating system's default value",
                    "把界面缩放恢复到操作系统的默认值",
                ),
            ),

            Self::ToggleCommandPalette => (
                tr("Command palette…", "命令面板…"),
                tr("Toggle the Command Palette", "打开或关闭命令面板"),
            ),

            #[cfg(not(target_arch = "wasm32"))]
            Self::ScreenshotWholeApp => (
                tr("Screenshot", "截图"),
                tr(
                    "Copy screenshot of the whole app to clipboard",
                    "把整个应用的截图复制到剪贴板",
                ),
            ),
            #[cfg(debug_assertions)]
            Self::ResetEguiMemory => (
                tr("Reset egui memory", "重置 egui 内存"),
                tr(
                    "Reset egui memory, useful for debugging UI code.",
                    "重置 egui 内存，用于调试界面代码。",
                ),
            ),

            Self::Share => (
                tr("Share…", "分享…"),
                tr(
                    "Share the current screen as a link",
                    "把当前画面以链接形式分享",
                ),
            ),
            Self::CopyDirectLink => (
                tr("Copy direct link", "复制直达链接"),
                tr(
                    "Try to copy a shareable link to the current screen. This is not supported for all data sources & viewer states.",
                    "尝试复制当前画面的分享链接。并非所有数据源和 Viewer 状态都支持。",
                ),
            ),

            Self::CopyTimeSelectionLink => (
                tr("Copy link to selected time range", "复制选中时间段的链接"),
                tr(
                    "Copy a link to the part of the active recording within the loop selection bounds.",
                    "复制当前 episode 中循环选区对应时间段的链接。",
                ),
            ),

            Self::CopyEntityHierarchy => (
                tr("Copy entity hierarchy", "复制实体层级"),
                tr(
                    "Copy the complete entity hierarchy tree of the currently active recording to the clipboard.",
                    "把当前 episode 的完整实体层级树复制到剪贴板。",
                ),
            ),

            #[cfg(target_arch = "wasm32")]
            Self::RestartWithWebGl => (
                tr("Restart with WebGL", "用 WebGL 重启"),
                tr(
                    "Reloads the webpage and force WebGL for rendering. All data will be lost.",
                    "重新加载网页并强制使用 WebGL 渲染。所有数据会丢失。",
                ),
            ),
            #[cfg(target_arch = "wasm32")]
            Self::RestartWithWebGpu => (
                tr("Restart with WebGPU", "用 WebGPU 重启"),
                tr(
                    "Reloads the webpage and force WebGPU for rendering. All data will be lost.",
                    "重新加载网页并强制使用 WebGPU 渲染。所有数据会丢失。",
                ),
            ),

            Self::AddRedapServer => (
                tr("Connect to a server…", "连接服务器…"),
                tr(
                    "Connect to a Redap server (experimental)",
                    "连接 Redap 服务器（实验功能）",
                ),
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
