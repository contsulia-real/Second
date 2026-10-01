# Second 项目设计

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

一旦 identity range 正式分配，它永久消耗，即使同一 LegalTask 后续业务失败也不能复用。allocator 在推进 durable frontier 之前必须先成功物化本次 frozen address plan；本地 `Vec` 无法表示或预留所需容量时返回 execution error，不能因资源分配失败烧掉 identity。这个保护不定义 `Issue` 的协议级数量上限。

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
prepare
  ↓
per-scope BFT rounds (prevote / precommit)
  ↓ valid precommit QC for exact subject digest
irreversible Validator finality vote
  ↓
FinalityCertificate
  ↓
commit
~~~

禁止存在可由正式节点调用的 direct executor 旁路。

`prepare` 可以建立和持久化协议必须保留的事实（例如 TaskId binding、Transfer establishment、Currency identity reservation 和 claims），但不能直接提交最终业务资产变化。只有验证通过的 FinalityCertificate 才能进入 `PreparedTaskBook::commit()` 并提交 BusinessState。证书验证通过后，如果本地 plan apply 因状态不一致等原因失败，该错误不等价于 cancellation：不得隐式删除 prepared plan 或释放其 claims，必须保留可恢复的 finalized commit 上下文。

Genesis 本身的初始账户、地址起点和 Reserve 初始化不属于 LegalTask 执行，不要求经过 Finality。

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

只有 `Prepared` phase 允许 cancel。第一次为 PreparedTask 本地签 BFT prevote/precommit，或本地接受该 scope 的有效 precommit QC 时，必须在同一 durable snapshot 中把 `Prepared → Voting` 打开；该转换不可逆，因此真正的 BFT 投票一开始就不能再 cancel，而不是等到最后不可逆 `FinalityVote` 才关闭取消窗口。`sign_prepared_vote()` 面对已经处于 `Voting` 的 task 只继续既有 finality path。`commit()` 验证到有效 FinalityCertificate 后，必须先把 phase 持久化为 `Finalized`，再尝试 apply，因此即使本地 apply 暂时失败，任务也不能再 cancel。`Voting` / `Finalized` 都必须跨重启恢复。

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

PreparedTask 使用其 plan digest 作为 subject digest。

### 12.4 Per-scope BFT coordination

不可逆 `ValidatorVote` 之前增加独立的 per-scope Byzantine agreement 层。Second 不建立 block、高度链或全局 transaction total order；`ConsensusScope` 直接复用最终 vote-lock 的权威 scope：PreparedTask、PublicCheckpoint、ValidatorSetTransition、StateRecoveryCheckpoint 各自是独立 consensus instance。

BFT vote 使用独立 `SECOND_BFT_V1` signing domain，绑定 `protocol_version + validator_set_version + ConsensusScope + round + phase + value`。phase 当前固定为 `Prevote` / `Precommit`，value 为具体 subject digest 或 `Nil`。BFT vote/QC 与最终 `FinalityStatement` / `ValidatorVote` 是不同签名语义，不能互换。

每个 Validator、每个 scope 的 `BftLocalState` 持久保存当前 round、本 round 已投 prevote/precommit、locked round/digest，以及已经观察到的 precommit QC 对应 finality-ready digest。该状态属于本地 signing safety metadata，不进入 shared recovery digest；shared-state recovery 不恢复它，并继续受 `validator_safety_ready` / signing fence 约束。PreparedTask scope 的 BFT state 必须绑定该 task 的 exact active/retained ValidatorSet；PublicCheckpoint、ValidatorSetTransition、StateRecoveryCheckpoint 的 BFT state 只允许绑定当前 active ValidatorSet。membership 激活后旧的非-Prepared BFT state 会被丢弃，PreparedTask 完成/取消后其 BFT state 也会被清理。

锁规则当前冻结为：对 digest 的 precommit 必须附带同 scope、同 round、同 digest 的有效 prevote QC，并在本地形成 durable lock；已锁 A 后，后续 round 不能直接 prevote B，只有附带一个 `locked_round < proof_round < current_round`、且对 B 达到 quorum 的 prevote QC 才允许迁移。`Nil` 不建立 value lock。每个 round 的 prevote/precommit 各只能签一次，同 phase 同 round 冲突值永久拒绝；round 只能严格 `+1` 前进。

只有节点已经验证并持久接受 exact scope/digest 的有效 **precommit QC**，现有永久 `FinalityVote` 入口才被打开；成功写入不可逆 vote-lock 后，对应 transient BFT local state 被移除。已经存在的同 digest finality vote-lock 仍允许确定性 replay，不要求重新跑 BFT。

