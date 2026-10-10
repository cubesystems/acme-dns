use serde::Deserialize;
use std::collections::HashSet;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    pub zone_id: String,
    pub zone_name: String,
    pub clients: Vec<ClientConfig>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClientConfig {
    pub id: String,
    pub record_name: String,
    pub token_sha256: Vec<String>,
}

impl Config {
    pub fn parse(json: &str) -> Result<Self, &'static str> {
        let config: Self = serde_json::from_str(json).map_err(|_| "Invalid configuration JSON")?;
        if !hex(&config.zone_id, 32) || !hostname(&config.zone_name) {
            return Err("Invalid zone ID or zone name");
        }
        let (mut ids, mut names, mut hashes) = (HashSet::new(), HashSet::new(), HashSet::new());
        for client in &config.clients {
            let host = client.record_name.strip_prefix("_acme-challenge.");
            if client.id.is_empty()
                || client.id.len() > 32
                || !client
                    .id
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
                || !ids.insert(&client.id)
                || !names.insert(&client.record_name)
                || client.record_name.len() > 253
                || !host.is_some_and(|h| {
                    hostname(h)
                        && (h == config.zone_name || h.ends_with(&format!(".{}", config.zone_name)))
                })
                || client.token_sha256.is_empty()
                || client.token_sha256.len() > 2
            {
                return Err("Invalid or duplicate client configuration");
            }
            for digest in &client.token_sha256 {
                if !hex(digest, 64) || !hashes.insert(digest) {
                    return Err("Invalid or duplicate token digest");
                }
            }
        }
        Ok(config)
    }
}

pub(crate) fn hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn hostname(name: &str) -> bool {
    name.len() <= 253
        && name.contains('.')
        && name.split('.').all(|part| {
            !part.is_empty()
                && part.len() <= 63
                && !part.starts_with('-')
                && !part.ends_with('-')
                && part
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
}
