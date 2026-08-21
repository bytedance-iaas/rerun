# 部署指南

本文把整套 Rerun 云服务部署到一个火山引擎 VKE 集群,使用 Helm chart(`deploy/helm/`)。
全程约 30 分钟,其中等云资源(CLB、网关)就绪约 10 分钟。

## 1. 前提条件

**集群侧**(缺任何一项先找集群管理员):

- 一个 VKE 集群,kubectl 已能连上;
- 集群装有 APIG 的 `APIGInstance` CRD(VKE 平台组件)。验证 — 确认存在这一行,且 apiVersion 是 chart 使用的 `loadbalancer.vke.volcengine.com/v1beta1`:

  ```console
  $ kubectl api-resources | grep -i apiginstance
  apiginstances    apig    loadbalancer.vke.volcengine.com/v1beta1    false    APIGInstance
  ```

  只有别的 apiVersion(或 grep 无输出)都算不满足,先找集群管理员装/升级 APIG 组件;
  (质检台不再需要 fsx CSI 驱动:数据面已改为 TOS SDK 直连,不挂载任何桶。)
- 一个当前集群 VPC 内的子网 ID(给 CLB / 网关用)。查法 — 抄本集群任一正常 LoadBalancer Service 的注解:

  ```sh
  kubectl get svc -A -o jsonpath='{range .items[*]}{.metadata.name}{"\t"}{.metadata.annotations.service\.beta\.kubernetes\.io/volcengine-loadbalancer-subnet-id}{"\n"}{end}' | grep subnet-
  ```

**资源侧**:

