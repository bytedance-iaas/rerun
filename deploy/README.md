# deploy — the unified rerun viewer service (web + native + server in one image) <!-- NOLINT -->

One image (`rerun-viewer-unified`) built from this repo, three modes selected by the `MODE` env var:

- `MODE=web` (default): nginx serving the wasm web viewer, plus server-side default credentials (`/tos-config.json`) and an `rrd-cache` volume (port 80).
- `MODE=native`: the Linux-native viewer on a virtual display, streamed to the browser via noVNC (port 8080) — for datasets beyond the browser's ~1.5 GB per-file limit.
- `MODE=server`: the rerun catalog server (gRPC, port 51234) with two cloud additions — `dataset.register()` / `register_prefix()` accept `tos://` URLs, and the catalog survives restarts (SQLite on the `server-data` volume). Clients use the stock SDK: `rr.catalog.CatalogClient("rerun+http://<host>:9094")`.

Both viewers support "Open from Volcengine TOS…" and "Open from Hugging Face…" (streaming LeRobot v2/v3, MCAP/file repos, single files).

The same image builds and runs both **locally** (throttled defaults survive an 8 GB Docker Desktop VM) and in the **cloud / CI** (lift the throttles, point downloads at mirrors — see below). The cloud deployment is the `dataverse` Helm chart, which bundles both components — ReRun (web viewer + catalog server) and the curation console — see [`helm/dataverse/`](helm/dataverse/README.md), plus [`helm/rerun-native-session/`](helm/rerun-native-session/README.md) for on-demand native-viewer sessions.

## Layout

