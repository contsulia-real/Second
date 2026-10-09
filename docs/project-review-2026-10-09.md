# Second 完整仓库理解核对

2026-10-09。本文依据当前工作区源码、全部项目文档、测试、示例、部署脚本与构建配置的全文阅读。不是钱包实现方案，也不是重新宣布全部开发验收通过。

## 阅读范围与证据

全文覆盖 273 个非隐藏项目文件，包括 DESIGN.md、9 份 docs、188 个 src 文件、62 个 tests 文件、5 个 examples、5 个 deploy 文件，以及 AGENTS.md、Cargo.toml、Cargo.lock。逐段阅读范围累计 3,034,448 个 UTF-16 字符，逐文件检查无缺口；阅读结束时再次核对 SHA-256，273 个文件均未发生变化。随后补读 .gitignore 与 .cargo/config.toml，总计 275 个文件。

target 中的编译产物、临时网络数据和日志，以及 .git 内部对象不属于源码全文覆盖范围。第三方依赖锁文件已阅读，但没有审阅依赖的全部源码。阅读清单和哈希保存于 target/project-review-2026-10-09.json，记录修改前的阅读快照。本文和本次修改不计入该快照。

这次发现并复现了支付执行记录长度校验缺陷，因此又进行了修复和回归验证；验证结果另见文末。此前十分钟混合集群和系统服务测试的历史结果不冒充本次重新运行。

## 系统的实际边界

Second 接收已签名 LegalTask，验证输入和协议权限，确定具体执行计划，由任务所属委员会确认终态，再原子改变 Currency 的归属及相关业务状态。Secoin 是此网络中的原生货币单位；代码没有法币锚定、外部储备兑付或商业担保协议。

外部业务负责决定为什么签发任务。协议负责检查签名、结构、执行条件、冲突和最终性。任务签名有效不代表余额充足，也不证明链外商业承诺真实。服务商组织、保证金政策与商业赔付不属于现有内核；用于 LeakRepair 的 reserve 有专门资产语义，不能解释成业务保证金。

当前实现按 ConsensusScope 区分共识，没有区块或全局交易总排序。但现有任务委员会由 active/retained ValidatorSet 确定，没有实现每个账户或货币各自选择任意独立分片委员会。普通公开节点与验证者也不是同一类数据副本。低冗余目标不能被直接表述成所有对象分片能力已经交付。

## 四种标识与资产真值

| 标识 | 当前职责 | 不应从中推导的能力 |
| --- | --- | --- |
| CurrencyAddress | u64 货币对象标识，独立 owner 和 role，分配后永不复用 | 不是账户余额数值或账户公钥 |
| AccountAddress | 32 字节账户标识，账户集合中的永久注册事实 | 未绑定账户签名密钥，未建立读取 ACL |
| PaymentAddress | 32 字节支付标识，永久绑定一个 AccountAddress，带生命周期 | 不是新的账户或第二份资产账本 |
| TaskId | 1..128 字节限定字符标识，永久绑定精确签名请求摘要 | 不能用相同 ID 替换请求，也不是读取账户的凭证 |

资产唯一真值是 SecondState.business.currencies 中的 Currency.owner。src/state.rs 的 SecondState::balance 已实现 owner 等于指定账户的货币对象计数，返回 u64；无需另建余额表。这个方法对不存在的账户也返回零，钱包必须结合 has_account 判断，不能混淆“没有账户”和“余额为零”。

公开状态只包括货币地址、occupied、role 及其摘要、证明。tests/integration/public_state_summary.rs 特别验证：两份状态只有 owner 不同时公开摘要相同，账户间转移也不改变公开货币摘要。因此公开状态不足以推导个人余额。

## 业务操作与状态层次

src/task.rs 定义八种操作；“四种基础 Operation”仅指基础资产操作，不代表账户和支付地址操作没有实现。

| 操作 | 已实现的语义 |
| --- | --- |
| RegisterAccount | 注册永久账户标识，重复注册拒绝，遵守整个任务业务原子性 |
| RegisterPaymentAddress | 注册到既有账户；可在同一任务先注册账户再注册地址 |
| Transfer | 经支付地址解析账户，动态选择指定数量的货币，冻结选择后按证书提交归属变化 |
| Issue | 使用认证分配区间生成独立货币，归属于目标账户 |
| Destroy | 只销毁指定的无 owner 流通货币 |
| LeakRepair | 用 reserve 替换泄漏对象并补充新的 reserve 标识，保持余额、供给和 reserve 数量 |
| RetirePaymentAddress | Active → Retiring，阻止新的支付建立，允许既有执行结束 |
| FinalizePaymentAddressRetirement | Retiring → Retired，要求没有在途执行或认证交接 fence |

