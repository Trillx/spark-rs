//! Microsoft sign-in: OAuth 2.0 authorization code with PKCE, in the system
//! browser, with the redirect caught on a localhost port. This is what
//! Microsoft recommends for desktop apps, which are public clients with no
//! secret. The app registration needs the "Mobile and desktop applications"
//! platform with redirect URI `http://localhost` (any port matches).

use std::net::{Ipv4Addr, Ipv6Addr};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::time::timeout;

use crate::{Error, Result};

/// Delegated permissions: read and change mail, send mail, read the profile
/// (for the address), and a refresh token.
pub const SCOPES: &str = "offline_access https://graph.microsoft.com/User.Read \
     https://graph.microsoft.com/Mail.ReadWrite https://graph.microsoft.com/Mail.Send";

/// Read the calendar and answer its invitations. Sign-ins ask for it; tokens
/// are refreshed with it only for accounts that signed in since it was added,
/// so older sign-ins keep working for mail.
pub const CALENDAR_SCOPE: &str = "https://graph.microsoft.com/Calendars.ReadWrite";

fn scopes(calendar: bool) -> String {
    if calendar {
        format!("{SCOPES} {CALENDAR_SCOPE}")
    } else {
        SCOPES.to_owned()
    }
}

const SIGN_IN_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// An app registration in Microsoft Entra.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct App {
    pub client_id: String,
    /// Tenant ID or domain, e.g. `contoso.onmicrosoft.com`.
    pub tenant: String,
}

impl App {
    pub fn new(client_id: &str, tenant: &str) -> Result<Self> {
        let client_id = client_id.trim();
        let tenant = tenant.trim();
        if client_id.is_empty()
            || !client_id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-')
        {
            return Err(Error::Config(
                "Enter the application (client) ID from the app registration.".into(),
            ));
        }
        if tenant.is_empty()
            || !tenant
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.')
        {
            return Err(Error::Config(
                "Enter the directory (tenant) ID or your organization's domain.".into(),
            ));
        }
        Ok(Self {
            client_id: client_id.to_owned(),
            tenant: tenant.to_owned(),
        })
    }

    fn endpoint(&self, path: &str) -> String {
        format!(
            "https://login.microsoftonline.com/{}/oauth2/v2.0/{path}",
            self.tenant
        )
    }
}

#[derive(Clone, Debug)]
pub struct Tokens {
    pub access_token: String,
    /// Microsoft may rotate it; store the new one when present.
    pub refresh_token: Option<String>,
    /// Unix ms.
    pub expires_at: i64,
}

/// Opens Microsoft's sign-in page with `open_browser` and waits (up to 15
/// minutes) for the browser to come back with a code. `login_hint` suggests
/// the account to sign in as. Dropping the future cancels it.
pub async fn sign_in(
    app: &App,
    login_hint: Option<&str>,
    open_browser: impl FnOnce(&str) -> Result<(), String>,
) -> Result<Tokens> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    let port = listener.local_addr()?.port();
    // Browsers may resolve localhost to ::1 first.
    let listener_v6 = TcpListener::bind((Ipv6Addr::LOCALHOST, port)).await.ok();
    let redirect_uri = format!("http://localhost:{port}");

    let verifier = random_token(32);
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let state = random_token(16);
    let scope = scopes(true);
    let mut url = url::Url::parse_with_params(
        &app.endpoint("authorize"),
        [
            ("client_id", app.client_id.as_str()),
            ("response_type", "code"),
            ("redirect_uri", &redirect_uri),
            ("response_mode", "query"),
            ("scope", &scope),
            ("state", &state),
            ("code_challenge", &challenge),
            ("code_challenge_method", "S256"),
            ("prompt", "select_account"),
        ],
    )
    .expect("the authorize URL is valid");
    if let Some(hint) = login_hint {
        url.query_pairs_mut().append_pair("login_hint", hint);
    }
    open_browser(url.as_str())
        .map_err(|e| Error::Config(format!("Couldn't open the browser: {e}")))?;

    let code = timeout(
        SIGN_IN_TIMEOUT,
        wait_for_code(&listener, listener_v6.as_ref(), &state),
    )
    .await
    .map_err(|_| Error::Auth("Sign-in timed out. Try again.".into()))??;
    token_request(
        app,
        &scope,
        &[
            ("grant_type", "authorization_code"),
            ("code", &code),
            ("redirect_uri", &redirect_uri),
            ("code_verifier", &verifier),
        ],
    )
    .await
}

