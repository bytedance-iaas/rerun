# 测试流程:PyTorch dataloader 直读 TOS(预签名)

这份文档带你一步步验证 `rerun.experimental.dataloader`(`RerunIterableDataset` / `RerunMapDataset`)
在训练时**直接从 TOS 读 chunk 字节,而不经过 catalog server 中转**。用真实的地址和数据跑。

和 [`direct-read-demo.md`](direct-read-demo.md) 的区别:那份证明的是底层原语 `segment_store(direct=True)`;
这份针对真正的 PyTorch dataloader,它走 DataFusion 查询路径(`dataset.reader()`)。

## 原理:dataloader 的直读是"预签名"式的

- dataloader 每取一批样本,向 server 发一次查询(要 chunk 的元数据)。
- **server 对命中的每个 chunk,预签名它所在 RRD 对象的限时 URL,连同 `(offset, length)` 一起回传**
  (server 用它自己的 TOS key 签,`RERUN_PRESIGN_EXPIRY_SECS` 控制有效期)。
- 客户端凭预签名 URL **直接对 TOS 做 range 读**,把 chunk 字节拉回来;没签到的 chunk 回退 gRPC relay。
- 查询规划(latest-at、按 index 取帧、视频关键帧锚定、按实体投影)仍在 server 做,
  **只有 chunk 字节改成直连**。所以调用代码零改动 —— 你照常写 `RerunIterableDataset`。

关键点:**dataloader 端一个 TOS key 都不需要**(server 用自己的 key 签名);它只需要一个 catalog token 做认证。

## 前置准备

1. **更新过的 Python SDK**(必须):本次测试依赖两处新改动 —— server 端填预签名 URL,
   和客户端把 catalog token 传进 dataloader 的 worker 连接(否则对着认证的 server 会
   `Unauthenticated`)。在本 repo 里重新构建 SDK:

   ```sh
   pixi run py-build
   # 之后用 pixi run uvpy <脚本> 跑,确保用的是这个 .venv
   ```

2. **更新过的 server 镜像已部署**:预签名逻辑在 catalog server 里,必须是包含该改动的镜像。
   云端重出镜像后 `kubectl apply -f deploy/vke/rerun-cloud.yaml`。

3. **一个已注册的 TOS 数据集**(LeRobot 结构),用 `dataset.register("tos://…")` 注册的、带 footer 的 rrd。

4. **一个 catalog token**(`read` 权限即可 —— dataloader 只读):

   ```sh
   cd deploy
   ../target/release/rerun server generate-token \
       --secret "$(cat secrets/server_token_secret)" \
       --user tester --permission read --expiration 7d \
       --server-host 127.0.0.1 \
       --server-host <CLB公网IP> \
       --server-host rerun-cloud.rerun.svc.cluster.local
   ```

5. **catalog 可达**。办公网走 port-forward(飞连拦裸公网 IP 的 HTTP/2,详见 README 的「办公网例外」),
   并把 `127.0.0.1` 签进上面的 token:

   ```sh
   env -u HTTPS_PROXY -u https_proxy -u HTTP_PROXY -u http_proxy -u ALL_PROXY -u all_proxy \
     kubectl -n rerun port-forward svc/rerun-cloud 51234:51234
   ```

   > 预签名 URL 用哪个 endpoint 签,由 server 的 `RERUN_PRESIGN_ENDPOINT` 决定(见 rerun-cloud.yaml):
   > **缺省是公网**(`…volces.com`),所以云外/办公网客户端能直接连上签出来的 URL。
   > 客户端在**同一 VPC 内**跑时,把该 env 改成内网(`…ivolces.com`)让直读走内网(更快、不计公网流量)。
   >
   > 办公网提醒:公网 URL 从办公网 range 读大文件会被飞连限速(实测 ~100KB/s),能读通但慢;
   > 要跑真实吞吐就在云内节点跑(并把 `RERUN_PRESIGN_ENDPOINT` 设成内网)。

## 第一步:找到数据集里可用的字段

一个"字段"就是训练样本里的一列,由三段式路径 `entity_path:Archetype:component` 定位
(例:`/observation/joint_positions:Scalars:scalars`)。先把数据集里所有可选的三段路径列出来:

```python
import rerun as rr

TOKEN = "<你的 read token>"
client = rr.catalog.CatalogClient("rerun+http://127.0.0.1:51234", token=TOKEN)

# 先看有哪些数据集
for d in client.datasets():
    print("dataset:", d.name)

dataset = client.get_dataset(name="<上面某个数据集名>")
schema = dataset.schema()

print("\n可用的 index timeline(给 RerunIterableDataset 的 index=):")
for idx in schema.index_columns():
    print("  ", idx)

print("\n可用的字段(三段路径 —— 直接抄进 Field 的第一个参数):")
for col in schema.component_columns():
    if col.archetype and col.component_type:
        print(f'  {col.entity_path}:{col.archetype}:{col.component_type}')

print("\nsegments:", dataset.segment_ids()[:5], "…")
```

从打印结果里挑:
- 一个 **index timeline**(样本按它取,例如 `frame_index`、`timestamp`),填进第二步的 `index=`;
- 一到两个 **字段路径**,填进第二步的 `Field(...)`。标量列(关节角、动作)配 `NumericDecoder()`,
  压缩图配 `ImageDecoder()`,视频流配 `VideoFrameDecoder(...)`。

> **陷阱:`Field` 路径用短名,不是 schema 打印的全名。**
> schema 会打印全名 `rerun.archetypes.Scalars:rerun.components.Scalar`,但 `Field` 的路径要写短名
> `Scalars:scalars`(archetype 去掉 `rerun.archetypes.` 前缀;component 用小写复数)。
> 例:schema 里的 `/observation.state:rerun.archetypes.Scalars:rerun.components.Scalar`
> → 写成 `Field("/observation.state:Scalars:scalars", …)`。写全名会 `col()` 找不到列。