SecondState 分成三层：ProtocolState 保存前沿、TaskId 绑定和交接事实；PrerequisiteState 保存已经建立的支付执行；BusinessState 保存账户、支付地址和货币。业务失败回滚账户/货币等业务变化，但不会撤销已生效的 TaskId 绑定、认证地址分配或已建立执行。这是有意的协议语义，不能用一次全状态回滚替代。

## 从输入到提交的完整链路

1. 严格 JSON 请求映射为 LegalTask；签名使用唯一的 canonical 编码，网络紧凑编码另有明确大小限制。操作顺序属于请求身份。
2. AuthorizerSet 验证版本、受信任签发者和 Ed25519 签名。当前配置是受信任公钥集合，没有逐账户或逐操作 ACL。NodeId、Validator 身份、Authorizer 密钥各有职责。
3. TaskId 与精确请求摘要绑定。需要新 CurrencyAddress 时，CurrencyAllocation 在对应版本和前沿 scope 获得认证；普通转账无需经过地址分配。
4. PreparedTaskBook 按操作顺序验证完整业务，并冻结货币选择和计划摘要。资源 claims 只表达在途占用，不能提前改变 owner。
5. BftDriver 在实际 scope 中完成 proposal、prevote、precommit。持久锁、round、phase、QC 和 finality statement 分别约束签票与终态；Nil 或超时不等于任务取消。
6. 最终证书先持久化。业务应用失败时保留 Finalized 状态用于恢复，不能本地撤销证书或释放任务。
7. 被依赖、冲突和 fence 连接的任务组件必须全部满足认证终态，才一次原子写入业务结果、移除占用并唤醒等待者。成功请求重放不重复转账，永久 TaskId 结果不依赖有界 receipt 缓存。

对外状态已有 Unknown、Bound、Prepared、Voting、Finalized、Succeeded、Cancelled。提交返回 accepted 或 allocating 不等于资产已经改变；只有 succeeded 表示业务提交完成。任务查询匹配 TaskId 和原请求摘要，不能只凭 TaskId 对不一致请求返回原结果。

## 冲突、冻结变体与交接

同一货币或注册/生命周期资源存在竞争时，协议在原 PreparedTask scope 确定 Commit/Abort。TaskId 的确定性优先只适用于没有受保护 Commit 证明的竞争；不能压过已验证 Commit prevote QC、finality 或持久签票约束。

完整验证的 foreign witness 可以被保存，但不自动取得资源占用、Commit 权限或签票权。同一请求可以出现不同的合法冻结选择；各候选按精确摘要跟踪，不能让新公告覆盖另一候选的分片、已有占用或旧票。最终证书先到、正文后到也有恢复路径。

成员切换交接包含业务基线、冻结计划、原始待办请求及其原委员会。Collecting 阶段合并义务，封口后根不可改写；来源消息或较小远端正文不能取消本地任务。认证切换可以退役未受保护的遗漏本地权限，但保留原始签名请求供以后重新受理。

新成员安装认证基线后仍保持签名隔离，不继承原节点本地 claims、vote locks 或签票资格。历史终态只在交接列出的原委员会和任务范围内被动安装。保留旧 ValidatorSet 是完成这些具体义务的需要，不是允许旧集合继续处理任意新任务。

## 持久化与安全恢复

StateStore 的权威写入使用排他锁、业务/任务 CAS 和双槽提交记录。主槽与 commit 记录确认当前 generation，镜像写入尽力完成；损坏时可以恢复相同已提交内容，不能退回旧镜像继续签票。快照冷加载会核验状态、计划、注册表、认证分配、签票锁、QC 和交接的关联。

共享恢复只带业务/必要协议事实、ValidatorSet/Registry 与必要历史委员会，不复制 prepared claims、本地 BFT 状态、签票锁或治理工作队列。恢复目标必须为空，安装后 safety=locked。重新签名要求独立旧委员会认证的密钥轮换，以及当前恢复证明和 signing fence；单纯恢复数据或追上成员版本不足以解除隔离。

钱包备份应保存钱包材料与未完成原始请求。它不能被当作验证者签名安全历史的恢复证据。

## 网络、运行时与部署

QUIC 通过持久传输身份和证书固定建立 NodeId；BFT、恢复和交接有各自绑定会话与角色的身份认证。PeerManager 对同时互拨作确定性连接选择，旧 lease 不能移除替换后的连接。PeerStore 仅缓存经过认证的可达信息，不承担协议权威；坏缓存不会重置业务状态。

网络请求、连接、候选、分片和字节均有边界。拥堵时保留已认证连接并等待容量；普通消息与最终性消息有独立预算。存储工作移出网络 executor，失败后的定时重试不能空转。运行时围绕通知和最近期限推进，重启从持久 phase/round/lock 恢复，而不是重做已经签过的决定。

