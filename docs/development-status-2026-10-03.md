# Second 开发完整性核对

## 当前开发范围核对（2026-10-09）

固定连接修复产物的补验已通过：十分钟混合持续段 601 秒完成 134 笔转账，含基线共 630.05 秒（`target/repair-mixed-soak-600.log`）；覆盖必要 3/4 quorum、四成员轮流停机/重启、精确漏收请求补齐、最终完整共享状态一致及超过缓存后的旧请求成功幂等。客户端观测 P50=742、P95=12,934、最大 18,111 毫秒，包含 CLI/探针和故障，不作为纯协议基准。两端解压包的真实 SCM/systemd 生命周期均通过（`target/service-connectivity-windows.log`、`target/service-connectivity-linux.log`）：安装、签名业务、正常停止/启动、异常终止后新 PID 自动恢复、身份/状态连续与卸载保留数据。所有本次节点、临时服务和私有目录已清理；摘要、构建输入和清理核对见 `target/repair-soak-verification.json`、`target/service-connectivity-verification.json`。未更改源码、测试期限或二进制；下文首轮记录的未重跑边界现由此次补验补齐。整机 boot、独立物理主机网络仍未执行。

连接修复后的最终静态门禁两端 fmt/check/clippy(-D warnings) 通过；259 个构建输入 SHA-256 一致。两端同机同时运行默认 all-targets，连续两轮各 344 项全部通过：Windows 日志 `target/repair-complete-paired-windows.log`（库/集成/34 节点 13.29/52.34/30.90 秒）、`target/repair-complete-repeat-windows.log`（18.83/52.59/54.77 秒）；Linux 日志 `target/repair-complete-paired-linux.log`（12.83/47.94/10.51 秒）、`target/repair-complete-repeat-linux.log`（12.84/49.20/11.71 秒）。每端每轮 96 库 + 247 集成 + 1 个 34 节点行为测试；显式跨进程 helper 由父测试调用，Windows 混合测试另以原生 Release 执行。未改变测试线程数、生产/测试期限、签名或 fsync。两次通过证明本次压力回归已通过，不构成任意负载或所有未来时序的保证。

共享端点后的同源码双全量中，Linux 完整 344 项通过，Windows 库出现一次 certified resource ring 转发断言失败（`target/repair-shared-final-windows.log`）。该测试先观察磁盘业务终态，立即 abort runner 后手动查询内存转发结果；生产 runner 在阻塞线程提交后才登记完成范围，两个动作之间存在被测试抢跑的窗口，abort 也不等待已启动 blocking worker 完成。测试现于原十秒总预算内同时等待磁盘终态和该范围的完成转发登记，再停节点；证书、冷读原子性、历史权限及无签名断言保留。未增加生产期限或修改生产完成路径。

继续核查偶发超时：两端诊断轮次曾全部通过，但 Windows 单独执行也复现四个短期限失败（`target/repair-dual-windows.log`），不能只归因于双套件资源叠加。现有公开连接测试补充行为断言后，旧实现稳定复现两项失败（`target/repair-peer-before.log`）：新 outbound 连接的自动 endpoint 刷新与 bootstrap 同时发送 GetPeers，单次响应的健康节点会收到第二个请求；小网络达不到八个目标连接，发现退避增长后，丢失已建立连接也沿用旧退避。现在 outbound 使用已认证并持久化的 pinned record，邻居发现由 bootstrap 查询一次；inbound 仍查询远端广告地址。原维护循环在连接数下降时重置退避，空候选网络仍保留指数退避。无新循环、全网传播、线程限制或网络期限增加；原持久化、身份校验与 BFT 权限保留。空闲观察从七秒调整到十一秒以覆盖退避阶段，重连预算仍四秒、沉默节点 bootstrap 总预算仍七秒。其他 BFT/分配偶发失败尚不能认定同根因；本轮最终复验随后记录。以下固定包和服务记录对应上轮源码，不代表已包含此次修复。

上述两项修复后，首轮同机双全量两端各 343 项通过；第二轮 Windows 再通过，Linux 再次仅在沉默候选 bootstrap 的七秒期限失败（`target/repair-final-paired-repeat-linux.log`）。原串行候选路径把首个节点的握手与五秒请求超时放在健康候选之前。bootstrap 现同时推进最多四个借用当前节点的探测 future，使用现有拨号/发现/身份校验入口，不新建 detached dial task 或依赖；同一 NodeId 同时只有一个探测，失败后仍按完整 PeerRecord 尝试其他 endpoint，连接目标和全局 permit 保留。现有沉默候选测试还验证健康响应发生在沉默连接关闭之前，以及同连接不重复查询。最终定向和双全量证据另记，前述失败与部分修复的绿灯保留。

四路发现修复后的压力全量再次记录 Linux 五项 BFT/CLI 失败（`target/repair-bounded-final-linux.log`），公开连接两项回归通过，不能把 BFT 失败消去。随后仅在 Linux 客户端端点创建处插入耗时采样，日志/主机采样写入原生 Linux target 目录，结束后才复制到 Windows：`target/repair-bind-paired-linux.log` 仍复现五项失败；`Endpoint::client` 超过 20 毫秒的 482 次记录中，最大 2,757.64 毫秒、平均 991.00 毫秒（仅慢调用子集，非全部调用平均、非协议基准）。这个同步构造路径原先直接占用网络执行线程，尚未进入请求 deadline。公开与 BFT 拨号现分别用已有 blocking pool 调用同一个 QuicClient 构造器，并把原 ActiveConnectionPermit 与构造结果一同携带；取消不会让未结束的创建任务提前释放名额。身份校验、全局容量、BFT 授权、缓存持久化和请求期限不变；单命令 CLI 的同步构造入口不变。所有临时生产采样已恢复，最终全量另记。

阻塞创建的隔离实验使 Linux 完整全量 343 项通过（`target/repair-endpoint-final-linux.log`），但 Windows 库回归仍有一项交接连接中断（`target/repair-endpoint-final-windows.log`），因此继续收敛实际工作量。核查锁定依赖 Quinn 源码确认一个 Endpoint 可同时接收与发起多条连接，克隆 handle 独立保存 default ClientConfig。最终同地址族的公开/BFT 拨号复用 NodeRuntime 已绑定的 server endpoint/driver，并复用唯一 pinned-client-config 构造器；不同地址族继续使用必要的独立 socket，原阻塞 worker 和 permit 持有规则保留。新增一项真实 QUIC 安全回归验证多个远端 pin 不串用、共享实际源 UDP 端口、错误 pin 拒绝且原连接仍可用，以及不同地址族选择独立端点。没有 endpoint 池、缓存迁移或更改线协议；最终门禁另记。

真实共享 socket/pin 回归通过（`target/repair-shared-endpoint-proof.log`，0.01 秒）。NodeRuntime 释放时显式关闭共享 endpoint；现有双向连接回归保留 lower-NodeId canonical outbound handle，并释放其 runtime，验证另一存活节点不能继续 Ping 已停止节点。peer_manager 三项回归通过（`target/repair-shared-lifecycle-proof.log`，0.21 秒），覆盖重复身份、错误身份不写缓存、同时拨号与共享端点生命周期。所有采样代码已撤回，没有修改 deadline 或测试线程数。

本节是当前实现与验收的核对入口；下方分日期记录描述当时状态，不是当前待办。完成边界是 DESIGN.md 已确认的内部货币协议、现有 CLI/节点与 Windows/WSL 支持范围，不把未确认的钱包、SDK、现实身份治理、RecoverySet 治理、owner ZK 证明、DNS/DHT/NAT traversal 或任意规模容量加入交付要求。外部业务系统接入及独立物理主机网络不作为内部开发完成前置。

| 确认设计与不变量 | 权威实现/生产路径 | 对应行为证据 |
| --- | --- | --- |
| 唯一 ownership 真值、四种基础操作与供给守恒（19.1、19.11–19.14） | `state` / `executor` / `prepared` / `currency_allocation` | `core_protocol`、`concurrent_execution`；发行、转移、销毁、泄漏修复及余额派生 |
| Currency identity 永不复用（19.2、19.10） | 认证 allocation / 持久 frontier | `runtime_allocation`、`runtime_allocation_capacity`、`prepared_tasks`；取消/准备失败保留区间、耗尽不退出节点、重启与积压恢复 |
| 账户/支付地址永久绑定及 TaskId 原请求绑定（19.3、19.4、19.7） | `account` / `payment` / `state::bind_task` | `account_registration`、`payment_address`、`task_id`；运行时开户、不可改绑、失败后的原请求绑定 |
| 有序授权、业务原子性与成功幂等（19.6、19.8–19.10） | canonical LegalTask / 唯一准备器 / certified component 原子提交 | `authorization`、`core_protocol`、`prepared_tasks`、`prepared::certified`；篡改拒绝、后序不得提前建立事实、冷读提交与精确重放 |
| 支付退役保留已建立业务（19.5） | Transfer establishment / lifecycle claim / authenticated historical source | `payment_address`、`runtime_historical_source`、`runtime_tasks::handoff::tests::retiring`；迟到证书不重新按今日 retiring 状态否定旧执行 |
| 一 Validator 一票、原委员会 exact-set finality 与永久签票锁（19.16、19.17） | `ValidatorSigner` / `StateStore` / per-scope BFT | `bft_core`、`bft_driver`、`validator_vote_lock`、`finality`；伪造/错委员会拒绝、QC/lock 冷恢复、合法 Nil/换轮、无新消息最终性重试 |
| 冲突仲裁、同请求冻结变体、认证依赖闭包 | `runtime_tasks::contention` / 既有 contender 索引 / certified component | `runtime_conflicts`、`runtime_prepared_source`、`runtime_tasks::certified_admission_tests`；真实 2:2、已有 Commit QC 优先、证书先于正文、仅执行证书绑定计划 |
| 分布式交接并集、原子认证遗漏退役与请求保留 | `runtime_governance::handoff_source` / `persistence::task_handoff::retirement` | `cold_membership_request_collects_distinct_duties_and_switches_four_nodes`、同组运行/离线 quorum、`live_cut`、认证遗漏冷重试；合法 quorum 根不由本地更大并集替换 |
| 新成员业务基线、历史/迟到计划及终态处理 | `install_validator_handoff_baseline` / handoff fetch / `runtime_tasks::handoff` | `store_handoff_import`、`handoff_baseline_tests`、冻结变体/退役地址恢复；篡改基线零写入、保留后来业务、历史 Commit/Abort 仅消费原委员会证书 |
| 本地/共享恢复与独立轮换签名栅栏 | `store_recovery` / `safety_recovery` / 原 governance runner | `validator_safety_recovery`、`store_handoff_import::tests::reentry`、混合 release 验收；导入不复制旧票、提供方全重启后 proof 追赶、V2 fence 后才能签名 |
| 公开数据 owner 隐私、认证 checkpoint/delta 与长期同步（19.15） | 独立 PublicStateStore / `runtime_public_sync` / public governance | `network_public_summary`、`public_state_sync`、`runtime_public_checkpoint`、`runtime_public_sync`；全/增量认证、集合追赶、私有状态隔离 |
| 持久化原子性、进程隔离与低成本运行 | commit reference / 双槽 / 公共锁入口 / 现有重试期限 | `persistence`、跨进程锁父测试、重复 QC 冷读、20 路完整回归；无任意旧镜像回滚、无签名/fsync/期限削弱 |
| 可操作节点、业务 CLI 与 Windows/Linux 部署 | `init-network` / node / submit/status / 既有管理与打包工具 | `cli_*`、34 成员真实 QUIC 业务回归、Windows/WSL 四进程必要 quorum；SCM/systemd 历史生命周期记录见 deployment.md |

