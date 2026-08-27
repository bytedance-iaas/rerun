# 部署指南

本文把整套 Rerun 云服务部署到一个火山引擎 VKE 集群,使用 Helm chart(`deploy/helm/`)。
常驻服务是 `dataverse` chart —— 一个 chart 打包 **ReRun**(web viewer + catalog server)与
**质检台**两个组件,以及它们共用的 APIG 网关入口;
按需的 native viewer 会话是另一个 chart `rerun-native-session`(第 5 节)。
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
- （仅当**新建独立 APIG 网关**时,见 2.4)当前集群 VPC 内的子网 ID,给新网关及其前置 CLB 用;网关多副本高可用,建议备 2 个【不同可用区】的子网。复用已有网关(2.3 推荐做法)不需要子网。查法 — 抄本集群任一正常 LoadBalancer Service 的注解:

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
凭证只进当前终端的环境变量和集群 Secret,不落盘;token 签名密钥同样全程不落盘,只存在于集群中,签发 token 也在集群内完成(第 5 节)。

先构建 rerun 二进制,生成签名密钥要用它(已有 `./target/release/rerun` 就跳过这步):

```sh
cargo build --release --package rerun-cli --no-default-features --features release_no_web_viewer
```

定下部署用的 namespace 并创建(后文所有命令都引用这个变量,新开终端要重新 export):

```sh
export DATAVERSE_NS=dataverse   # 换成你要的名字;同一集群装第二套时换个名字即可互不干扰
kubectl create namespace $DATAVERSE_NS
```

把 AK/SK、火山方舟 API KEY 和 web 登录账号读进当前终端的环境变量(后文命令都引用这些变量,新开终端要重新 export):

```sh
export TOS_ACCESS_KEY="##############"
export TOS_SECRET_KEY="##############"
export ARK_API_KEY="##############"

# web viewer / 质检台的登录账号:
export WEB_USER="##############"
export WEB_PASS="##############"

# catalog 的 token 签名密钥:generate-secret 打印一串明文,和其他密钥同等保存。
# 只生成这一次并妥善保管;谁拿到它谁就能签任意权限的 token。
export SERVER_TOKEN_SECRET="$(./target/release/rerun server generate-secret)"
```

创建一个 Secret(整套部署的全部密钥都在这一个里,名字与下一节 values 里的引用对应):

```sh
# 各 key 的含义:
# - tos_access_key / tos_secret_key:各组件访问 TOS 的凭证。web viewer 靠它让浏览器
#   直读数据集并回写 rrd 缓存,catalog server 靠它读桶和给训练侧签预签名 URL,
#   native 会话同样复用。
# - server_token_secret:catalog 的 token 签名密钥(上面 generate-secret 生成的明文)。
# - web_htpasswd:登录账号表。key 名指的是它的【格式】(Apache 密码表:
#   「用户名:密码哈希」,nginx 和质检台都认这个格式验证),不需要 htpasswd 工具,
#   哈希由 openssl 现场生成,账号表只存在于这个 Secret 里。
# - ark_api_key:质检台走火山方舟 VLM 后端用的 key(配套 base url 是普通配置,
#   在 2.3 的 curator.arkBaseUrl);不用方舟(比如用自托管 vLLM)就删掉那一行。
# - 访问 HF 私有数据集才需要再加 --from-literal=hf_token=<token>。
kubectl -n $DATAVERSE_NS create secret generic dataverse-secrets \
    --from-literal=tos_access_key="$TOS_ACCESS_KEY" \
    --from-literal=tos_secret_key="$TOS_SECRET_KEY" \
    --from-literal=server_token_secret="$SERVER_TOKEN_SECRET" \
    --from-literal=ark_api_key="$ARK_API_KEY" \
    --from-literal=web_htpasswd="$WEB_USER:$(openssl passwd -apr1 "$WEB_PASS")"
```

### 2.3 写 values 文件

创建一个 values 文件,按注释替换尖括号里的值。
放哪里、叫什么都随意 — 文件里没有任何密钥(见下),不怕落盘;
本文档后续命令统一以 `deploy/secrets/values-prod.yaml` 为例
(这个目录已被 gitignore 挡住,放仓库里也不会误提交;目录不存在先 `mkdir -p deploy/secrets`),
用别的路径就把后文命令里的 `-f` 参数跟着换掉:

