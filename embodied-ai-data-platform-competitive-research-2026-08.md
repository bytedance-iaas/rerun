# 具身智能数据产品竞品调研报告

调研日期：2026-08-31。
方法：三路并行网络调研（国际产品 / Rerun 官方与 LeRobot 生态 / 国内产品），来源链接见各附录。

对比基准 —— 我们的产品：基于开源 Rerun 改造的具身智能数据栈：

- Web viewer 直读火山引擎 TOS 对象存储上的 LeRobot 格式数据集（流式转 rrd、支持数 GB 大文件、预签名 URL、自动 CORS）
- 本地原生 viewer（同一能力）
- Daft + Gradio 数据质检台（直挂存储桶，免数据搬移）
- Helm 私有化部署（火山 VKE + APIG 网关 gRPC 路由）
- 四平台预编译 wheel 分发

---

## 一句话结论

我们卡位的组合 —— **浏览器直读对象存储上的 LeRobot 数据集 + 私有化部署 + 直挂桶的质检台** —— 目前全球范围内没有任何一家（包括 Rerun 官方）以开源或可私有化的形态做到。
但三个方向的对手都在快速逼近：Rerun 官方的商业产品 Hub、Foxglove 的 "Physical AI 数据平台"、阿里云的 AnalyticDB 具身平台。
先发窗口存在，估计不会超过一年。

## 国际市场：钱多、动作快，但格式路线和我们错位

| 产品 | 定位 | 格式 | 云存储直读可视化 | 私有化 | 近况 |
|---|---|---|---|---|---|
| **Foxglove** | 最全面的对标：viewer + 数据平台 + 车队管理 | MCAP 为核心 | 有（浏览器按 URL 流式打开 MCAP） | 企业版可全私有 | 2025-11 融 4000 万美元 B 轮；viewer 已闭源 |
| **Roboto AI** | 机器人日志搜索/自动分诊 | rosbag/MCAP | 走平台摄取，非直读 | 仅 SaaS | 2026-07 上线 AI agent 自动质检 |
| **Encord** | 训练集筛选+标注，2025 推 Physical AI 套件 | MCAP/rosbag 摄取，可导出 LeRobot | 需先注册进平台 | 支持 VPC/离线 | 2026-02 融 6000 万美元 |
| **Voxel51 FiftyOne** | 开源数据集筛选（嵌入向量相似搜索强项） | MCAP、经典 CV 格式 | 部分（媒体可放 S3） | 企业版可离线部署 | 活跃，NVIDIA 生态绑定 |
| **Nominal** | 硬件测试数据栈（航天/国防向） | MCAP/HDF5 等 | 否 | 支持离线/涉密环境 | 2026-03 估值 10 亿美元 |
| **Heex** | 边端触发式采集（只传关键片段） | ROS 系 | 否 | 边缘+云混合 | 规模较小 |
| **ReSim** | 仿真/回归测试编排 | ROS 2 日志 | 可视化外包给 Foxglove/Rerun | SaaS | 活跃 |
| **Scale AI** | 数据采集/标注服务（非软件平台） | 按项目定制 | — | — | Meta 入股后中立性受质疑 |

要点：

1. **MCAP 是国际市场的通用格式**（Foxglove 创立的机器人日志格式），几乎所有国际竞品都支持，而我们目前不支持 —— 这是与国际产品对表时最明显的格式缺口。
2. **LeRobot 正在成为训练数据的通用语言**：LeRobot 0.6.0 官方加了 Foxglove 后端，Encord 支持 LeRobot 导出，NVIDIA GR00T、Physical Intelligence 的 openpi 都用 LeRobot 训练。我们押注 LeRobot 是押对了。
3. 没有任何一家做到"浏览器直读任意对象存储上的 LeRobot 数据集"。

## 上游 Rerun 官方：最直接的战略对手

（调研 agent 直接核对了上游 2026-08-31 的 main 分支代码）

- **开源版至今做不到我们做的事**：LeRobot 加载器（`re_importer/importer_lerobot.rs`）是"仅本地路径、仅原生端"；web viewer 的数据源枚举里只有单文件 URL（rrd/mcap 等）、本地文件、`rerun://` 目录服务，没有"远程 LeRobot 目录"。
- **这个能力被圈进了商业产品 Rerun Hub**（2025-03 融 1700 万美元，目前设计伙伴内测）：从客户自己的 S3 桶按字节范围流式读取、SQL/dataframe 查询、网页共享 viewer、直连 GPU 的 PyTorch dataloader。功能上是我们的超集，但**只提供公有云单租户，不能私有化部署，也没有火山引擎/中国云的迹象**。
- 开源版在从两侧逼近：0.32 开源 catalog 服务器（本地目录 SQL 查询）、0.35/0.36 实验性 web catalog（浏览器打开超内存 rrd）。但对象存储那一层被明确留作商业收费点（开源的 `re_server` 自述"仅内存实现，用于测试"）。
- **维护成本预警**：LeRobot 格式迭代快（v3.0 → v3.1 → 0.6.0 变体），上游加载器自己都被弄坏过两次（issue #11678 等）。我们跟上游 + 跟 LeRobot 是双线维护。
- HF 官方的 lerobot-dataset-visualizer（开源，Next.js）是最接近我们 web viewer 的开源物，但只面向 HF Hub，没有 S3/其他云的鉴权与 CORS 方案，也不是 Rerun viewer。

