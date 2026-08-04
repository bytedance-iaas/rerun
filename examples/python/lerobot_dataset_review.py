#!/usr/bin/env python3
"""
Load the first 5 episodes of `lerobot/aloha_static_coffee` into a Rerun catalog
and produce basic dataset-level results.

    LeRobotDataset(repo, episodes=[0..4])   # downloads parquet + av1 video from HF
        -> log each episode to its own .rrd (cameras + 14-dim state/action)
        -> compute one summary row per episode (frames, duration, motion, ranges)
        -> register the .rrds as a Rerun DATASET + create AND POPULATE a summary TABLE
        -> serve so the Viewer can browse all 5 episodes at the dataset level

Install:  pip install "rerun-sdk[datafusion]" lerobot pandas pyarrow
Run:      python lerobot_dataset_review.py
Then:     rerun rerun+http://127.0.0.1:51234

Verified on rerun-sdk 0.33.0. `aloha_static_coffee`: 50 eps, 50 fps,
14-dim state/action, 4x 480x640 cameras (~1100 frames/episode).
"""

from __future__ import annotations

import glob
import time
from pathlib import Path

import numpy as np
import pandas as pd
import pyarrow as pa
from lerobot.datasets.lerobot_dataset import LeRobotDataset

import rerun as rr
from rerun.catalog import CatalogClient

REPO = "lerobot/aloha_static_coffee"
DS = REPO.replace("/", "_")  # Rerun dataset names cannot contain "/"
N_EPISODES = 5
PORT = 51234

# --- size controls -----------------------------------------------------------
# Raw decoded RGB is huge: 480*640*3 * 4 cams ~= 3.5 MB / frame. Logging every
# frame of a ~1100-frame episode = multi-GB .rrd. Tune these down:
LOG_IMAGES = True  # set False for a tiny scalars-only .rrd (state/action plots)
IMG_STRIDE = 10  # log every Nth camera frame
IMG_DOWNSCALE = 2  # integer factor; 2 -> 240x320 thumbnails (4x smaller)

OUT = Path("aloha_episodes")
OUT.mkdir(exist_ok=True)


def to_numpy(x):
    return x.numpy() if hasattr(x, "numpy") else np.asarray(x)


def img_to_uint8_hwc(x):
    a = to_numpy(x)
    if a.ndim == 3 and a.shape[0] in (1, 3) and a.shape[2] not in (1, 3):
        a = np.transpose(a, (1, 2, 0))  # CHW -> HWC
    if a.dtype != np.uint8:
        a = (np.clip(a, 0, 1) * 255).astype(np.uint8) if a.max() <= 1.0 else a.astype(np.uint8)
    if IMG_DOWNSCALE > 1:
        a = a[::IMG_DOWNSCALE, ::IMG_DOWNSCALE]  # cheap, dependency-free downscale
    return a


def main() -> None:
    ds = LeRobotDataset(REPO, episodes=list(range(N_EPISODES)))
    fps = float(ds.fps)
    image_keys = [k for k in ds.meta.features if k.startswith("observation.images.")]
    print(f"Loaded {REPO}: {ds.num_episodes} eps, {len(ds)} frames, {fps:g} fps")
    print(f"  cameras: {[k.split('.')[-1] for k in image_keys]}  log_images={LOG_IMAGES}")

    recs, buffers = {}, {}
    for i in range(len(ds)):
        frame = ds[i]
        ep = int(to_numpy(frame["episode_index"]))
        fi = int(to_numpy(frame["frame_index"]))
        if ep not in recs:
            rec = rr.RecordingStream(application_id=DS, recording_id=f"episode_{ep:06d}")
            rec.save(str(OUT / f"episode_{ep:06d}.rrd"))
            recs[ep] = rec
            buffers[ep] = ([], [])
            print(f"  -> episode {ep}")
        rec = recs[ep]
        rec.set_time("frame_index", sequence=fi)

        if LOG_IMAGES and fi % IMG_STRIDE == 0:
            for k in image_keys:
                rec.log(f"cameras/{k.split('.')[-1]}", rr.Image(img_to_uint8_hwc(frame[k])))
        state = to_numpy(frame["observation.state"]).reshape(-1)
        action = to_numpy(frame["action"]).reshape(-1)
        for j, v in enumerate(state):
            rec.log(f"state/{j:02d}", rr.Scalars(float(v)))
        for j, v in enumerate(action):
            rec.log(f"action/{j:02d}", rr.Scalars(float(v)))
        buffers[ep][0].append(state)
        buffers[ep][1].append(action)

    rows = []
    for ep in sorted(buffers):
        S = np.asarray(buffers[ep][0])
        A = np.asarray(buffers[ep][1])
        rows.append({
            "episode": f"episode_{ep:06d}",
            "n_frames": len(S),
            "duration_s": round(len(S) / fps, 2),
            "action_motion": round(float(np.abs(np.diff(A, axis=0)).sum()), 2),
            "state_min": round(float(S.min()), 3),
            "state_max": round(float(S.max()), 3),
        })
    df = pd.DataFrame(rows)
    med = df["n_frames"].median()
    df["flag_review"] = (df["n_frames"] < 0.5 * med) | (df["n_frames"] > 1.5 * med)
    print("\n=== BASIC RESULTS (one row per episode) ===")
    print(df.to_string(index=False))

    # Register dataset + create AND POPULATE the summary table -----------------
    uris = [str(Path(p).absolute()) for p in sorted(glob.glob(str(OUT / "*.rrd")))]
    srv = rr.server.Server(port=PORT, datasets={DS: uris})
    client = CatalogClient(srv.url())

    tbl = pa.Table.from_pandas(df, preserve_index=False)
    entry = client.create_table(f"{DS}_review", tbl.schema)
    entry.append(tbl.to_batches())  # <-- the fix: write the rows, not just the schema

    print(f"\nServing {srv.url()}  | dataset '{DS}' | table '{DS}_review' ({entry.reader().count()} rows)")
    print(f"Open the Viewer:  rerun  {srv.url()}")
    print("Ctrl-C to stop.")
    try:
        while True:
            time.sleep(1)
    except KeyboardInterrupt:
        print("stopped.")


if __name__ == "__main__":
    main()
