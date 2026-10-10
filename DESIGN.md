# Second 系统设计

本文以当前 Rust 源码为依据，说明 Second 的系统目标、业务状态、签名权限、交易执行、共识、持久化、成员切换、网络通信、公开同步和钱包接口。本文记录现行协议语义及实现边界。相关模块在末节列出。

## 1. 系统与工程目标

Second 是独立的数字货币状态网络，原生货币称为 Secoin。网络接受经过授权的 LegalTask，通过验证者委员会确认可执行任务的终态，最终更新账户与 Currency 归属。

Second 的工程目标是安全、去中心化协作、隐私保护、高吞吐与低资源消耗。实现围绕具体 `ConsensusScope` 开展共识，以必要的计算、存储、传输和持久化操作获得拜占庭安全和恢复能力。运行时优先采用事件、期限和有界消息推进，避免无效后台轮询。业务数据以 `SecondState` 为统一权威来源；公开摘要、钱包视图和网络缓存均从相应权威事实派生。

系统内部负责签名和权限验证、对象身份管理、业务准备、冲突处理、最终性、提交、成员管理、节点通信和恢复。外部授权与商业系统负责请求发行或其他特殊操作的业务决定、市场定价、商业承诺和相关服务。

### 1.1 系统主要路径

~~~text
用户钱包或外部签发者
         │
         ▼
带账户签名及必要 Authorizer 签名的 LegalTask
         │
         ▼
Validator 核验网络、签名与业务权限
         │
         ├─ 需要新 CurrencyAddress ─► 地址区间认证
         │
         ▼
绑定 TaskId / request_digest
         │
         ▼
PreparedTask：按序预执行，冻结资源与选择
         │
         ▼
对应 ConsensusScope 的 BFT / Finality
         │
         ▼
持久化 Commit 或 Abort 终态
         │
         ▼
依赖闭包内原子提交 / 认证取消与资源恢复
         │
         ▼
任务状态、私有账户查询和公开 Currency 更新
~~~

## 2. 标识与权限域

Second 使用相互独立的标识和密钥职责。

| 概念 | 源码类型 | 语义 |
| --- | --- | --- |
| 账户 | `AccountAddress` | 32 字节账户公钥身份，文本前缀 `acct_`，注册后永久存在 |
| 支付地址 | `PaymentAddress` | 32 字节支付标识，文本前缀 `pay_`，注册后永久绑定一个账户 |
| 货币对象 | `CurrencyAddress` | `u64` 唯一货币地址，规范 Base62 文本表示，分配后永久消费 |
| 任务 | `TaskId` | 1–128 字节合法 ASCII 标识，永久绑定具体请求摘要 |
| 网络节点 | `NodeId` | QUIC 传输身份对应的节点标识 |
| 验证者 | `ValidatorId` | 委员会注册表中的验证者身份 |

账户使用 Ed25519 密钥签名。`AccountAddress` 的 32 字节对应账户公钥，协议据此校验与账户有关的操作。业务签名、私有查询签名和验证者投票各有独立签名域与输入约束。

一个 `ValidatorCredential` 包含三种互异的公钥：`identity_public_key` 负责验证者身份，`consensus_public_key` 用于 BFT 与最终性签票，`recovery_public_key` 用于安全恢复。网络 `QuicTransportIdentity` 另外维护传输密钥与固定证书。`ValidatorSet` 和 `ValidatorRegistry` 管理验证者 ID、密钥使用历史及生命周期。

## 3. 状态模型与资产归属

`SecondState` 由三层组成：

| 状态 | 源码结构 | 主要字段 |
| --- | --- | --- |
| 协议状态 | `ProtocolState` | `next_currency_address`、`task_bindings`、`task_handoff` |
| 执行前置状态 | `PrerequisiteState` | `payment_executions` |
| 业务状态 | `BusinessState` | `accounts`、`payment_addresses`、`currencies`、`payment_history` |

### 3.1 Currency

一个 Currency 包含 `address`、`role`、`owner`。角色有 `Circulation` 和 `Reserve`。`owner` 为账户地址或空值。每个存在的 Currency 对象具有独立身份。

资产归属的权威事实是 `BusinessState.currencies` 中的 owner。`SecondState::balance(account)` 累加 owner 等于目标账户的区间长度，得到该账户的 Secoin 数量。账户存在性由 `has_account` 单独判定。