这一层现在直接在既有 safety core 上补齐 proposer、proposal validation、Validator-only BFT transport 与 timeout/view-change driver，而没有引入第二套 consensus。proposer 对 exact `ValidatorSet` 按 `ValidatorId` 升序排列，并以 `round % N` 确定；`BftProposal` 使用独立 `SECOND_BFT_PROPOSAL_V1` signing domain，绑定 `protocol_version + validator_set_version + ConsensusScope + round + proposer_id + subject_digest`。网络收到的 digest 不能直接进入 signer：节点必须先从本地真实对象和状态构造 `BftProposalSubject`，PreparedTask 校验 durable plan 与其 exact active/retained ValidatorSet，PublicCheckpoint 校验本地 public summary/floor，ValidatorSetTransition 校验当前 registry transition，StateRecoveryCheckpoint 继续复用既有 recovery serial/floor 与 shared-state 匹配规则。`BftDriver` 只在本轮已经本地验证过 exact subject 后，才允许 digest prevote QC 触发 precommit 签名或 digest precommit QC 进入 finality-ready；仅凭网络 QC 中的 raw digest 不会打开签名入口。重启后 transient subject-validation context 不冒充 durable safety fact，需要重新从权威业务对象验证 proposal。

`BftDriver` 的 timeout 仍只是本地 liveness 触发器：proposal timeout 产生本轮 `Nil` prevote，prevote timeout 产生本轮 `Nil` precommit，precommit timeout 或有效同 round `Nil` precommit QC 只将 durable round 严格推进 `+1` 并轮换 proposer；即使本地完全错过该 round、尚无 BFT state，有效 Nil precommit QC 也可原子创建该 scope 的本地 state 后进入下一 round，但不能靠无 QC 的网络消息跳轮。协议不签名 timeout 时间戳、不创建 timeout certificate，也不把本地时钟变成共享事实。Validator-only BFT transport 复用现有 QUIC/TLS transport，但在 dedicated BFT connection 上额外用 active ValidatorCredential 的 identity key 对 `CURRENT_NETWORK_PROTOCOL_VERSION + QUIC TLS exporter channel binding + ValidatorId` 做一次会话认证；认证后的连接承载 bounded BFT Proposal/Vote/QC、不可逆 `FinalityVote` 与 `FinalityCertificate` envelope。Proposal 与 BFT Vote 必须由该连接认证出的原始 proposer/voter 发送；QC 与 FinalityCertificate 依靠自身 quorum signature 独立验证，FinalityVote 允许由其他已认证 Validator relay，但始终验证原始 `vote.validator_id` 的 consensus signature。认证后每个 BFT envelope 使用独立 QUIC 单向流，runtime 对发送与接收都做有界并发，并为 finality 消息保留优先容量；这些只是本地 liveness/resource 策略，不进入签名或共享协议状态。BFT transport 只承载共识 envelope，不替代业务对象传播；接收方必须已经能从本地可信对象/状态独立得到相同 `BftProposalSubject`。这些能力没有引入全局区块、全局序号、stake 权重或新 authority。

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

checkpoint epoch 是公开同步证明的单调 freshness 序号，不是全局 transaction/block height。节点持久化 `checkpoint_floor_epoch` 作为防回退下界；即使业务状态前进导致旧 checkpoint proof 不再匹配当前 state，floor 仍保留并跨重启恢复。当前机制只认证公开 Currency state，不加入 owner commitment，也不声称证明私有 ownership。

---

## 16. 网络设计

当前网络层以 QUIC 作为节点传输层，不依赖 JSON 作为节点间 framing。QUIC 连接完成 TLS 1.3 握手后，应用层先使用一个可靠双向 stream 完成 NodeId Hello；后续每个请求/响应使用独立的可靠双向 stream。当前 public session 按 connection 串行处理 request，因此 transport 也只允许每个 connection 同时存在 1 个双向 stream；完成一个请求后仍可继续打开下一条独立 stream。单向 stream 被禁用。当前协议不使用 QUIC DATAGRAM，也不使用 0-RTT。

QUIC 的 transport identity 与 Validator identity / consensus / recovery key 是不同职责，四者不得复用。transport identity 使用独立 Ed25519 key；`NodeId` 直接等于该 transport public key 的 32-byte 编码，因此 NodeId 不再是调用方可以任意声明的数字或标签。

NodeId 的认证权威是 Hello peer-auth Ed25519 key：Hello signature 的签名输入绑定当前 network protocol version、client/server role 与 Quinn TLS exporter 派生的 per-connection channel binding，因此该 NodeId 的 key ownership 被绑定到当前 TLS 会话；旧连接上的 Hello 不能在另一条 QUIC connection 上重放，client proof 也不能直接反射成 server proof。只有签名验证成功后，Hello 中的 NodeId 才能成为 `QuicPeer::remote_node_id()`。

