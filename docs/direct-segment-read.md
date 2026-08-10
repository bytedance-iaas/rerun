# Direct segment reads — the dataloader path without the catalog server relay

**TL;DR**: `dataset.segment_store(segment_id, direct=True)` asks the catalog server only for *metadata* (where the segment's RRDs live), then range-reads the chunk data **straight from object storage**.
Inside the cloud this means the training dataloader pulls bytes over the VPC-internal TOS endpoint — nothing flows through the catalog server's network, and nothing flows through the public CLB/EIP, whose bandwidth is billed separately.

## The problem this solves

The stock read path relays every chunk through the catalog server:

```
                     gRPC FetchChunks (all data bytes!)
dataloader  ◄────────────────────────────────  catalog server  ◄──── TOS
                    (public CLB/EIP when outside the cluster)
```

Two costs scale with data volume: the server relays every byte twice, and clients connecting over the public CLB pay EIP bandwidth for the whole dataset.

With `direct=True` the server is metadata-only:

```
dataloader  ──── gRPC: "where does segment X live?" ────►  catalog server     (tiny)
dataloader  ◄─── ranged GETs, straight from the bucket ──  TOS                (the gigabytes)
```

This mirrors how the viewers already stream LeRobot datasets (see `docs/direct-object-store-streaming.md`): the server stays in the control plane, the data plane is client ↔ object store.

## What changed

Server (`re_server`):

- The segment table used to report a synthetic `memory:///store/{slot}` as every layer's storage URL. It now reports the **registered storage URL** (`tos://…`, `s3://…`, `file://…`) and only falls back to `memory://` for sources that truly live in server memory (e.g. data written over gRPC).
  Threaded through `Source` → `Dataset::add_source` → both segment-table/manifest scans; restart-replay (persistence) records original URLs, so restored catalogs keep them.

Client (`re_redap_client`, native only):

- `ObjectStoreReader` — an async `AsyncRead + AsyncSeek` view of one object in an S3-compatible store (or a local file), where each read is one ranged `GET`. Slots into the existing RRD decoding stack unchanged.
- `DirectSegmentChunkProvider` — a `ChunkProvider` that opens every layer RRD of a segment at its storage URL, reads the RRD **footer manifest** (chunk index with byte offsets), and serves `load_chunks` as coalesced range reads. Multi-layer segments are merged exactly like the server's `GetRrdManifest`, so downstream code cannot tell the two providers apart.
- `ConnectionClient::get_segment_layer_urls` — the one metadata call: `(layer name, storage url)` pairs for a segment, from `/ScanSegmentTable`.

Python (`rerun_py`):

- `DatasetEntry.segment_store(segment_id, *, direct=None)` — `True` selects the direct provider; `None` (default) defers to the `RERUN_SEGMENT_DIRECT_READ` env var; `False`/unset keeps today's relayed path.

## Usage

```python
import rerun as rr

client = rr.catalog.CatalogClient("rerun+http://<catalog-host>:51234")
ds = client.get_dataset(name="my_dataset")

for segment_id in ds.segment_ids():
    lazy = ds.segment_store(segment_id, direct=True)   # ← the only change
    for chunk in lazy.stream().to_chunks():
        …  # feed your training batch
```

Or flip the default for a whole training job without touching code:

```sh
export RERUN_SEGMENT_DIRECT_READ=1
```

Credentials/endpoint for the direct connection come from the environment, same variables as everywhere else in this deployment (`TOS_ENDPOINT`, `TOS_REGION`, `TOS_ACCESS_KEY`, `TOS_SECRET_KEY`; the standard `AWS_*` variables also work, e.g. for MinIO).
**In-cluster training jobs should set `TOS_ENDPOINT` to the VPC-internal endpoint** (`https://tos-s3-cn-beijing.ivolces.com`) — that is what keeps the traffic off the public network entirely.

Requirements:

- The segment's RRDs must live at a `tos://` / `s3://` / `file://` URL — i.e. registered by URL, not written over gRPC. Anything registered through `dataset.register("tos://…")` / `register_prefix` qualifies.
- The RRDs must have a footer (any RRD produced by current SDKs/converters does). Footer-less legacy RRDs fail with a clear error and must use the relayed path.
- `direct=True` fails loudly when these aren't met — it never silently falls back, so you always know which path your bytes took.

## Verification

Unit / integration (no network, no credentials):

```sh
cargo test -p re_redap_client                       # reader + provider over file:// and in-memory stores
cargo test -p re_server --features lance --test redap_tests   # server suite incl. storage-url reporting
pixi run uvpy -m pytest -c rerun_py/pyproject.toml \
    rerun_py/tests/e2e_redap_tests/test_segment_store.py      # e2e: direct == relay, chunk for chunk
```

Against real TOS (needs `TOS_*` credentials):

1. Start any catalog server with the `TOS_*` env set, `dataset.register("tos://bucket/path/file.rrd")`, wait for the task.
2. Open the same segment twice — `segment_store(seg)` and `segment_store(seg, direct=True)` — and compare `schema()`, chunk ids, and row counts: they must match exactly.
3. Confirm the bytes bypass the server: watch the server's network I/O (or its logs — no `FetchChunks` calls appear for the direct read), or point `TOS_ENDPOINT` at the internal endpoint from an in-cluster pod and watch the EIP traffic graphs stay flat while the dataloader streams.

## Not in scope (yet)

- The DataFusion path (`dataset.reader()`) still relays through the server; the protocol's `chunk_direct_urls` slot is the designated route for it later.
- The client still holds TOS credentials. The follow-up ("pre-signed URL" phase) moves signing server-side so dataloaders hold no keys at all; this feature's metadata flow is designed to grow into that.
