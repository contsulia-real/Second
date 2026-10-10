# Currency 区间化验收与超时调查

本记录对应未发布的协议版本 1 区间表示调整。生产共识、签名域和任务规则保持原语义。本次追加修正仅涉及测试等待、测试快照观察与文档；以下生产路径只调查，不修改。

## 最初失败与复现频率

2026-10-10 06:25（Asia/Shanghai）的首次全量运行中，集成测试为 248 通过、3 失败，耗时 90.22 秒。三个失败分别是：

| 测试 | 原失败时的可观察事实 |
| --- | --- |
| `runtime_allocation::rejected_allocation_does_not_poison_restart_or_later_issue` | generation=12，frontier=2，目标任务未成功；round=0，已有 lock 和有效 QC，errors={}；5 秒总等待到期 |
| `runtime_allocation::exhausted_competing_allocation_rejects_without_stopping_other_business` | generation=16，frontier=u64::MAX，一项发行已成功；仅有预期的 CurrencySequenceSpaceExhausted 错误，后续普通业务尚未完成；5 秒总等待到期 |
| `runtime_bft::four_validator_runtimes_drive_consensus_to_certified_public_checkpoint` | 失败在连接准备阶段；验证者 3 的出站 BFT 连接只有验证者 1，其余三个节点已各有 3 条出站连接；5 秒总等待到期 |

这是一轮实际失败，不能据此估算普通机器负载下的长期失败率。随后受控实验只筛选这三条测试，每个测试进程使用 `--test-threads=3`：

| 条件 | 结果 |
| --- | --- |
| 16 份测试进程共享同四个逻辑 CPU，旧判定 | 48/48 通过 |
| 32 份测试进程共享同一个逻辑 CPU（ProcessorAffinity=1），旧 5 秒判定 | 5/96 通过，91/96 失败（94.8%） |
| 相同单核、32 份进程，仅把测试观察预算临时延长到 60 秒 | 96/96 通过，无需修改生产运行时或重启恢复 |
| 上一轮进度等待策略，同单核压力，连续两轮 | 192/192 通过；该策略当时尚未对首次观察启动空闲计时，不能当作当前修正后的验收结果 |

受控实验覆盖的失败率是三个测试的合计，并非每条测试各自的失败率。原始临时日志已按清理要求删除；以下摘录重新核对自当前任务保留的工具执行记录（2026-10-10 08:06、08:08、08:09）：

```text
timeout-trace startup node=Some(ValidatorId(1)) queue=588.2841ms work=7.2560556s
timeout-trace lock ...second-rejected-allocation-restart-31720-... elapsed=10.5416933s
timeout-trace drive ... queue=54.4467ms work=12.3039467s pending=true deadline=None
second-timeout-onecore completed 32 passed 5 failed 91
second-timeout-observe completed 32 passed 96 failed 0
allocation count 96 min 0.0764103 max 5.5795239 over5 2
connection count 32 min 9.1973048 max 23.8434225 over5 32
```

证据确认：原测试把启动、存储同步/锁等待、连接握手与重试压在同一个 5 秒总期限内，能把仍在推进、最终自行完成的流程判为失败。延长观察实验是同压力条件的独立运行，并非让已经被测试 abort 的原进程继续执行。采样中的锁/工作耗时包含调度等待，不能把它解释成纯磁盘耗时。最初失败轮没有 CPU/磁盘性能采样，因此不能声称已查明它当时具体的资源占用来源，也不能据此证明生产共识不存在任何偶发活性问题。

## 当前 ProgressDeadline

`tests/integration/support/progress.rs` 的第一次 observe 同时记录 last 和 changed。从第一份观察起，未变化超过 idle_budget 即失败；以后变化会更新 changed。60 秒整体硬期限不续期，且包住异步 probe。分配等待空闲预算为 5 秒，连接等待为 15 秒。连接预算覆盖已有 5 秒传输、5 秒认证和 2 秒维护重试间隔。

`tests/integration/progress_deadlines.rs` 使用受控 Instant 的同一个测试覆盖四种情况：持续进展超过一个空闲预算仍可继续、起初从未进展在空闲期限失败、中途停滞失败、即使继续变化也不能超过整体期限。测试无真实 sleep，且只由集成测试主模块加载一次。

## tests/ 固定 timeout 与快照轮询检查

检查包括 `tokio::time::timeout` 的 async 块和其中多行 `.load()`，并排除了 AtomicBool/AtomicUsize 的 `load(Ordering)`。同类直接同步快照轮询均已改为通过测试 support 在 spawn_blocking 中读取；设置、结束后的 cold-read 断言及失败诊断仍可同步执行。批量观察一次提交到阻塞池，不为每个节点各启一份工作任务。

