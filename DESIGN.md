# Second 项目设计

当前连接修复后的静态门禁两端通过，259 个构建输入 SHA-256 一致；同机同时运行两套默认 all-targets 全量，连续两轮两端各 344 项通过（`target/repair-complete-paired-*.log`、`target/repair-complete-repeat-*.log`）。新固定原生 Release 的 2 Windows + 2 WSL Linux 混合验收通过，28.82 秒（`target/repair-mixed-release.log`），前后二进制摘要不变。首批部署包 `target/dist-connectivity-20261009` 的 manifest、验收二进制、当前脚本/文档、相对链接、无密钥白名单与 Linux 权限已核对（`target/repair-package-verification.json`）。下文存储阶段的十分钟持续验收和系统服务记录对应当时二进制，不作为新连接修复产物的实测结果。

### 2026-10-09：公开邻居发现与断线退避

同一连接修复固定二进制随后完成十分钟持续段（601 秒、134 笔转账，含基线 630.05 秒，`target/repair-mixed-soak-600.log`）及两端解压包真实 SCM/systemd 生命周期（`target/service-connectivity-windows.log`、`target/service-connectivity-linux.log`）。覆盖四成员停机 quorum、重启精确补齐、完整状态一致、旧请求幂等，以及系统服务异常退出自动换 PID 恢复和卸载保留状态。前后二进制摘要、259 个构建输入未变化，临时节点/服务/私有目录已清理。文档更新后的最新包位于 `target/dist-connectivity-soak-20261009`；其程序和管理脚本与实际服务验收原包一致，核验见 `target/repair-soak-package-verification.json`。整机 boot 和独立物理主机网络未执行。

现有测试扩展行为断言后，旧实现复现同连接重复 GetPeers 与已有连接断开后沿用发现退避（`target/repair-peer-before.log`，两项失败）。outbound 拨号已经持久化 authenticated pinned record，取消服务启动时的重复广告地址查询；bootstrap 保留原邻居发现与认证自身广告记录的更新。inbound 仍通过远端自身广告获取可拨监听地址，不把 QUIC 客户端临时端口当作监听地址。公开维护的原循环在已建立连接数下降时重置退避，未发现新候选的空闲网络仍指数退避。原四秒重连及七秒沉默候选测试预算不变；只将连接空闲观察延长到十一秒覆盖旧退避失效阶段。BFT/分配偶发短期限失败仍单独核查，不能以这两项根因覆盖全部失败。

双全量第二轮仍复现 Linux 沉默候选的串行阻塞。bootstrap 候选探测改为至多四路并行推进，直接借用当前 runtime 并复用原拨号及 GetPeers，保留全局连接 permit、认证后才持久候选、完整 PeerRecord fallback 和三方记录先认证的边界。同一 NodeId 不占多个并行槽；不新增依赖、后台扫描或无边界 fanout。现有测试验证健康候选在沉默连接关闭前进展，避免只用“最终在放宽期限内成功”代替阻塞回归。

继续采样确认同步 `Endpoint::client` 创建在 WSL 压力场景多次耗时一至两秒，慢调用记录最大 2,757.64 毫秒（`target/repair-bind-paired-linux.log`），公开/BFT 拨号原先在网络 executor 上执行该同步步骤。两条节点拨号路径现将同一个 QuicClient 构造器交给既有 blocking pool，worker 在创建完成之前一直携带原连接 permit；返回或取消后的资源释放与错误传播保留。并未延长网络 deadline、绕开 pin/NodeId/BFT 校验，或新增永久线程/后台扫描。临时采样从 Linux 源码撤回，后续证据以两端同源最终测试为准。

上述隔离实验之后，最终实现复用节点已绑定的监听 Endpoint：同地址族的 outgoing QUIC 与 incoming QUIC 共用一个 UDP socket/driver，client handle 的证书 pin 仍彼此独立，统一由原 client-config 入口构造。不同地址族不强行复用不能发送的 socket，保持必要的独立端点和 blocking worker permit 生命周期。真实 QUIC 回归核对实际源端口相同、两种 pin 各自认证自己的服务器、错误 pin 被拒绝后原连接仍响应。无全网广播、无独立连接真值、无新增依赖。

共享端点的关闭权属于 NodeRuntime 生命周期：runtime 释放时显式关闭 endpoint，避免独立公开服务任务保留的 client clone 让停止后的节点继续响应。现有双向同时拨号回归在保留 canonical outbound handle 的情况下释放 lower-NodeId runtime，核对仍存活的另一节点 Ping 被拒绝；临时客户端或 BFT 拨号用 server clone 的释放不会关闭整个共享端点。

### 2026-10-09：开发收尾核对与历史取消恢复

收尾核对发现历史交接 Abort 路径仅原子安装取消证书，没有唤醒等待资源的当前请求，也没有通过既有完成通知清理运行时。新增最小回归在真实认证基线导入后提交冲突请求，合法原委员会 Abort 到达后请求仍无资源权；失败日志 `target/closure-abort-waiter-reproduction.log`。已有真实 QUIC 基线导入回归补充完成事件断言，也确认历史 Abort 无完成通知（`target/closure-abort-reproduction.log`）。

历史 Abort 现在在释放前通过原 contender 索引取得受影响请求，覆盖本地计划及同 TaskId 的全部已认证交接上下文；证书仍由同一持久化入口验证原委员会并原子取消。释放后复用业务完成路径重试 contender、恢复持久请求及更新恢复候选，并通过同一有界完成缓存清理会话、记录/去重事件。完成恢复逻辑按职责从共识主文件移至 `runtime_bft_consensus/completion.rs`，没有新增持久状态、协议格式、后台扫描或签票权。新回归同时覆盖有/无本地正文、伪造证书零写入、当前 quorum 完成等待业务、重复证书幂等和新成员始终 signing locked；定向修复验证已通过（`target/closure-abort-waiter-fixed.log`、`target/closure-abort-variants-fixed.log`）。以下历史进度不作为当前待办清单。

上面的取消回归扩展到同请求的两个冻结选币计划后，复现只查首个交接计划遗漏其他等待者（`target/closure-abort-variants-reproduction.log`）。原 contender 查询现按同 TaskId 的有序范围覆盖全部已认证交接候选，与本地候选一起复用一次派生索引并去重；不以首次上下文替代被释放的全部资源。

按确认设计逐项核对后，当前内部协议、CLI、节点与 Windows/WSL 支持范围已有功能验收证据。默认并发采样复现存储写入延迟，完整/公开状态与 commit 文件复用已有文件优先打开的路径，仅缺失才创建目录和文件；快照写入收敛为单一原语。提交、fsync、内容校验和原子性约束保留，首次嵌套目录由已有持久化回归验证。所有临时采样已移除，最终两端静态门禁及默认全量各 343 项通过，Windows 再重复两轮默认集成各 247 项通过；258 个构建输入 SHA-256 一致。冻结的原生 Release 四进程混合集群持续运行 611 秒、完成 132 笔支付并通过轮流停机、精确重提补齐、完整状态和旧请求幂等，日志 `target/stability-frozen-mixed-soak.log`。同一固定二进制的包内真实 SCM/systemd 生命周期均通过。此次修复和指定部署验收已完成，但额外同机双全量叠加的 Linux 短连接期限仍失败，不能声称全部稳定性问题已消除。开发矩阵、失败记录、性能数字和验收边界见 [当前开发完整性核对](docs/development-status-2026-10-03.md)。

### 2026-10-08：当前源码 Windows / WSL 混合集群验收

Windows 与 WSL Ubuntu 26.04 从当前工作区分别原生构建 Release 主程序及 `mixed_snapshot_probe`，使用 `--locked`；Linux 源码、构建和节点数据位于其原生文件系统。构建前复制的 Cargo 文件、源码、测试、示例与配置共 255 个文件经 SHA-256 比对一致，两端产物分别报告 `Second 0.1.0 windows x86_64` 和 `Second 0.1.0 linux x86_64`。

现有显式混合验收通过，1 项、0 失败，场景耗时 41.78 秒，日志 `target/cross-mixed-acceptance.log`。两个 Windows 与两个 Linux 独立节点验证双向固定证书 QUIC、错误证书拒绝、开户/发行/支付及全体持久共享状态一致；Windows 成员离线后混合 3/4 quorum 完成业务，重启成员补齐。Linux 成员从认证状态恢复到全新 base，健康成员完成密钥轮换并全部重启后，迟到恢复节点仍可取得持久 V2 proof；签名隔离跨重启保留，随后由 V2 recovery proof 恢复签名安全。再停另一个 Linux 成员，恢复节点作为必要 quorum 成员完成新业务；精确余额、签名 floor 和完整 canonical shared payload 均符合原断言。本次子进程已停止，临时节点目录已清理。

本轮无需修改代码、期限或安全断言。此为同机 Windows / WSL 跨系统验收，不证明独立物理主机的路由、防火墙、外网可达性或整机重启后的服务启动；当前验收范围按用户确认止于 Windows / WSL，不等待外部主机。

### 2026-10-08：20 路并发下的存储热点修复

修复前显式 20 路全量复现中，integration 239 项通过、8 项实时期限失败（87.70 秒）。临时分段测量显示运行器排队开销很小，但快照访问的锁路径耗时明显：每次访问都调用 `create_dir_all` 并用可创建模式重新打开已存在的锁文件。独立 1,000 次打开/加锁测量为原路径约 0.80 秒、直接打开已有文件约 0.10 秒；这是本机结果，不是跨平台性能承诺。

完整状态与公开状态共用的 `lock_store_file` 现在先打开已有文件，只在 `NotFound` 时创建父目录及文件；其他 I/O 错误仍直接返回。每次访问仍重新打开并持有同一跨进程文件锁，不缓存文件句柄，也不省略 durable commit、镜像元数据、签名或原子提交校验。重复的同轮同摘要 Precommit QC 在已持久化 readiness、且无需推进准备阶段时不再重复提交快照；现有 BFT 冷重启回归增加了重复证据不推进持久 generation 的断言。

已移除 `.cargo/config.toml` 的默认 4 路测试限制。两轮显式 20 路全量验证通过，每轮 lib 94 项、integration 247 项和 34 节点真实 QUIC 回归 1 项。第一轮带临时耗时采样，三个目标分别耗时 13.44、43.62、57.84 秒；采样代码全部移除后，最终代码复核分别为 13.41、49.76、37.56 秒。日志为 `target/second-load-open-existing.log` 和 `target/second-load-final-twenty.log`。全部原有期限和 safety、finality、资源所有权、持久化及隐私断言保留；这些并发结果覆盖本机有限负载，随后完成的 Windows / WSL 验收见上节。

