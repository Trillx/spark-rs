mod agent;
mod commands;

use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use rustls::pki_types::ServerName;
use spark_ai::{AiClient, ChatRequest, Message, OPENAI_BASE_URL, ProviderConfig, StopReason};
use spark_store::Store;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::net::TcpStream;
use tokio::time::timeout;

use commands::Mail;

#[derive(Parser)]
#[command(
    name = "spark-cli",
    version,
    about = "spark-rs for AI agents: read, triage and send your mail in the running spark-rs, \
             within the access you give each account in Settings > Agents.",
    after_help = "Agents: run `spark-cli skill` for how to use it, or `spark-cli mcp` as an MCP server."
)]
struct Cli {
    /// Print the result as JSON instead of text. A failure prints
    /// {"error": {"message": …}} and exits with 1.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    #[command(flatten)]
    Mail(Box<Mail>),
    /// Print the skill that teaches agents spark-cli, or another skill
    /// (`--list` shows them), or install them as <DIR>/<name>/SKILL.md.
    Skill {
        /// A skill's name, e.g. inbox-triage; use-spark-rs if omitted.
        name: Option<String>,
        /// Install into this folder (an agent's skills folder) instead of printing.
        #[arg(long, value_name = "DIR")]
        install: Option<PathBuf>,
        /// Every skill, not one.
        #[arg(long)]
        all: bool,
        /// List the skills and what each is for.
        #[arg(long, conflicts_with_all = ["name", "install", "all"])]
        list: bool,
    },
    /// Serve spark-rs's commands as MCP tools on stdin/stdout, for agents
    /// such as Claude Code and Codex. Starts spark-rs if it isn't running.
    Mcp,
    /// Print the MCP tool definitions as JSON.
    Tools,
    /// Run one MCP tool and print its result, e.g.
    /// `spark-cli call emails '{"filter": "is:unread"}'`.
    Call {
        tool: String,
        /// The tool's arguments as a JSON object.
        arguments: Option<String>,
    },
    /// Check that agents can reach spark-rs, and what they may do there.
    Status,
    /// Check that storage, TLS and networking work on this machine.
    Doctor {
        /// Database file to open; defaults to an in-memory database.
        #[arg(long)]
        db: Option<PathBuf>,
        /// IMAP server to greet over implicit TLS.
        #[arg(long, default_value = "imap.gmail.com")]
        imap_host: String,
        #[arg(long, default_value_t = 993)]
        imap_port: u16,
    },
    /// Ask your AI provider something, streaming the answer.
    ///
    /// The key comes from SPARK_AI_API_KEY, else ANTHROPIC_API_KEY or
    /// OPENAI_API_KEY to match the provider. Local servers need none.
    Ai {
        #[arg(long, value_enum, default_value_t = AiProvider::Anthropic)]
        provider: AiProvider,
        /// Override the API base URL, e.g. http://localhost:11434/v1 for Ollama.
        #[arg(long)]
        base_url: Option<String>,
        /// Model id; required for OpenAI-compatible providers.
        #[arg(long)]
        model: Option<String>,
        /// List the provider's models instead of asking.
        #[arg(long)]
        list_models: bool,
        prompt: Option<String>,
    },
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum AiProvider {
    Anthropic,
    Openai,
}

fn main() {
    let cli = match Cli::try_parse() {
        Ok(cli) => cli,
        Err(e) => {
            use clap::error::ErrorKind;
            let shown = matches!(
                e.kind(),
                ErrorKind::DisplayHelp
                    | ErrorKind::DisplayVersion
                    | ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
            );
            // With --json, a mistyped command answers in JSON too.
            if !shown && std::env::args_os().any(|a| a == "--json") {
                let message = e.to_string();
                println!(
                    "{}",
                    serde_json::json!({ "error": { "message": message.trim() } })
                );
                std::process::exit(1);
            }
            e.exit();
        }
    };
    let json = cli.json;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime");
    let result = runtime.block_on(run(cli));
    if let Err(e) = result {
        if json {
            println!(
                "{}",
                serde_json::json!({ "error": { "message": format!("{e:#}") } })
            );
        } else {
            eprintln!("Error: {e:#}");
        }
        std::process::exit(1);
    }
}

async fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Mail(mail) => commands::run(*mail, cli.json).await,
        Command::Skill {
            name,
            install,
            all,
            list,
        } => commands::skill(name, install, all, list),
        Command::Mcp => agent::mcp().await,
        Command::Tools => agent::tools().await,
        Command::Call { tool, arguments } => agent::call_tool(&tool, arguments.as_deref()).await,
        Command::Status => agent::status().await,
        Command::Doctor {
            db,
            imap_host,
            imap_port,
        } => doctor(db, &imap_host, imap_port).await,
        Command::Ai {
            provider,
            base_url,
            model,
            list_models,
            prompt,
        } => ai(provider, base_url, model, list_models, prompt).await,
    }
}

