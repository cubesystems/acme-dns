"""Check chart wiring without a Kubernetes cluster. Requires Helm and PyYAML."""

import json
import subprocess
import unittest
import yaml

CHART = "charts/acme-dns"


def render(*overrides, success=True):
    command = ["helm", "template", "demo", CHART, "-f", f"{CHART}/ci/test-values.yaml"]
    for override in overrides:
        command.extend(["--set-string", override])
    result = subprocess.run(command, capture_output=True, text=True)
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


if __name__ == "__main__":
    unittest.main()