~~~text
balance(A) = sum(run.len where run.owner = A)
~~~

`current_supply` 统计当前存在的 Currency；`reserve_count` 统计未被占有的 Reserve 对象。由 `CurrencyAllocation` 确认的地址区间以及 `next_currency_address` 遵守身份单调消费规则；销毁或后续业务失败均不能重新使用已经消费的身份。

逻辑上每单位 Secoin 仍是独立 Currency。完整状态节点使用 `CurrencyLedger` 保存 `{start, len, role, owner}` 区间；不重叠，相邻且 role/owner 相同的区间自动合并。单地址修改拆分并合并，余额和 Reserve 数量由区间长度求和，不另存余额真值。私有快照保存规范区间，解码拒绝空、溢出、重叠和可合并相邻区间；当前格式版本保持 1，无旧格式读取。

账本差分验证以测试专用的逐地址表为参考，混合单地址修改、随机跨度 `set_range` 和 `append_run`，逐步比对内容与规范形；同时比较随机范围 `scan` 的裁剪结果、规范追加恢复与逆序插入恢复。地址窗口覆盖零附近及 `u64::MAX` 前沿附近，单位规模另由十亿单位的分配预算测试验证。

### 3.2 账户与支付地址

账户由 `RegisterAccount` 注册。注册需要该账户私钥的签名。一个支付地址通过 `RegisterPaymentAddress` 绑定一个已经存在的账户；同一个 LegalTask 允许先注册账户再注册其支付地址。

支付地址的生命周期：

~~~text
Active → Retiring → Retired
~~~

`Active` 允许建立新支付；`Retiring` 停止建立以该地址参与的新支付，并保留已建立的在途执行；`Retired` 表示已完成认证退休。最终退休验证活动执行、资源占用和交接约束。地址与账户的永久绑定继续保留。

### 3.3 支付历史

`PaymentExecution` 保存来源和目标支付地址以及支付数量。`OperationClaimId` 将执行关联到 TaskId 和操作序号。成功提交后，`BusinessState.payment_history` 保留相应记录，私有账户历史视图据此判断 incoming 和 outgoing。

## 4. 操作模型

`src/task.rs` 中的 `Operation` 定义八类操作。

| 操作 | 内容 | 必需条件与结果 |
| --- | --- | --- |
| `RegisterAccount` | account | 账户签名；注册新账户 |
| `RegisterPaymentAddress` | address, account | 账户签名；永久绑定新支付地址 |
| `Transfer` | source, destination, amount | 源账户签名、有效地址、可用流通 Currency；提交后更新 owner |
| `Issue` | account, count | Authorizer 授权、有效分配区间；创建对应数量的流通 Currency |
| `Destroy` | currencies | Authorizer 授权；移除列举的空 owner 流通 Currency |
| `LeakRepair` | leaked | Authorizer 与相关所有者签名；Reserve 替换、归属保留与 Reserve 补充 |
| `RetirePaymentAddress` | address | 账户签名；进入 Retiring |
| `FinalizePaymentAddressRetirement` | address | 账户签名；满足在途约束后进入 Retired |

转账金额和发行数量必须大于零。Destroy 和 LeakRepair 使用非空、唯一的 Currency 列表。Prepare 在同一任务中按照 operations 的原始顺序构建中间业务状态并重新验证每步条件。任务执行遵守整体业务原子性。

### 4.1 Transfer 的冻结选择

Transfer 首先通过 `source` 与 `destination` 支付地址确定实际账户，并建立具有 `OperationClaimId` 的执行前置事实。准备器核对源账户签名、支付地址状态和资金条件，从可用的流通 Currency 中选取确切的 `amount` 个对象，并将规范货币地址区间、源目标账户和执行数据写入 `PreparedTask`。

冻结阶段维护区间资源占用，业务最终提交时按区间更改 Currency.owner。支付历史同业务结果一起持久化。同一请求的多个合法冻结变体具有各自的规范计划摘要，证据处理必须保持原冻结选择和对应签票约束。

### 4.2 发行、销毁与修复

`Issue` 使用认证地址区间创建新对象，分配、冻结和提交均不展开单位地址缓冲。`Destroy` 只能删除 owner 为空的 Circulation 对象。`LeakRepair` 验证每个被修复对象的角色、owner 和授权，从空闲 Reserve 中取出相同数量的对象，将其调整为原账户拥有的 Circulation，同时销毁旧对象，并用新分配地址补充同数量 Reserve。

