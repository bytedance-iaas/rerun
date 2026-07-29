//! Smoke test of the TOS `LeRobot` streaming pipeline against a real bucket.
//!
//! Needs credentials, so it is ignored by default. Run with:
//!
//! ```sh
//! TOS_ACCESS_KEY=… TOS_SECRET_KEY=… \
//!   cargo test -p re_data_source --test tos_smoke -- --ignored --nocapture
//! ```

use std::collections::HashMap;
use std::time::{Duration, Instant};

use re_data_source::tos::{
    TosClient, TosCredentials, TosDatasetSource, TosLocation, stream_lerobot_dataset,
};
use re_log_channel::SmartMessagePayload;
use re_log_types::LogMsg;

fn credentials() -> TosCredentials {
    TosCredentials {
        endpoint: std::env::var("TOS_ENDPOINT")
            .unwrap_or_else(|_| "https://tos-s3-cn-beijing.volces.com".to_owned()),
        region: std::env::var("TOS_REGION").unwrap_or_else(|_| "cn-beijing".to_owned()),
        access_key: std::env::var("TOS_ACCESS_KEY").expect("set TOS_ACCESS_KEY"),
        secret_key: std::env::var("TOS_SECRET_KEY").expect("set TOS_SECRET_KEY"),
    }
}

const TEST_DATASET: &str = "tos://physical-ai-rerun-test/dataset-1/so101-pick-place/";

/// End-to-end round-trip of the rrd-artifacts primitives against the real bucket:
/// PUT with fingerprint metadata → HEAD returns it → GET returns the bytes.
#[test]
#[ignore = "needs real TOS credentials via TOS_ACCESS_KEY / TOS_SECRET_KEY"]
fn tos_rrd_artifacts_roundtrip() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        use re_data_source::rrd_artifacts;

        let location = TosLocation::parse(rrd_artifacts::DEFAULT_RRD_ARTIFACTS_URL).unwrap();
        let client = TosClient::new(credentials(), location.bucket.clone());

        let key = format!("{}_selftest/roundtrip.rrd", location.prefix);
        let payload = b"rrd-artifacts self test payload".to_vec();
        let fingerprint = "test-fingerprint-123";

        client
            .put_object(
                &key,
                payload.clone(),
                &[
                    (
                        rrd_artifacts::FINGERPRINT_METADATA_KEY.to_owned(),
                        fingerprint.to_owned(),
                    ),
                    (
                        rrd_artifacts::SOURCE_URL_METADATA_KEY.to_owned(),
                        "tos://selftest/".to_owned(),
                    ),
                ],
            )
            .await
            .unwrap();
        println!("PUT ok: {key}");

        let head = client.head_object(&key).await.unwrap().expect("must exist");
        println!(
            "HEAD ok: {} bytes, metadata: {:?}",
            head.size, head.metadata
        );
        assert_eq!(head.size, payload.len() as u64);
        let stored_fingerprint = head
            .metadata
            .iter()
            .find(|(name, _)| name == rrd_artifacts::FINGERPRINT_METADATA_KEY)
            .map(|(_, value)| value.as_str());
        assert_eq!(stored_fingerprint, Some(fingerprint));

        let fetched = client.get_object(&key, None).await.unwrap();
        assert_eq!(fetched, payload);

        // A key that does not exist is a clean miss, not an error.
        let missing = client
            .head_object(&format!("{}_selftest/missing.rrd", location.prefix))
            .await
            .unwrap();
        assert!(missing.is_none());

        println!("rrd-artifacts roundtrip ok");
    });
}

#[test]
#[ignore = "needs real TOS credentials via TOS_ACCESS_KEY / TOS_SECRET_KEY"]
fn tos_list_and_get() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let location = TosLocation::parse(TEST_DATASET).unwrap();
        let client = TosClient::new(credentials(), location.bucket.clone());

        let objects = client.list_objects(&location.prefix).await.unwrap();
        println!("listed {} objects:", objects.len());
        for obj in &objects {
            println!("  {} ({} bytes)", obj.key, obj.size);
        }
        assert!(!objects.is_empty());

        let info_key = format!("{}meta/info.json", location.prefix);
        let info = client.get_object(&info_key, None).await.unwrap();
        println!("info.json: {} bytes", info.len());
        assert!(info.starts_with(b"{"));

        // Byte-range read.
        let ranged = client.get_object(&info_key, Some(0..16)).await.unwrap();
        assert_eq!(ranged.len(), 16);
        assert_eq!(&ranged[..], &info[..16]);
    });
}

#[test]
#[ignore = "needs real TOS credentials via TOS_ACCESS_KEY / TOS_SECRET_KEY"]
fn tos_lerobot_stream_smoke() {
    re_log::setup_logging();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _guard = rt.enter();

    let source = TosDatasetSource {
        location: TosLocation::parse(TEST_DATASET).unwrap(),
        credentials: credentials(),
        rrd_artifacts: None,
    };
    let rx = stream_lerobot_dataset(source);

    // Mimic the viewer auto-selecting the last episode: jump it to the front of the queue.
    std::thread::Builder::new()
        .name("tos-smoke-prioritize".to_owned())
        .spawn(|| {
            std::thread::sleep(Duration::from_secs(25));
            let store_id = re_log_types::StoreId::recording(
                re_log_types::ApplicationId::from(TEST_DATASET.to_owned()),
                "episode_46".to_owned(),
            );
            let found = re_data_source::lerobot_remote::prioritize_episode_for_store(&store_id);
            println!("prioritized episode 46: {found}");
        })
        .unwrap();

    let mut store_infos = 0usize;
    // Per-episode ArrowMsg counts. The single announce-time properties message doesn't count as
    // "loaded" — require several data messages per episode.
    let mut episodes_msgs: HashMap<String, usize> = HashMap::new();
    let mut arrow_msgs = 0usize;
    let started = Instant::now();

    loop {
        assert!(
            started.elapsed() < Duration::from_mins(10),
            "timed out; so far: {store_infos} store infos, {arrow_msgs} arrow msgs from {} episodes",
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
                            arrow_msgs += 1;
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

        // Success: all episodes announced up front, and several episodes fully loaded.
        let loaded: Vec<&String> = episodes_msgs
            .iter()
            .filter(|&(_, &n)| n >= 5)
            .map(|(k, _)| k)
            .collect();
        if store_infos >= 2 && loaded.len() >= 6 {
            println!(
                "OK: {store_infos} episodes announced, {arrow_msgs} data messages, loaded: {loaded:?}"
            );
            break;
        }
    }

    assert!(store_infos >= 2);
}