/// Trades a refresh token for a new access token, with the calendar
/// permission if the account has granted it.
pub async fn refresh(app: &App, refresh_token: &str, calendar: bool) -> Result<Tokens> {
    token_request(
        app,
        &scopes(calendar),
        &[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
        ],
    )
    .await
}

async fn wait_for_code(v4: &TcpListener, v6: Option<&TcpListener>, state: &str) -> Result<String> {
    loop {
        let (stream, _) = match v6 {
            Some(v6) => tokio::select! {
                accepted = v4.accept() => accepted?,
                accepted = v6.accept() => accepted?,
            },
            None => v4.accept().await?,
        };
        if let Some(outcome) = handle_redirect(stream, state).await {
            return outcome;
        }
    }
}

/// Answers one browser request. `None` means it wasn't the redirect (a
/// favicon, a stray tab) and the wait goes on.
async fn handle_redirect(mut stream: TcpStream, state: &str) -> Option<Result<String>> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 2048];
    while !buf.windows(4).any(|w| w == b"\r\n\r\n") && buf.len() < 16 * 1024 {
        match timeout(Duration::from_secs(10), stream.read(&mut chunk)).await {
            Ok(Ok(n)) if n > 0 => buf.extend_from_slice(&chunk[..n]),
            _ => return None,
        }
    }
    let request = String::from_utf8_lossy(&buf);
    let target = request.split_whitespace().nth(1).unwrap_or("/");
    let outcome = redirect_outcome(target, state);
    let (status, page) = match &outcome {
        None => ("404 Not Found", page("Nothing here", "")),
        Some(Ok(_)) => (
            "200 OK",
            page(
                "You're signed in",
                "You can close this tab and go back to spark-rs.",
            ),
        ),
        Some(Err(e)) => ("200 OK", page("Sign-in didn't work", &e.to_string())),
    };
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{page}",
        page.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
    let _ = stream.shutdown().await;
    outcome
}

/// Reads the code (or Microsoft's error) from the redirect's request target.
fn redirect_outcome(target: &str, state: &str) -> Option<Result<String>> {
    let url = url::Url::parse(&format!("http://localhost{target}")).ok()?;
    if url.path() != "/" {
        return None;
    }
    let param = |name: &str| {
        url.query_pairs()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.into_owned())
    };
    if let Some(error) = param("error") {
        let detail = param("error_description").unwrap_or(error);
        return Some(Err(Error::Auth(first_line(&detail))));
    }
    let code = param("code")?;
    if param("state").as_deref() != Some(state) {
        return Some(Err(Error::Auth(
            "The sign-in response didn't match this request. Try again.".into(),
        )));
    }
    Some(Ok(code))
}

#[derive(Deserialize)]
struct TokenResponse {
    access_token: Option<String>,
    refresh_token: Option<String>,
    expires_in: Option<i64>,
    error: Option<String>,
    error_description: Option<String>,
}

async fn token_request(app: &App, scope: &str, grant: &[(&str, &str)]) -> Result<Tokens> {
    let mut form = vec![("client_id", app.client_id.as_str()), ("scope", scope)];
    form.extend_from_slice(grant);
    let response = crate::graph::http()
        .post(app.endpoint("token"))
        .form(&form)
        .send()
        .await
        .map_err(|e| Error::network("login.microsoftonline.com", e))?;
    let body: TokenResponse = response
        .json()
        .await
        .map_err(|e| Error::Server(format!("Microsoft sent an unexpected reply: {e}")))?;
    match body {
        TokenResponse {
            access_token: Some(access_token),
            refresh_token,
            expires_in,
            ..
        } => Ok(Tokens {
            access_token,
            refresh_token,
            expires_at: spark_store::now_ms() + expires_in.unwrap_or(3600) * 1000,
        }),
        TokenResponse {
            error: Some(error), ..
        } if error == "invalid_grant" || error == "interaction_required" => {
            Err(Error::Auth("Microsoft needs you to sign in again.".into()))
        }
        TokenResponse {
            error,
            error_description,
            ..
        } => Err(Error::Auth(first_line(
            &error_description
                .or(error)
                .unwrap_or_else(|| "Microsoft didn't return a token.".into()),
        ))),
    }
}

