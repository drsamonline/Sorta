//! A local control channel between Sorta processes.
//!
//! Multi-instance needs two things: a way to know who else is running, and a way to
//! ask a running instance something without starting a second engine over the same
//! folders. The registry file answers the first; this answers the second.
//!
//! The protocol is transport-agnostic — one JSON object per line — so the backend
//! can change without touching callers:
//!
//! * **Unix** — a `SOCK_STREAM` socket at `~/.sorta/sorta.sock`, from `std`.
//! * **Windows** — not implemented. A named pipe needs `CreateNamedPipeW` and four
//!   more FFI calls that cannot be exercised in this environment, and shipping an
//!   untested unsafe surface for a status query is a bad trade. `serve` is a no-op
//!   and `call` says so; pause/resume do not depend on IPC because they are a flag
//!   file, so Windows loses no capability, only the status query.
//!
//! What the channel deliberately does **not** do is move files. Handing a path to
//! another process to sort would make the audit trail lie about which process did
//! it. Today it answers `Ping` and `Status`; anything that mutates state should be
//! added with authentication, not before.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Socket file name inside the state directory.
pub const SOCKET_NAME: &str = "sorta.sock";

/// How long a caller waits for an answer.
pub const CALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);

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

/// Where the socket lives for a given state directory.
pub fn socket_path(state_dir: &Path) -> PathBuf {
    state_dir.join(SOCKET_NAME)
}

/// True when a socket file is present (it may still be stale).
pub fn socket_exists(state_dir: &Path) -> bool {
    socket_path(state_dir).exists()
}

#[cfg(unix)]
mod backend {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::{UnixListener, UnixStream};

    /// Serves requests on a background thread until the process exits.
    ///
    /// A socket left behind by a crash is removed first: binding fails on an existing
    /// path, and a stale socket would otherwise make the instance unreachable forever.
    pub fn serve<F>(state_dir: &Path, handler: F) -> std::io::Result<std::thread::JoinHandle<()>>
    where
        F: Fn(Request) -> Response + Send + 'static,
    {
        let path = socket_path(state_dir);
        crate::paths::ensure_dir(state_dir)?;
        let _ = std::fs::remove_file(&path);

        let listener = UnixListener::bind(&path)?;

        Ok(std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut line = String::new();
                if BufReader::new(&stream).read_line(&mut line).is_err() {
                    continue;
                }
                let request = match serde_json::from_str::<Request>(line.trim()) {
                    Ok(request) => request,
                    Err(error) => {
                        let _ = writeln!(
                            stream,
                            "{}",
                            serde_json::to_string(&Response::Error {
                                message: format!("bad request: {error}")
                            })
                            .unwrap_or_default()
                        );
                        continue;
                    }
                };
                let response = handler(request);
                let _ = writeln!(
                    stream,
                    "{}",
                    serde_json::to_string(&response).unwrap_or_default()
                );
            }
        }))
    }

    /// Asks the running instance, waiting at most [`CALL_TIMEOUT`].
    pub fn call(state_dir: &Path, request: &Request) -> anyhow::Result<Response> {
        let path = socket_path(state_dir);
        let mut stream = UnixStream::connect(&path).map_err(|error| {
            anyhow::anyhow!(
                "no instance is listening on {} ({error}); start `sorta watch` first",
                path.display()
            )
        })?;
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
}

#[cfg(not(unix))]
mod backend {
    use super::*;

    pub fn serve<F>(_state_dir: &Path, _handler: F) -> std::io::Result<std::thread::JoinHandle<()>>
    where
        F: Fn(Request) -> Response + Send + 'static,
    {
        // Nothing to listen on yet; see the module docs for why.
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "IPC is not implemented on this platform (named pipe pending)",
        ))
    }

    pub fn call(_state_dir: &Path, _request: &Request) -> anyhow::Result<Response> {
        anyhow::bail!(
            "IPC is not implemented on this platform; use `sorta status` and the pause flag"
        )
    }
}

pub use backend::{call, serve};

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn a_caller_gets_an_answer_from_the_server() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().to_path_buf();
        let served = state.clone();

        let handle = serve(&served, |request| match request {
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

        // The listener thread is up as soon as bind returns, but give the socket a
        // moment on a slow filesystem.
        let mut answer = call(&state, &Request::Ping);
        for _ in 0..20 {
            if answer.is_ok() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
            answer = call(&state, &Request::Ping);
        }
        match answer.expect("ping should be answered") {
            Response::Pong { pid } => assert_eq!(pid, std::process::id()),
            other => panic!("expected pong, got {other:?}"),
        }

        match call(&state, &Request::Status).expect("status should be answered") {
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
        let _ = std::fs::remove_file(socket_path(&state));
    }

    #[cfg(unix)]
    #[test]
    fn a_missing_socket_says_how_to_start_one() {
        let dir = tempfile::tempdir().unwrap();
        let error = call(dir.path(), &Request::Ping).unwrap_err();
        assert!(error.to_string().contains("sorta watch"), "{error}");
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

    #[test]
    fn the_socket_lives_in_the_state_directory() {
        let state = Path::new("/home/u/.sorta");
        assert_eq!(socket_path(state), Path::new("/home/u/.sorta/sorta.sock"));
        assert!(!socket_exists(state));
    }
}
