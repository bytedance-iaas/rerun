//! Silent resolution of the default TOS/HF connection settings, for session restore.
//!
//! Same sources as the "Open from …" dialogs: on the web the deployment serves
//! `/config.json` next to the viewer; natively `~/.rerun/config.json`
//! (or `$RERUN_CONFIG`) with environment-variable overrides.

#[cfg(target_arch = "wasm32")]
use re_i18n::trf;
use std::sync::atomic::{AtomicBool, Ordering};

use parking_lot::Mutex;

/// The deployment/user-level default connection settings. May be entirely empty.
#[derive(Clone, serde::Deserialize)]
#[serde(default)]
pub struct ViewerConfig {
    pub tos_endpoint: String,
    pub tos_access_key: String,
    pub tos_secret_key: String,
    pub hf_token: String,

    /// Hub base-URL override (e.g. a mirror); empty = the official huggingface.co.
    pub hf_endpoint: String,

    /// Where converted rrds are stored; absent/`""`/`"off"` disables the artifacts store.
    pub tos_rrd_artifacts_url: String,

    /// The artifacts bucket's region. Empty = the deployment's own region (the common case);
    /// set it when the bucket lives elsewhere — `tos://` URLs carry no region, and TOS
    /// answers `NoSuchBucket` when a bucket is addressed through another region's endpoint.
    pub tos_rrd_artifacts_region: String,

    /// How many artifacts to prefetch at once; `0` (or absent) = automatic.
    pub rrd_artifacts_prefetch: usize,

    /// Where the "Diagnose" buttons send the user: the Daft curation console.
    /// Absent = same-domain `/curation` on the web, no buttons natively.
    pub daft_url: String,

    /// The deployment's web viewer address, used as the base of "Web viewer" share links
    /// in the native viewer (the web viewer uses its own page address instead).
    /// Absent = links carry the `https://web_viewer_dns/` placeholder to fill in by hand.
    pub web_viewer_url: String,

    /// Lifetime (seconds, 60–3600) asked for each URL the curation console presigns for a
    /// dataset opened from its "Visualize" link. `0` (or absent) = 30 minutes; set it to 60
    /// to watch re-signing happen.
    pub curator_sign_ttl: u32,
}

impl Default for ViewerConfig {
    fn default() -> Self {
        Self {
            tos_endpoint: String::new(),
            tos_access_key: String::new(),
            tos_secret_key: String::new(),
            hf_token: String::new(),
            hf_endpoint: String::new(),
            tos_rrd_artifacts_url: String::new(),
            tos_rrd_artifacts_region: String::new(),
            rrd_artifacts_prefetch: 0,
            daft_url: String::new(),
            web_viewer_url: String::new(),
            curator_sign_ttl: 0,
        }
    }
}

impl ViewerConfig {
    pub fn has_tos_credentials(&self) -> bool {
        !self.tos_access_key.is_empty() && !self.tos_secret_key.is_empty()
    }

    /// Access to a dataset registered in the curation console, every read presigned by the
    /// console with the key bound to the registration — no key in the viewer.
    ///
    /// `None` when that is not possible: natively (the signing request rides on the
    /// console's same-origin login, which only the web viewer has), or when `daft_url` puts
    /// the console on another origin (it refuses cross-site requests). The caller then
    /// opens the dataset the old way.
    pub fn curator_access(
        &self,
        region: &str,
        dataset_id: &str,
    ) -> Option<re_data_source::tos::TosAccess> {
        use re_data_source::tos::curator::DEFAULT_SIGN_TTL_S;

        let api_base = curator_api_base()?;
        let sign_ttl_s = if self.curator_sign_ttl == 0 {
            DEFAULT_SIGN_TTL_S
        } else {
            self.curator_sign_ttl.clamp(60, 3600)
        };
        Some(re_data_source::tos::TosAccess::CuratorDataset(
            re_data_source::tos::CuratorDatasetAccess {
                // Only the region matters here: the console decides where the URLs point.
                endpoint: re_data_source::tos::endpoint_for_region(region, &self.tos_endpoint),
                dataset_id: dataset_id.to_owned(),
                api_base,
                sign_ttl_s,
            },
        ))
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
                // The artifacts bucket has its own (optional) region — empty means the
                // deployment's, in which case this returns `tos_endpoint` verbatim.
                endpoint: re_data_source::tos::endpoint_for_region(
                    &self.tos_rrd_artifacts_region,
                    &self.tos_endpoint,
                ),
                access_key: self.tos_access_key.clone(),
                secret_key: self.tos_secret_key.clone(),
            },
            write_back,
            prefetch_items: self.rrd_artifacts_prefetch,
        })
    }
}

