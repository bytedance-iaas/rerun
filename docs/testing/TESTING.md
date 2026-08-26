# 部署验收测试(逐项命令)

部署或更新镜像之后,按本文从上到下过一遍,每项都写了**命令 + 预期结果**。
专项深测另有文档:dataloader 直读见 [`dataloader-direct-read-test.md`](dataloader-direct-read-test.md),
"断网证直连" demo 见 [`direct-read-demo.md`](direct-read-demo.md);本文不重复它们的内容。

## 0. 准备:把几个变量取到手

后面的命令都用这几个变量,先取好:

```sh
# CLB 公网 IP(catalog 的入口)
CLB_IP=$(kubectl -n rerun get svc rerun-cloud -o jsonpath='{.status.loadBalancer.ingress[0].ip}')

# web 域名:APIG 控制台查(kubectl 查不到),形如 xxx.apigateway-cn-beijing.volceapi.com
WEB=https://<web 的 volceapi.com 域名>

# 测试 token(read-write,7 天):怎么签见 README「Catalog token」一节;办公网自测要多签一个 127.0.0.1
TOKEN=<签出来的 token>
```

## 1. 集群侧:东西都起来了吗

```sh
kubectl -n rerun get pods,svc
# 预期:rerun-cloud-0 是 2/2 Running,daft-curation-0 是 1/1 Running;
#      svc rerun-cloud 的 EXTERNAL-IP 有值(<pending> 不动 → 见 README 故障速查)

kubectl get apiginstance -n rerun
# 预期:PHASE=Running(偶发 items 为空是 API 抖动,重试)

kubectl -n rerun get ingress -o wide
# 预期:每条 Ingress 的 ADDRESS 都是网关 CLB 的公网 IP

kubectl -n rerun exec daft-curation-0 -- ls /mnt/tos
# 预期:datasets/  deliveries/(TOS 挂载正常)
```

## 2. Web viewer(APIG 入口)

```sh
curl -s -o /dev/null -w '%{http_code}\n' $WEB/healthz
# 预期:200(健康检查免认证)

curl -s -o /dev/null -w '%{http_code}\n' $WEB/
# 预期:401(Basic auth 在拦 —— 如果是 200,说明认证没开,查 web_htpasswd)

curl -s -o /dev/null -w '%{http_code}\n' -u 'alice:passwd_1' $WEB/
# 预期:200(换成 web_htpasswd 里真实的账号密码)

curl -s -u 'alice:passwd_1' $WEB/config.json | head -c 200; echo
# 预期:JSON,含 tos_endpoint 与凭证字段(这是 TOS 弹窗的配置来源,404 的话弹窗会要求手填)
```

浏览器侧(命令测不了的部分):

1. 打开 `$WEB`,输入账号密码,应看到 rerun 欢迎页。
2. 菜单 → Extended → **Open from Volcengine TOS**:弹窗应只有「Dataset URL + 地区」两个输入
   (地区默认华北2(北京),凭证/endpoint 全部来自部署配置,没有手填框)。
3. 填 `tos://<bucket>/datasets/<数据集名>/` → Open。
   预期:左侧面板出现 **Volcengine TOS** 分组,数据集名显示为完整 tos:// 路径,
   episode 逐个出现并流入;点某个 episode 会被优先加载。
4. 打不开先分辨两种失败(README 故障速查有细节):
   F12 → Network 筛 `tos-s3` —— "blocked by CORS policy" = 桶 CORS 白名单没加这个域名;
   403 = AK/SK 权限;"Failed to fetch" 且 curl 正常 = 浏览器缓存投毒,清缓存硬刷。

## 3. Daft 质检台(同域名 /curation)

```sh
curl -s -o /dev/null -w '%{http_code}\n' -u 'alice:passwd_1' $WEB/curation/healthz
# 预期:200
```

浏览器侧:

1. 打开 `$WEB/curation`(同一组账号,登录一次两边通行)。
2. 顶部应有三个页签:**任务台 / 质检报告 / 终端**。
   「终端」现在默认开启;点进去应看到容器内的 shell 提示符,能敲 `ls /mnt/tos`。
   (不想要 shell 的部署,在质检台容器加环境变量 `CURATION_TERMINAL=0`。)
3. 跑质检页:数据集输入是「TOS 路径 + 地区」。
   填 `tos://<bucket>/datasets/<数据集名>/`,交付名随便起一个,点「开始质检」应能起任务。
   故意把地区换成「中国香港」再点,预期被拦:"本站的数据面挂载的是 cn-beijing 地域的桶…"。
4. **联动测试**:回到 web viewer,左侧 TOS 数据集行上点「Diagnose」,
   预期新标签页打开质检台、TOS 路径框已被自动填上完整 tos:// 路径。

## 4. Catalog server(CLB 入口 + token)

```sh
curl -s http://$CLB_IP:51234/version; echo
# 预期:版本串(此端点免认证;连不上先看第 6 节的白名单和办公网说明)
```

