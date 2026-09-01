//! OpenSubsonic credentials and token authentication.
//!
//! Passwords are persisted only in the user's credential file. They are never
//! included in `Debug`, `Display`, profile identifiers, media URIs, or artwork
//! references. Each authenticated request derives a fresh salted token.

use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use rand::RngCore;
use reqwest::Url;
use serde::{Deserialize, Deserializer, Serialize};
use sha1::{Digest, Sha1};
use thiserror::Error;

pub const API_VERSION: &str = "1.16.1";
pub const CLIENT_NAME: &str = "Fastpotify";

/// Stable, non-secret identity for one server/user profile.
#[derive(Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct ProfileId(String);

impl ProfileId {
    pub fn new(value: impl Into<String>) -> Self {
        Self::try_new(value).expect("profile fingerprints are 40 lowercase hexadecimal characters")
    }

    pub fn try_new(value: impl Into<String>) -> Result<Self, ProfileIdError> {
        let value = value.into();
        if value.len() == 40
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            Ok(Self(value))
        } else {
            Err(ProfileIdError)
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for ProfileId {
    fn default() -> Self {
        Self("0000000000000000000000000000000000000000".to_owned())
    }
}

impl std::str::FromStr for ProfileId {
    type Err = ProfileIdError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::try_new(value)
    }
}

impl<'de> Deserialize<'de> for ProfileId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::try_new(value).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
#[error("profile fingerprint must be 40 lowercase hexadecimal characters")]
pub struct ProfileIdError;

impl fmt::Debug for ProfileId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_tuple("ProfileId").field(&self.0).finish()
    }
}

impl fmt::Display for ProfileId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Credentials for one OpenSubsonic server.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Credentials {
    #[serde(alias = "server_url", alias = "url")]
    server: String,
    #[serde(alias = "user")]
    username: String,
    password: String,
}

impl Credentials {
    /// Validates and normalizes credentials. Deployments below a path are
    /// supported, while URL credentials, queries and fragments are rejected.
    pub fn new(
        server: impl AsRef<str>,
        username: impl AsRef<str>,
        password: impl Into<String>,
    ) -> Result<Self, CredentialError> {
        let server = normalize_server_url(server.as_ref())?;
        let username = username.as_ref().trim();
        if username.is_empty() {
            return Err(CredentialError::InvalidUsername);
        }
        Ok(Self {
            server,
            username: username.to_owned(),
            password: password.into(),
        })
    }

    pub fn server(&self) -> &str {
        &self.server
    }

    pub fn username(&self) -> &str {
        &self.username
    }

    pub(crate) fn password(&self) -> &str {
        &self.password
    }

    pub fn profile_id(&self) -> ProfileId {
        let mut digest = Sha1::new();
        digest.update(self.server.as_bytes());
        digest.update([0]);
        digest.update(self.username.as_bytes());
        ProfileId::new(format!("{:x}", digest.finalize()))
    }

    /// Atomically replaces a JSON credential file. On Unix the temporary and
    /// final files are owner-readable/writable only (0600).
    pub fn save(&self, path: &Path) -> Result<(), CredentialError> {
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty());
        if let Some(parent) = parent {
            fs::create_dir_all(parent).map_err(CredentialError::Io)?;
        }
        let bytes = serde_json::to_vec_pretty(self).map_err(CredentialError::Encode)?;
        let temp = temporary_path(path);

        let write_result = (|| -> Result<(), CredentialError> {
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                options.mode(0o600);
            }
            let mut file = options.open(&temp).map_err(CredentialError::Io)?;
            file.write_all(&bytes).map_err(CredentialError::Io)?;
            file.write_all(b"\n").map_err(CredentialError::Io)?;
            file.sync_all().map_err(CredentialError::Io)?;
            crate::paths::replace_file(&temp, path).map_err(CredentialError::Io)?;
            set_owner_only(path)?;
            sync_parent(path);
            Ok(())
        })();
        if write_result.is_err() {
            let _ = fs::remove_file(&temp);
        }
        write_result
    }

    pub fn load(path: &Path) -> Result<Self, CredentialError> {
        set_owner_only(path)?;
        let bytes = fs::read(path).map_err(CredentialError::Io)?;
        let stored: Credentials =
            serde_json::from_slice(&bytes).map_err(CredentialError::Decode)?;
        Self::new(stored.server, stored.username, stored.password)
    }
}