```yaml
# 完整镜像地址(必须带 tag):chart 不跟踪镜像版本,tag 每次构建都不同,必须显式给。
image:
  rerun: <仓库地址>/rerun:<tag>
  curator: <仓库地址>/robot_curator:<tag>

apig:
  # 复用集群里已有的 APIG 网关(推荐):create 保持 false,填平台实例 id —— APIG 控制台
  # https://console.volcengine.com/veapig → 实例列表里那一串。
  # 复用时不需要 subnetIds(网关和它的前置 CLB 都已存在);网关声明的 ingressClass
  # 会在安装时自动从集群反查出来,不用填(id 填错会直接安装报错,并附上列出
  # 集群里所有网关的命令)。只有离线渲染(helm template 不连集群)才需要显式
  # 加一行 ingressClassName: <该网关声明的 class>。
  create: false
  existingId: <apig 实例 id>

  # —— 或者:新建独立网关(见 2.4)。改用这种方式时,把 create 改成 true、
  #    删掉 existingId(留着会写死 CRD 的不可变字段 spec.id,后续所有 upgrade 都会被拒),
  #    放开下面的 subnetIds(第 1 节查到的,建议 2 个不同可用区的子网):
  # create: true
  # subnetIds:
  #   - <subnet-xxx（可用区 A）>
  #   - <subnet-yyy（可用区 B）>

tos:
  # 只填 region:公网/内网 TOS endpoint 都由它推导
  # (https://tos-s3-<region>.volces.com 与 .ivolces.com),不需要手写。
  region: cn-beijing
  rrdArtifactsUrl: tos://<rrd 缓存路径>

secrets:
  existingSecret: dataverse-secrets       # = 2.2 建的那个 Secret
```

密钥已在 2.2 建进集群,这里只引用 Secret 名字 — **values 文件里没有任何密钥**。
chart 不接受明文密钥输入,也不会自己渲染 Secret:helm 的 release 记录会原样保存 values,
`helm get values` 就能读回来,所以这两个 Secret 名字是必填项,缺了会在渲染阶段直接报错。
后续增删登录账号见第 7 节。

集群里还没有 APIG 网关、或就是要一个独立网关,改用新建方式,见 **2.4**。

区域不是 cn-beijing(改 `tos.region` 一处即可)、或要用其他开关(不部署质检台、presign 走内网等),
参数全集和默认值见 [`deploy/helm/dataverse/values.yaml`](../../deploy/helm/dataverse/values.yaml) 的注释。

### 2.4 改为新建独立 APIG 网关(可选)

集群里还没有 APIG 网关,或想给本部署一个**独立**网关(独立前置 CLB、独立公网域名,按量计费),
就用 2.3 `apig` 段里那几行注释掉的备选:**`create: true`,删掉 `existingId`,放开 `subnetIds`**。

- `subnetIds` 至少一个;网关多副本高可用,建议给 2 个【不同可用区】的子网(第 1 节查到的),
  平台把网关副本和前置 CLB 摊到多可用区,且必须是**本集群 VPC** 的子网(抄别的集群的会一直 Pending)。
- `create: true` 时 `existingId` 必须留空:新网关的 id 会写在 APIGInstance 的 `status.id` 里,
  Ingress 按 ingressClass 认领,不需要回填。填了就等于写 CRD 的不可变字段 `spec.id`,
  之后每次 upgrade 都会被准入 webhook 拒绝(`spec.id: Forbidden: forbidden to update`)。
  chart 会在渲染时直接拦下这个组合。
- ⚠️ 这样建出来的网关会被 `helm uninstall` 一起删掉,自动分配的 `*.volceapi.com` 域名随之失效;
  想保住它,**在卸载之前**先 `apig.retainOnDelete=true` 跑一次 upgrade。

### 2.5 启用自带的 vLLM 后端(可选)

质检台的模型检查需要一个 VLM 后端,默认走火山方舟(2.2 的 `ark_api_key`,已配好)。
chart 还内置了一套 **vLLM 部署**,默认关闭(`vllm.enabled: false`,因为要 GPU);
打开后它会自动以 `self-hosted` 为名注册进质检台的后端列表,界面里直接可选,不用手抄任何地址。

前提:集群里有 NVIDIA GPU 节点(`nvidia.com/gpu` 资源可分配)。
默认参数已按"32B 模型 + 单张 96GB GPU"调好(显存利用率、上下文长度等,见 values 里 `vllm.extraArgs` 的注释)。

vLLM 本体用的是**公开镜像**(`vllm/vllm-openai`,chart 默认已配好走 daocloud 代理,不用填);
模型权重默认由一个下载容器在 vLLM 启动前**经火山内网预取**到 GPU 节点本地盘(下载容器镜像自动复用 `image.curator`,同样不用填)。
所以 values 文件里只需要:

