"""Check chart wiring without a Kubernetes cluster. Requires Helm and PyYAML."""

import base64
import json
import subprocess
import unittest
import yaml

CHART = "charts/acme-dns"


def render(*overrides, success=True, chart=CHART, values=None):
    command = ["helm", "template", "demo", chart, "--namespace", "cert-manager", "-f", f"{chart}/ci/test-values.yaml"]
    for override in overrides:
        command.extend(["--set-string", override])
    if values is not None:
        command.extend(["-f", "-"])
    result = subprocess.run(command, input=json.dumps(values) if values is not None else None,
                            capture_output=True, text=True)
    if not success:
        assert result.returncode != 0, "Expected invalid chart values to fail"
        return result.stderr
    assert result.returncode == 0, result.stderr
    return {doc["kind"]: doc for doc in yaml.safe_load_all(result.stdout) if doc}


class ChartTests(unittest.TestCase):
    def test_config_secret_and_service_wiring(self):
        docs = render()
        deployment = docs["Deployment"]
        self.assertEqual(deployment["spec"]["replicas"], 1)
        self.assertEqual(deployment["spec"]["strategy"], {"type": "Recreate"})
        pod = deployment["spec"]["template"]
        container = pod["spec"]["containers"][0]
        env = {entry["name"]: entry for entry in container["env"]}
        self.assertEqual(env["CF_DNS_API_TOKEN"]["valueFrom"]["secretKeyRef"], {
            "name": "dns-api-test", "key": "CF_DNS_API_TOKEN",
        })
        self.assertNotIn("Secret", docs)
        self.assertEqual(env["CLIENT_CONFIG_FILE"]["value"], "/config/clients.json")
        volume = pod["spec"]["volumes"][0]["configMap"]
        self.assertEqual(volume["name"], docs["ConfigMap"]["metadata"]["name"])
        self.assertEqual(volume["items"], [{"key": "clients.json", "path": "clients.json"}])
        config = json.loads(docs["ConfigMap"]["data"]["clients.json"])
        self.assertEqual(config["zoneName"], "example.com")
        self.assertEqual(config["clients"][0]["recordName"], "_acme-challenge.site-a.example.com")
        self.assertEqual(docs["Service"]["spec"]["selector"], deployment["spec"]["selector"]["matchLabels"])
        self.assertEqual(docs["Service"]["spec"]["ports"][0]["targetPort"], container["ports"][0]["name"])
        self.assertEqual(container["readinessProbe"]["httpGet"], {"path": "/healthz", "port": "http"})
        self.assertFalse(pod["spec"]["automountServiceAccountToken"])
        self.assertTrue(container["securityContext"]["readOnlyRootFilesystem"])
        self.assertNotIn("Ingress", docs)

    def test_config_changes_trigger_replacement(self):
        before = render()["Deployment"]["spec"]["template"]["metadata"]["annotations"]
        after = render("config.zoneName=other.example")["Deployment"]["spec"]["template"]["metadata"]["annotations"]
        self.assertNotEqual(before["checksum/config"], after["checksum/config"])

    def test_external_config_and_custom_secret(self):
        docs = render("config.existingConfigMap=external-clients", "config.zoneId=",
                      "config.zoneName=", "cloudflare.existingSecret=external-token",
                      "cloudflare.secretKey=token")
        self.assertNotIn("ConfigMap", docs)
        pod = docs["Deployment"]["spec"]["template"]["spec"]
        self.assertEqual(pod["volumes"][0]["configMap"]["name"], "external-clients")
        env = {entry["name"]: entry for entry in pod["containers"][0]["env"]}
        self.assertEqual(env["CF_DNS_API_TOKEN"]["valueFrom"]["secretKeyRef"], {
            "name": "external-token", "key": "token",
        })

    def test_managed_secret_matches_reference_and_rotates_pod(self):
        field = "secrets.dns-api-test.data.CF_DNS_API_TOKEN"
        docs = render(f"{field}=fake-test-token")
        secret = docs["Secret"]
        self.assertEqual(secret["metadata"]["name"], "dns-api-test")
        self.assertEqual(secret["type"], "Opaque")
        self.assertEqual(base64.b64decode(secret["data"]["CF_DNS_API_TOKEN"]), b"fake-test-token")
        before = docs["Deployment"]["spec"]["template"]["metadata"]["annotations"]
        after = render(f"{field}=rotated-test-token")["Deployment"]["spec"]["template"]["metadata"]["annotations"]
        self.assertNotEqual(before["checksum/secrets"], after["checksum/secrets"])
        self.assertEqual(before["checksum/config"], after["checksum/config"])

    def test_encrypted_secret_requires_k8s_tools_decryption(self):
        error = render("secrets.dns-api-test.data.CF_DNS_API_TOKEN=HELM_SECRET:fake", success=False)
        self.assertIn("must be decrypted by k8s-tools", error)

    def test_ingress_targets_service_with_custom_name(self):
        docs = render("ingress.enabled=true", "ingress.host=dns.example.com",
                      "ingress.className=custom", "fullnameOverride=custom-dns")
        ingress = docs["Ingress"]["spec"]
        self.assertEqual(ingress["ingressClassName"], "custom")
        self.assertEqual(ingress["rules"][0]["host"], "dns.example.com")
        backend = ingress["rules"][0]["http"]["paths"][0]["backend"]["service"]
        self.assertEqual(backend, {"name": docs["Service"]["metadata"]["name"], "port": {"name": "http"}})

    def test_image_digest_takes_precedence(self):
        digest = "sha256:" + "a" * 64
        docs = render(f"image.digest={digest}", "image.tag=ignored")
        container = docs["Deployment"]["spec"]["template"]["spec"]["containers"][0]
        self.assertEqual(container["image"], f"example/acme-dns@{digest}")

    def test_missing_required_values_fail(self):
        for value, message in [
            ("image.repository=", "image.repository is required"),
            ("cloudflare.existingSecret=", "cloudflare.existingSecret is required"),
            ("config.zoneId=", "config.zoneId is required"),
            ("config.zoneName=", "config.zoneName is required"),
            ("ingress.enabled=true", "ingress.host is required"),
        ]:
            with self.subTest(value=value):
                self.assertIn(message, render(value, success=False))


