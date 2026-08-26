# Direct object-store streaming — no relay server in the data path

When this viewer opens a LeRobot dataset from Volcengine TOS or Hugging Face, the viewer itself — whether it is the wasm build running in a browser tab or the native desktop build — talks **directly** to the object store.
No cloud server sits between the data and the screen.

```
        control plane (tiny)                     data plane (the gigabytes)
┌─────────────────────────────┐        ┌────────────────────────────────────────┐
│ web deployment (nginx)      │        │                                        │
│  · serves the wasm viewer   │        │   Volcengine TOS   ◄──── signed GETs ──┼── browser viewer
│  · serves config.json   │        │   (S3-compatible)                      │   (wasm)
│    (default credentials)    │        │                                        │
└─────────────────────────────┘        │   huggingface.co   ◄──── ranged GETs ──┼── native viewer
   nothing else — no proxy_pass,       │                                        │   (desktop / cloud pod)
   no data ever passes through it      └────────────────────────────────────────┘
```

## Why not a relay?

A relay server ("viewer → our server → TOS") would have to move every byte twice.

- **It becomes the bottleneck.** Every concurrent user's episode stream funnels through one server's NIC and memory. Direct connection scales like the object store does — per client, with no shared choke point.
- **It doubles the bill, then keeps growing.** Egress would be paid twice (object store → server, server → user), and the server itself must be provisioned for the *aggregate* peak bandwidth of all users. With direct reads there is exactly one egress per byte, billed to the bucket like any other S3 read, and zero relay infrastructure to size, scale, or babysit.
- **It adds a failure domain.** A relay that is down takes every dataset with it. Direct reads fail only if the store itself does.

The only servers in the picture are control-plane conveniences: on the web, nginx serves the static wasm bundle and a `config.json` with default credentials (`deploy/nginx.conf` — note: no `proxy_pass` anywhere), and locally there is no server at all — the native viewer reads `~/.rerun/config.json` instead (`crates/viewer/re_viewer/src/ui/native_config.rs`).

## How the direct connection works

**Requests are signed/authorized in the viewer process itself.**
For TOS, the viewer holds the AK/SK and computes AWS Signature V4 locally — `crates/store/re_data_source/src/tos/client.rs` is a minimal S3-compatible client built for exactly this ("on top of `ehttp`, so it runs both natively and in the browser"): `list_objects()` (`client.rs:145`) and `signed_request()` (`client.rs:186`).
For Hugging Face, the viewer calls the public Hub API with an optional `Authorization: Bearer` token — `crates/store/re_data_source/src/hf/mod.rs` (`HfStore`, the tree-listing API, and `resolve/main` ranged GETs).

**One shared streaming engine, storage-agnostic.**
Both backends implement the small `DatasetStore` trait (`crates/store/re_data_source/src/lerobot_remote.rs:101`) — `list`, `file_size`, `get_range_once` — and everything else is shared: episode queueing, pause/resume, retries, progress, and conversion.
Adding a new storage backend means implementing those three methods; the rest comes for free.

**Only the bytes you watch are downloaded.**
Data is pulled with HTTP range requests in bounded chunks (`fetch_range`, `lerobot_remote.rs:121`).
For v3 datasets, videos are not downloaded whole: the viewer fetches the mp4 index once (`fetch_video_index`, `lerobot_remote.rs:1655`), then requests only the byte extent of the episode being loaded, stitched into a sparse blob (`SparseBlob`, `lerobot_remote.rs:1454`).
Episodes stream in one by one and can be paused, reprioritized, or closed individually.

**Conversion happens client-side too.**
The fetched LeRobot files land in an in-memory VFS and are converted to Rerun data inside the viewer process (`re_importer`'s VFS layer — `crates/store/re_importer/src/lerobot/vfs.rs`), so there is no server-side transcoding step either.

**The same code compiles to every viewer.**
`re_data_source` builds both to wasm and natively; the HTTP layer is `ehttp` in the browser (the browser's own fetch, with its TLS and proxy handling) and a platform-verifier `ureq` client natively (`crates/store/re_data_source/src/http_client.rs`, which trusts the OS certificate store so corporate TLS-intercepting proxies work).
Browser, local desktop, and the cloud-hosted native session are one codebase with three packagings — the direct-connect property holds in all three.

## Prerequisites for the browser path

Browsers enforce CORS, so a bucket must opt in to being read cross-origin.
This is self-service: the web viewer asks the same-origin `/api/ensure-cors` endpoint (the catalog server) to install the rule on first contact with a bucket (`crates/store/re_data_source/src/tos/cors.rs`).
Native viewers need no CORS.

## Key code index

| What | Where |
|---|---|
| TOS S3 client, SigV4 signed in-viewer | `crates/store/re_data_source/src/tos/client.rs` (`list_objects` :145, `signed_request` :186) |
| TOS → streaming engine glue | `crates/store/re_data_source/src/tos/lerobot_stream.rs` |
| Hugging Face backend (Hub API + `resolve/main`) | `crates/store/re_data_source/src/hf/mod.rs` |
| Shared streaming engine (`DatasetStore`, ranged fetch, sparse video, pause/progress) | `crates/store/re_data_source/src/lerobot_remote.rs` |
| Native HTTP with OS trust store | `crates/store/re_data_source/src/http_client.rs` |
| Client-side LeRobot → Rerun conversion (in-memory VFS) | `crates/store/re_importer/src/lerobot/vfs.rs` |
| Viewer entry points (dialogs) | `crates/viewer/re_viewer/src/ui/open_tos_modal.rs`, `open_hf_modal.rs` |
| Web deployment serves static only (no `proxy_pass`) | `deploy/nginx.conf`, `deploy/entrypoint.sh` |
| Bucket CORS for browser direct read (self-service) | `crates/store/re_data_source/src/tos/cors.rs` |