长期 server 当前用同一 transport Ed25519 key 生成 self-signed TLS certificate 和 Hello proof，使 certificate pin 与 NodeId 都能随同一持久 identity 稳定重建；但协议不把这描述成 mTLS。`QuicClient` 不向 server 提交 TLS client certificate，server 对 client NodeId 的认证来自 channel-bound Hello proof。对 server，显式 certificate pin 认证 TLS endpoint，Hello proof 再认证该会话上的 NodeId；当前接收端不额外解析 certificate SPKI 去重复证明“certificate key == Hello key”。

当前长期节点入口为 `second node <listen-address> <snapshot-base>`。长期节点的 transport private key 保存在独立的 `<snapshot-base>.transport` 本地 sidecar 中，不写入 Second state snapshot：首次不存在时创建，之后重启必须复用；已有 identity 文件损坏或无法解析时启动失败，不静默生成新身份。首次创建使用邻接 lock 文件串行化，并先把完整 identity 写入并 `sync_all` 到 `.transport.new`，再原子 rename 发布为 `.transport`；因此崩溃留下的 `.new` 不是 identity authority，下次启动可安全覆盖，只有最终 `.transport` 才是已发布身份。该文件包含私钥，Unix 创建权限为 `0600`。由同一 key 重建的 certificate 和 NodeId 在重启后保持稳定。显式地址的一次性 CLI（如 `ping` / `sync-public-certified`）仍由调用方 pin server certificate 并使用临时 transport identity；`observe-public-network` 则复用该 snapshot-base 对应的持久 transport identity、PeerStore 和静态 bootstrap records。

transport authentication 只证明“当前 QUIC peer 持有这个 NodeId 对应的 transport private key”，不自动授予 Validator 权限、网络信任或 admission。Validator 权限仍只来自有效 ValidatorCredential；trust/discovery/admission 仍需要独立规则。

当前 runtime 已有进程内 peer manager，并同时管理 inbound 与主动 outbound connection。peer 只有在 authenticated Hello 完成后才进入 registry；registry 以 NodeId 为唯一键，同一 NodeId 在同一节点上同时只保留一条 active connection，自连接（remote NodeId 等于 local NodeId）直接拒绝。没有重复连接时，任意方向的 authenticated connection 都可正常注册，因此 public read admission 仍保持开放。

当两个长期节点同时互拨形成两条连接时，peer manager 使用 NodeId 定义确定性仲裁：较小 NodeId 的节点保留 outbound，较大 NodeId 的节点保留对应的 inbound；反向连接被关闭。该规则只在同一 NodeId 已出现重复 active connection 时参与选择，不会因为连接方向“非首选”而拒绝一条原本唯一的连接。被替换连接的旧 lease 即使稍后释放，也不能删除新连接的 registry entry。

`PeerRecord` 是当前唯一的 outbound reachability 记录，内容只有 `NodeId + SocketAddr + self-signed server certificate pin`；它只回答“如何尝试连接这个 transport identity”，不携带 Validator role、ValidatorSet membership、共识权重或其他授权语义。`NodeRuntime::dial(&PeerRecord)` 复用节点自身持久 transport identity 发起 QUIC + authenticated Hello，要求 TLS endpoint 的 certificate pin 与 Hello 的 expected NodeId 都匹配。节点如果绑定到可直接表示的非 unspecified `SocketAddr`，runtime 会从自己的实际 listen address、NodeId 和当前 certificate 构造唯一 local PeerRecord；绑定 `0.0.0.0` / `::` 时则不对外宣称不可拨的 unspecified 地址。当前不会猜测 wildcard bind 对应的公网 IP，也没有 NAT/public-endpoint 探测；这类部署若需要被主动发现，仍应使用可拨的明确 listen address 或静态 bootstrap，未来只有出现真实部署需求时才增加显式 advertise endpoint 配置。outbound connection 注册后与 inbound 一样运行 public network session，因此保留下来的单条 QUIC connection 可由双方各自发起 public request。

节点维护独立的本地 `PeerStore` sidecar `<snapshot-base>.peers`，当前最多保存 32 个最近经过直接 transport authentication 的 `PeerRecord`。记录可以来自成功的 authenticated outbound dial，也可以来自该 NodeId 本人在现有 authenticated connection 上返回的自身 reachability 声明；后一种声明证明“这是 NodeId holder 自己发布的当前 endpoint/certificate”，但不把 endpoint 可达性升级成协议 authority。第三方 gossip 的其他 record 在真正与目标建立 authenticated connection 前仍只是未验证 candidate，不会因为中继 peer 宣称就直接进入本地 PeerStore。PeerStore 是可丢弃的本地连接缓存，不属于 Second state snapshot、ValidatorRegistry、finality 或任何共识 authority。