## 国内市场：商业化工具链基本空白，是我们的机会

- **刻行时空 coScene**：国内唯一成型的机器人数据管理商业软件，走 Foxglove fork + MCAP 路线，**公开层面完全不支持 LeRobot**。护城河在数采闭环（边端 agent + 规则引擎自动截取故障数据），可视化只是入口。与我们格式路线错位，正面冲突小于表面。
- **阿里云 AnalyticDB 具身平台**：形态上最接近我们的云产品 —— LeRobot 2.x/3.x、网页 Dataset Viewer、episode 星级质检、Ray 分布式格式转换。但走"数据从 OSS 导入平台托管"，不是浏览器直读桶，且绑死阿里云。**国内最需要盯的对手**。
- **华为云 CloudRobo**（2026-06 公测）：主打合成数据（"20% 采集 + 80% 生成"），无独立网页可视化工具。
- **百度智能云**：百舸 + 具身数据超市，市占第一（Omdia 1H25 约 35%），但无可视化产品。
- **火山引擎**：自己没有具身数据平台产品；其 AI 数据湖 LAS 官方博客明确支持 MCAP/LeRobot、计算引擎恰好是 Daft —— 我们与火山是"底座 + 应用层"互补，生态顺风。
- **智元 AgiBot**：官方数据集可视化脚本直接用 Rerun（README 明确 "will open rerun.io"）—— 国内头部数据集生态已给 Rerun 背书；且国内尚无公司基于 Rerun 商业化。Genie Studio 是国内本体厂商唯一成型的对外数据平台，需关注其向第三方开放的动向。
- **宇树**：98 个 HF 开源数据集全部原生 LeRobot v2、Apache 2.0，无自研可视化 —— 是 LeRobot 路线的典型潜在用户画像。
- **国家级机构**：北京人形创新中心（RoboMIND，HDF5 + 官方转 LeRobot 工具）、智源具身数据平台、上海国地中心白虎数据集（2.5PB 自有格式）—— 输出数采服务/数据集，均无对外数据工程软件。
- **传统标注商**（海天瑞声、整数智能、数据堂等）：全线转向具身，但清一色"训练场 + 人力采标"，没有一家做出面向客户的数据管理/可视化软件 —— **数据生产层与数据工程软件层之间存在明显工具断层**。
- **新势力**：光轮智能（合成数据独角兽，估值 20 亿美元+）、它石智航（Pre-A 4.55 亿美元）、无问智科（客户含字节）等 —— 多为合成数据/数据工厂路线，非数据工程软件。

## 竞争定位总表

| 能力 | 我们 | 最近的竞对 | 判断 |
|---|---|---|---|
| 浏览器直读对象存储 LeRobot 流式可视化 | 有 | 全市场无人做到；阿里云最接近但走导入托管 | 独有差异点 |
| LeRobot 商业化工具链 | 核心 | coScene 不支持；云厂商绑自家云 | 国内空白带 |
| 质检台直挂桶免搬移 | 有（Daft+Gradio） | 阿里云 episode 星级审核 | 差异点；与火山 LAS 同用 Daft 可借力叙事 |
| 私有化部署 | 有（Helm/VKE） | Rerun Hub 不提供；云厂商绑公有云；coScene 有但细节不公开 | 对数据不出域的客户是刚需卖点 |
| MCAP 支持 | 无 | 国际全员标配；上游 Rerun 已有实验性支持 | 最明显格式缺口 |
| 智能化质检（嵌入搜索/自动分诊） | 无 | Roboto agents、Encord EBIND、Voxel51 | 行业趋势方向 |
| 数采闭环（边端 agent） | 无 | coScene 护城河 | 建议对接而非自建 |
| 合成数据/仿真 | 无 | 华为云、光轮、智元 Genie Sim | 不同赛道，可作上游对接 |

## 对下一阶段规划的启示

该守住的：

1. 浏览器直读任意对象存储的 LeRobot 数据集 —— 全球独一份，但 LeRobotDataset v3 本身就是为流式设计的，对手补齐门槛不高，要快。
2. 完全私有化部署 —— Rerun Hub 的明确空档。
3. Daft 质检台 —— 与火山 LAS 技术栈同源。