/// The curation console's REST base (`/curation/api/v1`), when the console is on the web
/// viewer's own origin; see [`ViewerConfig::curator_access`].
fn curator_api_base() -> Option<String> {
    #[cfg(target_arch = "wasm32")]
    {
        let base = re_viewer_context::daft_link::base_url()?;
        let same_origin = if base.starts_with('/') && !base.starts_with("//") {
            true
        } else {
            let page = re_web::browser::current_page_url().ok()?;
            let origin = |url: &str| {
                url::Url::parse(url)
                    .ok()
                    .map(|url| url.origin().ascii_serialization())
            };
            origin(&base).is_some() && origin(&base) == origin(&page)
        };
        if same_origin {
            Some(format!("{base}/api/v1"))
        } else {
            re_log::warn_once!(
                "The curation console ({base}, `daft_url` in config.json) is on another origin, \
                 so it cannot sign reads for this viewer — datasets from its \"Visualize\" links \
                 open with this deployment's own settings instead."
            );
            None
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    None
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
            ehttp::Request::get("config.json").with_credentials(ehttp::Credentials::SameOrigin);
        ehttp::fetch(request, move |result| {
            // A missing/broken config file resolves to empty settings (not an error):
            // the viewer works without defaults, credentials are just not pre-resolved.
            // Still worth a console line — a 401 here looks exactly like "no defaults".
            let parsed = match &result {
                Ok(response) if response.status == 200 => {
                    serde_json::from_slice::<ViewerConfig>(&response.bytes).unwrap_or_else(|err| {
                        re_log::warn!(
                            "{}",
                            trf!(
                                "Failed to parse viewer defaults: {err}\nFile: config.json",
                                "解析 Viewer 默认配置失败：{err}\n文件：config.json"
                            )
                        );
                        ViewerConfig::default()
                    })
                }
                Ok(response) => {
                    re_log::warn!(
                        "{}",
                        trf!(
                            "Failed to load viewer defaults: HTTP {} {}\nFile: config.json",
                            "加载 Viewer 默认配置失败：HTTP {} {}\n文件：config.json",
                            response.status,
                            response.status_text
                        )
                    );
                    ViewerConfig::default()
                }
                Err(err) => {
                    re_log::warn!(
                        "{}",
                        trf!(
                            "Failed to load viewer defaults: {err}\nFile: config.json",
                            "加载 Viewer 默认配置失败：{err}\n文件：config.json"
                        )
                    );
                    ViewerConfig::default()
                }
            };
            re_viewer_context::daft_link::set_base_url(&parsed.daft_url);
            re_data_source::hf::set_configured_endpoint(&parsed.hf_endpoint);
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
        env_override(&mut parsed.tos_access_key, "TOS_ACCESS_KEY");
        env_override(&mut parsed.tos_secret_key, "TOS_SECRET_KEY");
        env_override(&mut parsed.hf_token, "HF_TOKEN");
        env_override(&mut parsed.hf_endpoint, "HF_ENDPOINT");
        env_override(&mut parsed.tos_rrd_artifacts_url, "TOS_RRD_ARTIFACTS_URL");
        env_override(
            &mut parsed.tos_rrd_artifacts_region,
            "TOS_RRD_ARTIFACTS_REGION",
        );
        env_override(&mut parsed.web_viewer_url, "WEB_VIEWER_URL");
        if let Ok(value) = std::env::var("RRD_ARTIFACTS_PREFETCH")
            && let Ok(n) = value.trim().parse()
        {
            parsed.rrd_artifacts_prefetch = n;
        }

        re_data_source::hf::set_configured_endpoint(&parsed.hf_endpoint);
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
        assert!(!config.has_tos_credentials());
    }

    #[test]
    fn artifacts_store_needs_explicit_url_and_credentials() {
        // An absent key means no artifacts store: there is no default bucket.
        let mut config: ViewerConfig = serde_json::from_slice(b"{}").unwrap();
        assert!(config.tos_rrd_artifacts_url.is_empty());
        assert!(config.rrd_artifacts(true).is_none());

        // A configured URL alone is not enough either — TOS credentials are required…
        config.tos_rrd_artifacts_url = "tos://example-bucket/rrd-data/".to_owned();
        assert!(config.rrd_artifacts(true).is_none());

        // …and with both, the store resolves.
        config.tos_access_key = "ak".to_owned();
        config.tos_secret_key = "sk".to_owned();
        let artifacts = config.rrd_artifacts(true).unwrap();
        assert_eq!(artifacts.location.bucket, "example-bucket");
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
