#!/bin/sh
# Launch the natively-built viewer with the same TOS/HF defaults the web deployment uses.
# The native dialogs read these environment variables instead of /config.json.
set -eu
cd "$(dirname "$0")/.."

export TOS_ENDPOINT="${TOS_ENDPOINT:-https://tos-s3-cn-beijing.volces.com}"
export TOS_REGION="${TOS_REGION:-cn-beijing}"
export TOS_ACCESS_KEY="$(cat deploy/secrets/tos_access_key)"
export TOS_SECRET_KEY="$(cat deploy/secrets/tos_secret_key)"
export HF_TOKEN="$(cat deploy/secrets/hf_token 2>/dev/null || true)"

exec ./target/debug/rerun "$@"