每项操作的 Currency 选择、需要的地址区间和资源冲突均纳入认证任务上下文。

## 5. 请求签名与授权

`LegalTaskPayload` 包含 TaskId、`protocol_version`、`expires_at` 和有序 `operations`。`LegalTask` 携带网络 ID、签发者公钥及签名、账户签名列表。请求使用规范化编码签名，操作顺序参与任务身份。当前协议版本为 `CURRENT_PROTOCOL_VERSION = 1`。

`LegalTask::verify` 校验协议版本、网络 ID、操作结构、Authorizer 签名以及账户签名。`SecondState::authorize_task` 依据当前状态检查每项操作所要求的账户签名，并在实际准备过程中复验。

当前授权规则：

- 注册账户、注册支付地址、转账、退役及最终退休要求对应账户签名。
- LeakRepair 要求相关 Currency 所有者的账户签名。
- Issue、Destroy 和 LeakRepair 还要求受网络 `AuthorizerSet` 信任的签发者签名。
- 普通账户操作允许使用零值 Authorizer 信息和有效账户签名。
- Authorizer 权限由当前网络配置提供。签名有效性与业务可执行性分别校验。

`expires_at` 控制首次准入时效；已经建立并持久化的相关协议事实以及认证恢复路径遵守各自状态约束。一个旧请求能否继续执行由 TaskId 绑定、分配证据、冻结计划和最终性共同决定。

## 6. TaskId、准备与任务终态

### 6.1 永久绑定与幂等

`ProtocolState.task_bindings` 将一个 TaskId 永久绑定到对应的精确 `request_digest`。相同 TaskId 的不同请求被拒绝。请求重试及交易状态查询均需要匹配原请求摘要。已成功任务再次提交时返回既有成功结果，不重复改变 Currency 归属。

每个任务可以包含多个有序操作；准备和提交必须保证完整任务的业务状态原子性。因业务失败而取消的执行仍保留安全必需的协议事实，例如已消费地址区间、TaskId 绑定和持久安全锁。

### 6.2 公开任务状态

`LegalTaskStatus` 提供以下状态：

| 状态 | 语义 |
| --- | --- |
| `Unknown` | 当前节点没有匹配 TaskId 和请求摘要的绑定 |
| `Bound` | 请求已持久绑定，相关业务尚未进入可报告的活动准备阶段 |
| `Prepared` | 已建立冻结业务计划 |
| `Voting` | 计划已进入共识签票阶段 |
| `Finalized` | Commit 最终性已持久保存，等待或正在执行最终提交 |
| `Succeeded` | 业务已成功提交 |
| `Cancelled` | 原任务得到合法 Abort 终态 |

任务提交回复中的 accepted、allocating、pending 等词表示受理和处理中状态。钱包及业务客户端根据 `Succeeded` 确认实际业务成功。

### 6.3 货币地址区间分配

Issue 与 LeakRepair 需要新的 CurrencyAddress。`CurrencyAllocation` 以当前委员会版本和地址前沿为范围，绑定请求摘要、起点和数量。认证结果可被精确恢复和重试。分配成功后，原始任务从认证地址区间继续 Prepare。地址身份在成功分配后保持消费状态。

`CurrencyAllocation` 与 ValidatorSetTransition 的成员切换请求通过当前 frontier 共识范围协调，防止同一地址前沿上出现互斥的认证推进结果。

### 6.4 PreparedTask

`PreparedTaskBook` 按操作顺序模拟业务变化，恢复或建立 `CurrencyClaimBook` 和生命周期资源占用，生成冻结的 `PreparedTask`，保存原签名请求、原委员会版本、业务操作及规范选币区间。`AddressRanges` 有序、不相交、不相邻；计划摘要及冻结见证编码使用起点与长度。Transfer 仍选择最低的可用 amount 个身份，按区间跳过其他任务占用；claim 引用计数、资源 fence 和私有持久化均按区间工作。Destroy/LeakRepair 的原签名地址列表及 LeakRepair 对应关系不变。

`PreparedTaskPhase` 为 Prepared、Voting、Finalized。持久化共识证据和签票权限决定可执行阶段转换。最终证书持久化后再执行资源依赖闭包的业务提交。`PreparedTaskBook::commit_certified_component` 基于已验证最终性和资源阻塞形成完整认证组件，复验计划，在同一 StateStore 业务写入中完成状态变化、移除准备项和触发等待者恢复。