认证行为(Python,`pixi run uvpy` 或任何装了 rerun SDK 的环境):

```python
import rerun as rr

# ① 不带 token → 必须被拒
try:
    rr.catalog.CatalogClient(f"rerun+http://<CLB_IP>:51234").datasets()
    print("⚠️ 没带 token 也能查 —— 认证没生效,立刻查 server_token_secret 是否配上")
except PermissionError as e:
    print("OK,无 token 被拒:", e)

# ② 带 token → 正常
client = rr.catalog.CatalogClient(f"rerun+http://<CLB_IP>:51234", token="<TOKEN>")
print("OK,数据集列表:", [d.name for d in client.datasets()])
```

权限边界(read 干写活必须被拒):

```python
ro = rr.catalog.CatalogClient(f"rerun+http://<CLB_IP>:51234", token="<read 权限的 token>")
try:
    ro.create_dataset("should-fail")
    print("⚠️ read token 竟然能建数据集 —— 权限校验失效")
except PermissionError as e:
    print("OK,read token 写操作被拒:", e)
```

端到端(read-write token 注册 TOS 数据集,完整版见 README「端到端自测」):

```python
ds = client.create_dataset("smoke-test", exist_ok=True)
client.get_dataset(name="smoke-test").register(
    ["tos://<bucket>/<path>/episode_000.rrd"]).wait(timeout_secs=60)
print(client.get_dataset(name="smoke-test").schema())   # 有 schema = 注册成功
```

server 侧对照:`kubectl -n rerun logs rerun-cloud-0 -c catalog | tail`,
无 token 的尝试应能看到 `Token verification failed`(限频每秒一条)。

> 办公网(飞连)里直连 CLB 会报 transport error(TCP 通但 HTTP/2 被掐),
> 这不是故障 —— 按 README「办公网例外」用 `kubectl port-forward` + 签了 `127.0.0.1` 的 token 测。

## 5. 直读与 dataloader(引用专项文档)

```sh
# 预签名直读快测:客户端全程不碰 TOS AK/SK
export RERUN_SEGMENT_DIRECT_READ=presigned
python docs/testing/test_dataloader_direct.py
# 预期:"读出 200 个样本 —— dataloader 跑通" + state/action 的 shape
# (脚本里的 token/地址换成自己的;token 用环境变量传,别写死进文件)
```

- 完整流程(字段发现、epoch、A/B 证明直连):[`dataloader-direct-read-test.md`](dataloader-direct-read-test.md)
- 断 server 证直连的演示:[`direct-read-demo.md`](direct-read-demo.md)

## 6. 安全验收

```sh
# ① CLB IP 白名单真的在拦:换一个不在白名单里的网络(手机热点最方便)
curl -sv --connect-timeout 5 http://$CLB_IP:51234/version
# 预期:连接超时/被拒 —— 如果通了,白名单没配或没绑到监听器(README 部署顺序第 5 步)

# ② 同一命令在办公网/VPN 里跑
# 预期:返回版本串(白名单放行了出口 IP;不通则出口 IP 列表不全,找 IT 核对)

# ③ web 侧不存在匿名通道
curl -s -o /dev/null -w '%{http_code}\n' $WEB/curation/
# 预期:401(质检台也在 Basic auth 后面)
```

提醒:catalog 的 token 在四层 CLB 上是**明文**传输的,白名单这道外层锁要一直留着;
token 泄露的止损手段是换 `server_token_secret` 重签(会波及所有人),所以有效期别签太长。

## 7. Native 会话(用到才测)

```sh
sed -e 's/<USERNAME>/qian/g' -e 's/<SESSION_PASSWORD>/我的密码/g' \
    native-viewer-template.yaml | kubectl apply -f -
kubectl -n rerun get ingress rerun-native-qian    # 等 ADDRESS
# 首次去 APIG 控制台查这个 host 的域名,浏览器开:
#   https://<域名>/vnc.html?autoconnect=true&resize=remote
# 预期:输会话密码后看到云上 native viewer 桌面;File → Extended 里 TOS/HF 弹窗可用
# 用完删掉:
sed -e 's/<USERNAME>/qian/g' -e 's/<SESSION_PASSWORD>/x/g' \
    native-viewer-template.yaml | kubectl delete -f -
```

## 8. 一分钟速查表

| 测什么 | 命令 | 预期 |
|---|---|---|
| pod 全活 | `kubectl -n rerun get pods` | 2/2 与 1/1 Running |
| web 健康 | `curl $WEB/healthz` | 200 |
| web 有锁 | `curl $WEB/` | 401 |
| 质检台健康 | `curl -u user:pass $WEB/curation/healthz` | 200 |
| catalog 活着 | `curl http://$CLB_IP:51234/version` | 版本串 |
| catalog 有锁 | 无 token 调 `datasets()` | `PermissionError` |
| 白名单在拦 | 热点下 curl catalog | 连不上 |
| 直读跑通 | `test_dataloader_direct.py` | 读出样本 |
