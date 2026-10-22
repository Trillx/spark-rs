//! Sending over SMTP with a password (a Gmail app password, or the account's own).

use std::time::Duration;

use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Tokio1Executor};

use crate::imap::{Security, Server};
use crate::{Error, Result};

pub async fn send(
    server: &Server,
    username: &str,
    password: &str,
    message: lettre::Message,
) -> Result<()> {
    let builder = match server.security {
        Security::Tls => AsyncSmtpTransport::<Tokio1Executor>::relay(&server.host),
        Security::StartTls => AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&server.host),
        Security::Plain => Ok(AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(
            &server.host,
        )),
    }
    .map_err(|e| Error::network(&server.host, e))?;
    let transport = builder
        .port(server.port)
        .credentials(Credentials::new(username.to_owned(), password.to_owned()))
        .timeout(Some(Duration::from_secs(60)))
        .build();
    transport.send(message).await.map_err(|e| {
        if e.is_permanent() && e.to_string().contains("authentication") {
            Error::Auth("The mail server didn't accept the password for sending.".into())
        } else if e.is_permanent() {
            Error::Server(format!("The mail server refused the message: {e}"))
        } else {
            Error::network(&server.host, e)
        }
    })?;
    Ok(())
}
