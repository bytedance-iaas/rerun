//! Smoke test of the Hugging Face `LeRobot` streaming pipeline against real repos.
//!
//! Hits the network (huggingface.co), so these are `#[ignore]`d by default. Run with:
//!
//! ```sh
//! cargo test -p re_data_source --test hf_smoke -- --ignored --nocapture
//! ```

use std::collections::HashMap;
use std::time::{Duration, Instant};

use re_log_channel::SmartMessagePayload;
use re_log_types::LogMsg;

const HF_TEST_DATASET: &str = "henry-guo/so101-pick-place";

/// after `cancel_dataset_stream`, the channel quits cleanly and the stream unregisters.
#[test]
#[ignore = "hits the network (huggingface.co)"]
fn hf_cancel_stream_stops_downloads() {
    re_log::setup_logging();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();

    let app_id = format!("hf://{HF_TEST_DATASET}");
    let rx = re_data_source::hf::stream_lerobot_dataset(re_data_source::hf::HfDatasetSource {
        repo: HF_TEST_DATASET.to_owned(),
        file_path: None,
        token: String::new(),
    });

    // Wait until the stream is up (episodes announced ⇒ the item loop is running).
    let started = Instant::now();
    loop {
        assert!(started.elapsed() < Duration::from_mins(2), "timed out");
        let msg = rx
            .recv_timeout(Duration::from_mins(1))
            .expect("channel starved");
        match msg.payload {
            SmartMessagePayload::Msg(_) => {
                if re_data_source::lerobot_remote::is_dataset_streaming(&app_id) {
                    break;
                }
            }
            SmartMessagePayload::Flush { on_flush_done } => on_flush_done(),
            SmartMessagePayload::Quit(err) => panic!("stream quit early: {err:?}"), // NOLINT: tests print errors via Debug on purpose
        }
    }

    re_data_source::lerobot_remote::cancel_dataset_stream(&app_id);

    // The stream must wind down with a clean quit — soon, even mid-download.
    let cancelled_at = Instant::now();
    loop {
        assert!(
            cancelled_at.elapsed() < Duration::from_secs(30),
            "stream did not stop within 30s of cancellation"
        );
        let msg = rx
            .recv_timeout(Duration::from_secs(30))
            .expect("channel starved after cancel");
        match msg.payload {
            SmartMessagePayload::Msg(_) => {} // In-flight messages may still drain.
            SmartMessagePayload::Flush { on_flush_done } => on_flush_done(),
            SmartMessagePayload::Quit(err) => {
                assert!(err.is_none(), "expected a clean quit, got: {err:?}"); // NOLINT: tests print errors via Debug on purpose
                break;
            }
        }
    }

    assert!(
        !re_data_source::lerobot_remote::is_dataset_streaming(&app_id),
        "stream still registered after cancellation"
    );
    println!(
        "OK: stream stopped {:.1}s after cancel",
        cancelled_at.elapsed().as_secs_f32()
    );
}

/// Re-downloading a loaded episode (the mark + close-hook sequence, exactly what the UI's
/// re-download button produces) must re-announce the recording and fetch it again —
/// regression test for the "episode disappears forever after clicking re-download" bug.
#[test]
#[ignore = "hits the network (huggingface.co)"]
fn hf_redownload_episode() {
    re_log::setup_logging();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();

    let rx = re_data_source::hf::stream_lerobot_dataset(re_data_source::hf::HfDatasetSource {
        repo: HF_TEST_DATASET.to_owned(),
        file_path: None,
        token: String::new(),
    });

    let ep0 = re_log_types::StoreId::recording(
        re_log_types::ApplicationId::from(format!("hf://{HF_TEST_DATASET}")),
        "episode_0".to_owned(),
    );

    let count_until = |min_data_msgs: usize, want_store_info: bool, what: &str| {
        let mut data_msgs = 0usize;
        let mut store_infos = 0usize;
        let started = Instant::now();
        loop {
            assert!(
                started.elapsed() < Duration::from_mins(3),
                "timed out waiting for {what}: {data_msgs} data msgs, {store_infos} store infos"
            );
            let msg = rx
                .recv_timeout(Duration::from_mins(2))
                .expect("channel starved");
            match msg.payload {
                SmartMessagePayload::Msg(re_log_channel::DataSourceMessage::LogMsg(log_msg)) => {
                    match log_msg {
                        LogMsg::SetStoreInfo(info)
                            if info.info.store_id.recording_id().as_str() == "episode_0" =>
                        {
                            store_infos += 1;
                        }
                        LogMsg::ArrowMsg(store_id, _)
                            if store_id.recording_id().as_str() == "episode_0" =>
                        {
                            data_msgs += 1;
                        }
                        _ => {}
                    }
                }
                SmartMessagePayload::Msg(_) => {}
                SmartMessagePayload::Flush { on_flush_done } => on_flush_done(),
                SmartMessagePayload::Quit(err) => panic!("stream quit early: {err:?}"), // NOLINT: tests print errors via Debug on purpose
            }
            if data_msgs >= min_data_msgs && (!want_store_info || store_infos > 0) {
                return (data_msgs, store_infos);
            }
        }
    };

    // Phase 1: wait until episode 0 is fully announced + loaded.
    count_until(5, true, "the initial episode 0 load");

    // Phase 2: what the UI's re-download button does — arm the marker, then the close hook
    // fires once the viewer has dropped the old recording.
    assert!(re_data_source::lerobot_remote::redownload_episode_for_store(&ep0));
    re_data_source::lerobot_remote::cancel_episode_for_store(&ep0);

    // Phase 3: the episode must be announced again and its data must arrive again.
    let (data_msgs, store_infos) = count_until(5, true, "the re-download");
    println!(
        "OK: episode 0 re-announced ({store_infos} store infos) and re-loaded ({data_msgs} data msgs)"
    );
}

