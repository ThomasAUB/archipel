//! One process, many windows (spec §2 "Several instances side by side").
//!
//! The first Archipel process becomes the *primary*: it listens on a
//! loopback TCP port and writes `port token` to `<data>/instance` (written
//! atomically). A later `archipel [<dir>]` reads that file, connects, sends
//! its request with the token and exits once the primary acknowledges it; the
//! primary then opens a window (or focuses the one already showing that
//! project). A window there costs only its project's data, not another GPU
//! context, font set and caches.
//!
//! Robustness: a stale file (crashed primary), a port reused by another
//! program or a wrong token all make the connection fail the handshake, and
//! the new process simply becomes the primary. Only the loopback interface is
//! used, and the random token (readable only through the user's data
//! directory) keeps other local users from opening windows.
//!
//! Protocol (one request per connection, UTF-8 lines):
//! `ARCHIPEL1 <token>\n` then `open\n` or `open <absolute dir>\n`; the reply
//! is `ok\n`.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// File holding the primary's port and token.
const INSTANCE_FILE: &str = "instance";
/// First line of every request.
const MAGIC: &str = "ARCHIPEL1";
/// Command opening a window.
const OPEN: &str = "open";
/// Reply of the primary.
const ACK: &str = "ok";
/// How long a launch waits to reach a running primary before starting its
/// own (a live primary answers in well under a millisecond).
const CONNECT_TIMEOUT: Duration = Duration::from_millis(300);
/// How long either side waits for the other's line.
const IO_TIMEOUT: Duration = Duration::from_secs(2);
/// Longest request line accepted (a path).
const MAX_LINE: u64 = 64 * 1024;

/// What a launch asks the primary for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    /// Directory to open (`archipel <dir>`); a new empty window when `None`.
    pub dir: Option<PathBuf>,
}

/// The listening side, owned by the primary.
pub struct Listener {
    listener: TcpListener,
    token: String,
}

/// Outcome of [`forward_or_listen`].
pub enum Startup {
    /// This process is the primary: serve requests with the listener.
    Primary(Listener),
    /// A running primary took the request: this process should exit.
    Forwarded,
}

/// Hands `request` to a running primary, or becomes the primary.
///
/// Any failure to become the primary (no data dir, port unavailable) is
/// logged and the process runs standalone, as before this feature.
pub fn forward_or_listen(data_dir: &Path, request: &Request) -> Option<Startup> {
    let file = data_dir.join(INSTANCE_FILE);
    if let Some((port, token)) = read_instance_file(&file) {
        match forward(port, &token, request) {
            Ok(()) => return Some(Startup::Forwarded),
            Err(e) => tracing::info!(error = %e, "no running instance answered; starting one"),
        }
    }
    match listen(&file) {
        Ok(listener) => Some(Startup::Primary(listener)),
        Err(e) => {
            tracing::warn!(error = %e, "cannot listen for other launches; running standalone");
            None
        }
    }
}

impl Listener {
    /// Serves requests on a background thread, calling `on_request` for each
    /// valid one (after acknowledging it).
    pub fn serve(self, on_request: impl Fn(Request) + Send + 'static) {
        let spawned = std::thread::Builder::new()
            .name("archipel-instance".into())
            .spawn(move || {
                for stream in self.listener.incoming() {
                    let Ok(stream) = stream else { continue };
                    match handle(stream, &self.token) {
                        Ok(request) => on_request(request),
                        Err(e) => tracing::debug!(error = %e, "rejected an instance request"),
                    }
                }
            });
        if let Err(e) = spawned {
            tracing::warn!(error = %e, "cannot start the instance listener");
        }
    }
}

fn read_instance_file(file: &Path) -> Option<(u16, String)> {
    let text = std::fs::read_to_string(file).ok()?;
    let (port, token) = text.trim().split_once(' ')?;
    Some((port.parse().ok()?, token.to_owned()))
}

fn forward(port: u16, token: &str, request: &Request) -> io::Result<()> {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
    let mut stream = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT)?;
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    let command = match &request.dir {
        Some(dir) => {
            let dir = dir.to_str().ok_or_else(|| invalid("path is not UTF-8"))?;
            if dir.contains('\n') {
                return Err(invalid("path contains a newline"));
            }
            format!("{OPEN} {dir}")
        }
        None => OPEN.to_owned(),
    };
    write!(stream, "{MAGIC} {token}\n{command}\n")?;
    stream.flush()?;
    let mut reply = String::new();
    BufReader::new(stream)
        .take(MAX_LINE)
        .read_line(&mut reply)?;
    if reply.trim() == ACK {
        Ok(())
    } else {
        Err(invalid("unexpected reply"))
    }
}

