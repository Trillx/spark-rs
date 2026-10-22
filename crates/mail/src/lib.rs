//! Mail backends for spark-rs: IMAP/SMTP (Gmail with an app password, or any
//! server) and Microsoft Graph (Microsoft 365 with OAuth), feeding the same
//! store. `engine` ties them together for the apps.

pub mod draft;
pub mod engine;
pub mod graph;
pub mod imap;
pub mod oauth;
pub mod parse;
pub mod smtp;

use std::fmt::Display;

pub use engine::{AccountConfig, Engine, Secrets};

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
