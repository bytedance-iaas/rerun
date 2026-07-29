use re_viewer_context::Item;

use crate::{ContextMenuAction, ContextMenuContext};

/// Copy the artifacts-store address of an episode's converted rrd (shown only when one exists).
pub struct CopyRrdArtifactUrl;

impl ContextMenuAction for CopyRrdArtifactUrl {
    fn supports_item(&self, _ctx: &ContextMenuContext<'_>, item: &Item) -> bool {
        match item {
            Item::StoreId(store_id) => {
                re_data_source::lerobot_remote::episode_rrd_artifact_url(store_id).is_some()
            }
            _ => false,
        }
    }

    fn label(&self, _ctx: &ContextMenuContext<'_>) -> String {
        "Copy rrd artifact address".to_owned()
    }

    fn process_store_id(&self, ctx: &ContextMenuContext<'_>, store_id: &re_log_types::StoreId) {
        if let Some(url) = re_data_source::lerobot_remote::episode_rrd_artifact_url(store_id) {
            ctx.egui_context().copy_text(url);
        }
    }
}
