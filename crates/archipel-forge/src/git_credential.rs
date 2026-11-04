//! The forge tokens as a git credential helper, so one sign-in also covers
//! HTTPS fetch and push.
//!
//! The git adapter registers the `archipel` binary with
//! `-c credential.helper=...`. Git asks the user's own helpers first: this one
//! only answers `get` for an `https` host with a signed-in account, and
//! ignores `store` and `erase` (the keychain is managed by sign-in and
//! sign-out, never by git).
//!
//! Protocol: <https://git-scm.com/docs/git-credential#IOFMT>.

use archipel_model::ForgeError;
use archipel_model::forge::normalize_host;
use archipel_model::ports::{CredentialStore, ForgeAccountStore};

/// Answers one helper invocation: `op` is `get`, `store` or `erase`, `input`
/// the `key=value` lines git wrote on stdin. Returns what to print on stdout
/// (empty: no credential for this request).
pub fn answer(
    op: &str,
    input: &str,
    accounts: &dyn ForgeAccountStore,
    credentials: &dyn CredentialStore,
) -> Result<String, ForgeError> {
    if op != "get" {
        return Ok(String::new());
    }
    let field = |key: &str| {
        input
            .lines()
            .find_map(|l| l.strip_prefix(key)?.strip_prefix('='))
            .map(str::trim)
    };
    if field("protocol") != Some("https") {
        return Ok(String::new());
    }
    let Some(host) = field("host").map(normalize_host) else {
        return Ok(String::new());
    };
    let Some(account) = accounts.accounts()?.into_iter().find(|a| a.host == host) else {
        return Ok(String::new());
    };
    let Some(token) = credentials.token(&account.host)? else {
        return Ok(String::new());
    };
    // A user name in the remote URL wins: git only asks for the password.
    let user = field("username").unwrap_or(&account.user);
    Ok(format!("username={user}\npassword={}\n", token.expose()))
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Mutex;

    use archipel_model::forge::{ForgeAccount, ForgeKind, ForgeToken};

    use super::*;

    struct Fake {
        accounts: Vec<ForgeAccount>,
        tokens: Mutex<HashMap<String, ForgeToken>>,
    }

    impl ForgeAccountStore for Fake {
        fn accounts(&self) -> Result<Vec<ForgeAccount>, ForgeError> {
            Ok(self.accounts.clone())
        }
        fn save_accounts(&self, _: &[ForgeAccount]) -> Result<(), ForgeError> {
            unreachable!("the helper never changes accounts")
        }
    }

    impl CredentialStore for Fake {
        fn token(&self, host: &str) -> Result<Option<ForgeToken>, ForgeError> {
            Ok(self.tokens.lock().unwrap().get(host).cloned())
        }
        fn set_token(&self, _: &str, _: &ForgeToken) -> Result<(), ForgeError> {
            unreachable!("the helper never stores")
        }
        fn delete_token(&self, _: &str) -> Result<(), ForgeError> {
            unreachable!("the helper never erases")
        }
    }

    fn signed_in() -> Fake {
        Fake {
            accounts: vec![ForgeAccount {
                host: "github.com".into(),
                kind: ForgeKind::github(),
                api_url: "https://api.github.com".into(),
                user: "octo".into(),
            }],
            tokens: Mutex::new(HashMap::from([(
                "github.com".to_owned(),
                ForgeToken::new("secret"),
            )])),
        }
    }

    fn get(input: &str) -> String {
        let f = signed_in();
        answer("get", input, &f, &f).unwrap()
    }

    #[test]
    fn answers_a_signed_in_https_host() {
        assert_eq!(
            get("protocol=https\nhost=github.com\n"),
            "username=octo\npassword=secret\n"
        );
        assert_eq!(
            get("protocol=https\nhost=GitHub.com\nusername=me\n"),
            "username=me\npassword=secret\n"
        );
    }

    #[test]
    fn stays_silent_otherwise() {
        assert_eq!(get("protocol=https\nhost=gitlab.com\n"), "");
        assert_eq!(get("protocol=http\nhost=github.com\n"), "");
        assert_eq!(get("host=github.com\n"), "");
        let f = signed_in();
        let input = "protocol=https\nhost=github.com\nusername=octo\npassword=secret\n";
        assert_eq!(answer("store", input, &f, &f).unwrap(), "");
        assert_eq!(answer("erase", input, &f, &f).unwrap(), "");
    }
}