| 位置（函数或场景） | 期限处理与理由 |
| --- | --- |
| `runtime_allocation.rs`：`certified_ranges_resume_after_crash_and_failed_business_never_reallocates` 的两次 5 秒等待 | 与最初分配失败相同的启动/恢复路径，改为进度等待；保留任务、身份消费和重启断言 |
| `runtime_allocation.rs`：竞争分配、分配与成员切换竞争，两处 20 秒等待 | 保留整体收敛期限；前沿竞争及重计划必须在有界时间完成，其他 generation 活动不能代替目标任务/版本完成 |
| `runtime_allocation_capacity.rs`：backlog 恢复，60 秒 | 保留批量 drain 整体期限，避免其他任务进展掩盖队尾永远饥饿 |
| `cli_business_client.rs::wait_success`，30 秒 | 保留 CLI 请求在所有目标节点完成的整体期限，避免只证明节点还有其他活动 |
| `runtime_checkpoint_connectivity.rs`：stalled dial 后认证，15 秒 | 保留故障注入后的恢复完成上限；失败/重连事件的时间测试不改成可续期等待 |
| `runtime_conflicts.rs::run_resource_conflicts`，两处 90 秒 | 保留全部冲突任务的终态及在线 quorum 恢复期限，防止少数任务持续换轮掩盖遗漏任务 |
| `runtime_finality_recovery.rs`，12 秒 | 保留缺席拜占庭成员后的实际终态恢复期限；无关 generation 变化不代表所需最终性已达成 |
| `runtime_governance_restart.rs`，三处 10 秒及一处 55 秒 | 保留具体 serial、成员版本或历史 QC 恢复的整体期限；必须完成准确目标，不能仅靠其他写入续期 |
| `runtime_historical_source.rs`，10 秒 | 保留指定旧任务认证 Abort 完成期限，而非一般节点活跃性 |
| `runtime_public_checkpoint.rs`，60 秒多阶段流程及 10 秒重启 | 保留准确 epoch/digest 的传播、追赶与重启完成期限；公开 PublicStateStore 的轮询也放入阻塞池 |
| `runtime_validator_discovery.rs`，90 秒 | 保留 34 验证者中全部目标节点提交的总期限，防止大部分节点进展掩盖个别节点永久不收敛 |

这些总期限仍可能在严重资源饥饿时失败，这是有界端到端验收的限制；本轮没有用“预算较大”宣称它们绝无误判风险。其余固定 timeout 只轮询连接/事件/原子量或调用网络请求，没有直接 `store.load()`；原有时效断言继续保留。

## src/ 生产快照调用调查（只读）

`StateStore::load`（`src/persistence/store.rs:152`）调用同步 load_shared 并 clone 整份 PersistedNodeState。load_shared 仍要取得进程/文件锁和读取 token；缓存失效时会读完整 slot 并解码验证，不能因为返回 Arc 就认定不会阻塞。写入最终进入 write_next_unlocked，整份编码并同步 slot/commit reference。下表将直接执行与已经隔离的路径分开；行号以本次检查源码为准。

### 未隔离的 StateStore 路径

| 位置 | 异步调用上下文与快照操作 |
| --- | --- |
| `src/runtime_public_sync.rs:23` | `sync_freshest_certified_public_currency_view` 的 full-store 分支直接 load；由钱包 `WalletNode::sync` 和 `observe_public_network` 调用 |
| `src/cli_node.rs:39` | `node_with_shutdown` 在 `node`/`node-check` 启动中直接 load，随后才进入 runtime.run；属于启动阶段，不是入站请求循环 |
| `src/cli_public.rs:108,129` | `sync_public_certified` 在发起网络同步前 load 信任快照；返回后 advance_checkpoint_floor，内部会整份快照写入 |
| `src/cli_public.rs:140` → `src/runtime.rs:188` | `observe_public_network` 调用同步 `NodeRuntime::load_and_bind`，内部直接 load，然后 bootstrap/同步 |
| `src/validator_operator.rs:216` | `submit_transition` 在连接前直接 load 委员会及注册表快照 |
| `src/validator_operator.rs:270` | `request_checkpoint` 在连接前直接 load，供 public/recovery checkpoint 操作 |
| `src/validator_operator.rs:341,352,376` | `install_recovery` 先同步检查目的与信任快照，网络获取后直接 install_recovered_state，进入整份初始快照写入 |
| `src/validator_operator/handoff.rs:14,23,74` | `install_handoff` 同步检查目的与信任快照，网络获取后直接 install_validator_handoff_baseline，进入整份初始快照写入 |

async main 的 `run`（`src/main.rs:44`）还直接调用同步 CLI 分支，因此下列同步函数也在异步线程执行：