`GetPeers { limit }` 当前上限为 32。若本节点存在可宣称的 local PeerRecord，响应第一项优先返回自己的当前 record，剩余名额再从 PeerStore 中按最近认证成功顺序返回记录，并排除当前请求方；这样不需要增加第二套 advertisement 消息或签名格式，远端可利用当前 authenticated QUIC connection 把“response 中 NodeId 等于 remote NodeId 的 record”识别为 NodeId holder 自己的 reachability 声明。`NodeRuntime::bootstrap` 先尝试本地持久 PeerStore，再把调用方提供的 bootstrap records 作为 fallback；连接任一 peer 成功后可继续请求更多 candidate，只有 responder 自己的 record 可以直接按 owner-authenticated reachability 更新 PeerStore，其他第三方 record 仍必须逐个通过真实 QUIC/TLS + Hello authentication 后才持久化。不同 endpoint/certificate 的同一 NodeId candidate 可以分别尝试，避免一个过期或恶意记录阻断后续正确 endpoint。

长期 `second node` 的初始 bootstrap 来源固定为可选 sidecar `<snapshot-base>.bootstrap.json`。文件不存在表示没有静态 bootstrap，不阻止仅依靠已有 PeerStore 或 inbound peer 启动；文件一旦存在则必须是严格 JSON 数组，每项只含 `node_id`（64 位小写 hex）、`address`（SocketAddr）和 `certificate_base64`，未知字段、非法 NodeId/address/certificate、超过 32 条记录都会在创建 transport identity 之前使节点启动失败。该 sidecar 是本地 deployment hint，不属于 snapshot、protocol state 或 authority。

runtime 当前以 8 个已验证且活跃的已知 peer 作为本地连接维护目标；这是实现策略，不是协议常量。节点每 2 秒运行维护 tick，连接不足时从 PeerStore + 静态 bootstrap + 已连接 peer 返回的候选继续扩展；没有取得新连接时，dial retry 从 1 秒指数退避到最多 60 秒，取得进展后重置。PeerStore 的选择策略保持有界且简单：直接认证成功或 owner-authenticated reachability 刷新会把记录提升到 MRU 端；对已持久记录的拨号失败会把该精确 record 降到最旧端，后续优先尝试近期成功/刷新的 peer，而不是永久反复先撞同一个坏 endpoint。QUIC client 与 server transport 都对已建立的长期 session 使用 2 秒 keepalive，并保留 5 秒 idle timeout；连接 admission、128 connection 容量与 peer/session 生命周期仍负责资源边界，keepalive 不授予任何额外 authority，也不会绕过 admission。当前仍没有 DHT、DNS seed 或 NAT traversal。

节点启动时恢复 snapshot，持续接受 QUIC connection；每个通过 peer manager 注册的连接独立运行 public network session，因此单个 peer 的断开、错误请求或握手失败不会结束 listener。当前 runtime 最多同时保留 128 个进入握手/已建立的 inbound + outbound connection；超过该本地容量的新 inbound `Incoming` 在握手前直接拒绝，主动 dial 则直接返回本地 capacity error。这个 128 是节点实现的 DoS / 资源保护默认值，不是协议、共识或 Validator 数量规则，不进入任何签名、frame 或 snapshot 版本。

`NodeRuntime::sync_freshest_certified_public_currency_view()` 把这些长期连接用于实际 public-state 观察：先向所有当前 active peer 请求 checkpoint proof，对低于本地 `checkpoint_floor_epoch` 的 proof 直接丢弃，并使用本地 snapshot 中的当前 `ValidatorSet` 在下载完整 public state 之前验证 checkpoint finality；只有 finality 有效的候选才按 checkpoint epoch 从高到低进入下载阶段。伪造一个更大的 epoch 不会获得选择优先权，因为未通过 Validator quorum 的 proof 不进入候选。checkpoint probe 每个 peer 最多等待 5 秒，完整 public sync 每个候选最多等待 30 秒，失败后继续下一个有效候选，避免一个不响应的 peer 独占整个选择流程。

该 runtime sync 的结果是 `RemoteCertifiedPublicCurrencyView`，明确属于经过验证的网络观察结果，而不是本地业务状态写入。它不会用 public view 覆盖本地 `SecondState`，也不会自动推进持久 `checkpoint_floor_epoch`：public view 不包含 owner、PaymentAddress、claims、prepared task 等 private/protocol state，不能成为完整 Second state 的替代品。CLI `second observe-public-network <snapshot-base>` 是这条语义的真实入口：先加载同一 snapshot-base 的 PeerStore + 可选 bootstrap sidecar，按 runtime 默认 active-peer target 建立 authenticated connections，然后输出选出的最高有效 certified public view；命令退出后 snapshot 的 private state、checkpoint floor 和 attached proof 保持不变。是否把某个远端观察结果提升为本地持久状态属于另一条尚未定义的同步/恢复协议，不能由 public read path 猜测完成。

