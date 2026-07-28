//! Helpers to turn `LeRobot` episodes into importer messages (`LogMsg`s).
//!
//! The dataset parsing itself lives in the `re_lerobot` crate; this module glues it to the
//! importer's message types so that both the local importer and remote streaming drivers
//! (e.g. `re_data_source`'s TOS / Hugging Face loaders) can share the same recording layout.

use re_chunk::{Chunk, EntityPath, RowId, TimePoint};
use re_lerobot::{EpisodeIndex, common::LeRobotDataset};
use re_log_types::{ApplicationId, StoreId};

use crate::{ImporterError, import_file::prepare_store_info};

/// The store id of a recording belonging to a (remote) dataset.
pub fn recording_store_id(application_id: &ApplicationId, recording_id: &str) -> StoreId {
    StoreId::recording(application_id.clone(), recording_id.to_owned())
}

/// The `SetStoreInfo` message announcing a recording of a (remote) dataset.
pub fn recording_store_info_msg(
    application_id: &ApplicationId,
    recording_id: &str,
) -> re_log_types::LogMsg {
    prepare_store_info(
        &recording_store_id(application_id, recording_id),
        re_log_types::FileSource::Sdk,
    )
}

/// A message setting the display name of a recording of a (remote) dataset.
pub fn recording_properties_msg(
    application_id: &ApplicationId,
    recording_id: &str,
    recording_name: &str,
) -> Result<re_log_types::LogMsg, ImporterError> {
    let store_id = recording_store_id(application_id, recording_id);

    let recording_info =
        re_sdk_types::archetypes::RecordingInfo::new().with_name(recording_name.to_owned());
    let chunk = Chunk::builder(EntityPath::properties())
        .with_archetype(RowId::new(), TimePoint::STATIC, &recording_info)
        .build()?;

    Ok(re_log_types::LogMsg::ArrowMsg(
        store_id,
        chunk.to_arrow_msg()?,
    ))
}

/// The store id used for one episode's recording.
pub fn episode_store_id(application_id: &ApplicationId, episode: EpisodeIndex) -> StoreId {
    recording_store_id(application_id, &format!("episode_{}", episode.0))
}

/// The recording id used for one episode's recording — the counterpart of [`episode_store_id`].
pub fn episode_index_from_recording_id(recording_id: &str) -> Option<EpisodeIndex> {
    recording_id
        .strip_prefix("episode_")
        .and_then(|n| n.parse().ok())
        .map(EpisodeIndex)
}

/// The `SetStoreInfo` message announcing one episode's recording.
pub fn episode_store_info_msg(
    application_id: &ApplicationId,
    episode: EpisodeIndex,
) -> re_log_types::LogMsg {
    prepare_store_info(
        &episode_store_id(application_id, episode),
        re_log_types::FileSource::Sdk,
    )
}

/// A message setting the display name of one episode's recording.
///
/// Sending this right after the `SetStoreInfo` gives the (still empty) recording a useful label
/// while its data is downloading.
pub fn episode_properties_msg(
    application_id: &ApplicationId,
    episode: EpisodeIndex,
    recording_name: &str,
) -> Result<re_log_types::LogMsg, ImporterError> {
    recording_properties_msg(
        application_id,
        &format!("episode_{}", episode.0),
        recording_name,
    )
}

/// Convert one episode into `LogMsg`s: the recording properties followed by the episode's data.
pub fn episode_log_msgs<D: LeRobotDataset>(
    dataset: &D,
    application_id: &ApplicationId,
    episode: EpisodeIndex,
    recording_name: &str,
) -> Result<Vec<re_log_types::LogMsg>, ImporterError> {
    let store_id = episode_store_id(application_id, episode);

    let initial = episode_properties_msg(application_id, episode, recording_name)?;
    let chunks = dataset
        .load_episode_chunks(episode)
        .map_err(|err| ImporterError::Other(anyhow::Error::new(err)))?;

    std::iter::chain(
        std::iter::once(Ok(initial)),
        chunks.into_iter().map(|chunk| {
            Ok(re_log_types::LogMsg::ArrowMsg(
                store_id.clone(),
                chunk.to_arrow_msg()?,
            ))
        }),
    )
    .collect()
}