impl fmt::Debug for Credentials {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Credentials")
            .field("server", &self.server)
            .field("username", &self.username)
            .field("password", &"<redacted>")
            .finish()
    }
}

impl fmt::Display for Credentials {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} at {}", self.username, self.server)
    }
}

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct RequestAuthentication {
    pub salt: String,
    pub token: String,
}

impl fmt::Debug for RequestAuthentication {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RequestAuthentication")
            .field("salt", &"<redacted>")
            .field("token", &"<redacted>")
            .finish()
    }
}

pub(crate) fn request_authentication(password: &str) -> RequestAuthentication {
    let mut random = [0_u8; 12];
    rand::rng().fill_bytes(&mut random);
    let salt = hex(&random);
    let token = format!(
        "{:x}",
        md5::compute([password.as_bytes(), salt.as_bytes()].concat())
    );
    RequestAuthentication { salt, token }
}

fn normalize_server_url(raw: &str) -> Result<String, CredentialError> {
    let raw = raw.trim();
    let mut url = Url::parse(raw).map_err(|_| CredentialError::InvalidServerUrl)?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || authority_has_userinfo(raw)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(CredentialError::InvalidServerUrl);
    }
    if url.path() == "/" {
        url.set_path("");
    } else {
        // `Url::path()` is already percent encoded. Feeding it back through
        // `set_path` encodes every `%` again (`%20` becomes `%2520`). Editing
        // only empty trailing segments leaves the parsed path bytes intact.
        while url.path().ends_with('/') {
            url.path_segments_mut()
                .map_err(|_| CredentialError::InvalidServerUrl)?
                .pop_if_empty();
        }
    }
    Ok(url.to_string().trim_end_matches('/').to_owned())
}

fn authority_has_userinfo(raw: &str) -> bool {
    raw.split_once("://")
        .map(|(_, remainder)| {
            remainder
                .split(['/', '?', '#'])
                .next()
                .is_some_and(|authority| authority.contains('@'))
        })
        .unwrap_or(false)
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    out
}

fn temporary_path(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("credentials");
    let mut random = [0_u8; 8];
    rand::rng().fill_bytes(&mut random);
    path.with_file_name(format!(
        ".{file_name}.{}.{}.tmp",
        std::process::id(),
        hex(&random)
    ))
}

#[cfg(unix)]
fn set_owner_only(path: &Path) -> Result<(), CredentialError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(CredentialError::Io)
}

#[cfg(not(unix))]
fn set_owner_only(_path: &Path) -> Result<(), CredentialError> {
    Ok(())
}

fn sync_parent(path: &Path) {
    #[cfg(unix)]
    if let Some(directory) = path
        .parent()
        .and_then(|parent| OpenOptions::new().read(true).open(parent).ok())
    {
        let _ = directory.sync_all();
    }
}

