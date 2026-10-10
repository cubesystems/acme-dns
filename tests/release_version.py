"""An image release tag must match the Rust package version."""

import pathlib
import re
import sys
import tomllib

tag = sys.argv[1]
assert re.fullmatch(r"v\d+\.\d+\.\d+", tag), "Use a stable vMAJOR.MINOR.PATCH tag"
version = tag[1:]
package = tomllib.loads(pathlib.Path("Cargo.toml").read_text())["package"]
lock = tomllib.loads(pathlib.Path("Cargo.lock").read_text())
locked_package = next(p for p in lock["package"] if p["name"] == package["name"] and "source" not in p)
assert version == package["version"] == locked_package["version"], "Image release versions differ"
print(f"Release versions match {tag}")
