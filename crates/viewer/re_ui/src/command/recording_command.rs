use egui::os::OperatingSystem;
use egui::{Id, Key, KeyboardShortcut, Modifiers};
use re_log_types::StoreId;
use smallvec::{SmallVec, smallvec};

use super::CommandEnvironment;
use crate::context_ext::ContextExt as _;

/// Interface for sending [`RecordingCommand`] messages.
pub trait RecordingCommandSender {
    fn send_recording_command(&self, command: RecordingCommand);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SetPlaybackSpeed(pub egui::emath::OrderedFloat<f32>);

impl Default for SetPlaybackSpeed {
    fn default() -> Self {
        Self(egui::emath::OrderedFloat(1.0))
    }
}

/// A command that acts on a specific recording.
///
/// Unlike [`super::UICommand`], these carry the [`StoreId`] of the recording they act on,
/// so they can be used both from the command palette (acting on the active recording)
/// and from menus and buttons (acting on a specific recording).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct RecordingCommand {
    /// The recording this command acts on.
    pub recording_id: StoreId,

    /// What to do with the recording.
    pub kind: RecordingCommandKind,
}

/// What a [`RecordingCommand`] does to its recording.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, strum_macros::EnumIter)]
pub enum RecordingCommandKind {
    // Listed in the order they show up in the command palette by default!
    /// Save the recording, or all selected recordings.
    Save,

    /// Save the current time selection of the recording.
    SaveTimeSelection,

    /// Save the active blueprint of the recording.
    SaveBlueprint,

    /// Close the recording.
    Close,

    /// Undo the latest blueprint edit.
    Undo,

    /// Redo the latest undone blueprint edit.
    Redo,

    /// Add a view or container to the viewport.
    AddViewOrContainer,

    /// Reset the active blueprint to the default one.
    ClearActiveBlueprint,

    /// Reset the active blueprint to a heuristic one.
    ClearActiveBlueprintAndEnableHeuristics,

    ToggleTimePanel,
    ToggleChunkStoreBrowser,

    #[cfg(debug_assertions)]
    ToggleBlueprintInspectionPanel,

    // Playback:
    PlaybackTogglePlayPause,
    PlaybackStepBack,
    PlaybackStepForward,
    PlaybackBack,
    PlaybackForward,
    PlaybackBackFast,
    PlaybackForwardFast,
    PlaybackBeginning,
    PlaybackEndAndFollow,
    PlaybackSpeed(SetPlaybackSpeed),

    // Dev-tools:
    #[cfg(not(target_arch = "wasm32"))]
    PrintChunkStore,
    #[cfg(not(target_arch = "wasm32"))]
    PrintBlueprintStore,
    #[cfg(not(target_arch = "wasm32"))]
    PrintPrimaryCache,
}

impl RecordingCommand {
    /// All commands that act on the given recording.
    pub fn all_for_recording(recording_id: &StoreId) -> impl Iterator<Item = Self> {
        use strum::IntoEnumIterator as _;
        let recording_id = recording_id.clone();
        RecordingCommandKind::iter().map(move |kind| Self {
            recording_id: recording_id.clone(),
            kind,
        })
    }

    /// Show this command as a menu-button.
    ///
    /// If clicked, enqueue the command.
    pub fn menu_button_ui(
        self,
        ui: &mut egui::Ui,
        command_sender: &impl RecordingCommandSender,
    ) -> egui::Response {
        let Self { recording_id, kind } = self;
        kind.menu_button_ui(ui, Some(&recording_id), command_sender)
    }
}

impl RecordingCommandKind {
    /// Is this a "timeline" command, i.e. playback bound to a space/arrow/home/end key
    /// (or the playback-speed chord)?
    ///
    /// These keys must be consumed early (in `on_begin_pass`) so egui doesn't first use
    /// them to move keyboard focus or scroll — see [`super::consume_timeline_shortcut`].
    pub fn is_timeline(self) -> bool {
        matches!(
            self,
            Self::PlaybackTogglePlayPause
                | Self::PlaybackStepBack
                | Self::PlaybackStepForward
                | Self::PlaybackBack
                | Self::PlaybackForward
                | Self::PlaybackBackFast
                | Self::PlaybackForwardFast
                | Self::PlaybackBeginning
                | Self::PlaybackEndAndFollow
                | Self::PlaybackSpeed(_)
        )
    }