class ClientChartTests(unittest.TestCase):
    def render(self, *overrides, **kwargs):
        return render(*overrides, chart="charts/acme-dns-client", **kwargs)

    def test_same_image_in_client_mode_without_cloudflare_or_kubernetes_credentials(self):
        docs = self.render()
        self.assertEqual(set(docs), {"Deployment", "Service"})
        pod = docs["Deployment"]["spec"]["template"]["spec"]
        container = pod["containers"][0]
        self.assertEqual(container["image"], "cubesystems/acme-dns:test-client-mode")
        self.assertEqual(container["args"], ["--client-mode"])
        self.assertEqual({e["name"]: e["value"] for e in container["env"]}, {
            "LISTEN_ADDR": "0.0.0.0:8080", "ACME_DNS_SERVER_URL": "https://acme.example.com",
        })
        self.assertFalse(pod["automountServiceAccountToken"])
        self.assertNotIn("volumes", pod)
        self.assertTrue(container["securityContext"]["readOnlyRootFilesystem"])
        self.assertTrue(pod["securityContext"]["runAsNonRoot"])
        self.assertEqual(docs["Service"]["spec"]["type"], "ClusterIP")
        self.assertEqual(docs["Service"]["spec"]["selector"], docs["Deployment"]["spec"]["selector"]["matchLabels"])
        self.assertEqual(container["readinessProbe"]["httpGet"], {"path": "/healthz", "port": "http"})

    def test_optional_issuer_points_to_this_service_and_account_secret(self):
        docs = self.render("issuer.enabled=true", "issuer.email=admin@example.com",
                           "issuer.dnsNames[0]=site-a.example.com", "issuer.accountSecretRef.name=client-keys",
                           "issuer.additionalSolvers[0].http01.ingress.class=public",
                           "fullnameOverride=shared-client", "service.port=8081")
        acme = docs["ClusterIssuer"]["spec"]["acme"]
        self.assertEqual(acme["privateKeySecretRef"]["name"], "letsencrypt-acme-dns-account")
        self.assertEqual(acme["solvers"], [
            {"selector": {"dnsNames": ["site-a.example.com"]}, "dns01": {"acmeDNS": {
                "host": "http://shared-client.cert-manager.svc:8081",
                "accountSecretRef": {"name": "client-keys", "key": "acmedns.json"},
            }}},
            {"http01": {"ingress": {"class": "public"}}},
        ])

    def test_secrets_support_k8s_tools_and_are_not_mounted_in_client(self):
        field = r"secrets.client-keys.data.acmedns\.json"
        account_json = '{"site-a.example.com":{"password":"fake"}}'
        docs = self.render(values={"secrets": {"client-keys": {"data": {"acmedns.json": account_json}}}})
        self.assertEqual(base64.b64decode(docs["Secret"]["data"]["acmedns.json"]).decode(), account_json)
        self.assertNotIn("volumes", docs["Deployment"]["spec"]["template"]["spec"])
        self.assertIn("must be decrypted by k8s-tools", self.render(f"{field}=HELM_SECRET:fake", success=False))

    def test_digest_and_upstream_configuration(self):
        digest = "sha256:" + "a" * 64
        docs = self.render(f"image.digest={digest}", "image.tag=", "serverUrl=https://other.example/base")
        container = docs["Deployment"]["spec"]["template"]["spec"]["containers"][0]
        self.assertEqual(container["image"], f"cubesystems/acme-dns@{digest}")
        self.assertEqual(container["env"][1]["value"], "https://other.example/base")

    def test_invalid_configuration_fails_rendering(self):
        for value, message in [
            ("serverUrl=", "serverUrl is required"),
            ("serverUrl=http://acme.example.com", "serverUrl must be HTTPS"),
            ("serverUrl=https://user:secret@acme.example.com", "serverUrl must be HTTPS"),
            ("image.repository=", "image.repository is required"),
            ("image.tag=", "image.tag or image.digest is required"),
            ("issuer.enabled=true", "issuer.email is required"),
        ]:
            with self.subTest(value=value):
                self.assertIn(message, self.render(value, success=False))


if __name__ == "__main__":
    unittest.main()
