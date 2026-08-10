# VKE 部署模板

| 文件 | 进 git? | 内容 |
|---|---|---|
| `rerun-cloud-template.yaml` | ✅ | 常驻服务模板(占位符):namespace + 密钥 + PVC + 一个 Deployment(web viewer 与 catalog server 两容器同 pod)+ web 的 ClusterIP service + catalog 专用公网 CLB(仅 51234) |
| `rerun-cloud.yaml` | ❌ .gitignore 已挡 | 上面模板的**已填真实密钥**版,直接 apply |
| `apig-template.yaml` | ✅ | APIG 网关实例(单 instance 多域名)+ web viewer 的 Ingress;HTTPS/域名/分流都在这层 |
| `apig.yaml` | ❌ .gitignore 已挡 | 上面模板的已填版(subnet-id) |
| `native-viewer-template.yaml` | ✅ | 单用户的云上 native viewer 会话 pod 模板(`<USERNAME>`/`<SESSION_PASSWORD>` 占位)+ ClusterIP service + 挂共享网关的 Ingress;不再建每会话公网 CLB |

## 公网入口一览(谁走哪个门)

| 入口 | 走哪 | 加密 | 认证 |
|---|---|---|---|
| web viewer | APIG 网关,固定 `*.volceapi.com` 域名 | HTTPS(平台证书) | nginx Basic auth(`web_htpasswd`) |
| native 会话 | 同一网关,每会话一个域名 | HTTPS | VNC 密码(`SESSION_PASSWORD`) |
| catalog server | 专用四层 CLB,裸 IP:51234 | 无(gRPC 过不了 APIG) | CLB IP 白名单(控制台配)+ token |

## 密钥安全

Secret 用 `stringData` 直接填**明文**(k8s apply 时自动转 base64,base64 不是加密)。
真实值只存在于 `rerun-cloud.yaml`(gitignore 已挡)和 `deploy/secrets/`,模板永远只有占位符。
从模板重新生成已填版:

```sh
cd deploy

# 首次:生成 catalog 的 token 签名密钥,存进 secrets/(gitignore 已挡)
rerun server generate-secret | tr -d '\n' > secrets/server_token_secret

sed -e "s|⚠️REPLACE_TOS_ACCESS_KEY|$(tr -d '\n' < secrets/tos_access_key)|" \
    -e "s|⚠️REPLACE_TOS_SECRET_KEY|$(tr -d '\n' < secrets/tos_secret_key)|" \
    -e "s|⚠️REPLACE_HF_TOKEN|$(tr -d '\n' < secrets/hf_token)|" \
    -e "s|⚠️REPLACE_SERVER_TOKEN_SECRET|$(tr -d '\n' < secrets/server_token_secret)|" \
    vke/rerun-cloud-template.yaml > vke/rerun-cloud.yaml
# (镜像地址、subnet-id、web_htpasswd 模板里仍是占位符,记得手动补 ——
#  web_htpasswd 可能多行,不适合 sed,直接编辑 rerun-cloud.yaml 填。)
```

`web_htpasswd` 是 web viewer 的密码表:每行「用户名:密码哈希」。生成命令会把整行(含冒号)打印出来,原样填进 Secret:

```console
$ htpasswd -nbB alice 'passwd_1'
alice:$2y$05$N9qo8uLOickgx2ZMRZoMyeIjZ…      ← 填这个(冒号在输出里)
# 没装 htpasswd 工具的等价写法:
$ printf 'alice:%s\n' "$(openssl passwd -apr1 'passwd_1')"
```

多个用户在 YAML 里用 `|` 块写法,一行一个:

```yaml
web_htpasswd: |
  alice:$2y$05$…
  bob:$apr1$…
```

加/删用户 = 改这个 Secret 再重启 web pod,不动任何云资源。

## 部署顺序(首次)

```sh
# 1. 常驻服务(namespace/secret/PVC/Deployment/service)
kubectl apply -f rerun-cloud.yaml
kubectl -n rerun get pods,svc

# 2. APIG 网关 + web Ingress
kubectl apply -f apig.yaml
kubectl get apiginstance -A            # PHASE=Running + LOADBALANCERID 即就绪
                                       # (偶发 false-empty:items 为空时重试,别急着下结论)
kubectl -n rerun get ingress -o wide   # ADDRESS = 网关 CLB 公网 IP

# 3. 查自动分配的域名(只能在控制台):
#    https://console.volcengine.com/veapig → 服务列表 → 每个 host 一行,对应一个 *.volceapi.com
#    kubectl 查不到这个域名。

# 4. 把 https://<web 的域名> 加进 TOS 桶 CORS 白名单(见下),否则浏览器打不开 tos:// 数据集。

# 5. catalog 的 IP 白名单:CLB 控制台 → rerun-cloud 那个 CLB → 访问控制 → 白名单,
#    只放行办公网/VPN 出口 IP。(四层 CLB,控制台操作,模板管不到。)
```