可考虑补的：

1. **MCAP 支持**（上游已有实验性实现，移植成本可能不高）。
2. **质检智能化**（嵌入向量相似搜索、自动质量检查）。
3. 数采侧不建议自建，走对接。

两个最大威胁（都可能在一年内发生）：

1. Rerun Hub 正式 GA 并推出自助/低价档。
2. 阿里云具身平台向"直读桶"演进。

---

# 附录 A：国际产品调研全文（英文原文）

## 1. Foxglove (foxglove.dev)

**What it does.** The closest overall competitor. Positions as "the agentic data platform for Physical AI": a multimodal visualization app (web + desktop) plus a data platform for recording, ingesting, indexing, streaming, and managing robot logs, fleet/device management, and teleoperation ([foxglove.dev](https://foxglove.dev/), [product](https://foxglove.dev/product)).

- **Formats:** MCAP (first-class; Foxglove created MCAP), ROS 1/2 bags, ULog, Protobuf/JSON; live connections to robots ([MCAP product page](https://foxglove.dev/product/mcap)). Notably, **LeRobot 0.6.0 added Foxglove as a native visualization backend** — `--display_mode=foxglove` for teleop/record/replay ([Foxglove SDK blog](https://foxglove.dev/blog/announcing-the-foxglove-sdk)).
- **Browser streaming from cloud storage:** Yes. app.foxglove.dev opens remote MCAP by URL with seek/streaming; a "remote data loader" caches/merges MCAP from your backend into a storage bucket; enterprise supports bring-your-own bucket ([docs](https://docs.foxglove.dev/docs/visualization/connecting/cloud-data/remote-data-loader)). No native LeRobot-parquet-from-bucket reading.
- **Search/curation:** indexing, event tagging, queries; 2026 positioning adds "agentic" workflows.
- **Deployment:** Foxglove Cloud, BYO bucket, or full on-prem/private-VPC (Enterprise) ([pricing](https://foxglove.dev/pricing)).
- **Open source:** Viewer went closed-source with Foxglove 2.0 ([ROS Discourse](https://discourse.openrobotics.org/t/foxglove-2-0-integrated-ui-new-pricing-and-open-source-changes/36583)); MCAP format and SDKs remain open.
- **Pricing:** Free ($0: 10 GB, 3 users), Pro $20/mo + usage, Enterprise custom.
- **Activity:** $40M Series B led by Bessemer, Nov 2025 ([Businesswire](https://www.businesswire.com/news/home/20251112126106/en/)); publishes a ["Rerun vs. Foxglove" page](https://foxglove.dev/robotics/rerun-vs-foxglove).

## 2. Roboto AI (roboto.ai)

Analytics/search engine for robotics logs: ingest, tag, query, agentic triage/root-cause analysis over rosbags and MCAP ([roboto.ai](https://www.roboto.ai/)).

- **Formats:** ROS 1/2 bags, MCAP, PX4, ArduPilot, Parquet, custom.
- **Search/curation:** multimodal queries, signal-similarity search, "Roboto Agents" (July 2026): automated triage, failure tagging, ticket creation, natural-language curation queries ([ROS Discourse](https://discourse.openrobotics.org/t/roboto-agents-agentic-triage-root-cause-analysis-and-data-curation-for-rosbags-and-mcap/57438)).
- **Deployment:** SaaS with BYO bucket; no self-hosted offering found.
- **Open source:** proprietary platform; open [Python SDK](https://github.com/roboto-ai/roboto-python-sdk).
- **Activity:** $5M seed (2023), Roboto Agents July 2026.

## 3. Nominal (nominal.io)

"Unified industrial data stack" for testing/operating hardware (aerospace/defense-leaning) ([nominal.io](https://nominal.io/)).

- **Formats:** MCAP (incl. video), HDF5, telemetry/CSV, video, point clouds. No LeRobot.
- **Deployment:** secure clouds, on-prem, air-gapped/gov.
- **Activity:** $75M Series B (Sequoia, June 2025), $80M at $1B valuation (March 2026) ([TechCrunch](https://techcrunch.com/2026/03/05/hardware-testing-startup-nominal-hits-1b-valuation-raises-155m-in-10-months/)); customers: US Air Force, Anduril, Shield AI.

## 4. Heex Technologies (heex.io)

Edge agents with event triggers capture only relevant data slices ("frugal AI") ([heex.io](https://www.heex.io/)). Hybrid edge + cloud SaaS. €6M round Jan 2024; modest scale.

## 5. ReSim (resim.ai)

Cloud platform for autonomy test/eval at scale — simulation, log replay, metrics, regression detection ([resim.ai](https://www.resim.ai/)). Delegates visualization to Foxglove/Rerun. SaaS; [open-core libraries](https://github.com/resim-ai/open-core).

## 6. Scale AI — Physical AI Data Engine

Data **service**, not software: large-scale robot-demo collection (100k+ production hours), 3D annotation, data streams for robotics foundation-model labs ([Scale blog](https://scale.com/blog/physical-ai)). Customers: Physical Intelligence, Generalist AI, Cobot. Meta bought 49% for $14.3B (June 2025); neutrality concerns push some labs elsewhere ([Sacra](https://sacra.com/c/scale-ai/)).

## 7. Encord (encord.com)

Multimodal data curation + annotation; dedicated **Physical AI suite** (June 2025) ([Businesswire](https://www.businesswire.com/news/home/20250612297482/en/)).

- **Formats:** native MCAP and ROS bag streaming; LiDAR (PCD, PLY), nuScenes, KITTI; **exports LeRobot, RLDS, MCAP, rosbag** ([physical-ai-data-services](https://encord.com/physical-ai-data-services/)).
- **Curation:** automated quality checks, embedding-based curation, edge-case mining; EBIND embedding model.
- **Deployment:** SaaS + VPC/on-prem incl. air-gapped.
- **Activity:** $60M raise Feb 2026 ([SiliconANGLE](https://siliconangle.com/2026/02/26/)); 300+ AI teams incl. Woven by Toyota, Zipline.

## 8. Voxel51 / FiftyOne (voxel51.com)

Open-source dataset curation/visualization, repositioned for Physical AI ([robotics page](https://voxel51.com/industries/robotics)).

- **Formats:** native MCAP playback; COCO/KITTI/nuScenes etc. LeRobot not documented.
- **Curation:** embedding similarity search, temporal tagging, model-eval loops — core strength.
- **Deployment:** OSS local (Apache-2.0, [GitHub](https://github.com/voxel51/fiftyone)); Enterprise on-prem/VPC/air-gapped.
- **Activity:** $30M Series B (2024), NVIDIA GTC 2025 push, Databricks partnership.

## Newer / adjacent entrants

- **Rerun upstream** — commercializing (see Appendix B).
- **Hugging Face LeRobot** — LeRobotDataset v3.0 designed for streaming from HF Hub; Foxglove native backend in 0.6.0; Yaak's petabyte-scale L2D driving dataset ([docs](https://huggingface.co/docs/lerobot/lerobot-dataset-v3)).
- **Yaak (yaak.ai)** — retrofit hardware + observability; open-source `rbyte` dataloader supports LeRobot and rrd.
- **Kleinkram (ETH Zürich)** — free open-source on-prem robotic data management: rosbag/MCAP, S3-compatible storage, web UI ([arXiv](https://arxiv.org/abs/2511.20492), [GitHub](https://github.com/leggedrobotics/kleinkram)). Closest OSS analog to a self-hosted bucket-mounted platform.
- **ReductStore** — Apache-2.0 time-indexed object store for robotics/IIoT ([GitHub](https://github.com/reductstore/reductstore)).

### Comparative takeaways

1. Nobody ships browser visualization reading LeRobot directly from arbitrary cloud object storage.
2. LeRobot support arriving fast everywhere; MCAP is the corresponding table stake we lack.
3. Private/self-hosted deployment is a common enterprise checkbox — competitive but not unique; our uniqueness is Volcengine tuning.
4. Curation is bifurcating: log-search/triage vs. training-set curation; embedding search + automated quality checks are now expected.
5. Upstream Rerun's commercial platform is the most direct strategic risk.

---

# 附录 B：Rerun 官方与 LeRobot 生态调研全文（英文原文）

## Headline answer

**No — upstream open-source Rerun today cannot open a LeRobot dataset straight from S3/object storage in the web viewer.** Verified in upstream `main` (2026-08-31, commit `f0c95de`):

- The LeRobot importer (`crates/store/re_importer/src/importer_lerobot.rs`) takes a local filesystem path, commented "this whole function is native-only". `re_lerobot` has no object_store/S3/HTTP path.
- The viewer's `LogDataSource` enum supports: HTTP URL to a single file (`.rrd`, `.rbl`, `.mcap`…), local file, `rerun://` redap dataset, `rerun+http://` proxy. No remote-LeRobot-directory source.
- The capability exists upstream only as the commercial **Rerun Hub**, and even there it streams their catalog/rrd-chunk format, not raw LeRobot directories.

## A) Rerun's commercial direction

- **$17M seed, 2025-03-20**, led by Point Nine; total $20.2M. Goal: "database and cloud data platform purpose-built for Physical AI" ([GlobeNewswire](https://www.globenewswire.com/news-release/2025/03/20/3046617/0/en/), [TechCrunch](https://techcrunch.com/2025/03/20/reruns-open-source-ai-platform-for-robots-drones-and-cars-revs-up-with-17m-seed)).
- ~78 employees; classic open-core: SDK/viewer OSS, Hub proprietary ([redhat-et deep-dive](https://github.com/redhat-et/physical-ai-platform-intel/blob/main/deliverables/intel/companies/rerun-deep-dive.md)).

### Rerun Hub (commercial, private preview)

Announced with "A new data layer for robot learning" (~May 2026) ([blog](https://rerun.io/blog/data-layer-for-robot-learning), [rerun.io/hub](https://rerun.io/hub), [pricing](https://www.rerun.io/pricing)):

- Catalog + storage engine over customer's own S3-compatible buckets; byte-range selective streaming; SQL/dataframe queries into columns/time-ranges; derived columns; video-codec-aware PyTorch dataloader streaming to GPUs; shared team web viewer with annotation, auth/SSO. Petabyte scale (DROID: 70,000 clips).
- **Cloud-only, single-tenant in customer-chosen region**; no self-hosting except "data plane in your own cloud account" on request. Contract pricing, design partners only.
- No sign of Volcengine/China-cloud support. Our fully private Helm deployment remains differentiated.

### OSS release trajectory

- 0.22–0.23 (early-mid 2025): native LeRobot loading begins; first redap server/catalog browser UI.
- 0.24 (Jul 2025): URDF loader, multi-sink gRPC.
- 0.26.x (Oct 2025): LeRobot v3 broke (issue [#11678](https://github.com/rerun-io/rerun/issues/11678)); fixed by PR #12071.
- 0.28 (2025-12): LeRobot v3 dataloader ships.
- 0.32 (May 2026): stable `.rrd`, chunk-processing APIs (MCAP, Parquet), **open-source catalog server with SQL over local rrd directories**, OSS PyTorch dataloader, LeRobot ACT training example.
- 0.33: headless viewer, push-down filtering. 0.34 (Jul 2026): viewer MCP for LLM agents.
- 0.35 (2026-07): experimental built-in viewer catalog — larger-than-RAM RRD without a server.
- 0.36 (2026-08): experimental **viewer catalog on the web**, manifest fetching via URLs, Gaussian splats; LeRobot 0.6.0 v3 variant supported.
- redap/gRPC: full `RerunCloudService` protocol is open (`cloud.proto`), but OSS `re_server` is "in-memory… for testing". S3-backed implementation is Hub only.

**Trajectory read:** upstream moving in exactly our direction, but the object-storage tier is fenced off as commercial, and everything routes through their catalog/rrd model, not raw LeRobot-from-a-bucket.

## B) LeRobot / Hugging Face ecosystem

- **LeRobotDataset v3.0** (LeRobot 0.4.0, Oct 2025; v3.1 since): consolidated parquet/MP4, relational episode metadata, `StreamingLeRobotDataset` — Hub-native streaming ([HF docs](https://huggingface.co/docs/lerobot/lerobot-dataset-v3)).
- De-facto standard: NVIDIA Isaac GR00T trains on LeRobot ([HF blog](https://huggingface.co/blog/nvidia/nvidia-isaac-gr00t-in-lerobot)); Physical Intelligence's openpi converts to LeRobot; OXE and AgiBot republished in LeRobot; [any4lerobot](https://github.com/Tavish9/any4lerobot) converter collection; "nearly all robot datasets are repackaged into LeRobot format" ([Pebblous 2026](https://blog.pebblous.ai/report/robot-physical-ai-datasets-landscape/en/)).
- **HF's visualizer** ([lerobot-dataset-visualizer](https://github.com/huggingface/lerobot-dataset-visualizer), Apache-2.0; [Space](https://huggingface.co/spaces/lerobot/visualize_dataset)): Next.js app reading Parquet (hyparquet) + MP4 directly in browser from HF Hub — pagination, lazy loading, synced video + graphs, 3D URDF viewer, annotation editing. **Differences vs. ours:** HF-Hub-centric (no S3/other-cloud auth, no CORS/presign machinery), not a Rerun viewer, curation limited to annotations.
- **LeRobot ↔ Rerun:** all visualization inside LeRobot is built on the Rerun SDK (`lerobot-dataset-viz`).
- **Curation at scale:** `lerobot-edit-dataset` CLI (delete/split/merge/relabel) — script-level, single-machine; phospho repair/merge tools. **No open-source equivalent of our Daft+Gradio curation console found.** Rerun's answer is Hub's SQL — commercial.

## Competitive summary table

| Capability | Our fork | Upstream OSS (0.36) | Rerun Hub | HF ecosystem |
|---|---|---|---|---|
| LeRobot v2/v3 ingest | Yes (browser + native) | Yes (native, local dir only) | Unclear (rrd-centric) | Native format |
| Web viewer direct from object storage | **Yes (arbitrary S3/TOS)** | No | Yes, via catalog (preview) | HF Hub only |
| Catalog + query | Basic | Local-dir catalog, SQL | SQL over petabytes | Hub search |
| Curation console | Daft+Gradio | No | Team viewer + annotations | CLI tools |
| Private/on-prem | Helm, fully private | Self-host OSS pieces | **Cloud-only** | HF Hub |

**Bottom line:** we are ahead on the specific combination; upstream closing in from two sides; LeRobot format churn is an ongoing maintenance cost. Main risks: Hub GA with self-serve tiers; upstream OSS wiring `re_lerobot` to remote sources. Neither has happened as of today.

---

# 附录 C：国内市场调研全文

## 0. 总览与关键判断

1. **LeRobot 已成为国内具身数据的事实交换格式**：宇树、星海图原生 LeRobot；智元、松灵、北京人形创新中心（RoboMIND）原生 HDF5/自有格式但官方提供 LeRobot 转换工具链；阿里云、火山引擎、华为云、百度的平台层全部对接 LeRobot。rosbag/MCAP 阵营主要剩 coScene 一家坚守。
2. **国内唯一成型的"机器人数据管理+可视化+质检"商业软件是刻行时空 coScene**，走 Foxglove fork + MCAP 路线，公开层面不支持 LeRobot。未发现任何国内公司基于 Rerun 做商业化 —— LeRobot 格式的商业化可视化/质检工具链在国内基本空白。
3. **火山引擎自己没有具身智能数据平台产品**，其多模态数据湖（LAS + TOS + Lance/Iceberg）官方博客明确支持 MCAP 和 LeRobot 格式，核心计算引擎是 Daft（[字节数据平台官方博客](https://www.cnblogs.com/bytedata/p/19192773)）—— 与我们技术栈同源，是潜在协同而非竞争。
4. **最接近我们形态的云产品是阿里云 AnalyticDB 具身智能平台**（约 2026 年中上线）：LeRobot 2.x/3.x、网页 Dataset Viewer、OSS 导入、episode 星级质检 —— 但走"数据搬入平台托管"路径，非浏览器直读桶。
5. **四家云厂商都没有"浏览器直连对象存储、免搬移流式可视化"能力**。
6. 市场热度极高：Omdia 估 2024 年中国具身智能 AI 云市场 1800 万美元、2030 年 4.08 亿美元（CAGR 69%，份额百度 35%/阿里 17%/腾讯 16%）（[新浪财经](https://finance.sina.com.cn/roll/2026-02-09/doc-inhmfive5356553.shtml)、[36氪](https://36kr.com/p/3855538404988167)）。

## 1. 刻行时空 coScene（最直接对标）

「时空多模态数据平台」，SceneOps 概念，覆盖研发-测试-生产-运维数据闭环。四大模块：数据平台、可视化播放器、测试平台、边端控制台（[官网](https://www.coscene.cn/)、[文档](https://docs.coscene.cn/en/docs/overview/)）。2022 年成立于上海。

| 维度 | 结论 |
|---|---|
| 格式 | MCAP 为中心；rosbag .bag/.db3、pcd、MP4、log；开源 [hdf5-mcap-converter](https://github.com/coscene-io/hdf5-mcap-converter)。**LeRobot：官网/文档/changelog/GitHub 42 个仓库均无支持证据** |
| 可视化 | 网页 viewer 是 Foxglove Studio fork（[honeybee](https://github.com/coscene-io/honeybee)）；边端实时桥 [cobridge](https://github.com/coscene-io/cobridge)；插件系统（v26.3.0）、H265（v26.6.0）。未见"浏览器直连对象存储流式读" |
| 检索/质检 | 云端规则引擎自动诊断（[文档](https://docs.coscene.cn/docs/recipes/data-diagnosis/rule-engine/)）；标签筛选批量下载（[用例](https://docs.coscene.io/docs/use-case/heterogeneous-robot-data-factory/)） |
| 部署 | SaaS/多租户/单租户/混合/私有化五种（[comprehensive-support](https://www.coscene.cn/comprehensive-support)） |
| 开源 | 平台核心不开源；边缘侧工具链开源（[GitHub](https://github.com/coscene-io)） |
| 商业化 | 定价未公开；客户：高仙、苏泊尔；线性资本被投（[Linear Capital](https://www.linear.vc/portfolio-cn)） |
| 动态 | 2025-04 SceneOps；2025-05 云端仿真测试+异构数采；产品迭代至 2026-03（v26.10.0）。未见 LeRobot 整合 |

护城河在数采闭环（边端 agent + 规则引擎），可视化是入口不是壁垒；格式路线与我们错位。

## 2. 云厂商

### 2.1 火山引擎 — 无专门产品

- 相关能力 = LAS + TOS + Lance/Iceberg + Daft/Ray（[LAS 产品页](https://www.volcengine.com/product/las)）。
- 官方博客："采用 Iceberg 和 Lance 作为核心湖格式，同时支持具身智能领域常用的 MCAP 和 LeRobot 格式"；"引入 Daft 作为核心计算引擎"（[博客](https://www.cnblogs.com/bytedata/p/19192773)，已核实）。
- 无机器人 episode 回放类网页可视化产品。2025-12 与优必选子公司优奇签具身合作（[界面](https://www.jiemian.com/article/13796200.html)）。
- 含义：火山留的是"数据湖底座"位，垂直应用层是空位 —— 我们恰好补这层。

### 2.2 阿里云 — AnalyticDB 具身智能平台（最接近我们形态）

- 一体化平台（约 2026 年中）：设备管理、数据管理、标注（Label Studio）、训练（内置 GR00T、π0.5）、仿真（Isaac Sim）（[概述](https://help.aliyun.com/zh/analyticdb/analyticdb-for-mysql/overview)，已核实）。
- 格式（已核实）：LeRobot 2.x/3.x、Unitree、EgoVerse Zarr、HDF5，Ray 分布式转换（[文档](https://help.aliyun.com/zh/analyticdb/analyticdb-for-mysql/data-management)）。未提 rosbag/MCAP。
- 可视化（已核实）：网页 Dataset Viewer（帧级数据表格、Episodes 缩略图）；OSS 导入（填 Endpoint/Bucket/AK）—— "导入托管"而非直读桶。
- 质检：episode 审核（通过/不通过 + 1-5 星 + 备注）、按审核状态与标签筛选。
- 公有云控制台，未见私有化。2026-06 发布 Qwen-Robot（[量子位](https://www.qbitai.com/2026/06/435873.html)）。

### 2.3 华为云 — CloudRobo（2026-06-30 公测）

- 数据合成、标注、模型开发、Real-Sim 评测、R2C 协议（[产品页](https://www.huaweicloud.com/product/cloudrobo.html)、[财联社](https://www.cls.cn/detail/2391562)）。
- SDK 绑定 LeRobot 生态（匹配 LeRobot-v0.5.1，[SDK 文档](https://support.huaweicloud.com/sdkreference-cloudrobo/cloudrobo_03_0002.html)）。
- 差异化在合成数据："训练样本 20% 采集、80% 生成"。未见独立网页可视化工具。

### 2.4 百度智能云 — 无单一平台，商业化最强

- 百舸 5.0（HDF5/RLDS→LeRobot 转换、导出 LeRobot V3.0，[文档](https://cloud.baidu.com/doc/AIHC/s/Nmdsnpzfj)）+ 具身智能数据超市 Beta（2026-04，[报道](https://www.163.com/dy/article/KQ6SVN4205118HA4.html)）。
- 无网页可视化产品。Omdia 1H25 市占 35%，服务智元、宇树等 30+ 企业（[人民日报](http://paper.people.com.cn/rmrb/pc/content/202505/29/content_30076067.html)）。

## 3. 机器人公司

### 3.1 智元 AgiBot

- **AgiBot World**：100 万条轨迹/约 44-48 TB/2976 小时（[GitHub](https://github.com/OpenDriveLab/AgiBot-World)、[HF](https://huggingface.co/datasets/agibot-world/AgiBotWorld-Beta)）。原生自有结构（MP4+PNG 深度+HDF5+JSON），官方经 any4lerobot 转 LeRobot（已核实）。License CC BY-NC-SA 4.0 禁商用。
- **官方可视化脚本直接用 Rerun**（README："will open rerun.io"，已核实）—— 对我们是直接背书。
- **Genie Studio**（2025-04）：行业首个具身一站式商业化开发平台（[官网](https://genie.agibot.com/geniestudio)）。Genie Sim 3.0 开源仿真（CES 2026）；2025 营收破十亿元。

### 3.2 宇树 Unitree

- HF 98 个开源数据集（2026-08），**原生 LeRobot v2.0、Apache 2.0**（[HF](https://huggingface.co/unitreerobotics)）。
- 工具链开源：[xr_teleoperate](https://github.com/unitreerobotics/xr_teleoperate)、[unitree_lerobot](https://github.com/unitreerobotics/unitree_lerobot)。
- 无自研可视化/质检产品，依赖 LeRobot/HF 生态。

### 3.3 银河通用 Galbot

- 合成数据路线：DexGraspNet、SynGrasp-1B 十亿帧（2026-08，CC BY-NC，[GraspVLA](https://github.com/PKU-EPIC/GraspVLA)）。管线不对外产品化；2025-06 融资 11 亿元（[新华网](https://www.news.cn/digital/20250623/00657ee2fbde4b4d8c005ea667b31737/c.html)）。

### 3.4 星海图与松灵

- **星海图**：Galaxea Open-World Dataset（2025-09，500+ 小时，**原生 LeRobot v2.1**，粗/细两级质量标注，[HF](https://huggingface.co/datasets/OpenGalaxea/Galaxea-Open-World-Dataset)、[arXiv](https://arxiv.org/abs/2509.00576)）。
- **松灵**：卖数采硬件（Cobot Magic、Pika），采集侧 rosbag/HDF5 转 LeRobot；无软件产品（[官网](https://global.agilex.ai/products/cobot-magic)）。

## 4. 国家级机构

- **北京人形创新中心**：RoboMIND 2.0（310K+ 轨迹/6 本体/739 任务，[arXiv](https://arxiv.org/abs/2512.24653)）；格式 HDF5，官方开源[转 LeRobot 工具链](https://github.com/x-humanoid-robomind/x-humanoid-training-toolchain)。无对外数据平台；石景山训练场输出数采服务（[新华网](https://www.news.cn/photo/20260613/8ef90bfc314447708c3b9ae17d6d976f/c.html)）。
- **智源 BAAI**：[具身数据平台](https://ei2data.baai.ac.cn/home)（任务管理、遥操采集、标注、数据资产管理，申请制）；RoboBrain/RoboOS 开源（[GitHub](https://github.com/FlagOpen/RoboOS)）。
- **上海国地中心**：白虎数据集（超 100 万条、2.5PB，自有格式，[国家数据局](https://www.nda.gov.cn/sjj/ywpd/szkjyjcss/1031/20251031193216150911981_pc.html)）；麒麟训练场。
- 广东/成都训练场：政策+基建阶段，无对外产品。

## 5. 标注服务商

| 公司 | 具身动作 | 定性 |
|---|---|---|
| 整数智能/Abaka AI | Embodied AI 三大垂直之一；MooreData 平台 | 标注平台+服务，无机器人格式证据 |
| 海天瑞声 | 石景山训练场共建；"具身数据工程化服务平台"；2025 营收 3.77 亿（+59%） | 绑定政府资源最深，平台细节零公开 |
| 云测数据 | 未列具身专项 | 尚未入场 |
| 数据堂 | Physical AI 数据金字塔、8000 平米基地 | 卖数据集+服务 |
| 景联文 | 三大数采基地，2026-07 超亿元采购计划 | 人力密集型 |
| 曼孚科技 | MindFlow SEED 含具身 | 标注工具 |

小结：全线转具身，但没有一家做出面向客户的数据管理/可视化/质检软件 —— 工具断层明显。

## 6. 新势力（2025-2026）

- **无问智科**："物理 AI 数据基座"，2026-04 超亿元融资，客户含字节（[中国日报](https://cn.chinadaily.com.cn/a/202604/24/WS69eb2422a310942cc49a94a7.html)）。
- **光轮智能**：具身数据独角兽，2026-05 估值 20 亿美元+，2026 Q1 新订单 5.5 亿元（[投资界](https://news.pedaily.cn/202603/561585.shtml)）。
- **极佳科技 GigaAI**：世界模型即数据引擎，开源 GigaWorld。
- **它石智航 TARS**：Pre-A 4.55 亿美元（中国具身史上最大单轮，[央广网](https://www.cnr.cn/mspd/jrhm/20260417/t20260417_527588908.shtml)）。
- **灵初智能 PsiBot**：10 万小时人手数据，累计融资 20 亿元。
- 其余：千寻智能、星尘智能、帕西尼、鹿明、枢途、穹彻等（[艾邦 25 家盘点](https://www.aibangbots.com/a/11921)）。

## 7. 竞争定位结论

| 能力 | 我们 | 最近的竞对 | 差距/机会 |
|---|---|---|---|
| 浏览器直读对象存储 LeRobot | 有 | 全市场无人做到；阿里云走导入托管 | 独有差异点 |
| LeRobot 商业化工具链 | 核心 | coScene 不支持；云厂商绑自家云 | 国内空白带 |
| 质检台直挂桶 | 有 | 阿里云 episode 星级审核 | 免搬移是差异点 |
| 私有化部署 | 有 | coScene 有但不公开细节；云厂商绑公有云 | 数据不出域刚需 |
| 数采闭环 | 无 | coScene 护城河 | 勿正面竞争 |
| 合成数据/仿真 | 无 | 华为云、光轮、智元 | 可作上游对接 |

三点战略判断：

1. 卡位"LeRobot 生态 × 对象存储直读 × 私有化"三者交集，目前国内无直接竞对；最大威胁是阿里云向直读桶演进、智元 Genie Studio 向第三方开放。
2. Rerun 已被智元官方工具链采用，国内尚无公司基于 Rerun 商业化 —— 先发窗口存在但不长期。
3. 火山数据湖支持 MCAP/LeRobot、计算引擎用 Daft —— 与火山是"底座+应用层"互补，生态顺风。