```yaml
vllm:
  enabled: true

  # 以下都可不配。默认跑 Cosmos-Reason2-32B,需要单张 96GB 显存的卡。

  # 可选:换模型 —— 两行一起改(下面以 8B 为例,单张 24GB 卡就能跑):
  # weightFetch:
  #   modelName: Cosmos-Reason2-8B             # 下载哪个模型(加载路径自动跟着它)
  # servedModelName: nvidia/Cosmos-Reason2-8B  # API 对外报的模型名

  # 可选:钉到指定 GPU 节点。不填则调度器自己挑 —— 注意它只认 GPU【数量】不认显存,
  # 集群里混着不同显存的卡时,建议钉到显存够的节点上:
  # nodeHostname: <节点的 kubernetes.io/hostname>
```

要点:

- 权重落在 GPU 节点本地盘(emptyDir),不进镜像也不占 TOS;pod 重建时下载是幂等的,已完整就秒过。
  节点本地盘要留出模型体量的空间(32B fp16 约 64GB)。
- 大模型加载慢是正常的,就绪探针给了约 10 分钟窗口;`kubectl -n $DATAVERSE_NS get pods` 看到 vllm pod Ready 即可用。
- 显存不够(OOM)或嫌上下文太小:`gpuCount: 2`,并在 `extraArgs` 里加 `--tensor-parallel-size` 和 `"2"` 两行。
  GPU 节点有污点或要挑卡型,用 `vllm.tolerations` / `vllm.nodeSelector`。
- 部署完成后再开也一样:改 values 后 `helm upgrade`(第 7 节),质检台自动滚动,后端列表里出现 `self-hosted`。
- 内网预取(oniond + ivolces 镜像源)只在火山云内可用。集群不在火山云时改从 HuggingFace 直拉
  (走默认的 hf-mirror 镜像;私有模型再往 2.2 的 dataverse-secrets 里补一个 hf_token key):

  ```yaml
  vllm:
    enabled: true
    weightFetch:
      enabled: false                        # 关掉内网预取,vLLM 自己拉
    model: nvidia/Cosmos-Reason2-32B        # 改写 HF repo id(默认是本地路径)
    servedModelName: nvidia/Cosmos-Reason2-32B
  ```

另外,如果你**已有现成的** vLLM(或任何 OpenAI 兼容)推理服务,不想让 chart 再拉一套,
就不开 `vllm.enabled`,把服务地址登记进 `curator.vlmBackends`(质检台没有自动发现,不登记就选不到):

```yaml
curator:
  vlmBackends:
    my-vllm:                                  # 界面里显示的后端名,自己起
      endpoint: http://<服务地址>:8000/v1      # OpenAI 兼容地址,以 /v1 结尾,须从质检台 pod 可达
      model: <模型名>                          # 与该服务的 --served-model-name 一致
```

两者可并存;`vlmBackends` 里出现同名 `self-hosted` 条目时,以你写的为准。

## 3. 安装

namespace 和密钥都已在 2.2 就位,安装就一条命令:

```sh
helm install dataverse deploy/helm/dataverse \
    -n $DATAVERSE_NS \
    -f deploy/secrets/values-prod.yaml
```


## 4. 部署后步骤

### 4.1 等资源就绪

```sh
kubectl -n $DATAVERSE_NS get pods
# 预期:rerun-cloud-0 2/2 Running,dataverse-curation-0 1/1 Running

kubectl get apiginstance -n $DATAVERSE_NS
# 等 PHASE=Running(偶发返回空列表,重试确认,不要据此断言实例不存在)
```

### 4.2 查网关域名

