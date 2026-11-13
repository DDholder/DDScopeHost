# DDScopeHost 架构与详细设计文档索引

本目录存放 **DDScopeHost** 超高速通用测控上位机的完整架构基线与模块接口规范。

---

## 文档导航

1. **[01-system-architecture.md](./01-system-architecture.md) — 系统总体架构设计**
   - 项目定位、核心性能指标（**> 1M ~ 10M 点/秒**）与分层全景图
   - 多数据源并发融合、三级混合时钟对齐、零拷贝二进制通道、M4 降采样金字塔等关键架构决策
   - 完整技术栈清单（Rust + Tauri v2 + React 19 + WebGPU）与 Monorepo 目录规划
2. **[02-core-modules-design.md](./02-core-modules-design.md) — Rust 核心引擎详细设计 (`ddscope-core`)**
   - 多源无锁并发流水线、接口层范围与原始字节抓包（`.bin`）、发送合并队列（Coalescing）
   - 三级混合时钟与软件 PLL 突发去抖动算法
   - 双向协议引擎：`.ddproto` 声明式多帧 `MsgID` 路由编译引擎、**C `struct` 粘贴一键导入**、下行 **ACK 请求-应答事务跟踪器**
   - 原生位宽三态通道存储（`ScalarSeries` / `FrameSeries` / `EventSeries`）、`Chunk Pinning` 防覆盖钉住与 `.ddcap` 流式 `mmap` 落盘
   - 虚拟通道 DAG：数学公式字节码编译器 + 多进多出有状态算子（内部 `f64` 高精度）+ **视口优先历史即时回算**
3. **[03-ui-and-oscilloscope-design.md](./03-ui-and-oscilloscope-design.md) — 跨端通信、React 19 工作区与 MSO 虚拟示波器设计**
   - `tauri-specta` 强类型控制面与 `DDSF` 多示波器零拷贝二进制帧内存布局
   - **MSO 混合信号实时虚拟示波器**：独立视口触发锁存、`SyncGroup` 时间轴联动、模拟波形 + 数字逻辑时序带 + 状态机跳变色带 + FFT 频谱帧同屏对齐
   - `Dockview` 多视口停靠 + 24/48 列磁吸自适应网格画布、React Flow 算子连线图、双向控件绑定与事务回显规范
   - 工程资产体系（`.ddscope.json` / `.ddproto.yaml` / `.ddcap`）与 Phase 1 ~ Phase 4 实施路线图
4. **[04-phase1-design-and-plan.md](./04-phase1-design-and-plan.md) — Phase 1 (MVP) 需求拆解、详细设计方案与开发实施计划**
   - Phase 1 MVP 范围界定（In-Scope vs. Deferred 前向兼容预留）、功能需求矩阵（FR）与性能基线指标（NFR）
   - 系统分层结构图、`ddscope-core` 核心类与 Trait 框架图、无锁并发流水线时序图、独立触发状态机与 WebGPU MSO 分层渲染框架图
   - `DDSF v1` 64 字节对齐二进制内存布局规范、三级混合时钟/二阶软件 PLL、64 叉 M4 金字塔与 `Chunk Pinning` 实现细则
   - **契约先行双端解耦策略**、M0 ~ M5 六阶段划分、**四轨串并联开发主流程图（含关键路径标注）**与 WBS 依赖关系详表

---

## 核心架构决策速查表（ADR Summary）

| 领域 | 已锁定架构方案 |
| :--- | :--- |
| **整体技术栈** | `Rust` 核心 (`ddscope-core`) + `Tauri v2` + `React 19` / `TypeScript` + `WebGPU`（`WebGL2` 回退） |
| **数据源拓扑** | **多源并发融合**：支持串口、TCP、UDP、原始文件回放、内置仿真源同时运行，汇聚为 `<source_id>/<path>` 全局通道树 |
| **高速时钟与去抖动** | **三级混合时钟**：① 硬件时间戳线性漂移锚定；② 软件 PLL 重构等间隔采样消除收包突发抖动；③ 主机单调到达时间 |
| **自定义协议与结构体** | 声明式 **`.ddproto` 多消息 `MsgID` 路由配置**（编译为扁平指令表高速执行）+ **支持直接粘贴 C `struct` 一键导入** |
| **下行反向控制** | **双模式**：高频调节走 `Fire-and-Forget` + 队列最新值覆盖合并；关键指令走 `ACK 事务跟踪`（超时重试 + UI 成功/失败回显） |
| **通道精度与形态** | **原生标量位宽列式存储**（省 50%~75% 内存带宽，有状态算子内部用 `f64`）+ **标量流 / 数组帧流（FFT）/ 结构体事件流**三形态 |
| **海量存储与录制** | **内存配额环形窗口 + 可选 `.ddcap` 流式落盘**（列式块 + M4 金字塔索引，`memmap2` 支持边录边回溯数十 GB 历史） |
| **虚拟通道编排与回算** | **「数学公式表达式 + React Flow 节点连线图」双入口**；暂停或复盘时新建/改参自动触发**视口优先历史分块即时回算** |
| **虚拟示波器架构** | **多实例独立触发锁存**（单台 `Single` 触发时自动 `Pin` 住内存块防覆盖，不打断其他示波器/仪表）+ **`SyncGroup` 联动** + **MSO 模拟/逻辑/状态机色带同屏** |
| **面板拖拽布局** | **`Dockview` 多窗口停靠分屏** + **24/48 列细粒度磁吸自适应网格画布**（支持从全局通道/命令树直接拖拽到控件完成绑定） |
