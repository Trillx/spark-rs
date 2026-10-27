//! How AI agents reach spark-rs. Agents run `spark-cli`, which talks to the
//! running desktop app over a loopback connection: one JSON request line in,
//! one JSON response line out, then the connection closes.
//!
//! The app writes its port and a secret token to `agent-endpoint.json` in
//! its local data folder, which only the user can read; every request
//! carries the token. The file also names the app's executable, so the CLI
//! can start the app when it isn't running, except while the installer's
//! `updating` file says an update is replacing it.

use std::future::Future;
use std::io;
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{
    AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader,
};
use tokio::net::{TcpListener, TcpStream};

/// The desktop app's identifier, which names its data folder.
pub const IDENTIFIER: &str = "dev.sparkrs.desktop";
pub const ENDPOINT_FILE: &str = "agent-endpoint.json";
/// Written next to the endpoint file by the installer while it replaces the
/// app (`apps/desktop/src-tauri/installer/hooks.nsh`).
pub const UPDATING_FILE: &str = "updating";
/// Requests and responses are single lines no longer than this.
pub const MAX_LINE: usize = 4 * 1024 * 1024;

/// Where the running app listens.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Endpoint {
    pub port: u16,
    pub token: String,
    /// The app's process; a file left by a crash names one that's gone.
    pub pid: u32,
    /// The app's executable, for starting it when it isn't running.
    pub exe: PathBuf,
}

impl Endpoint {
    /// A fresh random token for an app listening on `port`.
    pub fn new(port: u16, exe: PathBuf) -> io::Result<Self> {
        let mut bytes = [0u8; 32];
        getrandom::fill(&mut bytes).map_err(|e| io::Error::other(e.to_string()))?;
        Ok(Self {
            port,
            token: bytes.iter().map(|b| format!("{b:02x}")).collect(),
            pid: std::process::id(),
            exe,
        })
    }

    /// Where the app writes the file: its local data folder, e.g.
    /// `%LOCALAPPDATA%\dev.sparkrs.desktop\agent-endpoint.json`.
    pub fn default_path() -> Option<PathBuf> {
        dirs::data_local_dir().map(|dir| dir.join(IDENTIFIER).join(ENDPOINT_FILE))
    }

    pub fn read(path: &Path) -> io::Result<Self> {
        serde_json::from_slice(&std::fs::read(path)?)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
    }

    /// Writes a temporary file and renames it over the old one, so a reader
    /// never sees half a file.
    pub fn write(&self, path: &Path) -> io::Result<()> {
        let temp = path.with_extension("json.tmp");
        std::fs::write(&temp, serde_json::to_vec_pretty(self)?)?;
        std::fs::rename(&temp, path)
    }
}

#[derive(Debug, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub token: String,
    /// Who's asking, e.g. "claude-code"; the app shows it to the user.
    #[serde(default)]
    pub client: String,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

/// `{"result": …}` or `{"error": "…"}`.
#[derive(Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Response {
    Result(Value),
    Error(String),
}

#[derive(Debug)]
pub enum CallError {
    /// Nothing answered: the app isn't running.
    Unreachable(io::Error),
    /// The connection broke or the reply made no sense.
    Broken(String),
    /// The app answered with an error, in words for the user.
    Refused(String),
}

impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unreachable(e) => write!(f, "spark-rs isn't answering: {e}"),
            Self::Broken(e) => write!(f, "the connection to spark-rs broke: {e}"),
            Self::Refused(e) => f.write_str(e),
        }
    }
}

impl std::error::Error for CallError {}

/// Sends one request to the app and waits for its answer.
pub async fn call(
    endpoint: &Endpoint,
    client: &str,
    method: &str,
    params: Value,
) -> Result<Value, CallError> {
    let stream = TcpStream::connect((Ipv4Addr::LOCALHOST, endpoint.port))
        .await
        .map_err(CallError::Unreachable)?;
    let request = Request {
        token: endpoint.token.clone(),
        client: client.to_owned(),
        method: method.to_owned(),
        params,
    };
    let broken = |e: io::Error| CallError::Broken(e.to_string());
    let mut stream = BufReader::new(stream);
    write_line(stream.get_mut(), &request)
        .await
        .map_err(broken)?;
    let line = read_line(&mut stream)
        .await
        .map_err(broken)?
        .ok_or_else(|| CallError::Broken("spark-rs closed the connection".into()))?;
    match serde_json::from_slice(&line).map_err(|e| CallError::Broken(e.to_string()))? {
        Response::Result(value) => Ok(value),
        Response::Error(message) => Err(CallError::Refused(message)),
    }
}

