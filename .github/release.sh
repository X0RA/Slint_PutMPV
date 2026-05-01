#!/usr/bin/env bash
set -euo pipefail

read -rp "Version (e.g. 1.0.0): " VERSION

if ! [[ "${VERSION}" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]; then
  echo "Error: version must be x.x.x, got: ${VERSION}" >&2
  exit 1
fi

echo "Triggering release ${VERSION}..."
gh workflow run release.yml -f version="${VERSION}"

echo "Workflow dispatched. Watch it with:"
echo "   gh run watch"