执行暂时失败时，已经认证的终态和必要上下文继续留存。节点重启后通过持久状态恢复提交。

### 6.5 Commit / Abort

同一 `PreparedTask(TaskId)` 共识范围能够产生具体计划的 Commit 或任务取消的 Abort。证书验证必须使用原任务委员会、准确请求摘要及对应终态声明。Abort 解除受保护资源的依据是有效的原委员会终态证书。已有不可逆投票、QC 与 FinalityCertificate 决定终态冲突的安全约束。

## 7. 并发与冲突仲裁

准备期间的 Currency、支付执行、账户/支付地址生命周期资源由相应 claim、lock 和 fence 保护。

并发规则：

1. 同一个 Currency 的竞争使用精确地址与所有权验证。
2. 同一个任务的合法冻结变体按 plan digest 区分。
3. 本地收到可验证的外部任务见证时，见证可参与冲突恢复；提交权和签票权仍由资源拥有情况及原委员会认证证据约束。
4. Commit QC、最终性票及认证最终证书保护已经形成的决定，且不会被后来到达的任务优先级覆盖。
5. 竞争任务通过原 PreparedTask 的 Commit/Abort 共识确定各自终态。
6. 认证资源释放后，持久化待办原请求及其已验证来源可以重试。
7. 提交路径计算依赖闭包，整个受影响组件满足终态条件后一次性写入业务。

`src/prepared/conflict.rs`、`src/prepared/variants.rs`、`src/prepared/fences.rs` 与 `src/persistence/contention.rs` 承担冲突选择、冻结候选和已认证资源阻塞逻辑。

## 8. BFT 共识

### 8.1 委员会与 quorum

`ValidatorSet` 维护给定版本的验证者及其三类公钥。每成员一票，采用：

~~~text
quorum_threshold(n) = n - floor((n - 1) / 3)
~~~

例如四验证者集合要求三份合法成员签名。BFT 对象的验证绑定精确 ValidatorSet、版本、范围、摘要、轮次与签名。`FinalityCertificate` 验证原集合内去重后的有效法定票数。

### 8.2 共识范围

源码 `ConsensusScope` 包含四类：

| 范围 | 身份键 | 目的 |
| --- | --- | --- |
| `CurrencyAllocation` | validator_set_version + start | Currency 地址分配及同前沿成员切换 |
| `PreparedTask` | TaskId | 已冻结业务的 Commit / Abort |
| `PublicCheckpoint` | validator_set_version + epoch | 公开状态检查点 |
| `StateRecoveryCheckpoint` | validator_set_version + serial | 共享恢复检查点 |

任务范围保留任务实际绑定的原委员会。其他带显式版本的范围由其目标版本与当前签名权限校验。

### 8.3 轮次、QC 和最终性

`BftDriver` 的阶段为 Proposal、Prevote、Precommit。轮次提议者由 ValidatorId 的确定顺序和 round 选择。节点必须先从可信本地业务对象构造和验证 `BftProposalSubject`，才能为相应摘要签票。

`BftValue` 可以是 Digest 或 Nil。超时和合法的 Nil 证明推动轮次；完整验证的更高轮次 QC 支持本地追赶。有效 Digest Precommit QC 是进入 `FinalityReady` 的证据之一。业务最终性使用单独的 FinalityStatement、ValidatorVote 和 FinalityCertificate。

`ValidatorSigner` 和 StateStore 在签名前持久化必要的锁、阶段、轮次、签票历史与安全下界。冷重启恢复原签票条件。历史 QC 的接纳、重放与完整来源校验继续绑定原 scope 和委员会。

## 9. 验证者成员生命周期

`ValidatorRegistry` 保存当前版本、成员状态、凭证与已用密钥历史。成员可处于 Active 或 Retired。新成员申请、共识密钥轮换及成员集合变更由经过认证的管理请求和 `ValidatorSetTransition` 完成。下一集合版本逐次递增。

成员切换关联当前 Currency 地址前沿；只有原委员会合法最终性和有效交接正文满足条件，才能安装下一集合。

### 9.1 TaskHandoff

`TaskHandoff` 保存认证业务基线、仍有效的冻结计划、同任务的合法变体及未完成的原签名请求；每项继承义务保留原 TaskId、原请求摘要和原委员会上下文。