/// Answers requests on `listener` until the task is dropped. Requests
/// without the token are refused before `handle` sees them.
pub async fn serve<F, Fut>(listener: TcpListener, token: String, handle: F)
where
    F: Fn(Request) -> Fut + Clone + Send + 'static,
    Fut: Future<Output = Response> + Send,
{
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            continue;
        };
        let token = token.clone();
        let handle = handle.clone();
        tokio::spawn(async move {
            let mut stream = BufReader::new(stream);
            let response = match read_line(&mut stream).await {
                Ok(Some(line)) => match serde_json::from_slice::<Request>(&line) {
                    Ok(request) if token_matches(&token, &request.token) => handle(request).await,
                    Ok(_) => Response::Error("Not authorized.".into()),
                    Err(e) => Response::Error(format!("Unreadable request: {e}")),
                },
                Ok(None) => return,
                Err(e) => Response::Error(format!("Unreadable request: {e}")),
            };
            let _ = write_line(stream.get_mut(), &response).await;
        });
    }
}

async fn write_line<W: AsyncWrite + Unpin>(
    writer: &mut W,
    value: &impl Serialize,
) -> io::Result<()> {
    let mut line = serde_json::to_vec(value)?;
    line.push(b'\n');
    writer.write_all(&line).await?;
    writer.flush().await
}

/// One line without its newline; `None` at the end of the stream.
async fn read_line<R: AsyncBufRead + Unpin>(reader: &mut R) -> io::Result<Option<Vec<u8>>> {
    let mut line = Vec::new();
    reader
        .take(MAX_LINE as u64 + 1)
        .read_until(b'\n', &mut line)
        .await?;
    if line.is_empty() {
        return Ok(None);
    }
    if line.len() > MAX_LINE {
        return Err(io::Error::new(io::ErrorKind::InvalidData, "line too long"));
    }
    if line.last() == Some(&b'\n') {
        line.pop();
    }
    Ok(Some(line))
}

/// Compares in constant time, so timing doesn't reveal the token.
fn token_matches(expected: &str, given: &str) -> bool {
    expected.len() == given.len()
        && expected
            .bytes()
            .zip(given.bytes())
            .fold(0u8, |diff, (a, b)| diff | (a ^ b))
            == 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    async fn echo_server() -> Endpoint {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let endpoint =
            Endpoint::new(listener.local_addr().unwrap().port(), "app.exe".into()).unwrap();
        tokio::spawn(serve(
            listener,
            endpoint.token.clone(),
            |request: Request| async move {
                match request.method.as_str() {
                    "echo" => Response::Result(
                        json!({ "client": request.client, "params": request.params }),
                    ),
                    other => Response::Error(format!("no method {other}")),
                }
            },
        ));
        endpoint
    }

    #[tokio::test]
    async fn round_trips_a_request() {
        let endpoint = echo_server().await;
        let result = call(&endpoint, "codex", "echo", json!({ "n": 1 }))
            .await
            .unwrap();
        assert_eq!(result, json!({ "client": "codex", "params": { "n": 1 } }));
        match call(&endpoint, "codex", "nope", Value::Null).await {
            Err(CallError::Refused(message)) => assert_eq!(message, "no method nope"),
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn refuses_a_wrong_token() {
        let mut endpoint = echo_server().await;
        endpoint.token = "0".repeat(64);
        match call(&endpoint, "codex", "echo", Value::Null).await {
            Err(CallError::Refused(message)) => assert_eq!(message, "Not authorized."),
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn nothing_listening_is_unreachable() {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let endpoint = Endpoint::new(port, "app.exe".into()).unwrap();
        assert!(matches!(
            call(&endpoint, "codex", "echo", Value::Null).await,
            Err(CallError::Unreachable(_))
        ));
    }

    #[test]
    fn endpoint_file_round_trips() {
        let dir = std::env::temp_dir().join(format!("spark-agent-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(ENDPOINT_FILE);
        let endpoint = Endpoint::new(4242, "C:\\spark-rs\\spark-desktop.exe".into()).unwrap();
        assert_eq!(endpoint.token.len(), 64);
        endpoint.write(&path).unwrap();
        assert_eq!(Endpoint::read(&path).unwrap(), endpoint);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn tokens_must_match_exactly() {
        assert!(token_matches("abc", "abc"));
        assert!(!token_matches("abc", "abd"));
        assert!(!token_matches("abc", "abcd"));
    }
}