最终本地五项门禁全部通过：`cargo fmt --all -- --check`、`cargo check --all-targets`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all-targets`、`git diff --check`。普通测试命令无 `RUST_TEST_THREADS` 环境覆盖，本机 20 个逻辑处理器；lib 94 项通过（13.40 秒）、integration 247 项通过（62.40 秒）、34 节点回归 1 项通过（68.07 秒），共 342 项。日志为 `target/second-load-final-default.log`；两个 ignore 仍为被父测试实际调用的跨进程锁辅助用例和需显式运行的 WSL 镜像网络验收，后者已在上节单独通过。

### 2026-10-08：运行时收敛修复与本地验证

收集正文完整验证并原子导入后，现有私有同步器在内存中记住有界的 `(scope, digest)`，仅用于抑制同一贡献元数据引起的重复正文拉取。该缓存不注册候选、不封根、不授予资源或签名权，版本/分配前沿过期时清理；冷重启仍从持久状态重新验证正文，未修改 wire/snapshot 格式。

仅用于读取和验证的交接准备与源元数据处理复用现有不可变共享快照，避免每次公告复制整份业务、准备任务及证明。

连接重放和既有定时源公告按交接意图只传播当前义务最多的正文，相同义务数优先封存候选并以 digest 确定顺序。更小的旧封存根仍保留在原 pending 记录中，由提案、QC 或最终性证明触发精确正文拉取；只减少自动贡献公告，既不覆盖根，也不替换已有锁定选择。

最终性重传期限同时重试中断的本地最终性签名和证书落盘/业务执行，失败先安排下一期限，避免过期期限空转。签名、quorum、精确目标验证和原子提交规则保持原实现。针对性回归验证没有新消息时仍可完成已收齐证书的业务，以及已导入子集的重复公告不会再次拉取。

手动驱动的真实 QUIC 冷交接回归原先把同步磁盘/签名工作直接放在单线程 async 执行器内，使该测试自己的接收器不能同时推进；现与生产 runner 一样由 `spawn_blocking` 执行持久工作。原 10 秒期限、四节点完整并集、本地/远端资源权限、离线成员不变和冷读断言全部保留。

最终性证明验证并持久化后，运行时直接复用既有依赖闭包原子提交，避免再次加载业务、验证同一证书和重复 finalize。提交结果中的全部完成任务与资源竞争重试交还 runner；runner 从同一持久 receipt 恢复完成状态、清理对应会话和同步。直接安装历史最终性证明的恢复路径也在业务提交后恢复等待依赖的持久请求，并刷新恢复候选。

最终性投票达到 quorum 或收到通过验证的最终性证书时，也先安排恢复期限；即使本地尚未走完 BFT 阶段、随后落盘失败，仍能在没有新消息时重试。旧委员会权限的运行中轮换回归改用账户注册，避免夹具逐个安装发行分配时引入与测试目标无关的分配证据竞态；发行与前沿竞争继续由现有分配集成测试验证。

本阶段曾把本地测试默认并发设为 4，以固定测试执行资源预算；这不是高并发性能修复。当时 20 个跨用例线程的全量运行仍出现不同用例的实时期限失败。后续定位并修复锁文件打开热点，已移除该限制，结果见上面的 20 路并发记录。多节点用例内部并发、所有原有期限和安全断言均保留。

本阶段本地五项门禁全部通过，使用当时的 4 路默认并发：lib 94 项通过（24.63 秒），integration 247 项通过（84.91 秒），34 节点真实 QUIC 恢复与业务最终性回归通过（82.50 秒）。两个 ignore 分别为被父测试单独调用的跨进程锁辅助用例和需要额外环境的 WSL 镜像网络验收；WSL/独立主机验收未执行。此记录证明本地场景，不代表无限负载或跨主机性能验收。

> 本文档描述 Second 当前确认的系统设计，并把“协议必须满足的语义”与“当前 Rust 仓库的实现选择”分开记录。
>
> **协议语义优先于实现细节。** 当前仓库尚未正式发布，因此实现细节如果与已确认设计冲突，应直接修改到正确设计，不为旧实验实现保留兼容层。

---

## 1. 系统定位

Second 是一个**纯内部货币系统**。

它不负责现实货币、银行卡、法币清算或外部支付网络，也不负责判断退款、奖励、发行、销毁、泄漏修复等业务行为“应该不应该发生”。

整体职责边界：

~~~text
外部业务系统
    ↓
决定业务是否合法、为什么发生、允许发生什么
    ↓
Signed LegalTask
    ↓
Second
    ↓
验证授权
判断当前是否可执行
执行确定的 Currency 状态变化
保证原子性、幂等性、并发安全和身份不复用
~~~

Second 只回答两个不同的问题：

- **Legal**：这份任务是否获得有效授权。
- **Executable**：在当前系统状态下，这份合法任务是否能够成功执行。

因此：

~~~text
Legal = true
Executable = false
~~~

是正常状态，不应把业务授权失败与当前资产状态不足混为一谈。

### 1.1 网络定位与外部服务边界

Second 提供独立的货币状态与 LegalTask 执行网络。Secoin 是当前讨论中对网络货币的称呼，不因此新增另一套资产类型、计价真值或法币兑换承诺。外部参与者可以围绕网络提供钱包、支付、交易、授权与其他业务服务；货币的市场价格和商业用途由外部参与者形成，不由协议保证。

服务商自行决定业务接受条件，包括选择 Authorizer、是否要求保证金、担保资产、担保比例、责任期限及商业争议处理方式。以 Secoin 对任务转移金额提供 100% 担保可以是一种服务策略，但不是 Second 的统一协议门槛。其他服务不必采用同一策略。

Second 不因任务通过协议验证就证明链外商业承诺真实或已经履行，也不默认承担链外商业仲裁、保险或兑付职责。服务商、钱包、交易设施及其组织、信用和担保策略属于外部设施，不作为 Second 内部协议必须实现的业务模型或开发验收前置条件。当前协议不提供商业保证金锁定或自动赔付承诺；只用于 LeakRepair 的 reserve 不代表商业保证金。

### 1.2 当前协议输入的授权边界

当前 `LegalTask::verify` 通过 Validator 本地配置的 `AuthorizerSet` 判断签发者是否可信。该集合定义当前节点接受 LegalTask 的签名信任边界，不是服务商注册表或商业策略模型。生成签发密钥、运行节点或持有保证金都不会自动取得当前网络的写权限。

外部设施如何选择、组织和约束签发者，由外部授权系统决定并通过现有 Signed LegalTask 边界接入。不能从外部设施的业务需求直接推导出 Second 必须新增服务商分域、账户委托或保证金制度，也不能把这些尚未提出的协议能力列为当前开发缺口。

外部 Authorizer 服务的开放性不自动改变当前签名信任检查、Validator 成员准入或 BFT 安全边界。这些内部约束与外部设施的组织规则应分别描述；本次定位讨论不修改现有运行规则，也不推翻已确认内部功能范围的开发验收。

---

## 2. 资产模型：Currency ownership 是唯一资产真值

Second 不维护独立的“余额账本”作为资产真值。

每一枚当前存在的 Currency 都是独立资产对象，并且只存在以下所有权状态：

~~~text
Currency -> Account
~~~

或者：

~~~text
Currency -> null
~~~

账户余额只是所有权关系的派生计数：

~~~text
Balance(Account)
=
当前所有 Owner = Account 的 Currency 数量
~~~

因此：

- Currency ownership 是唯一资产真值；
- balance 不能成为第二套资产账本；
- 缓存、统计、公开摘要都只能是派生数据；
- 任何执行、恢复、持久化逻辑最终都必须还原到 Currency 对象及其 owner。

---

## 3. 核心身份

### 3.1 CurrencyAddress

每枚 Currency 有永久唯一的 CurrencyAddress。

核心要求：

- 一经分配永久消费；
- Currency 被 Destroy 后，其地址也不能复用；
- 批量创建时可以分配连续 identity range；
- 即使后续业务执行失败，已经正式分配的 identity range 也不能退回。

当前 Rust 实现使用：

~~~text
CurrencyAddress = u64 sequence
display = "0lxii" + canonical base62(sequence)
~~~

显示编码只是外部表示，真正协议不变量是**永久唯一且不可复用**。

### 3.2 AccountAddress

AccountAddress 永久唯一地标识一个 Account。

~~~text
AccountAddress -> Account
~~~

一旦绑定，不允许改绑或复用。

账户可由 Genesis 初始化，也可在运行中通过 `RegisterAccount { account }` 创建。运行时开户复用现有外部 `AuthorizerSet` 与 `LegalTask -> prepare -> finality -> commit`，prepare 不提前公开账户；业务状态的既有 accounts 集合是唯一权威。已存在地址拒绝再次开户，没有删除、改绑或复用入口。这里不新增用户公钥绑定、钱包私钥或账户恢复模型。

开户与支付地址生命周期共用 `LifecycleClaimBook`：不同 active PreparedTask 竞争同一账户地址时拒绝后来的本地 prepare；重启从 durable prepared operations 重建 claim，取消或提交后释放。同一任务按原始顺序执行，可先开户，再绑定支付地址、发行和转移；后续操作失败时整笔任务回滚。该 claim 是现有局部准备约束，不保证不同冲突请求在任意网络到达顺序下的公平性或活性。

四节点相反准备顺序曾复现两个任务各占两个节点、无法取得 3/4 quorum 的阻塞。局部冲突仲裁与安全释放现已接入原任务 scope 的 Commit/Abort 共识；真实冲突、冻结变体、QC 优先及认证交接的验证记录见 [conflict-arbitration.md](docs/conflict-arbitration.md)。投票后禁止本地取消的约束继续生效，资源释放必须由原委员会的有效 Abort 证书驱动。私有 PreparedTask source 现在传递原始签名 LegalTask 及 Transfer/LeakRepair 无法从请求重建的冻结选择，接收方复用权威 prepare 路径验证数量、唯一性、归属、业务和最终 plan digest。Issue 地址由已认证分配区间重建，不复制可重建数据。总 source 预算保持 2 MiB 加 4 字节长度 framing，开发 wire version 保持 1；不支持旧内部 source 格式。非法冻结 source 不能留下绑定、payment execution 或资源占用。

拉取冻结计划遇到资源占用时，另在临时状态和空 claim book 中复用同一准备路径验证整个计划，然后查询现有资源占用索引，生成其他 TaskId 的精确交集诊断。验证不安装远端计划、不赋予签票权限，也不释放原占用；同账户而冻结货币不相交、同一任务自身引用不作为冲突。正常准备成功路径不增加第二次执行；这一诊断仍不等同于取消最终证书。

当前 Rust 实现使用 32-byte opaque address，规范字符串前缀：

~~~text
acct_<canonical Base64URL-no-pad>
~~~

### 3.3 PaymentAddress

PaymentAddress 是支付入口，不是资产 owner。

~~~text
PaymentAddress -> Account
Currency -> Account
~~~

禁止形成：

~~~text
Currency -> PaymentAddress
~~~

PaymentAddress 与 Account 的绑定永久不变。

当前 Rust 实现使用 32-byte opaque address，规范字符串前缀：

~~~text
pay_<canonical Base64URL-no-pad>
~~~

### 3.4 TaskId

TaskId 是 LegalTask 的永久任务身份。

当前实现使用受限 ASCII 字符串：

- 长度 1–128；
- 允许字母、数字、下划线和连字符；
- 不允许同一个 TaskId 在第一次绑定后换成另一份已验证 signed request。

---

## 4. PaymentAddress 生命周期

PaymentAddress 有三个单向状态：

~~~text
active
  ↓
retiring
  ↓
retired
~~~

### active

可以建立新的 Transfer。

### retiring

不能建立新的 Transfer，但**已经建立的在途 Transfer 必须仍能继续完成**。

因此 retiring 的语义是：

~~~text
stop new payments
!=
invalidate established payments
~~~

### retired

完全退出支付。

只要仍存在引用该 PaymentAddress 的已建立 Transfer，就不能从 retiring 进入 retired。只有这些在途支付完成后，才能最终 retired。

PaymentAddress 本身没有独立余额，也不维护地址级 usage quota、max usage 或地址级过期逻辑。

PaymentAddress binding / lifecycle mutation 已纳入与资产业务相同的 LegalTask authority：`RegisterPaymentAddress`、`RetirePaymentAddress`、`FinalizePaymentAddressRetirement` 都必须走 `LegalTask -> prepare -> finality -> commit`。Second 不再暴露可绕过 finality 的公开 PaymentAddress state mutator；谁有资格签发这些 LegalTask 仍由外部 authorization 决定，不另设管理员写入口。

同一 active PreparedTask 可以按原始 operation 顺序连续操作同一个 PaymentAddress；不同 active PreparedTask 不能同时冻结同一 PaymentAddress 的 lifecycle mutation。这个 lifecycle claim 必须随 PreparedTask 持久化语义恢复，cancel/commit 后释放。Transfer establishment 不占 lifecycle claim：已经建立的 Transfer 仍允许跨 `retiring` 完成。

---

## 5. Currency 角色

Currency 当前有两种角色：

~~~text
circulation
reserve
~~~

### circulation

正常流通资产，可以：

- 被 Account 持有；
- 被 Transfer 改变 owner；
- 在满足条件时被 Destroy；
- 作为 leaked Currency 参与 Leak Repair。

### reserve

内部准备资产：

~~~text
role = reserve
owner = null
~~~

reserve 只服务 Leak Repair，普通 Transfer/Destroy 不把 reserve 当正常流通资产使用。

---

## 6. 四种基础 Operation

Second 当前只有四种基础货币状态变化：

~~~text
Transfer
Issue
Destroy
Leak Repair
~~~

所有业务最终都被压缩为这四种 Currency 状态转换的有序组合。

### 6.1 Transfer

输入：

~~~text
source PaymentAddress
destination PaymentAddress
amount = N
~~~

PaymentAddress 先解析到 Account：

~~~text
source PaymentAddress -> source Account
destination PaymentAddress -> destination Account
~~~

真正发生的资产变化：

~~~text
Currency owner:
source Account -> destination Account
~~~

Currency 不经过 owner = null 中间态。

Transfer(amount = N) 动态选择当前属于 source Account 的 N 枚 circulation Currency。

关键原则：

~~~text
Select != Claim
~~~

发现候选 Currency 不等于已经取得它们的执行权；真正执行前必须再次确认它们仍属于 source。

Transfer 不改变 CurrentSupply：

~~~text
ΔCurrentSupply = 0
~~~

### 6.2 Issue

Issue(count = N) 创建 N 个新的 Currency identity，并直接归属于目标 Account：

~~~text
∅ -> circulation Currency -> Account
~~~

不是修改一个余额数字，而是真正创建 N 个新资产对象。

需要创建新 Currency identity 的 LegalTask 不允许各 Validator 在本地 prepare 时自行推进共享 frontier。Validator 先针对当前 exact `next_currency_address` 建立 `CurrencyAllocation` scope，由当前 active ValidatorSet 对 `validator_set_version + start + count + request_digest` 形成 quorum certificate；证书 durable 安装后该 exact range 才正式分配并推进 frontier。正式分配后的 identity 永久消耗，即使后续 prepare 被取消或业务最终失败也不能复用；同一 task 的重试/重启必须继续使用同一 certified range。安装 allocation 之前必须先确认该 range 可以被本地实现物化，资源分配失败不能先烧掉 identity。这个保护不定义 `Issue` 的协议级数量上限。

~~~text
ΔCurrentSupply = +N
~~~

### 6.3 Destroy

Destroy 只允许：

~~~text
role = circulation
owner = null
~~~

禁止：

- 直接销毁用户当前持有的 Currency；
- 销毁 reserve；
- 复用被销毁 Currency 的地址。

~~~text
ΔCurrentSupply = -N
~~~

### 6.4 Leak Repair

Leak Repair 的目标是替换有问题的 Currency identity，而不改变用户价值。

对每个 leaked circulation Currency：

1. 记录原 owner；
2. 取一枚当前未占用的 reserve Currency；
3. reserve Currency 变为 circulation，并接替 leaked Currency 的 owner；
4. leaked Currency 永久销毁；
5. 分配一个全新的 Currency identity 作为新的 reserve。

最终必须同时保持：

~~~text
ΔBalance(account) = 0
ΔCurrentSupply = 0
ΔReserveCount = 0
~~~

Leak Repair 本质是：

> 更换资产身份，而不是改变资产价值。

---

## 7. LegalTask

Second 的业务执行单位不是单条 Operation，而是：

~~~text
LegalTask {
    task_id,
    protocol_version,
    expires_at,
    operations[]
}
~~~

### 7.1 Operation 顺序属于任务身份

~~~text
[A, B, C] != [B, A, C]
~~~

Second 必须严格按照授权时的原始顺序执行。

尤其不能提前执行或提前建立后续 Operation 的协议事实。

例如：前一个 Operation 已经失败时，后面的 Transfer 不能提前产生新的 in-flight payment establishment。

### 7.2 整体业务原子性

LegalTask 是完整业务原子单元。

如果：

~~~text
Operation 1 成功
Operation 2 成功
Operation 3 失败
~~~

则业务状态不能提交 Operation 1/2 的部分结果。

最终只能：

~~~text
全部业务变化成功提交
或
全部业务变化不提交
~~~

但有些**协议事实不是业务状态**，必须在失败后保留，例如：

- 第一次已验证 signed request 对 TaskId 的永久绑定；
- 已正式分配的 Currency identity；
- 已经真正到达并建立的 Transfer establishment。

这类事实不能为了业务回滚而被撤销。

### 7.3 TaskId 永久绑定

第一次收到一份通过授权验证的 signed request 后：

~~~text
task_id -> first verified signed request
~~~

永久成立。

即使业务执行失败，也不能再用同一个 TaskId 绑定另一份不同 signed request。

当前实现用 request_digest 作为内部 commitment；它由 authorizer public key、signature 和 canonical signing bytes 共同计算，用来判断 exact replay 与 TaskId conflict。

request_digest 只是内部表示，**不是第二个公开任务身份**。

### 7.4 成功幂等

如果任务已经成功：

~~~text
Success(task_X)
~~~

之后 exact replay：

- 不重新执行 Operation；
- 返回 already succeeded；
- 不重复 Issue / Transfer / Destroy / Leak Repair / PaymentAddress lifecycle mutation。

---

## 8. 授权与签名

当前实现使用 Ed25519。

授权流程：

~~~text
LegalTask
  ↓
payload structural validation
  ↓
protocol version check
  ↓
authorizer trust check
  ↓
Ed25519 strict verification
  ↓
VerifiedLegalTask
~~~

执行器只接受 VerifiedLegalTask，解析 JSON 或构造 LegalTask 本身不能绕过验签进入核心执行路径。

### 8.1 当前 canonical signing format

当前协议版本：

~~~text
CURRENT_PROTOCOL_VERSION = 1
~~~

签名 domain：

~~~text
Second/LegalTask/v1\0
~~~

当前实现使用确定性 typed CBOR-style encoding，包含：

- TaskId；
- protocol version；
- optional expires_at；
- operations[]；
- operation type discriminant；
- canonical AccountAddress / PaymentAddress / CurrencyAddress 文本。

当前 Operation discriminant：

~~~text
0 = Transfer
1 = Issue
2 = Destroy
3 = LeakRepair
4 = RegisterPaymentAddress
5 = RetirePaymentAddress
6 = FinalizePaymentAddressRetirement
7 = RegisterAccount
~~~

### 8.2 输入结构合法性

进入 VerifiedLegalTask 前必须拒绝：

- Transfer amount = 0；
- Issue count = 0；
- Destroy 空列表；
- Leak Repair 空列表；
- Destroy / Leak Repair 内重复 CurrencyAddress。

结构非法的 payload 不能建立任务身份或进入执行层。

---

## 9. expires_at 语义

expires_at 阻止的是：

> **新的执行开始。**

它不是“到了时间就杀掉所有已开始工作”的超时机制。

因此：

- 新 task 在 expiry 后不能开始 execute；
- 新 task 在 expiry 后不能 prepare；
- 已经成功的 task 在 expiry 后 exact replay 仍然返回 success；
- 已经在 expiry 前建立的 Transfer 不能因为时钟越过 expires_at 被自动销毁；
- 已经在 expiry 前完成 prepare 的 task 可以在之后完成 validator vote / finality commit。

禁止用 task expiry 充当 in-flight payment cancellation 机制。

如果未来需要取消已建立支付，必须有明确、独立的协议语义。

---

## 10. Transfer establishment

为了支持 PaymentAddress retiring，系统需要区分：

~~~text
new Transfer
established Transfer
~~~

当前内部以：

~~~text
(task_id, operation_index)
~~~

唯一标识一条 Transfer establishment。

establishment 记录固定：

- source PaymentAddress；
- destination PaymentAddress；
- amount；
- task expires_at。

重试同一个 operation 时必须完全匹配原 establishment，否则拒绝。

规则：

1. 只有真正按 LegalTask 顺序走到该 Transfer 时才能建立；
2. active PaymentAddress 才能建立新的 Transfer；
3. establishment 一旦存在，即使地址后来变为 retiring，也允许该 Transfer 完成；
4. Transfer 成功后删除对应 establishment；
5. 前面 Operation 已失败、尚未走到的 Transfer 不能提前留下 establishment；
6. 不因为 task expires_at 自动删除 establishment。

---

## 11. 并发模型

Second 的并发单位是实际 Currency，而不是整个 Account。

两个任务如果最终触碰的 Currency 集合不相交：

~~~text
Touch(T1) ∩ Touch(T2) = ∅
~~~

领域模型允许并发。

### 11.1 Currency Claim

CurrencyClaimBook 管理临时执行权。

Claim 必须区分：

- 真正余额不足；
- 余额理论上足够，但可用 Currency 正被别的任务 Claim。

对应语义不能混淆：

~~~text
InsufficientBalance
!=
CurrencyContention
~~~

### 11.2 Reserve Claim

Leak Repair 同样必须区分：

~~~text
ReserveUnavailable
!=
ReserveContention
~~~

### 11.3 锁顺序

同一 operation 涉及多枚 Currency 时使用稳定顺序获取执行权，避免死锁和不同执行者产生不确定锁顺序。

---

## 12. Prepare / Finality

当前仓库支持将 LegalTask 转为确定的 PreparedTask，再由 Validator 对其 plan digest 投票。

Prepare 的作用：

- 按 task 原顺序构造确定执行计划；
- 固定动态选出的 Currency 集合；
- 持有对应 Currency claims；
- 持久化必须保留的 protocol/prerequisite facts；
- 不提前提交最终业务 owner 变化。

### 12.1 Finality 是业务资产提交的唯一入口

Genesis 初始化完成后，任何 LegalTask 引起的 Currency / BusinessState 变化都必须经过：

~~~text
VerifiedLegalTask
  ↓
[仅当需要新 Currency identity]
CurrencyAllocation(scope = current validator-set version + current frontier start)
  ↓ per-scope BFT + FinalityCertificate
durably reserve exact identity range / advance frontier
  ↓
prepare exact plan from durable state + certified range
  ↓
PreparedTask per-scope BFT (prevote / precommit)
  ↓ valid precommit QC for exact plan digest
irreversible Validator finality vote
  ↓
FinalityCertificate
  ↓
commit
~~~

禁止存在可由正式节点调用的 direct executor 旁路。

`prepare` 可以建立和持久化协议必须保留的事实（例如 TaskId binding、Transfer establishment 和 claims），但不能直接提交最终业务资产变化。Issue / LeakRepair 需要的新 Currency identity reservation 在 prepare 之前由独立的 `CurrencyAllocation` scoped consensus durable 认证；prepare 只能消费该 task 已认证的 range，不能自行推进共享 frontier。只有验证通过的 FinalityCertificate 才能进入 `PreparedTaskBook::commit()` 并提交 BusinessState。PreparedTask 的 `Finalized` durable 状态必须与该 exact plan 的 quorum finality votes 在同一 snapshot 中原子持久化；snapshot reload 会按 task 绑定的 exact active/retained ValidatorSet 重新验证该证书，不允许只有 phase 标记而没有证明的“假 Finalized”。证书验证通过后，如果本地 plan apply 因状态不一致等原因失败，该错误不等价于 cancellation：不得隐式删除 prepared plan 或释放其 claims，必须保留可恢复的 finalized commit 上下文。

Genesis 本身的初始账户、地址起点和 Reserve 初始化不属于 LegalTask 执行，不要求经过 Finality。

Genesis 现在有正式 provisioning 路径，而不是要求部署者用测试代码直接构造 snapshot。`second validator-keygen <validator-id> <keyring-file>` 只生成该 Validator 的 identity / consensus / recovery 三把相互独立的 Ed25519 private key，并把它们写成与 runtime 完全相同的 strict keyring schema；命令输出对应 public `ValidatorCredential` 供部署者审阅。**key generation 本身不产生 Validator authority。** 某个 Validator 只有被显式列入随后使用的 Genesis 配置并进入 Genesis `ValidatorSet`，才获得该网络的验证权；因此“持有 key”和“被网络授权”仍是两件不同的事。

`second init-network <config-json> <output-dir>` 是从空目录创建一套可直接启动的 Genesis deployment 的正式入口。配置采用 strict JSON、拒绝未知字段，当前字段为：

~~~json
{
  "validator_set_version": 1,
  "first_currency_address": 1,
  "reserve_count": 0,
  "accounts": ["acct_..."],
  "authorizer_public_keys_base64": ["..."],
  "bft_timeouts_ms": {
    "proposal": 1000,
    "prevote": 1000,
    "precommit": 1000
  },
  "validators": [
    {
      "validator_id": 1,
      "listen_address": "203.0.113.10:4433",
      "keyring_file": "validator-1.keys.json"
    }
  ]
}
~~~

`keyring_file` 的相对路径相对于 init config 所在目录解析。初始化在发布任何最终 output 之前完成全部 preflight：Authorizer key 必须是有效且可组成非空 `AuthorizerSet` 的 Ed25519 public key；账户地址必须 canonical 且不重复；BFT timeout 必须全部大于 0；ValidatorId 和 listen address 不得重复；listen address 必须是可拨的非 wildcard、非零端口地址；keyring 内 ValidatorId 必须与配置一致；Genesis keyring 必须恰好只有一把 consensus key，因为 Genesis 尚不存在 historical consensus-key history；所有 Validator credential 最后统一通过 `ValidatorSet::new` 校验跨 Validator 的 key reuse / duplicate identity。

通过 preflight 后，init 先在与最终目录相邻的 `<output-dir>.new` staging directory 构建整套部署。每个 Validator 得到 `<output-dir>/validator-<id>/second` snapshot base，并生成与其配套的 `.validator.keys.json`、`.validator.json`、独立 `.transport` 和 `.bootstrap.json`。每个节点 snapshot 使用完全相同的 Genesis `SecondState + ValidatorSet`；Validator keyring 继续保持私有 sidecar，transport identity 则独立生成，不与 Validator identity / consensus / recovery key 复用。所有 transport identity 先生成，随后才能根据真实 NodeId + certificate pin + 配置的 listen address 生成 bootstrap records。当前 init-network 最多 provision 56 个 Genesis Validator，每个节点完整列出其他成员的静态 bootstrap。公开 GetPeers/Peers 响应仍最多 32 条；本地 bootstrap 与 PeerStore 候选上限独立为 128。56 是当前 128 条连接预算下的部署边界：55 条 outbound 加 55 条 inbound BFT，剩余 18 条供公开连接与提交服务使用，不是 ValidatorSet 协议数量上限。保留旧集合期间仍受同一连接预算约束，不能把 active 集合上限当作任意 retained-set 拓扑的容量保证。

只有 staging 中所有 snapshot、sidecar、transport identity 和 bootstrap file 都成功生成后，目录才通过同 parent rename 发布为最终 output；最终目录已存在时 `init-network` 直接拒绝，绝不覆盖现有 deployment；遗留 `.new` staging 也要求 operator 明确清理后才能重试，避免把不完整初始化误认为已发布网络。Genesis 的 AuthorizerSet/BFT timeout 属于各 Validator 的本地 runtime config，而不是 Second 共享资产状态；Genesis Validator authority 的共享根仍只有 snapshot 中的 Genesis ValidatorSet/Registry。

PreparedTask 的生命周期固定为：

~~~text
Prepared ──cancel──> Cancelled

Prepared
  ↓ first vote path opened
Voting
  ↓ valid FinalityCertificate verified
Finalized
  ↓ successful commit
Committed
~~~

只有 `Prepared` phase 允许 cancel。第一次为 PreparedTask 本地签 BFT prevote/precommit，或本地接受该 scope 的有效 precommit QC 时，必须在同一 durable snapshot 中把 `Prepared → Voting` 打开；该转换不可逆，因此真正的 BFT 投票一开始就不能再 cancel，而不是等到最后不可逆 `FinalityVote` 才关闭取消窗口。`sign_prepared_vote()` 面对已经处于 `Voting` 的 task 只继续既有 finality path。`commit()` 验证到有效 FinalityCertificate 后，统一通过 `StateStore::finalize_prepared_task()` 把 `Finalized + quorum votes` 原子持久化，再尝试 apply，因此即使本地 apply 暂时失败，任务也不能再 cancel。正常 Validator runtime 在本地形成或接收第一张有效 PreparedTask FinalityCertificate 后会自动执行该 commit，不再要求外部调用方消费事件后手工 apply；`CertifiedPreparedTask` 事件只在 Commit 业务提交或 Abort 取消终态已经 durable 后暴露，接入方须结合证书摘要或 exact 请求状态区分结果。若进程恰好崩溃在 finality 已 durable、业务 commit 尚未完成的窗口，重启会先验证 durable certificate 并补完 commit，再启动仍未 finalized 的 PreparedTask consensus。`Voting` / `Finalized` 都必须跨重启恢复。

phase 是本地 lifecycle / recovery 元数据，不属于 prepared plan 本身，也不得进入 plan digest；否则 `Prepared → Voting` 会改变 Validator 已经要签名的 finality subject。

### 12.2 Prepared plan

Prepared plan digest 绑定：

- TaskId；
- first verified request commitment；
- validator-set version；
- 按原始顺序展开的确定 Operation plan；
- Issue/Leak Repair 已正式分配的 Currency identity；
- Transfer 已确定的 owner/account/currency selection。

重启后必须恢复**同一份 prepared plan**，不能重新选币或重新分配 identity 再制造一份新计划。

### 12.3 Finality subject

Validator 对通用 FinalityStatement 投票：

~~~text
protocol_version
validator_set_version
subject_digest
~~~

PreparedTask 使用其 plan digest 作为 subject digest。`CurrencyAllocation` 使用独立 allocation digest，绑定 exact active ValidatorSet version、当前 frontier start、需要的 identity count 与原 signed LegalTask request digest。

### 12.4 Per-scope BFT coordination

不可逆 `ValidatorVote` 之前增加独立的 per-scope Byzantine agreement 层。Second 不建立 block、高度链或全局 transaction total order；`ConsensusScope` 直接复用最终 vote-lock 的权威 scope：CurrencyAllocation、PreparedTask、PublicCheckpoint、StateRecoveryCheckpoint 各自是独立 consensus instance；ValidatorSetTransition 与对应 frontier 的 CurrencyAllocation 共用 instance，作为 epoch seal。`CurrencyAllocation` 只序列化真正共享的 Currency identity frontier：scope 固定为 `(validator_set_version, start)`，同一个尚未消费的 `start` 可以同时出现多个候选 task digest，但 quorum/lock 规则最终只能认证一个候选；该 allocation durable 安装后 frontier 前进，其他尚待分配的 task 在新的 start 上继续竞争。没有新 identity 需求的 LegalTask 完全跳过这一步，因此它不是全局交易排序或全网串行执行。

BFT vote 使用独立 `SECOND_BFT_V1` signing domain，绑定 `protocol_version + validator_set_version + ConsensusScope + round + phase + value`。phase 当前固定为 `Prevote` / `Precommit`，value 为具体 subject digest 或 `Nil`。BFT vote/QC 与最终 `FinalityStatement` / `ValidatorVote` 是不同签名语义，不能互换。

每个 Validator、每个 scope 的 `BftLocalState` 持久保存当前 round、本 round 已投 prevote/precommit、locked round/digest、最高有效 digest prevote QC，以及已经观察到的 precommit QC 对应 finality-ready digest。prevote QC 与依赖它的签票在同一次原子写入中保存；已投 precommit 或已进入更高轮次后收到的有效旧 QC 仅在提高已知 proof 时写入。重启注册候选时恢复该 QC，后续 proposer 按既有锁规则选择候选并附带合法 unlock certificate；重复同一 proof 不重写。source、QC 证明和仍有效占用不能通过本地超时释放。该状态属于本地 signing safety metadata，不进入 shared recovery digest；shared-state recovery 不恢复它，并继续受 `validator_safety_ready` / signing fence 约束。PreparedTask scope 的 BFT state 必须绑定该 task 的 exact active/retained ValidatorSet；PublicCheckpoint、ValidatorSetTransition、StateRecoveryCheckpoint 的 BFT state 只允许绑定当前 active ValidatorSet。membership 激活后旧的非-Prepared BFT state 会被丢弃，PreparedTask 完成/取消后其 BFT state 也会被清理。

锁规则当前冻结为：对 digest 的 precommit 必须附带同 scope、同 round、同 digest 的有效 prevote QC，并在本地形成 durable lock；已锁 A 后，后续 round 不能直接 prevote B，只有附带一个 `locked_round < proof_round < current_round`、且对 B 达到 quorum 的 prevote QC 才允许迁移。`Nil` 不建立 value lock。每个 round 的 prevote/precommit 各只能签一次，同 phase 同 round 冲突值永久拒绝；round 只能严格 `+1` 前进。

只有节点已经验证并持久接受 exact scope/digest 的有效 **precommit QC**，现有永久 `FinalityVote` 入口才被打开；成功写入不可逆 vote-lock 后，对应 transient BFT local state 被移除。已经存在的同 digest finality vote-lock 仍允许确定性 replay，不要求重新跑 BFT。

这一层现在直接在既有 safety core 上补齐 proposer、proposal validation、Validator-only BFT transport 与 timeout/view-change driver，而没有引入第二套 consensus。proposer 对 exact `ValidatorSet` 按 `ValidatorId` 升序排列，并以 `round % N` 确定；`BftProposal` 使用独立 `SECOND_BFT_PROPOSAL_V1` signing domain，绑定 `protocol_version + validator_set_version + ConsensusScope + round + proposer_id + subject_digest`。网络收到的 digest 不能直接进入 signer：节点必须先从本地真实对象和状态构造 `BftProposalSubject`，PreparedTask 校验 durable plan 与其 exact active/retained ValidatorSet，PublicCheckpoint 校验本地 public summary/floor，ValidatorSetTransition 校验当前 registry transition，StateRecoveryCheckpoint 继续复用既有 recovery serial/floor 与 shared-state 匹配规则。`BftDriver` 只在本轮已经本地验证过 exact subject 后，才允许 digest prevote QC 触发 precommit 签名或 digest precommit QC 进入 finality-ready；仅凭网络 QC 中的 raw digest 不会打开签名入口。重启后 transient subject-validation context 不冒充 durable safety fact，需要重新从权威业务对象验证 proposal。

`BftDriver` 的 timeout 仍只是本地 liveness 触发器：proposal timeout 产生本轮 `Nil` prevote，prevote timeout 产生本轮 `Nil` precommit，precommit timeout 或有效同 round `Nil` precommit QC 将 durable round 严格推进并轮换 proposer；即使本地完全错过该 round、尚无 BFT state，有效 Nil precommit QC 也可原子创建该 scope 的本地 state 后进入下一 round。已注册 scope 收到高于本地 current round 的普通 Proposal/Vote 时，仍只进入每-scope 有界 transient future-message queue，不能凭无 QC 的网络消息跳轮；但**经过完整 exact-set quorum/signature 验证的 future QC 是共享共识证据**，允许节点把 durable BFT round 直接 catch up 到该 QC 的 round，再按原有 subject-validation 与 signer safety/fence/lock 规则处理该 QC，而不是被迫依赖本地 timeout 一轮轮追赶。低于 current round 的普通旧消息直接丢弃，但旧 round 的 digest-precommit Vote/QC 不能按 stale message 丢弃：它们仍可能聚合或直接证明既有 digest 已取得 quorum，因此继续交给现有 Driver/QC 验证并可触发 finality-ready。协议不签名 timeout 时间戳、不创建 timeout certificate，也不把本地时钟变成共享事实。Validator-only BFT transport 复用现有 QUIC/TLS transport，但在 dedicated BFT connection 上额外用本节点 active/retained authority 中该 Validator 跨版本保持稳定的 identity key 对 `CURRENT_NETWORK_PROTOCOL_VERSION + QUIC TLS exporter channel binding + role + ValidatorId + active_validator_set_version` 做一次会话认证；会话认证只证明该 peer 属于本节点当前可服务的某个 ValidatorSet，不把 retained membership 提升成当前 authority。认证后的连接承载 bounded BFT Proposal/Vote/QC、不可逆 `FinalityVote` 与 `FinalityCertificate` envelope，并对每条 envelope 再按其 `validator_set_version` 选择 exact set：非 PreparedTask 只能当前 active set，PreparedTask 才能使用 durable 绑定的 retained set，且两端 Validator 都必须属于该 exact set。Proposal 与 BFT Vote 必须由该连接认证出的原始 proposer/voter 发送；QC 与 FinalityCertificate 依靠自身 quorum signature 独立验证，FinalityVote 允许由同一 exact set 的其他已认证 Validator relay，但始终验证原始 `vote.validator_id` 的 consensus signature。认证后每个 BFT envelope 使用独立 QUIC 单向流，runtime 对发送与接收都做有界并发，并为 finality 消息保留优先容量；这些只是本地 liveness/resource 策略，不进入签名或共享协议状态。BFT transport 只承载共识 envelope，不替代业务对象传播；接收方必须已经能从本地可信对象/状态独立得到相同 `BftProposalSubject`。这些能力没有引入全局区块、全局序号、stake 权重或新 authority。

本地配置的 proposal/prevote/precommit duration 是基础窗口。各 scope 的有效窗口乘以 `ceil(quorum_threshold / 3) × (floor(round / member_count) + 1)`：较大委员会留出必要 quorum 验证工作量，每完成一次 proposer 轮转再增长窗口，避免对连续 proposer 逐个放大等待。倍率在 u32 上限饱和，不另存一份退避计数。新阶段从处理完成的当前时间开始计时，避免验证/落盘耗时吃掉下一阶段窗口。duration 无法表示为 Instant 时缩减至可表示的远期值，不转成当前时刻导致空转。这个调度策略不改变 QC 门槛、锁和最终性，不签名时间戳，也不以超时释放资源。

Finality certificate 必须：

- 绑定 exact protocol version；
- 绑定 exact validator-set version；
- 验证所有 Validator signature；
- 拒绝未知 Validator；
- 拒绝重复 Validator vote；
- 达到 quorum 后才成立。

### 12.3 Quorum

当前 Validator 模型：

~~~text
1 effective ValidatorCredential = 1 vote
~~~

不按：

- Currency 数量；
- stake；
- 算力；
- 存储；
- 机器数量

加权。

quorum：

~~~text
Q = floor(2N/3) + 1
~~~

### 12.4 永远不能改票

同一 Validator 对同一个 prepared task 一旦签过某个 digest：

~~~text
ValidatorId + TaskId -> locked digest
~~~

永久锁定。

以后：

- 重启；
- 重新 prepare；
- validator-set version 变化；
- consensus key rotation

都不能让同一个 Validator 对同一个 TaskId 改签另一个 digest。

允许重复返回完全相同的票，不允许换票。

---

## 13. Validator 身份与生命周期

Second 的 Validator 是协议授权成员，不是 permissionless stake/PoW 成员。

### 13.1 ValidatorCredential

当前 credential 包含：

- ValidatorId；
- identity public key；
- consensus public key；
- recovery public key。

### 13.2 ValidatorRegistry

Registry 保存 Validator 的永久历史。

状态：

~~~text
Active
Retired
~~~

核心约束：

- retired ValidatorId 永远不能重新作为新成员使用；
- identity key 永久不能被另一个 Validator 复用；
- recovery key 永久不能被另一个 Validator 复用；
- 历史 consensus key 永久进入 used-key history；
- consensus key rotation 不能旋转回任何已经用过的 key；
- current ValidatorSet 必须和 Registry 的 active records 精确一致。

### 13.3 ValidatorSet transition

ValidatorSet 有显式 version。

下一版本必须按版本顺序推进，不能跳过或倒退。

Validator transition / admission / rotation 仍然必须经过既有验证逻辑，不能绕过 Registry 的永久历史约束。若某 Validator 因 local safety state 丢失而进入 fail-closed recovery，它不能给用于建立自己新 signing domain 的那次 ValidatorSetTransition 投票；该 transition 必须由当前 set 中其余 Validator 独立达到正常 quorum。恢复流程不引入 emergency quorum、RecoverySet 或超级密钥。

Validator admission 的治理来源固定为**当前 active ValidatorSet 的 finality**：候选 Validator 的 `ValidatorAdmissionRequest` 只证明候选方同时控制其声明的 identity / consensus / recovery 三把 key，并不自行授予 Validator 权限。新 Validator 只有在其 credential 被纳入 `next_validator_set`，整个 `ValidatorSetTransition` 又由当前 ValidatorSet 达到正常 finality quorum 后，才获得协议授权。

ValidatorSet `version` 是当前唯一的 membership transition 序号。每次 transition 只能从当前 version 推进到严格相邻的 `current_version + 1`，不能跳过、倒退，也不存在另一套 membership 时间序号或基于本地时间的激活时钟。因此，Validator membership 的授权链是：当前 ValidatorSet → 对 next ValidatorSet transition 的 QC → 原子激活下一 version。Genesis 初始 ValidatorSet 是这条治理链的根；之后不引入持币量、算力、transport NodeId、单个管理员 key 或候选人的自签 admission 作为独立治理权。RecoverySet 的未来规则和现实世界“一人一 Validator”仍是独立未决问题，不改变当前协议内的 membership authorization source。

`ValidatorRegistry::apply_next_set` 只是 Registry 内部状态转换 primitive，不是公开授权入口；公共 membership mutation 必须经 `CertifiedValidatorSetTransition` 验证当前 ValidatorSet 的 QC。节点通过 `StateStore::activate_validator_set_transition` 原子持久化 next ValidatorSet、更新后的 Registry 以及仍被 active PreparedTask 引用的历史 ValidatorSet。consensus-key rotation request 只绑定当前 ValidatorSet version；由于 next version 被协议固定为 `current_version + 1`，不再额外绑定任何时间序号。

PreparedTask 永久绑定 prepare 时的 validator-set version。ValidatorSet 后续激活不要求清空旧 `Prepared` / `Voting` / `Finalized` task：节点只要仍有 active PreparedTask 引用某个旧 version，就必须在 snapshot 的 retained validator sets 中保留该 version 的完整 ValidatorSet，并用它完成该 task 的后续 vote / QC verify / commit。新 public checkpoint 和新的 ValidatorSet transition 始终只使用当前 active ValidatorSet。某个旧 validator-set version 不再被任何 active PreparedTask 引用时，下一次原子 snapshot 写入必须删除对应 retained set；历史 vote-lock 与 ValidatorRegistry 永久历史不因此删除。

---

## 14. 公开状态与隐私边界

Second 采用：

> **公开资产，私密所有权。**

每枚 Currency 可以公开展示：

- CurrencyAddress；
- 是否仍存在；
- occupied / unoccupied；
- circulation / reserve。

公开数据不能包含：

- owner AccountAddress；
- PaymentAddress -> Account 私有映射；
- 用户具体持有哪些 Currency；
- Currency claim/lock 状态；
- LegalTask 私有执行细节。

因此公开 Currency state：

~~~text
PublicCurrencyState {
    address,
    exists,
    occupied,
    role
}
~~~

而不是：

~~~text
PublicCurrencyState {
    ...,
    owner
}
~~~

SecondState 也不能因为 Debug/序列化方便而无意泄露 owner mapping。

---

## 15. Public state summary / checkpoint

当前实现可从公开 Currency state 计算确定的公共摘要，并可构造带 Validator finality proof 的 public checkpoint。

这些对象用于：

- 公开状态一致性校验；
- 网络同步；
- 防止把 owner 信息放进公开数据；
- 对某个公共状态摘要形成 Validator certificate。

重要边界：

> public checkpoint 不是第二套资产真值。

真正资产真值仍然只有 private Currency ownership。

当前 checkpoint/summary 是**同步与证明机制**，不是“账户余额账本”或“全局区块链状态”。

当前 public checkpoint 形态确定保留：`epoch + PublicCurrencySummary + validator-set finality proof`。其中 summary 已包含 public state frontier、supply/reserve/occupied 计数与确定性 `state_digest`；checkpoint digest 同时绑定 protocol version、epoch 和完整 summary，FinalityStatement 再绑定 validator-set version。网络收到的 `PublicCurrencyCheckpointProof` 只是未认证传输形态，只有用本地可信 ValidatorSet 验证 QC，并确认同步得到的 `PublicCurrencyView` 与 checkpoint summary 完全一致后，才得到 `CertifiedPublicCurrencyCheckpoint`。

Validator operator 可通过 `second public-checkpoint <address> <snapshot-base> <server-cert-base64>` 发起当前公开状态的 scoped consensus。请求复用 channel-bound governance identity authorization，使用独立 action；只需要当前 quorum 可连接。候选零票 source 复用已有 checkpoint proof 编码，仅在 exact-set BFT 内分发，各节点重新验证当前 summary 与 scope/version/epoch，并只接受本地合法下一 epoch/当前候选；零票 source 不能当作 QC，也不能任意跳到极大 epoch。已认证 proof 复用同一编码传播，必须重新验证 quorum，允许落后节点安装证明或推进 floor 后继续；旧的已认证 epoch 会话/缓冲被清除。形成合法 certificate 后 runtime 自动持久化 public proof、floor、baseline 和 bounded delta，不依赖诊断事件消费者。重复发布/安装相同认证 checkpoint 不改 epoch、不重写快照或丢弃已保存 delta。候选 epoch 从 durable floor/BFT/vote locks 推导；相同摘要的已签 epoch 可以恢复，不同摘要必须使用后续 epoch。重启只恢复已有 durable BFT/signing 记录且仍匹配当前状态的发布，其他登记需要 operator 重提。状态在 QC 形成前已改变时只推进认证 floor，不挂载旧 proof；后续显式发布认证新状态。Public backend 继续使用既有 15 秒同步 worker；不新增 Validator 发布扫描循环。

checkpoint epoch 是公开同步证明的单调 freshness 序号，不是全局 transaction/block height。节点持久化 `checkpoint_floor_epoch` 作为防回退下界；即使业务状态前进导致旧 checkpoint proof 不再匹配当前 state，floor 仍保留并跨重启恢复。当前机制只认证公开 Currency state，不加入 owner commitment，也不声称证明私有 ownership。

---

## 16. 网络设计

当前网络层以 QUIC 作为节点传输层，不依赖 JSON 作为节点间 framing。QUIC 连接完成 TLS 1.3 握手后，应用层先使用一个可靠双向 stream 完成 NodeId Hello；后续每个请求/响应使用独立的可靠双向 stream。当前 public session 按 connection 串行处理 request，因此 transport 也只允许每个 connection 同时存在 1 个双向 stream；完成一个请求后仍可继续打开下一条独立 stream。单向 stream 被禁用。当前协议不使用 QUIC DATAGRAM，也不使用 0-RTT。

QUIC 的 transport identity 与 Validator identity / consensus / recovery key 是不同职责，四者不得复用。transport identity 使用独立 Ed25519 key；`NodeId` 直接等于该 transport public key 的 32-byte 编码，因此 NodeId 不再是调用方可以任意声明的数字或标签。

NodeId 的认证权威是 Hello peer-auth Ed25519 key：Hello signature 的签名输入绑定当前 network protocol version、client/server role 与 Quinn TLS exporter 派生的 per-connection channel binding，因此该 NodeId 的 key ownership 被绑定到当前 TLS 会话；旧连接上的 Hello 不能在另一条 QUIC connection 上重放，client proof 也不能直接反射成 server proof。只有签名验证成功后，Hello 中的 NodeId 才能成为 `QuicPeer::remote_node_id()`。

长期 server 当前用同一 transport Ed25519 key 生成 self-signed TLS certificate 和 Hello proof，使 certificate pin 与 NodeId 都能随同一持久 identity 稳定重建；但协议不把这描述成 mTLS。`QuicClient` 不向 server 提交 TLS client certificate，server 对 client NodeId 的认证来自 channel-bound Hello proof。对 server，显式 certificate pin 认证 TLS endpoint，Hello proof 再认证该会话上的 NodeId；当前接收端不额外解析 certificate SPKI 去重复证明“certificate key == Hello key”。

Second 只有一个长期进程入口：`second node <listen-address> <snapshot-base>`，它表示“启动这个 Second 节点实例”，而不是某种与 Validator 互斥的普通节点角色。Node 是运行实体；Validator 是 Node 上按本地配置和 durable authority 启用的 capability；Full/Public 是 Node 的**状态 backend**，不是两种 daemon。`NodeRuntime` 只维护一套 listener、transport identity、PeerManager 与 public session：Full backend 使用 `StateStore`，持有完整 shared/private protocol state；Public backend 使用独立 `PublicStateStore`，只持有可信 `ValidatorSet + ValidatorRegistry + certified PublicCurrencyView + checkpoint proof`，绝不构造空壳/伪造 `SecondState` 来复用 Full 存储。Public backend 因此不能启用 Validator/BFT/governance/state-recovery/LegalTask submission，也不能回答任何依赖私有 TaskId binding / PreparedTask 的 LegalTask status 查询；若 public snapshot-base 上出现任一 Validator sidecar，启动必须 fail-closed。Full/Public snapshot 当前同样处于未发布内部格式阶段：格式 version 字段保持当前开发版本 `1`，内部字段布局变化直接修改当前格式，不因为每次 schema 变化累积 snapshot v2/v3/v4 历史，也不提供旧格式 reader。一次性 `ping` / `snapshot-status` / `sync-*` / `observe-*` / `public-init` 等 CLI 只是短命工具，不是另一种 daemon role；当前不存在 `second validator` 长期启动模式。长期节点的 transport private key 保存在独立的 `<snapshot-base>.transport` 本地 sidecar 中，不写入 Second state snapshot：首次不存在时创建，之后重启必须复用；已有 identity 文件损坏或无法解析时启动失败，不静默生成新身份。首次创建使用邻接 lock 文件串行化，并先把完整 identity 写入并 `sync_all` 到 `.transport.new`，再原子 rename 发布为 `.transport`；因此崩溃留下的 `.new` 不是 identity authority，下次启动可安全覆盖，只有最终 `.transport` 才是已发布身份。该文件包含私钥，Unix 创建权限为 `0600`。由同一 key 重建的 certificate 和 NodeId 在重启后保持稳定。显式地址的一次性 CLI（如 `ping` / `sync-public-certified`）仍由调用方 pin server certificate 并使用临时 transport identity；`observe-public-network` 则复用该 snapshot-base 对应的持久 transport identity、PeerStore 和静态 bootstrap records。

transport authentication 只证明“当前 QUIC peer 持有这个 NodeId 对应的 transport private key”，不自动授予 Validator 权限、网络信任或 admission。Validator 权限仍只来自有效 ValidatorCredential；trust/discovery/admission 仍需要独立规则。

Validator capability 使用两份严格 sidecar，且都不属于协议状态。`<snapshot-base>.validator.json` 只保存非秘密运行配置：`authorizer_public_keys_base64` 与 `bft_timeouts_ms.{proposal,prevote,precommit}`；Authorizer key 必须是标准 Base64 编码的 32-byte Ed25519 public key，三个 timeout 均以毫秒表示且必须大于 0。`<snapshot-base>.validator.keys.json` 是私钥 keyring，只保存 `validator_id`、identity private key、recovery private key 和一个或多个 current/historical consensus private key；每个私钥都是标准 Base64 编码的 32-byte Ed25519 seed。两份文件都采用 strict JSON、拒绝未知字段，并以 64 KiB 为读取上限，读取层本身不会先无界分配整个文件。keyring 在 Unix 上要求 `0600` 或更严格；Windows 当前依赖部署账户的文件 ACL，不伪造一套并不存在的跨平台权限模型。

`second node` 启动时先判定 state backend：若存在 Full `StateStore` snapshot，就从该完整状态启动 Node；若没有 Full snapshot、但存在 `<snapshot-base>.public` 的 `PublicStateStore`，则以纯 Public backend 启动并在启动行显式标记 `PUBLIC`；两种 durable backend 都不存在时拒绝启动。只有 Full backend 才继续组合 Validator capability：两份 Validator sidecar 都不存在时运行 non-validator Full Node；两份都存在时先完成 keyring 与 durable `ValidatorRegistry` / active+retained ValidatorSet 校验，再把 Validator capability 装入同一个 `NodeRuntime`；只存在其中一份属于不完整部署配置，必须在创建 transport identity 和绑定 socket 之前 fail-closed。`validator_id` 必须已经存在于 durable `ValidatorRegistry`；identity/recovery private key 导出的 public key 必须分别匹配该 Validator 的 registry credential；每个提供的 consensus private key 都必须属于该 Validator 的永久 consensus-key history；当前 active ValidatorSet 与所有仍被 PreparedTask 引用的 retained ValidatorSet 中，该 Validator 需要使用的每一把 consensus key 都必须存在。若该 Validator 在当前 snapshot 中既没有 active authority 也没有 retained PreparedTask authority，同样拒绝启用 Validator capability。recovery private key 在正常 BFT loop 中不参与会话认证或签共识票；当前只验证其 operator keyring 语义，既有 recovery/key-rotation 流程仍是它唯一的协议职责。启用 Validator capability 的 Node 复用同一 public network、PeerStore、bootstrap、connection budget 与 NodeRuntime，同时运行 Validator-only BFT；启动后先补完 durable finalized PreparedTask commit，再恢复未完成 PreparedTask scope。外部 LegalTask submission 已作为同一个 Node listener 上的 privileged service surface 提供，不存在第二个 daemon、第二个监听端口或第二套 transport/runtime。CLI 入口为 `second submit <address> <transaction-json-file> <authorizer-public-key-base64> <server-cert-base64>`：transaction JSON 从文件读取，避免把私有业务内容直接塞入命令行参数；CLI 中单独提供的 Authorizer public key 只用于把该 JSON 严格解析成原始 signed `LegalTask`，服务端不会把这个 CLI 参数当成授权来源，真正的业务授权仍来自 `LegalTask` 内嵌的 Authorizer public key + signature 并由接收 Validator 的本地 `AuthorizerSet` 独立验证。

Validator/governance operator surface 也复用同一个 `second` 可执行文件，不造第二个 daemon 或管理员网络。`validator-admission <keyring-file> <request-file>` 生成并本地验证候选 admission request；`validator-rotate <snapshot-base> <identity|recovery> <request-file>` 生成新的 consensus private key，并把 private key + signed rotation request 写入 bounded private rotation-key log；`validator-transition-build <snapshot-base> <plan-json> <source-file>` 以当前 durable ValidatorSet 为唯一基线，只读取 delta plan 中的 `remove_validator_ids + admission_request_files + rotation_request_files`，自动构造 next set 并调用既有 transition validator，operator 不手工维护第二份 membership 真值；`validator-transition-submit <address> <snapshot-base> <source-file> <server-cert-base64>` 用当前 Validator identity key 经 privileged governance service 提交 source，接收 Validator 必须针对自己的 current durable set/registry 重新验证，再沿 Validator-only BFT source 分发并注册既有 consensus。`recovery-checkpoint <address> <snapshot-base> <server-cert-base64>` 请求节点为下一 recovery serial 启动既有 StateRecoveryCheckpoint consensus；`recovery-install <address> <destination-snapshot-base> <trust-snapshot-base> <server-cert-base64>` 只向空 destination 安装由 trusted exact ValidatorSet 验证过的 certified shared recovery state，并保持 local signing safety locked。所有 governance/recovery service 都使用 current Validator identity authorization；没有 root/admin bypass。

membership/recovery 候选在共识注册与 Accepted 返回前写入同一 StateStore 快照，复用已有 source 编码；最多保留 64 个本地待办，不进入 shared recovery payload/digest。重复受理同候选不重写。membership 激活或 frontier 前进时，过时 transition 待办与主状态同次原子清理。恢复候选首次受理必须匹配当前 shared state；精确的本地已持久受理候选可以继续确认其历史 commitment，未知旧摘要仍拒绝。同 serial 的多个合法候选复用既有多候选 BFT，遵守 QC/lock；只有没有 QC/lock 时才优先选择当前状态候选，不改写不可逆 FinalityVote。恢复请求尚未完成期间，业务/分配提交及重启会触发必要的候选刷新，不冻结业务或引入全局排序。

恢复 QC 复用同一 proof 编码通过 Validator-only source 传播，零票 source 仍不是证据。合法 quorum proof 若匹配当前 shared state，原子保存 floor/proof 并清理完成待办；若已经过时，只推进 certified floor，并同次原子登记下一 serial 的当前状态候选，不把旧 proof/payload 挂到新状态上。未受理过旧候选的节点只有验证 quorum proof 才能补齐 floor。下一 serial 待办在重启后继续，共识签票锁永久保留；没有 pending 时不自动发布新的恢复 checkpoint。重复安装同一认证 floor/provider 不重写或重编码。显式 publish primitive 继续严格要求当前 payload 匹配。详见开发完整性清单。



当前 runtime 已有进程内 peer manager，并同时管理 inbound 与主动 outbound connection。peer 只有在 authenticated Hello 完成后才进入 registry；registry 以 NodeId 为唯一键，同一 NodeId 在同一节点上同时只保留一条 active connection，自连接（remote NodeId 等于 local NodeId）直接拒绝。没有重复连接时，任意方向的 authenticated connection 都可正常注册，因此 public read admission 仍保持开放。

当两个长期节点同时互拨形成两条连接时，peer manager 使用 NodeId 定义确定性仲裁：较小 NodeId 的节点保留 outbound，较大 NodeId 的节点保留对应的 inbound；反向连接被关闭。该规则只在同一 NodeId 已出现重复 active connection 时参与选择，不会因为连接方向“非首选”而拒绝一条原本唯一的连接。被替换连接的旧 lease 即使稍后释放，也不能删除新连接的 registry entry。

`PeerRecord` 是当前唯一的 outbound reachability 记录，内容只有 `NodeId + SocketAddr + self-signed server certificate pin`；它只回答“如何尝试连接这个 transport identity”，不携带 Validator role、ValidatorSet membership、共识权重或其他授权语义。`NodeRuntime::dial(&PeerRecord)` 复用节点自身持久 transport identity 发起 QUIC + authenticated Hello，要求 TLS endpoint 的 certificate pin 与 Hello 的 expected NodeId 都匹配。节点如果绑定到可直接表示的非 unspecified `SocketAddr`，runtime 会从自己的实际 listen address、NodeId 和当前 certificate 构造唯一 local PeerRecord；绑定 `0.0.0.0` / `::` 时则不对外宣称不可拨的 unspecified 地址。当前不会猜测 wildcard bind 对应的公网 IP，也没有 NAT/public-endpoint 探测；这类部署若需要被主动发现，仍应使用可拨的明确 listen address 或静态 bootstrap，未来只有出现真实部署需求时才增加显式 advertise endpoint 配置。outbound connection 注册后与 inbound 一样运行 public network session，因此保留下来的单条 QUIC connection 可由双方各自发起 public request。

节点维护独立的本地 `PeerStore` sidecar `<snapshot-base>.peers`，当前最多保存 128 个最近经过直接 transport authentication 的 `PeerRecord`。记录可以来自成功的 authenticated outbound dial，也可以来自该 NodeId 本人在现有 authenticated connection 上返回的自身 reachability 声明；后一种声明证明“这是 NodeId holder 自己发布的当前 endpoint/certificate”，但不把 endpoint 可达性升级成协议 authority。第三方 gossip 的其他 record 在真正与目标建立 authenticated connection 前仍只是未验证 candidate，不会因为中继 peer 宣称就直接进入本地 PeerStore。PeerStore 是可丢弃的本地连接缓存，不属于 Second state snapshot、ValidatorRegistry、finality 或任何共识 authority。成功完成 exact-set BFT 身份认证的端点附带仅本地 ValidatorId 提示，优先于普通端点保留和拨号；同一 Validator 只保留一个认证端点，普通 peer churn 不能挤掉它。成员 authority 变化后清除失效提示；每次连接仍重新进行 BFT 身份认证，提示本身不能授权。当前本地缓存格式为版本 1，逐条复用现有 Peers wire codec，并保存本地角色提示；不兼容之前的开发缓存。

`GetPeers { limit }` 当前上限为 32。若本节点存在可宣称的 local PeerRecord，响应第一项优先返回自己的当前 record，剩余名额再从 PeerStore 中按最近认证成功顺序返回记录，并排除当前请求方；这样不需要增加第二套 advertisement 消息或签名格式，远端可利用当前 authenticated QUIC connection 把“response 中 NodeId 等于 remote NodeId 的 record”识别为 NodeId holder 自己的 reachability 声明。`NodeRuntime::bootstrap` 先尝试本地持久 PeerStore，再把调用方提供的 bootstrap records 作为 fallback；连接任一 peer 成功后可继续请求更多 candidate，只有 responder 自己的 record 可以直接按 owner-authenticated reachability 更新 PeerStore，其他第三方 record 仍必须逐个通过真实 QUIC/TLS + Hello authentication 后才持久化。公开 bootstrap 与 Validator BFT maintenance 都按完整 PeerRecord 去重：不同 endpoint/certificate 的同一 NodeId candidate 可以分别尝试，避免一个过期或恶意记录阻断后续正确 endpoint。每个实际连接仍必须验证 certificate pin、transport NodeId 与 BFT identity authority。PeerStore 只是可重新发现的 reachability cache：写入使用邻接 `.new` 文件完整写入并 `sync_all` 后 rename 发布，不原地 truncate；已确认属于本地 peer-cache 格式内容损坏时启动时丢弃为 empty cache 并回到 bootstrap/discovery，真实文件系统 I/O 错误仍返回错误。

长期 `second node` 的初始 bootstrap 来源固定为可选 sidecar `<snapshot-base>.bootstrap.json`。文件不存在表示没有静态 bootstrap，不阻止仅依靠已有 PeerStore 或 inbound peer 启动；文件一旦存在则必须是严格 JSON 数组，每项只含 `node_id`（64 位小写 hex）、`address`（SocketAddr）和 `certificate_base64`，未知字段、非法 NodeId/address/certificate、超过 128 条记录或 256 KiB 文件都会在创建 transport identity 之前使节点启动失败。该 sidecar 是本地 deployment hint，不属于 snapshot、protocol state 或 authority。
`init-network` 生成 bootstrap 时复用上述同一个 `PeerRecord` address/certificate validation 与同一个 sidecar schema，不维护 provisioning 专用的第二套 reachability 格式。由于 transport identity 已在 init 阶段持久化到最终 snapshot-base 对应的 `.transport` 内容中，第一次 `second node` 启动只是加载该 identity；因此 init 输出的 NodeId/certificate pin 与实际 daemon 启动后的 NodeId/certificate 必须一致，后续重启也必须保持一致。


runtime 当前以 8 个已验证且活跃的已知 peer 作为本地连接维护目标；这是实现策略，不是协议常量。节点每 2 秒运行维护 tick，连接不足时从 PeerStore + 静态 bootstrap + 已连接 peer 返回的候选继续扩展；没有取得新连接时，dial retry 从 1 秒指数退避到最多 60 秒，取得进展后重置。PeerStore 的选择策略保持有界且简单：直接认证成功或 owner-authenticated reachability 刷新会把记录提升到 MRU 端；对已持久记录的拨号失败会把该精确 record 降到最旧端，后续优先尝试近期成功/刷新的 peer，而不是永久反复先撞同一个坏 endpoint。QUIC client 与 server transport 都对已建立的长期 session 使用 2 秒 keepalive，并保留 5 秒 idle timeout；连接 admission、128 connection 容量与 peer/session 生命周期仍负责资源边界，keepalive 不授予任何额外 authority，也不会绕过 admission。当前仍没有 DHT、DNS seed 或 NAT traversal。

节点启动时恢复对应 backend snapshot，持续接受 QUIC connection；每个通过 peer manager 注册的连接独立运行 public network session，因此单个 peer 的断开、错误请求或握手失败不会结束 listener。public session 不缓存 runtime 启动时的 `SecondState` 或 public proof，而只依赖 `RuntimePublicSnapshot` source：Full backend 每个请求从同一次最新 `StateStore` read 派生 `PublicCurrencyView + current checkpoint proof + latest delta`；Public backend 则从 `PublicStateStore` 只读取自己已经 certified/persisted 的 `PublicCurrencyView + checkpoint proof`，不保存或对外冒充 Full Node 的 delta archive。public Currency 网络响应从 `PublicCurrencyView` 生成，不要求也不能访问 owner、PaymentAddress、claims、PreparedTask 等私有状态；state recovery loader 则只存在于 Full backend。这样同一套 network session 可由 Full/Public backend 复用，而不会制造第二套 public protocol 或让 public-only Node 伪造完整状态。当前 runtime 最多同时保留 128 个进入握手/已建立的 inbound + outbound connection；超过该本地容量的新 inbound `Incoming` 在握手前直接拒绝，主动 dial 则直接返回本地 capacity error。这个 128 是节点实现的 DoS / 资源保护默认值，不是协议、共识或 Validator 数量规则，不进入任何签名、frame 或 snapshot 版本。

`NodeRuntime::sync_freshest_certified_public_currency_view()` 仍保留为 Full backend 的一次性、只读网络观察 primitive：它使用本地 durable ValidatorSet/checkpoint floor 验证候选并返回 `RemoteCertifiedPublicCurrencyView`，不会把远端 public view 覆盖进完整 `SecondState`。`second observe-public-network <snapshot-base>` 继续使用这条语义，因此它是观察工具，不是 Full Node state recovery。

真正的 Public backend 使用独立的 persistent public sync。首次创建使用 `second public-init <destination-snapshot-base> <trust-snapshot-base>`：从显式 trusted Full snapshot 提取当前 `ValidatorSet + ValidatorRegistry`，若该 trust snapshot 已含可验证的 public checkpoint proof，则同时验证并安装对应 `PublicCurrencyView`；生成的 `<destination>.public` snapshot 不包含 owner、PaymentAddress binding、claims、PreparedTask、BFT vote-lock 或其他 private/signer safety state。之后同一个 `second node` 自动识别该 public snapshot，以 `PUBLIC` backend 启动。

Public backend 的长期 worker 默认每 15 秒运行一次，错过 tick 使用 delay 而不是追赶空转。每轮先只向 active peer 请求轻量 checkpoint proof：若远端 checkpoint 与本地相同则不下载 public state；同一 ValidatorSet 下若 epoch 前进，优先请求 `PublicCurrencyDelta(from_epoch, from_state_digest)`。delta 绑定旧 checkpoint 的 epoch + `state_digest`、目标 epoch 与完整目标 summary，只允许最多 4096 个变更、编码最多 56 KiB；客户端应用 delta 后必须重新构造 `PublicCurrencyView` 并再次用目标 certified checkpoint 验证完整 summary/digest。delta 因而只是节省网络的传输优化，绝不是独立 authority。Full `StateStore` 为此区分“当前 attached checkpoint proof”和“最后 certified delta baseline”：业务状态变化可以使当前 proof 失效，但不会把 baseline 误当成当前状态证明；从 baseline 到下一 certified checkpoint 期间只累计真正公开变化的 CurrencyAddress。累计超过 4096、baseline 缺失或 peer 不提供精确 delta 时，客户端只退回一次完整 certified public sync；不会发送不完整 delta。Full snapshot 当前开发版本为 1，直接保存该 bounded sync metadata，不为未发布旧版本保留兼容 reader。

ValidatorSet 变化不允许靠更高版本号自行信任。Full Node 在正式 certified ValidatorSet transition 激活时持久化对应的 `ValidatorSetTransitionProof` 历史；Public Node 发现远端 checkpoint 使用更高 ValidatorSet version 时，按当前 trusted set 的 version 精确请求下一条 proof，在内存中逐条验证 transition source、admission/rotation 语义和旧 set quorum certificate，再推进本地 ValidatorSet/Registry。单轮最多验证/推进 64 个 transition，下一轮可继续追赶；缺失或非法 proof 只使该 peer/candidate 失败，不让恶意 public peer 终止 daemon。每次 membership transition 后本地 public view/checkpoint 被清空，必须在新 set 下重新取得 certified checkpoint，因此旧 set 的 public proof 不会被带进新 authority。当前 Public backend 只消费 transition proof/delta 并服务自己当前 certified public view；它不冒充 Full Node 的 transition-proof/delta archive。

当前公开节点网络面暴露 Ping/Pong、public Currency 查询/同步以及有界 `GetPeers/Peers` reachability discovery。BFT Proposal/Prevote/Precommit/QC、不可逆 `FinalityVote` 与 `FinalityCertificate` 都不进入这条 authenticated-open public session，而是使用独立 Validator-only BFT connection：runtime 以当前 active ValidatorSet 加仍被本地 active PreparedTask 引用的 retained ValidatorSet 组成可服务 authority，Validator 使用跨这些版本保持不变的 identity key 做 channel-bound 会话认证，再传 bounded binary BFT envelope。每个 envelope 仍按自身 `validator_set_version` 选择唯一 exact ValidatorSet：非 PreparedTask scope 只能使用当前 active set；PreparedTask scope 才允许使用其 durable 绑定的 retained set，而且发送者与接收者都必须是该 exact set 成员。发送 worker 在同一条 QUIC connection 的单向流额度中分别为普通 Proposal/Vote/QC 与 FinalityVote/FinalityCertificate 保留有界 in-flight 容量；finality 仍优先调度，但不能占满全部 stream credit 而饿死下一 scope 的普通 BFT。最前端已认证 Validator BFT inbound 也使用有界队列，容量与既有未注册 scope/message 上界保持同阶，满载直接 fail-closed 关闭当前输入路径，不允许恶意已认证 peer 用合法格式消息把 runtime 内存无限推高。NodeRuntime 为 PreparedTask、PublicCheckpoint、ValidatorSetTransition、StateRecoveryCheckpoint 继续复用同一条共识运行时；BFT timeout 已提升为 `ValidatorRuntimeConfig` 的节点级配置，所有 scope 读取同一配置，不再要求每次 `start_*_consensus` 手工传入 `BftTimeoutConfig`。PreparedTask proposal envelope 仍只承载 digest/签名等共识元数据，不把发送者计算出的 plan 当业务真值。私有对象传播现已固定为原始 signed `LegalTask`：本地 prepare 时 PreparedTask 在生命周期内 durable 保留该 source，供重启后的 exact-set peer 按需获取；source 不参与 plan digest，也不是第二份执行真值。拥有 source 的 Validator 只向该 scope 当前 proposer 发送轻量 availability hint，缺对象的 proposer/其他收到 proposal 的 exact-set Validator 通过同一 Validator-only BFT connection 分块 pull；hint 和 pull 在重连/漏包时按运行时 timeout 有界重试，不做公开 gossip 或全量广播。远端同时缺对象的 fetch 复用既有未注册 scope 的 32-scope 上限；单个 source 最多接收 64 个 32 KiB chunk，即 2 MiB，超过边界直接拒绝且本地新任务也不会进入自动共识路径。接收方必须先用本地 `AuthorizerSet` 重新验证 signed LegalTask，再基于自己的 durable state 与该 task 绑定的 exact active/retained ValidatorSet 独立 prepare；只有本地重新生成的 canonical plan digest 与公告/proposal subject 完全一致，才注册并进入现有 BFT signer 路径。新 LegalTask 通过 Validator runtime 成功 prepare 后自动启动对应 scope；节点运行循环启动时也会自动恢复本地 durable PreparedTask scope。

当前 public Node admission 是开放的：任何能够完成 authenticated Hello、证明持有其 NodeId 对应 transport private key 的 peer，都可以使用上述公开只读网络能力，前提是通过现有 connection / stream / sync resource guardrail 与 peer 去重规则。public admission 不维护 allowlist，也不要求 ValidatorCredential，因为这些数据本来就是公开状态。

这不意味着 Second 的 Validator 层是 permissionless。transport-authenticated public peer 只获得公开网络访问权；Validator vote、ValidatorSet membership、LegalTask submission 与其他 privileged protocol 都使用各自独立的授权规则。LegalTask submission 不把任意 transport NodeId 升级成业务写 authority：客户端只是在 certificate-pinned、加密的 QUIC connection 上把 signed LegalTask 定向提交给一个 Validator-capable Node，接收方必须用自己的 `AuthorizerSet` 验证 signed LegalTask 后才能 prepare。peer discovery 只传播 reachability candidate，不建立任何 authority；未来其他 privileged peer role 若需要与 transport endpoint 绑定，必须另外定义可验证 binding，不反向改变当前 public read service 的开放 admission。

每个 stream 内仍使用统一的自定义二进制 frame：

~~~text
magic
protocol_version
payload_length
payload
~~~

当前能力包括：

- Hello；
- Ping / Pong；
- 公共 Currency 分页；
- 公共 summary；
- public checkpoint proof；
- bounded public Currency delta；
- certified ValidatorSet transition proof 查询；
- certified public state sync；
- 有界 `PeerRecord` 查询与 bootstrap peer discovery；
- 仅 Validator capability 启用时可用的 chunked signed `LegalTask` submission service；
- 仅 Full backend 可用、由 exact `TaskId + request_digest` possession commitment 授权的私有 LegalTask status query。

当前未发布 wire protocol 只有**一个当前格式**，`CURRENT_NETWORK_PROTOCOL_VERSION = 1`。未发布阶段新增或修改 message set 时直接修改当前格式，不因为内部迭代递增为 v2/v3/v4……，也不维护旧格式 fallback、双解码、迁移桥或 legacy handshake；只有明确进入发布/稳定兼容阶段后才开始协议版本演进。LegalTask submission 与 authenticated-open public read session 使用同一个 QUIC listener 和统一 network frame，但由 connection 的第一条 service request 显式分流：第一条消息为 `LegalTaskSubmissionOpen` 时，在 public `PeerManager` 注册之前直接进入 submission handler，因此 raw LegalTask 不会进入 public session、PeerStore、peer discovery 或 public gossip；public-only Node 在读取 task body 前直接返回 `Unavailable`。Validator-capable Node 对 submission 另设最多 8 条并发 service connection，且这些连接仍同时计入整个节点 128 条 connection 总预算；除 QUIC idle timeout 外，connect/handshake、单次 request/response、stream read/write 都有独立有限 protocol deadline；listener 在完成握手后等待第一条 service request 也有 deadline，超时立即释放 connection permit。LegalTask chunked upload 使用单独的整体 service deadline，避免用普通短 request deadline 截断合法大任务。单个 canonical encoded LegalTask 最大 2 MiB，沿用 Validator 间 private source bootstrap 的同一语义上限；wire 上复用现有 32 KiB source chunk 大小顺序传输，并严格校验 total length、offset 与 chunk boundary，避免为了外部入口维护第二套 task 编码或资源常量。CLI 的 transaction JSON 属于本地输入表示，不是 wire object；其本地文件读取 budget 为 16 MiB，解析完成后仍必须编码到上述 2 MiB canonical LegalTask 上限内才能发送。

submission 服务在收齐 task 后调用同一个 Validator runtime task path：先用本地 AuthorizerSet 验证签名和 payload。若任务需要 Issue / LeakRepair 的新 identity，则先通过既有 CurrencyAllocation 区间与物化检查，再 durable 记录该 exact signed task 为 allocation pending，并注册 `CurrencyAllocation` BFT；allocation certificate 安装后 runtime 自动从 certified range prepare，并继续注册 PreparedTask BFT。无需新 identity 的任务直接 prepare。响应不会为了客户端同步等待 finality。`allocating` 表示这次请求已进入 address-allocation consensus，`prepared` 表示已新建 durable PreparedTask 并启动/注册其共识，`pending` 表示完全相同的 `TaskId + request digest + signed source` 已在本地进行中，`succeeded` 表示该请求已经完成业务 commit。这样网络重试是幂等的，同时同 TaskId 的不同 signed request 仍然按冲突 fail-closed；服务端对不合法/不可执行的业务拒绝只返回通用 `Rejected`，不把私有状态或具体执行失败原因泄露给外部提交者。

LegalTask status 不维护 `TaskHistory`、`TaskResultStore` 或第二份生命周期真值。CLI 为 `second task-status <address> <transaction-json-file> <authorizer-public-key-base64> <server-cert-base64>`：客户端在本地从原始 signed transaction request 重新构造 LegalTask，并复用现有 request-digest commitment，只在线上传输 `TaskId + 32-byte request_digest`，不重复上传最多 2 MiB 的任务正文。TaskId 本身是调用方可选字符串，不能作为查询认证；服务端只有在 durable binding 的 request digest 与查询 commitment 精确相等时才暴露该 exact request 的状态。不存在 binding 或 digest 不匹配统一返回 `unknown`，因此仅知道/猜中 TaskId 不能枚举某个私有任务是否存在。Full backend 即使没有 Validator capability 也可从自身 durable private state 回答；Public backend 没有 TaskId binding / PreparedTask，必须返回 `Unavailable`。

状态只从现有 durable facts 推导：`bound` = exact TaskId binding 已存在但当前没有 PreparedTask 且未成功（其中也包括已经进入 CurrencyAllocation、尚未形成 PreparedTask 的阶段；status API 不为 allocation 另建第二份生命周期真值）；`prepared` / `voting` / `finalized` 分别直接对应 durable PreparedTask phase，其中 `finalized` 表示 finality 已持久化但业务 commit 尚待完成/恢复；`succeeded` 与 `cancelled` 分别来自同一 TaskBinding 的成功、已认证取消终态；取消后的相同请求拒绝为 TaskCancelled，换请求仍拒绝为 TaskIdAlreadyBound。当前**没有 `failed` 状态**：协议尚未保存一份权威 durable final-failure result，因此状态服务不得为了 API 完整性额外创造历史结果表。响应也不返回 task body、Authorizer、账户、owner、claims 或业务失败原因。

full public sync 的本地 materialization budget 当前为 64 MiB，仅约束客户端为 `Vec<PublicCurrencyState>` 物化整份公开状态所允许占用的元素存储空间。允许的 state 数量通过当前 `size_of::<PublicCurrencyState>()` 动态换算，而不是把 Currency 数量写成协议上限；远端 summary 声明的 `current_supply` 超出预算时，客户端必须在请求任何分页数据之前拒绝。实际 Vec 使用 `try_reserve_exact`，无法满足本地分配时返回错误而不是继续无界增长。该预算同样是本地资源保护策略，不限制 Second 协议本身允许存在多少 Currency。

运行时公开服务按请求需要加载派生数据：checkpoint proof 与 bounded delta 查询只读取 durable proof/delta metadata，不为了一个固定大小 proof 重建完整 `PublicCurrencyView`；只有 summary / Currency page 请求才需要 view。Full/Public backend 的派生 view 按 snapshot generation 缓存，并仍以 `StateStore` / `PublicStateStore` 的 durable read token 作为失效依据，因此缓存只是已验证状态的派生加速层，不是第二份资产真值。

### 16.1 Node 与 Validator 分离

Node 的 public 能力与 Validator 权限分离。Public backend 可以：

- 持久同步并验证公开状态；
- 持久跟随 certified ValidatorSet transition trust；
- 保存并服务当前 certified public view；
- 进行公开审计。

Full backend 还可以保存完整 Second state；只有 Full backend 上成功启用、且拥有有效 ValidatorCredential 与对应本地 keyring 的 Validator capability 才拥有 finality vote 权。Public backend 永远不因“同步到 ValidatorSet”而获得 Validator 权限。

### 16.2 当前没有广播 LegalTask

LegalTask 包含 PaymentAddress、业务执行关系等敏感信息。

在没有明确、经过确认的隐私安全传播方案前，不把 LegalTask 直接广播到公开节点网络。

### 16.3 当前网络不是“全局交易总序”

Second 不默认要求：

- 全局 block chain；
- 所有 transaction 的单一总排序；
- stake-based consensus；
- UTXO-style fixed transaction inputs。

需要 finality 的对象是具体 subject digest，而不是强行把系统设计成传统区块链 block。

---

## 17. 持久化设计

当前实现使用本地私有 snapshot store。

持久化内容包括：

- Account；
- PaymentAddress binding / lifecycle；
- Currency private owner mapping；
- Currency allocator frontier；
- Task identity binding / success state；
- Transfer establishments；
- ValidatorSet；
- ValidatorRegistry；
- PreparedTask；
- Validator vote locks；
- 当前实现需要恢复的 public checkpoint 相关状态。

### 17.1 私有状态不得公开复用

持久化快照包含 owner，因此只能作为本地私有恢复数据。

不能把私有 snapshot 直接作为公共 state-sync payload。

### 17.2 双槽恢复

当前 StateStore 使用两个 generation slot：

~~~text
<base>.a
<base>.b
~~~

两个 slot 只是冗余载体，durable authority 由邻接 `<base>.commit` reference 明确记录：`generation + committed snapshot checksum`。写入顺序固定为 primary slot `write_all + sync_all` → 独占 store lock 下写入固定 80 字节 commit reference 并 `sync_all` → best-effort 刷新 mirror；一旦 commit reference 发布，旧 mirror 即使内容仍然有效也不能把已经输出过的 signing/vote-lock state 回滚。加载只接受 generation 与 checksum 同时匹配当前 commit reference 的有效 slot；commit 指向的 generation 已丢失/损坏时 fail-closed 为没有可用 snapshot，而不是退回旧 generation。mirror 刷新失败不能把已经发布的 commit 重新报告为未提交。

文件槽位 I/O、commit reference、快照 codec、validator codec、local prepared codec 已按职责拆分。`StateStore` / `PublicStateStore` 可缓存已经完整验证过的 immutable snapshot，但每次命中前都重新读取 commit reference 与 slot metadata 组成的 read token；跨进程 durable 写入会使 token 变化并自动失效，因此 cache 不是第二份状态真值。

### 17.3 崩溃恢复原则

重启后必须恢复：

- 已经永久消费的 Currency identity frontier；
- TaskId binding；
- exact PreparedTask；
- Currency claims 可从 prepared plan 重建；
- Transfer establishment；
- ValidatorRegistry 历史；
- Validator vote-lock。

不能因进程重启让“永久事实”消失或让 Validator 获得改票机会。BFT 的 current round、当轮 prevote/precommit、value lock 与 finality-ready 标记同样属于必须跨重启连续保存的本地 safety state；snapshot 写入/恢复还必须校验其 ValidatorId 与 exact active/retained ValidatorSet authority。

snapshot 中的 active PreparedTask 不能只满足字节格式正确：写入与恢复都必须验证 frozen plan 与 durable `SecondState` 的跨字段一致性。校验复用正式 `PreparedTask::apply()` / claim restore 语义，不在 persistence 层复制一套执行规则；Transfer 的 frozen Currency 数量必须等于 frozen amount，Issue / LeakRepair 的预分配 Currency identity 必须已经落在持久 allocator frontier 之下，并且 active prepared plans 之间不能重复占用同一预分配 identity 或产生互相冲突的 Currency claims。任何不可按当前 durable prerequisite/business state 验证的 active frozen plan 都视为无效 snapshot。

所有持久 Validator vote-lock 的 `ValidatorId` 还必须存在于永久 `ValidatorRegistry` 历史中；已经 retired 的 Validator 仍可保留历史 lock，但从未被网络授权过的 ValidatorId 不能出现在 snapshot vote-lock 中。若 `PreparedTask` 仍 active，则其 vote-lock signer 还必须属于该 task 绑定的 exact active/retained ValidatorSet；task 已完成、只剩历史 lock 时不为此永久保留完整旧 membership。

Validator 第一次为某个 finality scope 建立 vote-lock 时，签名 authority 必须在同一 store 锁临界区绑定到 durable snapshot，而不是只信调用方传入的同版本对象：调用方提供的 `ValidatorSet` 必须与持久 `ValidatorRegistry` 的 current set 精确一致；PublicCheckpoint 的 summary 必须等于锁内最新 durable public Currency state，且其 epoch 不得低于已持久化的 `checkpoint_floor_epoch`；ValidatorSetTransition 的 next set 必须重新通过锁内最新永久 `ValidatorRegistry` history 校验；StateRecoveryCheckpoint 的 shared-state digest 必须重新由锁内最新 durable shared state 计算并完全一致。subject-specific 校验成功后才能持久化 vote-lock。已经存在的同 digest lock 仍允许确定性重放；同 scope 不同 digest 继续 fail-closed 为 double-sign conflict。PublicCheckpoint 的 vote-lock scope 是 `(validator_set_version, epoch)`，StateRecoveryCheckpoint 的 vote-lock scope 是 `(validator_set_version, serial)`：两者都不能只靠 freshness 数字跨 ValidatorSet 复用，同一 Validator 留任到新 set 后必须建立独立的新 scope。

正式 LegalTask 状态写入还必须防止 stale writer 覆盖已经 durable 的更新。`PreparedTaskBook` 对包含 `SecondState` 与 PreparedTask 集合的 read-modify-write 使用语义 compare-and-swap：写入 candidate state 时，锁内最新 snapshot 的 `SecondState` 必须仍等于本次计算 candidate 所基于的 base state，并且 PreparedTask map 必须仍等于本地 book 所基于的旧 map；仅修改 PreparedTask lifecycle 时也必须以旧 map 做 compare-base。任一比较不成立都 fail-closed，调用方必须重新加载最新持久状态，不能自动把两份冻结计划或业务状态合并。

这里不使用 snapshot `generation` 作为业务 CAS token，因为 vote-lock、checkpoint floor 等独立持久元数据也会合法推进 generation；这些元数据更新不应无故让未冲突的 LegalTask writer 失败。store 锁负责原子检查+写入，语义 base-state / base-prepared 比较负责防 lost update。

`StateStore` 本身不是资产或协议状态 mutation API。空 store 可以通过 `initialize` 一次性写入 bootstrap state；需要携带既有 ValidatorRegistry 历史时使用同样仅限空 store 的初始化入口。初始化完成后，不再提供接受任意 `SecondState` 并覆盖当前 snapshot 的公开 writer。checkpoint proof / checkpoint floor 更新只作用于锁内读取到的最新 snapshot metadata，不接受调用方附带另一份 state。后续 LegalTask 导致的 `SecondState` 演进只能由正式 `CurrencyAllocation`（仅 identity frontier reservation）以及 `PreparedTaskBook` 的 prepare / finality / commit 持久化路径完成。

### 17.4 State recovery commitment

完整恢复分成“网络可共同认证的 shared state”和“Validator 自己必须连续保存的 local safety state”，二者不得混成一个 digest。`StateRecoveryCheckpoint` 当前承诺的 shared state 只包含：完整 `SecondState`（因此包括 committed TaskId bindings、PaymentAddress/ownership、payment execution prerequisite 等协议/业务事实）、当前 active `ValidatorSet`、永久 `ValidatorRegistry`。canonical bytes 复用 persistence 当前权威字段编码器；persistence codec 与 recovery commitment 不分别维护两套 `SecondState`/Validator 编码规则。

以下字段明确**不进入** shared recovery digest：snapshot slot `generation`、attached public checkpoint proof、`checkpoint_floor_epoch`、recovery checkpoint freshness floor、active/retained PreparedTask 本地 lifecycle、`retained_validator_sets`、BFT round/prevote/precommit/lock/finality-ready state、Validator finality vote-lock。`retained_validator_sets` 只为本节点仍活跃并绑定旧 set 的 PreparedTask 服务；recovery floor、BFT local state、finality vote-lock 与 PreparedTask phase 都是 Validator/节点本地 safety/recovery metadata，不保证不同节点相同。把这些字段塞进 quorum shared digest 会导致诚实 Validator 因各自本地签票/观察历史不同而无法形成同一 QC，并且“签 recovery checkpoint 本身新增本地 safety metadata”会造成自引用 digest 循环。

`StateRecoveryCheckpoint` 具有独立 `serial`，与 public checkpoint epoch、`ValidatorSet.version`、snapshot generation 均不是同一序列；checkpoint digest 使用独立 domain separation，并把 protocol version、serial、当前 validator-set version 与 shared-state digest 一起绑定。`ValidatorSigner` 只能用持久 `ValidatorRegistry` 认可的当前 active ValidatorSet 对它签票，vote-lock scope 为 `(validator_set_version, serial)`；同一 scope 重放同 digest 允许，同 scope 不同 shared state 永久拒绝。

节点还为 recovery checkpoint 持久化独立的 serial head，按 `ValidatorSet.version` 分桶。每个桶保存最高 `serial + checkpoint_digest + certified`：本地第一次为某个 recovery checkpoint 建立 vote-lock 时，serial head 与 vote-lock 在同一 snapshot 写入，但此时 `certified = false`；节点显式接受/发布一个已经通过当前 active ValidatorSet QC 的 recovery checkpoint 后，才把对应 head 标为 certified。低于当前 head 的 serial 一律拒绝；同 serial 的本地重复签名只允许同 digest；如果本地只投过某个未 finality 的候选，而同 serial 的另一 digest 后来取得合法 QC，则允许该 QC 覆盖未 certified 的本地 head，因为本机并未对新 digest 再次签票。若 head 已经 certified，则同 serial 不同 digest 永久拒绝。

recovery serial 的发行规则现已固定：**每个新的 `ValidatorSet.version` 从 serial 1 独立开始；Validator 只能为本 set 当前已 certified serial 的严格 `+1` 签票，不能跳号，也不能仅凭自己对前一号投过票就继续下一号。** `StateStore::next_state_recovery_checkpoint()` 是本地权威发行入口：没有本 set head 时生成 1；存在未 certified head 时返回 awaiting-finality；存在 certified head `S` 时只生成 `S+1`。serial 溢出直接 fail-closed。ValidatorSet transition 不删除旧 set 的历史 head，但新 set 使用独立桶，因此不会继承旧 set 的 serial 数字。

已通过 QC 的 checkpoint 属于更强的网络事实：节点可以直接接受高于本地 head 的 certified serial 进行离线 catch-up，包括空 store 直接安装当前较新的 recovery checkpoint。这样不要求恢复节点下载从 1 开始的全部历史 QC；合法高 serial QC 的 quorum 中至少包含遵守签票规则的诚实 Validator，因此其存在意味着该 set 的连续 serial 前驱已经按协议推进。该 catch-up 只推进本地 certified head，不允许未经 QC 的任意跳号。

这套规则解决的是 recovery checkpoint 的**编号、freshness 与 anti-equivocation 协调**，没有被 BFT liveness 工作重写。现有 per-scope BFT 在同一 safety core 上已经具备 round/prevote/precommit/locking、precommit-QC→FinalityVote gate、确定性 proposer、Validator-only BFT transport，以及本地 timeout 驱动的严格 `+1` view-change；StateRecoveryCheckpoint proposal subject 仍必须通过既有 recovery serial/floor 与 exact persisted shared state 校验，并与 `next_state_recovery_checkpoint()` 复用同一个 persistence serial 计算 helper，不维护第二份 recovery 编号规则。多个节点即使对同一 next serial 出现不同 proposal，锁与 quorum intersection 继续负责 safety，round-robin proposer + `Nil` timeout 路径提供 liveness 推进原语。现在 `NodeRuntime` 已能从 durable snapshot 在线刷新 current active + PreparedTask retained Validator authority：`ValidatorSetTransition` 取得合法 FinalityCertificate 后，runtime 通过唯一的 `StateStore::activate_validator_set_transition_for_runtime` 路径原子激活 next ValidatorSet/Registry；激活后 peer maintenance、BFT handshake/dial 与新 scope 注册会刷新共享 authority，不存在另一套管理员 membership mutation，已经建立的 BFT peer 也读取同一份刷新后的 exact-set authority。authority refresh 在 runtime 内串行执行完整的 durable load→exact active/retained authority compare→swap/prune，避免并发 refresh 让较旧 snapshot 覆盖较新的内存 authority；因 BFT unauthorized 记录的 rejected NodeId 在 authority 未变化时跨 maintenance 轮次保留，只有 exact authority 真正变化才清空并允许重新评估，避免稳定 membership 下每 2 秒重复握手撞拒绝。对 PreparedTask / PublicCheckpoint / ValidatorSetTransition / StateRecoveryCheckpoint fanout 与接收 BFT envelope、驱动每个已注册 scope 的本地 timeout、缓冲先于本地注册到达的有限消息，并在 precommit QC 后复用既有 signer 产生不可逆 FinalityVote、聚合/relay FinalityCertificate。PreparedTask 从本地 durable plan 解析其 exact active/retained ValidatorSet；`ValidatorRuntimeKeys` 按 consensus public key 保存本节点可用的 private key，scope 使用哪个 ValidatorSet 就只允许取该 set credential 对应的 key，缺失历史 key 直接 fail-closed，绝不拿当前 key 给旧 set 代签。PreparedTask 本地不可逆 FinalityVote 与 phase→`Voting` 在同一 snapshot 原子持久化；形成或接收有效 FinalityCertificate 后，runtime 先把 `Finalized + quorum votes` 原子持久化，再自动 apply/commit BusinessState，只有 commit 已成功 durable 才向调用方暴露 `CertifiedPreparedTask` 事件。若节点在 finality durable 后、commit 前崩溃，重启先从 PreparedTask 内持久化的 quorum votes 重建并重新验证 FinalityCertificate，再补完 commit，不重新跑已经完成的 BFT。私有 task source bootstrap 现已接入同一 Validator-only transport：CurrencyAllocation 传播原始 signed LegalTask source；PreparedTask 另携带 Transfer 货币和 LeakRepair 储备的不可重建冻结选择，其余业务参数及已认证连续区间仍从原始请求和 durable state 重建。接收方必须本地重新做 Authorizer 验证，并通过同一 prepare 执行/占用校验及完整 plan digest 验证后才能安装候选。availability hint / request / chunk 只限 exact ValidatorSet；每个 active fetch 维护 progress deadline，当前 source 保持连接但超过 deadline 没有进展时会放弃该 source 并优先尝试尚未尝试的其他已连接、已授权 Validator，避免一个 silent peer 永久占住 fetch slot。Validator 运行时同时持有 AuthorizerSet、统一 BFT timeout 与本地时间源；本地新 prepare 和重启恢复的 durable PreparedTask 都会自动注册共识，不再依赖调用方逐 scope 注入 timeout。实现仍不会用本地时钟、snapshot generation、public checkpoint epoch 或 `ValidatorSet.version` 冒充 recovery serial。

`CertifiedStateRecoveryCheckpoint` 复用通用 `FinalityCertificate`，因此阈值仍是当前 active ValidatorSet 的 `floor(2N/3)+1`。可信的是 quorum 对 shared-state commitment 的证明，不是提供 recovery payload 的某个 peer。retained old ValidatorSet 没有发布新 recovery checkpoint 的 authority；旧 set 只继续服务其绑定的历史 PreparedTask。

privileged recovery 已复用现有 QUIC/TLS connection 与二进制 framing，但授权层与 authenticated-open public read 明确分离。`NodeId` / transport key 仍只证明 transport peer；请求 private recovery manifest/chunk 时，调用方必须额外声明当前 `ValidatorId`，并使用该 active ValidatorCredential 的 **identity key** 对 recovery request 签名。签名 domain 独立，并绑定 `CURRENT_NETWORK_PROTOCOL_VERSION + 当前 QUIC TLS exporter channel binding + ValidatorId + request kind`；chunk 请求还绑定 checkpoint digest、offset、limit。服务端对每个 manifest/chunk 请求都读取 runtime 当前已验证 durable snapshot：当前 active ValidatorSet 仍负责验证请求者 identity；provider 是否仍可服务则通过 snapshot 中当前 durable recovery checkpoint proof 的 digest 与 immutable provider checkpoint digest 精确相等来判定，不再为每个最多 60 KiB chunk 重建/重哈希完整 shared payload。provider 创建/发布时已经用同一 durable shared state 做完整 commitment 校验，因此这里的 digest identity check 只是廉价 freshness fence，不建立第二份 recovery authority。`StateRecoveryProvider` 因此只是显式 publish 后的 immutable payload/proof 缓存，不缓存也不决定当前 authority；一旦 shared state、ValidatorRegistry 或 active ValidatorSet 在线变化，旧 provider 立即对外表现为没有可服务 checkpoint，manifest/chunk 都不能继续读取，必须显式 publish 与新 durable snapshot 匹配的 certified checkpoint 后才能恢复服务。错误 key、未知/retired ValidatorId 都统一拒绝。consensus key 继续只用于 finality，recovery key 继续只用于既定 recovery/rotation authority，不与会话认证职责复用。

`StateRecoveryPayload` 的 bytes 就是 shared-state commitment 使用的同一份 canonical `SecondState + active ValidatorSet + ValidatorRegistry` 编码，不创建第二套私有 snapshot serializer。下载先取得 manifest 中的未验证 recovery checkpoint proof 与 payload length；客户端必须先用调用方已经信任的 exact `ValidatorSet` 验证 QC，再请求 payload chunk。payload 以最多 60 KiB 的 chunk 传输，避免被 64 KiB network frame 上限卡住；每个 chunk request 都重新做 channel-bound identity proof，并绑定 checkpoint digest/offset/limit。组装完毕后重新 decode canonical payload，再次验证 payload digest、exact ValidatorSet 与 certified checkpoint，一处不一致即 fail-closed。QUIC 已提供传输加密，不另造应用层加密格式。

`NodeRuntime::publish_state_recovery_checkpoint` 只发布一个与当前 durable shared state 匹配的 certified checkpoint：每次调用从同一 `StateStore` snapshot 一次性取得 state、当前 ValidatorSet 与 ValidatorRegistry 构造候选 immutable provider，再由持久层在锁内对最新 snapshot 重新验证 QC/shared payload、durable 推进 recovery freshness floor，并把该 certified recovery proof 写进同一 snapshot；全部成功后才把 provider 暴露到内存。StateRecoveryCheckpoint 在 runtime BFT 中 finality 后，只有仍匹配当前 shared state 的 proof 才自动发布 provider；过时 QC 通过同一持久层推进 floor 并安排下一 serial，不需要额外管理员复制 certificate；节点重启时也直接从 snapshot 内这份唯一 durable proof 重建仍然有效的 provider，不维护第二份 recovery cache/sidecar。这样 runtime 在线激活 ValidatorSet 后不会拿启动时旧 membership/registry 发布新 checkpoint；若构造与持久验证之间 snapshot 已变化则 fail-closed。runtime 仍不会因启动节点就自动选择下一个 recovery serial。provider 可以被更新的 certified checkpoint 替换；正在下载旧 digest 的客户端若因此无法继续，应从 manifest 重新开始。

`StateStore::install_recovered_state` 只允许写入**空 store**：先验证 trusted ValidatorSet、QC、payload exact set 与 shared-state digest，再通过现有 dual-slot writer 一次性安装。任何已有 snapshot 都返回 `AlreadyInitialized`，因此 recovery 不能成为任意 state overwrite API。安装结果只包含 recovered shared state；retained sets、PreparedTask lifecycle、vote-lock、attached public checkpoint proof 均为空，public checkpoint floor 当前从 0 开始；recovery freshness floor 则由安装所依据的 certified recovery checkpoint 初始化。snapshot 同时写入 `validator_safety_ready = false` 和 `minimum_signing_validator_set_version = recovered ValidatorSet.version`，所以只恢复 shared state 的 Validator **可以读取/继续恢复数据，但不能重新签任何 finality vote**。

local-safety re-enable 采用 **consensus-key rotation safety fence**，不尝试重建无法证明完整的旧 vote-lock 历史。恢复中的 Validator 必须先由 identity key 或 recovery key 授权一个从 V 到严格下一版 V+1 的 `ValidatorConsensusKeyRotationRequest`，把自己的 consensus key 旋转到从未使用过的新 key；V→V+1 的 `CertifiedValidatorSetTransition` 必须由其余 Validator 独立达到 quorum，证书中出现恢复中 Validator 自己的票则本地拒绝把该 transition 作为 safety-recovery evidence。runtime 激活 V+1 时，仅对处于 `validator_safety_ready = false` 的本地 Validator 持久化一份最小 `PendingValidatorSafetyRecovery`：旧 exact ValidatorSet、该 Validator 的 rotation request 与 transition certificate；它是恢复本地 signing safety 所必需的证据，不是第二份 membership 真值。节点此时仍保持 locked。随后 V+1 必须形成与当前 durable shared state 匹配的 certified recovery checkpoint；本地 runtime 还要证明自己持有 V+1 credential 对应的新 consensus private key。只有 transition evidence、V+1 recovery proof 与新 consensus key 三者同时满足，runtime 才会原子把 `validator_safety_ready` 置回 true，并把 durable `minimum_signing_validator_set_version` 固定到 V+1；重启时也会从 snapshot 重新检查这套条件，因此 transition finality 与 recovery checkpoint finality 之间崩溃不要求人工重放解锁动作。`ValidatorSigner::complete_safety_recovery` 仍保留为底层显式 primitive，用于同一安全规则的直接验证路径。

之后所有签票入口除了检查 `validator_safety_ready`，还先检查所用 ValidatorSet.version 不得低于该 minimum。正常未丢失 safety state 的节点初始化时 minimum 等于其最初 active set version，后续正常 transition 不抬高它，因此仍可为合法 retained old-set PreparedTask 服务；经过 safety recovery 的节点 minimum 则从新 signing domain V+1 开始，哪怕旧 consensus private key 后来又从备份中被找回，也会因为 signing fence 永久拒绝 V 及更旧 scope。该 fence 与 recovery floor、vote-lock 一样属于本地 safety metadata，不进入 shared recovery digest。

这条流程只解决“consensus signing key 仍可通过 identity/recovery authority 安全轮换”的情况。如果 identity/recovery authority 也丢失或怀疑泄露，则不能用同一个 ValidatorId 解除 fail-closed；应通过正常 ValidatorSet transition 永久退休旧 ValidatorId，再以全新 ValidatorId 和全新三把 key 重新 admission。旧 PreparedTask 若仍绑定 V，只能由仍具备 V local safety continuity 的其他 Validator 继续完成；恢复节点不会重新加入旧 signing domain，liveness 不足时也不能用 recovery 绕过 safety。

---

BFT 会话认证的 `BftAuthenticate / BftAuthenticated` payload 为 tag + ValidatorId(u64) + active ValidatorSet version(u64) + identity signature(64 bytes)，共 81 字节。version 与 channel binding、role 和 ValidatorId 共同签名，只是会话建立时的追赶提示，不是新的 authority 或持久真值；身份、exact set 和 quorum 验证保持原规则。本地发现已认证对端版本更高时，关闭此次 BFT 连接，复用当前连接预算向该 pinned endpoint 按版本请求已有 durable transition proof；每步复用 source/rotation/quorum 校验和原子 store activation，从本地认证 anchor 连续推进。单次最多 64 步、总计 5 秒，部分进展持久化后由既有 maintenance 继续；缺失、伪造、frontier 不匹配不会解锁或跳过中间版本。临时只读证明会话仍占全局连接预算，只接受 transition proof 查询，不加入 peer discovery，也不替换普通去重连接。完成后重新认证 BFT，会话 authority 继续来自 durable exact set。内部开发 wire/framing 版本保持 1，不保留旧认证格式。

## 18. 当前模块边界

| 模块 | 职责 |
| --- | --- |
| ids.rs | Account / Payment / Currency / Task / Validator 身份格式 |
| currency.rs | Currency 内部对象与公开 Currency state |
| state.rs | Second 主状态容器、allocator、task binding、公共查询 |
| payment.rs | PaymentAddress 生命周期与 Transfer establishment |
| account.rs | 账户存在性校验与永久唯一的运行时开户 |
| prepared/lifecycle.rs | 账户与支付地址生命周期的单一 claim 实现及恢复 |
| task.rs | Operation、LegalTaskPayload、canonical signing encoding |
| authorization.rs | Authorizer trust、Ed25519 验签、VerifiedLegalTask |
| executor.rs | LegalTask 顺序执行、业务原子提交 |
| claims.rs | Currency / reserve claim 与 contention |
| prepared_plan.rs | 确定 prepared execution plan |
| prepared.rs | prepare / vote / finality commit 生命周期 |
| bft.rs | per-scope BFT statement / prevote / precommit / QC / local lock state |
| bft_proposal.rs | deterministic proposer proposal envelope / signature / subject binding |
| bft_driver.rs | 基于既有 BFT safety state 的 proposal→prevote→precommit、QC 聚合、timeout/view-change driver |
| finality.rs | 不可逆 FinalityStatement、ValidatorVote、certificate 验证 |
| finality_codec.rs | FinalityStatement / ValidatorVote / FinalityCertificate 的 crate-internal canonical binary codec；network/persistence 共用同一实现 |
| currency_allocation.rs | 需要新 Currency identity 的 LegalTask 对共享 allocator frontier 的 quorum-certified exact range reservation |
| validator.rs | ValidatorCredential / ValidatorSet / quorum |
| validator_registry.rs | Validator 永久历史与 key reuse 防护 |
| validator_admission.rs | Validator admission request |
| validator_rotation.rs | consensus-key rotation |
| validator_transition.rs | ValidatorSet transition |
| validator_transition_source.rs | admission/rotation evidence + next ValidatorSet 的 canonical governance source |
| validator_rotation_keys.rs | operator 新 consensus private key 的 bounded durable rotation-key log |
| validator_operator.rs | admission / rotation / transition / recovery CLI orchestration |
| network/governance.rs | privileged Validator governance/recovery request transport |
| runtime_governance.rs | governance source 验证、Validator-only 分发与 consensus 注册 |
| runtime_bft/membership_sync.rs | 根据已认证版本提示，按需从 durable proof 连续追赶 membership，复用原子 activation 与 existing maintenance |
| runtime_bft/checkpoint_sync.rs | fresh authenticated exact-set dial 时向单个成员补发当前 attached recovery proof |
| persistence/safety_recovery.rs | recovery 后 V→V+1 rotation transition 的最小 durable local safety evidence |
| validator_signer.rs | BFT 签票与 precommit-QC gated 的不可逆 finality 签票入口 |
| persistence/bft_store.rs | durable per-Validator/per-scope BFT round/lock/finality-ready state |
| public_state.rs | 公开 Currency summary / view |
| public_checkpoint.rs | 公共 checkpoint 与 finality proof |
| state_recovery_checkpoint.rs | shared recovery payload/checkpoint/QC commitment |
| network/codec.rs | network frame/version 校验与 message tag 领域分流；不承载各服务的具体 payload codec |
| network/codec/{core,public,recovery,bft,legal,governance}.rs | 按协议服务职责分离的 wire payload codec；各 message 语义只有对应领域的一份实现 |
| network/ | QUIC/session/public state sync 与各网络服务 runtime transport |
| network/bft_codec.rs | bounded BFT Proposal/Vote/QC/FinalityVote/FinalityCertificate binary envelope codec |
| network/submission.rs | 外部 signed LegalTask 的 bounded chunked client/service transport 与 submission response 语义 |
| task_status.rs | 从现有 TaskId binding / PreparedTask lifecycle 派生的 LegalTask status 枚举；不持久化第二份状态 |
| network/task_status.rs | 基于 exact TaskId + request_digest commitment 的私有状态查询 client 与 response 语义 |
| network_init.rs | strict Genesis deployment config、Validator keyring/credential preflight、snapshot/transport/bootstrap staging 与最终发布 |
| network/bft.rs | channel-bound active+retained Validator identity authority、exact-set envelope 授权与 bounded one-way BFT transport |
| runtime_bft.rs | Validator-only peer 维护、active+retained authority、inbound queue、普通 BFT / finality 独立保留 in-flight 容量的 exact-set fanout 与 send-failure 生命周期 |
| runtime_bft/keys.rs | Validator runtime identity key 与按 consensus public key 索引的 active/retained signing keyring |
| runtime_bft_consensus.rs | NodeRuntime per-scope BFT coordinator、PreparedTask/CurrencyAllocation 自动与恢复注册、round/timeout、early/future-message buffering 与完成 scope 的有界 compact certificate cache |
| runtime_bft_consensus/allocation.rs | 同 frontier 多候选 allocation 的 source/candidate 选择、QC 锁定与 restart/resume 编排 |
| runtime_submission.rs | 外部 LegalTask submission 的本地 Authorizer 验证、按需 allocation→prepare 的幂等 BFT 注册与 service handler |
| runtime_task_status.rs | Full backend 上从 durable binding/PreparedTask 推导 exact LegalTask 状态的 service handler |
| runtime_public_sync.rs | NodeRuntime certified public-state sync：Public backend long-running worker，以及 Full/Public backend 共用的一次性 certified network observation |
| runtime_recovery.rs | NodeRuntime recovery checkpoint 发布与当前 private recovery provider 生命周期 |
| runtime_bft_consensus/finality.rs | precommit-QC gated FinalityVote / FinalityCertificate 聚合、验证、relay、PreparedTask durable lifecycle 与 Certified* 事件收敛 |
| runtime_bft_consensus/candidates.rs | 局部分配与恢复检查点复用的多候选切换、等待及 QC 选择逻辑 |
| runtime_consensus_target.rs | CurrencyAllocation / PreparedTask / PublicCheckpoint / ValidatorSetTransition / StateRecoveryCheckpoint 到既有 proposal/finality 类型的单一适配层 |
| runtime_tasks.rs | Validator 间 PreparedTask/CurrencyAllocation 私有 signed LegalTask source pull / install / retry 与 durable task 恢复编排 |
| network/recovery.rs | channel-bound Validator identity 授权与 chunked private recovery transport |
| persistence/store.rs | `StateStore` 单一 slot/lock/load/write authority、durable read cache 与初始化入口 |
| persistence/commit.rs | snapshot generation+checksum 的 authoritative durable commit reference；防止 stale mirror 回退 |
| persistence/read_cache.rs | 以 commit reference + slot metadata 为 token 的 immutable validated snapshot cache；不是第二真值 |
| persistence/store_allocation.rs | CurrencyAllocation pending/range/certificate 的验证、安装与 frontier advance persistence |
| persistence/allocation_validation.rs | snapshot 中 allocation range/certificate 与 active/retained ValidatorSet 的跨字段不变量校验 |
| persistence/store_recovery.rs | shared-state recovery、recovery checkpoint floor 与 validator safety recovery persistence |
| persistence/store_transition.rs | ValidatorSet transition activation 与 retained-set lookup persistence |
| persistence/store_public_checkpoint.rs | public checkpoint/floor/delta metadata persistence |
| persistence/store_prepared.rs | PreparedTask lifecycle、business commit 与 retained-set derivation persistence |
| persistence/store_finality.rs | finality vote-lock persistence |
| persistence/ | 单一 Full snapshot schema/codec、BFT store 与以上领域化 `StateStore` impl；不维护第二份状态真值 |
| transaction.rs | 外部 transaction request 严格解析 |
| cli_transaction_sign.rs | 业务 Authorizer 本地密钥生成与严格 transaction JSON 离线签名 |
| local_file.rs | 有界本地读写、密钥随机性/权限、CLI 节点运行与离线安装的 OS 排他锁 |
| node_capabilities.rs | `second node` 启动时的本地 capability composition；Validator sidecar 成对存在/缺失规则与 fail-closed 装配 |
| validator_config.rs | Validator capability 非秘密 Authorizer / BFT timeout strict sidecar 的统一 load/write schema |
| validator_keyring.rs | Validator keygen、统一 keyring read/write schema、runtime identity/recovery/current+historical consensus durable authority 校验 |
| main.rs | CLI 参数分发与 validator-keygen / init-network 薄入口；不承载各命令业务实现 |
| cli_node.rs | 唯一长期 `second node` 命令的 Full/Public backend 装配与启动输出 |
| cli_legal_task.rs | submit / task-status 的 signed LegalTask 本地输入与短命 client 流程 |
| cli_public.rs | public-init / public sync / observe / query / snapshot-status 短命 CLI |
| cli_network.rs | ping 与 CLI/validator operator 共用 QUIC client、地址解析、digest 输出 helper |

新增能力应优先落入对应领域模块，不继续向无关大文件堆职责。

---

## 19. 核心不变量

Second 的所有实现最终都必须保持以下不变量。

### 19.1 唯一资产真值

~~~text
Currency -> Account | null
~~~

没有第二套余额资产账本。

### 19.2 Currency identity 永不复用

~~~text
used CurrencyAddress
=> forever used
~~~

### 19.3 AccountAddress 永久绑定

~~~text
AccountAddress -> same Account forever
~~~

### 19.4 PaymentAddress 永久绑定

~~~text
PaymentAddress -> same Account forever
~~~

### 19.5 PaymentAddress 退役不能破坏在途支付

~~~text
active -> retiring
blocks new Transfer establishment
but keeps established Transfer executable
~~~

### 19.6 成功 task 只执行一次

~~~text
Success(task)
=> exact replay causes no new asset mutation
~~~

### 19.7 TaskId 不能换请求

第一次绑定后，即使业务失败：

~~~text
TaskId cannot bind another verified signed request
~~~

### 19.8 Operation 严格按授权顺序

后序 Operation 不得提前建立协议事实。

### 19.9 Task 业务原子

~~~text
all business mutations commit
or
none of them commit
~~~

### 19.10 永久协议事实不因业务回滚消失

包括：

- 已分配 Currency identity；
- TaskId first-request binding；
- 已实际走到并建立的 Transfer establishment；
- Validator vote-lock；
- Validator identity/key history。

### 19.11 Transfer

~~~text
ΔCurrentSupply = 0
~~~

### 19.12 Issue

~~~text
ΔCurrentSupply = +N
~~~

### 19.13 Destroy

~~~text
ΔCurrentSupply = -N
~~~

### 19.14 Leak Repair

~~~text
ΔBalance(account) = 0
ΔCurrentSupply = 0
ΔReserveCount = 0
~~~

### 19.15 公开状态不得泄露 owner

~~~text
public Currency state
!=
Currency -> Account mapping
~~~

### 19.16 Validator 一票一权

~~~text
1 effective ValidatorCredential = 1 vote
~~~

不按财富、Currency 数量、算力或 stake 加权。

### 19.17 Validator 永远不能改票

~~~text
ValidatorId + TaskId
=> first signed prepared digest is permanent
~~~

---

## 20. 当前明确不能擅自拍板的设计

以下内容**不是当前已确认协议要求**，后续不能因为“区块链通常这样做”或“某数据库通常这样设计”就直接加入：

- PostgreSQL、SQLite 或其他特定数据库作为协议前提；
- Axum、HTTP server 或其他特定服务框架作为协议前提；
- stake / PoW / PoS；
- Currency wealth 决定 Validator 权重；
- 全局 block 总序；
- 全局 transaction total order；
- 把 Transfer 改成固定 Currency input 的 UTXO 模型；
- 为 Currency 强制引入独立 version chain；
- owner commitment 的具体密码学格式；
- ZK ownership proof 的具体方案；
- one-time ownership key；
- DAG / block / chain 必选结构；
- RecoverySet 的具体治理规则；
- Validator 现实世界“一人一身份”的治理实现；
- 为未发布旧快照/旧网络格式建立兼容层。

这些内容只有在出现真实协议需求并完成明确决策后才能加入。

---

## 21. 当前开发边界与部署验证

当前 CLI 主链路已接通 Genesis provision、Validator node、signed LegalTask submission/status、局部 identity allocation、私有任务 BFT 和自动业务 commit、membership/key rotation、shared recovery，以及显式 public checkpoint 发布和长期公开节点认证同步。完整状态核对与实现边界见 [开发完整性清单](docs/development-status-2026-10-03.md)。测试通过不是整个系统开发完毕的证明。

已补齐 membership/recovery source 的 durable admission/restart、恢复候选与业务前进的收敛，以及超过公开 32 条响应边界的本地 Validator endpoint retention。仍需核实外部业务系统的实际集成验收；当前部署上限 56，任意更大或累积 retained-set 拓扑仍需容量规划。

业务接入工具现已提供 `authorizer-keygen <key-file>` 与 `transaction-sign <authorizer-key-file> <unsigned-json> <signed-json>`，后者复用 transaction 的统一 payload 校验、LegalTask canonical 签名及现有 wire size 限制；文件创建复用既有私有文件 helper，拒绝覆盖。Authorizer 公钥仍由部署者配置，没有新增账户密钥绑定或在线授权变更入口。`examples/submit-transaction.ps1` 有限轮换 certificate-pinned 端点、精确请求查询/重试，只以 succeeded 作为完成依据；unknown、bound、超时或耗尽预算不能冒充失败终态。接入步骤、状态边界和真实回归证据见 `docs/business-integration.md`。

独立未决设计包括 RecoverySet/现实身份治理、owner 隐私证明，以及真实部署需要时的 DNS seed/DHT/NAT traversal；不能由实现自行猜测。当前显式批量发布 public checkpoint，不把每笔普通交易增加一次公开状态共识。


---

## 22. 开发约束

Second 当前未发布，因此开发遵循仓库 AGENTS.md：

- 不制造未发布兼容包袱；
- 不重复造轮子；
- 不写无价值重复测试；
- 不制造 God File；
- 发现当前修改路径上的重复真值或错误结构，应直接收敛；
- 协议未决定的问题不能由实现者擅自替用户决定。

---

## 23. 总体模型

Second 可以最终概括为：

~~~text
                 Signed LegalTask
                        │
                        ▼
                 验证授权真实性
                        │
                        ▼
              绑定永久 Task identity
                        │
                        ▼
            判断当前是否允许开始执行
                        │
                        ▼
              按原始 operations 顺序
                        │
           ┌────────────┴────────────┐
           ▼                         ▼
       全部成功                  任一失败
           │                         │
           ▼                         ▼
   提交完整业务状态             不提交部分业务状态
           │                         │
           │              保留必须永久存在的协议事实
           ▼
    记录 task success
~~~

所有实际资产变化最终都还原为：

~~~text
Transfer:
Owner A -> Owner B

Issue:
∅ -> Owner A

Destroy:
unoccupied circulation -> ∅

Leak Repair:
destroy leaked identity
reserve identity takes leaked owner
new identity replenishes reserve
~~~

Second 的核心不是维护一个数字余额，而是：

> **把每一个货币单位作为独立资产对象，以所有权关系作为唯一真值，再在其上建立授权、顺序、原子性、幂等、并发、最终性、隐私和修复机制。**

### 2026-10-03 审计整改补充约束

ValidatorSetTransition 绑定 currency_frontier，并与 CurrencyAllocation 共用 `{validator_set_version, start: currency_frontier}` scope；成员切换是该 frontier 的 epoch seal，不再使用独立的 ValidatorSetTransition scope。两类候选复用同一个不可逆投票锁，避免旧集合分配与新集合启用同时成功。分配先成功则必须在新 frontier 重建 transition；切换先成功则待处理任务在新集合继续分配。PendingValidatorSafetyRecovery 保存 frontier 并用它重新验证 transition digest。

过期的新分配请求仍经既有 prepare 路径持久绑定 TaskId 后拒绝。来自网络的过期分配源，仅在本地已经持久受理相同请求或存在 exact scope/digest 的合法 finality certificate 时允许补齐，不能凭未验证 hint 绕过过期限制。

提交新 allocation 待办前，必须先通过既有 CurrencyAllocation 构造器的区间溢出与物化检查；失败不写入 restart 队列。已受理待办可能在其他区间获确认后无法再分配（例如 frontier 耗尽），恢复编排记录有界拒绝诊断并清除该请求的 allocation_task 待办，继续处理其他任务；保留不可变 TaskId/request digest 绑定，不回滚 frontier、证书、签票锁或已分配区间。持久化错误仍向上传递并 fail-closed。分配已获 QC 后业务准备失败也不回收地址；精确重试复用原区间，不能因为失败重新分配。没有新失败结果表，查询 bound 仍不能冒充永久失败。

恢复 allocation 的候选注册达到既有 64 候选上限时，仅延后该持久待办，不能把正常资源限制升级成整个节点退出；在已有区间提交或集合切换后复用原恢复入口再尝试，不增大窗口，不另建队列/真值，不新增扫描定时器。提交响应若在 durable 入队后因候选窗口满而拒绝，仍不能据此推断该请求从未受理；精确请求查询/重提继续遵守已有 bound 语义。

快照提交引用在独占 store lock 下直接覆盖固定 80 字节记录并 sync_all，不依赖 Windows rename 的持久性。读入必须验证引用自身 checksum；中断造成的引用撕裂会拒绝加载和签名，不能回退旧 mirror。

恢复节点跨过后续集合版本时，若本节点 credential 未变，保留首次独立 rotation quorum 的 pending safety evidence；原 rotation 的目标集合直接引用 durable transition proof，不保存第二份目标集合。加载、写入和解锁时验证原 rotation/certificate 与当前 credential 的关联；仍须匹配当前集合的认证 recovery proof 才能解锁，并将 minimum signing version 提升到当前版本。退休或再次轮换不得沿用旧 pending evidence。

双平台部署复用同一 node 加载与运行路径；node-check 不驱动共识，系统服务停止只取消运行并释放资源，既有原子提交与重启恢复继续负责 safety。Windows SCM 与 Linux systemd 状态、日志、服务账户和包校验属于操作层，不成为 membership、业务成功或签名安全的协议真值。运行错误按宿主 service manager 策略重启，单节点不另造 supervisor 循环。见 [部署交付](docs/deployment.md)。


### 已认证任务取消与恢复（2026-10-04）

TaskBinding 使用 Pending / Succeeded / Cancelled 三种互斥结果；Cancelled 只记录确定性的 Abort FinalityStatement，不保存签名排列不同的 quorum votes 为共享真值。Abort 摘要域分离并绑定协议版本、TaskId、request digest 及 exact ValidatorSet 的版本和全部成员凭证。该 statement 在原 PreparedTask scope 中与 Commit 共用 round、QC、锁与最终签票；sign_prepared_abort 必须得到同 scope 的 precommit QC readiness，不能使用新的取消 scope 或永久 veto。

StateStore::install_prepared_abort 在持有 store 锁时验证本地活动计划的原委员会、确切 statement、quorum 签名及终态/最终签票冲突，然后一次原子提交取消结果、移除活动计划和对应 payment execution、关闭分配待办并清理该 scope 的 BFT metadata。已有成功或 Finalized Commit 拒绝取消；任何不同结果的不可逆最终票也拒绝取消。连续地址区间、请求绑定和最终签票锁保留。有效同结果证书重放不增加 generation。PreparedTaskBook::abort_certified 在 durable 安装后重载现有权威 claim book，不先释放资源。

共享恢复仅增加取消终态确实引用的历史委员会，每个版本编码一次，供取消 statement 绑定校验和旧证书重放；本地活动占用和签票 metadata 仍不共享。签名只在证书输入时验证，快照保存确定性终态并校验绑定，避免每次读取重复 quorum 验签。历史委员会经过 registry 校验，额外、重复或缺失版本拒绝。

运行时可恢复本地已认证 Abort QC / finality-ready / 最终票，恢复最终票时直接重发原结果，不重启另一种决策投票。持有活动计划的节点只凭 exact 委员会的合法 QC、携带合法 prevote QC 的提议或最终证书准入远端 Abort 候选；裸 Abort 提议和单票不能获得取消资格。收到最终证书后原子安装取消并发出完成事件。

当前仍没有从只读冲突证据到首次 Abort 候选的完整自动准入，也没有未持有计划节点的取消签票上下文和跨委员会待决资源交接。因此这里交付的是认证决策恢复与释放路径，四节点 2∶2 的自动冲突活性验收仍未完成。

恢复决策优先级进一步明确：已观察到的 precommit QC readiness 或已投不可逆最终票优先于较早的另一结果 prevote 锁；两种方向均由运行时恢复回归覆盖。恢复只按本地 validator + scope 索引读取 metadata，不逐任务扫描整个签票库。批量快照校验同版本取消绑定时复用临时 SHA-256 委员会前缀，每个委员会只处理一次成员凭证，不增加持久摘要真值。

## 2026-10-04：待决资源交接与恢复 fence

成员提案可通过 `StateStore::prepare_validator_set_transition` 绑定必要冻结计划的 canonical 根；公开 source/proof 仅携带根，本地持久候选保存正文。候选受理前必须覆盖所有本地活动计划和上次认证交接的未完成义务，受理后在 store 锁内拒绝新增任务计划；成员最终签票再次验证覆盖。正文复用唯一 prepared codec，临时资源索引复用既有 claim restore，不作为业务所有权或另一套状态账本。

激活仅接受证书所认证的根，不能在激活阶段给旧证书补摘要。已认证正文成为 shared state 的资源 fence；恢复载荷附带对应 membership proof，安装后复用既有 validator_transition_proofs 保存。恢复检查点对 canonical shared state 签名，附带 proof 独立验证，不把 quorum 签名排列作为业务真值。恢复节点不继承普通 PreparedTask 的 Commit 权或旧 signing safety metadata，但同资源请求仍被 fence 拦截。原 exact 集合的 Abort 最终证书可解除 fence，当前集合的取消票不授权释放；运行时被动消费这类最终证书，不产生旧集合签票。

当前正文只接受本地已验证或认证继承的计划。跨节点正文合并、按需传输、首次冲突 witness 准入、Abort-only 签票与释放后自动重试尚未接入，真实 2∶2 冲突仍不自动收敛。不能将本阶段的恢复 fence 回归当成完整仲裁交付。

## 2026-10-04：交接业务基线及原委员会绑定

成员交接根同时绑定 canonical 冻结计划和从唯一 SecondState 派生的业务基线。基线保留业务状态、终态请求绑定及已确认地址区间，排除本地未确认 Pending 绑定、payment prerequisite、上一份交接正文与 allocation_task 待准备进度。没有活动计划但存在终态或确认分配时，仍必须认证根；相同分配的不同合法 quorum 子集及本地准备进度不改变该根。首次受理检查当前基线，已持久受理的确切正文允许其已覆盖任务完成后继续认证。未知远端计划不能借此进入本地上下文。

继承的未决 TaskId 固定原委员会版本，普通准备和隔离冲突验证共用 origin 检查；相同请求不能被重新准备成当前集合的任务。真实 QUIC 恢复 provider 的首次发布和启动恢复都从完整 persisted snapshot 构造 payload，携带当前交接需要的既有 membership proof。正文中的临时验签缓存绑定重新计算的正文根与 proof 字节，不持久化；被克隆的缓存也不能授权变更后的证明。

以上实现尚不包含跨节点交接正文合并/按需传输、新成员业务基线同步与签票准入，也不包含首次冲突 witness 的 Abort-only 签票路径。真实四节点 2∶2 自动仲裁仍需完成这些路径后验收。

私有冻结源拉取的 round hint 只推进相同版本/摘要的公告轮次，不重置已收正文。过期来源或目标的响应不进入验证/候选路径；当前来源的重复分块必须逐字匹配已收数据才忽略，错误边界或数据仍拒绝。source-unavailable 同样绑定请求的版本、摘要和来源，不能干扰另一候选。忽略旧响应不延长 deadline、不新增重传或持久真值。

正文获取及公告重试沿用 proposal 间隔，但等待中的绝对重试期限在无关共识活动后保持不变；到期显式触发既有 reset_attempted 重试，重新开放已耗尽的来源轮次。调度将该期限与已有 BFT deadline 合并，到期重试优先于已排队的活动通知，避免每次活动重新创建相对 sleep 而无限推迟正文获取。不存在 pending sync 时不设置正文计时唤醒；不新增后台循环、网络 deadline、存储真值或广播范围。

Validator 出站有界队列的 Full 与 Closed 必须区分。Full 拒绝当次入队并返回背压错误，但不将仍存活的 worker/认证连接标为死亡，不删除 peer 或主动断开；既有正文重试与共识重发负责后续尝试。Closed 仍标记死亡、移除并关闭连接，释放原连接 permit。broadcast 与 send_direct 复用 ManagedValidatorBftPeer 的唯一入队判断及普通/终态队列选择；不增加队列容量、无界缓存、后台重试或传输期限。

本轮首次冲突仲裁接入进度详见 docs/conflict-arbitration.md 末节。PreparedTask 的本地权限位区分真实资源持有者与无占用的已验证竞争计划，canonical 交接正文不携带本地选择或 Commit 权限。现有 task scope 内互斥决定 Commit/Abort，原委员会、QC 与最终签票锁不被 TaskId 优先选择覆盖。真实四节点终态验收仍在执行，尚不能声明自动冲突收敛完成。

首次自动仲裁已取得真实 QUIC 终态证据：当前集合的三类不同 TaskId 2∶2 冲突，经完整源受理、同 scope Abort 认证、原子释放和事件重试后，一个 Commit、一个 Cancelled；不足 quorum 的中途重启保留占用，恢复四节点后也收敛。测试范围与未完成的冻结变体/QC 优先活性/跨成员交接路径见 docs/conflict-arbitration.md 最新验收节，不扩展为完整协议交付声明。

已有 Commit QC 的局部冲突恢复沿用任务 ConsensusScope 和 BftLocalState：QC 优先于 TaskId，最终签票/readiness 优先；无资源观察者不可签 Commit，认证释放后以原冻结源取得资源。资源占用中的本地合法提交现在完整验证并持久受理为 AlreadyPending。实现、验收及未覆盖边界见 docs/conflict-arbitration.md 的 2026-10-04 Commit QC 节。

2026-10-05 补充：错过早期 Commit QC 的已验证竞争上下文可使用 exact 委员会最终 Commit 证书保护既有 BFT readiness；资源取得仍复用原准备器，未授权节点不可签 Commit。已完成/认证取消的任务直接走终态处理，Pending 的 Some(false) 不视为终态。完整验收及未覆盖边界见 docs/conflict-arbitration.md。

同一任务冻结候选沿用一个 PreparedTask 和一个任务共识范围：候选共享签名请求，摘要仅绑定对应操作；选择另一候选不释放旧候选资源，恢复重建所有持有资源的候选并集。签票只允许已验证、已取得资源的摘要，最终证书执行唯一候选。交接正文按候选摘要展开，排除本地选择、候选到达顺序和投票阶段；覆盖校验保留全部候选义务。跨节点正文并集和新成员业务基线同步尚未接通；具体进度见 docs/conflict-arbitration.md 最新节。

迟到的已验证 Digest prevote QC 也是后续提案所需的最高证明，运行时不得将其与过期单票一同丢弃。仅在候选来源已验证、原委员会验签通过后复用既有最高 QC 存储；不回退轮次、不改变锁、不补签当前轮、不刷新 deadline、不新增广播或证明真值。

受阻的替代冻结源经现有隔离准备器完整验证后，保存在同一个 PreparedTask 中且自身 commit_authorized=false；它不继承主计划的签票或资源权利。认证资源释放事件与启动检查重试其原冻结来源，取得全部所需资源后才设置对应候选权限。该路径与证明选择的组合边界仍在实现，详见 docs/conflict-arbitration.md。

无资源替代候选的 Commit prevote QC 使用既有 BFT 证明和局部仲裁入口保护，按认证摘要对应的操作计算资源冲突。证明不授予资源或签票权利；取得资源前暂停旧候选会话并阻止启动回退。最终证书直接到达和其他未完成边界见 docs/conflict-arbitration.md，不将 QC 保护扩展声明为完整多候选活性。

未知任务候选的 prevote/precommit QC 或最终证书通过原委员会验签后，可以触发既有有界冻结来源拉取，但不授予资源或签票权利。在线完成缓存保留确切认证 Commit 的来源，按版本/scope/摘要匹配后沿原接口按需分块提供；沿用完成 scope 上限，不复制旧候选或维护第二份业务真值。重启后来源及缓存淘汰后的认证基线恢复尚未实现，详见 docs/conflict-arbitration.md。

上述来源缓存现已收敛为与 Commit 业务写入原子保存的本地 task_receipts，保存确切 finalized 计划和必要的原地址分配证明，按提交 generation 限制最新 64 项及 128 MiB。记录无资源/签票权利，不进入共享业务/交接摘要；确切原委员会验签后才可使用。启动恢复既有的限速终态证书回应，来源仍只沿既有按需分块接口传输。缓存淘汰后的认证基线与迟到请求接收安装尚未完成，详见 docs/conflict-arbitration.md。

最终 Commit 证书的争用保护按其摘要解析确切冻结候选，覆盖主候选有资源而认证替代候选受阻的情况，不要求本地先收到 prevote QC。原委员会验证后使用认证候选的冻结来源重新准备，证书本身不授予资源或签票权利。旧保留资源仍待认证终态释放；跨任务交叉保留资源和无持有主计划的多候选路径尚未完成。

已受理候选的认证选择互不冲突时，Commit 保护允许等待另一任务保留的未选候选资源，保留既有占用，不强迫认证任务 Abort。双方实际认证选择仍按同一资源索引检查，真正冲突拒绝；不同不可变 finality 选择拒绝。Commit 与 Abort 均通过既有终态事件唤醒受影响等待者，资源取得仍须完整重新准备。认证正文尚未受理的交叉上下文和无持有主计划的多候选仍需实现，不能由此宣称全部冻结计划收敛已完成。

已受理候选的环形旧占用通过局部认证依赖闭包提交恢复：完整最终证书保存在原 finalized 计划上，候选位置不决定资源或签票权限；缺少依赖证书不执行或释放。闭包全部认证且实际选择互不冲突时沿现有业务快照一次原子提交，恢复记录与业务终态同时写入。冷恢复和真实认证 QUIC 证书补齐路径已有回归验证；Finalized 选择与持久化 precommit readiness 的相反摘要在两个到达方向均拒绝。详见 docs/conflict-arbitration.md；未受理正文、跨成员基线和状态改变后的迟到源仍未完成。

受阻候选正文完成受理时，优先处理已缓存的确切最终 Commit 证书，复用原委员会验签、资源保护和依赖闭包执行；不再仅检查 prevote QC 而等待证书再次传播。正文尚未验证时，缓存证明仍不授予候选资源或签票权限；历史源资格和跨成员基线不由此规则替代。

多冻结候选的受理不要求主计划已经持有资源，纯见证上下文也能验证其他冻结来源并由最终证书选择。无资源主见证重新准备取得资源时保留原候选列表；任一候选已持有资源或任务已 Finalized 时，普通准备不得覆盖现有上下文。各候选资源/签票权限仍独立校验，历史状态变化和跨成员来源安装另需认证基线。

成员交接的业务摘要仅在规范空 genesis（frontier=1）可省略。即使没有任何任务绑定，已有账户、支付地址、货币/储备或不同 frontier 也必须纳入基线承诺；认证切换不能在不同业务基线上激活。新成员基线正文传输与安装仍需实现。

紧凑成员源无法由本地义务重建 handoff root 时，沿既有认证分块拉取取得交接正文。原委员会、成员源/正文摘要和同一业务基线匹配后，尚未受理来源必须通过签名授权与唯一准备构造器验证；只合并无资源见证，保留本地原权限和证明。正文必须覆盖本地全部候选及继承义务，不允许通过遗漏正文移除旧占用。传输不等于新成员业务基线安装，也尚未完成签票前的分布式义务并集收集。

完成成员切换后，交接正文提供复用快照中已安装的 handoff 与原委员会认证切换证明，按原版本、前沿范围及成员源摘要匹配，不依赖已清除的 pending transition，也不按当前业务状态重新构造旧交接。冷恢复后可提供相同正文，不新增历史正文副本或后台扫描；仅提供当前已安装交接，后续切换覆盖后的旧正文留存及接收端认证业务基线安装仍未完成。

认证的 validator 出站连接建立后，复用当前持久快照向该 exact-set 成员补发最新公开检查点证明（或其基线）及恢复检查点证明；接收方沿原证明验证与安装入口处理，不要求操作者再次发布。没有证明、旧委员会证明或不属于当前集合的双方不会触发该补传，不新增定时扫描或再共识。validator 连接维护按最多 4 个同时进行的拨号处理已知候选，沿用全局连接 permit、原认证和传输期限；离线候选不阻塞后面的健康成员建立连接。治理请求的已连接 quorum 门槛保持原规则，不通过取消 Busy 校验掩盖连接不足。

治理请求响应先区分身份授权失败，再报告连接 quorum 不足：错误身份始终 Unauthorized；合法身份在 quorum 尚未连接时仍 Busy，不创建治理候选或签票。就绪状态仍通过原入口刷新权威并计算一次，三类治理请求复用既有身份验证器，不新增平行规则。

交接和争用正文验证复用本地已持久受理请求的时间资格：必须匹配完整签名请求、request digest 与原委员会版本，才能在截止时间后验证另一份冻结候选。它只延续原请求资格，不跳过当前业务约束、确切操作摘要或资源/签票权限校验；Pending 请求绑定和远端提示都不能单独证明受理资格。首次收到的过期请求继续拒绝，整份交接验证失败时不保存先前暂存的候选。

已原子安装并由委员会证书认证的交接还为其中精确 TaskId/plan digest、完整签名请求和原委员会版本匹配的冻结正文保留截止资格，即使新成员尚无本地 PreparedTask；未列入的候选不继承该资格。准备仍验证原集合，持久写入保留实际活动集合并检查调用方交接与持久交接一致、原集合确切留存以及原业务/prepared CAS。这允许无关业务前进后的精确历史正文准备及旧 finality 证书提交，不授予新成员历史签票权，也不代表资源已改变的所有历史路径已经实现。

运行时对已安装交接中的精确历史 Commit 候选，若正文已取得本地提交资源权利，且本地成员不属于精确留存的原委员会，可以被动应用原委员会终态证书，复用 PreparedTaskBook 的唯一 finality 验证、原子提交与有界 CAS 重试。原成员继续由现有共识会话处理证书及完成清理。新成员不为该继承请求启动历史投票会话；启动恢复与收到正文后的路径均遵守该限制。证书不足或签名无效仍拒绝且不改变持久状态；资源未取得、未知候选与未继承任务不能走该被动提交入口。认证正文网络获取和资源已改变的历史路径仍须另行闭合。

Validator 连接维护的失败沿既有有界诊断队列记录 ConnectionFailed，包含目标 NodeId、端点、拨号排队与连接耗时、原始错误；它是本地诊断，不进入 wire、共识 scope 或共享业务真值。只在失败时记录，成功连接不增加诊断事件，不新增采样循环。该证据用于区分尚未建立连接与已建立连接的发送失败，不以错误分类替代稳定性根因修复。

Validator 维护与公开 peer 维护在同一节点生命周期内并行等待，公开 bootstrap 的无响应端点不再推迟下一轮 Validator 重连。两条路径各自沿用原 2 秒维护间隔、拨号预算、连接 permit、认证与请求期限；公开路径保留原退避。非 Validator 节点不启动 Validator 维护循环；任一路径返回致命错误时，节点统一取消其余运行路径，不产生脱离节点生命周期的维护任务。

LegalTask 提交只验证一次原请求签名和编码大小；业务状态与 PreparedTaskBook 从同一持久快照取得，任务簿沿唯一构造器恢复资源与 lifecycle claims。若受理期间并发写入使原子比较返回 StaleState 或 StalePreparedTasks，入口最多重新读取三次当前快照并重新走相同业务受理/共识注册路径，不重用上轮冻结计划、资源占用或业务判断。其他授权、业务、容量、文件系统及安全错误不重试；持续冲突仍返回原错误。持久化的预期状态/任务集合比较保持不变，不覆盖并发 finality 或修改不可逆签票规则。

当前 TaskHandoff 在业务摘要旁保存该摘要所承诺的规范化业务正文，作为当前交接锚点的不可变恢复数据。正文复用唯一 SecondState 编码，排除前一交接、payment prerequisites、未认证 Pending bindings 及 allocation retry source；终态绑定、已认证地址区间和完整账户/支付地址/货币状态保留。只有规范的空 genesis 省略正文和摘要。后续业务提交不更新这份基线，后续交接捕获新基线而不嵌套旧基线；不另建可变业务真值或无限历史表。

交接解码先核对正文与业务摘要，再复用 SecondState 解码器检查规范编码和上述排除规则；基线解码在进入嵌套 TaskHandoff 前拒绝其标记，不能形成递归解码链。正文计入现有 512 MiB 交接总限额、同一私有来源分块与不可变编码缓存，不逐块重复生成或哈希基线。基线留存不授予投票权，也不等同于新成员安装完成；安装仍须验证原委员会的成员切换证明、可信成员链、全部交接正文及本地签票安全条件。

`StateStore::install_validator_handoff_baseline` 是交接基线的原子共享状态安装入口。调用方提供本地可信的委员会/registry 快照锚点、成员切换 quorum proof、完整交接正文及本地 AuthorizerSet；锚点不能来自同次未经认证的 peer 响应。入口验证 admission/rotation、原 certifier set、交接根、正文基线/frontier、任务请求签名与绑定，再沿既有 registry 和 retained-set 规则登记下一集合与继承义务。原 quorum 验证沿已存在的 exact-root/proof 缓存复用；规范空 genesis 无 installed handoff 时仍单独验证原 quorum，不能绕过证书。

安装只接受空目标，在同一 StateStore 排他锁下全部验证后写入一次提交；不导入 PreparedTask 资源权利、vote locks、BFT rounds 或共享检查点 serial，不覆盖已运行节点。与 checkpoint recovery 共用唯一初始共享状态写入器，始终设置 safety locked 和下一集合的 minimum signing version，保留原切换证明以供持久追赶。从空目录重建同一密钥不证明没有旧签票，后续接入不得直接把 safety 改成 ready。

`client_fetch_validator_handoff` 先以本地可信旧委员会/registry 验证切换证明，再以被认证下一集合的 identity key 请求安装中的不可变交接正文。复用现有恢复分块消息、唯一签名布局与有界收集器，使用独立 handoff 签名 domain；签名绑定实际 QUIC channel、根、偏移与限额，checkpoint 用途签名不能获得交接正文。提供端只服务认证 next set 等于当前活动集合的已安装交接，不依赖恢复检查点或尚未初始化成员的 BFT listener。8 字节正文长度计入分块偏移；总正文上限沿用 512 MiB，分块按请求长度填充，拒绝截短或无进展响应。后续业务提交不能替换旧委员会认证的基线。规范空 genesis 可由已验证切换证明重建，无需私有正文请求。

公开切换证明查询使用专用只读会话；查询结束后必须在新连接上认证正文请求，不将该会话升级为私有数据会话。真实节点回归覆盖公开证明查询、跨帧下载、错误签名/跨用途签名拒绝及原子安装，并确认提供端已推进业务而目标仍获得原认证基线。`handoff-install` 操作入口复用该下载器和原子安装器，从本地可信旧委员会/registry 锚点查询切换证明，使用下一集合的本地 identity key 请求正文，并沿本地 Validator config 的 AuthorizerSet 验证任务；所有成功安装保持 locked。规范空 genesis 复用证明查询的连接对象完成本地重建，不再握手或请求私有正文。入口与 node/recovery-install 使用相同目标 runtime 排他锁，只接受未初始化的完整目标，不覆盖已有状态；证明/身份的私有请求前验证仍由下载器唯一负责。委员会义务并集、跨多次切换的追赶及完整历史资源变化处理仍待完成。

迟到正文的冲突见证受理复用普通冻结正文的原委员会/当前持久化集合判断。只有当前已安装交接中的精确 TaskId、plan digest 与原版本，且该交接等于实际持久快照、原集合等于实际活动或精确留存集合，才能将原验证委员会与当前持久化委员会分离；未列入的历史任务仍拒绝。正文继续通过唯一业务构造器、签名授权、摘要校验和实际 blocker 判断，见证不导入资源权利、投票锁或签票许可。原委员会合法 Abort 解除继承 fence 后，存活候选可沿既有准备/Commit 入口取得权利并提交，保留当前集合与无关新业务；没有终态证明不能释放。这只覆盖原认证业务条件仍成立的历史冲突正文，不替代业务条件已失效时的完整历史处理。

运行期间认证依赖组件提交后，完成记录恢复入口同时报告对应的 `CertifiedPreparedTask` 事件，证书来自已验证的持久 receipt。沿已有 recent_completed 和原会话 certified_emitted 判断避免重复报告，不新增事件历史、持久化标记或扫描；重启时恢复旧 receipt 仍仅恢复转发记录，不重放业务完成事件。事件报告不授予资源或签票权，也不替代原子提交与证书验证。

交接收集与最终准入使用同一正文验证和见证持久化实现。`PreparedTaskBook::collect_transition_handoff` 验证当前委员会、registry、前沿、正文根及业务基线，独立重建尚未受理的 signed request 与 frozen selection；再由唯一 `TaskHandoff::capture` 对本地、远端及继承义务生成规范并集。远端不能借已有本地摘要跳过正文一致性检查。校验及并集构造全部成功后才原子写入请求绑定和无资源见证，保留原本地权利、phase 和证明；相同并集重复收集不重写。返回的并集是新的候选，不能冒充原根的最终性证据。

收集入口将 `CollectingTransition` 与请求绑定、无资源见证在同一快照事务中落盘，复用既有有界 pending governance map 和成员源/正文编码。收集记录没有可注册的 BFT target，普通重启恢复跳过它；签票与注册前的唯一 governance subject 校验也拒绝同 scope 尚在收集的候选。相同切换意图的后续贡献更新原记录，不堆积旧根或保留第二份正文真值；重复贡献不重写。收集阶段关闭普通新增准备，只允许经同一验证器校验的贡献扩大并集；已进入最终准入的候选不能降级为收集或再扩大义务。

交接分块前缀及紧凑成员源公告都携带严格的收集/最终准入阶段标记，未知值拒绝。冷恢复提供端可传输收集中的正文，接收端沿同一准入器合并并持久保留收集阶段，不因下载完成注册 BFT；内部格式仍为当前开发版本 1。正文根由唯一 with_handoff 构造器计算并校验，规范空 genesis 沿相同规则使用缺省承诺，不重复哈希正文。

`NodeRuntime::collect_validator_set_transition` 启动自动贡献交换。接收收集公告时，先持久化本节点的完整义务并关闭普通新增准备，再按需使用既有认证分块 fetch 拉取对方正文；scope/版本/前沿不匹配的公告不进入 fetch。新根只在持久化成功后公告，重复受理不回声重播。重启后的 resume_governance 重新公告原收集记录，未完成交换复用既有来源重试的绝对期限；更早的普通 BFT timeout 不提前重传或重置该期限。无收集或来源待办时不为此启动定时唤醒，不新增正文仓库、重试循环或全网 gossip。

精确根 quorum 确认后的阶段提升仍未接入；现有 operator/直接切换共识启动入口尚未全面改走收集阶段。自动交换入口只收集，不完成成员切换，不能记作交接总目标完成。

普通冻结任务正文的受理从同一共享快照取得业务状态、准备记录及活动/留存验证委员会，再沿唯一 prepared 构造器校验与 CAS 写入。正文解码与业务签名验证只执行一次；仅 StaleState/StalePreparedTasks 最多重读重试三次，其他错误立即返回。重试重新验证最新业务条件及历史来源资格，不扩展旧任务准入或签票权。该行为覆盖下载、认证冲突正文和资源释放后的重试调用，不新增正文缓存、后台循环或网络重传；交接与分配正文仍走各自既有入口。

完整终态证书先到、正文在请求期限后到时，普通任务与地址分配复用同一待办证书资格判断。只有确切 scope、摘要及原验证委员会的合法 finality quorum 才保持原时间资格；单节点来源公告、其他摘要、未经认证的收集根或其他集合证书不构成例外。正文仍由唯一构造器重验签名、冻结选择、业务条件、资源及历史资格。普通任务正文受理后若已有确切终态证书，沿既有 finalize_prepared_task 与认证依赖闭包直接推进，复用 receipt 完成事件与有界证书回应，不新建投票会话、重做共识或增加证明副本。受阻正文仍走原见证/依赖恢复，未知资源和业务失效不因证书而放行。

通用 BFT 预投票/预提交的共同持久化 value 校验同样拒绝当前收集记录的根，防止调用方绕过切换专用 signer 或 runtime 注册入口。拒绝发生在签票状态写入之前，不创建 round/vote lock；其他业务 scope 与既有候选的终态推进保持原安全规则。

切换封存后的原子准备写入禁止新增 TaskId 和已有 TaskId 的新冻结候选。判断复用计划身份与完整 operations 相等关系，避免每次阶段推进重新哈希全部候选；既有候选的 phase、权利、终态推进及删除不因此阻断。检查与快照写入持有同一 StateStore 锁，避免收集期间并发封存绕过限制。

认证 BFT 接收回调为异步接口：消息经过确切委员会与签名校验后，等待既有有界 inbound 队列容量再入队并唤醒共识。队列满只施加背压，不把健康会话当作传输失败；接收器关闭仍终止会话。容量、并行 QUIC 读流上限与认证边界保持原约束，不增加缓存、轮询或重试循环。

认证交接安装后的新成员可接收被该交接义务绑定的原委员会任务终态证书。BFT authority 从既有认证 handoff 和完成 relay 派生 terminal-scope audience，复用原集合解析器；仅 PreparedTask 的完整 FinalityCertificate 可投递到当前集合的新成员，发送者、证书签名仍按确切原委员会验证。普通投票、终态单票、正文以及既未列入认证交接也不在既有完成 relay 中的旧 scope 保持原委员会接收边界，不扩展新成员的旧签票权限。历史 Abort 在 Commit 正文分支之前识别，原 quorum 通过唯一原子 install_prepared_abort 释放继承围栏，本地已受理正文和当前业务推进不阻断该终态结论。

新成员收到继承任务终态证书时，先检查已安装交接，再决定是否启动来源下载。对于交接内确切 Commit 摘要而本地尚未准备的正文，先验证原委员会终态 quorum，再把交接中的原编码正文送回唯一普通来源受理路径，重验签名、冻结选择与当前业务、资源条件。原子 finalize 和认证依赖闭包复用现有 receipt、完成事件及去重，不启动旧签票会话，也不再向旧委员会下载已安装的正文。非法证书、未列入交接的任务和无认证根的正文不能走此路径；业务失效仍拒绝，不能因历史证明绕过当前业务约束。

继承终态恢复以确切冻结候选为粒度，不以 TaskId 存在为完成正文受理的标志。同一任务已有其他候选时，认证交接内的缺失候选仍先验证原终态证书，再沿唯一来源受理路径恢复。共同 admit_frozen_variant 复用既有 persistence_validator_set：候选按原委员会与原请求验证，只有匹配已安装交接的确切根和原集合才用当前委员会原子持久化；不回滚活动成员、不扩展未知旧来源，也不授予新成员旧签票权。
委员会切换后，新成员可验证并被动提交确切交接根内的原委员会终态，但不会因此获得原委员会的发送身份。BFT 广播与定向发送入队前共用消息来源集合的本机成员资格及接收方资格检查；无权发送的历史回应不得进入发送工作队列并导致当前认证连接关闭。实际发送和接收仍完整验证原集合身份与签名，不扩大旧投票或私有正文的传播范围。
认证 BFT 会话接收失败沿既有 ConnectionFailed 事件保留传输身份、实际远端地址、会话耗时与原始错误；不能只关闭会话并让发送方留下 connection lost/reset 的二手症状。成功返回的会话关闭不新增失败事件，背压等待仍沿原有有界通道，不改变拒绝或认证规则。
尚未投票的交接收集根随已认证业务和货币前沿推进，在共同快照写入内原子更新。只有当前委员会的 CollectingTransition 可重新捕获当前基线及仍未完成的候选；已进入共识的 Transition 不改根。收集意图保留下一委员会、准入和轮换内容，原任务证书、投票锁及本地准备权保持原规则；前沿推进不能静默丢弃尚在收集的意图。捕获复用唯一 TaskHandoff 规范化实现，已受理来源提供的当前根直接复用，仅状态或准备集合改变时刷新，无新增后台循环或额外快照提交。

已成功任务的迟到正文仍通过共同解码与授权签名验证，再与唯一持久 receipt 的原委员会版本、完整签名请求、计划摘要及原正文冻结选择逐项核对。确切重复无写入返回成功，不重建准备权、旧签票会话或完成事件；缺失 receipt 或任何不匹配继续拒绝。该路径支持委员会切换后仍留存的原任务上下文，请求过期不使已认证完成的确切重复失效，不放宽未知历史来源准入。

QUIC 请求超时保留具体阶段：连接认证、开请求流、发送请求、等待响应、读取消息或等待交付确认。exchange 的开流、发送和接收使用同一个绝对期限，总预算保持五秒，不能通过分阶段计时延长期限。只扩充已有 NetworkError::Transport 文本，不增加诊断队列、后台任务、重传或传播正文。该诊断不证明未定位的间歇超时已经修复。

运行期交接收集从同一持久快照取得业务状态和准备集合，复用 PreparedTaskBook::from_tasks 恢复既有本地权利。共同 collect_transition 对 StaleState/StalePreparedTasks 最多重读重试三次，每次重新检查当前成员身份、正文基线、冻结候选和原子 CAS；其他错误立即返回。该边界覆盖本地启动和远端贡献，既有无写入重复与无资源见证规则保持，不新增重传、后台循环或并行状态。

冻结来源同步沿已认证当前委员会及货币前沿淘汰过期 CurrencyAllocation 的 fetch 和 announcement：来源委员会版本较旧，或同版本 start 小于已提交前沿时，不再需要本地正文下载。普通 PreparedTask、当前/未来前沿及未来委员会待办不由此清理。晚到公告、分块和不可用回应不重新建立过期待办；旧投票/证明仍交给既有共识校验和 relay，落后节点的旧正文请求仍沿既有提供端服务。清理在输入处理与认证分配/成员推进之后使用当前共享快照，不新增状态表、定时器或快照写入。

认证 BFT 入站消费以本次读取开始时的 receiver.len() 为上限，按 FIFO 取出已排队消息；持续并发入站不能把有界通道转成无界 Vec 或无限延长同一共识批次。后续消息仍留在原通道，沿原接收回调的 Notify 唤醒下一轮，不新增队列、丢弃、扫描循环、超时或并行工作线程。

外部业务提交的同步验签、持久化与共识注册在 Tokio 阻塞任务中执行，网络任务只接收正文并等待结果，不把存储或共识锁等待放在 QUIC 执行线程上。现有提交 permit 随事务移入阻塞任务并持有至返回；客户端断开或网络会话期限到期不能提前释放仍运行事务的名额。事务仍复用唯一 submit 路径和原子持久化，既有最大并发、授权、源大小和请求期限不变。已受理事务可能在回应丢失后完成，客户端继续沿原 TaskId 查询/重试，不靠撤销签票或回滚已写状态处理网络取消。

治理请求的身份、就绪、候选构造和共识注册在共同阻塞任务中执行，外部 QUIC 任务等待结果并发送响应。原活动连接 permit 随请求移入该任务，完成后随结果返回并保持至响应发送结束；取消等待不能提前释放仍运行工作的连接名额。三类治理请求复用唯一同步响应判断，不修改 Unauthorized/Busy 顺序、quorum 门槛、证书或注册规则。请求处理按职责位于 runtime_governance/requests.rs，不叠加到 BFT 来源受理模块。

交接义务并集不受未注册共识消息的 32 scope 窗口限制。已验证的交接正文沿现有规范编码总字节预算及快照大小限制受理；共同快照终态证明校验不按无权见证数量拒绝合法并集，编码与冷读遵循同一规则。普通冲突来源准入与未注册消息队列仍保持原容量限制。远端见证不获得本地资源、提交或签票权，收集阶段不启动成员切换投票。

节点共识 runner 的启动恢复、每轮认证输入处理/持久化/终态推进及到期来源重试按原顺序在现有 Tokio 阻塞线程池执行，每次完成后才启动下一轮；网络监督任务只等待结果和原通知/绝对期限。共识期限在同一轮同步工作结束时读取，异步等待不再争用同步 coordinator 锁；期间新增候选沿原 Notify 唤醒，不能丢失注册活动。验证者发现的权限刷新同样移出网络执行线程。NodeRuntime::run 以 Arc<Self> 持有唯一运行时，CLI 和现有节点调用共享既有 endpoint、存储、配额与共识状态，不复制业务真值或另建监听器。阻塞工作失败向监督入口传播 RuntimeTaskFailed，不静默丢弃；取消监督仍取消原监听 future，在途原子存储事务可以完成，不创建脱离监督的新监听任务。此隔离不改变认证、签票、最终性、队列容量或协议超时，也不意味着其他同步网络路径或原间歇故障已全部闭合。

已认证分配的业务恢复复用共同 submit_verified 路径，用同一快照恢复业务和准备集合，并沿既有有界 CAS 重试处理并发推进。准备及共识注册成功之后才清理本地 allocation_task；确定的业务准备拒绝可以清理，持久化或共识注册错误保留重启正文，不能将 StaleState/StalePreparedTasks 当成不可恢复业务失败。原签名请求、已认证地址区间和证明不重建、不重新分配。

任务状态查询的快照读取与状态判定在既有 Tokio 阻塞线程池执行，不能让文件锁等待占用 QUIC 执行线程。共同查询使用 StateStore::load_shared 的唯一已验证快照，不复制整个业务状态；请求及原活动连接 permit 一起进入阻塞任务，并随结果返回至响应结束，取消等待不能提前释放仍执行工作的名额。Unknown、Bound、Prepared、Voting、Finalized 和终态判定及请求摘要隐私边界保持原规则，不新增查询缓存、线程、轮询或协议期限。

认证验证者拨号先持久化确切远端身份和端点，成功后才将连接加入现有可发送集合并启动发送任务；发现循环不再在公布连接后另写一次缓存。缓存写入在既有阻塞线程池执行，连接 permit 随工作保持至返回，失败或取消不得公布无法从本地缓存恢复的连接。PeerStore 的认证记录、失败排序及过期角色移除先构造有界更新，沿唯一缓存编码/原子文件写入成功后才替换内存记录；失败保留上一份内存与磁盘状态，重复请求不能把未落盘记录误判成已完成。缓存容量、委员会认证与权利边界保持不变，不新增状态表或重试循环。

### 已受理但尚未冻结的交接请求（2026-10-06）

交接承诺除冻结计划外，纳入非终态 allocation_task 的完整原签名正文，以及上一份认证交接中尚未结束的此类正文；已出现同一正文冻结计划时只由计划承载，不重复保留请求。复用 LegalTask 的唯一编码、请求摘要、authorizer 验证和现有持久队列，格式仍为开发版本 1。请求正文不赋予旧集合签票权、不创建冻结资源占用、不延长过期时间。业务基线仍剔除本地重试进度；完整交接根必须承诺该正文，不能以基线稳定为由遗漏它。编解码按领域移至 task_handoff/codec.rs，请求受理与覆盖校验位于 requests.rs。

共同收集写入和新成员基线安装先验证正文及请求绑定，再原子保存；已结束的同摘要绑定不被复活。已有确切分配证明允许认证根恢复本地已清理的重试正文；温启动集合激活恢复非终态队列。封口后共同 save_with_prepared_and_collection 拒绝新增未覆盖请求，保持原 generation；已有正文及认证分配可继续推进。所有待分配恢复统一走 submit_verified，保留候选窗口、持久化失败与共识注册失败的重试正文，并沿原业务规则拒绝已过期请求，不能直接重建分配候选绕过期限。

行为证据：target/handoff-requests-reproduction.log 实际证明旧根删除未分配正文后摘要不变；target/handoff-requests-expiry-reproduction.log 实际证明旧恢复绕过期限。两处修复直接回归通过。target/handoff-requests-targeted-final.log 的 15 项交接及并发认证分配恢复通过；target/handoff-requests-collection-network.log 的真实认证 QUIC 收集验证异节点待办正文冷读完整、不新增准备、占用或 BFT 权利。原 frontier 测试要求封口后继续受理新任务，与完整根相矛盾，target/handoff-requests-allocation-targeted.log 保留该失败（其余五项通过）；调整为所有节点先受理、再封口，并验证新请求拒绝且没有写入，target/handoff-requests-frontier-barrier.log 通过。新成员原认证分块安装回归同时包含未分配正文；错误 authorizer、正文、证明与信任锚仍拒绝。

这是待办正文载体与恢复链路的补全，不等于集合切换已经自动推进。CollectingTransition 的 quorum 就绪、安全选根与自动进入已有成员共识仍未实现；冻结候选窗口上限、分配收敛间歇期限失败和检查点响应超时继续排查。本批最终两端完整门禁尚待验证，不以定向通过替代。

本批最终源码门禁结果：target/handoff-requests-linux.log 原生 WSL 的 fmt/check/clippy(-D warnings)/all-targets 全部退出 0，72 库测试、246 主集成、34 节点恢复（56.88 秒）及 examples 均通过，主集成 46.59 秒。Windows fmt/check/clippy 和 72 库测试通过，但 target/handoff-requests-windows.log 主集成 242 通过、4 失败、1 忽略，80.73 秒；失败为 exhausted/opposite allocation、65 项 backlog 和 business CLI retry budget，不能称两端门禁全绿。随后补跑未执行的 Windows 34 节点及 examples 退出 0（target/handoff-requests-windows-remaining.log，34 节点 48.53 秒）。

PID 32392 的私有失败现场保留；target/handoff-requests-failure-cold.log 使用正确完整路径和 t 前缀 TaskId 冷读：普通任务 1922 已成功，四个 opposite 节点任务 1801 均已成功且 frontier=6、supply=5；backlog generation=159、frontier=9、supply=7，仍远未完成 65 项；四个业务 CLI 节点 generation=37、frontier=5、supply=4、reserve=2。冷读不证明这些结果在原期限内完成，也未定位全部耗时来源。首次探测因路径带尾点报告无快照，错误命令日志另存 first-attempt，随后已使用 .commit 的确切后缀截取得到上述真实结果。全部工具过程已结束，文档后执行 git diff --check；总目标仍未完成。

### 2026-10-07：交接冻结候选不再受待处理消息窗口截断

真实持久回归 collection_persists_every_frozen_variant_beyond_the_message_window 构造同一 authorizer 签名转账的 65 个合法货币选择，编码并解码远端交接正文后通过共同收集入口受理。旧 merge_witness 因网络未注册消息窗口计数返回 SnapshotTooLarge（target/handoff-variant-capacity-reproduction.log，0.10 秒），正文并未达到现有存储字节上限。

移除认证交接见证与快照校验中的该计数上限；快照 decoder 在读取候选前按剩余字节和每候选最小 rights/count framing 校验数量，实际 operation-count/framing、签名请求绑定、候选重复、资源与最终性验证继续走唯一现有实现。整个交接和快照仍受原 512 MiB 编码上限约束，不引入无限网络消息队列。普通未认证冻结来源的既有候选准入窗口与 BFT 待处理消息窗口不改变。候选选择复用 from_persisted 构造确切选择，不先克隆整份 sibling 列表再删除；快照验证只构造一次无 sibling 的基底；交接插入使用现有按 TaskId 范围索引检查共同来源，不扫描全体无关任务。

修复回归通过（target/handoff-variant-capacity-fixed.log，0.15 秒），冷读保留全部 65 个确切冻结选择、本地唯一 owned 选择和一个实际货币占用；业务余额不变，没有票锁或 BFT 状态。相同收集再次调用不写新 generation。16 项交接回归及三项实际冻结来源集成通过（target/handoff-variant-capacity-targeted.log、target/handoff-variant-capacity-source-targeted.log）。本轮最终两端完整门禁尚待执行，不能把容量补全记作交接自动推进已实现。

自动推进仍需处理现有 CollectingTransition 对共同 governance_subject/raw vote 的拒绝，以及封口后 witness 扩展、已有 BFT 锁定根和未参与 quorum 的局部任务。不能采用各节点独立确认本地根后不可变封口的捷径：分歧根会留下活性问题；也不能以必须等待所有成员作为不可用成员情况下的推进规则。根选择需复用已有 CurrencyAllocation/成员切换作用域的 BFT，维持签票时覆盖本地已拥有的旧任务及既有认证义务；已经验证成员 QC 的落后节点如何安全停放未包含的私有旧候选并恢复原签名请求，尚无实现。此段是未完成的开发约束，不是现有代码能力。

### 2026-10-07：共识只读热路径复用唯一已验证快照

容量修复后的首次 Windows 完整门禁仍未通过（target/handoff-variant-capacity-windows.log）：73 库测试通过，主集成 245 通过、1 失败、1 忽略，73.27 秒；普通任务在 exhausted allocation 场景的原五秒期限失败。其余 backlog、冻结冲突及业务 CLI 均通过，不能用本轮缩少失败数宣称原根因已闭合。

继续沿实际恢复注册调用链检查发现，refresh_authority、prepared_task 候选恢复、prepared/public proposal subject、bft_local_state/终态查询与 validator_set_for_prepared_task 只读却调用 StateStore::load，克隆整份业务、队列、准备、签票和 BFT 证明。改为现有 load_shared 的唯一已验证 Arc 快照，认证刷新只为生成的 authority 克隆必需的 validator set，bft_local_state 只克隆请求的返回状态。验证器、原文件锁、失效 token、磁盘错误、共识证明和签票校验保持原实现；没有第二份缓存、TTL、摘要真值或延长期限。这是已确认的重复复制消除，不据此断言耗时与历史连接中断的全部因果关系。

最终源码分配恢复定向与两端完整门禁随后执行；首次门禁失败、源归档及私有现场保留。本批最后门禁基于此只读修改后的源码，不能用此前容量测试门禁作为最后源码结果。

继续检查的真实成本路径：encode_snapshot 的 validate_bft_local_state_registry 对每个保留的 valid_prevote_qc 调用 BftQuorumCertificate::verify，后者每次逐票验证签名；当前 BftQuorumCertificate 没有与不可变证明及确切 ValidatorSet 绑定的验证复用。retain_bft_states_for_validator_transition 仅在集合切换时清理非 PreparedTask 状态，分配 frontier 推进没有在同一原子写入清理历史分配作用域。这里只确认代码中的重复工作，未采集时间/调用数，不能声称它已解释历史五秒失败。

当前 validate_vote_validator_set 对 CurrencyAllocation 只核对当前 registry；最终性 sign_currency_allocation 另核对 frontier，而共同原始 BFT prevote/precommit signing-context 没有该 frontier 判断。因此，在没有先证明并落实共享签票围栏前，不能删除旧作用域锁定状态来追求性能。该路径与现有 source generation、cold proof 校验以及原签票规则需一并审查；尚未进行这类清理或新的证明缓存修改。

本批最后源码验证结果：原生 WSL 的 fmt/check/clippy(-D warnings)/all-targets 全部退出 0（target/consensus-shared-reads-linux.log），73 库测试、246 主集成、34 节点恢复和 examples 通过；主集成 47.92 秒，34 节点 56.80 秒。Windows fmt/check/clippy 和 73 库测试通过，但主集成 245 通过、1 失败、1 忽略，81.45 秒（target/consensus-shared-reads-windows.log），exhausted allocation 场景普通任务的原五秒期限仍失败；6 项 allocation 定向回归先前通过（target/consensus-shared-reads-allocation-targeted.log，34.65 秒），不能覆盖该全量失败。随后单独补跑 Windows 34 节点及 examples 退出 0（target/consensus-shared-reads-windows-remaining.log，35.95 秒），两套完整门禁没有同时执行。

失败 PID 27356 的准确现场冷读见 target/consensus-shared-reads-failure-cold.log，generation=23、frontier=u64::MAX、supply=2，完整任务 1922 已成功。这只能证明稍后持久结果，不能证明它在原期限内完成。此前容量源码失败 PID 35940 的现场和日志也保留。全部工具过程已结束，文档同步后 git diff --check 通过；容量缺口已由真实行为闭合，共识只读复制已消除，但自动安全选根/阶段推进及间歇超时根因仍未完成，总目标保持未完成。

### 2026-10-07：封口后继续收集外来见证，保持已准入根

旧共同持久入口在存在 Transition 时拒绝任何新增冻结正文，实际回归返回 TaskAdmissionClosed（target/handoff-late-witness-reproduction.log）。现允许经唯一交接受理器验证的收集继续添加无权冻结选择和原签名待办请求；共同写入仍禁止新增本地 owned 选择，也禁止将已收集见证升级为本地占用。请求签名、原版本、业务验证及 state/prepared CAS 保持原检查，收集不延长原请求期限。

已准入 Transition 的确切交接正文和摘要保持不可变。后来新增的无权、未最终化见证或外来待办不使原根失效；已有本地 owned 选择、最终化选择及继承认证义务仍必须覆盖。新并集作为 CollectingTransition 保存，不能注册或签票；同作用域存在该收集记录时，原来确切已准入根仍可按原 BFT 条件继续，不允许新根绕过收集阶段。重复收集原根或相同并集不降级原 Transition，也不增加 generation。

扩展既有持久回归 handoff_collection_unions_local_obligations_and_keeps_expiry_and_atomic_admission，证明封口后的远端冻结候选和未准备发行请求进入冷读，原根与本地唯一占用保留；原根完整 precommit QC 可进入最终签票，见证升级 owned 则拒绝且无写入。最初补签夹具缺少 precommit QC 的 BftFinalityNotReady 日志保留（target/handoff-late-witness-fixed.log）；补入正常完整验证 QC 后通过（target/handoff-late-witness-fixed-final.log），没有放宽最终签票条件。16 项交接回归与 4 项治理重启集成通过（target/handoff-late-witness-targeted.log、target/handoff-late-witness-governance-targeted.log）。最终两端完整门禁另行执行。

这是安全自动选根所需的迟到收集边界修复。自动收集后进入已有成员 BFT、未参与选根节点的认证安全停放/重新提交，以及间歇期限失败根因仍未完成，不以本项代替总目标。

本批最终源码的 Windows 与原生 WSL 完整门禁顺序执行，fmt/check/clippy(-D warnings)/all-targets 均退出 0（target/handoff-late-witness-windows.log；WSL target/handoff-late-witness-linux.log）。两端 73 个库测试、246 个主集成及独立 34 节点恢复目标和 examples 通过；Windows 主集成 63.11 秒，34 节点 39.85 秒；Linux 主集成 47.87 秒。此前 Windows 五秒期限失败日志与现场仍保留，本轮通过不能证明间歇根因已经解决。所有进程已结束，文档同步后再次执行 git diff --check。

### 2026-10-07：收集根原子准入与自动成员 BFT

共同持久层增加 promote_transition_collection：只有本地已经认证收集的确切根可提升。持同一文件锁先验证当前业务基线、全部冻结选择和待办正文覆盖，再将同一 pending 项原子改为 Transition，最后执行原有治理 subject 校验并落盘；未收集或失效根拒绝，相同已准入根幂等。提升不修改已有 BFT 状态、锁、票或原根。原始 CollectingTransition 的 raw vote 禁止保持，只有这一步完成后才成为既有 BFT 候选，不加入第二轮确认共识或全员等待规则。

提交切换的操作入口与直接节点入口均先通过唯一交接受理器收集，再提升并注册既有成员/CurrencyAllocation scope。认证远端完整正文也先与本地义务合并，遗漏本地任务的来源只产生补全后的新根，不能准入所遗漏的原摘要；无效授权/正文仍拒绝。已准入原根的重复来源继续复用确切正文，不因迟到见证或已覆盖业务完成而改根。运行时启动恢复及共同顺序驱动在正文收集后自动提升持久收集记录。提升成功时才传播非收集公告，重复来源不反复广播；冷恢复原根仍通过原 proposer/source 重传路径传播。

已有同 scope 多候选 BFT 继续负责选根。没有最高 prevote QC 或持久锁时，同一切换意图优先提议包含更多义务的已准入并集；最高 QC 和已有锁始终优先，不改其证明规则或终态规则。收集 API 的持久阶段与 raw vote 边界仍可单独验证，但运行节点现会自动将合法收集根提升，旧测试假设“重启仍永远停在收集阶段”已不再是目标行为。

新增实际认证 QUIC 回归 cold_membership_request_collects_distinct_duties_and_switches_four_nodes：四个节点各有一份独有开户冻结任务，仅首节点提交一次切换，丢失首次公告后重建运行时，沿真实源传输和原 BFT 驱动自动汇集并切换；冷读各节点安装同一四任务根、业务账户未执行、只保留原本本地提交权，没有任何旧 PreparedTask 投票锁。定向通过（target/automatic-handoff-four-node.log，2.74 秒）。旧定向首轮的三处阶段断言失败与随后两节点未驱动 BFT 导致的期限失败日志保留，不将其抹去。新逻辑的最终完整门禁尚待执行。

这补上了自动收集/选根/切换的已有节点路径，尚不证明缺席成员、根外私有旧任务和不同业务基线的落后节点都可恢复。认证安全停放/重新提交、完整业务变更后迟到处理及间歇超时根因继续开发；总目标仍未完成。

本批最终门禁：Windows fmt/check/clippy(-D warnings) 和 74 个库测试通过，主集成 245 通过、1 失败、1 忽略，112.95 秒（target/automatic-handoff-windows-final.log）；失败为 split_resource_claims_converge_by_certified_abort_and_preserve_terminal_results_after_restart 的原 90 秒任务收敛条件。随后单独执行 34 节点恢复目标和 examples 通过（target/automatic-handoff-windows-remaining.log），不记为完整通过。原生 WSL fmt/check/clippy 和 74 个库测试通过，主集成 245 通过、1 失败，47.12 秒（WSL target/automatic-handoff-linux.log）；失败为 validator_reconnect_progresses_while_public_discovery_is_stalled 的原四秒重连条件，Linux 剩余目标另行补跑。两套主门禁顺序执行，未以孤立重跑覆盖原失败。最初 Windows clippy 的测试 helper 不必要 cloned 已修正，失败日志 target/automatic-handoff-windows.log 保留。

Windows 失败原进程 PID 29876 的四节点现场保持，确切冷读见 target/automatic-handoff-conflict-cold.log：任务 3101 全部非终态、节点 2/4 在 round=20 且无 QC/锁，节点 1/3 没有该任务的 BFT 状态；3102 已认证取消，另外两对任务已经一成一取消。失败时四节点均有三名已连接验证者，日志只有两项 ConnectionFailed，不能把此失败认作断连根因。节点 1/3 明确记录 PaymentAddressUnavailable；同场景的支付地址退休任务 3105 已完成。核对共同 source_admission/prepare_expected_plan/admit_frozen_variant 路径确认，未取得本地权利的旧见证仍按当前 Active 条件重新建立支付前提，因此这条状态变化路径尚未推进。已拥有原前提的节点与没有原前提的见证节点不同，不能跳过地址检查直接授予见证提交权。完整历史认证/前提处理仍需实现，这项现场证据缩小了目标 3 的真实缺口；Linux 失败两份重连夹具和日志也保留。

补跑的 Linux 34 节点恢复目标及 examples 退出 0（WSL target/automatic-handoff-linux-remaining.log，34 节点 86.65 秒），Windows 对应目标 37.15 秒。当前所有验证进程已结束。收集推进遇到已被同一原子业务写入刷新掉的根时，记录 StaleState/StalePreparedTasks 并保留新收集记录待下次驱动，不结束整个节点；共同收集 map 容量判断允许已经存在的确切根在容量满时幂等复用，不扩容。未锁定根的并集提议选择放到已有 candidates 模块，避免继续向共识主文件混入选择职责。文档更新后 git diff --check 退出 0。自动四节点闭环已有行为证据，但两端全量失败仍然存在，缺席/落后成员恢复与历史业务前提处理仍未完成，目标保持 active。

### 2026-10-07：认证地址退休后的既有冻结来源与原任务 Abort

既有精确签名源、请求摘要、原委员会及冻结选择已在本地验证，但如今无法重新受理时，业务失败路径可仅设置本地 conflict_abort，并注册原 PreparedTask BFT；不创建 payment prerequisite、Commit 权限、终态或释放。篡改选择、未知来源、暂时分配/占用错误及过期新请求不能借此受理。持久 Commit prevote QC、precommit readiness、最终票及待处理的有效 Commit QC/完整终态证书继续保护全部已知候选。

最初只允许纯见证选择 Abort，真实四节点虽然互连却在第六轮仍无 QC（target/unusable-witness-network-rotation.log）。原因是两名持有者仅准入 Commit，裸 Abort 提议本身又不足以准入取消候选；延长轮转不能解决这一缺口。持有者现可对精确已知转账源，在临时状态中移除本任务支付建立记录并复用唯一冲突准备器，独立验证该源因 PaymentAddressUnavailable 无法在未建立前提的节点受理，才选择 Abort。原状态和资源保留，已有 Commit 证明仍优先；不是凭对端裸提议授予资格。真实四节点回归已通过（target/unusable-witness-local-validation.log，1.03 秒），认证取消后冷读余额不变且占用为零。新增安全回归同时检查持有者与见证的待处理 prevote/precommit QC 保护、错误冻结选择拒绝及重复重试无写入（target/unusable-source-security-final.log）。

地址退休的终态事件除原资源交集外，仅唤醒使用这些地址的已知转账；启动恢复也检查该条件，不加入后台扫描或平行索引。原三类独立资源冲突夹具的退休地址原来恰好也是转账目的地址，实际上包含跨 scope 业务失效；现将独立生命周期冲突放在同一账户的另一支付地址，保留原一成一取消验收，原业务交叉失效由新的真实网络回归专门覆盖。未改原期限或 quorum。此前两项原冲突测试通过（target/unusable-source-conflicts.log），最终源码仍需重新验证。

这只解决本地已有完整合法候选、缺少 Commit 保护时的业务失效收敛。未见过正文的历史认证、带 Commit 证明但缺少旧支付前提的执行恢复、落后成员根外义务及间歇重连失败仍须完成，不能据此宣布全部目标完成。当前批两端完整门禁待运行，原失败日志和现场保留。

本批完整门禁结果：Windows fmt/check/clippy(-D warnings) 与 75 个库测试通过，主集成 246 通过、1 失败、1 忽略（target/unusable-source-windows-final.log，75.10 秒）；失败为原 60 秒 allocation backlog，原冲突、历史来源及重连测试均通过。未执行的 34 节点目标及 examples 补跑通过（target/unusable-source-windows-remaining.log）。最新 backlog PID 2020 的保留现场冷读中，首尾任务 2000/2064 已成功（target/unusable-source-backlog-cold.log），不据此推断全部任务或原期限内完成。第一次 clippy 的 filter_map_bool_then 失败记录保留，修正为 filter/map 后通过。

同一源码的原生 WSL fmt/check/clippy 和 75 库测试通过，主集成 241 通过、6 失败（target/unusable-source-linux.log，62.04 秒）：两项 checkpoint connectivity、三项 runtime_bft 及 opposite allocation。日志包含约 10–11 秒的 Transport timed out/connection lost，不能由本次业务失效修复解释。Linux 34 节点目标和 examples 补跑通过（target/unusable-source-linux-remaining.log，34 节点 89.75 秒）。新的真实历史来源回归两端都通过，整套门禁并未全绿；日志和失败现场保留。

### 2026-10-07：入站 BFT 授权刷新不再阻塞网络 executor

已复现 serve_inbound 在异步线程直接调用 refresh_authority 时等待共享刷新锁会阻塞同线程的无关调度。新单线程真实认证握手回归由另一原生线程持有该锁两秒，在服务端收到认证请求后验证 timer 能在锁持有期间推进；旧实现失败（target/inbound-authority-wait-reproduction.log，2.05 秒），移到既有 spawn_blocking 池后同一回归通过（target/inbound-authority-wait-fixed.log，2.02 秒）。没有改变锁、认证、委员会解析、传输期限或签票规则，也没有添加后台线程循环。这是已确认并修复的 executor 阻塞路径，尚不证明此前所有间歇超时均由此引起；最新源码两端完整门禁待执行。

入站刷新修复的最新完整门禁：Windows fmt/check/clippy(-D warnings) 与 76 库测试通过，主集成 245 通过、2 失败、1 忽略（target/inbound-authority-windows.log，78.57 秒），失败为 opposite allocation 和原 60 秒 backlog；历史来源、原资源冲突及入站刷新回归通过。Windows 未执行的 34 节点目标和 examples 补跑通过（target/inbound-authority-windows-remaining.log，34 节点 67.95 秒）。原生 WSL 同一源码 fmt/check/clippy、76 库测试、247 主集成、34 节点目标与 examples 全部退出 0（target/inbound-authority-linux.log，主集成 63.82 秒）。之前 WSL 六项失败日志仍保留，单次全绿不证明所有间歇超时已经消失。

当前全部验证进程已结束。本轮已证实修复的是已有冻结候选在地址退休后的认证 Abort 收敛，以及入站握手授权刷新锁等待造成的 executor 阻塞；不是完整历史业务证明或全部落后成员恢复。成员证明追赶路径的同步存储、Windows 分配期限失败、根外旧义务与未知/带 Commit 证明的历史正文仍需继续推进，四项目标保持未完成。

### 2026-10-07：成员证明追赶的存储等待与未知冻结候选 QC

真实单线程 QUIC 成员证明回归，在提供者收到版本 1 请求后让另一个原生线程持有客户端存储锁两秒，再传回合法版本 2 证明。旧 apply_membership_chain 在异步线程直接验签、安装和重读快照，导致无关 timer 无法在锁释放前推进（target/membership-install-wait-reproduction.log，2.11 秒）。快照读取及原证明验签/原子安装现通过既有 spawn_blocking 池执行，事务和并发安装后原版本检查仍用原实现。网络维护、入站握手及成员追赶复用 refresh_authority_for_network，不另建缓存、验证器或后台循环。同步共识存储线程继续直接调用原 refresh_authority。原 64 步批次与五秒网络追赶期限未增加；若超时发生在已开始的有效安装期间，该原子事务可以继续完成，后续读取和刷新仍以持久版本为准，不回滚已认证进展。

修复后实际安装到版本 2，等待期间网络 executor 可继续调度；缺失、伪造与错误 frontier 证明仍保持恢复签票锁且无 generation 写入，两项成员回归通过（target/membership-install-wait-fixed.log，2.07 秒）。原入站握手锁等待回归仍通过（target/membership-common-refresh-fixed.log，2.03 秒）。这证明具体阻塞路径修复，不代替此前全量分配期限失败的耗时定位。

已受理源的 Abort 保护进一步覆盖尚未到达正文的合法 Commit QC：BFT QC 的签名 statement 绑定 exact PreparedTask scope，验证原委员会 quorum 且 digest 不等于该任务 Abort 即保护，不能只检查本地已知候选摘要。FinalityCertificate 的外层 scope 本身未签名，因此这一路仍需本地已知冻结摘要绑定，不凭未认证 envelope 给任意任务增加保护。既有安全回归同时验证已知/未知 QC、持有者/见证及 prevote/precommit，且不增加权利或终态（target/membership-unknown-commit-proof.log，4.16 秒）。未知正文的实际恢复执行仍未完成。

最新源码两端完整门禁待执行；Windows 原分配失败日志、旧成员安装阻塞回归失败现场均保留，全部四项目标继续 active。

本批最新源码完整门禁：Windows fmt/check/clippy(-D warnings) 与 77 库测试通过，主集成 243 通过、4 失败、1 忽略（target/membership-storage-windows.log，98.31 秒），失败为 public observer、opposite allocation、exhausted allocation 和原 60 秒 backlog。成员安装锁等待、入站握手及已知/未知 Commit QC 保护回归均通过；Windows 34 节点目标和 examples 补跑通过（target/membership-storage-windows-remaining.log，34 节点 68.56 秒）。同一源码原生 WSL fmt/check/clippy、77 库测试、247 主集成、34 节点及 examples 全部退出 0（target/membership-storage-linux.log，主集成 49.25 秒、34 节点 55.64 秒）。保留所有失败日志，不用 WSL 成功覆盖 Windows 失败。

使用既有 snapshot-status CLI 冷读 Windows 主集成 PID 5100 的确切失败现场（target/membership-storage-failure-status.log）：backlog generation=447、supply=31、frontier=33；opposite allocation 四节点 frontier 均为 6、supply 均为 2，generation 为 28/32/31/30。后一结果说明两笔区间均已认证分配，但业务尚只完成首笔发行；前者显示队列仍在逐笔推进，而不是已在原期限内完成。不能仅由这些计数归因网络、签名重复验证或磁盘耗时，后续需按阶段定位。

全部本批验证进程已结束，文档后 git diff --check。成员追赶安装的已复现 executor 阻塞已修复；普通公开快照/证明提供路径还存在同步 loader，Windows 分配耗时尚未定位，根外旧义务与完整历史业务证明仍须推进，总目标未完成。

### 2026-10-07：公开服务存储等待与取消后的连接限额

普通公开快照、成员证明和授权恢复响应原先在网络异步线程直接调用同步存储提供者。真实两连接 QUIC 回归令独立线程持有状态文件锁，旧证明响应阻塞另一连接的 Ping（target/public-storage-wait-reproduction.log）。共同响应构造现复用原验证与编码路径，在 spawn_blocking 中执行；Ping 保持直接响应。提供者使用不可变 Arc 句柄，沿用原共享快照和公开视图缓存，不复制整份业务状态或新增后台循环。运行时公开服务落到 runtime/public_serving.rs，原已有对端记录刷新任务的持久化也在阻塞池中完成。

阻塞工作持有原 ActiveConnectionPermit：即使五秒证明会话超时，未结束的存储操作仍占用原连接名额，避免反复取消绕过并发限制。实际回归覆盖证明查询、摘要查询和取消后名额三种情况，全部通过（target/public-storage-cancellation-quota-fixed.log，11.17 秒）；未增加会话期限。恢复传输五项安全回归及交接十七项回归通过（target/public-storage-recovery-security.log、target/public-storage-handoff-security.log）。本批两端完整门禁待执行，分配期限失败、根外旧义务和未知历史正文的恢复仍未完成。

公开服务这批完整验证：Windows fmt/check/clippy、78 库测试通过；主集成 244 通过、3 失败、1 忽略（target/public-storage-windows.log，79.92 秒），仍为 opposite/exhausted allocation 和 60 秒 backlog。34 节点恢复与 examples 补跑通过（target/public-storage-windows-remaining.log，74.69 秒）。冷读 PID 21780 的竞争分配四节点均 supply=5/frontier=6，大队列 generation=507/supply=36/frontier=38（target/public-storage-failure-status.log）；冷读完成不证明原期限内完成。

相同归档源码的 WSL fmt/check/clippy 通过，库测试 77 通过、1 失败、1 忽略，chunked_collection 的测试连接服务在 serve_inbound 返回 Transport(timed out) 时 panic，随后 listener worker 已结束（target/public-storage-linux.log，15.25 秒）。主集成 247、34 节点恢复及 examples 补跑均通过（target/public-storage-linux-remaining.log，51.83/54.43 秒）；该交接库测试单独重跑通过（target/public-storage-linux-handoff-isolated.log，3.45 秒）。保留失败现场，不将一次重跑视为间歇超时根因已解决。

### 2026-10-07：已认证退休恢复执行的事件遗漏

既有退休来源安全回归扩展为先持久化最终性证书，再通过 commit_certified_component 恢复执行。旧实现完成退休但没有在 outcome.retry 返回引用退休地址的旧转账（target/certified-retirement-wakeup-reproduction.log，0.15 秒），因此在不重发证书的依赖恢复路径会漏掉推进事件。普通最终性与认证组件恢复现共用 contenders_for_plan，复用原资源索引并只在退休事件上检查引用相关地址的候选；不授予权利、不单方取消、不添加扫描循环。扩展的原安全回归仍检查未知 Commit QC 保护、精确来源及持有者/见证的权利边界。本批修复后的定向与两端完整验证结果随后记录；未知历史正文及完整落后成员安装仍未完成。

认证退休恢复修复后的原安全回归通过（target/certified-retirement-wakeup-fixed.log，0.59 秒）。最新完整源码 Windows fmt/check/clippy(-D warnings)、78 库测试通过；主集成 242 通过、5 失败、1 忽略（target/certified-retirement-windows.log，81.79 秒），失败为 certified range crash recovery、opposite allocation、exhausted allocation、60 秒 backlog 和单来源 private pull。34 节点恢复与 examples 补跑通过（target/certified-retirement-windows-remaining.log，55.16 秒）。

同源码 WSL fmt/check/clippy、78 库测试通过；主集成 240 通过、7 失败（target/certified-retirement-linux.log，71.82 秒）：业务 CLI、opposite allocation、验证者重连、两项冻结来源，以及两项公开同步。库内退休恢复/Commit QC 安全回归通过，不用这一结果替代上述整体失败。失败日志中三个节点工作任务报告阻塞工作被取消，尚不能据此证明它们是原始故障而非测试退出后的取消结果。WSL 34 节点恢复和 examples 补跑通过（target/certified-retirement-linux-remaining.log）。全部验证进程已结束，所有失败日志与现场保留；历史恢复的事件遗漏已修，分配活性、间歇传输、根外旧义务及完整落后成员业务基线安装仍未完成。

### 2026-10-07：分配队列计时与注册阶段的旧快照重试

用实际 65 请求重启队列做临时阶段计时，未修改期限或持久化规则。两次原实现定向运行通过，但总耗时 63.14/61.41 秒（包含提交准备，队列运行仍在原 60 秒预算内）；第二次 849 次快照写入累计编码 907.644 毫秒、写盘 23396.425 毫秒，66 次恢复批次处理 2210 次请求，累计验签 493.023、加载 3251.676、提交 23081.754 毫秒（target/allocation-storage-timing.log、target/allocation-resume-timing.log）。提交计时包含它内部的写盘，不将这些数值相加或把总耗时都归因一个阶段。原生 Windows 写盘/重复读取成本已有实测，仍不足以证明全部连接超时根因。

共同 submit_snapshot 对已持久化的 exact allocation_task 不再重复排队，并复用传入的已验证快照构造候选；共识注册仍由原 proposal_subject 校验当前委员会、请求和 frontier，所有投票仍经原持久化围栏。注册阶段的 StaleState/StalePreparedTasks 现在也进入同一最多三次重试，避免它换了错误包装层后跳过重读。扩展原并发回归，分别覆盖已分配业务快照变化，以及未分配请求读快照后另一个认证区间推进 frontier：后者只注册新的起点 3，不分配或签旧起点 2（target/allocation-frontier-reuse-fixed.log，0.32 秒）。

诊断版本中去重后的实际队列也通过，63.16 秒；提交阶段累计 16656.008 毫秒，整轮总耗时没有明显改善，不宣称全量期限问题已修复（target/allocation-resume-reuse-timing.log、对应 summary.log）。所有临时计时输出已移除，线上不增加环境选项、循环、日志或额外时钟读取。最新代码两端完整验证随后记录；完整落后成员业务安装、根外旧义务、未知历史正文与间歇超时仍未完成。

本批去除诊断计时后的最新源码门禁：Windows fmt/check/clippy(-D warnings)、78 库测试通过，主集成 244 通过、3 失败、1 忽略（target/allocation-frontier-windows.log，80.78 秒），仍为 opposite allocation、exhausted allocation 与原 60 秒 backlog；34 节点恢复和 examples 补跑通过（target/allocation-frontier-windows-remaining.log，87.83 秒）。同一归档源码 WSL fmt/check/clippy、78 库测试、247 主集成、34 节点恢复及 examples 全部通过（target/allocation-frontier-linux.log，主集成 57.34 秒、34 节点 57.03 秒）。两端全量不是全绿；所有验证进程已结束，失败日志和现场保留，文档后 git diff --check。

对根外旧任务的后续恢复不能直接清除旧锁或按当前状态重新赋权：共同 BFT 验票只依据原任务委员会与已有本地提交权，跨成员切换的负向关闭证明、旧私有请求停放/重提和迟到正文认证仍须形成完整路径。当前未实现这些扩展，不将本次减少重复读取和注册重试记作该路径已完成。

### 2026-10-07：成员追赶补取认证交接正文

运行时成员追赶原先仅下载切换证明并尝试本地重建交接根，缺少远端冻结候选时返回 StalePreparedTasks。现在仅在该错误且证明带交接根时，使用同一 pinned provider 的新连接，通过既有成员身份认证与有界分块下载获取正文；复用原授权者验证、业务基线校验和 witness 受理，再通过原原子切换安装器安装。正文必须保持已验证证书的精确 finality statement；不会授予远端计划资源权利，也不会覆盖不同的本地业务状态或遗漏本地义务。存储、受理与安装继续在 blocking worker 执行，原五秒总预算与 64 次切换上限不变。

实际 QUIC 节点回归先用错误授权者验证拒绝且 generation 不变，再用正确授权者追赶至集合 2，冷读检查根一致、请求精确绑定且未完成、未取消、未执行开户、无 owned candidate、无投票锁及本地 BFT 状态。首次失败是测试将受理后的 Pending 绑定错误预期为 None（target/membership-body-runtime.log）；已按既有 task_succeeded 语义改为 Some(false)，并补上述外部行为检查。成员相关回归通过（target/membership-body-fixed.log）；本批完整门禁待执行。不同业务基线安装、根外本地义务与未知历史正文恢复仍未完成。

本批完整验证已经结束：Windows fmt/check/clippy(-D warnings)、79 库测试通过；主集成 242 通过、5 失败、1 忽略（target/membership-body-windows.log，82.51 秒），失败为 exhausted/opposite allocation、frontier barrier、原 60 秒 backlog 和单来源私有正文拉取。34 节点恢复与 examples 补跑通过（target/membership-body-windows-remaining.log，72.44 秒）。同一归档源码 WSL fmt/check/clippy 与库测试通过，主集成 246 通过、1 失败（target/membership-body-linux.log，62.41 秒）：running_bft_sessions 的初始建连超过原五秒预算，发生在任务准备和成员切换之前。34 节点恢复与 examples 补跑通过；该建连测试单独运行通过（target/membership-body-linux-retained-isolated.log），不据此宣称全量稳定性缺陷已修复。全部失败日志保留，四项目标继续；本批成员追赶缺正文的有效受理回归通过，不代表不同基线覆盖、根外义务或历史正文恢复已经完成。

### 2026-10-07：新成员恢复投票闭环与拨号检查点补发的存储阻塞

新增一个完整五节点运行回归，复用现有认证正文下载、空目标基线安装、独立 consensus key rotation、恢复检查点与签名围栏：非空业务基线由原委员会证书认证，新成员安装后拒签且无本地投票锁；四个健康成员完成下一次轮换，新成员收到匹配恢复证明后自动恢复；随后从新成员提交开户，五个节点完成业务，并冷读证明新成员自己持久签署了该任务（target/member-reentry-business-vote.log，8.50 秒）。没有新增解锁开关、首次加入即授票或第二套恢复状态；不同已初始化业务基线的覆盖、根外旧义务及未知历史来源仍需继续处理。

共同拨号末尾的 sync_checkpoints 仍直接在异步 executor 上 load_shared 和编码检查点。真实 OS 文件锁回归证实，旧实现让另一生产连接的 Ping 等到两秒锁释放（target/checkpoint-replay-wait-reproduction.log，失败 2.16 秒）。补发现在在 blocking worker 中读取、编码并发送，拨号 await 原补发完成，发布 outbound 的 mutex 作用域在 await 前结束；worker 保持原 runtime/连接资源存活。原成员身份、当前集合限定、持久证明来源与发送队列均未改变，不新增扫描、重试或放宽期限。同一回归通过（target/checkpoint-replay-wait-fixed-send.log，2.14 秒），不能据此将所有间歇连接/分配超时归因这一个调用。两端完整门禁随后记录。

本批完整门禁结束：Windows fmt/check/clippy(-D warnings)、81 库测试通过；主集成 244 通过、3 失败、1 忽略（target/member-reentry-replay-windows.log，79.01 秒），仍为 exhausted/opposite allocation 与原 60 秒 backlog。34 节点恢复与 examples 补跑通过（target/member-reentry-replay-windows-remaining.log，74.91 秒）。同一归档源码 WSL fmt/check/clippy、81 库测试、247 主集成、34 节点恢复及 examples 全部通过（target/member-reentry-replay-linux.log，主集成 49.01 秒）。两端全量不是全绿，保留失败日志；已验证的新成员恢复业务链路和拨号补发阻塞修复不代替根外义务、未知历史正文与其余间歇稳定性问题的完成证据。文档后 git diff --check。

### 2026-10-07：退休中的认证旧执行恢复与最终退休围栏

复现：新成员安装含旧转账的认证交接后，下一集合已完成 RetirePaymentAddress，目标没有旧 payment execution。原被动终态路径用当前状态重新准备旧转账，因地址 Retiring 被拒，原 Commit certificate 永远不能消费（target/historical-retiring-transfer-reproduction.log，失败 0.17 秒）。现在只对已安装交接的精确候选和原集合终态证书，先验证 finality，再从唯一认证正文受理 witness，不授予 owned candidate 或投票权；原原子认证组件执行按已认证 EstablishedTransfer 恢复仅用于此次执行的前置记录，仍复用当前账户归属、货币和地址执行校验。Retired 地址与归属变化不能由该恢复绕过。

同一场景同时复现 FinalizePaymentAddressRetirement 无视尚未终态的认证交接转账（target/historical-retirement-fence-reproduction.log，失败 0.13 秒）。原 ResourceFenceIndex 新增仅供交接使用的已建立支付地址索引，随唯一不可变 handoff 缓存；普通竞争/owned 索引不建立它。最终退休查询未终态的原转账义务；启动退休仍允许，Commit/Abort 终态后不再阻挡，不引入扫描循环或第二份业务状态。

完整回归通过真实运行节点的认证 BFT 连接传递旧终态证书：伪造证明不写状态，Retiring 上旧转账完成且当前货币归属正确，无旧投票锁与签名解锁，无残留支付前置记录；其后最终退休成功（target/historical-retiring-real-transport.log，0.34 秒）。已有二十项交接/来源/成员恢复安全回归通过（target/historical-retiring-handoff-cached-index.log，6.26 秒）。本批覆盖已认证交接中的历史执行，不声称根外来源、不同已初始化基线覆盖或其余稳定性问题已经闭合；两端完整门禁随后记录。

首次完整 Windows 库测试发现旧 certified-resource-ring 回归失败：恢复支付前置记录的条件曾过宽，将没有认证交接的普通未持有计划也允许在 Retiring 上恢复（target/historical-retiring-windows.log）。已收紧到 finalized unowned 计划的精确 digest 必须存在于已安装 handoff；其他计划继续走原 Active-only establishment。原环形资源安全回归通过（target/historical-retiring-uninherited-security.log，0.65 秒），精确交接/真实传输回归仍通过（target/historical-retiring-exact-establishment.log，0.35 秒）。保留失败日志，修复后完整门禁使用新日志，不将第一次失败隐去。

本批修复后的完整门禁已结束：Windows fmt/check/clippy(-D warnings)、82 库测试通过；主集成 245 通过、2 失败、1 忽略（target/historical-retiring-exact-windows.log，79.33 秒），失败为 exhausted_competing_allocation_rejects_without_stopping_other_business 和 allocation_backlog_beyond_candidate_window_survives_restart_and_drains。34 节点恢复与 examples 补跑通过（target/historical-retiring-exact-windows-remaining.log，79.37 秒）。同一归档源码 WSL fmt/check/clippy、82 库测试、247 主集成、34 节点恢复与 examples 全部通过（WSL target/historical-retiring-exact-linux.log，主集成 53.30 秒、34 节点 55.55 秒）。历史退休恢复与最终退休围栏回归两端通过；Windows 整体仍失败，不用 WSL 全绿替代双平台完成证据。根外本地义务、未知历史正文及其余间歇稳定性问题仍未完成。全部验证进程已结束，失败日志保留。

### 2026-10-07：节点 peer 缓存等待阻塞网络执行器

单线程运行节点回归持有真实 PeerStore 的原写入 mutex 两秒，同时调用公开 bootstrap 并发送另一连接的 Ping。原实现等写入锁释放才响应（target/peer-cache-wait-reproduction.log，失败 2.21 秒）。该 mutex 也由同步持久化写入持有；公开维护的 recent/计数、验证者重连的 validator_candidates，以及公开拨号的缓存写入/失败降序此前直接在 async 路径等待。现复用原缓存方法，查询与写入在 blocking worker 执行；不复制缓存真值、不改变排序与持久化校验，不放宽期限或增加重试。拨号写入 worker 持有原连接 permit、peer lease 和 transport client，取消不能提前释放未结束工作的名额。公开连接维护归入 runtime/peer_connectivity.rs，避免继续把独立职责堆入 runtime.rs。

同一回归扩展为同时驱动公开 bootstrap 和验证者维护，Ping 在写入锁仍持有时响应（target/peer-cache-wait-both-maintainers.log，通过 2.18 秒）。这证明缓存锁等待的 executor 阻塞已消除，不证明所有冻结来源/分配超时共用这一根因。两端完整门禁随后记录；根外义务与未知历史正文恢复仍未完成。

本批完整验证结束：Windows fmt/check/clippy(-D warnings)、83 库测试通过；主集成 242 通过、5 失败、1 忽略（target/peer-cache-windows.log，81.16 秒）：exhausted/opposite allocation、frontier barrier、原 60 秒 backlog，以及旧退休来源的认证 Abort 推进。真正的独立 34 节点测试通过（target/peer-cache-windows-discovery.log，80.90 秒）；examples 通过（target/peer-cache-windows-remaining.log）。remaining 日志中的 integration thirty_four 过滤实际运行零项，不计作 34 节点证据。

同一归档源码 WSL fmt/check/clippy 通过；库测试 80 通过、3 失败、1 忽略（target/peer-cache-exact-linux.log，18.54 秒），失败为自动交接并集、分块交接推进的建连超时和锁定提交存储下的 Ping 超时。主集成补跑 246 通过、1 失败（target/peer-cache-linux-integration.log，55.51 秒），失败为 one_validator_source_bootstraps_prepared_task_consensus_by_private_pull。34 节点恢复与 examples 通过（target/peer-cache-linux-remaining.log，53.53 秒）。四个 WSL 失败场景单独运行均通过（target/peer-cache-linux-failures-isolated.log），不据此证明全量稳定性问题已消除。新增缓存锁等待回归在两端完整库测试中通过；全部进程结束，失败日志保留，四项目标仍未完成。

### 2026-10-07：原委员会成员的认证历史终态恢复

既有真实网络退休转账回归扩展为原成员 1 与新成员 5 两种身份，统一夹具与断言。原成员安装认证交接、缺少旧执行前置记录且支付地址进入 Retiring 后，原委员会有效终态证书经运行节点传递仍超过原三秒预算（target/historical-origin-member-reproduction.log，失败 3.29 秒）。原因是 install_handoff_terminal 以本地身份属于原委员会为条件排除被动恢复，继而落入无法按当前业务重新建立旧执行的来源受理路径。

被动入口现在对已配置验证者统一消费精确认证交接候选的原终态证书：保留原集合解析、历史版本限制、精确请求/正文/操作绑定、证书验证和原原子组件执行，缺失候选仍只受理 witness；不新增签名、不授予资源权利、不解除安全锁。原成员与新成员均通过同一实际 BFT 传输回归（target/historical-origin-member-fixed.log，0.65 秒）；两者都检查伪造证明不写 generation、正确货币归属、无残留支付执行、无旧投票锁、安全锁保持及其后最终退休成功。此项修复仍不覆盖交接根之外的未知正文；完整门禁随后记录。

本批完整验证结束：第一次 Windows 门禁被 Clippy 的 unnecessary_get_then_check 拦截（target/historical-origin-member-windows.log），已用 contains_key 修正。修正后的 Windows fmt/check/clippy(-D warnings)、83 库测试通过；主集成 244 通过、3 失败、1 忽略（target/historical-origin-member-exact-windows.log，76.85 秒）：私有单来源拉取、opposite allocation、原 60 秒 backlog。34 节点恢复与 examples 通过（target/historical-origin-member-windows-remaining.log，78.37 秒）。同归档源码 WSL fmt/check/clippy 通过；库测试 82 通过、1 失败、1 忽略（target/historical-origin-member-linux.log，13.88 秒），失败为自动交接测试监听器的首次请求超时；主集成补跑 247 全部通过（target/historical-origin-member-linux-integration.log，50.11 秒）。34 节点与 examples 通过（target/historical-origin-member-linux-remaining.log，55.33 秒），自动交接单独运行通过（target/historical-origin-member-linux-collection-isolated.log，2.26 秒）；不以孤立通过替代全量稳定性证据。原/新成员历史终态回归在两端库测试通过，全部进程结束。

后续已定位待验证路径：分配恢复在每次前沿改变后遍历全部 allocation_task 并重新构造候选，队列推进存在累积重复工作；尚未修复。connect_handoff_nodes 测试监听器对单条首次请求超时执行 unwrap，再将子任务 panic 扩散为整个节点模拟退出；生产监听器对同类失败只关闭该连接。这一生命周期差异已由代码确认，但尚未验证其对全部超时的影响，不能据此豁免失败。根外交接义务、未知历史正文与其余稳定性问题仍未闭合。

### 2026-10-07：分配恢复只按需注册当前前沿候选

resume_currency_allocations 原来在每次前沿证书推进后，验证并重新注册所有剩余排队请求；N 项排队请求会反复成为同一前沿的竞争候选。现恢复一个可用的未分配请求后，其余未分配请求留在原 task_bindings 的持久队列，等下一次认证前沿推进继续。既有 prevote QC/locked digest 的来源仍优先，已获得认证地址区间的业务全部继续准备；永久业务拒绝仍清理该原恢复项并尝试下一项，不因单个无效请求停止节点。请求队列遍历借用唯一快照正文，避免重复复制签名请求。直接提交和网络获取候选的原 64 项窗口不变，无新配置、定时循环、排序协议或平行队列。

既有 65 请求真实重启回归通过（target/allocation-lazy-frontier-backlog.log，42.39 秒），仍覆盖第 65 项锁定候选优先恢复、原 60 秒排空预算、全部任务成功、连续前沿 66 与余额 65。该用例前一批完整 Windows 验证失败，当前孤立通过不替代完整门禁或证明所有间歇故障已修复。本批完整门禁随后记录。

本批完整门禁结束：Windows fmt/check/clippy(-D warnings)、83 库测试通过；主集成 242 通过、5 失败、1 忽略（target/allocation-lazy-frontier-windows.log，83.46 秒）：exhausted/opposite allocation、frontier barrier、私有单来源拉取及原 60 秒 backlog。65 请求的孤立通过未能在全量压力下保持，不宣称分配超时已修复。34 节点与 examples 通过（target/allocation-lazy-frontier-windows-remaining.log，96.46 秒）。同归档源码 WSL fmt/check/clippy、83 库测试通过；主集成 246 通过、1 失败（target/allocation-lazy-frontier-linux.log，56.20 秒），失败为旧退休来源认证 Abort 推进；65 请求队列在 WSL 全量中通过。34 节点和 examples 补跑通过（target/allocation-lazy-frontier-linux-remaining.log，55.41 秒）；旧退休来源单独运行通过（target/allocation-lazy-frontier-linux-historical-isolated.log，3.00 秒），不以此替代全量失败证据。全部验证进程结束，失败日志保留，文档后 git diff --check。已削减分配恢复中的重复受理，但四项目标仍未全部解决。

### 2026-10-07：交接测试复用生产监听器的连接生命周期

真实 QUIC 连接在首个请求前关闭即可复现原测试监听器退出：accept_request 返回 None 后 unwrap panic，JoinSet 的 unwrap 将其升级为整个节点模拟退出（target/handoff-listener-abandoned-reproduction.log，失败 0.36 秒）。这不是生产监听器的行为。connect_handoff_nodes 现在直接使用节点的原 PeerRecord 和生产 run_listener，删除额外 transport identity/QuicServer 与平行连接管理；run_listener 仅改为 crate 内可见，不改变外部 API 或生产处理。原自动交接回归保留首请求前断连并检查监听器仍存活，再完成四节点认证义务并集和切换；四项交接传输/收集回归通过（target/handoff-production-listener-fixed.log，4.84 秒），原权利、根、授权与投票边界断言保留。此项纠正测试故障传播，不代表生产冻结来源超时已全部解决。下一步验证完整 NodeRuntime::run 下的自动交接，而不是只验证人工驱动的共识步骤。

### 2026-10-07：完整运行交接的来源补发、被动终态与继承恢复

新增 cold_running_nodes_collect_membership_and_finish_distinct_business 使用四个真实 NodeRuntime::run、冷启动持久请求和认证拨号；没有人工推进共识，要求四节点均切换且四个独有请求全部成功，保留原十秒预算。首次失败显示封口节点拒绝外来终态来源 TaskAdmissionClosed。封口后只允许独立验证的非拥有 witness 受理，仍禁止新 owned 候选；已有终态证书通过原独立业务验证器验证精确 Commit digest，不要求存在资源冲突。普通冲突入口仍要求实际 blocker，Abort digest 不能授权 Commit witness；既有冲突回归扩展验证无冲突的有效终态正文、不同 digest 与 Abort 证明拒绝，验证过程不得写入状态或授予权利。

认证拨号复用检查点补发所读的唯一快照，向该接收方发送现有未完成成员来源摘要和本地拥有任务的来源摘要；不传播私有正文、不广播、不增加后台循环。原委员对已知 primary witness 的 promotion 走原完整正文验证与原子持久化，不复制同一 primary 为 variant。启动与成员激活时，按认证交接根恢复拥有原集合密钥、通过安全锁和签名版本围栏的原委员来源；新成员仍消费原证书而不获得旧投票权。继承恢复放在 runtime_tasks/handoff.rs，与认证继承义务职责一致。

完整运行回归曾孤立通过（target/automatic-handoff-inherited-resume.log，3.52 秒），但随后交接组测试 20 通过、1 失败（target/automatic-handoff-running-security.log，11.95 秒）：三个节点以三任务根切换并完成三任务，第四节点保留独有任务而仍处旧集合。故义务并集和全部节点收敛仍未完成，不能以孤立通过代替。代码确认收集根可立即提升为投票候选；已有 quorum 不保证尚未参与收集节点的独有任务被纳入。对缺项节点强行安装根或清除旧投票锁不安全，当前继续 fail-closed。

后续安全收敛应复用认证业务基线中已有 terminal task_bindings 和认证 allocation，区分已终结义务与遗漏未终结义务，禁止回滚已有认证结果和重新分配地址；必须建立遗漏任务旧投票封口证明后才能讨论新集合继续请求。此处记录待完成要求，不声明规则已实现。本批尚未完成 Windows/WSL 全量门禁，所有失败日志保留。

本批完整门禁已结束：Windows fmt/check/clippy(-D warnings) 通过，库测试 83 通过、1 失败、1 忽略（target/automatic-handoff-current-windows.log，15.74 秒），失败仍为完整运行交接；其三个节点已切换但任务均未完成，另一个节点被遗漏义务挡住。后续主集成 247 通过、1 忽略（target/automatic-handoff-current-windows-integration.log，64.11 秒），34 节点恢复与 examples 通过（target/automatic-handoff-current-windows-remaining.log，74.47 秒）。认证无冲突正文安全回归单独通过（target/certified-passive-source-boundary.log，0.50 秒），也通过两端完整库测试。

同归档源码 WSL fmt/check/clippy 通过；库测试 81 通过、3 失败、1 忽略（target/automatic-handoff-current-linux.log，30.11 秒）：新成员运行回归请求超时、人工驱动交接不收敛、完整运行交接在四节点均已安装四任务根后仍没有任务完成。主集成 247 全部通过（target/automatic-handoff-current-linux-integration.log，57.30 秒），34 节点恢复与 examples 通过（target/automatic-handoff-current-linux-remaining.log，55.22 秒）。因此不能将全部冻结来源超时归因于原测试监听器，也不能将全部失败归因于根遗漏。以上所有进程结束，失败日志保留。

随后只扩展完整运行回归的失败诊断：除业务拒绝计数外，收集连接失败及发送失败的类型计数，不输出正文、密钥或身份签名。网络授权刷新从 completed relay 和继承任务构造终态范围。进一步核对 finality.rs 与 coordinator.drive 确认：正常认证完成会在同轮 drive 退休会话并建立 completed 记录，不必等待 relay deadline，因此不能将延迟退休作为根因。成员追赶后合法旧消息是否被拒绝并关闭连接，仍需实际失败计数验证，尚未据此修改授权规则。诊断改动后的 Windows 交接组 21 项通过（target/automatic-handoff-transport-diagnostics.log，9.28 秒），随后完整库测试 84 通过、1 忽略（target/automatic-handoff-current-diagnostic-library.log，13.62 秒）；两次通过不抹除本批两端先前失败，也未取得断连假说的实测确认。

新增诊断后的同归档源码 WSL 完整库测试 84 通过、1 忽略（target/automatic-handoff-diagnostic-linux-library.log，14.80 秒）；本轮没有触发超时，因此新增连接计数尚未取得失败样本。Windows 最终 fmt/check/clippy(-D warnings) 和 diff --check 通过（target/automatic-handoff-diagnostic-final-checks.log）。生产代码在两次归档间未变化，只补测试失败诊断和实测记录；两端先前完整门禁失败仍作为未完成证据保留，不宣称偶发不收敛已修复。所有测试进程已结束。

### 2026-10-07：认证成员追赶后的运行循环漏恢复

代码确认 membership_sync 通过原认证证明安装成员集合后刷新网络授权并 wake 共识，但 runner 仅在启动或自身输出 validator_set_changed 时恢复继承正文。外部完成的集合更新不会产生该输出。新增真实 NodeRuntime::run 回归在运行循环启动后，受理二任务认证交接正文，以原成员证明安装集合 2 并执行追赶路径同一授权刷新/wake；要求原委员在原三秒预算内独立验证第二任务、取得原集合合法资源权利并建立旧任务 BFT 会话。无网络投票可伪造本地成员切换输出，不放宽安全锁、不消费无证明根、不改变过期时间和终态结果。先记录旧实现复现，修复和结果随后补充。

新回归的第一轮在 public collect_validator_set_transition 入口提前失败（target/external-membership-runner-reproduction.log，0.22 秒）：入口先用投票 hydrate 要求外来正文已经覆盖全部本地义务，故尚未收集的外来合法任务被 StalePreparedTasks 拦截。该入口现只为无正文意图捕获本地根；已携带正文的贡献直接进入原 stage_collection，由同一 authorizer、完整业务验证、原子并集及 covers 检查准入，不提前要求并集已经存在。普通投票 hydrate 和拒绝遗漏规则不改。随后继续复现外部成员激活后的 runner 漏恢复。

修正 public collection 提前覆盖检查后，回归准确复现运行循环漏恢复（target/external-membership-runner-missing-resume.log，3.23 秒）：集合已经安装而继承任务始终未建立原集合会话。runner 现保留已完成恢复工作的 (集合版本, 安全就绪) 游标，在原活动唤醒的驱动前后比较唯一持久快照；变化时复用原授权刷新、继承正文验证、公开检查点和治理恢复，分配恢复同样接收该变化事件。游标不是投票真值，不持久化或影响证明；在回调之前记录观测值，避免并发下一次成员更新被跳过。安全恢复在同集合解锁也能触发，原安全锁、minimum signing floor 和旧密钥检查仍由继承恢复入口执行。没有新 timer、后台线程或逐轮义务扫描，未变化只读已验证共享快照。修复回归结果随后记录。

原三秒预算的真实运行回归在修复后通过（target/external-membership-runner-fixed.log，0.29 秒），同时验证旧任务仍绑定集合 1、当前集合为 2、业务尚未凭空执行，以及合法资源受理和 BFT 会话已建立。首次入口失败和原漏恢复失败日志保留。此项只证明外部认证激活被运行循环消费，不证明 quorum 漏项收敛或全部间歇超时已消除；随后执行两端完整门禁。

首次完整 Windows 门禁在 Clippy type_complexity 处结束（target/external-membership-current-windows.log）：驱动返回值同时携带工作游标使显式嵌套 tuple 过于复杂。现保留原驱动返回的 pending/deadline，由已有 blocking worker 借用并返回自己的游标，不新增运行类型、配置或泛型包装；行为和回归要求不变。修正后的完整门禁另存日志。

修正后的 Windows fmt/check/clippy 通过；库测试 84 通过、1 失败、1 忽略（target/external-membership-fixed-windows.log，15.62 秒），新外部激活回归和完整运行四节点交接通过，人工驱动的冷交接仍失败。失败四节点均留在集合 1，仅有 StalePreparedTasks 计数，无连接/发送错误。保留旧失败，增加仅失败时的候选义务数量、成员 BFT 轮次、锁/prevote/precommit 对应候选数量和 finality-ready 状态诊断；不输出正文、私钥或完整票据，不改变预算、驱动方式和断言。人工收集不能进入活性的根因继续排查，未宣布已解决。

带候选/投票诊断的交接组 19 通过、2 失败（target/handoff-voting-state-diagnostics.log，12.24 秒）：人工及完整运行两种场景均复现三节点安装三任务根，遗漏节点已持有四任务候选仍被拒绝；完整运行的三个节点已完成三任务。无连接/发送失败。该证据确认遗漏义务是独立协议缺口，不因 runner 追赶修复消失。

Windows 主集成补跑 241 通过、6 失败、1 忽略（target/external-membership-windows-integration.log，99.09 秒），为私有来源拉取、exhausted/opposite allocation、frontier barrier、CLI 丢失确认重试及 65 请求排空；不能称全量稳定。34 节点恢复与 examples 结果另录。为避免普通业务每次驱动重复检查同一持久快照，驱动后仅在自身成员切换或恢复认证输出时再次检查游标；外部认证更新仍由原 membership_sync 的 wake 在下一次驱动入口消费，不增加 timer 或扫描。优化后的外部激活回归和完整 WSL 门禁随后记录。

本批后续门禁已结束：优化后的外部认证激活回归通过（target/external-membership-runner-final-regression.log，0.33 秒）。Windows 34 节点恢复及 examples 通过（target/external-membership-windows-remaining.log，80.85 秒）；Windows 主集成六项失败和库失败不因该通过被豁免。最后同归档源码 WSL fmt/check/clippy(-D warnings) 通过，库测试 83 通过、2 失败、1 忽略（target/external-membership-linux.log，29.17 秒）：人工驱动未收齐根；真实运行四节点安装四任务根后只完成三任务。新增外部激活回归在该完整库测试中通过。WSL 主集成 247 全部通过（target/external-membership-linux-integration.log，48.26 秒），34 节点恢复和 examples 通过（target/external-membership-linux-remaining.log，52.43 秒）。各失败保留原预算、原断言和日志；四项目标仍未闭合。Windows 最终 fmt/check/clippy 与文档后 diff --check 另存最终日志。

最终 Windows fmt/check/clippy(-D warnings) 与 diff --check 通过（target/external-membership-final-checks.log）；所有本批验证进程已结束。已验证的代码变更是公开并集收集入口的准入顺序与运行循环消费外部认证成员/安全状态变化，文档同步。遗漏义务的 quorum 封口后收敛、完整根下未完成任务和全量压力超时仍保留失败证据，目标继续进行。

### 2026-10-08：区分交接停滞与验证夹具干扰

历史分配证明已有独立发送路径；本批不据此猜测缺少正文证明，不修改 wire。已完成正文重传实验通过（target/late-certified-retry-reproduction.log），未复现预想的重复受理错误，实验测试已撤回。

并行治理测试实际出现初始化 AlreadyInitialized（target/handoff-business-pressure-diagnostics.log）。相关夹具此前仅以进程号和时钟命名，不能保证进程内并发唯一；复用 prepared 测试的 temp_store 并加入标准库原子序号，BFT 和治理测试删除重复命名实现。该修复仅隔离测试文件，不能充当生产连接超时已解决的证据。

四节点交接只在失败时增加每项业务的完成状态、所有权、阶段、Abort 标记、终结证据和投票轮次诊断，不输出正文或签名，不改变十秒预算及四项义务断言。五项交接来源测试通过（target/handoff-business-state-diagnostics.log）。完整库复跑 83 通过、2 失败、1 忽略（target/handoff-business-full-diagnostics.log，28.65 秒）：外部激活恢复回归在三秒原预算内超时；自动运行交接三节点安装三任务根，遗漏节点留在集合 1。新增诊断证实被遗漏任务在原节点仍持有 Voting 身份，在三个集合 2 节点仅为无终结证据的 Prepared witness；其余三任务均成功，无连接/发送失败。缺口仍是集合封口后的遗漏义务，不能将 witness 当作所有权或扩大预算掩盖。

本批完整门禁随后记录；四项目标仍在进行，旧失败证据保留。

本批门禁结束：Windows fmt/check/clippy(-D warnings) 通过，库 85 通过、1 忽略；主集成 246 通过、1 失败、1 忽略，反向提交/重启后的分配收敛仍失败（target/handoff-diagnostics-windows-gates.log，58.81 秒）。Windows 34 节点恢复通过（target/handoff-diagnostics-windows-remaining.log，90.01 秒），examples 通过。相同归档源码 WSL fmt/check/clippy 与完整 all-targets 通过：库 85 通过、1 忽略，主集成 247 通过（48.32 秒），34 节点恢复及 examples 通过（target/handoff-diagnostics-linux-gates.log）。前述完整库诊断运行的两项失败及 Windows 分配失败仍有效，不据本次通过声称交接、迟到历史正文或连接稳定性已全部解决。所有本批验证进程已结束；本批没有生产协议修改。

### 2026-10-08：收集阶段与投票封口的所有权边界

独立回归复现：仅持久化 CollectingTransition、尚无任何成员提案或票据时，普通合法任务 prepare 已返回 TaskAdmissionClosed（target/collection-ownership-reproduction.log）。这把尚可原子扩充的收集阶段误当成不可变投票根，阻止收集期间迟到义务通过原业务校验获得所有权。

save_with_prepared_and_collection 现在仅对已经提升为 Transition 的根关闭新增所有权；CollectingTransition 的新增任务仍走原业务、签名、资源校验，并由现有 write_next_unlocked/refresh_collecting 在同一持久化事务重新捕获唯一收集根。没有引入额外缓存、定时器、委员会全局排序或第二份收集真值。promote_transition_collection 仍在原存储锁内校验当前根覆盖，旧摘要不得提升；封口后的 ownership gate 及不可撤销票锁原样保留。

新增回归验证收集中新增义务、冷加载覆盖、旧根提升拒绝且不写入、新根提升及封口后新增所有权拒绝。该修复解决过早关闭的受理边界，尚不能证明 quorum 根不会遗漏未传到投票者的义务；四节点全量并集和稳定性仍须原预算测试验证。

边界回归修复后通过（target/collection-ownership-fixed.log，0.04 秒）。Windows fmt/check/clippy(-D warnings) 通过，完整库 86 通过、1 忽略；主集成 246 通过、1 失败、1 忽略（target/collection-ownership-windows-gates.log，79.30 秒），失败是超过候选窗口的分配积压恢复排空，反向提交分配和交接库回归本次通过。34 节点恢复及 examples 通过（target/collection-ownership-windows-remaining.log，62.03 秒）。相同归档源码 WSL 完整 fmt/check/clippy/all-targets 通过：库 86 通过、1 忽略，主集成 247 通过（47.99 秒），34 节点恢复通过（56.99 秒）及 examples 通过（target/collection-ownership-linux-gates.log）。原预算和完整根断言均未修改；旧交接遗漏与压力失败不能被本次通过消除，目标继续。所有验证进程已结束，最终文档后 diff --check 另存日志。

### 2026-10-08：分配超时的失败状态证据

积压失败诊断先按原预算复跑完整主集成（target/backlog-frontier-diagnostics-run.log）：244 通过、3 失败、1 忽略，积压场景通过；exhausted allocation、反向提交分配及成员/地址前沿竞争失败。仅凭这些超时不能认定连接丢失是原因。初始诊断编译误用了 crate-private finality getter，已改用现有公开 prevote QC，不扩大 API。

已把积压、反向分配、前沿切换及耗尽场景的失败状态输出集中到测试 support/allocation_diagnostics：保留原时间预算与业务断言，失败后记录节点版本、地址前沿、generation、任务终态、BFT 轮次/锁/QC 与错误计数，替换整份正文和证书的 Debug 输出。只在失败时读取，不改变生产运行与通过路径。

排查中尝试复用收据的 allocation 验签缓存；安全测试及生命周期核对表明任务成功会清除 TaskBinding.allocation_certificate，长期重复验签假设不成立，已全部撤回该优化及实验测试，不将其报告为修复。当前保留的改动仅是有定位价值的失败诊断。修正后的本地门禁及诊断复跑随后记录，四项目标继续。

本批证据已收齐：Windows fmt/check/clippy(-D warnings) 通过，库 84 通过、2 失败、1 忽略（target/allocation-diagnostics-windows-gates.log，17.49 秒）。人工交接四节点均持有四任务候选并在 round 3 锁定/prevote/precommit 四任务根但未 finality-ready；真实运行仍三节点安装三任务根，遗漏节点停在集合 1，两场景均无连接/发送错误。

Windows 主集成 245 通过、2 失败、1 忽略（target/allocation-diagnostics-windows-integration.log，67.48 秒）：反向提交失败时四节点地址前沿均为 6，第一业务成功、第二业务仍 Pending，节点 1 第二业务投票在 round 1 无锁/QC，其他节点无该业务投票状态，拒绝/连接/发送计数均为空；故此失败不能称为地址分配前沿未推进。耗尽场景前沿已达 u64::MAX，一项 Issue 成功，另一项 Issue 和之后 RegisterAccount 尚未完成，仅有预期 CurrencySequenceSpaceExhausted 拒绝。积压恢复本次通过。Windows 34 节点及 examples 通过（target/allocation-diagnostics-windows-remaining.log，77.21 秒）。

相同归档源码 WSL fmt/check/clippy 与库通过（86 通过、1 忽略），主集成 241 通过、6 失败（target/allocation-diagnostics-linux-gates.log，58.05 秒）：discovery response stall、private pull、public checkpoint、active-set refresh、validator reconnect 及 historical witness abort。分配场景通过。失败后测试 runtime unwind 的 cancelled worker 信息不能直接当成生产根因。WSL 34 节点及 examples 的测试结果通过（target/allocation-diagnostics-linux-remaining.log，64.25 秒）；补跑包装器在测试结束后因退出状态 shell 变量为空返回 1，已逐项读取日志确认测试结果，未用包装器状态代替测试证据。

检查 Windows 与 WSL 没有发现此前遗留的 Second/integration/runtime-validator-discovery 进程；这不构成系统无资源压力的证明。撤回的四个生产/实验文件与本批前归档逐行比较，差异均为零。当前仅保留测试失败诊断和文档，没有生产协议优化；下一步调查已分配业务未启动/未推进及调度延迟。所有本批验证进程已结束，四项目标仍未完成。

### 2026-10-08：认证前沿补齐后的业务恢复

复用 mixed_snapshot_probe 的 --bft 安全输出，补充 generation/frontier/本地 ownership；默认私有业务正文输出模式不变。上批保留的反向分配节点 2 快照确认前沿 6、第二任务未完成且 owned=true，无该任务持久 BFT 状态。该证据不直接证明其运行时是否已注册会话，继续区分队列恢复与调度延迟。

新增真实 NodeRuntime::run 回归：单节点先完成普通账户业务以确认原 runner 已工作，再通过原 StateStore.install_currency_allocation 写入有效 quorum 分配证明并触发既有 wake；不注入本轮 BFT allocation output，要求发行任务在原三秒预算内完成并冷加载验证原分配区间/证书、余额和无剩余计划。修复前失败（target/external-frontier-reproduction.log，3.40 秒）；修复后通过（target/external-frontier-fixed.log，0.54 秒）。

运行工作游标在既有成员版本、安全状态之外记录 canonical next_currency_address。现有驱动入口观察持久前沿变化；原成员/安全回调仅在相应字段变化时执行，原 retire_superseded_allocation_sync/resume_currency_allocations 在持久变化时执行一次。自身认证提交后仍即时检查同一游标；移除基于本轮 output 的第二份无条件队列恢复，避免重复扫描。游标仅是进程内工作标记，不是协议真值、地址分配器或权限；没有新后台循环、定时器、投票权或全局排序。认证安装路径仍必须通过原分配、签名、原子写入与安全验证。

这是实际复现的补齐/恢复缺口；不能据此声称上批压力超时或交接遗漏均已修复。完整两端门禁随后记录，四项目标仍在进行。

首轮 Windows fmt/check/clippy 与库 87 通过、1 忽略（target/external-frontier-windows-gates.log，13.67 秒），新认证前沿恢复回归通过；主集成仍有分配/前沿/CLI 失败，结果不得算作完整稳定。继续追踪 BFT 轮次启动发现 start_round 对只读 durable 使用 StateStore.load，后者将整个 cached PersistedNodeState 深克隆；现一行替换为已有 load_shared，复用原 commit token/外部变更/损坏校验缓存。不合并不同职责的验证，不删状态/签名校验，不引入新缓存；仅避免每 scope、每轮复制全部业务和准备任务。既有完整 BFT/并发/缓存损坏回归验证该只读路径，不增加同义测试；最终源码门禁另存日志。

### 2026-10-08：同 scope 多冻结候选的来源拉取丢失

实际复现来源队列只按 ConsensusScope 保存拉取状态：第二个不同摘要的公告会替换第一个已接收分片的 fetch，即便第一个正文尚未完成。既有乱序来源回归增加第二个候选后失败（target/distinct-source-fetch-reproduction.log，0.05 秒）。该问题同时影响共享地址前沿的交接候选和同任务冻结变体，不能将新公告视为旧候选已失效。

现复用原 PreparedTaskSyncState，将 fetch 索引改为 (scope, expected_plan_digest)；各候选独立维护来源、分片与重试，成功安装只清理对应摘要。业务终局或已认证前沿淘汰仍清理整个 scope。移除原 authoritative 替换开关：来源公告不授予资源、投票或终局权限，正文安装仍经过原验证。沿用原最多 64 个拉取记录、聚合字节预算、分片限制和超时，不新增后台线程、计时器或协议版本。回归还验证完成/拒绝旧候选不能删除另一候选，随后记录验证结果。此修复不等于完成交接义务闭合，过早提升候选根的问题仍需继续处理。

本批 Windows 最终源码验证：fmt/check/clippy(-D warnings) 通过；来源拉取两项回归通过（target/distinct-source-fetch-regressions.log，0.04 秒），交接来源五项定向测试通过（target/distinct-source-handoff-tests.log，5.69 秒）。完整库门禁仍为 85 通过、2 失败、1 忽略（target/distinct-source-windows-gates.log，19.25 秒）：人工驱动四节点均已收齐四任务候选且处于 round 3 锁定/预提交，但未在原预算内完成；实际运行另一次仍复现三节点安装三任务根而第四节点留在旧集合。不能以定向通过替代交接闭合或完整稳定。主集成 242 通过、5 失败、1 忽略（target/distinct-source-windows-integration.log，74.23 秒）：竞争/耗尽/认证范围恢复分配、成员前沿协调及私有来源拉取；65 请求积压场景本轮通过。34 节点恢复通过（target/distinct-source-windows-remaining.log，75.98 秒），examples 通过。最新源码同步 WSL 后的结果另录，四项目标保持未完成。

同归档源码 WSL 最终完整门禁全部通过（/home/why23/second-validation-20261003/target/distinct-source-linux-gates.log）：fmt/check/clippy(-D warnings)、库 87 通过/1 忽略（13.56 秒）、主集成 247 通过（78.29 秒）、34 节点恢复通过（55.44 秒）、examples 通过。包括两个自动四节点交接、认证新成员轮换/恢复/业务、外部认证前沿恢复以及来源多候选回归。两端进程均已结束；Windows 交接两项和主集成五项失败仍保留，不能以 WSL 通过宣布整体完成。新增来源回归验证同 scope 不同摘要同时存活、原分片不被覆盖、完成/拒绝旧摘要不伤其他候选、64 个候选总上限和 scope 终局整体清理；全程不写业务或投票状态。下一条主要修复路径仍是交接并集的认证闭合与过早封根，而非放宽超时或丢弃遗漏义务。

### 2026-10-08：排队消息权限失效误断开认证连接

真实发送 worker 回归复现：普通与 finality 队列分别排入旧前沿的合法消息，随后把同一 peer 的本地权限更新为新委员会、仍保留旧 PreparedTask 来源权限。原 worker 将发送前本地校验产生的 BftUnauthorized 当作传输损坏，关闭整条认证连接并终止所有其他在途发送（target/stale-authority-connection-reproduction.log，0.10 秒失败）。这不是远端身份认证失败，也未放宽接收端验证。

现唯一发送 worker 保留原 SendFailed 记录和唤醒，对 send() 写入网络之前的本地 BftUnauthorized 拒绝只丢弃该消息；其他传输/任务错误仍关闭连接并退出。没有新增重试或计时器。扩展原真实有界双队列回归，同时验证两类失效消息均被拒绝、现有同一连接仍能发送合法继承任务消息、permit 保留，以及 worker 真正关闭后仍释放连接和 permit。验证结果随后记录。该修复不证明全部冻结来源超时已消失，也不等于交接闭合完成。

交接设计方向保持 quorum 换届能力：单个拒答成员不能拥有永久换届否决权。切换证明须保留既有认证结果；未进入已认证交接根的少数本地请求，需要证明约束下的安全保留与重新受理，不能直接删除、解除已有 Commit QC 或假设所有缺席正文可被收齐。协议规则与实现仍需完成；既有四节点完整并集回归保持原要求，不能改成允许遗漏来通过。

发送端修复的同源码完整验证：Windows fmt/check/clippy 通过，库 86 通过/1 失败/1 忽略，主集成 246 通过/1 失败/1 忽略，34 节点恢复及 examples 通过（target/stale-authority-windows-*.log）。WSL fmt/check/clippy 通过，库 87 通过/1 忽略，主集成 240 通过/7 失败，34 节点恢复及 examples 通过（target/stale-authority-linux-*.log）。Windows 仍复现冷交接三任务根及竞争分配失败；WSL 的来源拉取、成员前沿、检查点、发现及 CLI 失败仍保留，不能报告完整通过。

### 2026-10-08：换届后迟到控制消息误断开接收连接

继续扩展同一真实 QUIC 双队列回归：发送端仍处于旧委员会，接收端先切换到新委员会，再收到原本有效的旧前沿 finality vote。旧接收端按当前权限拒绝并结束整条连接（target/stale-receive-reproduction.log，0.32 秒失败）。现在每条认证连接仅保留握手时已信任的原委员会；对于没有当前或终局接收权限、版本已过期的非 PreparedTask 控制消息，复用唯一发送者/签名/证书验证器按该委员会验证，通过后丢弃而不交付业务或终局处理。未知来源版本、非成员、无效签名仍拒绝；PreparedTask 和已有终局 audience 仍沿用原权限路径。保留该连接级认证锚不授予旧委员会新投票权，不新增协议版本、存储副本或计时器。

回归同时验证迟到合法消息不会被交付、连接不重拨仍能传递合法继承任务、普通/finality 两类排队失效消息仍被发送端拒绝，以及同一连接收到伪造旧委员会签名时仍 fail-closed。定向通过（target/stale-receive-fixed.log，0.41 秒）；两端最终门禁随后记录。此修复仍不能替代尚未实现的交接闭合及认证遗漏请求处理。

委员会交接选择维持 quorum 换届，具体遗漏处理前提与剩余实现边界见 [conflict-arbitration](docs/conflict-arbitration.md) 的“委员会交接取舍与剩余实现边界”。强制收齐所有旧成员会引入单成员否决；有效切换证明也不能当作无条件清除旧锁的许可。认证关闭根外旧签名、保护 Commit/Abort QC 与终局证据、逐候选处理、保留原请求与已认证地址区间、原子重试及历史来源权限仍须实现验证。

接收端修复的最终源码门禁：Windows fmt/check/clippy(-D warnings) 通过；库 86 通过/1 自动运行交接失败/1 忽略（target/stale-receive-windows-gates.log，17.32 秒），主集成 245 通过/2 失败/1 忽略（target/stale-receive-windows-remaining.log，78.21 秒，竞争分配与 65 请求积压排空）；34 节点恢复通过（77.35 秒），examples 通过。WSL 同归档源码 fmt/check/clippy(-D warnings) 通过；库 86 通过/1 同类自动交接失败/1 忽略（target/stale-receive-linux-gates.log，16.01 秒），主集成全部 247 通过（target/stale-receive-linux-integration.log，48.21 秒），34 节点恢复及 examples 通过（target/stale-receive-linux-remaining.log）。两端误断连/伪造旧签名回归通过；两端自动交接仍实际出现三任务根，第四节点停留旧委员会，Windows 分配失败仍保留。所有验证进程已结束；四项目标仍未全部完成，不以本批网络修复宣布开发闭合。

### 2026-10-08：来源公告在换轮和多冻结候选中丢失

新增一条真实 QUIC 回归，使用同一 Transfer 请求的两个合法、已取得本地资源权的冻结选币变体。唯一持有者先向原 proposer 公告；原 proposer 只发出 offset=0 拉取请求便消失，随后本地持久轮次经过自任 proposer 到下一位 proposer。旧代码在首次请求时删除公告（target/proposer-announcement-reproduction.log，0.14 秒失败）；只保留公告仍复现固定旧 proposer 导致下一位收不到来源（target/proposer-rotation-reproduction.log）。修复后又实际复现第二个已拥有冻结变体未公告（target/variant-announcement-reproduction.log，3.20 秒失败）：登记路径只认可主候选摘要。

现在拉取请求不作为传播完成确认；复用原有效 proposal 确认及 scope 终局清理。公告与拉取一样按 (ConsensusScope, candidate_digest) 保存；原既有重试读取一次 canonical shared snapshot，按相同原委员会的持久 BFT 轮次更新 proposer，自任 proposer 时保留待传播项而不发送给自己。登记路径识别所有已拥有候选；首次公告只发送当前项，避免每添加一个变体就重发整批形成二次方 fanout。迟到旧 proposer 确认不得删新 proposer 待传播项，单候选确认不得删其他变体。沿用既有重试时间和认证传输，公告仍无资源、投票或终局授权；旧委员会解析只取当前或受保留的精确原委员会。未将普通请求全局排序，未增加协议版本、网络类型或后台循环。

同一 QUIC 回归验证上述三条真实失败路径、两个变体都到达新 proposer、迟到/单候选确认的隔离、scope 终局整体清理及公告重试不写持久业务/投票状态。定向测试结果及两端完整门禁随后记录。此修复不等于交接根已包含缺席节点的不可知正文；认证遗漏恢复、健康节点封根前完整并集及暖业务基线处理仍按原目标继续。

本批最终源码验证：来源同步三项回归全部通过（target/variant-announcement-fixed-final.log，0.12 秒）。Windows fmt/check/clippy(-D warnings) 通过；库 85 通过/3 失败/1 忽略（target/variant-announcement-windows-gates.log，20.43 秒，两个自动交接及外部前沿恢复测试的最初热身业务超时）；主集成 236 通过/11 失败/1 忽略（target/variant-announcement-windows-remaining.log，111.40 秒，包括分配、私有来源、冻结变体、历史 abort、换届和业务 CLI），34 节点恢复通过（82.73 秒），examples 通过。同归档源码 WSL fmt/check/clippy(-D warnings)、库 88 通过/1 忽略（13.22 秒）、主集成 247 通过（48.20 秒）、34 节点恢复及 examples 全部通过（target/variant-announcement-linux-gates.log）。两端验证进程已结束，git diff --check 通过。Windows 失败不能由 WSL 通过抵消，也不能在没有定位证据时归因于本次公告修复、硬件或压力；健康完整并集与认证遗漏处理仍未完成，四项目标保持进行。

### 2026-10-08：封根后的根外 Abort 签名缺口

继续原“收集允许晚到本地义务、封根关闭所有权”回归，新增一个由其他节点真实准备、签名和验证的 late witness。它不取得本地资源权，却原本能在既有封根未列入该 TaskId 时签出旧委员会 Abort prevote（target/sealed-root-signing-reproduction.log，0.21 秒失败）。只冻结新资源不足以支持认证遗漏恢复：根外旧 Abort 仍可能与重新受理后的新 Commit 相冲突。

现在唯一持久化签名上下文同时约束资源和签名边界：每个当前已封根的候选都必须包含该 PreparedTask 的原委员会与 request digest；已切换委员会后的旧 PreparedTask 必须属于当前认证安装的交接。prevote/precommit 与 finality 共用该校验，并收敛 finality 中原有重复安全状态/签名版本/委员会校验。collecting 阶段仍不关闭签名，已覆盖任务仍可推进；原 QC 验证、观察和持久保存路径不套签名拒绝规则，因此不会把验证迟到证据等同于授予旧投票权。proposal 本身不是终局授权，签名前仍必须通过原资源与摘要校验。

扩展原回归验证：已封根后新增的合法 foreign witness 不能获得根外 Abort 签名，拒绝不增加持久 generation，原已纳入任务仍可签出合法 prevote（target/sealed-root-signing-fixed.log，0.22 秒）。不清除任何锁、证书、正文或旧签名，不引入全体成员否决、全局排序、额外协议版本或后台任务。本批补的是认证遗漏处理所必需的旧签名关闭前提；遗漏请求保留/重新受理和封根前完整并集仍未实现闭合，完整两端门禁另记。
### 2026-10-08：封根签名修复的完整验证结果

本批 Windows fmt/check/clippy 通过，库测试 88 通过、1 忽略；主集成 245 通过、2 失败、1 忽略。失败为竞争分配收敛、成员变更与分配前沿协调，日志 `target/sealed-root-signing-windows-gates.log`。其余 34 节点恢复与 examples 通过（`target/sealed-root-signing-windows-remaining.log`）。

同源 WSL fmt/check/clippy、库测试通过；主集成 246 通过、1 失败，失败为四节点 CLI 重启流程的恢复检查点响应超时（`target/sealed-root-signing-linux-gates.log`）。另行完成的 34 节点恢复与 examples 通过（`target/sealed-root-signing-linux-remaining.log`）。本批不能据此宣称跨平台全绿或完整交接完成。

### 2026-10-08：启动失败后的空闲共识会话恢复

真实持久化回归证明：分配提案启动时，已有同轮 Nil prevote 会使 digest prevote 正确拒绝；该次失败没有设置会话 deadline。随后合法 Nil precommit QC 推进轮次，再次注册完全相同的任务，旧实现仍直接返回，导致没有期限也没有启动标记的会话永远空闲。复现日志为 `target/idle-session-reproduction.log`（0.10 秒失败）。

统一 coordinator 注册路径现在只为未结束且没有 deadline 的已验证会话恢复 `needs_start`，涵盖相同任务重试与后来抵达的候选；已有期限不被续期，完成结果和持久化投票均不清除，不新增扫描或计时器。随后进一步消除启动失败根因：`start_round` 对已持久化 prevote/precommit 恢复原阶段的期限，不再尝试另一值的提案。最终回归 `restarted_nil_voter_advances_without_conflicting_proposal_or_deadline_renewal` 在缺少阶段恢复时失败（`target/idle-phase-reproduction.log`），修复后正常期限形成真实签名 Nil QC，再进入下一轮完成分配（`target/idle-phase-fixed.log`，0.15 秒通过）。冷读确认原请求摘要、连续地址区间及分配证明；重复注册不续期、不增加持久 generation，完成后旧前沿仍拒绝。此前显式推进 QC 的初步回归日志为 `target/idle-session-{reproduction,fixed}.log`，最终阶段恢复回归已替代它。本批尚不能证明此前平台级失败的根因，也没有完成交接遗漏恢复。
### 2026-10-08：原阶段恢复的最终门禁

最终源码 Windows fmt/check/clippy(-D warnings) 通过；库 88 通过、1 失败、1 忽略（17.11 秒），失败仍是手动驱动的四节点自动交接并集收集。补跑主集成 241 通过、6 失败、1 忽略（111.99 秒）：竞争分配、成员前沿协调、耗尽前沿下的其他业务、超过候选窗口的分配队列、私有正文拉取、业务 CLI 丢确认重试。34 节点恢复通过（63.15 秒），examples 通过。日志 `target/idle-phase-windows-gates.log`、`target/idle-phase-windows-remaining.log`。

同一份源代码归档 `target/idle-phase-exact-source.tar` 在 WSL 完整执行 fmt/check/clippy(-D warnings)/test --all-targets：库 89 通过、1 忽略（13.16 秒），主集成 247 通过（49.31 秒），34 节点恢复通过（59.99 秒），examples 通过，所有进程均结束。日志 `target/idle-phase-linux-gates.log`。此前仅显式重注册修复的 Windows 初步库验证记录保留在 `target/idle-session-windows-gates.log`，不能冒充最终源码门禁。Windows 剩余失败与交接遗漏安全释放/重新接纳仍待解决，四项目标继续进行。
### 2026-10-08：交接收集使用现有 BFT 的一个完整轮次，不再首个唤醒就封根

操作请求和收集正文现在先持久化 `CollectingTransition`、交换贡献，再注册同一 membership/frontier scope 从本次收集起始轮次开始的 Nil 阶段。该阶段使用原有 proposal/prevote/precommit 期限和持久化轮次；进入后续轮次才把当前完整已验证并集提升为可投票根。没有新增 scope、定时线程、全体成员屏障、普通任务全局排序或协议版本。收集中的稳定 intent 摘要仅作为内存注册键，根变化覆写同一候选，不把每份未封根并集堆入 64 项业务候选窗口；原持久化签名函数同时拒绝收集正文摘要与 intent 摘要的 digest 投票。候选选择仅使用当前权威快照内已封根正文；治理检查放在原 governance 模块，不继续堆入共识主文件。

真实初步验证中，四节点虽完成并集和切换，连接建立时先于交接意图抵达的普通任务公告却让私有拉取重新准备了远端任务（`target/collection-ownership-scope.log`）。现在收集节点的初始传播不重复发送已由交接覆盖的独立业务公告；收集期间迟到的自动拉取只能复用精确已验证见证，未知来源暂不取得新所有权。明确提交的本地任务、已有资源权以及独立运行中的任务共识仍走原路径；认证切换后由现有运行循环恢复继承任务的独立资源验证与业务共识。不是通过删除见证、改宽权限断言或延长测试期限通过回归。

最终针对性三个真实 QUIC/冷恢复回归通过（`target/collection-round-domain-final.log`，6.70 秒）：在线四节点收齐四份独有任务且保持原本地/远端权限；运行四节点完成四项业务；一个旧成员离线时在线 quorum 切换，离线成员原任务和状态不被篡改。原四节点根必须包含 4 份正文的断言保留。该回归没有证明离线成员归来后的认证遗漏恢复，后者仍未完成；首轮期限也不承诺收齐不可达或任意延迟成员。完整跨平台门禁结果另记。

收集起始轮次是 coordinator 的内存工作游标：已有非零轮次不会直接触发封根；冷重启从权威持久化轮次重新完成收集，不新增持久化格式或第二份真值。一个新封根候选在尚未投票的 Proposal 阶段立即启动，重复注册不续期，已保存 prevote/precommit 则继续原阶段。最终针对性验证：三项自动交接（含第 4 轮开始并冷重启）通过，原原子并集/过期校验、分块交换候选及新成员基线重入测试通过；日志 collection-relative-round-final.log、collection-admission-relative-final.log、collection-chunked-relative-final.log、collection-reentry-relative-final.log 均在 target 下。中间全库曾有 5 项失败，不能把此前针对性通过当作全门禁完成；当前源码完整门禁另记。

签名仍被恢复栅栏锁住的新成员不参加收集 Nil 投票，但可以复用完整正文的原严格受理路径注册观察会话、接收 quorum 证明。仅收集元数据不会封根；未验证正文仍拒绝，观察不会解锁签名、导入旧票或跳过密钥轮换。此前真实五节点重入测试停在换届到第 3 版（collection-reentry-running-backtrace.log），两端完整库也复现；本次在 collection 的统一入口修复观察与签名阶段混淆，沿用原五节点轮换、恢复、业务及冷读权限回归，不延长超时。

观察修复的首个尝试仍失败：诊断确认旧四成员已到第 3 版，锁住的新成员停在第 2 版，33 次 TransitionCollectionIncomplete。严格验证后复用原 promote_transition_collection 将同一完整根转为可观察记录，原签名恢复栅栏不变；最终真实五节点回归通过（target/collection-locked-observer-final.log，6.05 秒）。原回归新增仅在失败时输出阶段、版本、签名准备状态和拒绝计数的诊断，未增加新测试或期限。
成员与发行前沿集成回归原先假定操作提交就封根；现按明确的收集阶段规则，验证随后提交的本地注册请求被绑定并最终与原发行一起完成，连续发行地址和旧前沿拒绝断言保留。封根后关闭签名及无持久化副作用仍由原共享存储回归验证，未放宽四节点完整 root 和本地/远端所有权断言。

### 2026-10-08：收集轮次与锁定成员观察的最终源码门禁

Windows fmt/check/clippy(-D warnings) 通过；库 87 通过、3 失败、1 忽略（17.18 秒），失败为三项自动交接/继承业务回归，新成员轮换恢复回归通过。主集成 240 通过、7 失败、1 忽略（85.96 秒），涉及竞争分配、耗尽前沿、队列、私有拉取、成员前沿/重启及业务 CLI；34 节点恢复通过（43.92 秒），examples 通过。日志 target/collection-observer-windows-gates.log、target/collection-observer-windows-remaining.log。

同源代码归档 target/collection-observer-exact-source.tar 的 WSL fmt/check/clippy(-D warnings) 通过，库 90 通过、1 忽略（13.39 秒），包含五节点恢复与三个自动交接；主集成 243 通过、4 失败（69.15 秒），为两个发现/首请求超时、四节点 CLI 提交超时、单节点 CLI 检查点证明未持久化。日志 target/collection-observer-linux-gates.log。另行补跑的 34 节点恢复通过（59.77 秒），examples 通过，日志 target/collection-observer-linux-remaining.log；两端验证进程均已结束。

完整 Windows 回归已确认部分节点采用包含 3 项任务的认证根，另一个收齐 4 项的节点保持严格拒绝；另一次离线成员用例停在各自单项贡献，运行用例有一节点尚未补齐末项业务。有限 Nil 阶段只能延后封根，不能证明所有正文已汇集或保证迟到根进展。本批不能宣布四项目标完成，也不把 Windows 失败归因于硬件或用单独通过覆盖它；认证遗漏安全退休/请求重新受理、来源收敛及超时仍需实现和验证。

### 2026-10-08：认证切换中的本地遗漏权限退役

普通未认证提案继续要求完整本地覆盖；只有旧 quorum 的完整切换证书验证后，才在存储写锁内解析其精确正文、校验基线、退役未受保护的遗漏权限、保存原请求并安装新委员会/证明，合为一次原子写入。缺少或错误正文、继承认证义务遗漏或遗漏受保护证据所需正文均拒绝退役；Commit QC/非 Nil 投票/锁/最终性签名保护精确候选，Abort 保护原请求上下文，已认证连续分配保留原区间及其正文，失败不改变磁盘 generation。此前已受理根忽略新增 foreign witness 的例外，现在不能忽略该见证后来收到的 Abort 或其他保护证据。

完整遗漏保留 TaskId、请求摘要和原签名进入现有请求队列；局部遗漏候选保留原正文作无所有权见证，只保留认证根覆盖候选的既有资源权。复用 allocation_task 原持久字段承担准备重试，零分配请求也走原请求队列，未建立第二份正文或修改协议开发版本。原成员证明追赶先复用收集器验证并导入无权限见证，再原子应用证书中的实际根；收集器形成的并集不替代 quorum 已签的根。委员会变更唤醒仅移除已由持久认证切换退役的旧任务内存会话，不清除任何持久签名或锁；冷启动沿用同一请求重试路径。

保护索引复用一次临时扫描，不建立第二份持久真值。同一请求的候选 1 已有 Commit QC 时，认证根可以退休未投票的候选 2，保留全部 65 份见证并恢复被根覆盖的候选为主计划，原 QC 不清除。业务准备拒绝继续保留原签名队列及已认证地址区间；业务认证完成回调复用现有恢复入口自动重试，无新轮询或定时器。冷恢复、热会话退休、Commit/Abort 保护及局部候选回归通过；原四节点自动交接和离线 quorum 回归 4 项通过（target/certified-cut-automatic-final.log），原业务依赖恢复回归通过（target/certified-cut-business-rejection-final.log）。完整两端门禁继续验证，尚未据此证明所有在线贡献均被汇集、历史前缀全面验证或来源超时全部解决。
首轮 Windows 完整库验证 88 通过、4 失败、1 忽略（target/certified-cut-windows-gates.log）：三项自动交接/业务收敛超时，以及一项仍要求业务过期后清除原签名的旧断言。过期回归现同时断言原签名保留、无准备计划/投票/分配、地址前沿与余额不变；保留请求并不续期。真实 QUIC 证明追赶回归加入接收方独有遗漏任务，验证错误 authorizer 无写入/无权限退役，合法认证正文才退休旧计划并保留原签名队列（target/certified-cut-omitted-proof-chase.log 通过）。

源重试核查发现封根后不再进入原源公告重试，而一次公告丢失或暂时拒绝仍可能发生。现原共识 runner 的既有重试期限覆盖未完成的 collecting/sealed 交接正文，按其实际阶段发送，认证切换后 pending 记录消失即停止；无新循环、配置、持久表或延长期限。修复后自动交接与冷恢复 4 项针对性通过（target/certified-cut-source-retry.log），尚不据此断言完整负载下已稳定。业务依赖完成自动恢复回归通过（target/certified-cut-business-wake-final.log）。
### 2026-10-08：认证遗漏及源重试的同源完整门禁

Windows fmt/check/clippy(-D warnings)/diff-check 通过；完整库 89 通过、3 失败、1 忽略（16.92 秒），失败为三个自动交接回归。主集成 244 通过、3 失败、1 忽略（72.67 秒）：相反顺序竞争分配、发行与换届共用前沿、首票前治理重启恢复；34 节点恢复通过（67.75 秒），examples 通过。日志 target/certified-cut-source-windows-gates.log 与 target/certified-cut-source-windows-remaining.log。完整失败确认有限收集轮次仍可形成部分认证根，另一个持有更大根的节点未跟进；另一运行场景切换到完整 4 项根但仅完成 2 项业务。公告重试修复不能视为该收敛问题已解决。

同源归档 target/certified-cut-exact-source.tar 在 WSL fmt/check/clippy(-D warnings) 通过；库 92 通过、1 忽略（13.34 秒），包含认证遗漏、真实正文追赶、原四节点根及权限断言。主集成 243 通过、4 失败（57.51 秒）：私有正文拉取、发现阻塞时验证者重连、首票前换届重启、业务状态变化后恢复检查点重启；34 节点恢复通过（90.65 秒），examples 通过。日志 target/certified-cut-linux-gates.log 与 target/certified-cut-linux-remaining.log。两端进程均已结束，失败夹具保留。

本批交付认证切换的原子遗漏退役、精确 Commit/Abort 证据保护、原签名/分配范围保留和业务依赖完成后的自动重试；完整负载下交接/分配/来源收敛仍失败，历史前缀与既有成员业务基线全面恢复仍未闭合。四项目标保持未完成，外部业务或独立主机不作为开发完成边界。
### 2026-10-08：单验证者恢复避免空收集轮次

单独复现首票前治理重启回归通过但耗时 6.63 秒（target/membership-restart-isolated-current.log）。当前唯一验证者已掌握全部委员会贡献，没有远端正文可等待；原 advance_transition_collections 入口据此直接提升严格验证后的本地根。多成员继续原收集轮次；单成员仍须完整正文/业务基线/义务覆盖校验和原 BFT/finality 持久证明，签名恢复栅栏不变。消除无意义 Nil 轮次，不提高超时或跳过认证。针对性及完整验证结果另记。
### 2026-10-08：活连接中的认证切换正文

静态追踪确认旧实时正文入口仅调用 begin_transition，较小根即使已有最终 quorum 证书仍被当成未认证提案，无法使用认证遗漏退役；重新握手的 membership_sync 与活连接行为不一致。现 CurrencyAllocation 范围最终证书先验证，未知正文沿原有 bounded source fetch 获取并缓存在原 pending 消息中；正文到达后复用通用 pending_finality_certificate（原 prepared-only 命名方法），验证精确声明、签名和正文后导入见证并原子应用该认证根。较大的本地并集不改写旧 quorum 根；原本地资源遗漏使用上一批共享安全退役。已认证过期委员会的内存前沿会话在同一退休入口删除，不动持久票/锁。未认证较小根继续严格拒绝；无新消息格式、协议版本、独立线程或全局排序。回归与完整门禁另记。
实时入口同时保留原已经认证/受理的精确根，后续覆盖任务完成时直接复用该正文；不再把它重建到今日业务快照后误拒绝。扩展同一 live-cut 回归验证覆盖注册已完成后的证书切换：保留已提交账户和终态，一次原子写入，原根/签名栅栏不变。已登记的普通分配候选复用原 finality 验证器，不额外验证或拉取其正文；只有未知的当前前沿证书先认证再请求来源，过期范围仍由原 completed proof 路径处理。
手动驱动的冷恢复夹具原先仅调用 retry_prepared_task_sync(false)，没有运行实际 runner 的源重试期限/封根公告重发；已尝试的提供者耗尽后不再重试。夹具现补齐与节点配置相同的一秒绝对重试期限及原公告入口，仍使用同一十秒总期限、四项完整根及严格权限断言，不延长预算或把部分根当成功。此修正只保证测试驱动包含已存在的生产生命周期，不据此否认真实运行/分配的完整门禁失败。


## 2026-10-09：M0 私有账户查询硬化

AccountQuery 结构与账户签名先验证，再加载私有快照。每个账户查询连接从同一不可变快照生成一个类型化短期投影，Currency 只扫描一次，后续分页只切片；构建后释放完整快照。generation 继续表示来源快照的本地写入计数，投影不受后续 BFT 元数据或业务提交影响。响应 total 与查询种类、严格顺序、跨页边界、游标、终页及余额一起验证，库与钱包共用完整查询客户端；balance 不枚举资产。

每页 128 行、每枚举 100000 行、每投影 16 MiB、每进程 4 个投影；额度在加载前取得。总期限 30 秒、等下一页最多 5 秒，不由 keepalive 续期。终页、断开、异常或过期回收投影；超预算拒绝而非截断，冷磁盘读取返回后检查取消。无新增持久化状态、资产真值、共识、广播、长期缓存或兼容格式；未发布协议版本保持 1。读取仍是信任回答节点的私有快照查询，不是独立 BFT 资产证明、最新余额或线性化读取。详细协议与预算见 docs/cli-wallet-design.md。

本地 fmt/check/clippy/all-targets test/diff 门禁通过：105 项库测试、249 项集成测试及 1 项 34 节点发现测试通过；默认忽略 2 项，其中跨进程锁 helper 在子进程实际执行通过，WSL 混合运行未验收。新增 8 项账户查询定向测试及现有真实钱包支付测试中的 assets/history 断言覆盖本次安全、分页、资源回收与功能回归。

### 2026-10-09：Windows–WSL 混合验收改为必做

移除 mixed_windows_linux_validators_pay_and_recover_into_required_quorum 的 ignored 标记，将其纳入 Windows 普通全量测试，并在 AGENTS.md 与 node-operations.md 明确同源 Windows/WSL 原生产物、mirrored 网络及 SECOND_WSL_BINARY 前提。条件缺失必须失败，不能跳过后宣称验收完成。前文和历史文档中的忽略记录保留为当时事实，不代表当前门禁要求。

当前生产输入两端 205 项归一化 SHA-256 一致，原生 Release 混合场景 25.83 秒通过（target/m0-required-mixed-release.log）；默认 Windows 全量门禁库 105 项、主集成 250 项（包含混合场景，0 忽略，44.86 秒）、34 节点目标与 examples 均通过（target/m0-required-mixed-all-targets.log）。fmt/check/clippy(-D warnings)/diff 通过；故意缺失 SECOND_WSL_BINARY 时测试立即失败且 0 忽略（target/m0-required-mixed-missing-env.log），符合必须执行的 fail-closed 要求。本次不改变生产代码、期限、线程数或安全断言；节点与临时状态清理完毕。混合基线覆盖支付和恢复，不冒充私有账户查询跨系统对抗证明。