交接根绑定业务基线和规范资源义务。交接内容的 `PreparedTask` 表示需要继续保护的候选资源；各节点的本地投票阶段与签名锁另由各自 StateStore 保存。

新成员能够在原委员会有效证书到达后应用对应历史终态，同时遵守旧任务签名资格限制。过期委员会的必要凭证和证明由未完成任务及其引用确定保留范围。

### 9.2 签名安全恢复

空的恢复目标可安装带认证恢复检查点的 `StateRecoveryPayload`。共享载荷包含业务及必要协议事实、验证者集合/注册表、历史委员会与相关证明。安装后的本地 `validator_safety_ready` 为 false，本地 PreparedTask、vote lock 与 BFT 票据历史由新节点独立维护。

恢复到可签状态还需取得符合本身份和当前委员会的独立共识密钥轮换、恢复证明及 signing fence。`minimum_signing_validator_set_version` 为签名下界。普通正常重启复用本地持续保存的签票安全历史。

## 10. 持久化设计

### 10.1 完整状态

`StateStore` 保存业务、任务、冻结资源、注册表、委员会、签名锁、BFT 状态、公开证据、恢复证明和管理待办。提交在进程内与跨进程存储锁下完成，使用已有 generation 和语义 CAS 防止陈旧状态覆盖当前提交。

持久化顺序：

1. 校验待保存状态和快照对象结构。
2. 将整个权威快照编码为有边界、带校验和的字节序列。
3. 写入主 slot 并执行磁盘同步。
4. 发布 `generation + checksum` 的 commit reference 并执行磁盘同步。
5. 尽力写入镜像 slot。
6. 更新本地共享读取缓存。

读取时只接纳与 commit reference 精确匹配的有效 slot。无法证明历史签票连续性时拒绝签名。快照载荷最大值为 512 MiB。

`load_shared` 利用经验证的不可变共享快照和 token 缓存减少重复完整解析。当前权威持久化写入仍采用整份快照编码，状态总量会影响写入成本。

### 10.2 崩溃、共享与公开恢复

冷加载检查快照校验和、业务状态、TaskId 绑定、准备计划、委员会历史、签票锁、QC 和交接关联。已经认证为 Finalized 的任务在恢复后仍可继续执行业务提交。

`StateRecoveryPayload` 提供认证共享恢复内容。`PublicStateStore` 独立保存公开货币状态、检查点及其同步材料。私有完整状态、公开状态和签名安全恢复有各自的存储与权限边界。

## 11. QUIC 网络与运行时

网络使用 Quinn、rustls 和持久化传输身份实现加密 QUIC 连接。NodeId 与传输证书经认证绑定；连接目标使用固定服务器证书。`PeerRecord` 保存节点 ID、监听地址与证书；`PeerManager` 以确定性规则处理双向重复连接；`PeerStore` 缓存已认证可达节点信息。

`NodeRuntime` 负责监听、连接、公开查询、BFT 会话、任务提交、同步、治理和恢复。`Validator` 是具备完整状态及相应密钥与准入权限的节点能力。使用 `PublicStateStore` 的节点提供公开状态服务。完整状态节点可以在缺少 Validator 签票能力时提供其被授权的非签票服务。

BFT 会话在传输身份之外验证 ValidatorId、委员会及会话角色。消息发送与接收按具体版本、scope 和签名分别校验。网络具有连接、帧、分块、并发和候选预算；共识及最终性消息使用各自的排队容量。共识循环、入站业务/治理/账户查询、BFT 权限刷新和成员追赶的阻塞式存储工作经线程池执行。公开同步及若干 CLI 启动/运维入口仍直接调用同步快照读写，会阻塞其所在 executor；完整快照写入的成本与当前状态大小有关。

节点启动期间对 snapshot base 持有排他运行锁。同一 Validator 的私钥和身份须保持唯一活动实例。

本地分配/重启与 BFT 检查点集成测试按可观察进度判断停滞，避免把整个多阶段流程的 CPU 排队、存储同步及连接重试压进同一个 5 秒期限。第一次观察即开始空闲计时；分配测试观察持久 generation，连续 5 秒不变即失败（包括从未有进展）；连接测试观察各验证者的实际连接集合，连续 15 秒不变即失败（涵盖 5 秒传输、5 秒认证及 2 秒重试间隔）。每次等待另有 60 秒整体硬期限且不会续期，持续换轮或连接抖动不能无限等待。测试中的快照轮询在阻塞池执行，避免阻塞当前线程的网络 executor；这些预算只用于测试，不改变协议运行时期限。其他快照轮询同样将读取放到阻塞池；多节点收敛、故障恢复和容量场景保留原有端到端总预算，不能用其他任务的 generation 变化无限延长目标场景。

