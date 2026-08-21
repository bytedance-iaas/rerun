# rerun-native-session Helm chart

按需自助的云上 native viewer 会话(一人一个 pod,用完即删)。
一个 release = 一个会话(pod + service + 可选 Ingress),与 helm 的生命周期天然对齐:
`helm install` 开会话,`helm uninstall` 一把清干净。

前提:同 namespace 已部署 [`dataverse`](../dataverse/) chart(ReRun + 质检台;复用其凭证 Secret 与 APIG 网关)。

```sh
# 开一个会话(release 名 = 你的名字,小写字母/数字/中划线)。
# values 结构是 dataverse chart 的子集:直接 -f 同一份 values 文件,
# 镜像/缓存桶/凭证 Secret 全部沿用,只需另给会话密码:
helm install qian deploy/helm/rerun-native-session -n rerun \
    -f deploy/secrets/values-prod.yaml \
    --set sessionPassword=<会话密码>

# 列出在跑的会话(rerun-native-session chart 的 release)
helm list -n rerun

# 用完删掉
helm uninstall qian -n rerun
```

不走公网时 `--set ingress.enabled=false`,用 port-forward(命令见安装后的 NOTES)。
`dataverse` 的 release 名不是 `dataverse` 时,`secrets.existingSecret` 和 `ingress.className` 要按实际名字改。
