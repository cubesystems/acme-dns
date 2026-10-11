# acme-dns

Servers behind firewalls still need automatic HTTPS certificates. DNS-01 validation allows this without inbound access, but giving every server a Cloudflare DNS token puts the whole zone at risk. `acme-dns` holds that token centrally and limits each client's API key to one challenge TXT record.

Certbot sends a value; the service creates or updates the TXT record. Certificates and private keys stay with Certbot. The last TXT value remains until the next challenge.

Run **one container and one validation at a time per DNS name**. The service serves HTTP on port 8080; terminate HTTPS at your ingress or proxy.

## Credentials

Create a Cloudflare token with **Zone / DNS / Edit** limited to your zone. Each Certbot server gets a separate client key. Generate one below, replacing `/secure/path` with a private directory outside this repository:

```sh
(umask 077; set -C; set -o pipefail; openssl rand -base64 32 | tr '+/' '-_' | tr -d '=\n' > /secure/path/site-a.token)
openssl dgst -sha256 /secure/path/site-a.token
```

Give the client the file's key. Put the printed SHA-256 hash in `tokenSha256` below.

## Docker

Copy [config.example.json](config.example.json) to `/secure/path/clients.json` and fill in your zone, record names and key hashes. Export `CF_DNS_API_TOKEN`, then run:

```sh
docker run -d --name acme-dns \
  --restart unless-stopped --read-only \
  --publish 80:8080 \
  --mount type=bind,src=/secure/path/clients.json,dst=/config/clients.json,readonly \
  --env CF_DNS_API_TOKEN \
  cubesystems/acme-dns:v1.0.0
```

Optional environment overrides: `CLIENT_CONFIG_FILE` (default `/config/clients.json`) and `LISTEN_ADDR` (default `0.0.0.0:8080`). For Cloudflare proxying to an HTTP origin, use [Flexible mode](https://developers.cloudflare.com/ssl/origin-configuration/ssl-modes/flexible/) for this hostname only.

## Helm

In namespace `dns`, create Secret `cloudflare-dns` with key `CF_DNS_API_TOKEN` containing the Cloudflare token. Save these values in `/secure/path/acme-dns-values.yaml`:

```yaml
image:
  tag: "v1.0.0"
cloudflare:
  existingSecret: cloudflare-dns
config:
  zoneId: "YOUR_32_CHARACTER_ZONE_ID"
  zoneName: example.com
  clients:
    - id: site-a
      recordName: _acme-challenge.site-a.example.com
      tokenSha256:
        - "YOUR_64_CHARACTER_SHA256_HASH"
```

```sh
helm repo add acme-dns https://cubesystems.github.io/acme-dns/
helm repo update
helm upgrade --install dns-api acme-dns/acme-dns \
  --namespace dns \
  --values /secure/path/acme-dns-values.yaml
```

Or install `./charts/acme-dns` from source. See [chart values](charts/acme-dns/values.yaml) for ingress and other options, and [chart notes](charts/acme-dns/README.md) for updates.

## Certbot and API

The Certbot authentication hook sends:

```http
POST /v1/challenges
Authorization: Bearer <client-api-key>
Content-Type: application/json

{"value":"<CERTBOT_VALIDATION>"}
```

Require `200 {"ok":true}`, then **wait for the value in public DNS**, failing on timeout. Omit the cleanup hook. Send `CERTBOT_VALIDATION` unchanged; the server chooses the record name. Body limit: 512 bytes, five seconds.

Challenge errors use `{"error":"<code>"}`. `409` means multiple TXT records exist at that name; remove extras before retrying. `502` means a Cloudflare API/network failure; `504` means the 15-second deadline expired. A timed-out write may have succeeded. Requests are not retried automatically; each new request checks DNS again.

`GET /healthz` checks the process, not Cloudflare access. A hostname's key also permits wildcard certificates beneath it, since both use the same challenge record.

Update saved Certbot renewal settings to use this hook. Verify staging issuance, scheduled renewal and certificate reloads.

## Updates

Stop the old container before starting its replacement; Helm handles this automatically. Configuration is read at startup. For key rotation, add the new hash, restart, switch the client, then remove the old hash and restart. Keep current credentials when reverting an image.

## Development

Requires Rust 1.95, a C compiler and CMake; chart tests need Helm 3 and PyYAML.

```sh
cargo test --locked
cargo clippy --locked --all-targets -- -D warnings
cargo fmt --all -- --check
docker build -t acme-dns:local .
python3 tests/container_smoke.py acme-dns:local
helm lint charts/acme-dns --strict -f charts/acme-dns/ci/test-values.yaml
python3 tests/helm_test.py
```

Tests use fake keys and Cloudflare responses. See [publishing](RELEASING.md) for images and charts.