#[derive(Debug, Error)]
pub enum CredentialError {
    #[error(
        "server URL must be an http(s) origin or base path without credentials, query, or fragment"
    )]
    InvalidServerUrl,
    #[error("username must not be empty")]
    InvalidUsername,
    #[error("unable to access the credential file: {0}")]
    Io(#[source] io::Error),
    #[error("credential file is not valid JSON")]
    Decode(#[source] serde_json::Error),
    #[error("unable to encode the credential file")]
    Encode(#[source] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn official_md5_token_vector() {
        let salt = "c19b2d";
        let token = format!("{:x}", md5::compute(format!("sesame{salt}")));
        assert_eq!(token, "26719a1196d2a940705a59634eb18eab");
    }

    #[test]
    fn request_tokens_have_fresh_salts_and_expected_shape() {
        let first = request_authentication("secret");
        let second = request_authentication("secret");
        assert_ne!(first.salt, second.salt);
        assert_eq!(first.salt.len(), 24);
        assert_eq!(first.token.len(), 32);
        assert!(first.salt.bytes().all(|byte| byte.is_ascii_hexdigit()));
        assert!(first.token.bytes().all(|byte| byte.is_ascii_hexdigit()));
    }

    #[test]
    fn credentials_normalize_without_leaking_password() {
        let credentials = Credentials::new(
            " https://music.example.test/navidrome/// ",
            " alice ",
            "do-not-print",
        )
        .unwrap();
        assert_eq!(credentials.server(), "https://music.example.test/navidrome");
        assert_eq!(credentials.username(), "alice");
        assert!(!format!("{credentials:?}").contains("do-not-print"));
        assert!(!credentials.to_string().contains("do-not-print"));
        assert_eq!(credentials.profile_id().as_str().len(), 40);
        assert_eq!(
            Credentials::new("http://localhost:4533", "alice", "different-password")
                .unwrap()
                .profile_id()
                .as_str(),
            "1c12d49145ac5cb1b31a9ef0baa6ef7372d70ee0"
        );

        for invalid in [
            "ftp://music.example.test",
            "https://@music.example.test",
            "https://alice:secret@music.example.test",
            "https://music.example.test/?token=secret",
            "https://music.example.test/#secret",
        ] {
            assert!(matches!(
                Credentials::new(invalid, "alice", "secret"),
                Err(CredentialError::InvalidServerUrl)
            ));
        }
    }

    #[test]
    fn encoded_base_paths_are_not_encoded_a_second_time() {
        let encoded = Credentials::new(
            "https://music.example.test/Music%20Server/%E9%9F%B3%E4%B9%90/%2F/",
            "alice",
            "secret",
        )
        .unwrap();
        assert_eq!(
            encoded.server(),
            "https://music.example.test/Music%20Server/%E9%9F%B3%E4%B9%90/%2F"
        );
        assert!(!encoded.server().contains("%25"));
    }

    #[test]
    fn profile_ids_reject_noncanonical_construction_and_deserialization() {
        let valid = "0123456789abcdef0123456789abcdef01234567";
        assert_eq!(ProfileId::try_new(valid).unwrap().as_str(), valid);
        for invalid in [
            "",
            "profile",
            "ABCDEF0123456789abcdef0123456789abcdef01",
            "0123:567890123456789012345678901234567890",
        ] {
            assert!(ProfileId::try_new(invalid).is_err());
            assert!(serde_json::from_str::<ProfileId>(&format!("\"{invalid}\"")).is_err());
        }
        let encoded = serde_json::to_string(&ProfileId::new(valid)).unwrap();
        assert_eq!(
            serde_json::from_str::<ProfileId>(&encoded)
                .unwrap()
                .as_str(),
            valid
        );
    }

    #[test]
    fn credential_file_round_trips_legacy_field_aliases_atomically() {
        let directory = std::env::temp_dir().join(format!(
            "fastpotify-auth-test-{}-{}",
            std::process::id(),
            hex(&rand::random::<[u8; 8]>())
        ));
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("credentials.json");
        let credentials = Credentials::new("http://localhost:4533/", "alice", "secret").unwrap();
        credentials.save(&path).unwrap();
        assert_eq!(Credentials::load(&path).unwrap(), credentials);
        assert!(
            !directory
                .read_dir()
                .unwrap()
                .flatten()
                .any(|entry| entry.file_name().to_string_lossy().ends_with(".tmp"))
        );

        fs::write(
            &path,
            r#"{"server_url":"https://example.test/music/","user":"bob","password":"pw"}"#,
        )
        .unwrap();
        let legacy = Credentials::load(&path).unwrap();
        assert_eq!(legacy.server(), "https://example.test/music");
        assert_eq!(legacy.username(), "bob");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        fs::remove_dir_all(directory).unwrap();
    }
}