- 一个火山引擎账户,已开通 TOS 服务,具备读写权限,容量足够存放数据集;
- 该账户下一对有 TOS 读写权限的 AK/SK;
- 一个 rrd 缓存桶(viewer 写转换产物,可与数据桶同一个);
  (质检台不需要预留专门的桶:数据集来源与交付去向都是用户在界面上运行时
  填的 tos:// 路径,AK/SK 对哪些桶有权限就能用哪些桶。)
- rerun 镜像和 robot_curator 镜像的仓库地址。

**本机工具**:kubectl、helm(≥ 3.8)、openssl。

## 2. 准备配置

### 2.1 获取代码

```sh
git clone https://github.com/bytedance-iaas/rerun.git
cd rerun
git checkout <分支名> # like: release_v1
```

后续命令都在这个仓库根目录执行。

### 2.2 生成密钥

整套部署涉及三种凭证,作用和来源各不相同,别混:

| 凭证 | 用途 | 来源 |
|---|---|---|
| token 签名密钥 | catalog server 验证用户 token 用的"印章":server 拿它验签,给用户签发 token 也用它(见第 5 节) | 本节生成并直接写入集群 Secret |
| web 登录账号表 | 浏览器打开 web viewer / 质检台时的用户名密码(存的是密码哈希) | 本节生成并直接写入集群 Secret |
| 火山引擎 AK/SK | 各组件读写 TOS 对象存储的云账户凭证 | 火山引擎控制台(第 1 节资源侧) |

特别注意:**token 签名密钥和火山引擎 AK/SK 是两回事**。
前者只在本产品内部用(签发/验证 catalog token),后者是云厂商的账户凭证,两者互填部署都起不来且报错不直观。

所有密钥都用 kubectl 直接建成集群里的 Secret,helm 只引用名字,密钥不出现在 values 文件和 helm 发布记录里。
本机磁盘上只留一份 gitignore 挡住的 AK/SK 文件(方便多条命令引用);token 签名密钥全程不落盘,只存在于集群中,签发 token 也在集群内完成(第 5 节)。

先构建 rerun 二进制,生成签名密钥要用它(已有 `./target/release/rerun` 就跳过这步):

```sh
cargo build --release --package rerun-cli --no-default-features --features release_no_web_viewer
```

定下部署用的 namespace 并创建(后文所有命令都引用这个变量,新开终端要重新 export):

```sh
export RERUN_NS=rerun   # 换成你要的名字;同一集群装第二套时换个名字即可互不干扰
kubectl create namespace $RERUN_NS
```

把 AK/SK 和火山方舟API KEY读进当前终端的环境变量:

```
export  TOS_ACCESS_KEY="##############"
export TOS_SECRET_KEY=="##############"
export ARK_API_KEY=="##############"
```

创建两个 Secret(名字与下一节 values 里的引用一一对应):

```sh
# 1) 应用凭证:AK/SK + 登录账号表。
#    AK/SK 是各组件访问 TOS 的凭证:web viewer 靠它让浏览器直读数据集并回写
#    rrd 缓存,catalog server 靠它读桶和给训练侧签预签名 URL,native 会话同样复用。
#    账号表的 key 名叫 web_htpasswd,指的是它的【格式】(Apache 密码表:每行
#    「用户名:密码哈希」,nginx 和质检台都认这个格式验证),不需要 htpasswd 工具,
#    哈希由 openssl 现场生成,账号表只存在于这个 Secret 里。
#    示例建两个账号 —— alice 密码 pwd123,bob 密码 pwd456(换成你要的;
#    访问 HF 私有数据集才需要再加 --from-literal=hf_token=<token>)。
#    ark_api_key 是质检台走火山方舟 VLM 后端用的 key(配套 base url 是普通配置,
#    在 2.3 的 daft.arkBaseUrl);不用方舟(比如用自托管 vLLM)就删掉那一行:
kubectl -n $RERUN_NS create secret generic rerun-cloud-secrets \
    --from-literal=tos_access_key="$TOS_ACCESS_KEY" \
    --from-literal=tos_secret_key="$TOS_SECRET_KEY" \
    --from-literal=ark_api_key="<火山方舟 API key>" \
    --from-literal=web_htpasswd="$(printf '%s\n%s\n' \
        "alice:$(openssl passwd -apr1 'pwd123')" \
        "bob:$(openssl passwd -apr1 'pwd456')")"

# 2) token 签名密钥:管道直建,不落盘、不进 shell 历史。只生成这一次;
#    它在集群里只会以 0400 权限的文件挂进 catalog 容器,web / native 会话都看不到。
./target/release/rerun server generate-secret | kubectl -n $RERUN_NS \
    create secret generic rerun-catalog-server-secrets --from-file=server_token_secret=/dev/stdin
```

### 2.3 写 values 文件

创建 `deploy/secrets/values-prod.yaml`(目录不存在先 `mkdir -p deploy/secrets`;该目录已被 gitignore 挡住),按注释替换尖括号里的值:

```yaml
image:
  rerun: <rerun 镜像,例 iaas-us-cn-beijing.cr.volces.com/physicalai/rerun:TAG>
  curator: <robot_curator 镜像>

network:
  subnetId: <第 1 节查到的 subnet-xxx>

tos:
  rrdArtifactsUrl: tos://<rrd 缓存路径>

secrets:
  existingSecret: rerun-cloud-secrets       # = 2.2 建的两个 Secret 之一
  existingTokenSecret: rerun-catalog-server-secrets
```

密钥已在 2.2 建进集群,这里只引用 Secret 名字 — **values 文件里没有任何密钥**。
后续增删登录账号见第 7 节。

质检台(daft)的参数都带默认值,一般不用在这份文件里写:火山方舟 base url
(`daft.arkBaseUrl`,默认北京)、CPU/内存与 `ephemeral-storage` 体积预检额度
(`daft.resources`)等。要改就直接编辑 [`deploy/helm/rerun-cloud/values.yaml`](../../deploy/helm/rerun-cloud/values.yaml)
里对应项(那里有逐项注释),或按需在本文件里覆盖同名键。
方舟的 API key 是密钥,走 2.2 的 `ark_api_key`,不在这里配。

缺省区域不是 cn-beijing、或要用其他开关(不部署质检台、内网 CLB、外部 Secret 等),
参数全集和默认值见 [`deploy/helm/rerun-cloud/values.yaml`](../../deploy/helm/rerun-cloud/values.yaml) 的注释。

## 3. 安装

namespace 和密钥都已在 2.2 就位,安装就一条命令:

```sh
helm install rerun-cloud deploy/helm/rerun-cloud \
    -n $RERUN_NS \
    -f deploy/secrets/values-prod.yaml
```


## 4. 部署后步骤

### 4.1 等资源就绪

```sh
kubectl -n $RERUN_NS get pods
# 预期:rerun-cloud-0 2/2 Running,rerun-cloud-curation-0 1/1 Running

kubectl get apiginstance -n $RERUN_NS
# 等 PHASE=Running(偶发返回空列表,重试确认,不要据此断言实例不存在)
```

### 4.2 查网关域名

自动分配的 `*.volceapi.com` 域名只能在 APIG 控制台看,kubectl 查不到:
[console.volcengine.com/veapig](https://console.volcengine.com/veapig) → 服务列表,找 host 为 `rerun-cloud-web.apig.internal` 的行,对应的域名就是整套服务的**唯一公网入口**:
web viewer(`/`)、质检台(`/curation`)、catalog 的 gRPC(网关按路径分流到 51234,客户端连 `rerun+https://<域名>:443`)、SDK 下载(`/downloads/`)全在这一个域名上。
查到后存进环境变量,后文引用:

```sh
export GW_DOMAIN=<控制台查到的域名>   # 例 xxxx.apigateway-cn-beijing.volceapi.com
```

### 4.3 TOS 桶 CORS(自动,一般无需操作)

浏览器直读 TOS 需要桶上有放行 viewer 域名的 CORS 规则(浏览器的跨域安全机制),否则打不开 `tos://` 数据集。
本产品**自动处理**:web viewer 每次打开一个桶,会先经同域 `/api/ensure-cors` 请 catalog server 检查该桶的 CORS,缺我们的规则就补上(只追加,不覆盖桶上别家的规则)。
所以运行时用新桶、甚至新建的桶,都不需要预先配置 — 前提是 AK/SK 具备桶管理权限。

需要了解的三点:

- 自动配置写入的放行名单是**通配符**(`https://*.apigateway-<region>.volceapi.com`),覆盖该区域网关分配的所有域名,重装换域名也不用重来;
- 不希望服务自动改桶配置时,values 里 `catalog.autoCors.enabled=false` 关闭;
- 重装换域名后浏览器若报跨域/Failed to fetch,先换无痕窗口试:无痕能开 = 桶没问题,是浏览器缓存了旧域名时代的响应(清缓存硬刷新即可,见 [03-test.md](03-test.md) 排查一节)。

**手动后备**(关闭了自动配置、或 AK/SK 无桶管理权限时):

```sh
# AK/SK 环境变量沿用 2.2 的 tos-keys.env(新开终端重新 source 即可),
# 对每个需要浏览器直读的桶执行,参数格式 = <桶名>.<TOS S3 公网域名>
set -a; source deploy/secrets/tos-keys.env; set +a
deploy/enable-cors.sh curation.tos-s3-cn-beijing.volces.com physical-ai-rerun-test.tos-s3-cn-beijing.volces.com <其他桶…>
```

也可在 TOS 控制台手动配置,方法(GET/HEAD/PUT)和 ExposeHeader 照抄脚本里的 XML,origin 务必用通配符。

### 4.4 catalog 的入口与安全

catalog 的 gRPC 走网关同一个域名(TLS 加密 + token 认证),**没有单独的公网入口**,无需额外配置。
token 的签发见第 5 节。

### 4.5 验证

浏览器打开 `https://<域名>`,用 2.2 建的账号登录,应看到 viewer 界面;
`https://<域名>/curation` 应看到质检台(共用账号表,免再登录)。
完整验证清单见 [03-test.md](03-test.md)。

## 5. 签发 catalog token

用户访问 catalog server 需要 token。
签发在 **catalog 容器内**执行 — 签名密钥只存在于集群里(0400 文件),不出集群;因此**能签发 token 的人 = 有该 namespace `kubectl exec` 权限的人**,由集群 RBAC 管控:

```sh
kubectl -n $RERUN_NS exec rerun-cloud-0 -c catalog -- sh -c \
    "rerun server generate-token --secret \"\$(cat /run/secrets/server_token_secret)\" \
        --user zhang --permission read --expiration 90d \
        --server-host $GW_DOMAIN \
        --server-host rerun-cloud-headless.$RERUN_NS.svc.cluster.local"
```

- `--permission` 取 `read` 或 `read-write`(注册数据集需要后者);
- `--server-host` 是允许用这个 token 连的地址,可多个:网关域名给云外客户端,集群内域名给云内训练任务;自测要走 port-forward 的再加 `--server-host 127.0.0.1`;
- 用户侧先装 SDK:部署自带分发点,浏览器开 `https://$GW_DOMAIN/downloads/sdk/` 看 wheel 文件名,`pip install "https://<用户名>:<密码>@$GW_DOMAIN/downloads/sdk/<wheel 文件名>"` — 与在跑的 server 出自同一次构建,版本天然一致;
- 用法:云外 `CatalogClient("rerun+https://$GW_DOMAIN:443", token=<token>)`,云内 `CatalogClient("rerun+http://rerun-cloud-headless.<ns>.svc.cluster.local:51234", token=<token>)`。

安全边界说明:能读该 namespace Secret 或能 exec 进 pod 的人依然拿得到签名密钥 — 这一层靠收紧 namespace 的 RBAC(最小授权)兜底;0400 文件挡的是密钥被 pod 内非属主进程误读和被顺手带出。

## 6. 云上 native viewer 会话(按需)

前提:第 3 节的 rerun-cloud 已装好(会话复用它的凭证和网关)。

```sh
# 开一个会话,release 名 = 用户名(小写字母/数字/中划线)。
# 直接复用 2.3 的 values 文件(镜像、缓存桶、凭证 Secret 全部沿用),
# 只需另给一个会话密码:
helm install <unique_name> deploy/helm/rerun-native-session -n $RERUN_NS \
    -f deploy/secrets/values-prod.yaml \
    --set sessionPassword=<会话密码>

# 域名同 4.2 的查法(host = rerun-native-qian.apig.internal),浏览器打开:
#   https://<域名>/vnc.html?autoconnect=true&resize=remote
# 输入会话密码,即进入云上原生 viewer 的远程桌面。

# 用完删掉(占着节点资源):
helm uninstall <unique_name> -n $RERUN_NS
```

不想走公网:加 `--set ingress.enabled=false`,然后

```sh
kubectl -n $RERUN_NS port-forward pod/rerun-native-qian 9092:8080
# 浏览器打开 http://127.0.0.1:9092/vnc.html?autoconnect=true&resize=scale
```

## 7. 升级、变更与卸载

升级(改了 values 之后执行):

```sh
helm upgrade rerun-cloud deploy/helm/rerun-cloud -n $RERUN_NS \
    -f deploy/secrets/values-prod.yaml
```

改密钥(密钥在集群 Secret 里,不归 helm 管;chart 看不到内容,改完必须手动重启生效)。
以增删登录账号为例:重跑 2.2 的建 Secret 命令,带上**完整的目标账号列表**(该命令就是账号表的唯一权威来源),末尾加 `--dry-run=client -o yaml | kubectl apply -f -` 变成覆盖写,然后重启。
注意这是**整份覆盖**:命令里没列的 key 会被一并抹掉,所以 2.2 建过的可选 key(`ark_api_key`、`hf_token` 等)这次也要照带,漏了就等于删掉。

```sh
# 先确保 AK/SK 环境变量已就位:set -a; source deploy/secrets/tos-keys.env; set +a
kubectl -n $RERUN_NS create secret generic rerun-cloud-secrets \
    --from-literal=tos_access_key="$TOS_ACCESS_KEY" \
    --from-literal=tos_secret_key="$TOS_SECRET_KEY" \
    --from-literal=ark_api_key="<火山方舟 API key>" \
    --from-literal=web_htpasswd="$(printf '%s\n%s\n%s\n' \
        "alice:$(openssl passwd -apr1 'pwd123')" \
        "bob:$(openssl passwd -apr1 'pwd456')" \
        "carol:$(openssl passwd -apr1 'pwd789')")" \
    --dry-run=client -o yaml | kubectl apply -f -

kubectl -n $RERUN_NS rollout restart statefulset rerun-cloud rerun-cloud-curation
```

卸载:

```sh
helm uninstall rerun-cloud -n $RERUN_NS
# kubectl 直建的 Secret 不归 helm 管,如需彻底清理:
# kubectl -n $RERUN_NS delete secret rerun-cloud-secrets rerun-catalog-server-secrets
```

注意:

- catalog 的数据盘 PVC(`server-data-rerun-cloud-0`)卸载时**不会删除**,注册记录都在;确认不要了再手动删;
- 不要改 `apig.webHost` 和 ingressClass:改动需删 Ingress 重建,且换 host 等于换公网域名(CORS、用户书签全要跟着换)。

## 8. 从旧 kubectl 模板部署迁移

已经用 `kubectl apply -f deploy/vke/rerun-cloud.yaml` 部署过的环境,迁移步骤见
[`deploy/helm/rerun-cloud/README.md`](../../deploy/helm/rerun-cloud/README.md) 的"从 kubectl 模板部署迁移"一节。
要点:资源名和 selector 不同,须删旧建新;catalog 数据盘 PVC 同名,保留即可无缝复用;网关域名会重新分配。
