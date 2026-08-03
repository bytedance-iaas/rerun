#!/bin/sh
# One image, three modes:
#   MODE=web (default) — nginx serving the wasm web viewer + /tos-config.json  (port 80)
#   MODE=native        — the native viewer on a virtual display, via noVNC     (port 8080)
#   MODE=server        — the rerun catalog server with tos:// registration and
#                        a persistent catalog                                  (port 51234)
set -eu

MODE="${MODE:-web}"

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
    # Server-side defaults for the browser dialogs. The default credentials are used
    # unless the user opts into "Use non-default AK/SK" in the dialog.
    cat > /run/tos-config.json <<EOF
{
  "tos_endpoint": "${TOS_ENDPOINT:-https://tos-s3-cn-beijing.volces.com}",
  "tos_region": "${TOS_REGION:-cn-beijing}",
  "tos_access_key": "${TOS_AK}",
  "tos_secret_key": "${TOS_SK}",
  "hf_token": "${HF_TOKEN_VALUE}"
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

    GEOMETRY="${SESSION_GEOMETRY:-1280x800}"

    # Virtual X server with built-in VNC output (no auth: front it with your
    # platform's ingress/port-forward; never expose the raw VNC port).
    Xvnc :1 \
        -geometry "$GEOMETRY" \
        -depth 24 \
        -SecurityTypes None \
        -rfbport 5901 \
        -AlwaysShared \
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
    # Software rendering: Vulkan/lavapipe (needs Mesa >= 24; see Dockerfile).
    export WGPU_BACKEND="${SESSION_WGPU_BACKEND:-vulkan}"

    # Keep the session alive across viewer restarts (closing the window doesn't kill
    # the pod; the platform owns the pod lifecycle).
    while true; do
        rerun "$@" &
        rerun_pid=$!

        # No window manager runs in this session, so nothing ever grants the viewer
        # window X input focus — and without it, text fields ignore the keyboard.
        # Focus the window once it appears.
        (
            i=0
            while [ "$i" -lt 100 ]; do
                i=$((i + 1))
                sleep 0.3
                window_id="$(xdotool search --onlyvisible --name '^Rerun' 2>/dev/null | head -n1)"
                if [ -n "$window_id" ]; then
                    xdotool windowfocus "$window_id" 2>/dev/null && break
                fi
            done
        ) &

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
