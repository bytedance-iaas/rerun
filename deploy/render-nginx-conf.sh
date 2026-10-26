#!/bin/sh
# Render the web viewer's nginx site (deploy/nginx.conf, a template) to stdout:
#
#   BASE_PATH=/rerun CATALOG_UPSTREAM=127.0.0.1 render-nginx-conf.sh <template>
#
#   BASE_PATH         URL base path of the whole site: empty (the default) = the origin root,
#                     otherwise `/seg[/seg…]` of plain characters. A trailing slash is dropped,
#                     so `/rerun/` and `/rerun` are the same; `/` alone means the root.
#   CATALOG_UPSTREAM  catalog server host (default 127.0.0.1, the k8s pod layout).
#
# Used by entrypoint.sh (MODE=web) at container start, and by anyone checking the config
# offline: pipe the output into `nginx -t`. Placeholders are only filled in on non-comment
# lines, so the template's own documentation survives rendering verbatim.
set -eu

template="${1:?usage: render-nginx-conf.sh <template>}"

base_path="${BASE_PATH:-}"
# `/rerun/` -> `/rerun`; `/` -> `` (the root).
while [ "${base_path%/}" != "$base_path" ]; do
    base_path="${base_path%/}"
done
# Only plain path characters: the value is pasted into nginx `location` and `return` lines,
# where whitespace, `;`, braces, quotes or `$` would break (or rewrite) the configuration.
case "$base_path" in
'') ;;
/*)
    case "$base_path" in
    *//* | *[!A-Za-z0-9._~/-]* | */. | */./* | */.. | */../*)
        echo "render-nginx-conf: BASE_PATH must be a plain URL path like /rerun, got '${BASE_PATH:-}'" >&2
        exit 1
        ;;
    esac
    ;;
*)
    echo "render-nginx-conf: BASE_PATH must start with '/', got '${BASE_PATH:-}'" >&2
    exit 1
    ;;
esac

upstream="${CATALOG_UPSTREAM:-127.0.0.1}"
case "$upstream" in
'' | *[!A-Za-z0-9.-]*)
    echo "render-nginx-conf: CATALOG_UPSTREAM must be a host name or IPv4 address, got '$upstream'" >&2
    exit 1
    ;;
esac

# The block markers are whole lines of their own (the template's header mentions them in prose).
begin='^[[:space:]]*# BEGIN base-path-only[[:space:]]*$'
end='^[[:space:]]*# END base-path-only[[:space:]]*$'
if [ -z "$base_path" ]; then
    # Root layout: the sub-path-only locations would duplicate the root ones.
    drop="/$begin/,/$end/d"
else
    # Sub-path layout: keep the block, drop only its two marker lines.
    drop="/$begin/d;/$end/d"
fi

# `|` as the s-command delimiter: the base path contains `/` and is validated to hold no `|`
# (nor `&` or `\`, which sed would interpret in the replacement).
sed -e "$drop" \
    -e "/^[[:space:]]*#/!s|@BASE_PATH@|$base_path|g" \
    -e "/^[[:space:]]*#/!s|@CATALOG_UPSTREAM@|$upstream|g" \
    "$template"