浏览器打开 `https://<web 域名>` → 弹用户名/密码框(`web_htpasswd` 里的账号)→ 之后体验与原来完全一致。

## ⚠️ Bucket CORS:域名固定后只配一次

以前每次 CLB 换 IP 都要重跑 CORS;上了 APIG 后 web 的域名固定,把
`https://<web 的 volceapi.com 域名>` 加进 `../enable-cors.sh` 的 `AllowedOrigin` 重跑一次即可,
以后不再变。本地 docker 的 `http://127.0.0.1:9091` 等本地 origin 保留不动。

## 常用命令

```sh
# 个人 native 会话(把 qian 换成自己的名字,密码换成自己的)
sed -e 's/<USERNAME>/qian/g' -e 's/<SESSION_PASSWORD>/我的密码/g' \
    native-viewer-template.yaml | kubectl apply -f -
kubectl -n rerun get ingress rerun-native-qian          # 等 ADDRESS
# 第一次:去 APIG 控制台查这个 host 对应的域名,然后
#   https://<域名>/vnc.html?autoconnect=true&resize=remote   (先输会话密码)
#   (resize=remote 最清晰;办公网等慢链路卡顿时加 &quality=4&compression=7)
# 用完删掉(释放节点资源;没有每会话 CLB 了,不存在忘删计费问题)
sed -e 's/<USERNAME>/qian/g' -e 's/<SESSION_PASSWORD>/x/g' \
    native-viewer-template.yaml | kubectl delete -f -
```

## 坑(APIG 相关,实测)

- **改 Ingress 的 `host` 或 `ingressClassName` 时,in-place `apply` 不干净**:ADDRESS 会残留旧值。delete + 重新 apply 该 Ingress。
- `kubectl get apiginstance` 偶尔因集群 API 抖动返回 **false-empty**(`items: []`);重试确认。
- 自动域名**只在 APIG 控制台**能看到,`kubectl` 拿不到;每个新 host 第一次要去控制台查一次。
- 集群里如果还有别的 APIG 实例,**各实例的 `ingressClasses` 千万不能重名**,否则互相抢 Ingress。
- 想在网关上配 JWT 鉴权?不要——浏览器地址栏导航不带 Bearer,页面会被挡死。认证就放后端(Basic auth / VNC 密码)。
- WebSocket(noVNC)走 HTTP/1.1,网关正常转发;**gRPC(HTTP/2)过不了网关**,catalog 必须走自己的 CLB。

## Catalog token(签发与使用)

服务端配置在「密钥安全」一节里已完成:`generate-secret` 的输出存进 `secrets/server_token_secret`,
sed 生成时自动填进 Secret 的 `server_token_secret`,apply + 重启 catalog pod 后认证即生效。
密钥只生成一次,妥善保存——**签发用户 token 用的就是这同一个密钥**,拿到它的人能签任意权限的 token。

给用户签发 token(一人一条命令;离线签,不联 server;server 也无须预先知道这些 token,它只验签名):

```sh
rerun server generate-token \
    --secret "$(cat secrets/server_token_secret)" \
    --user alice --permission read --expiration 90d \
    --server-host <CLB公网IP> \
    --server-host rerun-cloud.rerun.svc.cluster.local
```

- `--permission read`:只能查询/拉数据;`read-write` 才能注册/更新/删除。
- `--server-host` 可重复:token 只会被 SDK 发给列出的主机(公网 IP + 集群内域名都要列上),
  泄露到别的服务器也用不了。
- 到期自动失效;想立刻作废某人的 token,目前只能换密钥重签(会波及所有人),所以有效期别给太长。

用户侧(Python,集群内训练任务同样要带):

```python
client = rr.catalog.CatalogClient("rerun+http://<IP>:51234", token="<发给你的token>")
```

token 无效/缺失 → `PermissionError`;read token 做写操作 → `PermissionError`(错误信息带用户名)。
注意:token 在四层 CLB 上是明文传输的,IP 白名单这道外层锁要一直留着。

