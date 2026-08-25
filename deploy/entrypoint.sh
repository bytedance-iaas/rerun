#!/bin/sh
# One image, three modes:
#   MODE=web    — nginx serving the wasm web viewer + /tos-config.json  (port 80)
#   MODE=native — the native viewer on a virtual display, via noVNC     (port 8080)
#   MODE=server — the rerun catalog server with tos:// registration and
#                 a persistent catalog                                  (port 51234)
set -eu

# require_env VAR — fail loudly if VAR is unset or empty. The deployment (the Helm chart) is the
# single source of truth for these values; the entrypoint deliberately carries no fallback default,
# so a chart that forgets to set one surfaces here instead of silently running with a stand-in.
require_env() {
    eval "_value=\${$1:-}"
    if [ -z "$_value" ]; then
        echo "Missing required environment variable: $1" >&2
        exit 1
    fi
}

require_env MODE

# Read secrets mounted by docker/k8s; env vars as fallback.
read_secret() {
    if [ -f "/run/secrets/$1" ]; then
        cat "/run/secrets/$1"
    else
        eval "printf '%s' \"\${$2:-}\""
    fi
}
TOS_AK="$(read_secret tos_access_key TOS_ACCESS_KEY)"
TOS_SK="$(read_secret tos_secret_key TOS_SECRET_KEY)"
HF_TOKEN_VALUE="$(read_secret hf_token HF_TOKEN)"

case "$MODE" in
web)
    # Optional Basic auth: a `web_htpasswd` secret (htpasswd format, e.g. from
    # `htpasswd -nbB user pass`) locks the whole site — viewer, /tos-config.json,
    # /rrd-cache. Without it everything stays open (local dev). /healthz is always open.
    WEB_HTPASSWD_VALUE="$(read_secret web_htpasswd WEB_HTPASSWD)"
    if [ -n "$WEB_HTPASSWD_VALUE" ]; then
        printf '%s\n' "$WEB_HTPASSWD_VALUE" > /run/htpasswd
        chmod 640 /run/htpasswd
        chown root:www-data /run/htpasswd
        cat > /run/nginx-auth.conf <<'AUTH'
auth_basic "rerun";
auth_basic_user_file /run/htpasswd;
AUTH
        echo "web: Basic auth enabled"
    else
        : > /run/nginx-auth.conf
        echo "web: no web_htpasswd secret — serving without authentication"
    fi

    # Endpoint/region/bucket come from the deployment — fail loudly rather than baking in a stand-in
    # that would quietly point the browser at the wrong TOS.
    require_env TOS_ENDPOINT
    require_env TOS_REGION
    require_env TOS_RRD_ARTIFACTS_URL
    require_env RRD_ARTIFACTS_PREFETCH

    # tos_access_key/tos_secret_key/hf_token are the server-side defaults for the browser dialogs
    # (used unless the user opts into "Use non-default AK/SK"). No daft_url: the viewer derives the
    # curation console as the same-origin /curation sibling path on its own.
    cat > /run/tos-config.json <<EOF
{
  "tos_endpoint": "${TOS_ENDPOINT}",
  "tos_region": "${TOS_REGION}",
  "tos_access_key": "${TOS_AK}",
  "tos_secret_key": "${TOS_SK}",
  "hf_token": "${HF_TOKEN_VALUE}",
  "tos_rrd_artifacts_url": "${TOS_RRD_ARTIFACTS_URL}",
  "rrd_artifacts_prefetch": ${RRD_ARTIFACTS_PREFETCH}
}
EOF
    chmod 644 /run/tos-config.json
    # Adopt the cache volume: a volume created by an earlier image keeps that
    # image's ownership, which blocks WebDAV PUTs from this nginx's www-data.
    chown www-data:www-data /rrd-cache
    exec nginx -g 'daemon off;'
    ;;

native)
    export TOS_ACCESS_KEY="$TOS_AK"
    export TOS_SECRET_KEY="$TOS_SK"
    export HF_TOKEN="$HF_TOKEN_VALUE"

    require_env SESSION_GEOMETRY
    GEOMETRY="$SESSION_GEOMETRY"

    # Optional session password: with a `session_password` secret (or SESSION_PASSWORD
    # env), the VNC handshake requires it — noVNC prompts for it in the browser before
    # the desktop is shown. Without one the session is open (port-forward / local use
    # only; never expose an unauthenticated session on a public address).
    SESSION_PASSWORD_VALUE="$(read_secret session_password SESSION_PASSWORD)"
    if [ -n "$SESSION_PASSWORD_VALUE" ]; then
        printf '%s' "$SESSION_PASSWORD_VALUE" | vncpasswd -f > /run/vncpasswd
        chmod 600 /run/vncpasswd
        SECURITY_ARGS="-SecurityTypes VncAuth -rfbauth /run/vncpasswd"
        echo "native: VNC password enabled"
    else
        SECURITY_ARGS="-SecurityTypes None"
        echo "native: no session_password secret — session is unauthenticated"
    fi

    # Virtual X server with built-in VNC output. $SECURITY_ARGS is intentionally
    # unquoted: it is two-to-four space-separated flags, none of which contain spaces.
    Xvnc :1 \
        -geometry "$GEOMETRY" \
        -depth 24 \
        -rfbport 5901 \
        -AlwaysShared \
        $SECURITY_ARGS \
        &

    i=0
    while ! [ -e "/tmp/.X11-unix/X1" ]; do
        i=$((i + 1))
        [ "$i" -gt 50 ] && echo "Xvnc failed to start" && exit 1
        sleep 0.2
    done

    # Browser-facing bridge: serves the noVNC page and proxies websocket -> VNC.
    websockify --web /usr/share/novnc 8080 localhost:5901 &

    export DISPLAY=":1"

    # Clipboard bridge: copies text between the VNC clipboard (what the browser sends)
    # and the X selections the viewer reads. Paste is dead without it.
    vncconfig -nowin &

    # Kiosk window manager: keeps the viewer maximized to whatever the desktop size
    # currently is. This is what makes noVNC's resize=remote mode work end to end —
    # the browser resizes the remote desktop (RandR), and matchbox re-fits the window.
    # Without a WM the window keeps its default size: on a large desktop it huddles
    # in the top-left corner, and scaling makes everything blurry.
    matchbox-window-manager -use_titlebar no &
    # Software rendering: Vulkan/lavapipe (needs Mesa >= 24; see Dockerfile).
    export WGPU_BACKEND="${SESSION_WGPU_BACKEND:-vulkan}"

    # Keep the session alive across viewer restarts (closing the window doesn't kill
    # the pod; the platform owns the pod lifecycle).
    while true; do
        rerun "$@" &
        rerun_pid=$!

        wait "$rerun_pid" || true
        echo "viewer exited; restarting in 2s…"
        sleep 2
    done
    ;;

server)
    # The catalog server reads TOS credentials from the environment for tos:// registration.
    export TOS_ACCESS_KEY="$TOS_AK"
    export TOS_SECRET_KEY="$TOS_SK"

    # Catalog + remote-file cache live on the mounted volume: restarts keep the catalog.
    export RERUN_SERVER_DATA_DIR="${RERUN_SERVER_DATA_DIR:-/server-data}"

    exec rerun server --port 51234 "$@"
    ;;

*)
    echo "Unknown MODE: $MODE (expected 'web', 'native', or 'server')" >&2
    exit 1
    ;;
esac
