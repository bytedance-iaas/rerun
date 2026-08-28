# Rerun 云服务 v1 发布说明

发布日期:2026-08-31

本产品基于开源的 [rerun](https://rerun.io)(机器人 / 具身智能领域的多模态数据可视化工具,本版本对应 rerun 0.36.0),针对火山引擎做了增强,并内置质检台联动。
v1 是首个正式版本,面向在火山引擎上浏览、管理、训练机器人(LeRobot)数据集的团队。

使用方式见[使用指南](user-guide/),部署见 [02-deploy.md](02-deploy.md),验证见 [03-test.md](03-test.md)。

## 主要特性

### Viewer:直接打开云上数据集

- **TOS / HuggingFace 数据集直读**:viewer 直接打开火山引擎 TOS 或 HuggingFace 上的 LeRobot 数据集(v2 / v3),在线转换为 rrd,无需先下载到本地。
- **rrd 自动缓存**:转换产物写回 TOS 缓存桶,同一数据集二次打开直接秒开;可从 viewer 里删除缓存产物。
- **离线预转换工具**:命令行 `rerun rrd-convert` 可提前把数据集转好、灌进缓存,免去用户首次打开的等待;幂等,适合放进上线流程。
- **一键质检 Diagnose**:web viewer 中一键跳转质检台,数据集的 TOS 路径与地区自动填好;仅对 TOS 上的 LeRobot v2 / v3 数据集提供。
- **三种 viewer**:浏览器 web viewer(随开随用)、本地 native viewer、云上 native viewer 会话(经内网读 TOS、看超大数据集),三者共用同一套实现。

### Catalog server:数据集管理与训练取数据

- **数据集注册与查询**:把 TOS 上的数据集登记进目录,之后按名字引用;登记的只是元数据,数据本体留在 TOS。
- **训练直读(预签名)**:训练时数据字节直接从 TOS 流到训练机,不经过服务器中转,训练机也无需持有 TOS 密钥;`RerunIterableDataset` 可直接喂给 PyTorch。
- **token 认证**:管理员离线签发 token,限定用户、读写权限、有效期和可连接的服务器地址。
- **持久化**:注册记录落在云盘上,服务器重启后不丢,无须重新注册。

### 分发与部署

- **SDK 随部署分发**:`/downloads/sdk/` 提供四个平台的 wheel(Linux x86_64 / arm64、macOS Apple Silicon、Windows x64),与服务同源构建,版本天然一致;viewer 内置在 wheel 里,`pip install` 后 `rerun` 命令即可用,无需发布到公共包仓库。
- **云上部署形态**:面向火山引擎 VKE 的完整部署,统一 HTTPS 网关入口(APIG),含按需拉起的云上 native viewer 会话。
- **国内网络适配**:HuggingFace 访问走镜像站,云内组件访问 TOS 走内网 endpoint。

## 支持范围与已知限制

- 目前主要支持机器人 **LeRobot 数据集(v2 / v3)**。
- Web viewer 运行在浏览器 WASM 环境中,实际可用内存约 1.4 GB,较大的数据集需改用 native viewer。
- 预编译 wheel 覆盖 **Linux x86_64 / arm64、macOS(Apple Silicon)、Windows x64** 四个平台;Intel 芯片的 Mac 暂无预编译 wheel(苹果已停产该硬件),需源码构建。
- Diagnose 仅在 web viewer 中提供;native viewer 无此按钮。

## 文档

- [使用指南 · Viewer](user-guide/01-viewer.md)
- [使用指南 · Catalog Server](user-guide/02-catalog.md)
- [产品概述](01-overview.md) · [部署](02-deploy.md) · [验证](03-test.md)