**Dataloader 免持 TOS key(预签名)**:拿着 catalog token 的用户可以完全不碰 TOS AK/SK ——

```python
lazy = ds.segment_store(segment_id, direct="presigned")
# 或整个任务:export RERUN_SEGMENT_DIRECT_READ=presigned
```

客户端先向 catalog 换每层 rrd 的限时预签名 URL(有效期 `RERUN_PRESIGN_EXPIRY_SECS`,默认 3600 秒,
server 端环境变量),然后直接对 TOS 做 range 读;签名由 server 用自己的 key 生成,客户端全程无 key。
详见 `docs/direct-segment-read.md`。

**预签名 URL 的 endpoint(`RERUN_PRESIGN_ENDPOINT`)**:签出来的 URL 用哪个 TOS 域名,决定客户端从哪条网络读。
缺省填**公网**(`…volces.com`,见 rerun-cloud.yaml),这样云外的训练/dataloader 客户端才连得上;
客户端在**同一 VPC 内**跑时改成内网(`…ivolces.com`),直读走内网更快、不计公网流量。
只影响给客户端的 URL,server 自己读 TOS 仍走 `TOS_ENDPOINT`。dataloader 直读也用这套(见
`dataloader-direct-read-test.md`)。

### 端到端自测:签 token → 用它注册 TOS 数据集

注册是写操作,token 必须签 `read-write`(`read` 的会被 `PermissionError` 拒掉):

```sh
rerun server generate-token \
    --secret "$(cat secrets/server_token_secret)" \
    --user tester --permission read-write --expiration 7d \
    --server-host <CLB公网IP> \
    --server-host rerun-cloud.rerun.svc.cluster.local
# CLB 公网 IP 查法:
#   kubectl -n rerun get svc rerun-cloud -o jsonpath='{.status.loadBalancer.ingress[0].ip}'
```

Python 侧(server 用自己 Secret 里的 TOS key 去读桶,客户端不需要任何 AK/SK):

```python
import rerun as rr

client = rr.catalog.CatalogClient("rerun+http://<CLB公网IP>:51234", token="<上面签出的token>")

ds = client.create_dataset("smoke-test", exist_ok=True)
task = ds.register(["tos://<bucket>/<path>/episode_000.rrd"])  # 必须是列表;整个目录用 register_prefix
task.wait(timeout_secs=60)

print(ds.schema())                                     # 能看到 schema = 注册成功
```

对照验证认证真的在工作:同一段代码把 token 去掉(或换成 `read` 权限的)再跑一遍,
`create_dataset` / `register` 应该抛 `PermissionError`。注册完的数据集怎么免 EIP 直读,
见上面的预签名一节和 `docs/direct-segment-read.md`。

#### 正常环境:直连 CLB 公网 IP

云内训练任务、以及**出口网络不拦 HTTP/2 的**普通环境(家用宽带、多数 VPN),直接连就行:

- 云内任务:连集群内域名 `rerun+http://rerun-cloud.rerun.svc.cluster.local:51234`,不出公网,token 的
  `--server-host` 里带上这个域名即可。
- 云外直连:连 `rerun+http://<CLB公网IP>:51234`,token 的 `--server-host` 里带上这个公网 IP。

这是设计上的常规路径,上面的示例命令按这个来即可。

#### 办公网(飞连)例外:必须走 kubectl port-forward

**现象**:在公司办公网(飞连 SealSuite/CorpLink 常驻)里,`CatalogClient(...)` 直连 CLB 公网 IP 报
`RuntimeError: verifying connection to server (Internal), transport error`。此时 `nc -vz <IP> 51234`
却显示 TCP `succeeded`,很容易误判成"端口通、是 token 或 server 的问题"。

**原因**:catalog 走的是 gRPC,底层是 **HTTP/2**;目标又是**裸公网 IP**。飞连网关会**应答 TCP 三次握手**
(所以 `nc` 通),但把上层的 HTTP/2 流量掐断——`curl --http2-prior-knowledge http://<IP>:51234/` 会看到
`Connection reset by peer`。这不是配置能绕过的,是办公网这条路本身不通。web viewer 不受影响,因为它走
APIG 的 HTTPS/HTTP1.1;唯独 catalog 的裸 IP + HTTP/2 组合触发拦截。

**被迫的办法**:用 `kubectl port-forward` 把端口转发到本地(飞连放行 kubectl 流量),客户端连
`127.0.0.1`:

