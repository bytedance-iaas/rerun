# Demo:证明 dataloader 直连 TOS、不经过 catalog server

这份文档教你一步步做一场**无可辩驳**的演示:训练读数据这条路只向 catalog server 问一句"数据在哪",
然后把 GB 级的数据**直接从 TOS 对象存储**拉走,一个字节都不经过 server。

它偏演示、偏"取信于人",不是日常操作,所以单独成篇,不塞进常规 README。
功能本身的设计与 API 见 [`docs/direct-segment-read.md`](../../docs/direct-segment-read.md);
token / 部署 / 排障见 [`README.md`](README.md)。

### 关于 "dataloader" 这个词

这里沿用 `direct-segment-read.md` 的说法,"dataloader" 指**训练时拉数据这条路**,是个概念,不是某个具体 class。
演示直接调用它的核心原语 `ds.segment_store(segment_id, direct=True)` ——
这正是训练代码为了拿一个 segment 的 chunk 会发的那一个调用;在这一层证明了直连,建立在它之上的读取循环也就都直连。

一个要说明的点:SDK 里现成的 PyTorch 模块 `rerun.experimental.dataloader`(`RerunIterableDataset` 等)
走的是另一条 DataFusion 查询路径(`dataset.reader()`)。这条路**也已经支持直连**了 ——
server 给查询命中的每个 chunk 预签名并回传 `(url, offset, length)`,客户端直接对 TOS range 读,
没签到的 chunk 回退 relay;查询规划仍在 server 做,所以直连零改调用代码。
对 TOS 数据集,这条直连在**催更过这项支持的 server 镜像**上生效(见 `direct-segment-read.md` 的
"The DataFusion / dataloader path")。本演示用的 `segment_store(direct=True)` 是同一套直连的底层原语,
证明的是直连这件事本身;dataloader(`RerunIterableDataset`)的直读有单独的分步测试文档:
[`dataloader-direct-read-test.md`](dataloader-direct-read-test.md)。

## 为什么"断了 server 还能读"就等于直连

这场演示的底气来自代码事实,先讲清楚,演示才站得住:

- `ds.segment_store(seg, direct=True)` 在**这一行之内**做完所有对 server 的访问 ——
  问一次"这个 segment 的 rrd 在 TOS 的哪个位置"(一次 `ScanSegmentTable` 元数据调用),
  拿到 URL 后直接对 TOS 读每个 rrd 的 footer,建好一个"直连 provider"。
- 这一行返回后,handle 里握着的只有指向 TOS 的 reader,**没有任何 server 连接对象**。
  之后每次读 chunk 都是对 TOS 的一次带 Range 的 `GET`。
- 相对地,relay 模式(`direct=False`)的 provider **一直握着 gRPC 连接**,
  每个 chunk 都走 `FetchChunks` 经过 server 中转。

所以:**元数据拿到之后把 server 整个断掉,直连照样能读完;relay 立刻就断。**
这一正一反就是最强的证据 —— 数据若真经过 server,server 没了必然失败。

## 前置准备

1. **一个已注册 TOS 数据的数据集**。
   按 [`README.md`](README.md) 的「端到端自测」注册一个,或用已有的。
   要求:rrd 是用 `dataset.register("tos://…")` 注册的(不是 gRPC 写入的),且带 footer(现行 SDK/转换器产出的都带)。

2. **一个 read-write 或 read 的 catalog token**(读数据 read 就够)。
   办公网测试要把 `127.0.0.1` 签进 `--server-host`,原因见 README 的「办公网例外」。

3. **catalog server 可达**。办公网走 port-forward(飞连拦裸公网 IP 的 HTTP/2,详见 README):

   ```sh
   env -u HTTPS_PROXY -u https_proxy -u HTTP_PROXY -u http_proxy -u ALL_PROXY -u all_proxy \
     kubectl -n rerun port-forward svc/rerun-cloud 51234:51234
   ```

4. **本机有 TOS 凭证**(仅 `direct=True` 需要;`direct="presigned"` 完全不需要 —— server 帮你签名)。
   直连读 TOS 的凭证从**环境变量**来,不是从 viewer 的 `~/.rerun/config.json`:

   ```sh
   export TOS_ENDPOINT=https://tos-s3-cn-beijing.volces.com   # 办公网用公网 endpoint
   export TOS_REGION=cn-beijing
   export TOS_ACCESS_KEY=<AK>
   export TOS_SECRET_KEY=<SK>
   ```

   > 云内训练任务应把 `TOS_ENDPOINT` 设成 VPC 内网 endpoint(`https://tos-s3-cn-beijing.ivolces.com`),
   > 这才是让流量彻底不出公网的关键;办公网机器用公网 endpoint。

## 招式一(主秀):断掉 server,直连照读不误

把下面存成 `demo_direct.py`,填好 `TOKEN`:

