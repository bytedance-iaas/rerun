# Rerun 云服务

本产品基于开源的 [rerun](https://rerun.io)(机器人/具身智能领域的多模态数据可视化工具),实现了针对火山引擎的增强和优化:

- **TOS 和 HuggingFace 数据集直读**:viewer 直接打开火山引擎 TOS 或 HuggingFace 上的 LeRobot 数据集,在线转换为 rrd,无需下载到本地。
- **rrd 自动缓存**:转换产物自动写回 TOS 缓存桶,同一数据集第二次打开直接加载现成 rrd,无须再次转换。
- **桶 CORS 自助配置**:web viewer 打开新桶时自动补配桶的跨域放行规则,运行时换桶、新建桶都无需预先配置。
- **SDK 随部署分发,viewer 内置其中**:`/downloads/sdk/` 提供四个平台的 wheel(Linux x86_64 / arm64、macOS Apple Silicon、Windows x64),与镜像同源构建;viewer 打包在 wheel 内,`pip install` 后 `rerun` 命令即可用,版本与在跑的服务天然一致,无需发布到任何包仓库。
- **数据集管理**:catalog server 提供数据集注册、查询和 token 认证。
- **catalog server 持久化**:注册记录和缓存落在云盘上,服务器重启后数据不丢、无须重新注册。
- **训练直读**:训练侧凭 catalog server 签发的预签名 URL 从 TOS 直读数据,不经服务器中转,也无需持有 TOS 密钥。
- **云上部署形态**:面向火山引擎 VKE 的完整部署,含 HTTPS 网关入口和按需启动的云上 native viewer 会话。
- **质检联动**:web viewer 中一键「质检」(Diagnose)跳转质检台
。
- **中英双语界面**:viewer 界面支持中英文,右上角按钮一键切换、即时生效并记住选择;专有名词(Rerun、TOS、rrd、episode 等)保留英文。

当前版本的特性与已知限制见[发布说明](release-notes-v1.zh.md);产品构成与架构见 [01-overview.md](01-overview.md),部署步骤见 [02-deploy.md](02-deploy.md),部署后验证见 [03-test.md](03-test.md)。

## 术语表

| 术语 | 解释 |
|---|---|
| rerun | 开源的时序多模态数据可视化工具([rerun.io](https://rerun.io)),本产品在其基础上二次开发。 |
| viewer | rerun 的可视化界面。本产品提供 web viewer(浏览器版)和 native viewer(原生版,分本地运行和云上会话两款)。 |
| LeRobot 数据集 | HuggingFace 的机器人数据集格式,由 parquet 表格和 mp4 视频组成,分 v2 / v3 两个版本,本产品均支持。 |
| rrd | rerun 的数据文件格式。viewer 只渲染 rrd;打开 LeRobot 数据集时先转换成 rrd,转换产物缓存在 TOS 上。 |
| `tos://` | 火山引擎 TOS 上一个位置的地址写法:`tos://桶名/路径/`。 |
| catalog server | 本产品的数据集注册与查询服务。gRPC 协议,端口 51234,token 认证;Python 客户端为 `rerun.catalog.CatalogClient`。 |
| 预签名 URL | catalog server 用自己的密钥为 TOS 对象签发的限时下载链接。训练侧凭它直接从 TOS 读数据,无需持有任何 TOS 密钥。 |
| 直读 | 数据字节从 TOS 直达客户端、不经过 catalog server 中转的读取方式。server 只负责元数据查询和签发预签名 URL。 |
