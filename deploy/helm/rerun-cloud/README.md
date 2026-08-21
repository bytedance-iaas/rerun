# rerun-cloud Helm chart

Rerun 云服务常驻部署的 Helm chart:web viewer + catalog(StatefulSet)+ Daft 质检台 + APIG 网关(含 catalog 的 gRPC 路由)。
包含:rerun StatefulSet(web viewer + catalog server 两容器)、Daft 质检台 StatefulSet(fsx-TOS 挂载)、集群内 Service、APIG 网关实例与按路径分流的 Ingress。
按需的云上 native viewer 会话是独立 chart:[`../rerun-native-session`](../rerun-native-session/)。

## 安装

推荐流程(密钥 kubectl 直建、values 零密钥,完整步骤见 docs/release/02-deploy.md):

```sh
# 1. 建 namespace 和三个 Secret(应用凭证 / fsx 挂载凭证 / token 签名密钥;
#    签名密钥用管道直建,不落盘):
kubectl create namespace rerun
kubectl -n rerun create secret generic rerun-cloud-secrets \
    --from-literal=tos_access_key=… --from-literal=tos_secret_key=… \
    --from-literal=web_htpasswd="alice:$(openssl passwd -apr1 'pw123')"
kubectl -n rerun create secret generic daft-secrets \
    --from-literal=AccessKeyId=… --from-literal=SecretAccessKey=…
rerun server generate-secret | kubectl -n rerun create secret generic \
    rerun-catalog-server-secrets --from-file=server_token_secret=/dev/stdin

# 2. values 里只引用名字(secrets.existingSecret / secrets.existingTokenSecret /
#    daft.fsx.existingSecret)+ 非密钥配置,然后安装:
helm install rerun-cloud deploy/helm/rerun-cloud -n rerun -f deploy/secrets/values-prod.yaml

# 3. 照安装结束打印的 NOTES 走完部署后步骤(查域名、配 CORS、签 token)。
```

开发环境也可不建 Secret,直填 `secrets.*` 字段由 chart 渲染(见 values.yaml 注释)。
签名密钥只以 0400 文件挂进 catalog 容器(projected 卷),不进环境变量,web / native 会话不可见;签发 token 用 `kubectl exec` 在容器内做,密钥不出集群。

release 名建议就叫 `rerun-cloud`:资源名前缀 = release 名,这样和文档里的名字一致。

## 常用开关

| value | 作用 |
|---|---|
| `daft.enabled=false` | 不部署质检台(连带跳过 TOS 挂载与 `/curation` 路由) |
| `vllm.enabled=true` | 在同 namespace 部署一个自托管 vLLM(GPU),并自动注册成质检台的一个 VLM 后端 |
| `vllm.model` / `vllm.servedModelName` / `vllm.gpuCount` / `vllm.nodeHostname` | 选模型、对外模型名、GPU 卡数、钉到哪个 GPU 节点 |
| `apig.enabled=false` | 不建网关和 Ingress(自备入口) |
| `apig.existingInstanceId` | 适配已有 APIG 实例而不新建 |
| `web.basicAuth.enabled=false` / `catalog.tokenAuth.enabled=false` | 关认证(仅限内网调试) |
| `secrets.existingSecret` / `daft.fsx.existingSecret` | 密钥由外部 Secret 提供,chart 不渲染明文 |
| `catalog.hfEndpoint=""` | 海外集群直连 HuggingFace 官方 |

## 自托管 vLLM(可选,给质检台用)

`vllm.enabled=true` 时,chart 在同 namespace 多起一个独立的 vLLM `Deployment` + `Service`(需要 GPU)。
质检台的可用后端只认 `site.yaml` 里的 `vlm_backends`,没有 k8s 自动发现 —— 所以打开这个开关后,chart 会把这个 vLLM 以 `self-hosted` 键自动注入 `vlm_backends`(endpoint 指向它的集群内 DNS,以 `/v1` 结尾),质检台界面直接能选到,不用手抄地址。

权重默认走内网 `oniond` + 火山镜像源的 initContainer 下到 `emptyDir`(不进镜像、不落 TOS,pod 重建时按 `*.safetensors`/`*.aria2` 幂等跳过)。
不想用这套流程,把 `vllm.weightFetch.image` 留空,`vllm.model` 填 HF repo id,让 vLLM 自己从 HF 拉(会复用 `catalog.hfEndpoint` 镜像站和共用 secret 里的 `hf_token`)。

关键点:`vllm.servedModelName` 必须与质检台发请求用的模型名一致 —— 自动注入的后端 `model` 字段就取它,所以一般不用管;换模型改 `vllm.model`(本地路径 `/models/<weightFetch.modelName>`,与 `weightFetch.modelName` 对应)。
单卡放不下就把 `vllm.gpuCount` 提到 2 并在 `vllm.extraArgs` 加 `--tensor-parallel-size 2`。

## 升级与配置变更

- `helm upgrade rerun-cloud deploy/helm/rerun-cloud -n rerun -f …` 即可;
  Secret / 站点配置内容变化通过 checksum 注解自动滚动重启 pod
  (`secrets.existingSecret` 场景 chart 看不到内容,改完外部 Secret 需手动 `kubectl rollout restart`)。
- **不要改的字段**:StatefulSet 的 selector 标签(k8s 不允许,改 = 删了重建);
  `apig.webHost` 与 ingressClass(in-place 改会残留旧 ADDRESS,要删 Ingress 重建,且换 host = 换公网域名)。

## 卸载与数据

- `helm uninstall rerun-cloud -n rerun`。
- catalog 的数据盘 PVC(`server-data-<fullname>-0`)由 volumeClaimTemplates 管理,**uninstall 不删**;确认不要了再手动删 PVC。
- 质检台的 PV/PVC 只是 TOS 桶的挂载通道,删了不丢数据(数据在桶里)。
- APIG 网关实例删除后,自动分配的域名随之失效。

## 从 kubectl 模板部署迁移

旧的 `kubectl apply -f rerun-cloud.yaml` 部署与本 chart 的资源名和 selector 标签不同
(旧 `rerun-web`/`daft-curation` → 新 `<fullname>-web`/`<fullname>-curation`),不能原地接管:

1. 备好 token 签名密钥和各账号密码(与旧部署一致,用户无感);
2. 删旧 StatefulSet/Service/Ingress/APIGInstance(旧 catalog PVC `server-data-rerun-cloud-0` 名字恰好与新 chart 一致,保留即可被新 StatefulSet 复用;不一致时先改名或接受重新注册);
3. `helm install`;
4. 网关域名会重新分配:更新 TOS 桶 CORS 白名单,通知用户换地址。

## 已知前提

- 集群在火山引擎 VKE 上,且已装 APIGInstance CRD(`loadbalancer.vke.volcengine.com/v1beta1`)与 fsx CSI(TOS 模式);
- `network.subnetId` 必须是当前集群 VPC 里的子网;
- robot_curator 镜像 ≥ Daft 仓库 commit 412b91ce8(子路径支持)。
