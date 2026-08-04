# VKE 部署模板

| 文件 | 进 git? | 内容 |
|---|---|---|
| `rerun-cloud-template.yaml` | ✅ | 常驻服务模板(占位符):namespace + 密钥 + PVC + 一个 Deployment(web viewer 与 catalog server 两容器同 pod)+ 公网 CLB |
| `rerun-cloud.yaml` | ❌ .gitignore 已挡 | 上面模板的**已填真实密钥**版,直接 apply |
| `native-viewer-template.yaml` | ✅ | 单用户的云上 native viewer 会话 pod 模板(`<USERNAME>` 占位)+ 会话专属公网 CLB;不含密钥(引用集群里的 Secret) |

## 密钥安全

Secret 用 `stringData` 直接填**明文**(k8s apply 时自动转 base64,base64 不是加密)。
真实值只存在于 `rerun-cloud.yaml`(gitignore 已挡)和 `deploy/secrets/`,模板永远只有占位符。
从模板重新生成已填版:

```sh
cd deploy
sed -e "s|⚠️REPLACE_TOS_ACCESS_KEY|$(tr -d '\n' < secrets/tos_access_key)|" \
    -e "s|⚠️REPLACE_TOS_SECRET_KEY|$(tr -d '\n' < secrets/tos_secret_key)|" \
    -e "s|⚠️REPLACE_HF_TOKEN|$(tr -d '\n' < secrets/hf_token)|" \
    vke/rerun-cloud-template.yaml > vke/rerun-cloud.yaml
# (镜像地址和 subnet-id 模板里仍是占位符,记得补)
```

## 常用命令

```sh
# 常驻服务(首次:集群里有 tos-poc 时代的旧 pod 占着 PVC,先清)
kubectl -n rerun delete pod rerun-web rerun-server --ignore-not-found
kubectl -n rerun delete svc rerun-web rerun-server --ignore-not-found
kubectl apply -f rerun-cloud.yaml
kubectl -n rerun get pods,svc

# ⚠️ 拿到 EXTERNAL-IP 之后还有一步:把新地址加进 TOS 桶的 CORS 白名单,
# 否则浏览器打不开 tos:// 数据集(native viewer 不受影响,所以容易漏)。
# 编辑 ../enable-cors.sh 的 AllowedOrigin 加上 http://<EXTERNAL-IP>,然后:
(cd .. && ./enable-cors.sh)
# 详见 ../README.md 的 "Bucket CORS" 一节。

# 个人 native 会话(把 qian 换成自己的名字)
sed 's/<USERNAME>/qian/g' native-viewer-template.yaml | kubectl apply -f -
kubectl -n rerun get svc rerun-native-qian        # 拿 EXTERNAL-IP
# 用完删掉(CLB 按小时计费)
sed 's/<USERNAME>/qian/g' native-viewer-template.yaml | kubectl delete -f -
```

noVNC 无认证,公网暴露前务必看 `native-viewer-template.yaml` 文件头的安全提醒(port-forward 或 CLB ACL)。
