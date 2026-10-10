# acme-dns Helm chart

Requires Kubernetes 1.25+ and Helm 3. See [installation](../../README.md#helm) and [values.yaml](values.yaml).

- One replica; updates stop the old pod first.
- `config.existingConfigMap` accepts a ConfigMap with a `clients.json` key instead of inline config.
- Config changes in Helm values restart the pod. For external ConfigMap or Secret changes, change `podAnnotations.configRevision` and run `helm upgrade`.
- For branch images, set `image.pullPolicy: Always` and change `podAnnotations.imageRevision` on each upgrade. Alternatively, deploy a version tag.
