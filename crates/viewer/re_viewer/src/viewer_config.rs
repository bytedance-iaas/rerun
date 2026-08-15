//! Silent resolution of the default TOS/HF connection settings, for session restore.
//!
//! Same sources as the "Open from …" dialogs: on the web the deployment serves
//! `/tos-config.json` next to the viewer; natively `~/.rerun/tos-config.json`
//! (or `$RERUN_TOS_CONFIG`) with environment-variable overrides.

use std::sync::atomic::{AtomicBool, Ordering};

use parking_lot::Mutex;

/// The deployment/user-level default connection settings. May be entirely empty.
#[derive(Clone, serde::Deserialize)]
#[serde(default)]
pub struct ViewerConfig {
    pub tos_endpoint: String,
    pub tos_region: String,
    pub tos_access_key: String,
    pub tos_secret_key: String,
    pub hf_token: String,

    /// Where converted rrds are stored; an absent key means the default bucket,
    /// `""`/`"off"` disables the artifacts store.
    #[serde(default = "re_data_source::rrd_artifacts::default_artifacts_url")]
    pub tos_rrd_artifacts_url: String,

    /// How many artifacts to prefetch at once; `0` (or absent) = automatic.
    pub rrd_artifacts_prefetch: usize,

    /// Where the "Diagnose" buttons send the user: the Daft curation console.
    /// Absent = same-domain `/curation` on the web, no buttons natively.
    pub daft_url: String,
}

impl Default for ViewerConfig {
    fn default() -> Self {
        Self {
            tos_endpoint: String::new(),
            tos_region: String::new(),
            tos_access_key: String::new(),
            tos_secret_key: String::new(),
            hf_token: String::new(),
            tos_rrd_artifacts_url: re_data_source::rrd_artifacts::default_artifacts_url(),
            rrd_artifacts_prefetch: 0,
            daft_url: String::new(),
        }
    }
}

impl ViewerConfig {
    pub fn has_tos_credentials(&self) -> bool {
        !self.tos_access_key.is_empty() && !self.tos_secret_key.is_empty()
    }

    /// The resolved rrd-artifacts target — `None` when disabled or without TOS credentials.
    pub fn rrd_artifacts(
        &self,
        write_back: bool,
    ) -> Option<re_data_source::rrd_artifacts::RrdArtifactsConfig> {
        let location =
            re_data_source::rrd_artifacts::parse_artifacts_url(&self.tos_rrd_artifacts_url)?;
        if !self.has_tos_credentials() {
            return None; // No credentials for the artifacts bucket: silently skip.
        }
        Some(re_data_source::rrd_artifacts::RrdArtifactsConfig {
            location,
            credentials: re_data_source::tos::TosCredentials {
                endpoint: self.tos_endpoint.clone(),
                region: self.tos_region.clone(),
                access_key: self.tos_access_key.clone(),
                secret_key: self.tos_secret_key.clone(),
            },
            write_back,
            prefetch_items: self.rrd_artifacts_prefetch,
        })
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
        // `SameOrigin`: the deployment may sit behind HTTP Basic auth, and ehttp's
        // default (`Omit`) tells the browser to strip the authenticated session,
        // turning every fetch into a 401.
        let request =
            ehttp::Request::get("tos-config.json").with_credentials(ehttp::Credentials::SameOrigin);
        ehttp::fetch(request, move |result| {
            // A missing/broken config file resolves to empty settings (not an error):
            // the viewer works without defaults, credentials are just not pre-resolved.
            // Still worth a console line — a 401 here looks exactly like "no defaults".
            let parsed = match &result {
                Ok(response) if response.status == 200 => {
                    serde_json::from_slice::<ViewerConfig>(&response.bytes).unwrap_or_else(|err| {
                        re_log::warn!(
                            "Failed to parse viewer defaults: {err}\nFile: tos-config.json"
                        );
                        ViewerConfig::default()
                    })
                }
                Ok(response) => {
                    re_log::warn!(
                        "Failed to load viewer defaults: HTTP {} {}\nFile: tos-config.json",
                        response.status,
                        response.status_text
                    );
                    ViewerConfig::default()
                }
                Err(err) => {
                    re_log::warn!("Failed to load viewer defaults: {err}\nFile: tos-config.json");
                    ViewerConfig::default()
                }
            };
            re_viewer_context::daft_link::set_base_url(&parsed.daft_url);
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
        env_override(&mut parsed.tos_rrd_artifacts_url, "TOS_RRD_ARTIFACTS_URL");
        if let Ok(value) = std::env::var("RRD_ARTIFACTS_PREFETCH")
            && let Ok(n) = value.trim().parse()
        {
            parsed.rrd_artifacts_prefetch = n;
        }

        *CONFIG.lock() = Some(parsed);
    }
}

/// The resolved config, once [`request`] finished (immediately on native, async on the web).
pub fn get() -> Option<ViewerConfig> {
    CONFIG.lock().clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The config file is shared with the "Open from …" dialogs and the web deployment, and
    /// grows keys over time: missing fields must default, unknown fields must be ignored.
    #[test]
    fn config_tolerates_partial_and_unknown_fields() {
        let config: ViewerConfig = serde_json::from_slice(
            br#"{"tos_endpoint":"https://tos.example.com","some_future_key":1}"#,
        )
        .unwrap();
        assert_eq!(config.tos_endpoint, "https://tos.example.com");
        assert_eq!(config.tos_region, "");
        assert!(!config.has_tos_credentials());
    }

    #[test]
    fn artifacts_store_defaults_on_but_needs_credentials() {
        // An absent key resolves to the default bucket…
        let mut config: ViewerConfig = serde_json::from_slice(b"{}").unwrap();
        assert_eq!(
            config.tos_rrd_artifacts_url,
            re_data_source::rrd_artifacts::DEFAULT_RRD_ARTIFACTS_URL
        );
        // …but without TOS credentials there is no artifacts target.
        assert!(config.rrd_artifacts(true).is_none());

        config.tos_access_key = "ak".to_owned();
        config.tos_secret_key = "sk".to_owned();
        let artifacts = config.rrd_artifacts(true).unwrap();
        assert_eq!(artifacts.location.bucket, "physical-ai-rerun-test");
        assert!(artifacts.write_back);
    }

    #[test]
    fn artifacts_store_off_switch_wins_over_credentials() {
        let config = ViewerConfig {
            tos_access_key: "ak".to_owned(),
            tos_secret_key: "sk".to_owned(),
            tos_rrd_artifacts_url: "off".to_owned(),
            ..Default::default()
        };
        assert!(config.rrd_artifacts(false).is_none());
    }

    #[test]
    fn credentials_need_both_keys() {
        let mut config = ViewerConfig {
            tos_access_key: "ak".to_owned(),
            ..Default::default()
        };
        assert!(!config.has_tos_credentials());
        config.tos_secret_key = "sk".to_owned();
        assert!(config.has_tos_credentials());
    }
}
