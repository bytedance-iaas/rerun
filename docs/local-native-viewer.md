# Local native viewer

The local native viewer is the Rerun viewer running as a plain desktop application on the user's own machine.
It needs no cloud resources: no serving deployment, no `re_server` catalog, no browser, no VNC.

It is not a separate viewer.
It is the exact same `re_viewer` core that powers the web (wasm) viewer and the cloud-hosted native viewer — the same code, packaged as a local binary.
That means every feature is shared: opening local `.rrd` files, and streaming LeRobot datasets from Volcengine TOS or Hugging Face all work identically to the web and cloud builds.

## Build

```bash
pixi run local-viewer
```

This produces `target/release/rerun`.
It is the same feature set as the release binaries, minus the embedded web viewer (which a local desktop app does not need).

If you build with `cargo` directly instead of `pixi`, the equivalent command is:

```bash
cargo build --release --package rerun-cli --no-default-features --features release_no_web_viewer
```

## Run

```bash
./target/release/rerun                    # open the viewer
./target/release/rerun path/to/file.rrd   # open a recording directly
```

To stream a remote LeRobot dataset, open the viewer and use the menu:
**Menu → Open → Open from Volcengine TOS** (or **Open from Hugging Face**), then fill in the dialog.

## Configuring default credentials (optional)

The "Open from …" dialogs always let you type in the endpoint, dataset URL, and credentials by hand, so no configuration is required.

For convenience you can pre-fill the non-secret defaults (and, if you want, the credentials) so you do not retype them every run.
On the web these defaults come from a `config.json` served next to the viewer.
The local native viewer reads the same file from your machine, in this order:

1. The path in the `RERUN_CONFIG` environment variable, if set.
2. Otherwise `config.json` in the `.rerun` directory under your home directory — the exact path depends on your OS:

| OS | Path |
|---|---|
| Linux / macOS | `~/.rerun/config.json` |
| Windows | `%USERPROFILE%\.rerun\config.json` (typically `C:\Users\<you>\.rerun\config.json`) |

Environment variables — `TOS_ENDPOINT`, `TOS_REGION`, `TOS_ACCESS_KEY`, `TOS_SECRET_KEY`, `HF_TOKEN` — override the corresponding file fields when they are set.

Example `config.json`:

```json
{
  "tos_endpoint": "https://tos-s3-cn-beijing.volces.com",
  "tos_region": "cn-beijing",
  "tos_access_key": "AK…",
  "tos_secret_key": "SK…",
  "hf_token": "hf_…",
  "tos_rrd_artifacts_url": "tos://physical-ai-rerun-test/rrd-data/",
  "rrd_artifacts_prefetch": 0
}
```

`tos_rrd_artifacts_url` is where converted rrd artifacts are stored (read + write-back), shared by all viewers.
When the key is absent the default bucket above is used; set it to `"off"` (or use `TOS_RRD_ARTIFACTS_URL=off`) to disable the artifacts store.

`rrd_artifacts_prefetch` is how many ready-made rrd artifacts to download at once when opening a dataset (also `RRD_ARTIFACTS_PREFETCH`).
`0` (or absent) picks the automatic default — 3 in the browser (its ~6-connections-per-host budget), 4 in the native viewer; explicit values are capped at 16.

All fields are optional.
Omit `tos_access_key`/`tos_secret_key` and the dialog will ask for credentials when you open a dataset.
The credential fields are never shown in the dialog unless you opt into overriding them, and they are never written back to disk by the viewer.

> Note: this file can hold secrets (`tos_secret_key`, `hf_token`).
> Keep it readable only by your user and do not commit it.
> On Linux/macOS: `chmod 600 ~/.rerun/config.json`.
> On Windows the file sits under your user profile, which other standard users cannot read — no extra step needed.

## Corporate networks: `RERUN_HTTP_KEEP_ALIVE=0`

By default the native HTTP client reuses connections between requests (normal keep-alive — saves a TLS handshake per request).
Some corporate gateways silently kill idle connections without telling either side; a request sent into such a connection stalls until its timeout, felt as sporadic multi-second hangs between downloads.
If you see that, set `RERUN_HTTP_KEEP_ALIVE=0` (values `0`/`false`/`off`/`no`): every request then opens a fresh connection, trading ~hundreds of ms of handshake per request for immunity.
