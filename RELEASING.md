# Publishing

Docker images publish through GitHub Actions. Charts are packaged manually and served by GitHub Pages.

## Docker Hub

Under **GitHub Settings → Secrets and variables → Actions**, add:

| Name | Kind | Value |
| --- | --- | --- |
| `DOCKERHUB_USERNAME` | Variable | Docker Hub login with push access |
| `DOCKERHUB_TOKEN` | Secret | Docker Hub token with write permission |
| `DOCKERHUB_IMAGE` | Optional variable | Default `cubesystems/acme-dns` |

Pushes and pull requests run Rust, container and Helm checks. Successful pushes publish amd64/arm64 images; pull requests do not publish.

| Git ref | Image tags |
| --- | --- |
| Branch `master` | `master` |
| Branch `feature/example` | `feature-example` |
| Tag `preview` | `preview` |
| Tag `v1.1.0` | `v1.1.0` |

Only branch and Git tag names are published. Branch tags update on each successful push. Avoid names that become identical when `/` is replaced by `-`.

Tags starting with `v` must be `vMAJOR.MINOR.PATCH`, matching `Cargo.toml` and `Cargo.lock`. Other pushes need no version bump.

## Chart updates

Once, enable **GitHub Settings → Pages → Deploy from a branch → master → / (root)**.

From the repository root:

1. Bump `version` in the changed chart's `Chart.yaml`. The server chart is `charts/acme-dns`; the client chart is `charts/acme-dns-client`. Change the server chart's `appVersion` only to change its default image tag. The client chart requires an explicit image tag or digest containing `--client-mode`.
2. Package:

   ```sh
   helm package charts/acme-dns --destination packages
   helm package charts/acme-dns-client --destination packages
   ```

3. Reindex:

   ```sh
   helm repo index . --url https://cubesystems.github.io/acme-dns/
   ```

4. Commit and push the chart, new package and `index.yaml` to `master`. Keep older packages.

GitHub Pages serves `https://cubesystems.github.io/acme-dns/`. CI checks charts but does not publish them.

Both charts use the same `cubesystems/acme-dns` image. Client mode is part of the normal Rust binary and Docker build; it has no separate image or release pipeline. Publish an image containing client mode before distributing the client chart for deployment.