本轮已复现并修复历史 Abort 缺失完成通知/资源等待者重试，以及只查询首个继承冻结计划的问题；定向回归同时覆盖 lifecycle/currency 资源、有/无本地正文、多个冻结计划、错误证书、原集合权限、当前 quorum 完成等待业务和重复证书幂等。

历史 Abort 修复后的阶段源码两端 fmt/check/clippy(-D warnings) 全部通过。Windows 默认并发的完整 all-targets 连续两轮通过，每轮库 95 项、集成 247 项、34 节点真实 QUIC 1 项，共 343 项；日志 `target/closure-windows-final.log`（13.55/53.71/34.59 秒）和 `target/closure-windows-repeat.log`（15.21/41.64/51.07 秒）。WSL 原生源码完整 all-targets 同样 343 项通过，日志 `target/closure-linux-tests.log`（13.40/48.86/52.64 秒），静态门禁日志 `target/closure-linux-static-final.log`。锁父测试调用显式忽略的跨进程 helper；Windows 普通门禁另忽略需要原生 Linux 产物的混合测试，该场景单独验收。未限制测试线程、增加期限或放宽安全断言。两端 257 个构建输入文件的 SHA-256 一致，记录 `target/closure-source-manifest.json`；本轮最终固定源码的门禁另见下文。

该阶段 Release 的 2 Windows + 2 WSL Linux 混合集群通过，1 项、0 失败，31.53 秒，日志 `target/closure-mixed-release.log`。覆盖固定证书双向 QUIC、错误证书拒绝、支付及完整共享状态一致、成员掉线后的必要 quorum、重启追赶、全新 Linux base 认证恢复、健康提供方全部重启后的 V2 proof 拉取、签名隔离/恢复，以及恢复成员参与必要 3/4 quorum 完成新业务。节点与临时目录已清理。

**当前状态：已确认内部功能范围开发与本轮修复验收完成，连接修复后的两轮同机双全量、新固定原生 Release 混合集群及部署包核验均通过。** 下文存储阶段的持续运行、真实服务生命周期及 `target/dist-stability-final-20261009` 对应旧产物；其双全量压力失败是后续连接修复的输入，不能将旧失败描述为此次最终回归结果，也不能用旧服务实测替代新二进制验收。

最终原生 Release 使用锁定依赖分别重建；固定摘要保存在 `target/repair-release-binaries.json`。2 Windows + 2 WSL Linux 混合集群通过，28.82 秒（`target/repair-mixed-release.log`），覆盖 pinned QUIC/错误证书、支付及全状态一致、掉线必要 quorum、重启追赶、全新 Linux base 恢复、提供方全重启后 V2 proof 拉取、签名隔离与恢复成员参与必要 3/4 quorum。前后二进制摘要一致，所有本次节点和临时目录已清理。首包位于 `target/dist-connectivity-20261009`，当时核验日志 `target/repair-package-verification.json`：manifest、已验收二进制、源码文档/脚本、相对链接、无状态/密钥白名单、Linux 权限均通过。十分钟持续段与系统服务生命周期已由本节顶部补验完成；更新文档后的最新包位于 `target/dist-connectivity-soak-20261009`，核验日志 `target/repair-soak-package-verification.json`，二进制和服务管理脚本与此次实际服务验收原包逐字节相同。独立物理主机网络和整机 boot 未执行。

部署包实际校验记录为 `target/closure-package-verification.json`：两个包的 manifest 全部匹配，二进制 SHA-256 与各自已验收的原生 Release 一致，操作文档与仓库一致且包内相对链接有效；Linux 归档权限、包内不含节点状态/密钥也已核对。运维接入文档的旧“尚无首次冲突准入/完整重试闭环”描述同步替换为当前行为，不把过期进度作为缺失功能。最终 `git diff --check` 通过。

首次 Windows 完整执行出现三个 5 秒单节点分配超时（`target/closure-windows-gates.log`），失败现场和日志保留。重新核对日志时长及落盘时间后，Linux 编译只与 Windows 编译阶段重叠，测试超时阶段没有该编译任务，不能据此归因。分配组五项单独通过，随后分别完成 Linux 全量和上述两轮 Windows 默认全量；这些通过不替代根因分析。

继续采样的第三轮默认集成测试复现认证范围恢复/耗尽分配超时及私有来源超时（`target/stability-storage-round-3.log`）。分配用例阻塞线程排队 0–6 毫秒，失败窗口 CPU 26%–47%；单次处理接近 5 秒，已记录的快照编码通常不足 1 毫秒，磁盘写入/同步及其存储锁等待占主要时间。重复创建父目录单次最多 178 毫秒；`write_slots` 唯一调用者已经持有创建目录的存储锁，故移除这次无价值 I/O。文件锁、两份镜像、fsync、commit reference 与原期限不变。现有 checkpoint 持久回归扩展到不存在的嵌套目录，证明第一次加锁仍能创建目录及完整原子提交。采样代码为定位临时插入，验证后移除；没有新增生产后台任务。最新包的原生 SCM/systemd 生命周期已通过（`target/service-current-windows.log`、`target/service-current-linux.log`），当前存储修复后的产物需重建复验。

移除重复目录工作后的带采样默认完整集成连续四轮 247 项通过（`target/stability-parent-fixed-1.log` 至 `-4.log`，42.65/43.37/41.76/44.60 秒）。临时采样从三个生产文件全部移除，只保留公共写入入口的目录检查修复及失败时诊断；原期限、线程数和安全断言未变。两端并行完整集成另出现一个 Linux 发现超时（`target/stability-dual-linux.log`），它不能归入已证明的 Windows 存储根因；最终同源两端全量及并行复验另记。持续验收扩展现有显式混合集群，以同一批节点反复支付/故障/追赶、旧请求幂等与资源采样，详细入口见 node-operations.md。所有收尾结果以去除采样后的最终源码和重建产物为准。

持续负载期间再次出现 Windows 分配短期限失败（`target/stability-final-windows-repeat.log`），目录检查修复不足以消除所有抖动。继续收敛锁文件、完整/公开快照及 commit reference 的文件打开路径：已有文件先用原读写/truncate 语义打开，仅 `NotFound` 才创建父目录和文件；公开与完整状态复用同一快照写入原语。所有错误传播、跨进程锁、内容校验、主槽/镜像写入与 fsync 保留。该改动消除已存在文件的重复 create 操作，不把潜在收益当作已验证的全部超时根因；最终门禁和持续验收另记。新持续夹具最初要求无请求的全量历史自动追赶而失败，已改为正式业务接入的原签名请求精确重提；完整共享状态、逐笔持久完成和幂等断言保留，没有新增全量历史广播。

最终固定源码两端 fmt/check/clippy(-D warnings) 通过，Windows/WSL 默认 all-targets 各 343 项通过；Windows 再重复两轮默认集成，各 247 项通过，46.98/44.26 秒。全量日志 `target/stability-open-windows-all.log`（13.02/52.86/77.46 秒）与 `target/stability-open-linux-sequential.log`（13.37/48.13/53.54 秒），静态日志 `target/stability-open-final-static.log` 与 `target/stability-open-linux-static-build.log`。未降低测试线程或改变期限。258 个构建输入在两端 SHA-256 一致（`target/stability-source-manifest.json`），两端锁定依赖的原生 Release 主程序及 probe 已重建；运行前后二进制摘要与 `target/stability-frozen-binaries.json` 一致。

