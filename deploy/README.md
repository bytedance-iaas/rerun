# deploy — the unified rerun viewer service (web + native + server in one image) <!-- NOLINT -->

One image (`rerun-viewer-unified`) built from this repo, three modes selected by the `MODE` env var:

- `MODE=web` (default): nginx serving the wasm web viewer, plus server-side default credentials (`/tos-config.json`) and an `rrd-cache` volume (port 80).
- `MODE=native`: the Linux-native viewer on a virtual display, streamed to the browser via noVNC (port 8080) — for datasets beyond the browser's ~1.5 GB per-file limit.
- `MODE=server`: the rerun catalog server (gRPC, port 51234) with two cloud additions — `dataset.register()` / `register_prefix()` accept `tos://` URLs, and the catalog survives restarts (SQLite on the `server-data` volume). Clients use the stock SDK: `rr.catalog.CatalogClient("rerun+http://<host>:9094")`.

Both viewers support "Open from Volcengine TOS…" and "Open from Hugging Face…" (streaming LeRobot v2/v3, MCAP/file repos, single files).

The same image builds and runs both **locally** (throttled defaults survive an 8 GB Docker Desktop VM) and in the **cloud / CI** (lift the throttles, point downloads at mirrors — see below). Kubernetes manifests are intentionally not included here yet.

## Layout

| File | Purpose |
|---|---|
| `Dockerfile` | Multi-stage: wasm viewer build + native viewer build (serialized to avoid OOM) + one runtime with nginx and the Xvnc/noVNC stack. Build context is the repo root. |
| `docker-compose.yml` | Three services from the same image: `viewer` (web, `:9091`), `native-session` (native, `:9092`), `server` (catalog, `:9094`). |
| `entrypoint.sh` | `MODE` dispatch: web renders `/tos-config.json` and runs nginx; native starts Xvnc + websockify + the viewer; server runs the catalog. |
| `nginx.conf` | Static serving + `/tos-config.json` + WebDAV `PUT` on `/rrd-cache/` (phase-2 write-back). |
| `novnc-paste-bridge.js` | Appended into noVNC's `ui.js` at build time so Cmd+V / Ctrl+V pastes into the native session in one step. |
| `.env.example` | Non-secret settings: endpoint, region, default dataset URLs. Copy to `.env` (gitignored). |
| `gen-ca-bundle.sh` | Exports macOS keychain certs so cargo can download deps behind a corporate TLS-intercepting proxy. Optional; skip on Linux / no proxy. |
| `enable-cors.sh` | One-time CORS setup for a new TOS bucket (so the browser reads the bucket directly). |
| `run-native.sh` | Runs a host-built native viewer with credentials from `secrets/` (dev convenience). |

Credentials live in `deploy/secrets/` (`tos_access_key`, `tos_secret_key`, `hf_token`) — gitignored, mounted as docker secrets. `secrets/`, `.env`, and `ca-bundle.pem` are all gitignored.

## Local build & run

```bash
cd deploy
./gen-ca-bundle.sh             # once per machine, only behind a corporate proxy
cp .env.example .env           # then edit; create secrets/ with your AK/SK
docker compose up --build -d   # first build compiles both viewers: expect ~45-60 min
open http://127.0.0.1:9091     # web viewer
open "http://127.0.0.1:9092/vnc.html?autoconnect=true&resize=scale"   # native session
```

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

## Credential model

`entrypoint.sh` reads AK/SK/token from docker/k8s secrets (`/run/secrets/*`), falling back to `TOS_ACCESS_KEY` / `TOS_SECRET_KEY` / `HF_TOKEN` env vars. In `web` mode these are baked into `/tos-config.json` for the browser dialogs; in `native` and `server` modes they are exported into the viewer's environment. Anyone who can reach the web viewer can read `/tos-config.json` — acceptable for the PoC; a later phase replaces it with server-side URL pre-signing.