节点可以使用完整 StateStore 或 PublicStateStore，Validator 是额外能力。完整状态后端但未启用验证者的节点，也不能被自动等同于“只有公开数据”。反之，公开后端没有 owner、账户集合或支付地址绑定。钱包软件可以组合普通 NodeRuntime，是否启用 Validator 与是否是钱包没有必然关系。

现有 CLI 已提供网络初始化、节点运行/检查、Authorizer/Validator 密钥工具、transaction-sign、submit、task-status、公开同步、交接/恢复和成员操作。没有 second-wallet 二进制或完整账户钱包命令。

部署覆盖 Windows SCM 和 Linux systemd、独立节点目录、拒绝覆盖、异常自动重启及保留状态的卸载。package.py 按白名单生成本机平台包和 SHA-256 manifest，不打包私钥。构建配置当前为 jobs=8、Windows rust-lld.exe；不是运行时协议配置。

## 模块与测试对应关系

| 路径 | 核心职责 | 关键行为证据 |
| --- | --- | --- |
| ids/account/currency/payment/state | 标识、归属、账户和生命周期 | account_registration、address_format、core_protocol、payment_address |
| task/authorization/transaction/legal_task_codec | 请求身份、结构、编码和签名 | authorization、transaction_request、task_id |
| claims/prepared/executor | 冻结、占用、组件原子提交、冲突 | prepared 单元回归、prepared_tasks、runtime_conflicts、runtime_prepared_source |
| bft/bft_driver/validator_signer/finality | scope 共识、持久签票与证书 | bft_core、bft_driver、validator_vote_lock、runtime_bft |
| currency_allocation | 唯一地址前沿与认证分配 | runtime_allocation、runtime_allocation_capacity |
| validator_registry/transition/operator/keyring | 准入、历史身份和轮换 | validator_*、cli_governance、cli_node_reentry |
| persistence | 唯一快照、交接基线、锁、receipt 和恢复 | persistence、codec/handoff/receipt 单元回归、state_recovery_* |
| network/runtime_* | 传输、私有按需拉取、后台推进和重启 | peer_*、network_*、runtime_*、34 节点真实签名/QUIC 测试 |
| CLI/deploy/examples | 外部提交、进程和系统服务 | cli_*、mixed_wsl/soak、service-smoke.py |

测试包含真实进程、QUIC 和持久化，也包含直接构造 QC 的协议夹具。夹具构造终态可证明状态应用和拒绝边界，不能单独证明在线委员会曾实际达成共识。对此必须看对应 runtime 或 CLI 集群测试。

## 对钱包讨论的更正

钱包应复用已有账户、支付地址、余额计算、LegalTask 和任务状态，不再设计替代模型。账户标识不等于账户签名密钥；可信签发者不等于账户所有人或私有读取者。之前自行推导的开户签发者读取规则已从 cli-wallet-design.md 撤回，该文件仍是未经确认的草案。

完整状态本地计算余额已有能力；普通公开节点取得个人资产数据的网络接口与证明目前没有实现。真正要闭合的是钱包的数据来源、访问边界和结果验证方式。不能把余额未知显示成零，也不能用本地发送记录冒充完整资产状态；同样不能用公开同步不足否认账户 ownership 模型。

## 本次发现与修复

src/persistence/codec.rs 的支付执行记录编码实际为 TaskId 长度与字节、operation_index、两个 PaymentAddress、amount。最短合法记录是 82 字节；原计数保护写成 90 字节。过高的下界会把有效短 TaskId 数据错误判为 InvalidSnapshot。

先以当前源码构建库，使用真实 prepare 路径建立零余额账户的未完成支付：41 笔执行、5,530 字节恢复数据即可复现自身编码无法解码。进一步增加 100 笔的集成回归，修复前独立 StateStore 冷加载报 NoValidSnapshot。这不仅是钱包展示问题，也影响共享恢复和冷加载。

修复仅删除最小长度公式多算的 8 字节，保留数量预算、字段校验、签名和恢复验证；编码格式与开发版本 1 均未改变。新回归验证 100 笔短 TaskId 建立执行的独立冷加载、共享恢复解码、执行数量和逐字节重编码一致性。

同时更正 DESIGN.md 正文中“仲裁运行时尚未交付”的过时判断和 node-operations.md 关于失败 staging 的描述。历史带日期的日志仍保留原时间语义，不能把早期未完成记录当成当前状态，也不能用新的结果改写当时的失败事实。

修复后的定向回归通过；随后 `cargo fmt --all -- --check`、`cargo check --all-targets`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all-targets` 和 `git diff --check` 全部通过。测试门禁包含真实 CLI、QUIC 集群及持久化行为。

本次完整阅读不是密码学安全证明或无限规模性能验收；未重新运行十分钟 Windows/WSL 混合集群及真实系统服务生命周期。本次修复的本地门禁已重新执行，未沿用修改前的二进制验收记录。