## 12. 公开 Currency 状态

`PublicCurrencyState` 仅保存存活区间 `{start, len, occupied}`。occupied 为 `owner.is_some() || role == Reserve`；Reserve 与有主的 Circulation 在公开投影中相同。不保存 owner 或 role，且相邻同 occupied 的区间必须合并，不泄露内部边界。低于地址前沿但不存活的地址视为已废弃，无需另存废弃区间；身份不复用。Destroy 与 LeakRepair 的旧地址在公开投影中都只表现为存活转废弃。

`PublicCurrencySummary` 的 current_supply、occupied_count 是单位数；state_digest 对地址前沿、可重算计数和规范公开存活区间求哈希，不包含无法公开重算的 reserve_count。reserve_count 由委员会认证检查点对完整摘要的签名背书；`PublicCurrencyView::new` 只验证 `reserve_count <= occupied_count <= current_supply` 等可检查关系，不能独立证明其精确值。未认证的 summary 不具有此证书保证。

公开分页、同步内存预算和 `PublicCurrencyDelta` 的数量上限按区间计。完整节点保存上次认证检查点的规范公开区间作为历史基线（不是当前资产的第二份真值），直接比较基线与新投影生成 `Upsert(存活区间)` / `Retire(地址范围)`。周期内被触碰但公开状态没变的地址不出现在增量中；应用增量后重新归并为规范视图并核验 digest。超出增量区间或字节预算时走已有全量公开同步。

正常 Issue、Transfer、LeakRepair 均不会产生空 owner 的 Circulation。内部状态格式仍允许这种状态并由 Destroy 处理，公开 occupied 因此继续保留；当前任务没有缩减此内部语义。

Destroy 与 LeakRepair 使用相同的 Retire 编码，不携带业务类型；完整公开历史仍可区分它们：Destroy 仅删除此前 occupied=false 的空 owner Circulation，而 LeakRepair 仅替换此前 occupied=true 的有主 Circulation。保留 occupied 和内部操作语义时，这条残余信息无法消除，不能把相同的废弃表示解释为完整历史不可区分。

完整状态节点仍持有 owner 和 role；公开观察者与完整私有节点的可见范围不同。

## 13. 私有账户查询

`src/network/account_query.rs` 提供以账户密钥签名的私有查询。签名消息绑定 QUIC channel binding、账户地址、查询类型、nonce、cursor 和 generation。应答者先完成认证，再加载其本地私有快照。

| kind | 数据 |
| --- | --- |
| 1 | 账户存在性与余额 |
| 2 | 账户支付地址和生命周期状态 |
| 3 | 账户收付款历史 |
| 4 | 该账户拥有的规范地址区间 `{start, len}` |

服务端从单次 `StateStore::load_shared` 的已验证快照派生短期投影。同一分页查询的余额、行、委员会版本与 generation 来自该快照。投影扫描 CurrencyLedger 区间，balance 按 owner 区间长度求和，kind 4 按区间保存与分页；不展开单位地址。后续分页只切片读取。

当前限制：每页至多 128 行，每次列举至多 100,000 行，kind 4 的行数、total 和 cursor 均按区间计，不限制区间内单位数量。投影和并发查询有内存预算，完整查询连接最长 30 秒。超出预算时报告错误。客户端逐页验证来源 generation、游标、总数、区间非空与不溢出、不重叠不相邻（包括跨页边界），并在完整列举后验证区间长度总和等于 balance。

### 13.1 账户余额的可信范围

`AccountView.balance` 表示回答节点在某个已提交本地 generation 所保存的 Currency 归属计数。账户签名保护读取权限，分页规则保证该本地快照响应内部一致。

当前接口输出明确标注：

~~~text
authenticated_node_snapshot=true
independent_account_finality_proof=false
network_latest_guaranteed=false
linearizable_read=false
~~~

`generation` 为本地快照代次，`validator_set_version` 是应答快照所使用的委员会版本。当前客户端没有独立的账户资产完整性最终性证明，也无法凭该接口保证回答节点已追上全网全部已确认的相关业务。