| File | Purpose |
|---|---|
| `Dockerfile` | Multi-stage: wasm viewer build + native viewer build (serialized to avoid OOM) + one runtime with nginx and the Xvnc/noVNC stack. Build context is the repo root. |
| `docker-compose.yml` | Three services from the same image: `viewer` (web, `:9091`), `native-session` (native, `:9092`), `server` (catalog, `:9094`). |
| `entrypoint.sh` | `MODE` dispatch: web renders `/tos-config.json` and runs nginx; native starts Xvnc + websockify + the viewer; server runs the catalog. |
| `nginx.conf` | Static serving + `/tos-config.json` + WebDAV `PUT` on `/rrd-cache/` (phase-2 write-back) + `301 /rerun → /` (the gateway routes `/curation` to the curation console, everything else here). |
| `helm/dataverse/` | The whole cloud stack as one Helm chart bundling two components: **ReRun** (web + catalog StatefulSet with a persistent volume) and the **curation console** (its own StatefulSet, reading and writing TOS through the SDK, served under `/curation`; the viewer's Diagnose buttons deep-link into it, `?dataset=<name>`), plus the APIG gateway they share — path-routed Ingress (`/` = viewer, `/curation` = console) and a dedicated gRPC route for the catalog. `helm install dataverse deploy/helm/dataverse -f <your-values>.yaml`. |
| `helm/rerun-native-session/` | On-demand native-viewer session (one pod per user), started/torn down per session. |
| `publish_charts.sh` | Packages **both** charts (`helm/dataverse/` and `helm/rerun-native-session/`) and pushes them to the Volcengine OCI registry. Registry coordinates and the robot account come from the environment (`HELM_REGISTRY_HOST`, `HELM_REGISTRY_NAMESPACE`, `HELM_REGISTRY_USERNAME`, `HELM_REGISTRY_PASSWORD`); `DRY_RUN=1` packages and lints without logging in, `CHARTS="<name>"` restricts it to one chart. |
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
# Building the SDK needs the wheel URLs on TOS: set SDK_WHEEL_URLS in .env (see below),
# or pass BUILD_SDK_WHEEL=0 to skip the SDK entirely for a quick dev image.
docker compose up --build -d   # first build compiles both viewers: expect ~45-60 min
open http://127.0.0.1:9091     # web viewer
open "http://127.0.0.1:9092/vnc.html?autoconnect=true&resize=remote"   # native session
```

## SDK wheels (from GitHub Actions, via TOS)

The image serves the Python SDK wheels at nginx `/downloads/sdk/`, viewer bundled inside each wheel — `pip install` it and `rerun` is on PATH.
The image does **not** build any wheel itself. All platforms (Linux x64/arm64, macOS arm64, Windows x64) are built by the GitHub Actions workflow `.github/workflows/build_binary_and_wheels.yml`, uploaded to a **public-read TOS bucket**, and the Dockerfile only downloads them at build time. Two reasons over building in-image: the Actions Linux wheel is zig-linked against glibc 2.28 (`manylinux_2_28`), so it installs on much older distros than a wheel built in the bookworm container (glibc 2.36) — and the image build gets simpler and faster.

Release flow (all from the same commit, to keep the wheels and the catalog server in sync):

```bash
# 1. Trigger the cloud build: push a build-* tag on the commit you want to ship.
#    (Or, once the workflow file is on main: Actions → "Build Binaries & Wheels" → Run workflow.)
git tag build-$(date +%Y%m%d) && git push origin build-$(date +%Y%m%d)

# 2. Wait for the run (~2 h), download the rerun-sdk-wheel-* artifacts from the run's
#    Artifacts section, and unzip them — each zip holds one .whl.

# 3. Upload the wheels to the public-read TOS bucket (AWS CLI works against TOS;
#    virtual-hosted addressing; a one-time bucket policy already grants anonymous GetObject).
aws configure set default.s3.addressing_style virtual
aws s3 cp rerun_sdk-<ver>-cp310-abi3-manylinux_2_28_x86_64.whl s3://<bucket>/sdk/ \
  --endpoint-url https://tos-s3-cn-beijing.volces.com --region cn-beijing
aws s3 cp rerun_sdk-<ver>-cp310-abi3-macosx_11_0_arm64.whl s3://<bucket>/sdk/ \
  --endpoint-url https://tos-s3-cn-beijing.volces.com --region cn-beijing

# 4. Tell the image build where the wheels are, and build (from deploy/).
#    The bucket URLs are NOT hardcoded — set SDK_WHEEL_URLS (space-separated) in .env
#    (compose passes it through as a build arg), or `docker build --build-arg SDK_WHEEL_URLS=…`.
echo 'SDK_WHEEL_URLS=https://<bucket>.tos-s3-cn-beijing.volces.com/sdk/rerun_sdk-<ver>-cp310-abi3-manylinux_2_28_x86_64.whl https://<bucket>.tos-s3-cn-beijing.volces.com/sdk/rerun_sdk-<ver>-cp310-abi3-macosx_11_0_arm64.whl' >> deploy/.env
cd deploy && docker compose build
```

The wheels are **not in git** (~120 MB each, over GitHub's 100 MB file limit) and the bucket URLs are **not hardcoded** — pass them via `SDK_WHEEL_URLS` (`.env` / `--build-arg`). When building the SDK (`BUILD_SDK_WHEEL=1`, the default) `SDK_WHEEL_URLS` is **required**, and the build verifies the set before shipping it: every download must succeed and look like a real wheel (≥ 1 MB, zip magic bytes), all wheels must carry **one single version**, that version must **equal the source tree's workspace version** (root `Cargo.toml`) — catching the wheel-vs-server drift described above — and every platform tag in `SDK_WHEEL_REQUIRED_TAGS` must be covered (default `manylinux_2_28_x86_64 macosx_11_0_arm64`; set it to `none` to skip the platform check). Any violation fails the build rather than silently shipping a broken or stale `/sdk/`. Set `BUILD_SDK_WHEEL=0` to skip the SDK for a fast dev image. The bucket needs to be readable by the build host — public-read objects or pre-signed URLs both work (query strings are stripped from the saved filename). Serve whichever platforms your users need; linux-x64 + macos-arm64 is the usual minimum. Only Apple-Silicon is produced for macOS; Intel Macs are not covered (Apple has discontinued them, and upstream does not ship x64 mac wheels either).

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

Two more optional secrets gate access to the browser-facing modes (see the auth commit / `helm/dataverse/README.md`):

- `web_htpasswd` (htpasswd format) — enables nginx Basic auth for the whole web mode, including `/tos-config.json`. Without it the site (and the default credentials) is readable by anyone who can reach it — fine locally, not on a public address. `/healthz` stays open for probes.
- `session_password` / `SESSION_PASSWORD` env — enables the VNC password prompt on native sessions.

With Basic auth on, `/tos-config.json` is only readable by authenticated users; the endgame (server-side URL pre-signing, so browsers never hold AK/SK at all) is a later phase.