| 位置 | 上层 CLI 与操作 |
| --- | --- |
| `src/network_init.rs:267` | `init-network` → `init_network`，逐验证者 StateStore::initialize 写整份 genesis 快照 |
| `src/cli_public.rs:18,28` | `public-init`，以及 async 钱包 run → commands::init → public_init，读取目的/信任完整快照 |
| `src/cli_public.rs:231` | `snapshot-status` 同步 load 完整快照 |
| `src/validator_operator.rs:102` | `validator-transition-build` 同步 load |
| `src/validator_rotation_keys.rs:23` | `validator-rotate` → prepare_rotation → prepare，同步 load |

### 同类 PublicStateStore 路径

这些不是私有 StateStore，但同样读写整份公开快照，故单独列出：

| 位置 | 上下文 |
| --- | --- |
| `src/runtime_public_sync.rs:29` | freshest-view 的 public-store 分支同步 load |
| `src/runtime_public_sync.rs:119,147,139,184,197` | runtime 的 15 秒周期 `run_public_state_sync` → sync_public_state_once，直接 load、activate_validator_set_transition、install_certified_view；后两者同步写快照 |
| `src/cli_node.rs:59` | public-only 节点启动同步 load |
| `src/cli_public.rs:35,53` | public_init 同步 initialize/可选 install_certified_view；从 async CLI 或钱包初始化调用 |
| `src/wallet_cli/client.rs:29` → `src/runtime.rs:238` | async execute/open → 同步 WalletNode::start → load_public_and_bind，直接 load |
| `src/wallet_cli/storage.rs:245` | async 钱包 restore → 同步 restore，恢复原始公开 slot 文件后 load 验证 |

钱包的 `session.save()`/`DurableBlobStore` 是加密钱包资料，不是 StateStore 整份节点快照，未混入上述结论。

### 已隔离，不应误报为直接阻塞 async

| 位置 | 已有隔离 |
| --- | --- |
| `src/runtime_bft_consensus/runner.rs:16,23,39` | 启动、每轮共识工作和源重试均在 spawn_blocking；其中的 load/save/signing fence 写入不直接占用网络 executor |
| `src/runtime_bft.rs:327` | refresh_authority_for_network 包住同步 load_shared |
| `src/runtime_bft/membership_sync.rs:62,107,151` | 成员快照、补充正文、安装成员切换在阻塞池执行 |
| `src/runtime_bft/checkpoint_sync.rs:15` | 检查点重放在阻塞池执行 |
| `src/runtime_submission.rs:340`、`src/runtime_task_status.rs:74` | 网络提交/状态查询通过 spawn_blocking 调用同步上下文 |
| `src/runtime_governance/requests.rs:11` | 网络治理请求的同步快照事务在阻塞池执行 |
| `src/network/account_query/server.rs:85` | 账户投影及其 snapshot loader 在阻塞池执行 |
| `src/runtime/public_serving.rs:117,130` → `src/network/session.rs:360,558` | loader 在 async 函数中构建，但在 session 的 spawn_blocking 中调用；不是直接读取 |

公开 API `submit_legal_task`、`start_*_consensus` 等是同步接口；若外部调用者自行放进 async 任务且不隔离，也会同步读写。仓库内上述网络入口已经隔离，不能把所有同步 StateStore 方法都笼统认作网络 executor 缺陷。本调查是静态调用链核对，没有进行生产运行时阻塞采样或修改生产 I/O 路径。

## 当前源码验证结果

以下命令均退出 0：

```powershell
cargo fmt --all -- --check
cargo check --all-targets
cargo clippy --all-targets -- -D warnings
cargo build --bins --examples
wsl -d Ubuntu-26.04 --exec bash -lc 'cd /mnt/c/Users/Why23/Projects/Second && CARGO_TARGET_DIR=/home/why23/.cache/second-range-target cargo build --bins --examples'
$env:SECOND_WSL_DISTRO='Ubuntu-26.04'
$env:SECOND_WSL_BINARY='/home/why23/.cache/second-range-target/debug/second'
cargo test --all-targets
git diff --check
```

Windows 与 WSL 产物来自同份当前源码。未设置 RUST_TEST_THREADS，使用默认并发。库测试 112 通过；既有 cross_process_store_lock_helper 是由父测试单独启动的 subprocess helper，子进程也实际运行通过。规模测试 1 通过；集成测试 252/252 通过，73.77 秒，包含 `mixed_windows_linux_validators_pay_and_recover_into_required_quorum` 实际通过；34 验证者发现测试 1 通过，13.90 秒。

修正首次 observe 计时后，最初三条测试另在普通负载下连续运行 10 轮，每轮 `--test-threads=3`，30/30 通过，单轮 libtest 耗时 0.32–0.47 秒。该结果与早先的强制单核饥饿实验分开统计；没有声称修正后的严格空闲判定在超过其空闲预算的资源饥饿下也应通过。期限四情形回归测试单独运行及全量运行均通过。

本轮生成的 787 份临时快照文件与日志已清理；开始时已有的 121 份文件保留。Windows 测试/节点进程、WSL 节点进程及混合验收目录检查无残留。构建产物保留用于后续开发。
