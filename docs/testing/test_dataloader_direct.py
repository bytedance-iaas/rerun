"""验证 RerunIterableDataset 直读 TOS。"""

from __future__ import annotations

import torch.multiprocessing
# Rerun 的 tokio runtime 不是 fork-safe:DataLoader worker 必须用 spawn。
torch.multiprocessing.set_start_method("spawn", force=True)

import rerun as rr
from rerun.experimental.dataloader import (
    DataSource,
    Field,
    NumericDecoder,
    RerunIterableDataset,
)

TOKEN="eyJ0eXAiOiJKV1QiLCJhbGciOiJIUzI1NiJ9.eyJpc3MiOiJyZXJ1bi1vc3Mtc2VydmVyIiwic3ViIjoidGVzdGVyIiwiYXVkIjoicmVkYXAiLCJleHAiOjE3ODY4Mzc1MTksImlhdCI6MTc4NjIzMjcxOSwicGVybWlzc2lvbnMiOlsicmVhZCJdLCJhbGxvd2VkX2hvc3RzIjpbIjEyNy4wLjAuMSIsIjE4MC4xODQuNDYuMTA4IiwicmVydW4tY2xvdWQucmVydW4uc3ZjLmNsdXN0ZXIubG9jYWwiXX0.lQKwzgCw8Gek5LEeLVG27FBkS-UqUpfOtW5ir0i_crM"
client = rr.catalog.CatalogClient("rerun+http://127.0.0.1:51234", token=TOKEN)
dataset = client.get_dataset(name="smoke-test")

source = DataSource(dataset=dataset)

fields = {
    "state":  Field("/observation.state:Scalars:scalars", decode=NumericDecoder()),
    "action": Field("/action:Scalars:scalars",            decode=NumericDecoder()),
}

ds = RerunIterableDataset(
    source=source,
    index="frame_index",       # 整数 timeline，不需要 timeline_sampling
    fields=fields,
    fetch_size=128,
)

from torch.utils.data import DataLoader

loader = DataLoader(ds, batch_size=8, num_workers=0)

n_samples = 0
last_batch = None
for batch in loader:
    last_batch = batch
    n_samples += batch["state"].shape[0]
    if n_samples >= 200:
        break

print(f"读出 {n_samples} 个样本 —— dataloader 跑通")
if last_batch is not None:
    print("示例 state shape:", last_batch["state"].shape,
          " action shape:", last_batch["action"].shape)
else:
    print("⚠️ 一个样本都没读到 —— 检查 index/字段路径是否对,或数据集是否为空")
