# DDScopeHost Phase 1 M5 极限性能压测、边缘异常加固与 MVP 验收报告（Gate 4）

> **对应里程碑**：Phase 1 Milestone M5（`T08` / Gate 4：极限性能压测、边缘异常加固与 MVP 验收）  
> **验收结论**：**通过（PASS）** — 全工作空间零告警零失败，30 秒 Release 混合信号极限压测与桌面端安装包构建全部达标。

---

## 1. 验收结论与 Gate 4 总览

| 验收维度 | 目标基线（Gate 4 / DoD） | 实测结果 | 状态 |
| --- | --- | --- | --- |
| 多协议并发与自适应 PLL | `Sim` + `UDP` + `UART`（`FireWater`）并行采集与同步绘图 | `dod_1_concurrent_multi_source_sim_udp_and_serial_firewater_pll` 通过 | 通过 |
| 三轨 MSO 对齐与窄脉冲不漏采 | `20s` 跨度下模拟/数字/离散状态三轨时间对齐，1-sample 毛刺不丢失 | `40,042,496` pts/ch 压测与 `dod_2` 验证通过 | 通过 |
| 独立单次触发锁存与高倍缩放 | `Single` 触发锁存跨 `>2x` 配额淘汰、`10,000x` 缩放与 `Rearm` 释放 | 锁存锁定 `830,208 B`，缩放峰值 `103.98438 V`，`Rearm` 后 `0 B` | 通过 |
| 有背压下行闭环与高优抢占 | `115200 baud` 背压下高低频参数合并与 `High` 急停优先抢占 | 100 次 `kp` 突发电报合并发射，急停抢占 `< 15 ms` | 通过 |
| 极端断流与时钟重启恢复 | 串口拔插、半截粘包、MCU Tick 归零及 TCP 自动重连无崩溃 | 4 项 `e2e_m5_hardening_tests` + Windows UDP `10054` 容错全通过 | 通过 |
| Release 吞吐与打包延迟 | `> 5,000,000 pts/s`、`p99 < 500 µs`、`0` 丢包、`<= ±5%` 内存波动 | **`7,023,527 pts/s`**、`p99 = 53.00 µs`、`0` 丢包、**`±0.45%`** 波动 | 通过 |
| 安装包构建与桌面端冒烟 | Release 编译、Tauri v2 桌面端启动冒烟与 Windows 安装包产物 | `.msi` (`4.06 MiB`) + `-setup.exe` (`2.74 MiB`) + 冒烟退出码 `0` | 通过 |

---

## 2. M5 边缘异常加固与核心修复清单

1. **串口 / TCP 热插拔断流状态机、缓冲区重置与 MCU 时钟重启恢复**
   - 文件：`crates/ddscope-core/src/transport/{mod,buffer_pool,replay,scheduler,serial,net}.rs`、`crates/ddscope-core/src/workspace.rs`
   - 加固点：完善 `Connecting` / `Reconnecting` 瞬态传导，断流重连时清空残留半截协议帧并重置解码状态，安全吸收 MCU 重启导致的硬件 Tick 计数器归零跳变，避免出现负时间差崩溃或死锁。
2. **Windows 非连接 OS UDP `WSAECONNRESET` (`os error 10054`) ICMP 端口不可达容错**
   - 文件：`crates/ddscope-core/src/transport/net.rs`（`OsUdpSocketHandle::recv_from`）
   - 问题根因：Windows 默认开启 `SIO_UDP_CONNRESET`，向未监听远端端口发送双向 UDP 下行指令后，后续 `recv_from` 会收到 `io::ErrorKind::ConnectionReset`（`os error 10054`），导致 `UdpTransport` 误入 `Error` 状态并使 `stop_source` 报错。
   - 修复方式：在 `OsUdpSocketHandle::recv_from` 内捕获瞬态 `io::ErrorKind::ConnectionReset` 并 `yield_now()` 后继续等待数据报（保持 `VirtualUdpHub` 关闭语义不变），确保 `td1_control_plane_tests::live_workspace_control_plane_roundtrip` 与真实双向 UDP 长跑稳健运行。
3. **宏观视口（大跨度降采样）数字边沿防漏抽取与 `EventSeries` 配额公平淘汰**
   - 文件：`crates/ddscope-core/src/storage/event_series.rs`（`extract_chunks_state_ribbons`、`collect_event_transitions_in_window`）、`crates/ddscope-core/src/storage/hub.rs`（`ChannelHub::enforce_global_quota`、`inspect_oldest_sealed`）、`apps/web-ui/src/render/ScopeGeometryBuilder.ts`（`emitDigitalEdges`）
   - 加固点：修复 `EventDataChunk::StateTransitions` 窗口边界状态跳变漏采问题，保障超密集方波突发（`< 2 px`）及孤立单样本毛刺在前后景视口抽取下保持终端奇偶一致性；当某通道最前序 sealed chunk 被 `Single` 锁存钉住（`Arc::strong_count > 1`）时，`ChannelHub::enforce_global_quota` 按全局最老**未锁定** sealed chunk 时间戳公平淘汰，避免饿死其他通道，同时将 `.is_none_or` 替换为兼容 Rust MSRV `1.80.0` 的 `match` 分支。