当前公开节点网络面暴露 Ping/Pong、public Currency 查询/同步以及有界 `GetPeers/Peers` reachability discovery。BFT Proposal/Prevote/Precommit/QC、不可逆 `FinalityVote` 与 `FinalityCertificate` 都不进入这条 authenticated-open public session，而是使用独立 Validator-only BFT connection：先用 active Validator identity key 做 channel-bound 会话认证，再传 bounded binary BFT envelope。NodeRuntime 当前会为 PublicCheckpoint、ValidatorSetTransition、StateRecoveryCheckpoint 维护这条共识运行时；proposal envelope 仍只承载 digest/签名等共识元数据，不负责分发对应业务对象。LegalTask / PreparedTask 的隐私安全对象传播方案尚未冻结，因此当前 public runtime 不会擅自广播这些私有对象，也尚未把 PreparedTask 自动接入该节点级 coordinator。

当前 public Node admission 是开放的：任何能够完成 authenticated Hello、证明持有其 NodeId 对应 transport private key 的 peer，都可以使用上述公开只读网络能力，前提是通过现有 connection / stream / sync resource guardrail 与 peer 去重规则。public admission 不维护 allowlist，也不要求 ValidatorCredential，因为这些数据本来就是公开状态。

这不意味着 Second 的 Validator 层是 permissionless。transport-authenticated public peer 只获得公开网络访问权；Validator vote、ValidatorSet membership、未来任何私有 LegalTask 通道或其他 privileged protocol 都必须使用各自独立的授权规则。peer discovery 只传播 reachability candidate，不建立任何 authority；未来 privileged peer role 若需要与 transport endpoint 绑定，必须另外定义可验证 binding，不反向改变当前 public read service 的开放 admission。

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
- certified public state sync；
- 有界 `PeerRecord` 查询与 bootstrap peer discovery。

full public sync 的本地 materialization budget 当前为 64 MiB，仅约束客户端为 `Vec<PublicCurrencyState>` 物化整份公开状态所允许占用的元素存储空间。允许的 state 数量通过当前 `size_of::<PublicCurrencyState>()` 动态换算，而不是把 Currency 数量写成协议上限；远端 summary 声明的 `current_supply` 超出预算时，客户端必须在请求任何分页数据之前拒绝。实际 Vec 使用 `try_reserve_exact`，无法满足本地分配时返回错误而不是继续无界增长。该预算同样是本地资源保护策略，不限制 Second 协议本身允许存在多少 Currency。

### 16.1 Node 与 Validator 分离

普通 Node 可以：

- 同步公开状态；
- 验证公开状态；
- 保存/转发公开数据；
- 进行审计。

只有有效 ValidatorCredential 才拥有 finality vote 权。

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

加载时选择有效 generation 中最新的一份。同一 generation 的 primary slot 完成 `write_all + sync_all` 即构成 durable commit point；另一 slot 是同 generation 的恢复镜像，镜像刷新失败不能把已经 durable 的提交重新报告为失败。读取时只要至少一份 slot 有效即可恢复；若同 generation 存在两份内容不同但都有效的 snapshot，则仍视为冲突并拒绝。

文件槽位 I/O、快照 codec、validator codec、local prepared codec 已按职责拆分。

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

`StateStore` 本身不是资产或协议状态 mutation API。空 store 可以通过 `initialize` 一次性写入 bootstrap state；需要携带既有 ValidatorRegistry 历史时使用同样仅限空 store 的初始化入口。初始化完成后，不再提供接受任意 `SecondState` 并覆盖当前 snapshot 的公开 writer。checkpoint proof / checkpoint floor 更新只作用于锁内读取到的最新 snapshot metadata，不接受调用方附带另一份 state。后续 LegalTask 导致的 `SecondState` 演进只能由正式 `PreparedTaskBook` 的 prepare / finality / commit 持久化路径完成。

### 17.4 State recovery commitment

完整恢复分成“网络可共同认证的 shared state”和“Validator 自己必须连续保存的 local safety state”，二者不得混成一个 digest。`StateRecoveryCheckpoint` 当前承诺的 shared state 只包含：完整 `SecondState`（因此包括 committed TaskId bindings、PaymentAddress/ownership、payment execution prerequisite 等协议/业务事实）、当前 active `ValidatorSet`、永久 `ValidatorRegistry`。canonical bytes 复用 persistence 当前权威字段编码器；persistence codec 与 recovery commitment 不分别维护两套 `SecondState`/Validator 编码规则。