/// Microsoft's descriptions carry trace and correlation ids on later lines.
fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or_default().trim().to_owned()
}

fn random_token(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    getrandom::fill(&mut buf).expect("the OS provides randomness");
    URL_SAFE_NO_PAD.encode(buf)
}

fn page(title: &str, text: &str) -> String {
    let escape = |s: &str| {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    };
    format!(
        "<!doctype html><meta charset=utf-8><title>spark-rs</title>\
         <body style=\"font:16px system-ui;margin:20vh auto;max-width:30rem;text-align:center;color:#222\">\
         <h1 style=\"font-size:22px\">{}</h1><p>{}</p></body>",
        escape(title),
        escape(text)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_ids_are_checked() {
        assert!(
            App::new(
                " 1b2c3d4e-0000-1111-2222-333344445555 ",
                "contoso.onmicrosoft.com"
            )
            .is_ok()
        );
        assert!(App::new("", "contoso.com").is_err());
        assert!(App::new("abc", "contoso.com/../x").is_err());
        let app = App::new("abc", "contoso.com").unwrap();
        assert_eq!(
            app.endpoint("token"),
            "https://login.microsoftonline.com/contoso.com/oauth2/v2.0/token"
        );
    }

    #[test]
    fn the_redirect_yields_the_code_only_with_the_right_state() {
        assert_eq!(
            redirect_outcome("/?code=abc&state=s1", "s1")
                .unwrap()
                .unwrap(),
            "abc"
        );
        assert!(
            redirect_outcome("/?code=abc&state=other", "s1")
                .unwrap()
                .is_err()
        );
        assert!(redirect_outcome("/favicon.ico", "s1").is_none());
        assert!(redirect_outcome("/", "s1").is_none());
        let denied = redirect_outcome(
            "/?error=access_denied&error_description=AADSTS65004%3A+User+declined+to+consent.%0D%0ATrace+ID%3A+x",
            "s1",
        )
        .unwrap()
        .unwrap_err();
        assert_eq!(denied.to_string(), "AADSTS65004: User declined to consent.");
    }

    #[tokio::test]
    async fn sign_in_catches_the_browser_redirect() {
        let app = App::new("client", "contoso.com").unwrap();
        let (url_tx, url_rx) = tokio::sync::oneshot::channel();
        let sign_in = tokio::spawn(async move {
            sign_in(&app, Some("me@contoso.com"), |url| {
                url_tx.send(url.to_owned()).unwrap();
                Ok(())
            })
            .await
        });
        let url = url::Url::parse(&url_rx.await.unwrap()).unwrap();
        let param = |name: &str| {
            url.query_pairs()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.into_owned())
                .unwrap()
        };
        assert_eq!(param("code_challenge_method"), "S256");
        assert_eq!(param("client_id"), "client");
        assert_eq!(param("login_hint"), "me@contoso.com");
        assert!(
            param("scope").ends_with("Mail.Send https://graph.microsoft.com/Calendars.ReadWrite")
        );
        let redirect = url::Url::parse(&param("redirect_uri")).unwrap();
        assert_eq!(redirect.host_str(), Some("localhost"));

        // The "browser": a stray favicon request, then the redirect with an error.
        let port = redirect.port().unwrap();
        let get = |path: String| async move {
            let mut s = TcpStream::connect((Ipv4Addr::LOCALHOST, port))
                .await
                .unwrap();
            s.write_all(format!("GET {path} HTTP/1.1\r\nHost: localhost\r\n\r\n").as_bytes())
                .await
                .unwrap();
            let mut reply = String::new();
            s.read_to_string(&mut reply).await.unwrap();
            reply
        };
        assert!(get("/favicon.ico".into()).await.starts_with("HTTP/1.1 404"));
        let page = get(format!("/?error=access_denied&state={}", param("state"))).await;
        assert!(page.contains("Sign-in didn't work"), "{page}");
        let error = sign_in.await.unwrap().unwrap_err();
        assert_eq!(error.to_string(), "access_denied");
    }
}