    /// Does this command only exist in debug builds?
    ///
    /// Such commands are marked with an orange "debug only" badge in the UI.
    #[cfg(debug_assertions)]
    pub fn is_debug_only(self) -> bool {
        matches!(self, Self::ToggleBlueprintInspectionPanel)
    }

    /// Pair this command with the active recording (from `env`) to make it dispatchable.
    ///
    /// Returns `None` when there is no active recording.
    pub fn for_environment(self, env: &CommandEnvironment) -> Option<RecordingCommand> {
        env.recording.clone().map(|recording_id| RecordingCommand {
            recording_id,
            kind: self,
        })
    }

    pub fn text(self) -> &'static str {
        self.text_and_tooltip().0
    }

    pub fn tooltip(self) -> &'static str {
        self.text_and_tooltip().1
    }

    pub fn text_and_tooltip(self) -> (&'static str, &'static str) {
        match self {
            Self::Save => (
                "保存录制文件…",
                "把全部数据保存为 Rerun 数据文件（.rrd）",
            ),

            Self::SaveTimeSelection => (
                "保存当前选中时间段…",
                "把当前循环选区内的数据保存为 Rerun 数据文件（.rrd）",
            ),

            Self::SaveBlueprint => (
                "保存 blueprint…",
                "把当前的 Viewer 布局保存为 Rerun blueprint 文件（.rbl）",
            ),

            Self::Close => (
                "关闭当前录制文件",
                "关闭当前录制文件（未保存的数据会丢失）",
            ),

            Self::Undo => (
                "撤销",
                "撤销当前录制文件上最近一次 blueprint 修改",
            ),
            Self::Redo => ("重做", "重做刚撤销的操作"),

            Self::AddViewOrContainer => (
                "添加视图或容器…",
                "在视口中添加一个新的视图或容器",
            ),

            Self::ClearActiveBlueprint => (
                "重置为默认 blueprint",
                "清除当前 blueprint，改用默认 blueprint。如果没有设置默认 blueprint，会改用自动推断的 blueprint。",
            ),

            Self::ClearActiveBlueprintAndEnableHeuristics => (
                "重置为自动推断的 blueprint",
                "用默认可视化器自动选择视图，重新填充视口",
            ),

            Self::ToggleTimePanel => ("显示/隐藏时间面板", "显示或隐藏底部面板"),
            Self::ToggleChunkStoreBrowser => (
                "显示/隐藏 chunk 存储浏览器",
                "显示或隐藏 chunk 存储浏览器",
            ),

            #[cfg(debug_assertions)]
            Self::ToggleBlueprintInspectionPanel => (
                "显示/隐藏 blueprint 检查面板",
                "查看内部 blueprint 数据的时间轴。",
            ),

            Self::PlaybackTogglePlayPause => ("播放/暂停", "播放或暂停时间"),
            Self::PlaybackStepBack => (
                "上一步",
                "把时间标记移到上一个有数据的时间点",
            ),
            Self::PlaybackStepForward => (
                "下一步",
                "把时间标记移到下一个有数据的时间点",
            ),
            Self::PlaybackBack => ("后退 1", "把时间标记后退 1 秒"),
            Self::PlaybackForward => ("前进 1", "把时间标记前进 0.1 秒"),
            Self::PlaybackBackFast => ("后退 10", "把时间标记后退 1 秒"),
            Self::PlaybackForwardFast => {
                ("前进 10", "把时间标记前进 0.1 秒")
            }
            Self::PlaybackBeginning => ("回到时间轴开头", "跳到时间轴的起点"),
            Self::PlaybackEndAndFollow => (
                "跳到时间轴末尾",
                "跳到时间轴末尾，并跟随不断流入的最新数据",
            ),

            Self::PlaybackSpeed(_) => (
                "设置播放速度",
                "这是组合按键：比如依次按 5、0 就是 50 倍速",
            ),

            #[cfg(not(target_arch = "wasm32"))]
            Self::PrintChunkStore => (
                "打印数据存储",
                "把整个 chunk 存储打印到控制台和剪贴板。注意：文本量可能非常大。",
            ),
            #[cfg(not(target_arch = "wasm32"))]
            Self::PrintBlueprintStore => (
                "打印 blueprint 存储",
                "把整个 blueprint 存储打印到控制台和剪贴板。注意：文本量可能非常大。",
            ),
            #[cfg(not(target_arch = "wasm32"))]
            Self::PrintPrimaryCache => (
                "打印主缓存",
                "把整个主缓存的状态打印到控制台和剪贴板。注意：文本量可能非常大。",
            ),
        }
    }

    pub fn icon(self) -> Option<&'static crate::Icon> {
        match self {
            Self::AddViewOrContainer => Some(&crate::icons::ADD),
            Self::ClearActiveBlueprint | Self::ClearActiveBlueprintAndEnableHeuristics => {
                Some(&crate::icons::RESET)
            }
            _ => None,
        }
    }

    /// All keyboard shortcuts, with the primary first.
    pub fn kb_shortcuts(self, os: OperatingSystem) -> SmallVec<[KeyboardShortcut; 2]> {
        fn key(key: Key) -> KeyboardShortcut {
            KeyboardShortcut::new(Modifiers::NONE, key)
        }

        fn ctrl(key: Key) -> KeyboardShortcut {
            KeyboardShortcut::new(Modifiers::CTRL, key)
        }

        fn cmd(key: Key) -> KeyboardShortcut {
            KeyboardShortcut::new(Modifiers::COMMAND, key)
        }

        fn alt(key: Key) -> KeyboardShortcut {
            KeyboardShortcut::new(Modifiers::ALT, key)
        }

        fn shift(key: Key) -> KeyboardShortcut {
            KeyboardShortcut::new(Modifiers::SHIFT, key)
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
            Self::Save => smallvec![cmd(Key::S)],
            Self::SaveTimeSelection => smallvec![cmd_alt(Key::S)],
            Self::SaveBlueprint => smallvec![],
            Self::Close => smallvec![],

            Self::Undo => smallvec![cmd(Key::Z)],
            Self::Redo => {
                if os == OperatingSystem::Mac {
                    smallvec![cmd_shift(Key::Z), cmd(Key::Y)]
                } else {
                    smallvec![ctrl(Key::Y), ctrl_shift(Key::Z)]
                }
            }

            Self::AddViewOrContainer => smallvec![],
            Self::ClearActiveBlueprint => smallvec![],
            Self::ClearActiveBlueprintAndEnableHeuristics => smallvec![],

            Self::ToggleTimePanel => smallvec![ctrl_shift(Key::T)],
            Self::ToggleChunkStoreBrowser => smallvec![ctrl_shift(Key::D)],

            #[cfg(debug_assertions)]
            Self::ToggleBlueprintInspectionPanel => smallvec![ctrl_shift(Key::I)],

            Self::PlaybackTogglePlayPause => smallvec![key(Key::Space)],
            Self::PlaybackStepBack => smallvec![cmd(Key::ArrowLeft)],
            Self::PlaybackStepForward => smallvec![cmd(Key::ArrowRight)],
            Self::PlaybackBack => smallvec![key(Key::ArrowLeft)],
            Self::PlaybackForward => smallvec![key(Key::ArrowRight)],
            Self::PlaybackBackFast => smallvec![shift(Key::ArrowLeft)],
            Self::PlaybackForwardFast => smallvec![shift(Key::ArrowRight)],
            Self::PlaybackBeginning => smallvec![key(Key::Home)],
            Self::PlaybackEndAndFollow => smallvec![key(Key::End), alt(Key::ArrowRight)],

            Self::PlaybackSpeed(_) => {
                // This is a chord, so no single shortcut.
                smallvec![]
            }

            #[cfg(not(target_arch = "wasm32"))]
            Self::PrintChunkStore | Self::PrintBlueprintStore | Self::PrintPrimaryCache => {
                smallvec![]
            }
        }
    }

    /// Primary keyboard shortcut
    pub fn primary_kb_shortcut(self, os: OperatingSystem) -> Option<KeyboardShortcut> {
        self.kb_shortcuts(os).first().copied()
    }

    /// Return the keyboard shortcut for this command, nicely formatted
    pub fn formatted_kb_shortcut(self, egui_ctx: &egui::Context) -> Option<String> {
        if matches!(self, Self::PlaybackSpeed(_)) {
            return Some("01-99".to_owned());
        }
        // Note: we only show the primary shortcut to the user.
        // The fallbacks are there for people who have muscle memory for the other shortcuts.
        self.primary_kb_shortcut(egui_ctx.os())
            .map(|shortcut| egui_ctx.format_shortcut(&shortcut))
    }

    /// Show this command as a menu-button.
    ///
    /// Disabled if `recording_id` is `None`;
    /// otherwise, if clicked, enqueue the command for that recording.
    pub fn menu_button_ui(
        self,
        ui: &mut egui::Ui,
        recording_id: Option<&StoreId>,
        command_sender: &impl RecordingCommandSender,
    ) -> egui::Response {
        let button = self.menu_button(ui.ctx());
        let response = ui
            .add_enabled(recording_id.is_some(), button)
            .on_hover_text(self.tooltip());

        if response.clicked()
            && let Some(recording_id) = recording_id
        {
            command_sender.send_recording_command(RecordingCommand {
                recording_id: recording_id.clone(),
                kind: self,
            });
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

    /// A chord for setting the playback speed: type e.g. `5` then `0` for 50x speed.
    pub(super) fn handle_playback_chord(ctx: &egui::Context) -> Option<Self> {
        const CHORD_TIMEOUT: std::time::Duration = std::time::Duration::from_millis(500);
        const NUMBER_KEYS: [Key; 10] = [
            Key::Num0,
            Key::Num1,
            Key::Num2,
            Key::Num3,
            Key::Num4,
            Key::Num5,
            Key::Num6,
            Key::Num7,
            Key::Num8,
            Key::Num9,
        ];

        fn key_to_digit(key: Key) -> Option<char> {
            let i = NUMBER_KEYS.iter().position(|&k| k == key)?;
            char::from_digit(i as u32, 10)
        }

        #[derive(Default, Clone)]
        struct PlaybackChordState {
            last_key_time: Option<web_time::Instant>,
            accumulated: String,
        }

        if ctx.text_edit_focused() {
            return None;
        }

        let mut chord_state = ctx.data_mut(|data| {
            data.get_temp_mut_or_default::<PlaybackChordState>(Id::NULL)
                .clone()
        });

        let now = web_time::Instant::now();

        let pressed_number = ctx.input(|i| {
            let mut pressed_number = NUMBER_KEYS.iter().find(|&&k| i.key_pressed(k)).copied();
            let has_other = i.keys_down.iter().any(|k| !NUMBER_KEYS.contains(k));

            if has_other || i.modifiers.any() {
                chord_state = PlaybackChordState::default();
                pressed_number = None;
            }

            pressed_number
        });

        // Check if timeout expired - clear old state
        if let Some(last_time) = chord_state.last_key_time
            && now.duration_since(last_time) >= CHORD_TIMEOUT
        {
            chord_state = PlaybackChordState::default();
        }

        let mut command = None;

        // Handle number key press
        if let Some(key) = pressed_number {
            if let Some(digit) = key_to_digit(key) {
                // Cap the length so key-repeat (e.g. holding `0`) can't grow this
                // unboundedly and overflow the `10.pow(leading_zeros)` below.
                if chord_state.accumulated.len() < 8 {
                    chord_state.accumulated.push(digit);
                }
            }

            chord_state.last_key_time = Some(now);

            // Leading zeros should divide the speed by 10 for each zero.
            // So e.g. 05 = 0.5x speed, 005 = 0.05x speed, etc.
            let leading_zeros = chord_state
                .accumulated
                .chars()
                .take_while(|&c| c == '0')
                .count();

            let factor = 10usize.pow(leading_zeros as u32);

            if let Ok(speed) = chord_state.accumulated.parse::<f32>()
                && speed > 0.0
            {
                command = Some(Self::PlaybackSpeed(SetPlaybackSpeed(
                    egui::emath::OrderedFloat(speed / factor as f32),
                )));
            }
        }

        ctx.data_mut(|data| data.insert_temp(Id::NULL, chord_state.clone()));

        command
    }
}