```python
import rerun as rr

TOKEN = "<你的 token>"
client = rr.catalog.CatalogClient("rerun+http://127.0.0.1:51234", token=TOKEN)
ds = client.get_dataset(name="smoke-test")     # 换成你的数据集名
seg = ds.segment_ids()[0]

# ① 唯一联系 server 的一步:拿到 TOS 位置 + 读 footer,建好直连 handle。
lazy = ds.segment_store(seg, direct=True)
print(f"manifest 已在本地:{len(lazy)} 个 chunk;此刻已物理加载 {lazy._chunks_loaded} 个(应为 0)")

# ② 在这里暂停,去另一个终端把 server 断掉(见下)。
input(">>> 现在去断掉 server,然后回车继续 …")

# ③ server 已死。数据从 TOS 流出来 —— 全部读完。
store = lazy.stream().collect()
print(f"读取完成:物理加载了 {lazy._chunks_loaded} / {len(lazy)} 个 chunk")
print("server 已不在,数据依旧读出 —— 字节来自 TOS,不经过 catalog server。")
```

跑它:

```sh
python3 demo_direct.py
```

停在 `input(...)` 时,**在另一个终端把 server 断掉**,二选一:

- **最简单**:回到跑 port-forward 的终端,按 `Ctrl-C`。
  所有到 server 的路都归它,掐了它 = server 对客户端彻底消失。
- **最震撼**:把整个 pod 缩到 0 副本 —— server 真的没了。

  ```sh
  kubectl -n rerun scale deploy/rerun-cloud --replicas=0
  kubectl -n rerun get pod -w      # 看着 pod 变 Terminating / 消失
  ```

回到 Python 终端按回车。你会看到它把所有 chunk 读完,`_chunks_loaded` 从 0 变成满值 —— **server 已经不在了**。

### 对照组:让观众看到 relay 会死

把上面脚本里 `direct=True` 改成 `direct=False`,其余不变,重跑一遍、同样断掉 server 后回车。
这次 `lazy.stream().collect()` 会**立刻抛连接错误**(relay 每个 chunk 都要经过已死的 server)。
一个读得出、一个读不出,结论无可争辩。

### 演示后恢复

```sh
kubectl -n rerun scale deploy/rerun-cloud --replicas=1   # 若用了缩容
# pod Running 后,重新起 port-forward(上面那条命令)
```

## 招式二(量化):数 server 网卡搬了多少字节

直连时 server 只搬几 KB 元数据,relay 时 server 搬整段 GB 级数据 —— 数字最硬。
在读之前和读之后各看一次 catalog 容器的网卡计数:

```sh
# 读之前
kubectl -n rerun exec deploy/rerun-cloud -c catalog -- cat /proc/net/dev
# （在 Python 里跑一次完整的 direct=True stream().collect()）
# 读之后
kubectl -n rerun exec deploy/rerun-cloud -c catalog -- cat /proc/net/dev
```

看主网卡(通常 `eth0`)那行的收发字节数:
`direct=True` 前后几乎不动(只有元数据);同一个 segment 换 `direct=False` 再跑一次,发送字节会涨满整段体积。
把两个差值并排一放(例:直连 server 发了 ~3 KB,relay server 发了 1.2 GB),不用多解释。

## 招式三(客户端视角):看连接连到哪

读数据时,直连模式的客户端会直接和 **TOS 桶的域名**建连接,而不是 server。
`stream()` 进行中,在另一个终端:

```sh
lsof -nP -iTCP -a -p $(pgrep -f demo_direct) | grep ESTABLISHED
```

`direct=True`:能看到到 `tos-s3-cn-beijing.volces.com:443` 的连接(数据在这儿流),
到 catalog(`127.0.0.1:51234`)只有一个早已结束的元数据连接。
`direct=False`:只有到 catalog 的连接,全程不碰 TOS。

## 招式四(最省事):看 server 日志

```sh
kubectl -n rerun logs deploy/rerun-cloud -c catalog -f
```

`direct=True` 的读只出现一次 `ScanSegmentTable`(元数据),**不出现** `FetchChunks`;
`direct=False` 的读会刷出 `FetchChunks`。

## 进阶:连 TOS 凭证都不给客户端(presigned)

想更进一步证明"客户端连 TOS 的 key 都不需要",把 `direct=True` 换成 `direct="presigned"`,
并且**不设** `TOS_ACCESS_KEY` / `TOS_SECRET_KEY`:

```python
lazy = ds.segment_store(seg, direct="presigned")
```

客户端拿 catalog token 向 server 换来"限时、限单个对象"的预签名 URL,再凭 URL 里的签名直接读 TOS;
签名是 server 用它自己的 key 生成的,客户端全程无 key。
数据依旧不经过 server(招式二/三/四同样成立),但这次连凭证都省了。细节见 `docs/direct-segment-read.md`。

## 给演示的建议

主打**招式一**(断了 server 还在出数据,视觉冲击最强),配**招式二**给一个量化数字。
一个定性、一个定量,基本堵死所有质疑。