以下字段明确**不进入** shared recovery digest：snapshot slot `generation`、attached public checkpoint proof、`checkpoint_floor_epoch`、recovery checkpoint freshness floor、active/retained PreparedTask 本地 lifecycle、`retained_validator_sets`、BFT round/prevote/precommit/lock/finality-ready state、Validator finality vote-lock。`retained_validator_sets` 只为本节点仍活跃并绑定旧 set 的 PreparedTask 服务；recovery floor、BFT local state、finality vote-lock 与 PreparedTask phase 都是 Validator/节点本地 safety/recovery metadata，不保证不同节点相同。把这些字段塞进 quorum shared digest 会导致诚实 Validator 因各自本地签票/观察历史不同而无法形成同一 QC，并且“签 recovery checkpoint 本身新增本地 safety metadata”会造成自引用 digest 循环。

`StateRecoveryCheckpoint` 具有独立 `serial`，与 public checkpoint epoch、`ValidatorSet.version`、snapshot generation 均不是同一序列；checkpoint digest 使用独立 domain separation，并把 protocol version、serial、当前 validator-set version 与 shared-state digest 一起绑定。`ValidatorSigner` 只能用持久 `ValidatorRegistry` 认可的当前 active ValidatorSet 对它签票，vote-lock scope 为 `(validator_set_version, serial)`；同一 scope 重放同 digest 允许，同 scope 不同 shared state 永久拒绝。

节点还为 recovery checkpoint 持久化独立的 serial head，按 `ValidatorSet.version` 分桶。每个桶保存最高 `serial + checkpoint_digest + certified`：本地第一次为某个 recovery checkpoint 建立 vote-lock 时，serial head 与 vote-lock 在同一 snapshot 写入，但此时 `certified = false`；节点显式接受/发布一个已经通过当前 active ValidatorSet QC 的 recovery checkpoint 后，才把对应 head 标为 certified。低于当前 head 的 serial 一律拒绝；同 serial 的本地重复签名只允许同 digest；如果本地只投过某个未 finality 的候选，而同 serial 的另一 digest 后来取得合法 QC，则允许该 QC 覆盖未 certified 的本地 head，因为本机并未对新 digest 再次签票。若 head 已经 certified，则同 serial 不同 digest 永久拒绝。

recovery serial 的发行规则现已固定：**每个新的 `ValidatorSet.version` 从 serial 1 独立开始；Validator 只能为本 set 当前已 certified serial 的严格 `+1` 签票，不能跳号，也不能仅凭自己对前一号投过票就继续下一号。** `StateStore::next_state_recovery_checkpoint()` 是本地权威发行入口：没有本 set head 时生成 1；存在未 certified head 时返回 awaiting-finality；存在 certified head `S` 时只生成 `S+1`。serial 溢出直接 fail-closed。ValidatorSet transition 不删除旧 set 的历史 head，但新 set 使用独立桶，因此不会继承旧 set 的 serial 数字。

已通过 QC 的 checkpoint 属于更强的网络事实：节点可以直接接受高于本地 head 的 certified serial 进行离线 catch-up，包括空 store 直接安装当前较新的 recovery checkpoint。这样不要求恢复节点下载从 1 开始的全部历史 QC；合法高 serial QC 的 quorum 中至少包含遵守签票规则的诚实 Validator，因此其存在意味着该 set 的连续 serial 前驱已经按协议推进。该 catch-up 只推进本地 certified head，不允许未经 QC 的任意跳号。

这套规则解决的是 recovery checkpoint 的**编号、freshness 与 anti-equivocation 协调**，没有被 BFT liveness 工作重写。现有 per-scope BFT 在同一 safety core 上已经具备 round/prevote/precommit/locking、precommit-QC→FinalityVote gate、确定性 proposer、Validator-only BFT transport，以及本地 timeout 驱动的严格 `+1` view-change；StateRecoveryCheckpoint proposal subject 仍必须通过既有 recovery serial/floor 与 exact persisted shared state 校验，并与 `next_state_recovery_checkpoint()` 复用同一个 persistence serial 计算 helper，不维护第二份 recovery 编号规则。多个节点即使对同一 next serial 出现不同 proposal，锁与 quorum intersection 继续负责 safety，round-robin proposer + `Nil` timeout 路径提供 liveness 推进原语。现在 `NodeRuntime` 已能维护 active Validator-only peer 集合、对 PublicCheckpoint / ValidatorSetTransition / StateRecoveryCheckpoint fanout 与接收 BFT envelope、驱动每个已注册 scope 的本地 timeout、缓冲先于本地注册到达的有限消息，并在 precommit QC 后复用既有 signer 产生不可逆 FinalityVote、聚合/relay FinalityCertificate。PreparedTask 的 core safety 仍存在，但其私有 LegalTask/plan 对象传播与节点级自动注册尚未冻结，因此没有被公共 runtime 猜测实现。实现仍不会用本地时钟、snapshot generation、public checkpoint epoch 或 `ValidatorSet.version` 冒充 recovery serial。