4. **前端重连状态指示、单次锁存恢复联动与 Release 桌面冒烟钩子**
   - 文件：`apps/web-ui/src/i18n/index.ts`、`apps/web-ui/src/workspace/ChannelTreeSidebar.tsx`、`apps/web-ui/src/styles/global.css`、`apps/web-ui/src/workspace/store.ts`、`apps/web-ui/src/workspace/WorkspaceShell.tsx`、`apps/desktop/src/lib.rs`
   - 加固点：补全 `sidebar.state.connecting` / `sidebar.state.reconnecting` 中英双语词条与脉冲指示样式；修复 `rearmScope` 同步解除 `paused` 锁存并让 `handleReleaseAllPins` 对全部示波器执行 Rearm；移除 `DDSCOPE_SMOKE_EXIT_MS` 的 `debug_assertions` 条件编译限制（新增 `ms > 0` 守卫），支持 Release 二进制自动化启动冒烟检查。
5. **CLI 自动化长稳压测基准命令 (`ddscope-cli soak`)**
   - 文件：`crates/ddscope-cli/Cargo.toml`、`crates/ddscope-cli/src/main.rs`（`Commands::Soak`、`SoakReport`、`run_soak`、`print_soak_report`）、`crates/ddscope-cli/tests/cli_t04_tests.rs`
   - 能力：内置高吞吐混合信号并发采集、`120 FPS` `DDSF v1` 连续打包、全局环形配额滚动、`Single` 毛刺锁存/缩放置信度及 `Rearm` 自动校验，并支持一键输出机器可读 JSON 报告。

---

## 3. Phase 1 MVP DoD 与 M5 加固测试矩阵

### 3.1 四大 MVP DoD 端到端验收测试（`apps/desktop/tests/m5_acceptance_dod_tests.rs`）

| DoD 条目 | 对应测试符号 | 覆盖路径与关键断言 | 状态 |
| --- | --- | --- | --- |
| **DoD #1** | `dod_1_concurrent_multi_source_sim_udp_and_serial_firewater_pll` | 桌面运行时 `sim0` + Loopback `UDP` + `VirtualSerialPair`（`uart1` `FireWater` 二阶 PLL）并发采样与 `ChannelTree` / `DDSF v1` 同屏交付 | 通过 |
| **DoD #2** | `dod_2_mso_20s_three_lane_glitch_and_state_ribbon_alignment` | `20s` 跨度下模拟轨、数字边沿轨与离散状态色带 X 轴统一对齐，1-sample 毛刺保留无漏采 | 通过 |
| **DoD #3** | `dod_3_independent_single_latch_across_2x_quota_rollover_10000x_zoom_and_rearm` | `Single` 模式捕获异常峰值并跨 `> 2x` 配额淘汰持续锁存，`10,000x` 微观缩放无锯齿无缺损，调用 `rearm_scope` 后立即解锁回归实时配额滚动 | 通过 |
| **DoD #4** | `dod_4_closed_loop_downlink_tx_coalescing_under_115200_baud_backpressure` | `Paced115200SerialProvider`（16 字节硬件 FIFO、`86.8 µs/B` 流控）下突发 100 次 `kp` 更新自动合并覆盖（最新值胜出），同时 `High` 优先级 `STOP` 急停指令在 `< 15 ms` 内插队抢占 | 通过 |

### 3.2 核心引擎极限异常加固测试（`crates/ddscope-core/tests/e2e_m5_hardening_tests.rs`）

| 测试符号 | 验证场景 | 状态 |
| --- | --- | --- |
| `m5_serial_hotplug_midframe_and_mcu_tick_reset_recovery` | 串口突发断流拔插、半截协议帧损坏及 MCU 硬件 Tick 归零重启恢复 | 通过 |
| `m5_tcp_client_auto_reconnect_and_state_propagation` | `DuplexTcpNetwork` 下 `TcpClient` 断线自动重连与 `Reconnecting -> Running` 状态传导 | 通过 |
| `m5_macro_20s_mso_viewport_latency_and_parity_invariants` | `20 s @ 2 MHz`（`40,042,496` pts/ch）三轨 MSO 毫秒级抽取延迟、1-sample 毛刺及边沿奇偶一致性 | 通过 |
| `m5_event_series_and_scalar_multi_round_quota_rollover_with_pinning` | `4 MiB` 配额下 `EventSeries` + `ScalarSeries` 多轮滚动、`Single` 锁存护盾与 `rearm_scope` 释放 | 通过 |

---

## 4. Release 极限混合信号压测实测报告与长稳指南

### 4.1 30 秒 Release 门禁实测指标（直接读取自 `target/m5_soak_report.json`）

