# deploy — the unified rerun viewer service (web + native + server in one image) <!-- NOLINT -->

One image (`rerun-viewer-unified`) built from this repo, three modes selected by the `MODE` env var:

- `MODE=web` (default): nginx serving the wasm web viewer, plus server-side default credentials (`/tos-config.json`) and an `rrd-cache` volume (port 80).
- `MODE=native`: the Linux-native viewer on a virtual display, streamed to the browser via noVNC (port 8080) — for datasets beyond the browser's ~1.5 GB per-file limit.
- `MODE=server`: the rerun catalog server (gRPC, port 51234) with two cloud additions — `dataset.register()` / `register_prefix()` accept `tos://` URLs, and the catalog survives restarts (SQLite on the `server-data` volume). Clients use the stock SDK: `rr.catalog.CatalogClient("rerun+http://<host>:9094")`.

Both viewers support "Open from Volcengine TOS…" and "Open from Hugging Face…" (streaming LeRobot v2/v3, MCAP/file repos, single files).

The same image builds and runs both **locally** (throttled defaults survive an 8 GB Docker Desktop VM) and in the **cloud / CI** (lift the throttles, point downloads at mirrors — see below). The cloud deployment is a Helm chart — see [`helm/rerun-cloud/`](helm/rerun-cloud/README.md), plus [`helm/rerun-native-session/`](helm/rerun-native-session/README.md) for on-demand native-viewer sessions.

## Layout