## 第二步:用 dataloader 跑一个 epoch

存成 `test_dataloader_direct.py`,把 `TOKEN`、数据集名、字段路径、index timeline 换成你的:

```python
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

TOKEN = "<你的 read token>"
client = rr.catalog.CatalogClient("rerun+http://127.0.0.1:51234", token=TOKEN)
dataset = client.get_dataset(name="<你的数据集名>")

source = DataSource(dataset=dataset)          # 不填 segments = 全部;也可 segments=[...] 限定

fields = {
    # 按第一步 schema 里的真实路径改。整数 index 用不到 timeline_sampling;
    # 时间戳 index 要传 FixedRateSampling(rate_hz=...)。
    "state": Field("<entity_path>:Scalars:scalars", decode=NumericDecoder()),
}

ds = RerunIterableDataset(
    source=source,
    index="<index timeline 名，例如 frame_index 或 timestamp>",
    fields=fields,
    fetch_size=128,
)

from torch.utils.data import DataLoader

loader = DataLoader(ds, batch_size=8, num_workers=0)   # 先 num_workers=0 跑通,再加 worker

n_samples = 0
for batch in loader:
    n_samples += next(iter(batch.values())).shape[0]
    if n_samples >= 256:      # 拉几百个样本足够验证直读了
        break

print(f"读出 {n_samples} 个样本 —— dataloader 跑通")
```

跑它:

```sh
pixi run uvpy test_dataloader_direct.py
```

能打印出样本数,说明 dataloader 连上了、认证过了、数据流出来了。**但这还没证明是"直连"**——下一步才是关键。

## 第三步:证明数据是直连 TOS,不经过 server(A/B 对比)

有个环境变量能强制走 relay:`RERUN_CHUNK_STRATEGY=grpc`(让客户端所有 chunk 走 `FetchChunks` 中转,
server 也不再生成预签名 URL)。用它做对照,量 server 网卡搬了多少字节:

```sh
# ① 直连(默认):读之前记一次 server 网卡计数
kubectl -n rerun exec deploy/rerun-cloud -c catalog -- cat /proc/net/dev
# 跑一遍 dataloader(默认直连)
pixi run uvpy test_dataloader_direct.py
# 读之后再记一次
kubectl -n rerun exec deploy/rerun-cloud -c catalog -- cat /proc/net/dev

# ② relay(对照):强制走中转,同样前后各记一次
kubectl -n rerun exec deploy/rerun-cloud -c catalog -- cat /proc/net/dev
RERUN_CHUNK_STRATEGY=grpc pixi run uvpy test_dataloader_direct.py
kubectl -n rerun exec deploy/rerun-cloud -c catalog -- cat /proc/net/dev
```

看主网卡(通常 `eth0`)那行的**发送字节数(TX)**差值:

- **直连**:server TX 只涨了查询响应那点量(元数据 + 预签名 URL,和数据量无关);
- **relay**:server TX 涨了整段 chunk 数据的体积。

两个差值一并排(例:直连 server 发了几百 KB,relay 发了几百 MB),就证明了直连时数据没过 server。

### 佐证:看 server 日志

```sh
kubectl -n rerun logs deploy/rerun-cloud -c catalog -f
```

直连时不会出现搬运 chunk 数据的 `FetchChunks` 大流量;relay(`RERUN_CHUNK_STRATEGY=grpc`)时会刷。

### 佐证:看客户端连到哪

dataloader 跑的时候,另一个终端:

```sh
lsof -nP -iTCP -a -p $(pgrep -f test_dataloader_direct) | grep ESTABLISHED
```

直连模式能看到到 TOS 桶域名(`tos-s3-…volces.com:443`)的连接;relay 模式只有到 catalog 的连接。

## 常见错误对照

- **`RuntimeError: … transport error`(连接时)**:办公网直连裸公网 IP 的 gRPC 被飞连掐了,用 port-forward + `127.0.0.1`。见 README「办公网例外」。
- **`Unauthenticated`**:token 没带上,或 SDK 不是重新 `py-build` 过的旧版(旧版 dataloader 不传 token)。确认用 `pixi run uvpy` 跑。
- **`PermissionError`**:token 的 `allowed_hosts` 不含你连的主机(port-forward 要 `127.0.0.1`),或用了没权限的 token。
- **`operation timed out` / `Connect` 失败,URL 里是 `ivolces.com`**:server 用内网 endpoint 签了 URL,
  但你的客户端在 VPC 外(办公网/家里/别的云),连不上内网域名。把 catalog 的 `RERUN_PRESIGN_ENDPOINT`
  改成公网 `https://tos-s3-cn-beijing.volces.com` 再 apply(这是缺省值,若被改过就是这个原因)。
- **数据慢但能读**:办公网限速,属正常;换云内节点跑,或只验证字节计数(招式不看速度)。
- **没看到直连、server TX 和 relay 一样大**:大概率 server 镜像还没更新到含预签名的版本,或数据不是 `tos://` 注册的(而是 gRPC 写入的内存 store —— 那种只能 relay)。

## 说明

这条 dataloader 直连和 `segment_store(direct=True)` 是同一套底层直读能力的两个入口;
dataloader 走的是"server 预签名、客户端 range 读"的形式,客户端不需要任何 TOS 凭证。
细节见 [`docs/direct-segment-read.md`](../../docs/direct-segment-read.md) 的 "The DataFusion / dataloader path"。