冻结后的 2 Windows + 2 WSL Linux 同集群持续验收通过（`target/stability-frozen-mixed-soak.log`）：基线场景加持续段共 646.84 秒，持续段 611 秒、132 笔支付；四个成员轮流离线，必要 3/4 quorum 继续持久业务，重启成员按正式客户端路径逐笔精确重提遗漏请求并补齐完整 canonical shared payload。超过 64 项 receipt 缓存后精确重放首笔仍成功幂等；余额、原身份/证书、当前 V2 与恢复成员签名 floor=2 的断言全部保留。客户端观察提交 P50=800、P95=13,402、最大 14,890 毫秒，包含 CLI、探针、轮流故障条件，不是纯协议吞吐基准。CPU/RSS 样本在同一日志中；测试进程与临时节点目录已清理。

同一固定二进制的部署包解压后，真实管理员 SCM 和 root/systemd 生命周期分别通过（`target/service-frozen-windows.log`、`target/service-frozen-linux.log`）。检查签名开户/发行、停启、强制结束本次服务进程后的自动重启、身份/业务状态连续及卸载保留状态；临时服务和其数据已清理。最终包 manifest、固定已验收二进制、源码脚本/文档及包内相对链接、Linux 归档权限和无状态/密钥白名单校验记录为 `target/stability-package-verification.json`。最终 diff-check 通过。

另加的同机双全量叠加压力复验不是通过结果：Windows 全量通过，Linux 五项连接/发现期限失败（`target/stability-open-linux-all.log`）；随后 Linux 单独默认全量通过。负载条件的区别已确认，但不足以证明这些压力失败的全部根因，不能归入已测得的 Windows 存储延迟，也不能用正常门禁或持续支付通过抹去失败。独立物理主机网络、整机启动与任意并发容量未验收，当前部署验收范围按用户确认止于 Windows/WSL。

## 历史实施记录

2026-10-08 已修复每次状态访问重复创建目录/锁文件的存储热点，并抑制无状态变化的重复 Precommit QC 写入；此前的默认 4 路测试限制已移除。两轮显式 20 路及最终普通默认命令的全量测试均通过，每轮 342 项，包含 34 节点真实 QUIC 回归；第一轮带临时采样，之后已移除全部采样代码。本地五项门禁全部通过。此前完成的交接贡献去重、最终性失败后的无新消息重试、依赖闭包提交完成通知和业务依赖恢复均继续受现有回归验证。随后用当前源码原生 Release 产物完成 2 Windows + 2 WSL Linux 混合集群验收，41.78 秒通过支付、掉线追赶、认证恢复、轮换证明跨重启追赶、签名隔离/恢复及恢复成员参与必要 3/4 quorum；日志 `target/cross-mixed-acceptance.log`。详见 [DESIGN.md](../DESIGN.md) 开头的实现与验证记录。用户确认当前仅用 WSL；这是同机跨系统验收，独立主机网络与整机 boot 行为未验证。

2026-10-05 时的内部开发评估：协议尚未闭合。待完成项是切换前的分布式义务并集收集、新成员认证业务基线安装、状态改变后的历史/迟到正文验证和推进，以及间歇连接丢失/超时的根因修复。业务接入与独立主机验收属于外部集成/部署验证，不作为 Second 内部开发边界。下列分日期内容保留当时进度，不能把早期“仅剩外部验收”的判断当作当前完整性结论；该阶段的实现与验证证据见 [conflict-arbitration.md](conflict-arbitration.md) 后续记录。

本清单核对 DESIGN.md 已确认设计和实际生产入口，不能用测试数量或七项审计整改代替完整产品交付。未确认的协议能力不计作已承诺功能，也不擅自实现。

| 能力 | 当前生产入口与实现 | 状态/边界 |
| --- | --- | --- |
| Genesis 与节点启动 | `init-network`、`node`，network_init / cli_node | 已接通；endpoint 必须明确可拨，Genesis 最多 56 个 Validator；本地候选最多 128，公开查询每次最多 32 |
| 节点部署与离线恢复操作 | CLI runtime lock、examples/run-network.ps1、docs/node-operations.md | 同一 base 的重复运行/离线安装拒绝，进程结束可重启；另提供原生 Windows SCM / Linux systemd 安装管理；当前源码 Windows / WSL 混合集群验收通过，shared recovery 保持 signing safety locked，V2 recovery proof 后恢复签名；独立主机网络未覆盖 |
| Currency 业务语义 | executor / prepared / payment / claims | 已有真实执行和协议回归；发行、销毁、转移、reserve、LeakRepair 等仍以 LegalTask 授权和原子业务执行为边界 |
| 运行时开户与支付地址 | RegisterAccount / account / prepared/lifecycle / payment | 已接通现有 signed LegalTask 提交、最终性与持久提交；账户永久唯一、支付地址不可改绑，沿用外部 Authorizer 信任模型 |
| 外部任务提交与查询 | `submit`、`task-status`，runtime_submission / runtime_task_status | 已接通；失败终态没有独立权威结果表，`bound` 不能冒充成功或永久失败 |
| 业务签名与接入示例 | `authorizer-keygen`、`transaction-sign`、examples/submit-transaction.ps1 | 已接通；离线签名、有限精确重试与固定证书端点切换，不替代业务方授权政策和实际接入验收 |
| 连续 identity 分配 | CurrencyAllocation + runtime_bft_consensus/allocation | 已接通；局部 quorum 确认区间，取消/失败不能复用已分配地址；无效区间入队前拒绝，竞争导致空间耗尽只清理该待办，不退出节点 |
| 私有任务传播与最终性 | exact-set BFT transport、private source pull、PreparedTask commit | 已接通；签名任务及冻结选择仅提供给对应 Validator；当前集合三类不同 TaskId 的 2∶2 自动仲裁和中途重启通过；冻结变体与跨成员交接仍在实施 |
| 公开认证状态发布 | `public-checkpoint`，runtime_governance / BFT public source / store_public_checkpoint | 本轮补齐：显式发起、源摘要验证、quorum、自动持久证明/floor/baseline/delta；不要求客户端消费诊断事件 |
| 公开节点持续同步 | `public-init`、public backend `node`，runtime_public_sync | 已有长期 worker；每 15 秒检查证明，优先使用既有 bounded delta，否则完整认证同步 |
| Validator membership 与 key rotation | validator admission/rotation/transition CLI、runtime_governance | 主链路已接通；transition 与 allocation 共用 frontier barrier。已将受理 source 写入现有快照，重启恢复同一 frontier/candidate；集合或 frontier 已前进的待办随原子提交清理 |
| shared-state recovery 与本地签名恢复 | `recovery-checkpoint`、`recovery-install`、rotation safety fence | 已有认证传输、空目标安装、恢复后签名隔离；已受理历史候选可续跑原 serial/digest；过时 QC 只推进 floor，并原子登记下一 serial 的当前状态候选，不能把 shared-state recovery 当作完整旧签票历史恢复 |
| 持久化与资源边界 | commit reference、immutable cache、network deadlines、bounded diagnostics | 七项整改完成；完整快照写入仍 O(N)，没有实测数据前不替换存储体系 |

## 本轮公开发布操作

```
second public-checkpoint <address> <operator-snapshot-base> <server-cert-base64>
```

operator 从自己的 snapshot/keyring 取得 current Validator 身份和 identity key。请求签名复用 governance auth 的 channel binding/version/ValidatorId，并使用独立 public-checkpoint action，不能把 recovery request 签名重放为公开发布。

`PUBLIC-CHECKPOINT-ACCEPTED` 只表示请求已登记或已有证明可复用，不表示新请求已经 final。节点实际形成合法 FinalityCertificate 后，自动调用既有 StateStore 路径保存 proof、floor、delta baseline 和 bounded delta。公开客户端/节点仍验证 exact ValidatorSet 的 quorum 与完整 public summary。

发起方只需具备当前 quorum 的连接，不要求全部 Validator 在线。候选源通过 Validator-only BFT 发送，并在 proposer 的后续 round 重发；每个接收者重新检查 scope/version/epoch 和自己当前 public state。候选 source 复用现有 proof 编码的零票形式，必须等于本地合法下一 epoch/当前待处理候选，不把任意大 epoch 当作可签提案，也不把零票 source 当作证明。已有 quorum proof 可复用同一编码传播：验证成功后，落后节点可以安装对应状态证明或推进 floor，随后参与后续 epoch；没有第二套 checkpoint serializer。已认证 epoch 的旧会话和缓冲会被清除，不继续空转。

同一状态已有 attached proof 时复用已有 epoch/QC；重复安装相同 checkpoint 保留 baseline/delta，不重新写全量快照。无 attached proof 时，从 durable checkpoint floor、同集合 BFT 状态和不可逆签票锁推导候选：可以恢复精确相同摘要的未完成 epoch，但已经签过不同摘要的 epoch 不能重新使用。已进入 BFT 且摘要仍匹配的发布会在重启时恢复；尚未产生 durable BFT/signing 记录的登记、或者业务状态已经改变的未完成发布，应由 operator 重提。

业务改变使旧 proof 失效时，operator 可对新状态再次发布；不把每笔业务提交升级为公开 checkpoint 共识，也不增加固定轮询的 Validator 发布循环。若形成 QC 时业务状态已经前进，只推进已认证 epoch 的 floor，不把旧 summary 挂到新状态上。下一次发布使用后续 epoch。

## 真正后续工作

1. **部署容量规划**：已分开公开响应、本地候选与部署上限，并验证 34 成员端点连接/重启、单节点私有发行到全体持久提交及真实 CLI provisioning。任意超过 56 成员或累积 retained-set 拓扑的容量仍未承诺。
2. **真实业务方的集成验收**：当前 CLI 主链路成立，但外部业务系统的授权、重试、状态查询和运维发布方式需要在实际接入中确认；不预设新 SDK、钱包或管理 UI 是已确认要求。

## 需要独立决定而非直接开工的内容

RecoverySet 的成员资格/更换与现实身份治理、owner 隐私的公开可验证证明，以及 DNS seed/DHT/NAT traversal 等真实拓扑需求，仍属未确认设计。不得把这些事项包装成已有规范的实现缺陷。

