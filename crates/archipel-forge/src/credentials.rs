//! Where forge credentials live: tokens in the OS keychain
//! ([`KeychainCredentials`]), the non-secret account list in a TOML file of
//! the user config directory ([`FileAccountStore`]). Neither ever touches
//! `archipel.toml`.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError, mpsc};
use std::time::Duration;

use archipel_model::ForgeError;
use archipel_model::forge::{ForgeAccount, ForgeToken};
use archipel_model::ports::{CredentialStore, ForgeAccountStore};
use serde::{Deserialize, Serialize};

/// How long a token read waits for the keychain. A read normally takes
/// milliseconds; a locked keychain may instead wait on an unlock prompt that
/// never gets an answer (Secret Service: `KWallet`, GNOME Keyring).
const READ_TIMEOUT: Duration = Duration::from_secs(5);

/// Tokens in the OS keychain (Windows Credential Manager, macOS Keychain,
/// Secret Service on Linux), one entry per host under one service name.
///
/// Reads are bounded by [`READ_TIMEOUT`]: past it the read fails with
/// [`ForgeError::Keychain`] and keeps waiting in the background, and later
/// reads of that host fail at once until it ends (so a locked keychain costs
/// a few waiting threads per host, not one per forge request). Reads that
/// merely overlap all run: a refresh reads one host for many repos at once.
pub struct KeychainCredentials {
    service: String,
    reads: BoundedReads,
}

impl KeychainCredentials {
    /// A store using `service` as the keychain service name.
    #[must_use]
    pub fn new(service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
            reads: BoundedReads::new(READ_TIMEOUT),
        }
    }

    fn entry(&self, host: &str) -> Result<keyring::Entry, ForgeError> {
        keyring::Entry::new(&self.service, host).map_err(|e| ForgeError::Keychain(e.to_string()))
    }
}

impl CredentialStore for KeychainCredentials {
    fn token(&self, host: &str) -> Result<Option<ForgeToken>, ForgeError> {
        let entry = self.entry(host)?;
        self.reads.run(host, move || match entry.get_password() {
            Ok(secret) => Ok(Some(ForgeToken::new(secret))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(ForgeError::Keychain(e.to_string())),
        })
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

/// Keychain reads that give up after a timeout.
struct BoundedReads {
    timeout: Duration,
    /// Hosts with a read that outlived its timeout and is still waiting:
    /// further reads of them fail at once instead of piling up threads.
    stuck: Arc<Mutex<HashSet<String>>>,
}

impl BoundedReads {
    fn new(timeout: Duration) -> Self {
        Self {
            timeout,
            stuck: Arc::default(),
        }
    }

    /// Runs `read` for `host` on its own thread, waiting at most the timeout.
    /// Concurrent reads of one host each run; only a host whose read timed
    /// out (and has not ended since) fails at once.
    fn run<T: Send + 'static>(
        &self,
        host: &str,
        read: impl FnOnce() -> Result<T, ForgeError> + Send + 'static,
    ) -> Result<T, ForgeError> {
        let not_answering = || {
            ForgeError::Keychain(format!(
                "no answer for {host} within {} s; is the keychain locked?",
                self.timeout.as_secs()
            ))
        };
        if lock(&self.stuck).contains(host) {
            return Err(not_answering());
        }
        let (tx, rx) = mpsc::channel();
        let stuck = Arc::clone(&self.stuck);
        let owned_host = host.to_owned();
        let spawned = std::thread::Builder::new()
            .name("archipel-keychain".into())
            .spawn(move || {
                let result = read();
                // Under the lock, so a caller timing out at this very moment
                // either sees the result or marks the host before we unmark it.
                let mut stuck = lock(&stuck);
                // The caller may have given up already.
                let _ = tx.send(result);
                stuck.remove(&owned_host);
            });
        if let Err(e) = spawned {
            return Err(ForgeError::Keychain(format!("cannot start a read: {e}")));
        }
        if let Ok(result) = rx.recv_timeout(self.timeout) {
            return result;
        }
        let mut stuck = lock(&self.stuck);
        if let Ok(result) = rx.try_recv() {
            return result;
        }
        stuck.insert(host.to_owned());
        tracing::warn!(host, "keychain read timed out");
        Err(not_answering())
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
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
    fn bounded_read_returns_a_prompt_answer() {
        let reads = BoundedReads::new(Duration::from_secs(5));
        assert_eq!(reads.run("h", || Ok(7)).unwrap(), 7);
        assert!(matches!(
            reads.run::<()>("h", || Err(ForgeError::Keychain("boom".into()))),
            Err(ForgeError::Keychain(m)) if m == "boom"
        ));
    }

    #[test]
    fn stuck_read_times_out_and_blocks_only_its_host_until_it_ends() {
        let reads = BoundedReads::new(Duration::from_millis(50));
        let (release, wait) = mpsc::channel::<()>();
        let stuck = reads.run("locked.example", move || {
            let _ = wait.recv();
            Ok(1)
        });
        assert!(matches!(stuck, Err(ForgeError::Keychain(m)) if m.contains("locked.example")));
        // A second read of that host fails at once, without a new thread.
        let started = std::time::Instant::now();
        assert!(reads.run("locked.example", || Ok(2)).is_err());
        assert!(started.elapsed() < Duration::from_millis(50));
        // Other hosts are unaffected.
        assert_eq!(reads.run("other.example", || Ok(3)).unwrap(), 3);
        // Once the stuck read ends (keychain unlocked), the host reads again.
        release.send(()).unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            match reads.run("locked.example", || Ok(4)) {
                Ok(v) => break assert_eq!(v, 4),
                Err(_) if std::time::Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(e) => panic!("host still blocked: {e}"),
            }
        }
    }

    #[test]
    fn overlapping_reads_of_one_host_all_succeed() {
        let reads = Arc::new(BoundedReads::new(Duration::from_secs(5)));
        let barrier = Arc::new(std::sync::Barrier::new(8));
        let handles: Vec<_> = (0..8)
            .map(|i| {
                let (reads, barrier) = (Arc::clone(&reads), Arc::clone(&barrier));
                std::thread::spawn(move || {
                    reads.run("github.com", move || {
                        // Every read is in flight before any returns.
                        barrier.wait();
                        Ok(i)
                    })
                })
            })
            .collect();
        for (i, h) in handles.into_iter().enumerate() {
            assert_eq!(h.join().unwrap().unwrap(), i);
        }
    }

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