fn listen(file: &Path) -> io::Result<Listener> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    let port = listener.local_addr()?.port();
    let token = new_token();
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temp = file.with_extension("tmp");
    write_private(&temp, format!("{port} {token}\n").as_bytes())?;
    std::fs::rename(&temp, file)?;
    Ok(Listener { listener, token })
}

/// Writes a file only the current user can read. On Windows the per-user
/// data directory already ensures it; on Unix `~/.local/share` is often
/// world-readable, so the file itself is created `0600`.
fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    // A leftover from a crashed launch may have other permissions: start fresh.
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
        _ => {}
    }
    options.open(path)?.write_all(bytes)
}

fn handle(stream: TcpStream, token: &str) -> io::Result<Request> {
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    let mut reader = BufReader::new(stream.try_clone()?).take(MAX_LINE);
    let mut hello = String::new();
    reader.read_line(&mut hello)?;
    if hello.trim_end() != format!("{MAGIC} {token}") {
        return Err(invalid("bad handshake"));
    }
    let mut line = String::new();
    reader.read_line(&mut line)?;
    let request = parse_command(line.trim_end_matches(['\r', '\n']))?;
    let mut stream = stream;
    writeln!(stream, "{ACK}")?;
    Ok(request)
}

fn parse_command(line: &str) -> io::Result<Request> {
    match line.split_once(' ') {
        None if line == OPEN => Ok(Request { dir: None }),
        Some((OPEN, dir)) if !dir.is_empty() => Ok(Request {
            dir: Some(PathBuf::from(dir)),
        }),
        _ => Err(invalid("unknown command")),
    }
}

/// 128 random bits as hex (std's `RandomState` keys are random per process).
fn new_token() -> String {
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let part = |salt: u8| {
        let mut h = RandomState::new().build_hasher();
        h.write_u8(salt);
        h.write_u32(pid);
        h.write_u128(nanos);
        h.finish()
    };
    format!("{:016x}{:016x}", part(1), part(2))
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn second_launch_is_forwarded_to_the_primary() {
        let dir = tempfile::tempdir().unwrap();
        let Some(Startup::Primary(listener)) =
            forward_or_listen(dir.path(), &Request { dir: None })
        else {
            panic!("the first launch must become the primary");
        };
        let (tx, rx) = mpsc::channel();
        listener.serve(move |r| tx.send(r).unwrap());
        let wanted = Request {
            dir: Some(dir.path().join("some project")),
        };
        assert!(matches!(
            forward_or_listen(dir.path(), &wanted),
            Some(Startup::Forwarded)
        ));
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap(), wanted);
        assert!(matches!(
            forward_or_listen(dir.path(), &Request { dir: None }),
            Some(Startup::Forwarded)
        ));
        assert_eq!(rx.recv_timeout(Duration::from_secs(5)).unwrap().dir, None);
    }

    #[test]
    fn stale_file_or_wrong_token_starts_a_new_primary() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(INSTANCE_FILE);
        // A port nobody listens on.
        let free = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = free.local_addr().unwrap().port();
        drop(free);
        std::fs::write(&file, format!("{port} deadbeef\n")).unwrap();
        assert!(matches!(
            forward_or_listen(dir.path(), &Request { dir: None }),
            Some(Startup::Primary(_))
        ));

        // A listener with another token rejects the request.
        let other = listen(&dir.path().join("other")).unwrap();
        let port = other.listener.local_addr().unwrap().port();
        other.serve(|_| panic!("must not be accepted"));
        assert!(forward(port, "wrong", &Request { dir: None }).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn instance_file_is_private_to_the_user() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join(INSTANCE_FILE);
        // A leftover temp file with loose permissions is replaced, not reused.
        std::fs::write(file.with_extension("tmp"), "old").unwrap();
        listen(&file).unwrap();
        let mode = std::fs::metadata(&file).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }

    #[test]
    fn commands_parse() {
        assert_eq!(parse_command("open").unwrap().dir, None);
        assert_eq!(
            parse_command("open C:\\a b").unwrap().dir,
            Some(PathBuf::from("C:\\a b"))
        );
        assert!(parse_command("close").is_err());
        assert!(parse_command("open ").is_err());
    }
}
