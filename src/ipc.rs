//! A local control channel between Sorta processes.
//!
//! Multi-instance needs two things: a way to know who else is running, and a way to
//! ask a running instance something without starting a second engine over the same
//! folders. The registry file answers the first; this answers the second.
//!
//! Transport: **loopback TCP**, `std::net` only. One JSON object per line, both ways.
//! The listening instance binds `127.0.0.1:0`, publishes the ephemeral port and its
//! pid in `~/.sorta/port`, and a caller connects there after checking the pid is
//! still alive.
//!
//! Loopback TCP rather than a Unix socket or a named pipe, because it is the one
//! transport that behaves identically on both target platforms with no `unsafe` and
//! no extra crate. The pattern is borrowed from CLDR (same author), which uses it for
//! its summon signal for exactly the same reason.
//!
//! What the channel deliberately does **not** do is move files. Handing a path to
//! another process to sort would make the audit trail lie about which process did
//! it. Today it answers `Ping` and `Status`; anything that mutates state should be
//! added with authentication, not before.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// File that publishes the listening port and pid.
pub const PORT_FILE: &str = "port";

/// How long a caller waits for an answer.
pub const CALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

/// Hard cap on one request line; anything larger is malformed.
const MAX_LINE: u64 = 4096;

/// A question for a running instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "request", rename_all = "snake_case")]
pub enum Request {
    /// Is anything listening, and who?
    Ping,
    /// What is the running instance doing?
    Status,
}

/// The answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "response", rename_all = "snake_case")]
pub enum Response {
    Pong {
        pid: u32,
    },
    Status {
        pid: u32,
        paused: bool,
        instances: usize,
        indexed: usize,
        quarantined: usize,
    },
    Error {
        message: String,
    },
}

/// Where the port file lives for a given state directory.
pub fn port_path(state_dir: &Path) -> PathBuf {
    state_dir.join(PORT_FILE)
}

/// Serves requests on a background thread until the process exits.
///
/// The port file is written before the thread starts, so a caller that sees the file
/// always has a listener behind it.
pub fn serve<F>(state_dir: &Path, handler: F) -> std::io::Result<std::thread::JoinHandle<()>>
where
    F: Fn(Request) -> Response + Send + 'static,
{
    crate::paths::ensure_dir(state_dir)?;
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    let port = listener.local_addr()?.port();
    std::fs::write(
        port_path(state_dir),
        format!("{port}\n{}\n", std::process::id()),
    )?;

    Ok(std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let _ = stream.set_read_timeout(Some(CALL_TIMEOUT));

            let mut line = String::new();
            if BufReader::new(&stream)
                .take(MAX_LINE)
                .read_line(&mut line)
                .is_err()
            {
                continue;
            }

            let response = match serde_json::from_str::<Request>(line.trim()) {
                Ok(request) => handler(request),
                Err(error) => Response::Error {
                    message: format!("bad request: {error}"),
                },
            };
            let mut writer = &stream;
            let _ = writeln!(
                writer,
                "{}",
                serde_json::to_string(&response).unwrap_or_default()
            );
        }
    }))
}

/// Asks the running instance, waiting at most [`CALL_TIMEOUT`].
pub fn call(state_dir: &Path, request: &Request) -> anyhow::Result<Response> {
    let path = port_path(state_dir);
    let text = std::fs::read_to_string(&path).map_err(|_| {
        anyhow::anyhow!(
            "no instance is listening ({} is absent); start `sorta watch` first",
            path.display()
        )
    })?;

    let mut lines = text.lines();
    let port: u16 = lines
        .next()
        .and_then(|line| line.trim().parse().ok())
        .ok_or_else(|| anyhow::anyhow!("{} is malformed; delete it", path.display()))?;
    let pid: u32 = lines
        .next()
        .and_then(|line| line.trim().parse().ok())
        .unwrap_or(0);

    if pid != 0 && !crate::platform::pid_alive(pid) {
        anyhow::bail!(
            "instance {pid} is gone but {} remains; delete it, then start `sorta watch`",
            path.display()
        );
    }

    let address = SocketAddr::from(([127, 0, 0, 1], port));
    let mut stream = TcpStream::connect_timeout(&address, CALL_TIMEOUT)?;
    stream.set_read_timeout(Some(CALL_TIMEOUT))?;
    stream.set_write_timeout(Some(CALL_TIMEOUT))?;

    writeln!(stream, "{}", serde_json::to_string(request)?)?;

    let mut line = String::new();
    BufReader::new(&stream).read_line(&mut line)?;
    if line.trim().is_empty() {
        anyhow::bail!("the instance closed the connection without answering");
    }
    Ok(serde_json::from_str(line.trim())?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wait_for_server(state: &Path, request: &Request) -> anyhow::Result<Response> {
        let mut answer = call(state, request);
        for _ in 0..20 {
            if answer.is_ok() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
            answer = call(state, request);
        }
        answer
    }

    #[test]
    fn a_caller_gets_an_answer_from_the_server() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().to_path_buf();

        let handle = serve(&state, |request| match request {
            Request::Ping => Response::Pong {
                pid: std::process::id(),
            },
            Request::Status => Response::Status {
                pid: std::process::id(),
                paused: false,
                instances: 1,
                indexed: 7,
                quarantined: 2,
            },
        })
        .expect("server should bind");

        assert!(port_path(&state).exists(), "the port file must be published");

        match wait_for_server(&state, &Request::Ping).expect("ping is answered") {
            Response::Pong { pid } => assert_eq!(pid, std::process::id()),
            other => panic!("expected pong, got {other:?}"),
        }

        match call(&state, &Request::Status).expect("status is answered") {
            Response::Status {
                indexed,
                quarantined,
                ..
            } => {
                assert_eq!(indexed, 7);
                assert_eq!(quarantined, 2);
            }
            other => panic!("expected status, got {other:?}"),
        }

        drop(handle);
    }

    #[test]
    fn a_missing_port_file_says_how_to_start_one() {
        let dir = tempfile::tempdir().unwrap();
        let error = call(dir.path(), &Request::Ping).unwrap_err();
        assert!(error.to_string().contains("sorta watch"), "{error}");
    }

    #[cfg(unix)]
    #[test]
    fn a_stale_port_file_from_a_dead_process_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        // Port 1 will not have a listener, and pid 999999 is not running.
        std::fs::write(port_path(dir.path()), "1\n999999\n").unwrap();

        let error = call(dir.path(), &Request::Ping).unwrap_err().to_string();
        assert!(error.contains("is gone"), "{error}");
    }

    #[test]
    fn a_malformed_port_file_is_named() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(port_path(dir.path()), "not-a-port\n").unwrap();
        let error = call(dir.path(), &Request::Ping).unwrap_err().to_string();
        assert!(error.contains("malformed"), "{error}");
    }

    #[test]
    fn the_protocol_round_trips_as_json() {
        let request = Request::Status;
        let line = serde_json::to_string(&request).unwrap();
        assert_eq!(line, r#"{"request":"status"}"#);
        assert_eq!(serde_json::from_str::<Request>(&line).unwrap(), request);

        let response = Response::Pong { pid: 42 };
        let line = serde_json::to_string(&response).unwrap();
        assert_eq!(line, r#"{"response":"pong","pid":42}"#);
        assert_eq!(serde_json::from_str::<Response>(&line).unwrap(), response);
    }
}
