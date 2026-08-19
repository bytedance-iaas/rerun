use re_viewer_context::Item;

use crate::{ContextMenuAction, ContextMenuContext};

/// Ask to delete one episode's converted rrd from the artifacts store.
///
/// Deletion is destructive, so this only *queues a request*: the viewer shows a
/// confirmation dialog and deletes on confirm (`rrd_artifacts::request_deletion`).
pub struct DeleteRrdArtifact;

impl ContextMenuAction for DeleteRrdArtifact {
    fn supports_item(&self, _ctx: &ContextMenuContext<'_>, item: &Item) -> bool {
        match item {
            Item::StoreId(store_id) => {
                re_data_source::lerobot_remote::episode_rrd_artifact_url(store_id).is_some()
            }
            _ => false,
        }
    }

    fn label(&self, _ctx: &ContextMenuContext<'_>) -> String {
        "Delete rrd artifact…".to_owned()
    }

    fn process_store_id(&self, _ctx: &ContextMenuContext<'_>, store_id: &re_log_types::StoreId) {
        let Some(target_url) = re_data_source::lerobot_remote::episode_rrd_artifact_url(store_id)
        else {
            return;
        };
        let app_id = store_id.application_id();
        re_data_source::rrd_artifacts::request_deletion(
            re_data_source::rrd_artifacts::ArtifactDeletionRequest {
                application_id: app_id.to_string(),
                dataset_url: re_data_source::lerobot_remote::dataset_url_of(app_id.as_str())
                    .unwrap_or_else(|| app_id.to_string()),
                episode: re_data_source::lerobot_remote::episode_queue_index(store_id),
                target_url,
            },
        );
    }
}

/// Ask to delete ALL converted rrds of a dataset from the artifacts store
/// (same confirmation flow as [`DeleteRrdArtifact`]).
pub struct DeleteDatasetRrdArtifacts;

impl ContextMenuAction for DeleteDatasetRrdArtifacts {
    fn supports_item(&self, _ctx: &ContextMenuContext<'_>, item: &Item) -> bool {
        match item {
            Item::AppId(app_id) => {
                re_data_source::lerobot_remote::dataset_artifact_count(app_id.as_str()) > 0
                    && re_data_source::lerobot_remote::dataset_artifacts_config(app_id.as_str())
                        .is_some()
            }
            _ => false,
        }
    }

    fn label(&self, _ctx: &ContextMenuContext<'_>) -> String {
        "Delete all rrd artifacts…".to_owned()
    }

    fn process_app_id(&self, _ctx: &ContextMenuContext<'_>, app_id: &re_log_types::ApplicationId) {
        let Some(config) =
            re_data_source::lerobot_remote::dataset_artifacts_config(app_id.as_str())
        else {
            return;
        };
        let dataset_url = re_data_source::lerobot_remote::dataset_url_of(app_id.as_str())
            .unwrap_or_else(|| app_id.to_string());
        let dir = re_data_source::rrd_artifacts::dataset_artifacts_dir(
            &config.location.prefix,
            &dataset_url,
        );
        re_data_source::rrd_artifacts::request_deletion(
            re_data_source::rrd_artifacts::ArtifactDeletionRequest {
                application_id: app_id.to_string(),
                dataset_url,
                episode: None,
                target_url: format!("tos://{}/{dir}", config.location.bucket),
            },
        );
    }
}
