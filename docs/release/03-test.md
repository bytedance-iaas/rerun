# 验证手册

前提:[02-deploy.md](02-deploy.md) 已走完,`RERUN_NS` 和 `GW_DOMAIN`(网关域名)已 export,手里有登录账号。
顺序:先做准备(第 1 节),再跑冒烟(第 2 节,约 15 分钟);冒烟全过说明环境健康,需要逐项验收时再做第 3 节;出问题查第 4 节。

## 1. 准备

### 1.1 测试数据

上传一个 LeRobot 数据集(v2 或 v3)到部署 AK/SK 有权限的任一 TOS 桶下
(桶和前缀随意,质检台与 viewer 都是运行时按 tos:// 路径直连):

```
tos://<桶>/<前缀>/<数据集名>/
```

同一个路径 viewer 直读、Diagnose 跳转质检台都用它。
上传用 TOS 控制台或 tosutil 均可,保持数据集原目录结构。

### 1.2 Python 客户端(SDK)

SDK 由**部署自己分发**:wheel 和 catalog server 出自同一次镜像构建,从部署装到的 SDK 永远与在跑的 server 版本一致。

```sh
# 1) 浏览器打开 https://<网关域名>/downloads/sdk/(用登录账号),页面列出 wheel 文件名
# 2) 装进你的 Python(≥3.10)环境:
pip install "https://<用户名>:<密码>@<网关域名>/downloads/sdk/<wheel 文件名>"
# 3) 跑训练直读(3.5)的 dataloader 用例还需:pip install torch
```

说明:`/downloads/sdk/` 下同时提供 **Linux x86_64** 和 **macOS(Apple Silicon)** 两种 wheel,`pip install` 会按你的机器自动挑对应那颗;升级部署后重装同一 URL 即拿到新版。
viewer 已随 wheel 一起分发——装完 SDK 后 `rerun` 命令直接可用(见 3.7),无需单独下载。
其他平台(Windows、Intel Mac、arm64 Linux)暂不提供预编译 wheel,本地开发可源码构建(`pixi run py-build`,之后用 `pixi run uvpy <脚本.py>` 跑;代理做 TLS 中间人的网络先 `export SSL_CERT_FILE=<CA 证书包.pem>`)。

### 1.3 签测试 token

签两枚,一枚读写、一枚只读(对照认证用例要用)。
签发在 catalog 容器内执行,签名密钥不出集群(见 02-deploy 第 5 节):

```sh
kubectl -n $RERUN_NS exec rerun-cloud-0 -c catalog -- sh -c \
    "rerun server generate-token --secret \"\$(cat /run/secrets/server_token_secret)\" \
        --user tester --permission read-write --expiration 7d \
        --server-host $GW_DOMAIN --server-host 127.0.0.1 \
        --server-host rerun-cloud-headless.$RERUN_NS.svc.cluster.local"
# 再跑一遍,--permission read,存成只读 token
```

`127.0.0.1` 是为 port-forward 兜底场景签的(见 1.4);发给真实用户的 token 不要带它。

### 1.4 办公网须知

catalog 现在走网关域名(TLS + HTTP/2),与 web viewer 同一条通道 —— 办公网(飞连)对"域名 + TLS"是放行的,所以**优先直连** `rerun+https://$GW_DOMAIN:443` 试。
若确实连不上(飞连策略因网点而异),退回 port-forward 兜底,另开一个终端常驻:

```sh
env -u HTTPS_PROXY -u https_proxy -u HTTP_PROXY -u http_proxy -u ALL_PROXY -u all_proxy \
  kubectl -n $RERUN_NS port-forward svc/rerun-cloud-headless 51234:51234
```

之后 Python 里连 `rerun+http://127.0.0.1:51234`(token 须含 `127.0.0.1`,见 1.3)。

## 2. 冒烟测试

按顺序执行,每步的预期都写在旁边;任何一步不符合,跳到第 4 节对号入座。

**2.1 集群状态**

```sh
kubectl -n $RERUN_NS get pods
# 预期:rerun-cloud-0 2/2 Running,rerun-cloud-curation-0 1/1 Running
kubectl get apiginstance -n $RERUN_NS
# 预期:PHASE=Running(偶发空列表是集群 API 抖动,重试)
```

**2.2 免认证健康端点**

```sh
curl -s -o /dev/null -w '%{http_code}\n' https://$GW_DOMAIN/healthz   # 预期 200(web)
curl -s https://$GW_DOMAIN/version                                     # 预期返回版本串(catalog,经网关)
```

**2.3 web viewer 登录**:浏览器打开 `https://<网关域名>`,弹出登录框;错密码被拒,alice/pwd123 登录后看到 viewer 界面。

**2.4 质检台通行**:同一浏览器打开 `https://<网关域名>/curation`,**不再弹登录框**,直接看到质检台页面。

**2.5 打开数据集**:viewer 里输入 1.1 的 `tos://<桶>/<前缀>/<数据集名>/`,数据集加载,拖动时间轴,视频与曲线同步回放。

**2.6 缓存加速**:刷新页面重新打开同一数据集,加载时间明显短于首次(直接下载现成 rrd,跳过转换)。

**2.7 catalog 连通**(云外直连网关域名;连不上再按 1.4 走 port-forward 并换 `rerun+http://127.0.0.1:51234`):

```python
import rerun as rr
client = rr.catalog.CatalogClient("rerun+https://<网关域名>:443", token="<读写 token>")
print([d.name for d in client.datasets()])   # 预期:正常返回(首次为空列表)
```

**2.8 native 会话**:按 02-deploy 第 6 节开一个会话,`https://<会话域名>/vnc.html?autoconnect=true&resize=remote` 输会话密码后进入远程桌面,测完删除。

## 3. 功能验证

每条用例:步骤 → **预期**。

### 3.1 数据集直读(web viewer)

- 打开 `tos://…` 的 LeRobot v2 数据集与 v3 数据集各一个 → **均能加载回放**(两个版本代码路径不同,都要覆盖)。
- 打开一个**从未配置过 CORS 的桶**里的数据集 → **能直接加载**(viewer 自动请 server 补配桶 CORS);catalog 日志出现 `auto-CORS: installed viewer rule on bucket <桶名>`,TOS 控制台可见该桶多了一条含通配 origin 的 CORS 规则,且桶上原有的规则原样保留。
- 通过 viewer 的 HuggingFace 入口打开一个公开数据集(例:`lerobot/pusht`)→ **能加载**(流量走 hf-mirror 镜像)。
- 打开一个不存在的路径(如 `tos://<桶>/no-such/`)→ **明确报错,界面不挂死**。

### 3.2 rrd 缓存

- 首次打开数据集后,查看 rrd 缓存桶(`values` 里 `tos.rrdArtifactsUrl` 指向的位置)→ **出现新的 rrd 产物对象**。
- 二次打开 → **明显加速**(冒烟 2.6 已覆盖,此处确认产物确实来自缓存桶)。
- 删掉该数据集的缓存产物再打开 → **退回首次的转换流程,完成后产物重新出现**(缓存只是加速,删了不丢数据)。
- **离线预转换**(`rerun rrd-convert`):在装了 SDK、配好 `~/.rerun/tos-config.json`(含 TOS 凭证与 `tos_rrd_artifacts_url` 缓存桶)的机器上,对一个**未打开过、缓存桶里没有产物**的数据集跑:

  ```sh
  rerun rrd-convert tos://<桶>/<路径>/<数据集名>/
  ```

  → **逐个 episode 转换并写回缓存桶**,缓存桶里出现产物;随后在 web viewer 首次打开该数据集就**直接命中缓存、秒开**(不再现场转换)。再跑一遍同一命令 → **各 episode 被跳过**(同源指纹已最新,只发 HEAD 探测),几乎瞬间完成。`--artifacts-url tos://桶/前缀/` 可覆盖输出缓存桶;`hf://org/name` 同理转换 HuggingFace 数据集。

### 3.3 认证与账号

- 无痕窗口直接开 `https://<域名>/` 与 `/curation` → **都弹登录框**;alice、bob **都能登录两处**(共用一份账号表)。
- 错误密码 → **401 再次弹框**;`/healthz` 无凭证 → **200**(探针豁免)。
- catalog 三连,用 2.7 的脚本改造:
  - 不带 token → **`PermissionError`(missing credentials)**;
  - 只读 token 执行注册(3.4 的脚本)→ **`PermissionError`**(错误信息带用户名);
  - 用没把当前地址签进 `--server-host` 的 token → **`PermissionError`(not allowed for host)**,SDK 直接拒发。

### 3.4 catalog 注册、查询与持久化

读写 token 端到端(rrd 路径用 3.2 里缓存桶产物的,或任一 TOS 上的 rrd):

```python
import rerun as rr
client = rr.catalog.CatalogClient("rerun+http://<地址>:51234", token="<读写 token>")
ds = client.create_dataset("smoke-test", exist_ok=True)
task = ds.register(["tos://<桶>/<路径>/<某个>.rrd"])   # 参数必须是列表;整目录用 register_prefix
task.wait(timeout_secs=60)
print(ds.schema())        # 能打印 schema = 注册成功
```

- 上述脚本 → **注册成功,`client.datasets()` 里能看到 `smoke-test`**。
- 重启验证持久化:`kubectl -n $RERUN_NS delete pod rerun-cloud-0`,等重新 2/2 Running 后再查 → **`smoke-test` 仍在,schema 正常**(注册记录在云盘上,不随 pod 走)。

### 3.5 训练直读(预签名)

- 拿 3.4 注册的数据集,验证客户端**不设任何 TOS AK/SK 环境变量**:

  ```python
  seg = ds.segment_ids()[0]
  lazy = ds.segment_store(seg, direct="presigned")   # server 签限时 URL,客户端免 key
  store = lazy.stream().collect()
  print(f"读取 {lazy._chunks_loaded}/{len(lazy)} 个 chunk")
  ```

  → **全部读完**;字节走的是预签名 URL 直连 TOS。
- 想验证"数据真的不经过 server":拿到 `lazy` 之后把 server 断掉(`kubectl -n $RERUN_NS delete pod rerun-cloud-0`),再 `collect()` → **照常读完**。完整剧本(含 relay 模式对照)见 [`docs/testing/direct-read-demo.md`](../testing/direct-read-demo.md)。
- PyTorch dataloader(`RerunIterableDataset`)一路的直读分步验证见 [`docs/testing/dataloader-direct-read-test.md`](../testing/dataloader-direct-read-test.md)。
- 办公网提醒:预签名 URL 指向 TOS 公网 endpoint,办公网读大文件会被限速到 ~100KB/s,能读通但慢;吞吐测试放云内跑。

### 3.6 Daft 质检联动

- viewer 里打开 1.1 上传的 TOS 数据集,点 **Diagnose** → **跳到
  `/curation?dataset=tos://…&region=…`,免登录,「数据集 TOS 路径」和
  「数据集地区」已自动填好**(并出现一条"已按链接填好"的提示)。
- 补上「输出 TOS 路径 + 地区 + 交付名」(输出可以指向 AK/SK 有权限的任意桶),
  质检范围选「快速质检」、「只跑前 N 条」填 2,点开始 → 任务日志依次出现
  `[tos] 下载 …`、质检各阶段进度、`[tos] 交付已上传`。
- TOS 控制台查看 输出路径/<交付名>/<时间戳>/ → **交付物/报告在,且
  `passed.json` 在其余对象之后出现**(完整性标志最后传的协议;质检功能本身
  属 Daft 侧,这里只验证联动与直连数据面通畅)。

### 3.7 native viewer

- 云上会话:开一个会话打开超出浏览器内存的大数据集(web viewer 会提示改用 native 的那种)→ **能加载回放**;拉 TOS 走内网,速度应明显好于本地。
- 本地 native viewer:**随 SDK 分发,不再单独下载**。按 1.2 装好 wheel(Linux 或 macOS)后,`rerun` 命令即可用 —

  ```sh
  rerun            # 直接开 viewer 窗口
  ```

  本机 `~/.rerun/tos-config.json` 配好 TOS 凭证后打开同一 `tos://` 地址 → **能加载**(数据走公网)。升级 = 重装同一 wheel URL。Windows / Intel Mac / arm64 Linux 暂需源码构建。

## 4. 常见问题排查

### 4.1 部署起不来

| 症状 | 原因与处理 |
|---|---|
| APIGInstance 一直 Pending | `describe apiginstance` 看 Events;`cannot be found from VPC` / `InvalidVPC.NotFound` = `apig.subnetIds` 里某个子网抄了别的集群的,改对重装,不会自愈 |
| `no matches for kind "APIGInstance"` | 集群没装 APIG 组件,VKE 控制台 → 组件管理 → 安装 |
| `kubectl get apiginstance` 返回空列表 | 集群 API 偶发抖动,重试确认,别据此断言实例不存在 |
| 质检跑批开头就报「缺少 TOS 凭证」 | curation 容器没拿到 `TOS_ACCESS_KEY/TOS_SECRET_KEY`:确认 `secrets.existingSecret` 指的 Secret 里有 `tos_access_key/tos_secret_key` 两个 key |
| 质检下载阶段报桶/前缀不存在或无权限 | 界面填的 tos:// 路径写错,或部署那对 AK/SK 对该桶没有权限(质检台能访问哪些桶完全由这对密钥决定) |

### 4.2 浏览器侧

| 症状 | 原因与处理 |
|---|---|
| 页面能开,`tos://` 数据集打不开;或缓存明明有却逐集重新转换 | 先看是不是 CORS:F12 → Network 筛 `tos-s3`,看到 "blocked by CORS policy" 说明自动配置没生效 — 查 catalog 日志里的 `auto-CORS failed`(常见:AK/SK 无桶管理权限、`catalog.autoCors.enabled=false`、网关缺 `/api` 路由),按 02-deploy 4.3 的手动后备处理;看到 403 才是 AK/SK 数据权限问题 |
| 网关重建换域名后一律 "Failed to fetch",curl 却全通 | Chrome 把旧域名时代的 CORS 头连文件一起缓存了;DevTools → 右键刷新按钮 → 清空缓存并硬性重新加载(或换无痕窗口) |
| 视频区域黑屏、曲线正常 | 浏览器 VideoDecoder 需要 HTTPS 安全上下文;必须走网关域名访问,http + 裸 IP 不行 |
| 办公网 curl 网关得到奇怪的 401/404/504 | 看响应头 `Server:`,`feilian-agw` = 飞连代答,请求没到服务;正常经 APIG 的响应是 `istio-envoy` |
| 大数据集加载中页面崩溃 | wasm 内存上限(不足 4 GB);换 native viewer 会话 |
| `/curation` 下静态资源/WS 全 404 | curator 镜像太旧,不支持子路径;换新镜像 |
| `/curation` 全部 401,正确密码也进不去 | htpasswd 挂载坏了(鉴权 fail-closed 锁死);`kubectl -n $RERUN_NS logs rerun-cloud-curation-0` 找 "没有可用账号" |

### 4.3 catalog / token

`PermissionError` 按错误文案分流:

- `missing credentials` — 没带 token;
- `bad token` / `invalid signature` — token 与 server 密钥不匹配,或已过期;
- `not allowed for host` — 签发时 `--server-host` 没列当前连接的地址(port-forward 场景最常见,见 1.3)。

server 侧对应日志:`kubectl -n $RERUN_NS logs rerun-cloud-0 -c catalog` 里的 `Token verification failed`。

### 4.4 办公网连 catalog 失败

catalog 常规路径是网关域名(TLS),办公网一般放行;若报 `transport error`:

- 连的是 `rerun+https://<域名>:443` 吗?`rerun+http` 或裸 IP 都会被办公网(飞连)掐 —— 它拦"裸 IP + 明文 HTTP/2",且 `nc` 探端口是通的,极具迷惑性;
- token 的 `--server-host` 是否包含所连的地址(错配是 `PermissionError`,不是 transport error);
- 仍不通就按 1.4 的 port-forward 兜底(目标 `svc/rerun-cloud-headless`),token 须含 `127.0.0.1`。

### 4.5 日志位置速查

```sh
kubectl -n $RERUN_NS logs rerun-cloud-0 -c web            # web 容器(启动时打印认证状态)
kubectl -n $RERUN_NS exec rerun-cloud-0 -c web -- tail -20 /var/log/nginx/access.log   # 实际请求/状态码
kubectl -n $RERUN_NS logs rerun-cloud-0 -c catalog        # catalog(验签失败有告警)
kubectl -n $RERUN_NS logs rerun-cloud-curation-0          # 质检台
kubectl -n kube-system logs deploy/apig-controller --tail=50   # 网关控制器
```

定位在哪一层的通用手法 — port-forward 直连后端做对照(不经网关):

```sh
kubectl -n $RERUN_NS port-forward svc/rerun-cloud-web 9091:80
curl -i http://127.0.0.1:9091/healthz   # 应 200;/ 应 401(Basic auth)
```

直连对、经网关错 → 问题在网关或网络路径;直连也错 → 问题在 pod 或配置。
