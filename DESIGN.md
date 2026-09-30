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

一旦 identity range 正式分配，它永久消耗，即使同一 LegalTask 后续业务失败也不能复用。

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
- 不重复 Issue / Transfer / Destroy / Leak Repair。

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
Validator vote
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

只有 `Prepared` phase 允许 cancel。第一次通过 `sign_prepared_vote()` 打开投票路径时，必须先把 `Prepared → Voting` 持久化，再尝试产生 Validator vote；该转换不可逆。`commit()` 验证到有效 FinalityCertificate 后，必须先把 phase 持久化为 `Finalized`，再尝试 apply，因此即使本地 apply 暂时失败，任务也不能再 cancel。`Voting` / `Finalized` 都必须跨重启恢复。

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

Validator transition / admission / rotation 仍然必须经过既有验证逻辑，不能绕过 Registry 的永久历史约束。

Validator admission 的治理来源固定为**当前 active ValidatorSet 的 finality**：候选 Validator 的 `ValidatorAdmissionRequest` 只证明候选方同时控制其声明的 identity / consensus / recovery 三把 key，并不自行授予 Validator 权限。新 Validator 只有在其 credential 被纳入 `next_validator_set`，整个 `ValidatorSetTransition` 又由当前 ValidatorSet 达到正常 finality quorum 后，才获得协议授权，并在声明的下一 activation epoch 生效。

因此，Validator membership 的授权链是：当前 ValidatorSet → 对 next ValidatorSet transition 的 QC → 下一 epoch 激活。Genesis 初始 ValidatorSet 是这条治理链的根；之后不引入持币量、算力、transport NodeId、单个管理员 key 或候选人的自签 admission 作为独立治理权。RecoverySet 的未来规则和现实世界“一人一 Validator”仍是独立未决问题，不改变当前协议内的 membership authorization source。

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

同一 transport Ed25519 key 同时用于生成节点的 TLS self-signed certificate，并用于 Hello peer-auth signature。Hello signature 的签名输入绑定当前 network protocol version、client/server role 与 Quinn TLS exporter 派生的 per-connection channel binding；因此旧连接上的 Hello 不能在另一条 QUIC connection 上重放，client proof 也不能直接反射成 server proof。只有签名验证成功后，Hello 中的 NodeId 才能成为 `QuicPeer::remote_node_id()`。

当前长期节点入口为 `second node <listen-address> <snapshot-base>`。长期节点的 transport private key 保存在独立的 `<snapshot-base>.transport` 本地 sidecar 中，不写入 Second state snapshot：首次不存在时创建，之后重启必须复用；已有 identity 文件损坏或无法解析时启动失败，不静默生成新身份。该文件包含私钥，Unix 创建权限为 `0600`。由同一 key 重建的 certificate 和 NodeId 在重启后保持稳定。当前 CLI 客户端仍显式 pin server certificate；一次性 CLI client 使用临时 transport identity。

transport authentication 只证明“当前 QUIC peer 持有这个 NodeId 对应的 transport private key”，不自动授予 Validator 权限、网络信任或 admission。Validator 权限仍只来自有效 ValidatorCredential；trust/discovery/admission 仍需要独立规则。

当前 runtime 已有进程内 peer manager。peer 只有在 authenticated Hello 完成后才进入 registry；registry 以 NodeId 为唯一键，同一 NodeId 在同一节点上同时只允许一条 active connection，自连接（remote NodeId 等于 local NodeId）直接拒绝。当前 runtime 只有 inbound connection，因此重复连接采用 first-active-wins：已有连接继续服务，后到的重复连接关闭。connection task 结束时 lease 自动释放 NodeId，registry 不进入 snapshot，也不作为任何协议状态或共识状态持久化。

如果以后加入主动 outbound dialing，不能直接沿用当前 inbound-only first-active-wins 作为双向连接竞态规则；届时必须在同一个 peer manager 上定义确定性的 simultaneous-dial arbitration，避免双方各自保留不同 connection 或互相关闭导致无连接。该规则在 outbound runtime 真正实现前不提前冻结。

节点启动时恢复 snapshot，持续接受 QUIC connection；每个通过 peer manager 注册的连接独立运行 public Currency session，因此单个 peer 的断开、错误请求或握手失败不会结束 listener。当前 runtime 最多同时保留 128 个进入握手/已建立的 connection；超过该本地容量的新 `Incoming` 在握手前直接拒绝。这个 128 是节点实现的 DoS / 资源保护默认值，不是协议、共识或 Validator 数量规则，不进入任何签名、frame 或 snapshot 版本。

当前节点网络面只暴露 Ping/Pong 与 public Currency 查询/同步；LegalTask、Validator vote 和其他私有/共识消息尚未定义网络传播协议，因此不会由 node runtime 猜测实现。

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
- certified public state sync。

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

不能因进程重启让“永久事实”消失或让 Validator 获得改票机会。

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
| finality.rs | FinalityStatement、ValidatorVote、certificate 验证 |
| validator.rs | ValidatorCredential / ValidatorSet / quorum |
| validator_registry.rs | Validator 永久历史与 key reuse 防护 |
| validator_admission.rs | Validator admission request |
| validator_rotation.rs | consensus-key rotation |
| validator_transition.rs | ValidatorSet transition |
| validator_signer.rs | 永久 vote-lock 后的签票入口 |
| public_state.rs | 公开 Currency summary / view |
| public_checkpoint.rs | 公共 checkpoint 与 finality proof |
| network/ | 二进制网络 framing / session / public state sync |
| persistence/ | 私有 snapshot、prepared、vote-lock、registry 恢复 |
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

- 在现有 authenticated NodeId + inbound peer manager 基础上继续完成 peer trust/discovery/admission；如果引入 outbound dialing，再补确定性的 simultaneous-dial arbitration；transport identity 只证明 key ownership，不等于 Validator authority；
- 如果 owner 隐私需要“公开可验证证明”，再单独决定具体密码学机制；
- 如果需要完整 Byzantine consensus state machine，再单独设计 round / locking / view-change；当前 quorum certificate 本身不等于完整 BFT consensus；
- LegalTask 的隐私安全网络传播方案目前未冻结，因此不能直接公开广播；
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
