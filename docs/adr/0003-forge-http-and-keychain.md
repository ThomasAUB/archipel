# ADR 0003 — Forge integration: `ureq` for HTTP, `keyring` for tokens

- Status: accepted
- Date: 2026-11-04
- Context: SPEC §8 (forge integration), ARCHITECTURE §2 and §3.

## Context

The forge integration talks to the GitHub REST API (v3, also GitHub
Enterprise Server) and the GitLab REST API (v4, gitlab.com and
self-hosted). It needs an HTTP client and a place to keep personal access
tokens that is **not** `archipel.toml` nor any plain file.

Constraints:

- Every forge call already runs inside an `archipel-ops` scheduler job, i.e.
  on a **blocking** worker thread of the network pool (bounded concurrency,
  per-repo serialization, cancellation before start, operations drawer).
  An async client would need its own reactor or `block_on` inside the job.
- Small dependency tree and binary (RAM and start-up targets), pure Rust TLS
  so Windows and Linux builds need no OpenSSL.
- Self-hosted forges often sit behind a corporate CA: the OS trust store must
  be honored. Corporate proxies are configured through `HTTPS_PROXY`.
- Tests must not touch the network.

## Decision

### HTTP: `ureq` 3

- Blocking API that fits the scheduler's blocking jobs with no extra runtime.
- rustls (pure Rust), with `rustls-platform-verifier` (`RootCerts::PlatformVerifier`)
  so the OS certificate store (and any corporate CA in it) is trusted.
- Proxy from the environment by default; one global timeout per request
  (`Settings::forge_timeout_secs`, default 20 s).
- Rejected: `reqwest` (async, pulls `hyper`/`tower`; its blocking mode spins
  up a tokio runtime per client), `curl` (C dependency, libcurl on Linux),
  `attohttpc` (smaller community, no platform verifier).

`ureq` stays behind the `HttpTransport` trait of `archipel-forge`: the
GitHub and GitLab providers only build requests and parse answers, and are
tested against a fake transport (`crates/archipel-forge/tests/providers.rs`).

### Tokens: `keyring` 3

- One crate over the native stores: Windows Credential Manager
  (`windows-native`), macOS Keychain (`apple-native`), Secret Service on Linux
  (`sync-secret-service` + `crypto-rust`; builds need `libdbus-1-dev`).
- One entry per host under the service `dev.archipel.forge`.
- 3.6 rather than 4.x: 4.x requires Rust 1.88 (workspace MSRV is 1.85) and
  splits stores into separate crates the composition root would have to pick.
- Rejected: storing tokens in the settings file or SQLite store (plain
  text), relying on Git credential helpers (they hold Git-over-HTTPS
  credentials, not API tokens, and their protocol varies per helper).

The non-secret part of an account (host, provider, API URL, user) is kept in
`forges.toml` in the user config directory, so the app can list accounts
without unlocking the keychain.

### Sign-in method: personal access token

OAuth device flow needs an OAuth application registered on each forge (and
on every self-hosted GitLab), with a client id shipped in the binary. That is
a product decision outside this change; personal access tokens work
everywhere today. The `ForgeSignIn` port can gain a device-flow
implementation later without touching consumers.

## Consequences

- New adapter crate `archipel-forge` (depends on `archipel-model` only),
  wired in `archipel-compose::forge_ports`.
- Linux CI and builds install `libdbus-1-dev` and `pkg-config`.
- A fork adds a forge (Gitea, Bitbucket…) by implementing the provider traits
  of `archipel-forge::provider` and calling `ForgeClient::register` in the
  composition root.
