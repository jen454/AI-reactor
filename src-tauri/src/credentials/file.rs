//! The `~/.claude/.credentials.json` fallback.
//!
//! On this Mac the `claude` CLI uses the keychain and this file does not
//! exist, so in practice this path is dormant. It is here because the CLI
//! writes this file on Linux/WSL and anywhere the keychain is unavailable,
//! and because it is the only way to exercise the parser end-to-end without
//! a keychain prompt.
//!
//! **Read only.** There is deliberately no write function in this module.
//! `~/.claude/` belongs to the CLI; AI reactor is a spectator.

use std::path::PathBuf;

use super::error::CredentialError;

/// `~/.claude/.credentials.json`, or `None` if we cannot tell where home is.
pub fn path() -> Option<PathBuf> {
    // `std::env::home_dir` is deprecated for its buggy Windows behaviour, and
    // $HOME is what the CLI itself resolves against on Unix, so we read it
    // directly rather than pull in a crate for one lookup.
    let home = std::env::var_os("HOME")?;
    Some(PathBuf::from(home).join(".claude").join(".credentials.json"))
}

/// Read the raw bytes. Absence is `NotFound`, not an error to shout about —
/// it is the normal state on a Mac.
pub fn read() -> Result<Vec<u8>, CredentialError> {
    let Some(path) = path() else {
        return Err(CredentialError::Backend("HOME이 설정되어 있지 않음".into()));
    };
    read_from(&path)
}

/// The part that actually touches the disk, split out from `path()` so tests
/// can point it at a fixture. Mutating `$HOME` from a test would work too, but
/// it is process-global and Rust runs tests in parallel — two tests fighting
/// over the same env var is exactly the kind of flake worth designing out.
fn read_from(path: &std::path::Path) -> Result<Vec<u8>, CredentialError> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(CredentialError::NotFound),
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => Err(CredentialError::Denied),
        Err(e) => Err(CredentialError::Backend(e.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credentials::{parse, CredentialState};

    /// A scratch directory that cleans itself up, so the fixture never lands
    /// anywhere near the user's real `~/.claude`.
    struct TempDir(std::path::PathBuf);

    impl TempDir {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("ai-reactor-test-{name}-{}", std::process::id()));
            std::fs::create_dir_all(&dir).expect("create temp dir");
            Self(dir)
        }
        fn file(&self, contents: &str) -> std::path::PathBuf {
            let path = self.0.join(".credentials.json");
            std::fs::write(&path, contents).expect("write fixture");
            path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn resolves_under_dot_claude() {
        let path = path().expect("HOME is set in the test environment");
        assert!(path.ends_with(".claude/.credentials.json"), "{path:?}");
    }

    /// The full file path end to end: bytes on disk -> parsed -> classified.
    #[test]
    fn reads_and_parses_a_credential_file() {
        let dir = TempDir::new("read");
        let path = dir.file(
            r#"{"claudeAiOauth":{"accessToken":"EXAMPLE","refreshToken":"NOPE","expiresAt":4102444800000}}"#,
        );

        let bytes = read_from(&path).expect("should read");
        let cred = parse::parse(&bytes).expect("should parse");
        // Expiry is the year 2100, so this is valid whenever the test runs.
        assert!(matches!(
            CredentialState::from_credentials(&cred, crate::credentials::now_ms()),
            CredentialState::Ok { .. }
        ));
    }

    #[test]
    fn a_missing_file_is_not_found() {
        let dir = TempDir::new("missing");
        let path = dir.0.join("does-not-exist.json");
        assert_eq!(read_from(&path), Err(CredentialError::NotFound));
    }

    #[test]
    fn an_unreadable_file_is_denied() {
        use std::os::unix::fs::PermissionsExt;
        let dir = TempDir::new("perm");
        let path = dir.file("{}");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o000))
            .expect("chmod the fixture");

        // Running as root would defeat the point; skip rather than fail.
        if read_from(&path).is_ok() {
            eprintln!("skipped: running with privileges that ignore file modes");
            return;
        }
        assert_eq!(read_from(&path), Err(CredentialError::Denied));
    }
}