自动分配的 `*.volceapi.com` 域名只能在 APIG 控制台看,kubectl 查不到:
[console.volcengine.com/veapig](https://console.volcengine.com/veapig) → 服务列表,找 host 为 `dataverse-web.apig.internal` 的行,对应的域名就是整套服务的**唯一公网入口**:
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

**手动后备**(关闭了自动配置、或 AK/SK 无桶管理权限时):在 TOS 控制台给桶手动加一条 CORS 规则,内容照抄自动配置写的那条:

- 允许来源(AllowedOrigin):`https://*.apigateway-<region>.volceapi.com`(务必用通配符;本地 docker 调试再加 `http://127.0.0.1:9091`);
- 允许方法:GET、HEAD、PUT、DELETE;允许 Header:`*`;
- 暴露 Header(ExposeHeader)必须逐个列全:`ETag`、`Content-Range`、`Content-Length`、`x-amz-meta-rerun-fingerprint`、`x-amz-meta-rerun-source-url` —— 少了指纹那个,rrd 缓存查询会全部静默失效,表现为明明有缓存却每次重新转换。

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
kubectl -n $DATAVERSE_NS exec rerun-cloud-0 -c catalog -- sh -c \
    "rerun server generate-token --secret \"\$(cat /run/secrets/server_token_secret)\" \
        --user zhang --permission read --expiration 90d \
        --server-host $GW_DOMAIN \
        --server-host rerun-cloud-headless.$DATAVERSE_NS.svc.cluster.local"
```

- `--permission` 取 `read` 或 `read-write`(注册数据集需要后者);
- `--server-host` 是允许用这个 token 连的地址,可多个:网关域名给云外客户端,集群内域名给云内训练任务;自测要走 port-forward 的再加 `--server-host 127.0.0.1`;
- 用户侧先装 SDK:部署自带分发点,浏览器开 `https://$GW_DOMAIN/downloads/sdk/` 看 wheel 文件名,`pip install "https://<用户名>:<密码>@$GW_DOMAIN/downloads/sdk/<wheel 文件名>"` — 与在跑的 server 出自同一次构建,版本天然一致;
- 用法:云外 `CatalogClient("rerun+https://$GW_DOMAIN:443", token=<token>)`,云内 `CatalogClient("rerun+http://rerun-cloud-headless.<ns>.svc.cluster.local:51234", token=<token>)`。

安全边界说明:能读该 namespace Secret 或能 exec 进 pod 的人依然拿得到签名密钥 — 这一层靠收紧 namespace 的 RBAC(最小授权)兜底;0400 文件挡的是密钥被 pod 内非属主进程误读和被顺手带出。

## 6. 云上 native viewer 会话(按需)

前提:第 3 节的 dataverse 已装好(会话复用它的凭证和网关)。

```sh
# 会话密码只能来自 Secret,chart 不接受明文输入(理由同 2.2:values 会原样进 release 记录)。
# 先建这个会话专属的 Secret,key 必须是 session_password:
kubectl -n $DATAVERSE_NS create secret generic <unique_name>-vnc \
    --from-literal=session_password=<会话密码>

# 开一个会话,release 名 = 用户名(小写字母/数字/中划线)。
# 直接复用 2.3 的 values 文件(镜像、缓存桶、凭证 Secret 全部沿用),
# 只需另给会话密码 Secret 的名字:
helm install <unique_name> deploy/helm/rerun-native-session -n $DATAVERSE_NS \
    -f deploy/secrets/values-prod.yaml \
    --set existingPasswordSecret=<unique_name>-vnc

# 域名同 4.2 的查法(host = rerun-native-qian.apig.internal),浏览器打开:
#   https://<域名>/vnc.html?autoconnect=true&resize=remote
# 输入会话密码,即进入云上原生 viewer 的远程桌面。

# 用完删掉(占着节点资源),密码 Secret 一并清掉:
helm uninstall <unique_name> -n $DATAVERSE_NS
kubectl -n $DATAVERSE_NS delete secret <unique_name>-vnc
```

不想走公网:加 `--set ingress.enabled=false`,然后

```sh
kubectl -n $DATAVERSE_NS port-forward pod/rerun-native-qian 9092:8080
# 浏览器打开 http://127.0.0.1:9092/vnc.html?autoconnect=true&resize=scale
```

## 7. 升级、变更与卸载

升级(改了 values 之后执行):

```sh
helm upgrade dataverse deploy/helm/dataverse -n $DATAVERSE_NS \
    -f deploy/secrets/values-prod.yaml
```

改密钥(密钥在集群 Secret 里,不归 helm 管;chart 看不到内容,改完必须手动重启生效)。
用 `kubectl patch` 只改要改的 key,**其他 key 原样保留** —— 千万不要重跑 2.2 的 create 命令覆盖写:整个部署的密钥都在这一个 Secret 里,覆盖写会把没列出的 key(尤其 `server_token_secret`)一并抹掉,catalog 认证会当场失效、所有已发 token 作废。
以改登录账号/密码为例:

```sh
export WEB_USER="##############"
export WEB_PASS="##############"
kubectl -n $DATAVERSE_NS patch secret dataverse-secrets --type merge \
    -p "{\"stringData\":{\"web_htpasswd\":\"$WEB_USER:$(openssl passwd -apr1 "$WEB_PASS")\"}}"

kubectl -n $DATAVERSE_NS rollout restart statefulset rerun-cloud dataverse-curation
```

改其他 key 同理,换掉 `stringData` 里的 key/值即可(比如补 `hf_token`、换 AK/SK)。

卸载:

```sh
helm uninstall dataverse -n $DATAVERSE_NS
# kubectl 直建的 Secret 不归 helm 管,如需彻底清理:
# kubectl -n $DATAVERSE_NS delete secret dataverse-secrets
```

注意:

- catalog 的数据盘 PVC(`server-data-rerun-cloud-0`)卸载时**不会删除**,注册记录都在;确认不要了再手动删;
- 不要改 `apig.host` 和 ingressClass:改动需删 Ingress 重建,且换 host 等于换公网域名(CORS、用户书签全要跟着换)。
