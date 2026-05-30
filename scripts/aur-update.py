#!/usr/bin/env python3
"""
Update PKGBUILD and .SRCINFO to the latest GitHub release.
Usage: python3 aur-update.py
"""

import json
import re
import subprocess
import sys
import urllib.request

REPO = "X0RA/Slint_PutMPV"
PROJECT_URL = f"https://github.com/{REPO}"
LATEST_RELEASE_URL = "https://api.github.com/repos/X0RA/Slint_PutMPV/releases/latest"
LINUX_ASSET_NAME = "putmpv-linux-x86_64"


def fetch_json(url):
    with urllib.request.urlopen(url) as r:
        return json.loads(r.read().decode())


def find_release_asset(release, name):
    for asset in release.get("assets", []):
        if asset.get("name") == name:
            return asset

    available = ", ".join(
        asset.get("name", "<unnamed>") for asset in release.get("assets", [])
    )
    raise RuntimeError(f"Could not find release asset {name!r}. Available: {available}")


def get_sha256(asset):
    digest = asset.get("digest", "")
    prefix = "sha256:"
    if digest.startswith(prefix):
        return digest[len(prefix) :]

    raise RuntimeError(f"Release asset {asset['name']!r} does not include a sha256 digest")


def main():
    print("Fetching latest release...")
    release = fetch_json(LATEST_RELEASE_URL)
    tag = release["tag_name"]
    version = tag.lstrip("v")
    print(f"  Latest: {tag}")

    print(f"Finding release asset: {LINUX_ASSET_NAME}")
    asset = find_release_asset(release, LINUX_ASSET_NAME)
    asset_url = asset["browser_download_url"]
    pkgbuild_asset_url = f"https://github.com/{REPO}/releases/download/v${{pkgver}}/{LINUX_ASSET_NAME}"
    sha256 = get_sha256(asset)
    print(f"  Asset: {asset_url}")
    print(f"  SHA256: {sha256}")

    with open("PKGBUILD") as f:
        content = f.read()

    original_content = content
    current_ver = re.search(r"^pkgver=(.+)", content, re.MULTILINE).group(1)

    content = re.sub(r"^pkgver=.*", f"pkgver={version}", content, flags=re.MULTILINE)
    content = re.sub(r"^pkgrel=.*", "pkgrel=1", content, flags=re.MULTILINE)
    content = re.sub(r'^url=".*"', f'url="{PROJECT_URL}"', content, flags=re.MULTILINE)
    content = re.sub(
        r'^source=\(".*?::https://github\.com/[^"]+"',
        f'source=("PutMPV-${{pkgver}}::{pkgbuild_asset_url}"',
        content,
        flags=re.MULTILINE,
    )
    content = re.sub(r"sha256sums=\('[a-f0-9]+'", f"sha256sums=('{sha256}'", content)

    if content == original_content:
        print(f"\nPKGBUILD already up to date at v{version}")
    else:
        with open("PKGBUILD", "w") as f:
            f.write(content)
        print(f"\nPKGBUILD updated: {current_ver} → {version}")

    result = subprocess.run(["makepkg", "--printsrcinfo"], capture_output=True, text=True)
    if result.returncode != 0:
        print("Error regenerating .SRCINFO:", result.stderr)
        sys.exit(1)

    with open(".SRCINFO", "w") as f:
        f.write(result.stdout)
    print(".SRCINFO regenerated")

    print(f"\nDone. Review with: git diff")
    print(f"Then commit: git add PKGBUILD .SRCINFO && git commit -m 'Update to v{version}' && git push")


if __name__ == "__main__":
    main()