async fn ai(
    provider: AiProvider,
    base_url: Option<String>,
    model: Option<String>,
    list_models: bool,
    prompt: Option<String>,
) -> Result<()> {
    let (mut config, key_var) = match provider {
        AiProvider::Anthropic => (ProviderConfig::anthropic(), "ANTHROPIC_API_KEY"),
        AiProvider::Openai => (
            ProviderConfig::openai_compatible(OPENAI_BASE_URL, model.clone().unwrap_or_default()),
            "OPENAI_API_KEY",
        ),
    };
    if let Some(url) = base_url {
        config.base_url = url;
    }
    if let Some(model) = model {
        config.model = model;
    }
    let key = std::env::var("SPARK_AI_API_KEY")
        .or_else(|_| std::env::var(key_var))
        .ok();
    let client = AiClient::new(config, key)?;

    if list_models {
        for id in client.list_models().await? {
            println!("{id}");
        }
        return Ok(());
    }
    let prompt = prompt.context("give a prompt, or --list-models")?;
    if client.config().model.is_empty() {
        anyhow::bail!("--model is required for OpenAI-compatible providers");
    }
    let request = ChatRequest {
        system: None,
        messages: vec![Message::user(prompt)],
        max_tokens: 16_000,
    };
    let done = client
        .complete(&request, |text| {
            print!("{text}");
            let _ = std::io::stdout().flush();
        })
        .await?;
    println!();
    if let StopReason::Refusal { category } = &done.stop_reason {
        eprintln!(
            "(declined{}; ignore the partial answer above)",
            category
                .as_deref()
                .map(|c| format!(": {c}"))
                .unwrap_or_default()
        );
    }
    eprintln!(
        "[{} · {} in / {} out tokens · {:?}]",
        done.model.as_deref().unwrap_or("?"),
        done.usage.input_tokens,
        done.usage.output_tokens,
        done.stop_reason
    );
    Ok(())
}

async fn doctor(db: Option<PathBuf>, imap_host: &str, imap_port: u16) -> Result<()> {
    println!(
        "spark-cli {} on {}/{}",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH
    );

    let store = match &db {
        Some(path) => Store::open(path).with_context(|| format!("opening {}", path.display()))?,
        None => Store::open_in_memory()?,
    };
    let location = db
        .as_deref()
        .map_or("in-memory".into(), |p| p.display().to_string());
    println!(
        "sqlite   {} ({location}), fts5: {}",
        Store::sqlite_version(),
        if store.has_fts5()? { "yes" } else { "NO" }
    );

    let (tls, greeting) = timeout(Duration::from_secs(15), imap_greeting(imap_host, imap_port))
        .await
        .context("timed out talking to the IMAP server")??;
    println!("tls      {imap_host}:{imap_port} {tls}");
    println!("imap     {}", greeting.trim_end());
    Ok(())
}

/// Connects with implicit TLS and returns the negotiated parameters and the server greeting.
async fn imap_greeting(host: &str, port: u16) -> Result<(String, String)> {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()?
    .with_root_certificates(roots)
    .with_no_client_auth();

    let tcp = TcpStream::connect((host, port))
        .await
        .with_context(|| format!("connecting to {host}:{port}"))?;
    let server_name = ServerName::try_from(host.to_owned())?;
    let stream = tokio_rustls::TlsConnector::from(Arc::new(config))
        .connect(server_name, tcp)
        .await
        .context("TLS handshake")?;

    let (_, session) = stream.get_ref();
    let tls = format!(
        "{:?} {:?}",
        session.protocol_version().context("no TLS version")?,
        session
            .negotiated_cipher_suite()
            .context("no cipher suite")?
            .suite()
    );

    let mut greeting = String::new();
    BufReader::new(stream).read_line(&mut greeting).await?;
    Ok((tls, greeting))
}