| File | Purpose |
|---|---|
| `Dockerfile` | Multi-stage: wasm viewer build + native viewer build (serialized to avoid OOM) + one runtime with nginx and the Xvnc/noVNC stack. Build context is the repo root. |
| `docker-compose.yml` | Three services from the same image: `viewer` (web, `:9091`), `native-session` (native, `:9092`), `server` (catalog, `:9094`). |
| `entrypoint.sh` | `MODE` dispatch: web renders `/tos-config.json` and runs nginx; native starts Xvnc + websockify + the viewer; server runs the catalog. |
| `nginx.conf` | Static serving + `/tos-config.json` + WebDAV `PUT` on `/rrd-cache/` (phase-2 write-back) + `301 /rerun → /` (the gateway routes `/curation` to the Daft console, everything else here). |
| `helm/rerun-cloud/` | The whole cloud stack as a Helm chart: rerun (web + catalog StatefulSet with a persistent volume) + the Daft curation console (StatefulSet on a TOS mount, served under `/curation`; the viewer's Diagnose buttons deep-link into it, `?dataset=<name>`) + the APIG gateway with path-routed Ingress (`/` = viewer, `/curation` = Daft) and a dedicated gRPC route for the catalog. `helm install rerun-cloud deploy/helm/rerun-cloud -f <your-values>.yaml`. |
| `helm/rerun-native-session/` | On-demand native-viewer session (one pod per user), started/torn down per session. |
| `novnc-paste-bridge.js` | Appended into noVNC's `ui.js` at build time so Cmd+V / Ctrl+V pastes into the native session in one step. |
| `.env.example` | Non-secret settings: endpoint, region. Copy to `.env` (gitignored). |
| `gen-ca-bundle.sh` | Exports macOS keychain certs so cargo can download deps behind a corporate TLS-intercepting proxy. Optional; skip on Linux / no proxy. |
| `enable-cors.sh` | CORS setup for the TOS bucket (so the browser reads the bucket directly). Must list **every origin the web viewer is served from** — re-run it whenever the viewer gets a new address (see "Bucket CORS"). |
| `run-native.sh` | Runs a host-built native viewer with credentials from `secrets/` (dev convenience). |

Credentials live in `deploy/secrets/` (`tos_access_key`, `tos_secret_key`, `hf_token`) — gitignored, mounted as docker secrets. `secrets/`, `.env`, and `ca-bundle.pem` are all gitignored.

## Local build & run

```bash
cd deploy
./gen-ca-bundle.sh             # once per machine, only behind a corporate proxy
cp .env.example .env           # then edit; create secrets/ with your AK/SK
# Building the SDK needs the macOS wheel URL: set MAC_WHEEL_URL in .env (see below),
# or pass BUILD_SDK_WHEEL=0 to skip the SDK entirely for a quick dev image.
docker compose up --build -d   # first build compiles both viewers: expect ~45-60 min
open http://127.0.0.1:9091     # web viewer
open "http://127.0.0.1:9092/vnc.html?autoconnect=true&resize=remote"   # native session
```

## macOS SDK wheel

The image builds and serves the **Linux** SDK wheel itself (nginx `/downloads/sdk/`), viewer bundled inside — `pip install` it and `rerun` is on PATH.
A **macOS** wheel cannot be built in the Linux build container (a Mach-O binary needs the macOS SDK). It also cannot travel in git — at ~120 MB it exceeds GitHub's 100 MB file limit, and git-lfs does not survive the CN build pipeline's shallow clone. So it is built **on a Mac**, uploaded to a **public-read TOS bucket**, and the Dockerfile fetches it at build time into `/downloads/sdk/` next to the Linux wheel; `pip install` on a Mac picks the matching one.

Build it on an Apple-Silicon Mac from the repo root, upload it, then build the image:

```bash
# 1. Build the native viewer binary that gets bundled into the wheel (Outputs target/release/rerun).
pixi run rerun-build     # or: cargo build -p rerun-cli --release --features native_viewer,map_view

# 2. Put that binary where maturin's `include` list expects it, and build the wheel into artifacts/.
rm -f rerun_py/rerun_sdk/rerun_cli/rerun
cp target/release/rerun rerun_py/rerun_sdk/rerun_cli/rerun
PYO3_CONFIG_FILE="$PWD/rerun_py/pyo3-build.cfg" \
  .venv/bin/maturin build --release --manifest-path rerun_py/Cargo.toml -o artifacts
#   → artifacts/rerun_sdk-<ver>-cp310-abi3-macosx_11_0_arm64.whl

# 3. Upload to the public-read TOS bucket (AWS CLI works against TOS; virtual-hosted addressing).
#    Then set the object public-read (a one-time bucket policy already grants anonymous GetObject).
aws configure set default.s3.addressing_style virtual
aws s3 cp artifacts/rerun_sdk-*.whl s3://<bucket>/mac/ \
  --endpoint-url https://tos-s3-cn-beijing.volces.com --region cn-beijing

# 4. Tell the build where the wheel is, and build the image (from deploy/).
#    The bucket URL is NOT hardcoded — set MAC_WHEEL_URL in .env (compose passes it through
#    as a build arg), or `docker build --build-arg MAC_WHEEL_URL=…` for a plain build.
echo 'MAC_WHEEL_URL=https://<bucket>.tos-s3-cn-beijing.volces.com/mac/rerun_sdk-<ver>-cp310-abi3-macosx_11_0_arm64.whl' >> deploy/.env
cd deploy && docker compose build
```

Call maturin directly (not `uv run maturin`) so it does not try to sync PyPI runtime deps — behind a TLS-intercepting proxy that download fails, and the wheel build does not need them.

The wheel is **not in git** (`artifacts/` is gitignored) and its bucket URL is **not hardcoded** — pass it via `MAC_WHEEL_URL` (`.env` / `--build-arg`). When building the SDK (`BUILD_SDK_WHEEL=1`, the default) `MAC_WHEEL_URL` is **required**: an empty value, a failed download, or an under-1 MB response fails the build rather than silently shipping without the macOS wheel. Set `BUILD_SDK_WHEEL=0` to skip the SDK for a fast dev image. The bucket needs to be readable by the build host — either make the object public-read (an anonymous-`GetObject` bucket policy, so a plain `curl` works with no credentials) or use a pre-signed URL. On a version bump: rebuild on the Mac, `aws s3 cp` the new wheel up, and update `MAC_WHEEL_URL`. Only Apple-Silicon (arm64) is produced; add `--target x86_64-apple-darwin` / `universal2` if Intel Macs need covering.

## Local native viewer (no cloud, no docker)

The three modes above all run the viewer as a *service* (behind nginx / noVNC / gRPC).
The same viewer also runs as a plain desktop app directly on your machine — no Docker, no VNC, no cloud — sharing the exact same `re_viewer` code and the same "Open from Volcengine TOS / Hugging Face" features.
See [`docs/local-native-viewer.md`](../docs/local-native-viewer.md) for the full reference; the smoke test:

```bash
# 1. Build the local viewer (from the repo root). Outputs target/release/rerun.
pixi run local-viewer

# 2. (Optional) pre-fill credentials so you don't retype them.
#    Same file the web deployment serves as /tos-config.json, read from your home dir.
mkdir -p ~/.rerun
cat > ~/.rerun/tos-config.json <<'EOF'
{
  "tos_endpoint": "https://tos-s3-cn-beijing.volces.com",
  "tos_region": "cn-beijing",
  "tos_access_key": "AK…",
  "tos_secret_key": "SK…",
  "hf_token": "hf_…"
}
EOF
chmod 600 ~/.rerun/tos-config.json

# 3. Run it.
./target/release/rerun
```

Then, in the viewer: **Menu → Open → Open from Volcengine TOS…**, enter a `tos://bucket/prefix/` URL, and click **Open** — episodes should appear immediately and stream in one by one.
Credentials can come from three places (highest priority first): the `TOS_ACCESS_KEY` / `TOS_SECRET_KEY` / `HF_TOKEN` environment variables, then `~/.rerun/tos-config.json` (or `$RERUN_TOS_CONFIG`), then whatever you type into the dialog.
With no file and no env vars, the dialog just asks for the AK/SK directly — nothing cloud-side is required.

## Cloud / CI builds

The default build args throttle for a small Docker VM (stages serialized, 3 cargo jobs). On a well-provisioned builder, lift them:

```bash
docker build -f deploy/Dockerfile -t rerun-viewer-unified \
  --build-arg BUILD_GATE=no-gate \   # wasm + native stages in parallel
  --build-arg CARGO_JOBS= \          # empty = all CPU cores
  --build-arg SCM_COMMIT_ID=$(git rev-parse HEAD) \
  .
```

`RELEASE_LTO` / `RELEASE_CGU` are also args if you want the workspace's size-optimized settings back (needs plenty of RAM).

Building inside mainland China, point every download at a mirror:

```bash
  --build-arg BASE_REGISTRY=hub-cache-cn-beijing.cr.volces.com/
  --build-arg APT_MIRROR=http://mirrors.volces.com     # http! see the Dockerfile note
  --build-arg RUSTUP_DIST_SERVER=https://rsproxy.cn
  --build-arg RUSTUP_UPDATE_ROOT=https://rsproxy.cn/rustup
  --build-arg CARGO_MIRROR=sparse+https://rsproxy.cn/index/
```

## Bucket CORS — required for every new web-viewer address

The web viewer talks to TOS **directly from the browser**, and browsers enforce CORS: the bucket only answers pages served from an origin on its `AllowedOrigin` whitelist.
That whitelist lives in `enable-cors.sh`, so putting the web viewer at a new address is always two steps:

1. Add the new origin (scheme + host + port, no trailing slash) to the `AllowedOrigin` list in `enable-cors.sh`. Common ones:
   - `https://*.apigateway-cn-beijing.volceapi.com` — **use the wildcard** for the auto-assigned APIG domain (how most users reach the viewer publicly). The APIG domain is re-assigned on every gateway recreate, so a wildcard means you never have to chase the new `<id>` — TOS supports one `*` in an `AllowedOrigin`.
   - `http://rerun-web` — the in-cluster Service name, when testing the web viewer from a browser running inside the cluster (e.g. `browser-test.yaml`).
   - `http://101.126.41.246` — a VKE LoadBalancer / gateway public IP.
   - `http://127.0.0.1:9091` — local docker.
2. Re-run `./enable-cors.sh` (needs `secrets/`) — it overwrites the bucket's CORS config and prints a verification preflight.

The origin is the address in the browser's URL bar (the viewer's own host), **not** the TOS endpoint the data lives on — those are different hosts.