`CertifiedStateRecoveryCheckpoint` 复用通用 `FinalityCertificate`，因此阈值仍是当前 active ValidatorSet 的 `floor(2N/3)+1`。可信的是 quorum 对 shared-state commitment 的证明，不是提供 recovery payload 的某个 peer。retained old ValidatorSet 没有发布新 recovery checkpoint 的 authority；旧 set 只继续服务其绑定的历史 PreparedTask。

privileged recovery 已复用现有 QUIC/TLS connection 与二进制 framing，但授权层与 authenticated-open public read 明确分离。`NodeId` / transport key 仍只证明 transport peer；请求 private recovery manifest/chunk 时，调用方必须额外声明当前 `ValidatorId`，并使用该 active ValidatorCredential 的 **identity key** 对 recovery request 签名。签名 domain 独立，并绑定 `CURRENT_NETWORK_PROTOCOL_VERSION + 当前 QUIC TLS exporter channel binding + ValidatorId + request kind`；chunk 请求还绑定 checkpoint digest、offset、limit。服务端只按自己当前 active `ValidatorSet` 的 identity public key 验证，错误 key、未知/retired ValidatorId 都统一拒绝。consensus key 继续只用于 finality，recovery key 继续只用于既定 recovery/rotation authority，不与会话认证职责复用。

`StateRecoveryPayload` 的 bytes 就是 shared-state commitment 使用的同一份 canonical `SecondState + active ValidatorSet + ValidatorRegistry` 编码，不创建第二套私有 snapshot serializer。下载先取得 manifest 中的未验证 recovery checkpoint proof 与 payload length；客户端必须先用调用方已经信任的 exact `ValidatorSet` 验证 QC，再请求 payload chunk。payload 以最多 60 KiB 的 chunk 传输，避免被 64 KiB network frame 上限卡住；每个 chunk request 都重新做 channel-bound identity proof，并绑定 checkpoint digest/offset/limit。组装完毕后重新 decode canonical payload，再次验证 payload digest、exact ValidatorSet 与 certified checkpoint，一处不一致即 fail-closed。QUIC 已提供传输加密，不另造应用层加密格式。

`NodeRuntime::publish_state_recovery_checkpoint` 只显式发布一个与 runtime 当前 shared state 匹配的 certified checkpoint，并把 immutable provider 放在内存中；发布前必须先验证 QC/shared payload 并 durable 推进 recovery freshness floor，失败则不能把 provider 暴露出去。runtime 仍不会因启动节点就自动选择下一个 recovery serial。provider 可以被更新的 certified checkpoint 替换；正在下载旧 digest 的客户端若因此无法继续，应从 manifest 重新开始。

`StateStore::install_recovered_state` 只允许写入**空 store**：先验证 trusted ValidatorSet、QC、payload exact set 与 shared-state digest，再通过现有 dual-slot writer 一次性安装。任何已有 snapshot 都返回 `AlreadyInitialized`，因此 recovery 不能成为任意 state overwrite API。安装结果只包含 recovered shared state；retained sets、PreparedTask lifecycle、vote-lock、attached public checkpoint proof 均为空，public checkpoint floor 当前从 0 开始；recovery freshness floor 则由安装所依据的 certified recovery checkpoint 初始化。snapshot 同时写入 `validator_safety_ready = false` 和 `minimum_signing_validator_set_version = recovered ValidatorSet.version`，所以只恢复 shared state 的 Validator **可以读取/继续恢复数据，但不能重新签任何 finality vote**。

local-safety re-enable 采用 **consensus-key rotation safety fence**，不尝试重建无法证明完整的旧 vote-lock 历史。恢复中的 Validator 必须先由 identity key 或 recovery key 授权一个从 V 到严格下一版 V+1 的 `ValidatorConsensusKeyRotationRequest`，把自己的 consensus key 旋转到从未使用过的新 key；V→V+1 的 `CertifiedValidatorSetTransition` 必须由其余 Validator 独立达到 quorum，证书中出现恢复中 Validator 自己的票则本地拒绝解锁。随后节点必须安装/持有 V+1 的 certified shared recovery state；本地恢复 signer 还要证明自己持有 V+1 credential 对应的新 consensus private key。只有这些条件同时满足，`ValidatorSigner::complete_safety_recovery` 才会原子把 `validator_safety_ready` 置回 true，并把 durable `minimum_signing_validator_set_version` 固定到 V+1。

之后所有签票入口除了检查 `validator_safety_ready`，还先检查所用 ValidatorSet.version 不得低于该 minimum。正常未丢失 safety state 的节点初始化时 minimum 等于其最初 active set version，后续正常 transition 不抬高它，因此仍可为合法 retained old-set PreparedTask 服务；经过 safety recovery 的节点 minimum 则从新 signing domain V+1 开始，哪怕旧 consensus private key 后来又从备份中被找回，也会因为 signing fence 永久拒绝 V 及更旧 scope。该 fence 与 recovery floor、vote-lock 一样属于本地 safety metadata，不进入 shared recovery digest。