/// The dataset that originally hung: `LeRobot` v2.0, 53k episodes, ~266k files.
/// Exercises the v2 path, the no-full-listing metadata flow, and the episode cap.
#[test]
#[ignore = "hits the network (huggingface.co)"]
fn hf_lerobot_v2_stream_smoke() {
    re_log::setup_logging();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();

    let rx = re_data_source::hf::stream_lerobot_dataset(re_data_source::hf::HfDatasetSource {
        repo: "jesbu1/bridge_v2_lerobot".to_owned(),
        file_path: None,
        token: String::new(),
    });

    let mut store_infos = 0usize;
    let mut episodes_msgs: HashMap<String, usize> = HashMap::new();
    let started = Instant::now();

    loop {
        assert!(
            started.elapsed() < Duration::from_mins(5),
            "timed out; so far: {store_infos} store infos from {} episodes",
            episodes_msgs.len()
        );

        let msg = rx
            .recv_timeout(Duration::from_mins(2))
            .expect("channel starved");

        match msg.payload {
            SmartMessagePayload::Msg(data) => {
                if let re_log_channel::DataSourceMessage::LogMsg(log_msg) = data {
                    match log_msg {
                        LogMsg::SetStoreInfo(info) => {
                            store_infos += 1;
                            if store_infos.is_multiple_of(100) || store_infos == 1 {
                                println!(
                                    "SetStoreInfo #{store_infos}: {}",
                                    info.info.store_id.recording_id()
                                );
                            }
                        }
                        LogMsg::ArrowMsg(store_id, _) => {
                            *episodes_msgs
                                .entry(store_id.recording_id().as_str().to_owned())
                                .or_default() += 1;
                        }
                        LogMsg::BlueprintActivationCommand(_) => {}
                    }
                }
            }
            SmartMessagePayload::Flush { on_flush_done } => on_flush_done(),
            SmartMessagePayload::Quit(err) => {
                panic!("stream quit early: {err:?}"); // NOLINT: tests print errors via Debug on purpose
            }
        }

        let loaded = episodes_msgs.values().filter(|&&n| n >= 5).count();
        if store_infos >= 200 && loaded >= 2 {
            println!("OK: {store_infos} episodes announced (capped), {loaded} loaded");
            break;
        }
    }

    // The 53k-episode dataset must be capped at 200 announced episodes,
    // plus the synthetic "… more" entry.
    assert_eq!(store_infos, 201);

    // Clicking the "… more" entry (simulated via the prioritize hook) must announce
    // the next batch of 200.
    let more_store = re_log_types::StoreId::recording(
        re_log_types::ApplicationId::from("hf://jesbu1/bridge_v2_lerobot".to_owned()),
        "more".to_owned(),
    );
    assert!(re_data_source::lerobot_remote::prioritize_episode_for_store(&more_store));

    let mut new_infos = 0;
    let started = Instant::now();
    while new_infos < 200 {
        assert!(
            started.elapsed() < Duration::from_mins(2),
            "timed out waiting for the next batch; got {new_infos}"
        );
        let msg = rx
            .recv_timeout(Duration::from_mins(1))
            .expect("channel starved");
        if let SmartMessagePayload::Msg(re_log_channel::DataSourceMessage::LogMsg(
            LogMsg::SetStoreInfo(info),
        )) = msg.payload
        {
            let id = info.info.store_id.recording_id().as_str().to_owned();
            if id.starts_with("episode_") {
                new_infos += 1;
            }
        }
    }
    println!("OK: next batch of {new_infos} episodes announced after clicking 'more'");
}