Symptoms of a missing origin: the web viewer loads fine, but opening any `tos://` dataset fails in the browser with a cryptic `Request failed: Failed to fetch` (CORS errors in the dev console), and rrd-artifact lookups silently miss — while the native viewers (which are not subject to CORS) work normally.
The `ExposeHeader` entries for `x-amz-meta-rerun-*` are equally load-bearing: without them the browser hides the fingerprint header and every artifact lookup misses.

## Credential model

`entrypoint.sh` reads AK/SK/token from docker/k8s secrets (`/run/secrets/*`), falling back to `TOS_ACCESS_KEY` / `TOS_SECRET_KEY` / `HF_TOKEN` env vars. In `web` mode these are baked into `/tos-config.json` for the browser dialogs; in `native` and `server` modes they are exported into the viewer's environment.

Two more optional secrets gate access to the browser-facing modes (see the auth commit / `helm/rerun-cloud/README.md`):

- `web_htpasswd` (htpasswd format) — enables nginx Basic auth for the whole web mode, including `/tos-config.json`. Without it the site (and the default credentials) is readable by anyone who can reach it — fine locally, not on a public address. `/healthz` stays open for probes.
- `session_password` / `SESSION_PASSWORD` env — enables the VNC password prompt on native sessions.

With Basic auth on, `/tos-config.json` is only readable by authenticated users; the endgame (server-side URL pre-signing, so browsers never hold AK/SK at all) is a later phase.