## 14. 钱包实现

钱包作为 `second wallet` 命令集嵌入主二进制。钱包拥有自己的加密资料库，保存账户密钥、活动账户、支付地址、联系人、固定网络端点、已签名请求和重试材料。钱包启动一个普通的公开 `NodeRuntime`，通过带固定证书的远端 QUIC 连接执行账户查询及业务提交。

主要命令：

~~~text
second wallet init <dir> <trust-snapshot> <network-json>
second wallet open <dir>
second wallet register <dir>
second wallet address <dir>
second wallet balance <dir>
second wallet assets <dir>
second wallet send <dir> <pay_...> <amount>
second wallet status <dir> <task-id>
second wallet retry <dir> <task-id>
second wallet history <dir>
second wallet backup <dir> <backup-file>
second wallet restore <backup-file> <dir>
~~~

还包括多账户创建、导入、导出、切换，支付地址注册、退役及最终退休，联系人，钱包请求导出与 Authorizer 授权、密码修改和公开同步。

### 14.1 钱包签名、发送与恢复

发送普通转账时，钱包查询远端账户信息，选择活动源地址，展示收款地址与金额，取得用户确认，生成独立 TaskId 与账户签名。`StoredRequest` 在发往网络前持久保存原请求与签名。提交后钱包通过任务状态接口确认 `Succeeded` 或 `Cancelled`。断连、超时及钱包重启后的重试保持原 TaskId 和原签名请求。

钱包文件由 `DurableBlobStore` 管理；`VaultCipher` 使用 PBKDF2-HMAC-SHA256 和 ChaCha20-Poly1305 加密并验证数据。备份保存钱包资料及必要的未决请求，不承担 Validator 签票历史的恢复职责。

### 14.2 钱包余额与同步

`balance`、`assets`、`address list` 和 `history` 当前为按需签名查询。`assets` 输出 `{start, len}` 资产区间，balance 为长度之和，不枚举单位身份。`sync` 通过钱包普通节点同步认证的公开 Currency 视图。个人余额尚未实现持续推送更新、跨离线期可认证的账户变更追赶、抗恶意节点遗漏与全网最新性判定。

交互式 `open` 提供会话内命令操作。当前用户界面为 CLI，个人资产信息的可信范围以 `AccountQuery` 实际返回的来源标记为准。

## 15. 网络初始化与部署

`init-network` 从 Genesis 配置生成初始 SecondState 和 ValidatorSet、各验证者数据目录、签名密钥、传输身份、bootstrap 记录与 `wallet-network.json`。

常用主程序入口：

~~~text
second init-network <config-json> <deployment-directory>
second node <listen-address> <snapshot-base>
second node-check <listen-address> <snapshot-base>
second snapshot-status <snapshot-base>
second ping <address> <nonce> <server-cert-base64>
second submit <address> <request-file> <authorizer-public-key> <server-cert>
second task-status <address> <request-file> <authorizer-public-key> <server-cert>
~~~

密钥生成、签发者工具、成员准入与轮换、委员会切换、恢复检查点、恢复安装及公开状态同步由主 CLI 对应子命令提供。

部署工具分别支持 Windows SCM 和 Linux systemd 的服务生命周期。Genesis 初始化工具按当前连接预算支持最多 56 个活动 Validator；实际拓扑还受保留历史集合、公开连接和服务容量共同限制。多主机网络需要可达的 UDP 端口、匹配的固定证书与受保护的节点目录。

## 16. 核心协议不变量

1. **资产唯一真值**：每个当前存在的 Currency 最多对应一个 owner；账户余额派生自 owner。
2. **对象身份永久性**：货币地址分配后永久消费，账户及支付地址遵守唯一注册和永久绑定规则。
3. **请求绑定**：TaskId 唯一绑定规范请求摘要；成功任务精确重试具有幂等性。
4. **授权完整性**：账户操作必须具有对应账户签名；特殊操作必须满足 Authorizer 权限。
5. **任务原子性**：一个 LegalTask 的 operations 依次准备，完整认证业务结果一次提交。
6. **资源占用安全**：冻结货币、支付执行、生命周期变更及交接 fence 一起限制冲突操作。
7. **BFT 签票安全**：轮次、QC、最终性、原委员会及签票锁决定可签名的内容和不可逆终态。
8. **提交前认证**：资产归属变化须由准确的 Commit 最终性及满足依赖闭包的持久提交驱动。
9. **崩溃恢复安全**：快照 commit reference、持久票锁和 signing fence 共同禁止旧状态引发不一致签票。
10. **验证者签名隔离**：共享状态恢复和新成员安装后需独立取得合法的当前签名资格。
11. **公开隐私边界**：公开记录只携带地址区间、occupied 与认证摘要，不携带 owner 与 role。
12. **资源有界**：网络、快照、账户查询、分块传输和候选集合使用明确的容量限制。

