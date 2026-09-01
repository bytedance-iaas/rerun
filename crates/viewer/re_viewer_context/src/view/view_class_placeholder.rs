use re_chunk::EntityPath;
use re_chunk_store::MissingChunkReporter;
use re_i18n::tr;
use re_sdk_types::ViewClassIdentifier;
use re_ui::{Help, UiExt as _};

use crate::{
    SystemExecutionOutput, ViewClass, ViewClassRegistryError, ViewClassUiOutput, ViewQuery,
    ViewSpawnHeuristics, ViewState, ViewSystemExecutionError, ViewSystemRegistrator, ViewerContext,
    ViewerDiagnostic, ViewerReportSeverity,
};

/// A placeholder view class that can be used when the actual class is not registered.
#[derive(Default)]
pub struct ViewClassPlaceholder;

impl ViewClass for ViewClassPlaceholder {
    fn identifier() -> ViewClassIdentifier {
        re_string_interner::intern_static_nonempty!(ViewClassIdentifier, "UnknownViewClass")
    }

    fn display_name(&self) -> &'static str {
        tr("Unknown view class", "未知视图类型")
    }

    fn icon(&self) -> &'static re_ui::Icon {
        &re_ui::icons::VIEW_UNKNOWN
    }

    fn help(&self, _os: egui::os::OperatingSystem) -> Help {
        Help::new(tr("Placeholder view", "占位视图")).markdown(tr(
            "Placeholder view for unknown view class",
            "未知视图类型的占位视图",
        ))
    }

    fn on_register(
        &self,
        _system_registry: &mut ViewSystemRegistrator<'_>,
    ) -> Result<(), ViewClassRegistryError> {
        Ok(())
    }

    fn new_state(&self) -> Box<dyn ViewState> {
        Box::<()>::default()
    }

    fn layout_priority(&self) -> crate::ViewClassLayoutPriority {
        crate::ViewClassLayoutPriority::Low
    }

    fn spawn_heuristics(
        &self,
        _ctx: &ViewerContext<'_>,
        _include_entity: &dyn Fn(&EntityPath) -> bool,
    ) -> ViewSpawnHeuristics {
        ViewSpawnHeuristics::empty()
    }

    fn ui(
        &self,
        _ctx: &ViewerContext<'_>,
        _missing_chunk_reporter: &MissingChunkReporter,
        ui: &mut egui::Ui,
        _state: &mut dyn ViewState,
        _query: &ViewQuery<'_>,
        _system_output: SystemExecutionOutput,
    ) -> Result<ViewClassUiOutput, ViewSystemExecutionError> {
        let tokens = ui.tokens();

        let error_details = "出现这种情况，可能是 blueprint 指定了无效的视图类型，\
                或者当前版本的 viewer 不认识这个类型。\n\n\
                \
                **注意**：有些视图需要启用特定的 Cargo feature。\
                比如地图视图需要 `map_view` feature。";

        egui::Frame {
            inner_margin: egui::Margin::same(tokens.view_padding()),
            ..Default::default()
        }
        .show(ui, |ui| {
            ui.error_label("未知的视图类型");
            ui.markdown_ui(error_details);
        });

        Ok(ViewClassUiOutput::default().with_report(ViewerDiagnostic {
            severity: ViewerReportSeverity::Error,
            summary: "Unknown view class".into(),
            details: Some(error_details.to_owned()),
        }))
    }
}
