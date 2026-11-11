#!/usr/bin/env bash
# sepia launcher — works from a source checkout without installing anything.
set -euo pipefail
DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PY="${PYTHON:-python3}"
export PYTHONPATH="${DIR}${PYTHONPATH:+:${PYTHONPATH}}"
exec "${PY}" -m sepia.cli "$@"
