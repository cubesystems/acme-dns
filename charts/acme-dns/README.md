# acme-dns Helm chart

Requires Kubernetes 1.25+ and Helm 3. See [installation](../../README.md#helm) and [values.yaml](values.yaml).

- One replica; updates stop the old pod first.
- `config.existingConfigMap` accepts a ConfigMap with a `clients.json` key instead of inline config.
- Config and chart-managed Secret changes in Helm values restart the pod. For external ConfigMap or Secret changes, change `podAnnotations.configRevision` and run `helm upgrade`.
- For branch images, set `image.pullPolicy: Always` and change `podAnnotations.imageRevision` on each upgrade. Alternatively, deploy a version tag.

## Encrypted secrets with k8s-tools 2.x

The chart can create the Secret referenced by `cloudflare.existingSecret` from
`secrets.<name>.data`. k8s-tools encrypts these values for Git and decrypts them
before invoking Helm. The chart rejects any remaining `HELM_SECRET:` value so
ciphertext cannot accidentally be installed as the API token.

Configure the deployment's `keyRef` to point to a local, Git-ignored key file
or a supported secret-manager reference. Then, from your deployment directory:

```sh
./secrets set acme-dns --secret cloudflare-dns --key CF_DNS_API_TOKEN
```

This example assumes one values file and `cloudflare.existingSecret: cloudflare-dns`.
The command prompts without echoing the token and stores only ciphertext:

```yaml
cloudflare:
  existingSecret: cloudflare-dns
secrets:
  cloudflare-dns:
    data:
      CF_DNS_API_TOKEN: HELM_SECRET:<generated-ciphertext>
```

Deploy through k8s-tools so Helm receives decrypted values. Keep the encryption
key outside Git and back it up in your secret manager; deployment and future
token edits require it. Kubernetes and Helm release storage receive the actual
secret, so cluster access must remain restricted. External Secrets continue to
work when `secrets` is empty.