执行命令：

```powershell
.\target\release\ddscope-cli.exe soak --duration-secs 30 --verify-dod --json-out target/m5_soak_report.json
```

| 指标字段 | 实测值 | 门禁要求 |
| --- | --- | --- |
| 并发规模 | `sources = 2`，`total_channels = 16`，`per_source_rate_hz = 501428.57 Hz`，`fps = 120` | 多源混合并发 |
| 实际时长 (`duration_secs`) | `30.0003424 s` | `>= 30 s` |
| 总采样点数 (`pts_delta` / `rows_delta`) | `210,708,210 pts`（`30,083,982 rows`） | 持续高负载滚动 |
| **聚合吞吐量 (`achieved_pts_per_sec`)** | **`7,023,526.84 pts/s`** | `>= 5,200,000 pts/s`（DoD `> 5M pts/s`） |
| **丢包 / 解码错误** | `dropped_packets = 0`，`decode_errors = 0` | 严格为 `0` |
| **DDSF v1 打包耗时 (`frames_packed = 3,601`)** | **`p50 = 21.9 µs`**，**`p99 = 53.0 µs`** | `p99 < 500.0 µs`（裕度约 `9.4x`） |
| 全局内存配额与稳态控制 | `quota_limit = 33,554,432 B (32 MiB)`，`peak = 33,548,544 B`，`evicted_total = 206,370,156` | 无 OOM / 配额稳定 |
| **稳态内存波动 (`steady_mem_samples = 3,523`)** | 均值 `33,541,605.17 B`，区间 `[33,244,416 .. 33,548,544] B`，**波动 `±0.4534%`** | `<= ±5.0%` |
| **DoD 毛刺锁存闭环 (`verify_dod = true`)** | `glitch_latched = true`，`glitch_pinned = true`（`830,208 B`），`zoomed_ch3_max = 103.98438 V`，`rearmed_pinned_bytes = 0` | 全链路无损闭环 |
| 综合判定 (`passed`) | **`true`**（退出码 `0`） | `true` |

### 4.2 宿主机 30 分钟长稳压测建议命令

为在交付前或硬件联调前独立复核 30 分钟（`1800 s`）连续稳定运行能力（本次交付已跑完 30 秒 Release 门禁，未在对话内执行 30 分钟长跑），可在宿主机终端直接执行：

```powershell
.\target\release\ddscope-cli.exe soak --duration-secs 1800 --verify-dod --json-out target/m5_soak_30m_report.json
```

---

## 5. 全量工程门禁、桌面端冒烟与构建产物清单

1. **架构边界与代码质量门禁**
   - `cargo clippy --workspace --all-targets -- -D warnings`：退出码 `0`（零警告零错误）。
   - `cargo tree -p ddscope-core -e normal`：退出码 `0`，确认 `ddscope-core` 零 `tauri` / `wry` / UI 框架依赖。
   - `pnpm -r typecheck`：退出码 `0`。
2. **全量自动化测试结果**
   - 前端单元测试（`pnpm --filter @ddscope/web-ui test`）：**20 passed, 0 failed, 0 skipped**（耗时 `374.791 ms`）。
   - Rust 全工作空间测试（`cargo test --workspace`）：**1002 passed, 0 failed, 0 ignored**（涵盖 `ddscope-plugin-api`、`ddscope-core` 全部单测/对抗/集成/277 项冻结基准与 `e2e_m5_hardening_tests`、`ddscope-cli` 4 项 `cli_t04_tests`、`ddscope-desktop` `m4_fullstack_integration_tests` / `m5_acceptance_dod_tests` / `td1_control_plane_tests` / `td2_data_bridge_bench`）。
3. **生产构建、Windows 安装包与 Release 桌面端冒烟**
   - 前端生产产物（`pnpm --filter @ddscope/web-ui build`）：`152 modules transformed`，构建完成（`1.17 s`）。
   - Rust Release 编译（`cargo build --release --workspace`）：构建完成（`31.94 s`）。
   - Windows Tauri v2 完整打包（`pnpm --filter @ddscope/desktop build`）：退出码 `0`，生成以下交付产物：
     - 独立桌面端可执行文件：`target/release/ddscope-desktop.exe`（`12,624,384` bytes）
     - 独立 CLI 工具与压测器：`target/release/ddscope-cli.exe`（`3,023,360` bytes）
     - MSI 安装包：`target/release/bundle/msi/DDScopeHost_0.1.0_x64_en-US.msi`（`4,255,744` bytes / `4.06 MiB`）
     - NSIS 安装包：`target/release/bundle/nsis/DDScopeHost_0.1.0_x64-setup.exe`（`2,876,752` bytes / `2.74 MiB`）
   - 桌面端 Release 实机启动冒烟（在安装包打包完成后使用 `DDSCOPE_SMOKE_EXIT_MS=1500` 启动 `target/release/ddscope-desktop.exe` 并等待进程退出）：`smoke_exit_code=0`。