```sh
# 另开一个终端常驻;飞连若劫持 kubectl 的 SOCKS 代理,前面加 env -u {,HTTP,HTTPS,ALL}_PROXY
kubectl -n rerun port-forward svc/rerun-cloud 51234:51234
```

关键坑:token 的 `allowed_hosts` 是**客户端**门禁——SDK 只把 token 发给列出的主机。改连 `127.0.0.1`
后,若签发时没带 `--server-host 127.0.0.1`,SDK 会**拒绝发送 token**,表现为 `PermissionError`(不是
transport error)。所以办公网自测要把 `127.0.0.1` 也签进去:

```sh
rerun server generate-token \
    --secret "$(cat secrets/server_token_secret)" \
    --user tester --permission read-write --expiration 7d \
    --server-host 127.0.0.1 \
    --server-host <CLB公网IP> \
    --server-host rerun-cloud.rerun.svc.cluster.local
```

```python
client = rr.catalog.CatalogClient("rerun+http://127.0.0.1:51234", token="<带 127.0.0.1 的 token>")
```

这只是办公网下的自测权宜;发给真实用户的 token 不要带 `127.0.0.1`,按上面「正常环境」的主机来签。

## 排障:看日志与调试

### 各组件日志在哪

```sh
# web(nginx)容器:entrypoint 输出(启动时会打印 "Basic auth enabled" 等)
kubectl -n rerun logs deploy/rerun-cloud -c web
# nginx 的访问/错误日志在容器内文件里(Debian 装法,不进 stdout)——
# 查"请求到没到后端、实际路径/状态码是什么"就看它:
kubectl -n rerun exec deploy/rerun-cloud -c web -- tail -20 /var/log/nginx/access.log
kubectl -n rerun exec deploy/rerun-cloud -c web -- tail -20 /var/log/nginx/error.log

# catalog 容器:token 验签失败会有 "Token verification failed" 警告(限频每秒一条)
kubectl -n rerun logs deploy/rerun-cloud -c catalog

# native 会话 pod
kubectl -n rerun logs pod/rerun-native-<name>

# 网关侧(需要查 APIG 转发行为时)
kubectl -n kube-system logs deploy/apig-controller --tail=50
```

### 定位在哪一层:port-forward 对照法

经网关的请求表现异常时,先 port-forward 直连后端做对照(走 k8s API 通道,不经网关、不受办公网拦截):

```sh
kubectl -n rerun port-forward svc/rerun-web 9091:80
curl -i http://127.0.0.1:9091/healthz     # 应 200
curl -i http://127.0.0.1:9091/            # 开了 Basic auth 应 401
```

直连对、经网关错 → 问题在网关或网络路径;直连也错 → 问题在 pod/配置。

### 实战故障速查(都踩过)

- **Service `EXTERNAL-IP` 一直 `<pending>`** → `kubectl -n rerun describe svc rerun-cloud` 看 Events。`InvalidVPC.NotFound` = subnet-id 不是本集群 VPC 的(跨集群抄了);不会自愈,改对再 apply。
- **APIGInstance 一直 Pending** → `kubectl describe apiginstance <name>` 看 Events。`cannot be found from VPC` 同上;删实例、改好、重 apply。
- **`no matches for kind "APIGInstance"`** → 集群没装 APIG 组件(`kubectl get crd | grep apig` 为空)。VKE 控制台 → 组件管理 → 安装。
- **页面能开,但 tos:// 数据集打不开 / artifacts 不命中(明明桶里有,却逐集重新下载转换)** → 九成是 CORS:当前 origin 不在桶白名单。浏览器 F12 → Network 筛 `tos-s3`:看到 "blocked by CORS policy" 就是白名单(加进 `../enable-cors.sh` 重跑,然后强刷页面);看到 403 才是 AK/SK 权限问题。
- **办公网 curl 网关公网 IP 得到奇怪的 401/404/504** → 看响应头 `Server:`。`feilian-agw` = 办公网飞连网关代答,请求根本没到我们的服务;改用 HTTPS+域名访问,或 port-forward 验证。(经 APIG 的正常响应 `server:` 是 `istio-envoy`。)
- **客户端 `PermissionError`** → 按文案分:`missing credentials`=没带 token;`bad token`/`invalid signature`=token 与 server 密钥不匹配或已过期;`not allowed for host`=签发时 `--server-host` 没列当前连接的地址。server 端对应日志:`Token verification failed`。

快速健康检查(都免认证):`curl https://<web域名>/healthz`(web)、`curl http://<catalog地址>:51234/version`(catalog,返回版本串)。
