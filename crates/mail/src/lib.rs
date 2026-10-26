//! Mail backends for spark-rs: IMAP/SMTP (Gmail with an app password, or any
//! server) and Microsoft Graph (Microsoft 365 with OAuth), feeding the same
//! store. `engine` ties them together for the apps.

pub mod attachment;
pub mod calendar;
pub mod draft;
pub mod drafts;
pub mod engine;
pub mod graph;
pub mod imap;
pub mod invite;
pub mod oauth;
pub mod parse;
pub mod search;
pub mod signature;
pub mod smtp;
pub mod tls;
pub mod unsubscribe;
pub mod utf7;

use std::fmt::Display;

use serde::{Deserialize, Serialize};

pub use engine::{AccountConfig, CalendarStatus, Engine, Round, Secrets};

/// How many messages further back the inbox or a folder reaches each time
/// the user scrolls to the end of its list, as in Spark.
pub const OLDER_STEP: usize = 100;

/// The folder mail from blocked senders goes to, named as Spark names it.
pub const BLOCKED: &str = "Blocked";

/// Errors are phrased for the user; they never contain passwords or tokens.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The server rejected the sign-in.
    #[error("{0}")]
    Auth(String),
    #[error("Couldn't reach {host}: {detail}")]
    Network { host: String, detail: String },
    #[error("{0}")]
    Server(String),
    #[error("Local storage failed: {0}")]
    Store(#[from] spark_store::Error),
    /// Bad input or settings, e.g. an invalid address.
    #[error("{0}")]
    Config(String),
    #[error("The message no longer exists on the server.")]
    Gone,
    /// The server's certificate isn't trusted; the user may trust it anyway
    /// (`Engine::trust_certificate`).
    #[error("The security certificate of {} isn't trusted: {}.", .0.host, .0.reason)]
    Certificate(Box<Untrusted>),
}

/// One of an IMAP account's two servers.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Service {
    Imap,
    Smtp,
}

/// A server certificate the system doesn't trust.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Untrusted {
    pub service: Service,
    pub host: String,
    /// The certificate's SHA-256 (`tls::fingerprint`), which trusting it records.
    pub fingerprint: String,
    /// Why the system refused it, e.g. "it has expired".
    pub reason: String,
}

pub type Result<T, E = Error> = std::result::Result<T, E>;

impl Error {
    pub(crate) fn network(host: &str, detail: impl Display) -> Self {
        Self::Network {
            host: host.to_owned(),
            detail: detail.to_string(),
        }
    }
}

impl From<async_imap::error::Error> for Error {
    fn from(e: async_imap::error::Error) -> Self {
        use async_imap::error::Error as E;
        match e {
            E::No(text) | E::Bad(text) => Self::Server(format!("The mail server said: {text}")),
            E::ConnectionLost => Self::Server("The mail server closed the connection.".into()),
            E::Io(e) => Self::Server(format!("Connection problem: {e}")),
            other => Self::Server(format!(
                "The mail server sent something unexpected ({other})."
            )),
        }
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Self::Server(format!("Connection problem: {e}"))
    }
}