这条流程只解决“consensus signing key 仍可通过 identity/recovery authority 安全轮换”的情况。如果 identity/recovery authority 也丢失或怀疑泄露，则不能用同一个 ValidatorId 解除 fail-closed；应通过正常 ValidatorSet transition 永久退休旧 ValidatorId，再以全新 ValidatorId 和全新三把 key 重新 admission。旧 PreparedTask 若仍绑定 V，只能由仍具备 V local safety continuity 的其他 Validator 继续完成；恢复节点不会重新加入旧 signing domain，liveness 不足时也不能用 recovery 绕过 safety。

---

## 18. 当前模块边界

| 模块 | 职责 |
| --- | --- |
| ids.rs | Account / Payment / Currency / Task / Validator 身份格式 |
| currency.rs | Currency 内部对象与公开 Currency state |
| state.rs | Second 主状态容器、allocator、task binding、公共查询 |
| payment.rs | PaymentAddress 生命周期与 Transfer establishment |
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
| validator.rs | ValidatorCredential / ValidatorSet / quorum |
| validator_registry.rs | Validator 永久历史与 key reuse 防护 |
| validator_admission.rs | Validator admission request |
| validator_rotation.rs | consensus-key rotation |
| validator_transition.rs | ValidatorSet transition |
| validator_signer.rs | BFT 签票与 precommit-QC gated 的不可逆 finality 签票入口 |
| persistence/bft_store.rs | durable per-Validator/per-scope BFT round/lock/finality-ready state |
| public_state.rs | 公开 Currency summary / view |
| public_checkpoint.rs | 公共 checkpoint 与 finality proof |
| state_recovery_checkpoint.rs | shared recovery payload/checkpoint/QC commitment |
| network/ | 二进制网络 framing / session / public state sync |
| network/bft_codec.rs | bounded BFT Proposal/Vote/QC/FinalityVote/FinalityCertificate binary envelope codec |
| network/bft.rs | channel-bound active-Validator identity session 与 bounded one-way Validator-only BFT envelope transport |
| runtime_bft.rs | Validator-only peer 维护、inbound queue、优先级/有界并发 fanout 与 send-failure 生命周期 |
| runtime_bft_consensus.rs | NodeRuntime per-scope BFT coordinator、timeout、early-message buffering、FinalityVote/Certificate 收敛 |
| runtime_consensus_target.rs | PublicCheckpoint / ValidatorSetTransition / StateRecoveryCheckpoint 到既有 proposal/finality 类型的单一适配层 |
| network/recovery.rs | channel-bound Validator identity 授权与 chunked private recovery transport |
| persistence/ | 私有 snapshot、prepared、vote-lock、registry 与空-store recovery 安装 |
| transaction.rs | 外部 transaction request 严格解析 |
| main.rs | 当前 CLI / 可执行网络入口 |

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

## 21. 当前仍需继续完成/审核的部分

当前代码已经覆盖大量协议骨架，但仍不能把整个系统称为完成。

后续重点：

- privileged recovery transport、active-Validator identity 授权、chunked shared payload、空-store 原子安装、按 ValidatorSet 独立且必须 certified 后才能 `+1` 的 recovery serial head，以及基于独立 quorum key-rotation transition 的 local signing safety fence 已具备；recovery serial 不再依赖未定义的本地计数器，也没有被本轮 BFT liveness 工作重做。per-scope BFT core 的 round/prevote/precommit/durable locking、precommit-QC→FinalityVote gate、deterministic proposer、Validator-only BFT transport、timeout/view-change driver，以及 PublicCheckpoint / ValidatorSetTransition / StateRecoveryCheckpoint 的 NodeRuntime multi-peer orchestration 已接到同一条 consensus 路径；FinalityVote/FinalityCertificate 也沿该 Validator-only 通道验证和收敛，identity/recovery authority 本身丢失时仍走旧 ValidatorId 退休 + 新 ValidatorId admission，而不是绕过 quorum；
- 如果 owner 隐私需要“公开可验证证明”，再单独决定具体密码学机制；
- PreparedTask 的 BFT safety core 已存在，但 LegalTask / prepared-plan 的隐私安全网络传播方案和节点级自动注册尚未冻结，因此当前 NodeRuntime 不会把 PreparedTask 对象公开广播，也不会把 digest transport 偷换成业务对象分发协议；后续接入时必须复用现有 vote/QC/lock/finality 状态机；
- 持续检查 persistence / network / executor 等大模块是否开始职责混杂，避免形成 God File。

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
