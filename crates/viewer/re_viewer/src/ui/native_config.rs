//! Local viewer config for native builds.
//!
//! The web deployment serves default TOS/Hugging Face settings next to the viewer as
//! `config.json`. A locally-run native viewer has no such server, so instead it reads
//! the same file from the user's config directory. This lets someone run the viewer entirely
//! on their own machine — no cloud, no serving deployment — and still get their default
//! endpoint/dataset pre-filled in the "Open from …" dialogs.

use std::path::PathBuf;

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
                    "Failed to read local viewer config: {err}\nFile path: {}",
                    path.display()
                );
            }
            None
        }
    }
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

    #[test]
    #[expect(unsafe_code)]
    fn reads_config_from_explicit_path() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let contents = br#"{"endpoint":"https://example.com","hfToken":"hf_x"}"#;
        std::fs::write(&path, contents).unwrap();

        // SAFETY: single-threaded test; nothing else reads the environment concurrently.
        unsafe { std::env::set_var("RERUN_CONFIG", &path) };
        assert_eq!(load_local_config_bytes().as_deref(), Some(&contents[..]));
        // SAFETY: single-threaded test; nothing else reads the environment concurrently.
        unsafe { std::env::remove_var("RERUN_CONFIG") };
    }

    #[test]
    #[expect(unsafe_code)]
    fn missing_file_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("does-not-exist.json");

        // SAFETY: single-threaded test; nothing else reads the environment concurrently.
        unsafe { std::env::set_var("RERUN_CONFIG", &path) };
        assert_eq!(load_local_config_bytes(), None);
        // SAFETY: single-threaded test; nothing else reads the environment concurrently.
        unsafe { std::env::remove_var("RERUN_CONFIG") };
    }
}
