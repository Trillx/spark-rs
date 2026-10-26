//! Sending over SMTP with a password (a Gmail app password, or the account's own).
//!
//! A server whose certificate the user chose to trust (see `tls`) is sent
//! to over a connection of our own: TLS without the issuer check, then the
//! certificate is compared with the trusted one before signing in.

use std::time::Duration;

use lettre::transport::smtp::authentication::{Credentials, Mechanism};
use lettre::transport::smtp::client::{AsyncSmtpConnection, TlsParameters};
use lettre::transport::smtp::extension::ClientId;
use lettre::{AsyncSmtpTransport, AsyncTransport, Tokio1Executor};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;

use crate::imap::{Security, Server};
use crate::{Error, Result, Service, tls};

const TIMEOUT: Duration = Duration::from_secs(60);
/// How long checking a server's certificate may take. Adding an account
/// waits for it, and a server that doesn't answer is left for sending.
const CHECK_TIMEOUT: Duration = Duration::from_secs(10);

pub async fn send(
    server: &Server,
    username: &str,
    password: &str,
    message: lettre::Message,
) -> Result<()> {
    if let Some(trusted) = &server.trusted_cert
        && server.security != Security::Plain
    {
        return send_trusting(server, trusted, username, password, message).await;
    }
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
        .timeout(Some(TIMEOUT))
        .build();
    let Err(e) = transport.send(message).await else {
        return Ok(());
    };
    // lettre reports a refused certificate as a connection failure: find
    // out whether it was the certificate, and which one.
    if !e.is_permanent() && !e.is_transient() {
        check_certificate(server).await?;
    }
    Err(refused(server, e))
}

fn refused(server: &Server, e: lettre::transport::smtp::Error) -> Error {
    if e.is_permanent() && e.to_string().contains("authentication") {
        Error::Auth("The mail server didn't accept the password for sending.".into())
    } else if e.is_permanent() {
        Error::Server(format!("The mail server refused the message: {e}"))
    } else {
        Error::network(&server.host, e)
    }
}

/// Sends where the user trusts the server's certificate by its fingerprint:
/// a certificate the system trusts will do too, but no other.
async fn send_trusting(
    server: &Server,
    trusted: &str,
    username: &str,
    password: &str,
    message: lettre::Message,
) -> Result<()> {
    let network = |e: lettre::transport::smtp::Error| Error::network(&server.host, e);
    // The handshake's signatures are still checked; who issued the
    // certificate, and for which name, is checked below, before signing in.
    let parameters = TlsParameters::builder(server.host.clone())
        .dangerous_accept_invalid_certs(true)
        .dangerous_accept_invalid_hostnames(true)
        .build_rustls()
        .map_err(network)?;
    let hello = ClientId::default();
    let implicit = (server.security == Security::Tls).then(|| parameters.clone());
    let mut connection = AsyncSmtpConnection::connect_tokio1(
        (server.host.as_str(), server.port),
        Some(TIMEOUT),
        &hello,
        implicit,
        None,
    )
    .await
    .map_err(network)?;
    if server.security == Security::StartTls {
        connection
            .starttls(parameters, &hello)
            .await
            .map_err(network)?;
    }
    let shown = tls::fingerprint(&connection.peer_certificate().map_err(network)?);
    let chain = connection.certificate_chain().map_err(network)?;
    if shown != trusted && !tls::system_trusts(&server.host, &chain)? {
        connection.abort().await;
        return Err(tls::untrusted(
            Service::Smtp,
            &server.host,
            shown,
            "it isn't the certificate you chose to trust".into(),
        ));
    }
    let sent = async {
        connection
            .auth(
                &[Mechanism::Plain, Mechanism::Login],
                &Credentials::new(username.to_owned(), password.to_owned()),
            )
            .await?;
        connection
            .send(message.envelope(), &message.formatted())
            .await
    }
    .await;
    match sent {
        Ok(_) => {
            let _ = connection.quit().await;
            Ok(())
        }
        Err(e) => {
            connection.abort().await;
            Err(refused(server, e))
        }
    }
}

/// Starts TLS with the server as sending does, only to see its
/// certificate: `Error::Certificate` if it's one the system doesn't trust
/// and the user hasn't trusted. Other failures are left for sending to
/// report, so they come back `Ok`.
pub async fn check_certificate(server: &Server) -> Result<()> {
    if server.security == Security::Plain {
        return Ok(());
    }
    let checked = timeout(CHECK_TIMEOUT, async {
        let mut tcp = TcpStream::connect((server.host.as_str(), server.port)).await?;
        if server.security == Security::StartTls {
            start_tls(&mut tcp).await?;
        }
        let trusted = server.trusted_cert.as_deref();
        tls::connect(Service::Smtp, &server.host, tcp, trusted).await?;
        Ok::<_, Error>(())
    })
    .await;
    match checked {
        Ok(Err(e @ Error::Certificate(_))) => Err(e),
        _ => Ok(()),
    }
}

/// Asks a plain connection to switch to TLS: greeting, EHLO, STARTTLS.
async fn start_tls(tcp: &mut TcpStream) -> Result<()> {
    expect(tcp, 220).await?;
    tcp.write_all(b"EHLO spark-rs\r\n").await?;
    expect(tcp, 250).await?;
    tcp.write_all(b"STARTTLS\r\n").await?;
    expect(tcp, 220).await
}

/// Reads one reply, which may run over several lines ("250-…" until
/// "250 …"), and checks its code.
async fn expect(tcp: &mut TcpStream, code: u16) -> Result<()> {
    let mut line = Vec::new();
    loop {
        let mut byte = [0u8];
        if tcp.read(&mut byte).await? == 0 {
            return Err(Error::Server(
                "The mail server closed the connection.".into(),
            ));
        }
        line.push(byte[0]);
        if byte[0] == b'\n' {
            if line.get(3) != Some(&b'-') {
                let got = std::str::from_utf8(line.get(..3).unwrap_or_default()).unwrap_or("");
                return if got.parse() == Ok(code) {
                    Ok(())
                } else {
                    Err(Error::Server(format!(
                        "The mail server said: {}",
                        String::from_utf8_lossy(&line).trim()
                    )))
                };
            }
            line.clear();
        } else if line.len() > 4096 {
            return Err(Error::Server(
                "The mail server's reply was too long.".into(),
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::TcpListener;

    #[tokio::test]
    async fn replies_are_read_to_their_last_line() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut peer, _) = listener.accept().await.unwrap();
            peer.write_all(b"250-mail.example\r\n250-STARTTLS\r\n250 OK\r\n554 No thanks\r\n")
                .await
                .unwrap();
        });
        let mut tcp = TcpStream::connect(address).await.unwrap();
        expect(&mut tcp, 250).await.unwrap();
        let refused = expect(&mut tcp, 220).await.unwrap_err();
        assert!(refused.to_string().contains("554 No thanks"), "{refused}");
        server.await.unwrap();
    }
}
