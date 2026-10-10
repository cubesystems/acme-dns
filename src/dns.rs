use crate::config::hex;
use reqwest::{Client, Method};
use serde::Deserialize;
use serde_json::{Value, json};
use std::time::Duration;

#[derive(Clone)]
pub struct Dns {
    client: Client,
    base: String,
    token: String,
}

#[derive(Debug, PartialEq)]
pub enum DnsError {
    Upstream,
    InvalidResponse,
    MultipleRecords,
}

#[derive(Deserialize)]
struct Envelope<T> {
    success: bool,
    result: T,
    result_info: Option<Page>,
}

#[derive(Deserialize)]
struct Page {
    page: u32,
    total_pages: u32,
}

#[derive(Deserialize)]
struct Record {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    name: String,
    content: String,
}

impl Dns {
    pub fn new(zone_id: &str, token: String) -> Result<Self, reqwest::Error> {
        Self::with_base(
            format!("https://api.cloudflare.com/client/v4/zones/{zone_id}/dns_records"),
            token,
        )
    }

    // Only unit tests substitute a local mock. The production URL is fixed above.
    pub(crate) fn with_base(base: String, token: String) -> Result<Self, reqwest::Error> {
        Ok(Self {
            client: Client::builder()
                .timeout(Duration::from_secs(10))
                .connect_timeout(Duration::from_secs(3))
                .redirect(reqwest::redirect::Policy::none())
                .retry(reqwest::retry::never())
                .build()?,
            base,
            token,
        })
    }

    pub async fn upsert(&self, name: &str, value: &str) -> Result<(), DnsError> {
        let mut existing: Option<Record> = None;
        let mut page = 1;
        loop {
            let query = [
                ("type", "TXT".to_owned()),
                ("name.exact", name.to_owned()),
                ("page", page.to_string()),
                ("per_page", "100".to_owned()),
            ];
            let body: Envelope<Vec<Record>> = self
                .decode(self.client.get(&self.base).query(&query))
                .await?;
            let info = body.result_info.ok_or(DnsError::InvalidResponse)?;
            if info.page != page
                || (info.total_pages < page
                    && !(page == 1 && info.total_pages == 0 && body.result.is_empty()))
            {
                return Err(DnsError::InvalidResponse);
            }
            for record in body.result {
                // Authorization does not depend on Cloudflare applying the filters.
                if record.kind != "TXT" || record.name != name {
                    continue;
                }
                if !hex(&record.id, 32) {
                    return Err(DnsError::InvalidResponse);
                }
                if existing.as_ref().is_some_and(|old| old.id != record.id) {
                    return Err(DnsError::MultipleRecords);
                }
                existing = Some(record);
            }
            if page >= info.total_pages {
                break;
            }
            page = page.checked_add(1).ok_or(DnsError::InvalidResponse)?;
        }

        let (method, url) = match existing {
            Some(record) if matches(&record.content, value) => return Ok(()),
            Some(record) => (Method::PUT, format!("{}/{}", self.base, record.id)),
            None => (Method::POST, self.base.clone()),
        };
        let body = json!({"type":"TXT", "name":name, "content":value, "ttl":60});
        let response: Envelope<Record> = self
            .decode(self.client.request(method, url).json(&body))
            .await?;
        let record = response.result;
        if !hex(&record.id, 32)
            || record.kind != "TXT"
            || record.name != name
            || !matches(&record.content, value)
        {
            return Err(DnsError::InvalidResponse);
        }
        Ok(())
    }

    async fn decode<T: serde::de::DeserializeOwned>(
        &self,
        request: reqwest::RequestBuilder,
    ) -> Result<Envelope<T>, DnsError> {
        let response = request
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|_| DnsError::Upstream)?;
        if !response.status().is_success() {
            return Err(DnsError::Upstream);
        }
        // Never put upstream response bodies, headers, or error strings in logs.
        let value: Value = response
            .json()
            .await
            .map_err(|_| DnsError::InvalidResponse)?;
        let body: Envelope<T> =
            serde_json::from_value(value).map_err(|_| DnsError::InvalidResponse)?;
        if !body.success {
            return Err(DnsError::Upstream);
        }
        Ok(body)
    }
}

fn matches(content: &str, value: &str) -> bool {
    content == value || content == format!("\"{value}\"")
}
