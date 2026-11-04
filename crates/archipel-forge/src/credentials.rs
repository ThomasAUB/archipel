//! Where forge credentials live: tokens in the OS keychain
//! ([`KeychainCredentials`]), the non-secret account list in a TOML file of
//! the user config directory ([`FileAccountStore`]). Neither ever touches
//! `.gitproject.toml`.

use std::path::PathBuf;

use archipel_model::ForgeError;
use archipel_model::forge::{ForgeAccount, ForgeToken};
use archipel_model::ports::{CredentialStore, ForgeAccountStore};
use serde::{Deserialize, Serialize};

/// Tokens in the OS keychain (Windows Credential Manager, macOS Keychain,
/// Secret Service on Linux), one entry per host under one service name.
pub struct KeychainCredentials {
    service: String,
}

impl KeychainCredentials {
    /// A store using `service` as the keychain service name.
    #[must_use]
    pub fn new(service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
        }
    }

    fn entry(&self, host: &str) -> Result<keyring::Entry, ForgeError> {
        keyring::Entry::new(&self.service, host).map_err(|e| ForgeError::Keychain(e.to_string()))
    }
}

impl CredentialStore for KeychainCredentials {
    fn token(&self, host: &str) -> Result<Option<ForgeToken>, ForgeError> {
        match self.entry(host)?.get_password() {
            Ok(secret) => Ok(Some(ForgeToken::new(secret))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(ForgeError::Keychain(e.to_string())),
        }
    }

    fn set_token(&self, host: &str, token: &ForgeToken) -> Result<(), ForgeError> {
        self.entry(host)?
            .set_password(token.expose())
            .map_err(|e| ForgeError::Keychain(e.to_string()))
    }

    fn delete_token(&self, host: &str) -> Result<(), ForgeError> {
        match self.entry(host)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(ForgeError::Keychain(e.to_string())),
        }
    }
}

/// The account list as a TOML file (`forges.toml`).
pub struct FileAccountStore {
    path: PathBuf,
}

#[derive(Default, Serialize, Deserialize)]
struct AccountsFile {
    #[serde(default, rename = "account")]
    accounts: Vec<ForgeAccount>,
}

impl FileAccountStore {
    /// A store reading and writing `path` (missing file: no account).
    #[must_use]
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

impl ForgeAccountStore for FileAccountStore {
    fn accounts(&self) -> Result<Vec<ForgeAccount>, ForgeError> {
        let text = match std::fs::read_to_string(&self.path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => {
                return Err(ForgeError::Accounts(format!(
                    "cannot read {}: {e}",
                    self.path.display()
                )));
            }
        };
        toml::from_str::<AccountsFile>(&text)
            .map(|f| f.accounts)
            .map_err(|e| ForgeError::Accounts(format!("invalid {}: {e}", self.path.display())))
    }

    fn save_accounts(&self, accounts: &[ForgeAccount]) -> Result<(), ForgeError> {
        let file = AccountsFile {
            accounts: accounts.to_vec(),
        };
        let text =
            toml::to_string_pretty(&file).map_err(|e| ForgeError::Accounts(e.to_string()))?;
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| ForgeError::Accounts(e.to_string()))?;
        }
        // Write then rename, so a concurrent reader (another window) never
        // sees a half-written file.
        let tmp = self.path.with_extension("toml.tmp");
        std::fs::write(&tmp, text)
            .and_then(|()| std::fs::rename(&tmp, &self.path))
            .map_err(|e| ForgeError::Accounts(format!("cannot write {}: {e}", self.path.display())))
    }
}

#[cfg(test)]
mod tests {
    use archipel_model::forge::ForgeKind;

    use super::*;

    #[test]
    fn account_file_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let store = FileAccountStore::new(dir.path().join("cfg/forges.toml"));
        assert!(store.accounts().unwrap().is_empty());
        let account = ForgeAccount {
            host: "gitlab.example.com".into(),
            kind: ForgeKind::gitlab(),
            api_url: "https://gitlab.example.com/api/v4".into(),
            user: "me".into(),
        };
        store.save_accounts(std::slice::from_ref(&account)).unwrap();
        assert_eq!(store.accounts().unwrap(), [account]);
        let text = std::fs::read_to_string(dir.path().join("cfg/forges.toml")).unwrap();
        assert!(text.contains("[[account]]") && text.contains("kind = \"gitlab\""));
    }
}