## 本轮验证结果

上一轮 `cargo fmt --all -- --check`、`cargo check --all-targets`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all-targets` 和 `git diff --check` 通过，包含 230 项集成测试（原套件 229 项、独立大网络目标 1 项）及 25 项单元测试与既有进程隔离子检查。运行时开户这一轮的最终结果见文末。34 节点目标单独运行，避免与其他具有 deadline 的网络测试争用本机资源；仍属于完整 all-targets 门禁，不降低生产超时约束。

新增行为证据：真实 CLI 发起 public-checkpoint 并通过 sync-public-certified 验证；四成员集合在只有三个节点运行时形成 quorum，晚加入节点复用已有证明补齐；业务变化后发布下一 epoch，长期 public worker 跟进认证状态和既有 delta；已签 epoch 在重启后完成同一摘要；未认证巨大 epoch、伪造 proof 被拒绝；高 epoch 的合法 QC 在状态暂不匹配时只推进 floor 并清除过期 attached proof。旧 membership/retained-task 测试修正了启动自动恢复与手工注册之间的夹具竞态，不改变生产自动提交规则。

公开发布和治理受理恢复是已完成的链路；Second 全部开发仍未完成。恢复检查点与业务状态前进的并发收敛及本地候选/部署边界已处理；剩余实际工作是外部业务接入验收，更大或累积 retained-set 拓扑需另做容量规划。


## 治理受理与重启恢复

membership/recovery 的 CLI、节点 API 和 Validator-only source 都在注册共识前，将 canonical candidate 原子写入现有 StateStore 快照。源编码复用 ValidatorSetTransitionSource / StateRecoveryCheckpoint；本地 pending metadata 不进入 shared recovery digest，不保存另一份业务状态，也没有 sidecar、兼容格式或新增轮询。当前开发快照版本仍为 1。

同一候选重复受理不写快照。待办总数最多 64，恢复检查点在同一 serial 内复用现有多候选 BFT，不另建共识实现。候选的首次受理必须验证当前 shared state；本地已在锁内验证并持久受理的精确历史候选可继续签名和恢复，未知旧摘要仍拒绝。BFT QC/lock 决定后续 round 的值；没有 QC/lock 时，优先选当前状态候选。已有不可逆 FinalityVote 仍只能续跑原 serial/digest，不能为同 serial 另一个摘要签票。

仅在已受理恢复操作尚未完成时，业务提交、allocation 提交或重启触发必要的候选刷新；没有 pending 时不哈希完整状态、不启动恢复共识。超过既有候选预算时拒绝新候选并记录诊断，不以业务后台刷新为理由使整个节点退出。已经完成的请求不会因以后每笔业务变化自动发布新 checkpoint。

形成恢复 QC 时，runtime 安装证据的路径验证 exact active ValidatorSet 和 certificate：若 commitment 匹配当前 shared state，原子保存 proof/floor 并清理已完成待办；若已过时，仅原子推进 certified floor、清理旧 serial 待办并登记下一 serial 的当前状态候选，不挂载旧 proof，也不发布旧 payload。显式 publish primitive 仍要求 payload 匹配当前状态。下一 serial 的待办与 floor 同次提交，崩溃后会继续恢复，永久签票锁不清除。

恢复 source 复用现有 StateRecoveryCheckpointProof 编码：零票代表候选，非零票必须通过 quorum 验证，才允许未受理过历史候选的节点补齐 floor。BFT finality relay 直接发送该 proof，包含源和 QC，避免再发送一份重复 finality certificate。重新安装同一认证 floor 和已缓存 provider 不重写快照、不重新编码/哈希完整 payload。已认证 serial 的旧会话和缓冲及时清理。

回归覆盖：首次投票前重启恢复 membership；recovery 在首次投票前及 serial 锁落盘后恢复同一 serial/digest；重复受理无额外写入；认证后下一 serial 才解锁；本地待办不改变 shared digest；状态变化时，未锁定候选可选当前值，已签旧候选先完成再自动认证下一 serial；四节点真实 QUIC 传播与未受理旧候选节点的 QC catch-up、当前恢复 payload 下载；未知旧摘要、伪造 QC、未认证 serial 跳跃被拒绝；重复合法 QC 不新增写入。

## Validator 端点保留与部署容量

公开 GetPeers/Peers 仍最多 32 条，避免扩大网络响应。bootstrap 读取最多 128 条且限 256 KiB；本地 PeerStore 最多 128 条，逐条复用现有 PeerRecord wire codec。BFT 握手成功后才缓存 ValidatorId 提示，优先保留及拨号，同一 Validator 不可通过多个 transport identity 占满缓存。提示退出 active/retained authority 后撤销保护；加载缓存不会赋予身份或投票权限。普通 authenticated peer churn 不挤掉仍有 authority 的 Validator 候选。损坏/超限缓存仍丢弃，I/O 错误仍返回，写入仍使用 sync 后 rename。

部署/治理入口最多 56 个 active Validator，对应 110 条双向 BFT 连接，128 总连接预算剩余 18 条；不改变 ValidatorSet 的协议语义。更换成员时的 retained authority 也消耗这个预算，当前并未保证任意跨代拓扑。回归用真实 34 成员 QUIC 节点验证 33 条 outbound BFT 连接，并在没有静态 bootstrap 的重启后全部恢复，再建立完整 exact-set 连接，从单节点私有发行请求形成合法 quorum 证书并在全部成员持久提交；真实 CLI 生成 34 个节点的完整 33-record bootstrap，并启动加载该配置。另验证普通端点 churn 后 Validator 候选保留、成员退休后撤销保护和超限缓存丢弃。

大集合业务验收进一步核对同一成员重启改变 endpoint 后的完整 exact-set 连接。发现并修复 BFT maintenance 按 NodeId 去重导致旧缓存失败后跳过正确 bootstrap 地址的问题：现在复用 PeerRecord 完整记录去重，身份认证不变。该验收仍是本地协议业务链路，不代表外部业务系统已经完成接入。

大规模验收使用独立测试目标和每个模拟 Validator 一个执行线程，避免让同步签名/持久化挤占 QUIC I/O。test profile 的依赖开启 opt-level=3，主项目仍保留调试构建，debug assertions 不关闭；这让真实密码学与传输库以优化代码运行。该设置不改变生产共识阶段超时、网络 deadline、证书校验或投票锁。验收明确要求 allocation、只向一个节点提交的私有任务传播、至少 23 票的 34 成员证书，以及全部成员持久状态一致；不会用连接数量替代业务结果。

34 成员完整业务场景已通过：单节点提交发行请求，经 CurrencyAllocation 和 private source pull 完成 PreparedTask finality，证书验证至少 23 票；用独立 StateStore 实例重读磁盘，并比较 canonical shared recovery payload，确认私有/公开共享状态都收敛。此为本地协议验收，外部业务接入仍待实际业务方参与，不冒充外部生产交付。

上一轮大集合完整测试日志为 `target/large-validator-business-verified.log`：229 项原集成套件加 1 项大集合业务场景全部通过，大集合场景 32.29 秒。该耗时仅描述本机测试夹具，不是生产吞吐量或延迟基准；生产 deadline 未放宽。

## 运行时开户与完整支付链路

严格 transaction JSON 新增 `register_account` operation，字段为 `account`，使用既有 canonical `acct_` 地址字符串。签名的 canonical CBOR discriminant 为 7，内部 LegalTask wire / prepared operation tag 为 8；当前开发格式仍为 1，没有旧格式 reader。地址由受信任业务签发者指定，不引入第二套地址分配器或用户密钥体系。

开户只在合法 finality 后原子写入既有 accounts 集合，并随既有 snapshot/shared recovery payload 保存。准备中的地址通过账户与支付地址共用的 lifecycle claim 排他占用；重启恢复占用，取消释放。不同 TaskId 再次创建已存在账户会被拒绝；成功任务的相同 TaskId 重放沿用既有 succeeded 幂等语义。同一任务内重复开户会整体回滚并释放 claim。

真实四进程 CLI 场景从空 Genesis accounts 开始，先验证非受信任 Authorizer 不能留下账户或 TaskId binding，再由受信任签发者在同一任务中开户两次、绑定两个支付地址、发行、转账、退役并最终退出源支付地址。逐节点独立重读磁盘核对账户、绑定、接收方余额与 Retired 状态，重启一个节点后重放原请求仍返回 succeeded。没有用账户查询或连接数量替代业务结果。

这完成现有外部授权模型下的运行时开户；用户公钥账户、钱包、密钥恢复及 owner 公开隐私证明仍需单独设计，不能据此宣称已经实现。

本轮 fmt/check/clippy/diff-check 通过；`cargo test --all-targets -- --test-threads=4` 完整通过 232 项集成测试（主套件 231 项、独立 34 节点目标 1 项）、25 项单元测试及既有进程隔离子检查。完整日志为 `target/account-registration-verified.log`，主套件 43.60 秒，大集合 86.14 秒，均不是生产性能基准。首次默认并发运行中，既有四节点公开 checkpoint 测试未在 5 秒内连齐 BFT 通道；该项单独重跑通过，随后控制测试并发的完整运行通过。并行资源争用是合理推测，尚未证明根因；没有修改生产 deadline 或降低测试断言。

## 业务方最小接入工具

新增 Authorizer 本地密钥生成和 transaction 离线签名命令，签名与提交共用 transaction payload 校验，签名复用 LegalTask canonical 编码，大小校验复用既有 wire codec。随机 signing key 和私有文件权限检查从 Validator keyring 收敛到现有 local_file helper，避免两套密钥生成/权限规则。私钥只存在本地文件，signed request 创建禁止覆盖；未新增依赖、后台任务、授权 registry 或钱包模型。

接入指南见 [business-integration.md](business-integration.md)，提供开户、发行、支付三笔顺序业务，以及有轮数上限的 PowerShell 7 重试示例。示例固定每个端点证书并保存精确请求字节，unknown/bound 时可重提，prepared/voting/finalized 时等待，只有 succeeded 才完成。网络失败或重试耗尽不创造永久失败状态。

两项新增回归覆盖真实离线 keygen/sign、有效签名验证、非法操作/地址/已签输入拒绝与文件不可覆盖，以及四成员 CLI 跨任务开户→发行→支付。QUIC relay 在完整请求被节点受理后扣住回执，客户端真实超时后向另一个成员重提，全部成员磁盘余额证明没有重复发行。不同摘要复用 TaskId 的提交被拒绝，查询按既有隐私语义返回 unknown；签名内容篡改被拒绝。Windows 实际运行 PowerShell 示例，从不可用端点切换到另一成员，完成新退役任务并核对全体 Retiring 状态。shared runtime fixture 从原测试文件移入既有 test support，没有复制节点装配逻辑。

本轮 fmt/check/clippy/diff-check 全通过；`cargo test --all-targets -- --test-threads=4` 完整通过 234 项集成测试（主套件 233 项、独立大集合 1 项）、25 项单元测试与既有进程隔离子检查。最终日志 `target/business-integration-verified.log`，主套件 47.51 秒、大集合 31.48 秒，仅为本机测试耗时。真实业务方的私钥托管、授权政策和业务系统部署仍待实际接入验收。

## 节点部署与恢复操作

CLI node 在加载状态前取得同一 snapshot base 的 OS runtime 排他锁，public-init 和 recovery-install 在目标安装期间复用该锁。空锁文件不是 PID 或协议状态，不进入 snapshot/digest；父目录 canonicalize 后取得同一目标，文件保持不删，句柄持有期间争用直接拒绝。已有跨进程 StateStore 写锁继续保护原子提交。未改变 BFT、finality、local signing safety 或网络 deadline；复制到其他 base/主机的同一 Validator key 不由局部 runtime lock 保护。

Windows PowerShell 7 的 run-network.ps1 监督选定本机成员，隐藏后台窗口、限时检查启动地址/ValidatorId、保存本次日志与固定证书 endpoint inventory；失败和退出时只结束本次创建的子进程，没有 PID registry、协议 sidecar 或自动重启循环。它是前台 operator 工具，不宣称系统 service 或跨主机编排。初始化、停机备份、正常重启、认证 shared recovery 与丢失 signer metadata 后的 rotation/fence 边界见 [node-operations.md](node-operations.md)。

扩展既有真实 CLI 场景，未增加同义测试：同目录别名的重复启动/安装被拒绝，终止后重启身份与成功任务持续；四节点监督脚本退出后端口与锁可重新使用；健康 CLI 发布 recovery checkpoint，空目标经 recovery-install 恢复的完整 shared payload 相同、safety 保持 locked，目标再次安装被拒绝且 generation 不变。没有让恢复节点在旧 signing domain 重新投票。

本轮 fmt/check/clippy/diff-check 通过；完整 all-targets 以 test-threads=4 通过 234 项集成测试（233 主套件 + 1 独立大集合）、25 项单元测试及既有进程隔离子检查。日志 `target/node-operations-verified.log`，主套件 49.18 秒、大集合 33.44 秒，仅是本机测试耗时。未新增依赖、workflow、协议格式兼容层或治理自动化。

## 恢复成员重新参与业务

扩展已有四进程 CLI 生命周期，复用其 provision、节点启动、signed request 和清理夹具。节点恢复、CLI rotation/transition、transition 后重启、V2 recovery proof、自动 signing safety 恢复、再次重启后的旧版本 fence 以及必要成员参与新业务已串联验证。恢复后故意停止第四成员，仅三个成员在线，新任务在三者持久提交，证明恢复成员重新参与 quorum。

实际复现并修复两个生产阻塞：恢复/transition operator 入口原先要求全部 active peer 在线，故障成员离线即使满足 quorum 也返回 Busy，现收敛到统一的 active connection quorum 检查；locked 成员重启漏掉 recovery proof 广播后无签票可诱发 completed-scope relay，现于 fresh authenticated exact-set dial 定向补发现有 attached proof。没有新增定时循环、全网广播、第二份状态或重复共识。受理连接计数不替代 quorum signatures，接收 proof 继续 fail-closed 验证 exact set、证书、状态匹配与 floor。

最终 fmt/check/clippy/diff-check 通过；`cargo test --all-targets -- --test-threads=4` 完整通过 234 项集成测试（233 主套件 + 1 独立大集合）、25 项单元测试及既有进程隔离子检查，日志 `target/validator-reentry-verified.log`。主套件 50.97 秒，大集合 29.42 秒，仅为本机测试耗时；没有放宽生产 timeout 或关闭 signer safety gate。

## Windows 与 Linux 部署

部署目标明确为 Windows 和 Linux。保留 Windows PowerShell 7 前台监督入口，新增 Linux Python 3 标准库前台监督入口 `examples/run-network.py`；两端复用同一 init-network 配置、CLI、持久状态、固定证书 inventory 与恢复流程。Linux 脚本按 argv 启动选定成员，限定启动等待，保存本次日志，停止或失败时只结束本次创建的子进程；没有新增协议状态、依赖包、自动重启循环或服务注册。

扩展已有四节点 CLI 生命周期来执行平台对应脚本，验证 inventory 证书、限时退出和节点身份连续。Linux 额外发送真实 SIGTERM 给监督进程，等待其回收所选成员后重启同一端口/base。扩展既有业务签名回归验证 Unix 新私钥 0600 权限，以及 0644 权限被拒绝且不生成签名输出，未增加机械测试计数。

在 WSL2 Ubuntu 26.04 LTS 的 Linux 原生文件系统中，用 Rust 1.98 完成 fmt/check/clippy 与 all-targets 测试：233 项主集成、1 项独立 34 成员目标、25 项单元和既有进程隔离子检查通过。新增权限断言后重跑对应签名测试通过。`cargo build --release --locked` 通过；release 四节点真实 CLI 场景进一步通过开户/支付、正常重启、Linux 监督与 SIGTERM、shared recovery、rotation、版本间重启、旧域 fence 和恢复成员参与必要 quorum 的新业务。完整日志 `target/linux-deployment-verified.log`。

构建与操作指引见 [node-operations.md](node-operations.md)。本轮独立平台验收证明当前 Ubuntu Linux 进程与文件系统路径成立，混合集群验收结果见下节；其他发行版、外部主机到 WSL 的防火墙/NAT 与生产容量未验证。系统服务安装与开机自启是独立运维选项，目前不作为基本部署前置条件。

Windows 同步通过 fmt/check/clippy、diff-check 和完整 all-targets 测试（233 项主集成、1 项独立大集合、25 项单元及既有进程隔离子检查），日志 `target/windows-linux-deployment-verified.log`；四节点场景继续验证 PowerShell 监督与固定证书 inventory。Windows 的 `cargo build --release --locked` 同样通过。两端测试没有放宽协议 deadline、权限检查或 signer safety gate。

## 原生 Windows / Linux 混合集群

显式本地验收使用两个原生 Windows release 进程和两个 WSL2 Ubuntu 26.04 原生 Linux release 进程，全部使用各系统本地目录、独立 transport identity、固定证书与同一四成员集合。测试依赖 mirrored localhost UDP，不修改机器路由、防火墙或 WSL 配置；常规测试将它标为需要环境的 ignored 项，由独立命令实际执行，不以 ignored 状态宣称验收通过。

实际通过双向 pinned QUIC 与错误证书拒绝；Windows 单源开户/发行/支付，四个成员的账户绑定、余额与 canonical shared recovery payload 相同。停止 Windows 1 后，Windows 2 + Linux 3/4 组成必要 3/4 quorum，仍完成新发行；Windows 1 重启补齐成功任务且证书保持连续。

Linux 3 在全新 native base 安装认证恢复状态并保持 locked；其余三个混合成员先完成 V2 rotation transition，之后恢复成员才上线，补齐 source/finality 后仍 locked。在 membership 与 recovery proof 之间重启，再取得 V2 proof 自动恢复 safety-ready 与 minimum signing version=2；再次重启后 identity/证书持续。停止 Linux 4，仅 Windows 1/2 + 恢复后的 Linux 3 完成全新业务，余额只增加一次，三者完整 canonical shared payload 相同，证明 Linux 恢复成员是该次 quorum 的必要参与者。

验收复现了恢复成员错过 transition source 后停在 V1 的生产缺口。当时修复复用已有 completed-scope cache（后续已被下文持久追赶替代），在 fresh authenticated outbound dial 上只向落后一代的成员定向补发已有 source/certificate；不增加广播循环、第二套治理真值、历史拷贝或重复共识。握手在现有 identity signature 中额外绑定 active ValidatorSet version，用于跳过已经进入新集合的重启成员，避免旧 scope 导致 exact-set transport 拒绝连接；这个提示不授予 membership/投票权，最终 source、frontier、rotation、quorum/finality 校验保持原规则。开发 wire 版本保持 1，认证 payload 为 81 字节，没有旧格式兼容层。新增密码学回归证明签名后篡改该版本字段会被拒绝，原伪造身份回归继续运行。

既有单系统四进程 reentry 回归同步改成 transition finality 后才启动 locked 目标，成为普通门禁中的确定性回归；复用其部署、签名、安全 fence 与最终业务夹具。混合验收的 Linux 磁盘读取由原生 StateStore/helper 执行，只经本地受控管道传递现有 canonical payload；不依赖 WSL UNC 文件共享对运行中 store 的 Windows I/O 语义。helper 不是公共 API，其输出含私有业务状态。日志 `target/mixed-windows-linux-verified.log`，真实混合场景 36.74 秒，仅为本机验收耗时。

此前的补发依赖 64 completed scope 内存缓存，已由下述持久追赶修复替代。独立主机、WSL NAT 或外部网络连通性、更多节点容量仍须单独验收。启动、复现命令与诊断目录边界见 [node-operations.md](node-operations.md)。

最终 Windows 与 Linux 两端 fmt/check/clippy、完整 `cargo test --all-targets -- --test-threads=4` 均通过，仓库 diff-check 通过。每端 233 项主集成 + 1 项独立 34 成员目标、26 项单元测试及既有进程隔离子检查通过；Windows 常规门禁额外显示 1 项需要 WSL 环境的 ignored 混合测试，该项已由上述独立 release 命令真实通过。常规日志分别为 `target/mixed-regular-windows-verified.log` 与 `target/mixed-regular-linux-verified.log`。没有放宽生产 deadline、identity/finality 认证、签名 fence 或原子持久化要求。

## Membership 持久追赶修复

移除基于 completed cache 的最近一次 transition 补发。握手的已签版本提示触发按需读取现有 durable transition proof，逐版验证 quorum、rotation 与本地 frontier，原子落盘后刷新 exact-set authority。临时 proof 会话只读且限制 64 步/5 秒，持有全局连接 permit，不加入 discovery、不替换普通连接；复用已有 proof loader/codec/store activation，没有第二套 membership 真值或重复共识。

另修复后续 transition 清空 pending safety recovery 的问题：本地 credential 未变化时保留独立 rotation 的证据，原目标集合引用既有 proof；当前 recovery proof 到位后 fence 提升至最新集合。正常四进程回归增加 V1→V2→V3、全部健康提供者重启、目标追赶后再重启、V1/V2 签名隔离和必要 quorum 业务。混合 Windows/Linux 回归增加全部 proof 提供者重启。真实 QUIC 失败回归验证 proof 缺失、签名伪造与合法但 frontier 不匹配三种响应均不改变 generation、membership 或 locked 状态。

追赶仅覆盖具有连续可验证 membership 证明且各步 frontier 匹配本地状态的路径；不承诺任意丢失业务状态的自动修补。

本次最终验证：Windows 与原生 WSL Linux 两端 fmt/check/clippy、完整 all-targets 测试均通过，diff-check 通过。每端 27 项常规单元与既有隔离子检查、233 项主集成、1 项 34 成员集成通过；Windows 的混合 ignored 项另以 release 显式执行并通过，实际场景 48.98 秒。日志分别为 `target/membership-windows-verified.log`、`target/membership-linux-verified.log`、`target/membership-mixed-verified.log`。混合验收全部 owned 子进程已停止，成功现场按夹具清理。

## 双平台部署交付

新增 host-native 部署打包器（zip/tar.gz、文件校验 manifest、公开模板/文档/平台脚本，不包含节点数据或私钥）；`--version` 用于匹配 package version/OS/architecture。`node-check` 与 node/Windows service 复用同一启动路径和锁，检查实际 QUIC bind/capabilities 后退出，保持业务 generation；节点现支持事件驱动的正常停止，Linux SIGTERM/SIGINT、Windows SCM Stop/Shutdown 均不增加轮询或新协议状态。

Windows 增加平台专用 windows-service 依赖，原生 SCM dispatcher/reporting、独立虚拟 service account、受限 ACL、delayed-auto、failure actions 和有界日志；Linux unit 使用已有非 root 用户、UMask/目录权限/可写边界、journal 和 on-failure restart。安装拒绝覆盖，卸载保留状态与密钥。复用 CLI provisioning、签名、状态查询的显式 service-smoke 验证安装→实际业务→停止/启动→异常重启→卸载后身份/状态连续；Windows 提供无需管理员的 Plan 检查与单独的管理员生命周期验收入口。操作与验证边界见 [deployment.md](deployment.md)。

部署交付最终验证：两端 fmt/check/clippy/all-targets 均通过；Linux release 真实 systemd 安装、签名业务、stop/start、异常新 PID 自动重启与卸载后状态持续通过；Windows release Plan/预检/路径空格与 SCM-only 入口失败边界通过，随后 UAC 管理员进程的真实 SCM smoke 也通过安装、拒绝覆盖、签名业务、stop/start、异常退出后新 PID 自动重启、身份/任务连续及卸载保留状态，退出码 0。混合 release 回归通过（39.88 秒）。独立主机可达性和整机重启 boot 验收仍未完成；操作与证据见 [deployment.md](deployment.md)。本次测试正常结束后仅关闭本次拥有的服务/进程，保留失败私有现场，不改变实际部署数据。

追加收尾：Windows/Linux 解压交付包的真实系统服务生命周期均再次通过；混合集群再次通过（35.67 秒）。本机没有第二台主机，独立主机网络验收保留为条件具备后的现场验证；整机重启验收只补证 boot 行为，本轮不重启电脑。当前部署开发不因此阻塞；详细日志、Linux 测试目录权限失误和最终文档包见 deployment.md。

## 连续地址分配恢复与资源边界复查

本轮实际复现并修复三个节点可用性问题：

- 不可能分配的请求先入持久队列再报错，导致无效请求留下 restart 待办；现在先调用权威 CurrencyAllocation 构造器校验，再入队。回归核对拒绝不改变 generation、不留下绑定，重启后的正常发行和 succeeded 精确重试均正常。
- 两个各自合法的区间竞争最后的 u64 空间时，获确认的区间推进 frontier，使另一请求重建失败；原恢复循环向上传错而退出。现在记录既有有界拒绝诊断并清理该任务待办，保留 TaskId 绑定和全部已确认区间；普通开户业务及下一次重启后的开户仍能成功。
- durable 分配待办超过同 scope 的 64 候选窗口时，原启动恢复把 PendingFutureMessagesFull 当作节点故障。现在保留超窗待办，已有区间确认后复用原恢复入口继续推进。65 个真实持久发行请求在重启后全部完成，余额与 frontier 精确匹配，没有扩大候选窗口或新增轮询、队列和失败结果表。

另补获 QC 后、prepare 前的崩溃边界：直接安装合法分配证书后重新构建 runtime，自动完成合法业务；非法业务准备失败仍永久保留区间，精确重试不重新分配，后续发行继续。原证书在 frontier 推进后重复安装不增加 generation。现有四节点乱序提交、完整 shared payload 收敛和 membership frontier barrier 回归继续保留。

复现日志 `target/allocation-regression-before.log`、`target/allocation-capacity-before.log`；定向通过日志 `target/allocation-targeted-verified.log`。首次 Windows 四并发全套测试中的既有四节点 CLI 恢复场景超时，单独重跑通过（`target/allocation-cli-reentry-rerun.log`）；资源争用只是推测，未证明根因，没有修改生产 deadline 或协议断言。

最终 Windows 与 WSL Ubuntu 26.04 两端 fmt/check/clippy/diff-check 通过，完整 all-targets 以两并发运行通过；Windows 主集成 237 项通过、1 项环境限定 mixed 场景忽略，Linux 主集成 237 项通过，两端独立 34 Validator 回归均通过。日志 `target/allocation-windows-final-verified.log`、`target/allocation-linux-final-gates.log`、`target/allocation-linux-final-verified.log`。本轮新增四项重要回归，不改 wire/snapshot version 或 u64 连续地址规则。完整测试使用当前 debug 构建；两端另完成当前源码的原生 `cargo build --release --locked --bin second`，新的部署包位于 `target/dist-allocation-verified`，此前服务验收阶段的包不包含本轮修复。原生 release 构建日志 `target/allocation-windows-release.log`、`target/allocation-linux-release.log`。
## 局部冲突仲裁：已批准，实施中

真实四节点复现了同一货币的两个转账、同一账户地址的两个开户、同一支付地址的两个退役各占 2 个节点；无法取得 3/4 quorum，投票后本地取消拒绝，重建后占用保留；无关开户成功。复现日志 `target/resource-conflict-reproduction.log`。该回归的重建阶段仅验证 durable 锁和占用，不声称 ephemeral 地址变化后的网络重连已经通过。

安全释放方案见 [conflict-arbitration.md](conflict-arbitration.md)。先实现了仲裁必需的精确冻结计划同步：signed LegalTask 与 Transfer/LeakRepair 选择共用有界 source，Issue 等参数从现有权威事实重建，复用 prepare/claim/apply 校验。新增真实选择差异回归核对远端选择正确提交，并核对有效但摘要不同的选择、错误归属、截断和尾随数据被拒绝。非法 fetched source 不持久化 TaskId 绑定或 payment execution。

仲裁候选、同 scope Commit/Abort BFT、取消终态的原子持久化、成员交接边界和 2∶2 收敛验收尚未完成；不宣称冲突活性已经修复，也没有放开投票后的本地取消。

冻结 source 前置改动的最终验证：Windows 与原生 WSL Linux 的 fmt/check/clippy/all-targets 测试均通过；每端主集成 239 项、常规单元 29 项及既有隔离子检查通过，独立 34 Validator 目标通过。Windows 的环境限定 mixed 项保持 ignored，本轮没有重跑 mixed release 或系统服务验收，也没有更新交付 release 包。日志 `target/conflict-source-windows-final.log`、`target/conflict-source-linux-final.log`。新增两项冻结选择/篡改单元回归、一项真实 QUIC 四节点冻结选择私有拉取回归，以及上述缺失仲裁的行为刻画回归。

第二项前置改动：最高有效 digest prevote QC 保存到已有 BftLocalState，与依赖该 proof 的 prevote/precommit 同次原子落盘；旧轮次或已投 precommit 后获得更高 proof 时只补必要元数据写入，重复不重写。Snapshot loader 对 proof 的 scope、round、phase、digest、确切集合及 quorum 签名复核；runtime 注册时恢复 proof，重启 proposer 选择已锁候选并携带 unlock certificate。新增 proposer 重建回归并核对旧 proof 重放不增加 generation；65 待办回归扩展为最后一项已锁的恢复场景，优先恢复所锁任务再填充 64 窗口，全部待办继续排空。没有开放 Abort、修改终态或提前释放业务占用。

第二项改动的最终验证：两端 fmt/check/clippy/all-targets 和仓库 diff-check 通过；每端主集成 239 项、常规单元 30 项及既有隔离子检查、独立 34 Validator 目标通过。Linux 日志 `target/conflict-qc-linux-final.log`，Windows 最终通过日志 `target/conflict-qc-windows-verified.log`。首次 Windows 与 Linux 同时跑全套时，Windows 的既有四进程 CLI 恢复场景在 transition-submit 报 connection lost（`target/conflict-qc-windows-final.log`）；单独重跑通过（`target/conflict-qc-cli-rerun.log`），随后完整 Windows 两并发重跑通过。长期节点 stderr 未被该夹具持久保留，现有失败现场不足以确认根因，不能断言一定是资源争用；没有放宽 deadline 或业务/协议断言。失败私有现场保留，不清理部署数据。

仲裁安全设计仍须落实跨成员集合的 pending resource 交接。只绑定任务来源版本不能证明新成员继承旧 private claims；拟定 epoch 准入封闭、quorum 认证交接承诺和资源 fence 的约束见 conflict-arbitration.md。目前只是安全推导，不能作为已验证能力或已复现漏洞的修复声明。此次前置实现不提供自动 Commit/Abort 选择、取消终态或跨集合交接。

继续实现冻结源被占用时的只读验证：复用同一业务准备路径在临时状态/空 claim book 中完成全部操作与摘要检查，再查询原货币/生命周期 claim 索引，获得所有实际占用者，不复制第二套资源语义。仅占用拒绝路径增加这次必要验证；正常成功路径不重复执行。新增多资源、非法后续操作、篡改选择、同账户独立货币、自身引用和无持久副作用回归；现有四节点相反顺序回归扩展为真实 QUIC 拉取验证三类精确占用诊断。此阶段仍不安装远端占用、不签 Abort、不自动释放，不能宣称 2∶2 活性问题已修复。

这轮完整 Windows 检查进一步暴露 34 节点共识不能按测试期限完成，初次与单独重跑分别失败（`target/conflict-witness-windows-final.log`、`target/conflict-witness-34-rerun.log`）。只去重内部 QC 验签后仍失败（`target/conflict-witness-34-verified-path.log`），不能把重复验签视为唯一根因。新增失败持久状态诊断确认全员 frontier=3、prepared=true、succeeded=false，节点 round 分布 27–33，部分节点已锁同一计划，伴随未来消息队列满和传输超时；原生 Linux 同阶段全套通过（`target/conflict-witness-linux-final.log`）。

已修改阶段调度：使用现有 durable round 增长窗口，新阶段从处理完成时间开始计时；极端 duration 溢出不产生立即唤醒。没有修改测试总期限、扩大消息队列或降低签票/QC 约束。内部网络已验签 QC 和本地已验票聚合 QC 复用验证结果，公开 driver 与快照恢复仍独立验签；恢复 proposer 回归扩展了伪造 QC 不可触发候选切换和高 round 窗口到期行为。修改后的 Windows 34 节点场景通过，42.68 秒（`target/conflict-witness-34-backoff.log`）。这证明该次验收恢复，不能代替跨主机性能验证，也不等于冲突取消仲裁完成；三次失败的私有现场保留。

上述首次窗口方案按每个 round 放大，在完整 Windows 验证和单独 CLI 恢复回归中再次出现等待业务 quorum 超时（`target/conflict-witness-windows-verified.log`、`target/conflict-witness-cli-rerun.log`、`target/conflict-witness-cli-diagnostic.log`）。持久诊断明确三个必需成员已 safety-ready，但新业务尚卡在 CurrencyAllocation、round 为 4/2/2，故不能称为业务计划冲突，也不能当作纯粹偶发。最终窗口按完整 proposer 轮转周期增长，并按必要 quorum 验证量缩放，精确公式见 DESIGN.md；避免逐个 proposer 放大等待。CLI 恢复回归在原总期限/断言下通过，57.61 秒（`target/conflict-witness-cli-cycle.log`）。旧方案测试记录仅用于排查，不冒充最终版本验收。

最终窗口策略的 Windows fmt/check/clippy/all-targets 全部通过，主集成 239 项、常规单元 31 项及隔离子检查、独立 34 节点目标通过（34 节点 30.50 秒），日志 `target/conflict-witness-windows-cycle-verified.log`。Linux 最终重跑主集成出现 CLI 子进程启动 stdout EOF，原夹具只断言启动行字段数，未呈现 stderr（`target/conflict-witness-linux-cycle-verified.log`）；已补 EOF 时真实退出状态/stderr 诊断，不能把这次完整运行记为通过。

Linux CLI 单独复验通过，50.22 秒（`target/conflict-witness-linux-cli-rerun.log`）；随后最终源码完整 fmt/check/clippy/all-targets 全部通过，主集成 239 项、常规单元 31 项及隔离子检查、独立 34 节点目标通过（34 节点 47.58 秒），日志 `target/conflict-witness-linux-cycle-final-verified.log`。Windows 补诊断后的 fmt/check/clippy 也通过，仓库 diff-check 通过。启动 EOF 未再次复现，初次未保留 stderr，因此根因仍未确认；不把诊断补充或重跑通过称为已经修复该启动失败。失败私有现场继续保留。两端本轮使用原生 debug 构建，未重建 release 交付包或重做 SCM/systemd 服务验收。

当前仲裁进度：完成真实冲突复现、冻结 source 精确同步、最高 prevote QC 持久恢复、占用隔离下的完整只读冲突验证，并修正本轮验收暴露的阶段调度。Commit/Abort 同任务互斥决策、TaskBinding 取消终态与原子释放、跨委员会待决资源交接仍未实现；原来的 2∶2 冲突不会自动解开。最终门禁通过不能当成这些待办已经完成。



## 2026-10-04：认证取消终态与同任务决策恢复

实现 TaskBinding 的 Pending/Succeeded/Cancelled 单一结果、域分离并绑定 exact 委员会的 Abort statement、同 scope 的 Abort finality 签票、证书驱动原子取消及 claim book 重载。取消保留请求绑定、地址区间和不可逆最终票；错证书、已成功/Finalized/冲突最终票拒绝且不写盘，证书重放无 generation 变化。取消状态接入 QUIC/CLI 和示例重试工具；共享恢复按版本带入取消结果必要的历史委员会，避免私有占用输出或签名排列成为共享真值。

运行时支持已有 Abort 认证结果的恢复、QC/最终证书驱动的候选准入，以及重发已有最终票。单元回归使用四个独立持久库验证这一条认证决策传播路径；它没有验证首次冲突 witness 准入或真实 QUIC 2∶2 自动仲裁。真实 CLI 回归扩展取消状态、重提拒绝及 Windows 重试脚本的终态停止行为。

当前尚未完成首次冲突候选准入、无本地计划的 Abort-only 上下文、释放后重试、同任务多冻结计划及跨委员会资源交接。不得因本轮测试通过宣称整个局部仲裁已交付；未重建 release 或重做系统服务验收。

首次 Windows 完整运行主集成 238 通过、1 失败：CLI 恢复场景中三个必需节点均已 prepared，一个已成功、另两个 BFT state 已清除但还未完成（target/task-abort-windows-verified.log）。不能把该次门禁记为通过。补充失败时冻结计划摘要、最终票锁和子进程 stderr 诊断；原私有现场保留。单独复验原期限通过，53.35 秒（target/task-abort-cli-diagnostic.log），首次失败根因尚未确认。另通过代码检查修正恢复时 finality-ready 对旧的相反 prevote 锁的优先级，并复用本地 scope 索引及取消校验的委员会哈希前缀；这些有独立回归证据，不冒充上述 CLI 失败的确定根因。
### 2026-10-04 取消同步与跨平台验证补充

认证取消的任务同步已区分业务 Commit 源与 Abort 决策：本地活动/取消上下文能准确识别 Abort，避免无效私有拉取；durable Commit/Abort 完成后按任务通知清理拉取和公告重试。裸 Abort 提议不能据此投票，错误域不能匹配。文档与回归同步更新，完整自动冲突仲裁仍未交付。

前一批 Windows 完整门禁通过（34 个单元、239 个集成和单独 34 节点验收；另有隔离辅助和混合平台条件测试跳过）。Linux 默认 20 测试线程完整运行两次出现三个 BFT 测试五秒建连超时，同组七个测试单独运行全部通过；四测试线程完整运行通过（34 个单元、239 个集成和单独 34 节点验收）。首次 Windows 业务恢复失败与 Linux 并发建连超时的根因仍未确认，失败日志保留。这些数字对应同步补充之前的源码；后续门禁结果另行记录。

同步补充后的最终源码：Windows 和原生 WSL Linux 均通过 fmt/check/clippy/all-targets；每端常规单元 35 项、主集成 239 项、既有隔离子检查及独立 34 节点目标通过。Windows 使用默认测试并发，34 节点 32.86 秒，日志 `target/task-abort-sync-windows-verified.log`；Linux 明确使用 `RUST_TEST_THREADS=4`，34 节点 47.56 秒，日志 `target/task-abort-sync-linux-verified.log`。仓库 diff-check 通过，两端无遗留 second 测试进程。未把默认 Linux 并发建连失败称为已修复；未重建 release 包或重验系统服务。后续自动仲裁仍必须补未持有计划节点的权限、首次冲突证据准入、事件重试、同 TaskId 多计划及成员待决资源交接。


## 2026-10-04：交接摘要、准入封闭与恢复资源保护

已接入本地成员候选的 canonical 冻结计划正文、公开 source 中的根、持久候选与受理后的原子准入封闭；最终成员签票和激活重新检查覆盖与认证根。普通 snapshot 及共享恢复校验交接正文、请求绑定和对应 membership proof。恢复载荷中的必要 proof 写入原有历史证明库，不建立第二份业务状态；不同 quorum 签名排列不参与 shared-state 摘要。

回归覆盖遗漏任务拒绝且不写入、封闭准入重启、原集合任务继续完成、共享恢复后的冲突资源 fence、独立资源准备、伪造 proof、正文篡改、当前集合 Abort 拒绝、原集合 Abort 消费及释放后资源可用。运行时消费继承任务的 Abort 最终证书时不授予签票权；已成功任务的迟到 Commit 证书继续走原有处理路径。

并发验收还暴露共识连接维护排在 public bootstrap 之后的依赖，现有同一维护循环改为共识先行。BFT checkpoint 回归移除了无关 public 全互联前置，保留原五秒 BFT 建连、认证与持久化断言；未延长业务期限或限制测试并发。旧失败日志保留，最新门禁结果以本次最终日志为准。

完整自动仲裁仍待实现：跨节点交接正文合并及私有按需传输、冲突 witness 初次受理、未持有普通计划节点的 Abort-only 权限、同 TaskId 冻结变体处理、释放后的事件驱动重试。当前真实 2∶2 测试仍保持阻塞断言，不宣称修完。

Linux CLI 夹具的一次完整运行保留了明确启动失败：`Address already in use (os error 98)`。预配置节点的预约端口原先来自 `bind(0)`，释放预约至子进程启动期间可能被其他并发 QUIC 端点分配。夹具现在读取 Linux 的真实 `ip_local_port_range`，用共享递增候选在该范围之外实际绑定预约 UDP socket；预约仍保留至既有启动步骤，配置仍使用真实预约地址。地址占用由 OS 检查并跳过，不修改节点监听错误处理，不盲目重跑失败子进程；Windows 预约行为保持不变。完整默认并发门禁验证此修复。

本阶段最终门禁：Windows fmt/check/clippy(-D warnings)/all-targets/diff-check 全部通过，37 常规单元、239 主集成、独立 34 节点目标通过；Windows 的混合 WSL 场景按既有规则忽略。Linux 原生 checkout 默认测试并发下 fmt/check/clippy(-D warnings)/all-targets 全部通过，37 常规单元、239 主集成、34 节点目标通过（主集成 43.71 秒、34 节点 46.48 秒）。日志为 `target/task-handoff-windows-port-verified.log` 与 `target/task-handoff-linux-port-verified.log`。这些结果只证明当前实现及所列回归；未重建 release 包、未用最新源码重做 SCM/systemd、未完成真实 2∶2 自动仲裁验收。

## 2026-10-04：业务基线、原委员会与恢复 provider 修补

交接认证新增已完成业务事实、终态 TaskId 与确认分配的基线绑定；没有活动计划不能省略这些事实。基线排除本地待准备进度，相同确认分配的不同 quorum 子集和 retry-source 清理不会产生不同根。已持久候选覆盖的任务可以完成后继续认证，未知计划仍拒绝。继承的未决 TaskId 不允许换到当前委员会重新准备，且拒绝不改变 generation。

修复真实 QUIC 恢复 provider 遗漏交接附带证明：发布和启动恢复统一使用 from_persisted。认证正文增加带内容/证明指纹的临时验签缓存，避免业务基线交接在活动任务清空后仍重复验证旧 quorum；签名篡改不能沿用缓存。BFT/public 共存探测使用独立 public client，保留实际 Ping、认证结果和 BFT 三 peer 断言。

此批初次完整运行 Windows 的 CLI reentry 业务共识超时，Linux 的 public 探测连接断开；现场和失败日志保留。修正后、待准备字段归一化之前，两端默认并行 fmt/check/clippy/all-targets 通过，38 常规单元、239 主集成、34 节点目标通过；日志 handoff-baseline-cache-windows-verified.log 与 handoff-baseline-cache-linux-verified.log。后续最终源码门禁另记，不将一次通过当成所有间歇故障根因已经确定。

完整自动仲裁仍未交付：跨节点交接合并/私有传输、新成员基线同步、首次 witness 受理、无普通计划节点的 Abort-only 上下文、同任务冻结变体和取消后的自动重试尚需接通。未重建 release 或用最新源码重验 SCM/systemd。

最终基线源码的全量运行未通过：Windows 237/239（冲突复现连接超时、CLI 重试耗尽）；Linux 238/239（正常私有拉取被拒绝）。对应日志 handoff-baseline-final-windows.log、handoff-baseline-final-linux.log 保留。随后修复已定位的拉取重试竞态：同目标高 round 不重置进度，迟到/重复响应有界丢弃，旧 unavailable 不能重置新目标，当前来源篡改继续拒绝。新回归和静默来源切换回归通过；不将此修复当作 Windows 超时的确定根因。最新完整门禁另记。

最终修补源码门禁通过：Windows 与原生 WSL Linux 均使用默认测试并行，通过 fmt/check/clippy(-D warnings)/all-targets；40 常规单元、239 主集成、独立 34 节点目标均通过。Windows 主集成 48.93 秒、34 节点 31.71 秒；Linux 主集成 52.84 秒、34 节点 46.01 秒。最终日志 target/handoff-fetch-final-windows.log、target/handoff-fetch-final-linux.log，diff-check 通过。Linux 拉取竞态有实现级回归与真实 QUIC 门禁证据；Windows 早前间歇连接超时的根因仍未确定，保留失败现场，不因最新通过宣称已经根治。完整自动冲突仲裁及最新 release/系统服务验收仍未完成。

2026-10-04 追加：首次冲突源受理、持久 Abort-only 权限和证书后事件重试已接入，权限隔离单元回归通过。真实 QUIC 已出现取消 prevote QC，但全部成功/取消终态验收仍未通过；未完成项继续保留，详见 conflict-arbitration.md 末节。

首次四节点自动仲裁终态及中途重启均已通过，详见 conflict-arbitration.md 最新验收节：三类 2∶2 不再仅验证阻塞。相同 TaskId 冻结变体、既有 Commit QC 与相交持有者的活性，以及跨成员交接合并/传输与基线同步仍未完成；最新全量门禁随后记录。

门禁进度：Windows 全量通过（contention-gates-windows.log）。Linux 初次全量的新仲裁回归通过，但既有 CLI lost-ack/node-switch 等待终态超时，238/239；contention-gates-linux-failed.log 保留。该失败补入具体 TaskId/BFT/连接诊断，等待窗口与断言不变，最新全量结果另记，不声称已找到根因。

原生 Linux 最新 fmt/check/clippy/all-targets 全通过（contention-gates-linux-native.log）：40 常规单元、239 主集成、34 节点发现目标。新增诊断那一轮的错误 Windows 挂载盘临时目录已修正，不计作原生门禁；失败日志保留。首轮 CLI 间歇超时仍未确定根因。Windows 诊断补充后的静态门禁也已通过，最后一次 all-targets 复核另记。

最终当前源码门禁：Windows fmt/check/clippy（contention-diagnostic-windows.log）及 all-targets（contention-final-tests-windows.log）通过；原生 Linux 完整门禁（contention-gates-linux-native.log）通过。两端 40 常规单元、239 主集成与独立 34 节点目标通过，diff-check 通过；Windows 既有混合 WSL 场景按原规则忽略。首次三类不同 TaskId 2∶2 仲裁及中途重启已交付到所列测试边界；冻结变体、既有 Commit QC 冲突活性、跨成员交接和最新 release/系统服务验收未交付，首轮 CLI 间歇超时未定位。

2026-10-04 Commit QC 冲突恢复：三类资源的已有 QC 四节点重启回归和原 2∶2 回归通过（commit-qc-third-quic.log）。本地竞争请求改为完整验证后受理 Pending，QC 观察不授予 Commit 权限。全量门禁正在复核；迟到未知源、同 TaskId 冻结变体及跨成员交接仍有未交付范围，详见 conflict-arbitration.md。

2026-10-05 最终复核：Commit QC / 最终证书冲突恢复和迟到终态处理已实现，Windows 完整门禁及 Linux 最新 check/clippy + 默认并发 all-targets 复核通过（commit-qc-reused-final-windows.log、commit-qc-reused-final-linux-recheck.log）。Linux 首轮 3 项既有间歇超时日志保留，根因未定位；单独复现通过不等于已修复。原冻结计划准备器为单一资源取得入口。未完成范围与部署包验收边界见 conflict-arbitration.md 最新节。

## 2026-10-05：冻结候选实现进度

同一 TaskId 的不同冻结选择已能在真实四节点 2∶2 初始条件下提交同一计划；候选保留在既有 PreparedTask 内，旧资源占用和 prevote 经重新加载保留，最终仅执行证书绑定计划。本地 handoff 规范化为逐候选条目，候选切换不改变根且不得遗漏旧义务。来源与候选的详细约束、失败现场及完整门禁结果见 docs/conflict-arbitration.md 最新节。

该进度尚不包含被其他任务占用的替代候选取得权利、跨节点交接正文并集、新成员业务基线同步、迟到竞争请求的完整处理或间歇超时根因定位。用户确认没有独立主机；独立主机验收保留为环境未满足，Windows/WSL 不替代该验收。
