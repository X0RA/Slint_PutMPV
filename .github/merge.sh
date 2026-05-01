#!/usr/bin/env bash
set -euo pipefail

git switch master && git merge --ff-only dev && git push origin master