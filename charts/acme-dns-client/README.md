# acme-dns client Helm chart

Install once per Kubernetes cluster to let cert-manager use an existing acme-dns server. This chart runs the **same `cubesystems/acme-dns` image** as the server, with `--client-mode`. There is one binary, Dockerfile and image release process.

Client mode accepts the [ACMEDNS protocol](https://cert-manager.io/docs/configuration/acme/dns01/acme-dns/) used by cert-manager's built-in solver, and forwards updates to the existing server's `/v1/challenges` API. The Cloudflare key stays on that server. Each certificate name has a restricted client token; the server authenticates it and chooses the only TXT record it may change.

Requires Kubernetes 1.25+, Helm 3 and an installed cert-manager. Client mode is available starting with `v1.1.0`; the `v1.0.0` image does not support it. Select the image by tag or digest explicitly. No separate client image build is needed.

## Account credentials

Create a client on the existing server as described in [Credentials](../../README.md#credentials), with `recordName: _acme-challenge.site-a.example.com`. Save the following JSON privately with mode `0600` as `/secure/path/acmedns.json`, replacing the password with its original 43-character client key (not the SHA-256 hash):

```json
{
  "site-a.example.com": {
    "username": "site-a",
    "password": "YOUR_43_CHARACTER_CLIENT_KEY",
    "subdomain": "site-a",
    "fulldomain": "_acme-challenge.site-a.example.com",
    "allowfrom": []
  }
}
```

Add one entry per certificate DNS name. Use the server's client ID for both `username` and `subdomain`; client mode checks they match each other, while the server enforces authorization using the token. `fulldomain` is the actual challenge record. Neither `/register` nor a CNAME delegation is needed. `allowfrom` is an unused compatibility field here.

For a ClusterIssuer, cert-manager reads this Secret from its **cluster resource namespace**, normally `cert-manager`:

```sh
kubectl -n cert-manager create secret generic acme-dns-accounts \
  --from-file=acmedns.json=/secure/path/acmedns.json
```

Alternatively, this chart supports `secrets.<name>.data` using the same [k8s-tools encryption workflow](../acme-dns/README.md#encrypted-secrets-with-k8s-tools-2x) as the server chart. Install the chart in cert-manager's cluster resource namespace when it manages this Secret. Unresolved `HELM_SECRET:` values fail rendering. The client pod does not mount this Secret or need Kubernetes API permissions: cert-manager sends the selected account credentials with each request.

## Install

Save values for this cluster:

```yaml
fullnameOverride: acme-dns-client
image:
  tag: "v1.1.0"
serverUrl: https://acme.example.com
issuer:
  enabled: true
  name: letsencrypt-dns-staging
  email: admin@example.com
  server: https://acme-staging-v02.api.letsencrypt.org/directory
  dnsNames:
    - site-a.example.com
  accountSecretRef:
    name: acme-dns-accounts
    key: acmedns.json
```

Replace the server URL, email and DNS names. `serverUrl` is the existing service's HTTPS base URL; client mode appends `/v1/challenges` and refuses redirects. Then install from this repository:

```sh
helm upgrade --install acme-dns-client ./charts/acme-dns-client \
  --namespace cert-manager \
  --values /secure/path/acme-dns-client-values.yaml
```

After the chart is published to the existing Helm repository, the same command can use `acme-dns/acme-dns-client` instead of the local path. Other clusters reuse the same image and chart with their own values and account Secrets.

The chart creates a Deployment and an internal ClusterIP Service. Enabling `issuer` also creates the ClusterIssuer. Applications in any namespace can reference it. Request a staging Certificate with a separate test TLS Secret; after successful issuance, configure a production issuer with `https://acme-v02.api.letsencrypt.org/directory` and a separate ACME account key Secret.

Client mode does not issue or store certificates. cert-manager checks DNS propagation, obtains certificates, updates application TLS Secrets and handles renewals. Neither the application's A record nor its inbound HTTP routing needs to change for DNS-01 validation.

## Existing ClusterIssuer

If `letsencrypt-production` already exists outside this Helm release, keep `issuer.enabled: false` and add this solver to its existing `spec.acme.solvers`. Retain the HTTP-01 solvers used by other applications:

```yaml
- selector:
    dnsNames:
      - site-a.example.com
  dns01:
    acmeDNS:
      host: http://acme-dns-client.cert-manager.svc:80
      accountSecretRef:
        name: acme-dns-accounts
        key: acmedns.json
```

The hostname above assumes `fullnameOverride: acme-dns-client`, namespace `cert-manager`, and default service port 80. A chart-managed issuer fills this URL automatically and can retain fallback solvers through `issuer.additionalSolvers`.

## Behavior

- `GET /healthz` checks the client process only. The server's health and DNS access are checked when processing an update.
- `POST /update` accepts `X-Api-User`, `X-Api-Key`, and `{"subdomain":"site-a","txt":"<validation>"}`. It forwards the key as a Bearer token and the value as `{"value":"<validation>"}` to the configured server.
- Body limits match the server: 512 bytes and five seconds. Upstream requests have a 20-second deadline, do not follow redirects, and are not retried by client mode. Errors do not include upstream response bodies or credentials. Success requires `200 {"ok":true}` from the server.
- cert-manager's ACMEDNS cleanup is a no-op. The latest TXT value remains until replaced.
- Keep one validation at a time per DNS name across all clients and clusters, including base and wildcard names sharing the same TXT record. Client mode does not change the server's single-value storage.
- This chart exposes only an internal HTTP Service. Keep it internal to the cluster. HTTPS is used to forward client credentials to the configured external server.
- No APIService registration, custom DNS webhook, additional runtime language, or Cloudflare credentials are installed in the client cluster.
