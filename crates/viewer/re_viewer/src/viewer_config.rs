//! Silent resolution of the default TOS/HF connection settings, for session restore.
//!
//! Same sources as the "Open from …" dialogs: on the web the deployment serves
//! `/tos-config.json` next to the viewer; natively `~/.rerun/tos-config.json`
//! (or `$RERUN_TOS_CONFIG`) with environment-variable overrides.

use std::sync::atomic::{AtomicBool, Ordering};

use parking_lot::Mutex;

/// The deployment/user-level default connection settings. May be entirely empty.
#[derive(Default, Clone, serde::Deserialize)]
#[serde(default)]
pub struct ViewerConfig {
    pub tos_endpoint: String,
    pub tos_region: String,
    pub tos_access_key: String,
    pub tos_secret_key: String,
    pub hf_token: String,
}

impl ViewerConfig {
    pub fn has_tos_credentials(&self) -> bool {
        !self.tos_access_key.is_empty() && !self.tos_secret_key.is_empty()
    }
}

static CONFIG: Mutex<Option<ViewerConfig>> = Mutex::new(None);
static REQUESTED: AtomicBool = AtomicBool::new(false);

/// Kick off the config resolution once. Async on the web; immediate natively.
pub fn request() {
    if REQUESTED.swap(true, Ordering::SeqCst) {
        return;
    }

    #[cfg(target_arch = "wasm32")]
    {
        ehttp::fetch(ehttp::Request::get("tos-config.json"), move |result| {
            // A missing/broken config file resolves to empty settings (not an error):
            // the viewer works without defaults, credentials are just not pre-resolved.
            let parsed = result
                .ok()
                .filter(|response| response.status == 200)
                .and_then(|response| serde_json::from_slice::<ViewerConfig>(&response.bytes).ok())
                .unwrap_or_default();
            *CONFIG.lock() = Some(parsed);
        });
    }

    #[cfg(not(target_arch = "wasm32"))]
    {
        let mut parsed = crate::ui::native_config::load_local_config_bytes()
            .and_then(|bytes| serde_json::from_slice::<ViewerConfig>(&bytes).ok())
            .unwrap_or_default();

        fn env_override(field: &mut String, key: &str) {
            if let Ok(value) = std::env::var(key)
                && !value.is_empty()
            {
                *field = value;
            }
        }
        env_override(&mut parsed.tos_endpoint, "TOS_ENDPOINT");
        env_override(&mut parsed.tos_region, "TOS_REGION");
        env_override(&mut parsed.tos_access_key, "TOS_ACCESS_KEY");
        env_override(&mut parsed.tos_secret_key, "TOS_SECRET_KEY");
        env_override(&mut parsed.hf_token, "HF_TOKEN");

        *CONFIG.lock() = Some(parsed);
    }
}

/// The resolved config, once [`request`] finished (immediately on native, async on the web).
pub fn get() -> Option<ViewerConfig> {
    CONFIG.lock().clone()
}