## 17. 当前架构边界

| 子系统 | 源码实现的范围 |
| --- | --- |
| 业务状态 | 独立 Currency 身份，私有规范区间保存，完整私有状态在相应节点维护 |
| 委员会 | 有准入的 ValidatorSet 和认证成员切换 |
| 共识 | 四类 ConsensusScope 与固定委员会法定票数 |
| 提交 | 冻结计划、认证终态、依赖闭包与单次业务原子写入 |
| 磁盘 | 完整快照编码与双槽提交，载荷上限 512 MiB |
| 公开同步 | 规范公开存活区间、基线差异增量及委员会认证检查点 |
| 残余公开泄露 | 周期内废弃数量与前沿新增数量相等可提示修复，单凭数量也可能是等量销毁与发行；保存此前 occupied 可进一步识别有主对象的修复。规范增量不列出 Reserve 接替者；Reserve 选取规则本身的可关联性未处理 |
| 钱包账户查询 | 对单个完整状态节点本地已提交快照的签名读取；资产区间分页、余额按长度求和，成本取决于区间数 |
| 账户数据完整性 | 目前缺少跨节点独立验证的全部资产覆盖证据 |
| 余额新鲜度 | 当前接口没有全网最新性与线性化读取保证 |
| 自动更新 | 钱包当前采用按需查询，缺少持续资产订阅与完整离线追赶 |
| 网络规模 | 初始化连接预算限定当前活动验证者上限；运行成本随全量状态和活动任务增长 |
| 部署验证 | 支持 Windows 和 Linux 节点/服务操作；跨独立物理主机与长期规模性能需实际验收 |

这些边界与协议安全规则共同定义现阶段产品能够提供的行为。规模与能耗评估以业务吞吐、节点资源、状态规模和故障恢复成本的实测结果为准。

## 18. 源码职责

| 模块 | 职责 |
| --- | --- |
| `src/ids.rs`、`src/account.rs`、`src/currency.rs`、`src/currency_ledger.rs`、`src/payment.rs`、`src/state.rs` | 身份、账户、支付、资产与唯一业务状态 |
| `src/address_ranges.rs`、`src/range_map.rs` | 规范地址集合，以及账本、claim 和公开增量共用的区间拆分与合并 |
| `src/task.rs`、`src/authorization.rs`、`src/transaction.rs` | LegalTask、交易输入、签名与授权 |
| `src/currency_allocation.rs`、`src/claims.rs`、`src/prepared.rs`、`src/prepared_plan.rs`、`src/prepared/` | 地址分配、冻结准备、冲突与业务提交 |
| `src/bft.rs`、`src/bft_driver.rs`、`src/finality.rs`、`src/validator_signer.rs` | BFT、投票安全与最终性 |
| `src/validator.rs`、`src/validator_registry.rs`、`src/validator_transition.rs`、`src/validator_admission.rs` | 验证者身份、准入和委员会演进 |
| `src/persistence/` | 快照、签名锁、任务、交接与安全恢复 |
| `src/network/`、`src/runtime.rs`、`src/runtime_bft.rs`、`src/runtime_bft_consensus.rs` | QUIC、连接管理、在线共识与恢复 |
| `src/public_state.rs`、`src/public_checkpoint.rs`、`src/public_sync.rs` | 公开 Currency 状态及认证同步 |
| `src/network/account_query.rs`、`src/network/account_query/` | 私有账户认证、分页与数据源 |
| `src/wallet_cli/` | 钱包密钥存储、支付、备份、查询与 CLI |
| `src/network_init.rs`、`src/cli_node.rs`、`src/main.rs`、`deploy/` | Genesis、节点管理、入口和服务部署 |

本文件随协议当前行为变化更新。历史变更、测试日志、回归记录及开发过程由版本控制与对应工程记录承担。
