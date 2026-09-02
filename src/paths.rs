//! Where Fastpotify keeps its files.
//!
//! Configuration, durable state (Navidrome credentials), and disposable caches
//! (artwork, lyrics) live in the platform's conventional directories, so
//! clearing a cache never signs the user out and a config backup never
//! contains a credential.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use directories::ProjectDirs;

#[derive(Clone, Debug)]
pub struct AppDirs {
    pub config: PathBuf,
    pub state: PathBuf,
    pub cache: PathBuf,
}

impl AppDirs {
    pub fn discover() -> Self {
        let project = ProjectDirs::from("me", "paolino", "fastpotify");
        match project {
            Some(project) => Self {
                config: project.config_dir().to_path_buf(),
                state: project
                    .state_dir()
                    .map(|path| path.to_path_buf())
                    .unwrap_or_else(|| project.data_local_dir().to_path_buf()),
                cache: project.cache_dir().to_path_buf(),
            },
            None => {
                let fallback = std::env::current_dir().unwrap_or_default();
                Self {
                    config: fallback.join("fastpotify-config"),
                    state: fallback.join("fastpotify-state"),
                    cache: fallback.join("fastpotify-cache"),
                }
            }
        }
    }

    pub fn settings_file(&self) -> PathBuf {
        self.config.join("settings.json")
    }

    /// Winamp skins the listener has added, as `.wsz` files or folders.
    pub fn skins_dir(&self) -> PathBuf {
        self.config.join("skins")
    }

    /// The single active Navidrome account. The file is owner-only and is
    /// written by [`crate::auth::Credentials`].
    pub fn credentials_file(&self) -> PathBuf {
        self.state.join("navidrome.json")
    }

    /// Durable state for one server/user profile. Keeping it scoped prevents
    /// opaque OpenSubsonic IDs from colliding across servers.
    pub fn profile_state_dir(&self, profile: &crate::auth::ProfileId) -> PathBuf {
        self.state.join("profiles").join(profile.as_str())
    }

    pub fn profile_cache_dir(&self, profile: &crate::auth::ProfileId) -> PathBuf {
        self.cache.join("profiles").join(profile.as_str())
    }

    pub fn session_file(&self, profile: &crate::auth::ProfileId) -> PathBuf {
        self.profile_state_dir(profile).join("session.json")
    }

    /// Plays made by this client, scoped to the active server/user.
    pub fn history_file(&self, profile: &crate::auth::ProfileId) -> PathBuf {
        self.profile_state_dir(profile).join("history.json")
    }

    /// The generated songs for the profile's current local-calendar day.
    pub fn daily_mix_file(&self, profile: &crate::auth::ProfileId) -> PathBuf {
        self.profile_state_dir(profile).join("daily-mix.json")
    }

    /// The log of the current run, replaced at every start.
    pub fn log_file(&self) -> PathBuf {
        self.state.join("fastpotify.log")
    }

    /// Where a panic is recorded before the process dies of it.
    pub fn panic_log(&self) -> PathBuf {
        self.state.join("panic.log")
    }

    pub fn art_cache_dir(&self) -> PathBuf {
        self.cache.join("art")
    }

    pub fn lyrics_cache_dir(&self) -> PathBuf {
        self.cache.join("lyrics")
    }

    pub fn ensure(&self) -> std::io::Result<()> {
        for dir in [&self.config, &self.state, &self.cache] {
            std::fs::create_dir_all(dir)?;
        }
        Ok(())
    }
}

/// Replaces a small state file atomically without exposing a partially written
/// JSON document after a crash. The temporary file lives beside the target so
/// the final rename stays on one filesystem.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("state");
    let temporary = path.with_file_name(format!(
        ".{name}.{}.{}.tmp",
        std::process::id(),
        rand::random::<u64>()
    ));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        replace_file(&temporary, path)?;
        #[cfg(unix)]
        if let Some(parent) = path.parent()
            && let Ok(directory) = OpenOptions::new().read(true).open(parent)
        {
            let _ = directory.sync_all();
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

/// Atomically puts `source` at `target`, replacing an existing target.
///
/// Unix `rename` already has replace semantics. Windows' standard-library
/// rename does not, so use the platform primitive that provides the same
/// contract for state files saved more than once.
#[cfg(not(windows))]
pub(crate) fn replace_file(source: &Path, target: &Path) -> io::Result<()> {
    fs::rename(source, target)
}

#[cfg(windows)]
pub(crate) fn replace_file(source: &Path, target: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt as _;

    const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x8;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn MoveFileExW(existing: *const u16, new: *const u16, flags: u32) -> i32;
    }

    let existing = source
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let new = target
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    // SAFETY: both pointers reference NUL-terminated buffers that remain alive
    // for the duration of the call; the flags request documented atomic
    // replacement and durable completion.
    if unsafe {
        MoveFileExW(
            existing.as_ptr(),
            new.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    } == 0
    {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_files_are_isolated_from_legacy_global_state() {
        let dirs = AppDirs {
            config: PathBuf::from("config"),
            state: PathBuf::from("state"),
            cache: PathBuf::from("cache"),
        };
        let profile = crate::auth::ProfileId::new("0123456789abcdef0123456789abcdef01234567");
        assert_eq!(
            dirs.session_file(&profile),
            PathBuf::from("state/profiles/0123456789abcdef0123456789abcdef01234567/session.json")
        );
        assert_eq!(
            dirs.history_file(&profile),
            PathBuf::from("state/profiles/0123456789abcdef0123456789abcdef01234567/history.json")
        );
        assert_eq!(
            dirs.daily_mix_file(&profile),
            PathBuf::from("state/profiles/0123456789abcdef0123456789abcdef01234567/daily-mix.json")
        );
        assert_eq!(
            dirs.credentials_file(),
            PathBuf::from("state/navidrome.json")
        );
    }

    #[test]
    fn atomic_write_replaces_the_complete_file() {
        let directory = std::env::temp_dir().join(format!(
            "fastpotify-atomic-write-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("state.json");
        atomic_write(&path, b"first").unwrap();
        atomic_write(&path, b"second").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"second");
        assert!(
            !fs::read_dir(&directory)
                .unwrap()
                .flatten()
                .any(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
        );
        fs::remove_dir_all(directory).unwrap();
    }
}
