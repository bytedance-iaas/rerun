//! Local viewer config for native builds.
//!
//! The web deployment serves default TOS/Hugging Face settings next to the viewer as
//! `config.json`. A locally-run native viewer has no such server, so instead it reads
//! the same file from the user's config directory. This lets someone run the viewer entirely
//! on their own machine — no cloud, no serving deployment — and still get their default
//! endpoint/dataset pre-filled in the "Open from …" dialogs.

use std::path::PathBuf;

use re_i18n::trf;

/// Reads the local viewer config file, mirroring the web deployment's `config.json`.
///
/// Looks at `$RERUN_CONFIG` first, then `~/.rerun/config.json`. Returns the raw bytes
/// so each dialog can deserialize just the fields it cares about, exactly like the web path.
/// A missing file is not an error (the dialogs still work with manual input); any other read
/// failure is logged and treated as absent.
pub fn load_local_config_bytes() -> Option<Vec<u8>> {
    let path = local_config_path()?;
    match std::fs::read(&path) {
        Ok(bytes) => Some(bytes),
        Err(err) => {
            if err.kind() != std::io::ErrorKind::NotFound {
                re_log::warn!(
                    "{}",
                    trf!(
                        "Failed to read local viewer config: {err}\nFile path: {}",
                        "读取本地 Viewer 配置失败：{err}\n文件路径：{}",
                        path.display()
                    )
                );
            }
            None
        }
    }
}

/// Writes the given TOS credentials into the local config file (creating it if needed),
/// preserving every other key — the file is shared with endpoints, tokens, and future settings.
///
/// Backs the "remember" option of the credential prompts; errors are returned as a
/// display string because the caller can only show them, not handle them.
pub fn save_tos_credentials(access_key: &str, secret_key: &str) -> Result<(), String> {
    let Some(path) = local_config_path() else {
        return Err(trf!(
            "Cannot determine the config file location (no home directory)",
            "无法确定配置文件位置（找不到主目录）"
        ));
    };

    let mut config: serde_json::Map<String, serde_json::Value> = match std::fs::read(&path) {
        // Refuse to overwrite a file we cannot parse — clobbering the user's
        // hand-edited config to save two keys is a bad trade.
        Ok(bytes) => serde_json::from_slice(&bytes).map_err(|err| {
            trf!(
                "The existing config file is not valid JSON, not overwriting: {err}\nFile path: {}",
                "现有配置文件不是有效的 JSON，不覆盖：{err}\n文件路径：{}",
                path.display()
            )
        })?,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => serde_json::Map::new(),
        Err(err) => {
            return Err(trf!(
                "Failed to read the config file: {err}\nFile path: {}",
                "读取配置文件失败：{err}\n文件路径：{}",
                path.display()
            ));
        }
    };

    config.insert("tos_access_key".to_owned(), access_key.trim().into());
    config.insert("tos_secret_key".to_owned(), secret_key.trim().into());

    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|err| {
            trf!(
                "Failed to create the config directory: {err}\nDirectory: {}",
                "创建配置目录失败：{err}\n目录：{}",
                dir.display()
            )
        })?;
    }

    #[expect(clippy::unwrap_used)] // A string-keyed map of JSON values always serializes.
    let contents = serde_json::to_vec_pretty(&serde_json::Value::Object(config)).unwrap();
    std::fs::write(&path, contents).map_err(|err| {
        trf!(
            "Failed to write the config file: {err}\nFile path: {}",
            "写入配置文件失败：{err}\n文件路径：{}",
            path.display()
        )
    })?;

    // The file now holds a secret key: keep it readable by the owner only.
    // Best effort — a config the user can read still works if this fails.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if let Err(err) = std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)) {
            re_log::warn!(
                "{}",
                trf!(
                    "Failed to restrict the config file's permissions: {err}\nFile path: {}",
                    "收紧配置文件权限失败：{err}\n文件路径：{}",
                    path.display()
                )
            );
        }
    }

    Ok(())
}

fn local_config_path() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("RERUN_CONFIG") {
        return Some(PathBuf::from(path));
    }
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    Some(PathBuf::from(home).join(".rerun").join("config.json"))
}

#[cfg(test)]
mod tests {
    use super::load_local_config_bytes;

    /// Serialized: these tests share the process-global `RERUN_CONFIG` variable.
    static TEST_LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

    #[test]
    #[expect(unsafe_code)]
    fn reads_config_from_explicit_path() {
        let _guard = TEST_LOCK.lock();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let contents = br#"{"endpoint":"https://example.com","hfToken":"hf_x"}"#;
        std::fs::write(&path, contents).unwrap();

        // SAFETY: the lock above keeps other env-touching tests out of this section.
        unsafe { std::env::set_var("RERUN_CONFIG", &path) };
        assert_eq!(load_local_config_bytes().as_deref(), Some(&contents[..]));
        // SAFETY: single-threaded test; nothing else reads the environment concurrently.
        unsafe { std::env::remove_var("RERUN_CONFIG") };
    }

    #[test]
    #[expect(unsafe_code)]
    fn save_credentials_creates_and_merges() {
        let _guard = TEST_LOCK.lock();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");

        // SAFETY: the lock above keeps other env-touching tests out of this section.
        unsafe { std::env::set_var("RERUN_CONFIG", &path) };

        // No file yet: it is created with just the two keys.
        super::save_tos_credentials(" ak-1 ", "sk-1").unwrap();
        let config: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(config["tos_access_key"], "ak-1"); // trimmed
        assert_eq!(config["tos_secret_key"], "sk-1");

        // Existing unrelated keys survive a re-save.
        std::fs::write(
            &path,
            br#"{"tos_endpoint":"https://tos.example.com","tos_access_key":"old"}"#,
        )
        .unwrap();
        super::save_tos_credentials("ak-2", "sk-2").unwrap();
        let config: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(config["tos_endpoint"], "https://tos.example.com");
        assert_eq!(config["tos_access_key"], "ak-2");
        assert_eq!(config["tos_secret_key"], "sk-2");

        // A file that is not valid JSON is left untouched.
        std::fs::write(&path, b"not json {").unwrap();
        assert!(super::save_tos_credentials("ak-3", "sk-3").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"not json {");

        // SAFETY: single-threaded test; nothing else reads the environment concurrently.
        unsafe { std::env::remove_var("RERUN_CONFIG") };
    }

    #[test]
    #[expect(unsafe_code)]
    fn missing_file_is_not_an_error() {
        let _guard = TEST_LOCK.lock();
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does-not-exist.json");

        // SAFETY: the lock above keeps other env-touching tests out of this section.
        unsafe { std::env::set_var("RERUN_CONFIG", &path) };
        assert_eq!(load_local_config_bytes(), None);
        // SAFETY: single-threaded test; nothing else reads the environment concurrently.
        unsafe { std::env::remove_var("RERUN_CONFIG") };
    }
}