/// A repo of loose .mcap files: every file must be announced as a recording (metadata only —
/// the files are GBs each, so the test hangs up before any download finishes).
#[test]
#[ignore = "hits the network (huggingface.co)"]
fn hf_mcap_repo_announces_files() {
    re_log::setup_logging();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();

    let rx = re_data_source::hf::stream_lerobot_dataset(re_data_source::hf::HfDatasetSource {
        repo: "cortexdatalabs/MCAP-Housing".to_owned(),
        file_path: None,
        token: String::new(),
    });

    let mut announced = Vec::new();
    let started = Instant::now();

    while announced.len() < 11 {
        assert!(
            started.elapsed() < Duration::from_mins(2),
            "timed out; announced so far: {announced:?}"
        );
        let msg = rx
            .recv_timeout(Duration::from_mins(1))
            .expect("channel starved");
        match msg.payload {
            SmartMessagePayload::Msg(re_log_channel::DataSourceMessage::LogMsg(
                LogMsg::SetStoreInfo(info),
            )) => {
                announced.push(info.info.store_id.recording_id().as_str().to_owned());
            }
            SmartMessagePayload::Quit(err) => panic!("stream quit early: {err:?}"), // NOLINT: tests print errors via Debug on purpose
            _ => {}
        }
    }

    println!("announced recordings: {announced:?}");
    assert!(announced.iter().any(|id| id.starts_with("file_")));
}

/// A repo that is not a `LeRobot` dataset must be rejected with a clear error, not hang.
#[test]
#[ignore = "hits the network (huggingface.co)"]
fn hf_non_lerobot_is_rejected() {
    re_log::setup_logging();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();

    let rx = re_data_source::hf::stream_lerobot_dataset(re_data_source::hf::HfDatasetSource {
        repo: "rerun-io/does-not-exist-4a7f".to_owned(),
        file_path: None,
        token: String::new(),
    });

    let started = Instant::now();
    loop {
        assert!(started.elapsed() < Duration::from_mins(2), "timed out");
        let msg = rx
            .recv_timeout(Duration::from_mins(1))
            .expect("channel starved");
        if let SmartMessagePayload::Quit(err) = msg.payload {
            let err = err.expect("expected a rejection error, got a clean quit");
            let text = err.to_string();
            println!("rejection message: {text}");
            assert!(
                text.contains("does not look like a LeRobot dataset"),
                "unexpected error text: {text}"
            );
            return;
        }
    }
}

/// Same shape as `tos_lerobot_stream_smoke`, but for a public Hugging Face dataset (no token).
#[test]
#[ignore = "hits the network (huggingface.co)"]
fn hf_lerobot_stream_smoke() {
    re_log::setup_logging();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();

    let rx = re_data_source::hf::stream_lerobot_dataset(re_data_source::hf::HfDatasetSource {
        repo: HF_TEST_DATASET.to_owned(),
        file_path: None,
        token: String::new(),
    });

    let mut store_infos = 0usize;
    let mut episodes_msgs: HashMap<String, usize> = HashMap::new();
    let started = Instant::now();

    loop {
        assert!(
            started.elapsed() < Duration::from_mins(10),
            "timed out; so far: {store_infos} store infos from {} episodes",
            episodes_msgs.len()
        );

        let msg = rx
            .recv_timeout(Duration::from_mins(2))
            .expect("channel starved");

        match msg.payload {
            SmartMessagePayload::Msg(data) => {
                if let re_log_channel::DataSourceMessage::LogMsg(log_msg) = data {
                    match log_msg {
                        LogMsg::SetStoreInfo(info) => {
                            store_infos += 1;
                            println!(
                                "SetStoreInfo #{store_infos}: {}",
                                info.info.store_id.recording_id()
                            );
                        }
                        LogMsg::ArrowMsg(store_id, _) => {
                            *episodes_msgs
                                .entry(store_id.recording_id().as_str().to_owned())
                                .or_default() += 1;
                        }
                        LogMsg::BlueprintActivationCommand(_) => {}
                    }
                }
            }
            SmartMessagePayload::Flush { on_flush_done } => on_flush_done(),
            SmartMessagePayload::Quit(err) => {
                panic!("stream quit early: {err:?}"); // NOLINT: tests print errors via Debug on purpose
            }
        }

        let loaded: Vec<&String> = episodes_msgs
            .iter()
            .filter(|&(_, &n)| n >= 5)
            .map(|(k, _)| k)
            .collect();
        if store_infos >= 2 && loaded.len() >= 2 {
            println!("OK: {store_infos} episodes announced, loaded: {loaded:?}");
            break;
        }
    }
}
