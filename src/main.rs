use acme_dns::{config::Config, router};
use std::{env, fs};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = env::var("CLIENT_CONFIG_FILE").unwrap_or_else(|_| "/config/clients.json".into());
    let config = Config::parse(&fs::read_to_string(path)?)?;
    let token = env::var("CF_DNS_API_TOKEN")?;
    if token.trim().is_empty() {
        return Err("CF_DNS_API_TOKEN is empty".into());
    }
    let listen = env::var("LISTEN_ADDR").unwrap_or_else(|_| "0.0.0.0:8080".into());
    let listener = tokio::net::TcpListener::bind(&listen).await?;
    eprintln!("event=listening address={listen}");
    axum::serve(listener, router(config, token)?)
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
