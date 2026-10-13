use acme_dns::{client, config::Config, router};
use std::{env, fs};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().skip(1).collect();
    let app = match args.as_slice() {
        [] => {
            let path =
                env::var("CLIENT_CONFIG_FILE").unwrap_or_else(|_| "/config/clients.json".into());
            let config = Config::parse(&fs::read_to_string(path)?)?;
            let token = env::var("CF_DNS_API_TOKEN")?;
            if token.trim().is_empty() {
                return Err("CF_DNS_API_TOKEN is empty".into());
            }
            router(config, token)?
        }
        [mode] if mode == "--client-mode" => client::router(&env::var("ACME_DNS_SERVER_URL")?)?,
        [help] if help == "--help" || help == "-h" => {
            println!(
                "Usage: acme-dns [--client-mode]\n\nDefault: serve the Cloudflare API using CLIENT_CONFIG_FILE and CF_DNS_API_TOKEN.\nClient mode: forward ACMEDNS updates to ACME_DNS_SERVER_URL.\nBoth modes: LISTEN_ADDR defaults to 0.0.0.0:8080."
            );
            return Ok(());
        }
        _ => return Err("Usage: acme-dns [--client-mode]".into()),
    };
    let listen = env::var("LISTEN_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".into());
    let listener = tokio::net::TcpListener::bind(&listen).await?;
    eprintln!("event=listening address={listen}");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown())
        .await?;
    Ok(())
}

async fn shutdown() {
    #[cfg(unix)]
    {
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .expect("Cannot register SIGTERM handler");
        tokio::select! {
            result = tokio::signal::ctrl_c() => result.expect("Cannot listen for SIGINT"),
            _ = terminate.recv() => {},
        }
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c()
        .await
        .expect("Cannot listen for shutdown");
}
