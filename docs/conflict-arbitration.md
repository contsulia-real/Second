# 局部冲突仲裁与安全释放

状态：已获实现授权；以下是实施约束，不能当成已交付能力。四节点复现见 `tests/integration/runtime_conflicts.rs` 和 `target/resource-conflict-reproduction.log`。

## 已确认的问题

同一货币的两个转账、同一地址的两个开户、同一支付地址的两个退役请求，在不同节点按相反顺序准备，会各占两个节点。四成员集合需要三个签名，两个任务都无法完成。开始投票后本地取消被拒绝，重启也不会释放。无关资源的任务仍可完成。这是缺少跨节点冲突决策，不是忘记清理本地占用。流通中且有归属的货币不能直接 Destroy，因此不能用这种无效请求证明转账与销毁竞争。

## 决策必须共用任务 BFT

对同一 `ConsensusScope::PreparedTask(TaskId)`，允许两类互斥结果：现有冻结计划的 Commit，以及域分离的 Abort。Abort 摘要绑定协议版本、原始 TaskId、不可变请求摘要和该任务的确切 ValidatorSet。它不是另一个 scope，也不是另一套永久 veto 签票；两类结果共用已有 BFT round、prevote QC、precommit 锁和不可逆的最终签票锁。

任何节点都不能因超时、较小 TaskId、单个对端的通知、NIL QC 或“当前没看到成功证书”释放资源。只有通过同一任务 BFT 得到的合法 Abort 最终证书才能释放。已形成的 Commit 最终证书不可取消；尚有 Commit 锁时只能遵循已有 QC 解锁规则切换候选。

独立 veto 的反例：两个诚实节点已因合法 Commit prevote QC 锁定，第三个诚实节点永久承诺取消，拜占庭节点随后沉默。Commit 与 Abort 都无法取得 quorum。这个实现即使没有双提交，也引入了新的永久死锁，因此禁止采用。

## 证据与选择

冲突依据是经过验证的冻结计划实际资源交集，不是账户相同、金额相同或用户声称冲突。资源包括 CurrencyAddress（Transfer、Destroy、LeakRepair 的实际货币集合）、开户 AccountAddress 和支付地址生命周期 PaymentAddress。一个任务内重复使用同一资源仍归该 TaskId，不能误判为任务间竞争。

同步 source 继续包含原始签名 LegalTask；另只携带无法从签名请求重建的 Transfer 货币选择和 LeakRepair 储备选择。Issue/替换地址由已有已认证连续区间重建，显式 Destroy/LeakRepair 目标和生命周期参数由原请求重建。接收者复用现有准备执行路径，检查数量、唯一性、归属、储备角色、业务规则及完整 plan digest；source 不获得修改签名业务的权利。

无已验证 QC 优先权时，实际冲突集合使用确定性的 TaskId 顺序选出保留任务，其他任务参与 Abort。排序只用于实际冲突，不创建全局高度、普通交易总序或全网复制。存在合法 Commit prevote QC 时，必须先遵循其锁/解锁约束，不能为了本地优先级覆盖它。两个不同候选的资源必须在诚实节点持续占用期间排他，直到互斥的最终结果确定。

活性只在部分同步、必要诚实委员会在线且竞争集合最终停止增长等 BFT 前提下承诺；无限新请求不附带公平性保证。多资源任务的优先规则必须一致，不能逐资源反向选择而形成等待环。

## 原子持久化与恢复

Abort 证书安装必须在一次 store 原子事务内完成：核对确切委员会及请求绑定，拒绝已成功或冲突最终签票结果，写入 TaskBinding 的唯一取消终态，删除该任务活动计划和相关 payment execution 建立记录，关闭待准备分配源。保留 TaskId 请求绑定、已确认连续地址区间和本地不可逆签票证据；地址永不回收。恢复不维护另一套失败结果表。

仅用于参与取消决策的远端计划不能获得本地业务资源占用，也不能获得 Commit 签票资格。快照必须区分“验证过的候选”和“持有排他资源的业务计划”；不能为允许仲裁而关闭现有快照冲突校验。取消证书重放不得增加 generation；取消后的相同请求返回明确终态，换请求仍触发 TaskIdAlreadyBound。

任务原始委员会必须可持久恢复。成员变更不能把当前集合的 Abort 证书用来取消旧集合的任务；历史委员会解析、签名 fence 和成员交接必须纳入验证。缺失上下文时 fail closed，不能从对端未认证的版本提示推断授权。

实施时还有两项不能略过的设计边界：其一，同一 signed TaskId 在不同节点也可能已经冻结了不同的合法货币选择，仲裁需要保留仍可能提交的旧计划占用，不能借候选切换提前释放；其二，多候选场景的最高有效 prevote QC 需要可恢复，单独保存 locked digest/round 不能替代用于合法解锁和重传播的 QC。目前已实现 QC 与依赖签票同次持久化、必要的更高 proof 记忆、快照确切集合验签和启动恢复；allocation 恢复优先把该 proof/lock 对应任务放入既有 64 候选窗口，不扩大窗口或新增循环。冻结 source 改动只解决新拉取时精确重建，不会替换已经 Voting 的本地计划。原始委员会绑定及成员交接规则仍需明确到持久化事务和验收，不能把这里的安全要求误当成已经完成的安全证明。

## 必须通过的验收

已实现冻结冲突源的只读验证：拉取计划因货币、开户或支付生命周期占用被挡住时，运行时在隔离的临时 claim book 和状态副本上复用同一准备执行路径，先验证完整业务与 plan digest，再用现有货币/生命周期占用索引求交集。`VerifiedResourceContention` 只报告排序、去重后的其他 TaskId；同一任务自身占用、同付款账户但不同冻结货币均不作为冲突。临时验证不落盘、不新增绑定或 payment execution、不释放本地占用，也不授予 Commit/Abort 签票资格。它不是取消证据或新共识结果；原始委员会与 QC 优先权仍须由决策层验证。仅在实际占用错误路径做这次验证，不为正常成功准备重复执行。

`src/prepared/conflict_tests.rs` 验证多操作计划同时识别三类占用、合法前缀不能遮住非法后续操作、篡改归属/冻结摘要被拒绝，以及重载后原占用和 generation 不变。四节点 `runtime_conflicts` 回归进一步要求真实 QUIC 拉取获得三类精确冲突诊断，再检查无关任务成功和重启保留占用。当前仍不会自动取消或释放投票中的任务。

1. 四节点相反到达顺序，三类真实冲突收敛到一个业务成功和其他取消；无关任务继续完成。
2. 在仲裁投票、Abort 证书落盘前后重启，最终状态一致，余额/归属/区间正确，证书重放无写入。
3. 已形成 Commit QC/最终证书、旧委员会任务和成员变更交叉情况下，不允许提前释放或双最终结果。
4. 伪造冲突、篡改冻结选择、不同任务摘要、错集合证书和不足 quorum 全部拒绝且不改变持久状态。
5. 同账户但冻结货币不相交的任务并行完成；多资源请求不形成反向等待环。
6. Windows 和原生 Linux 的本地完整门禁通过。没有完成这些验收前，不宣称冲突活性已经修复。

## 尚待落实的成员交接约束

当前实现正在补成员交接：membership source 绑定待决冻结计划摘要，本地候选持久保存正文，并在受理后关闭新计划准入；共享恢复携带已认证资源 fence。正文复用既有 prepared codec，fence 索引仅从正文推导。本地签票边界、证据恢复校验和事务回归已接入；不同节点正文合并与按需传输尚未接入，不能将本地候选当作已完成的跨节点交接，更不能据此宣称 2∶2 自动仲裁已完成。

另一个待完成的安全条件是交接与业务基线的关系：当前根认证冻结计划，已完成业务事实仍在既有 shared state 中；本地 membership proposal 已绑定终态事实、确认分配与业务基线；尚未完成跨节点基线同步，以及新成员在获得正确恢复基线前的签票准入。因此本阶段只能声称所测试的待决 fence 与取消消费行为，不能据此声称所有跨集合支付、TaskId 绑定和成员接入已经获得完整安全证明。涉及较晚本地准备的非签署者如何安装缺席任务的交接、又不取消可能已获 quorum 的任务，也仍须协议与对抗回归明确。

交接正文目前仅接受本地已经完整校验的计划或上一份认证交接所继承的计划；未知对端描述 fail closed，不能直接制造资源占用或提交权。成员最终签票先持久受理候选、关闭新任务准入，再在最终票事务内重新验证覆盖；候选生成不关闭准入，生成至受理期间新出现的计划会使遗漏候选被拒绝。已受理候选和封闭状态可重启恢复，现有任务继续沿原集合完成。

共享恢复载荷携带该交接所必需的一份 membership proof，恢复后写入既有 validator_transition_proofs。恢复检查点绑定 canonical shared state；附带证明独立验签并匹配正文根与目标集合，不把不同合法 quorum 的签名排列纳入业务状态摘要。正文中的 certifier exact set 已由 canonical state 绑定，载荷安装仍必须通过可信当前集合的恢复证书，不能仅凭正文自称的旧集合建立信任。旧任务来源集合只为必要 fence/终态保留，不复制普通本地 prepared 占用。

新增事务回归验证：遗漏活动任务的成员证书不能写入；候选受理后的重启拒绝新增任务且不改变 generation；认证旧任务可交接；恢复节点没有旧 Commit 权，但相同账户资源仍被 fence，独立资源可准备。现有跨版本任务完成与 key rotation 回归同步改为在认证前绑定交接摘要，不能拿旧的无摘要夹具绕过保护。

恢复后的 fence 可凭原 exact 委员会的 Abort 最终证书释放，无须把旧计划恢复成普通资源持有者；当前委员会的取消证书仍拒绝且不落盘。回归同时验证伪造附带 proof、清空正文、错误取消集合、合法取消后冲突资源重新可准备。此能力仅消费最终证书，不赋予恢复节点旧委员会签票资格。

Linux 默认并发再次复现三处共识连接前置超时。节点已有维护循环原先先等待 public bootstrap，再维护 BFT peers；现调整为优先维护共识连接，消除共识首次拨号对公开 bootstrap 完成的依赖，不增加扫描、循环或超时预算。是否足以消除并发超时须以调整后的默认并发门禁确认。

调整后的运行中，所有四节点 BFT 连接均已建立，但 public-checkpoint 测试仍等待与此次 BFT 认证无关的 public 全互联。该前置条件移除，仍在相同五秒预算内要求全部专用 BFT 连接，并保留原认证结果、签票锁与持久化断言；公开连接及发现行为由现有 peer/public-sync 回归覆盖。此调整不改变真实 2∶2 冲突测试的阻塞断言。

该测试末尾仍验证 public 与 BFT 共存：认证完成后复用实际 public 连接，或显式拨通查询需要的那条边，执行真实 ping 并再次断言三个 BFT peers 保持连接；不假定 bootstrap 已建立所有 public 边。

QC 内部处理入口只接受已经对 driver 的 exact set 验证的证明：网络入口先验签再选择候选，driver 本地聚合入口从已验签票构造 QC；公开 driver 接口仍先验签。QC 记忆入口复用这个验证结果，保留 scope/版本/成员/round 检查，避免再遍历 quorum 签名。持久快照恢复仍独立验签。这项去重不允许跳过首次输入或磁盘恢复验证。

34 节点 Windows 验收连续出现全员已准备但 round 不齐、部分节点已锁定同一计划却不能收敛的失败。去重 QC 验签后仍复现，故不能称它是唯一根因。阶段调度现从处理完成时间起计，并随本 scope 的 durable round 增加窗口，减少固定窗口下反复错过 quorum 的风险；不延长测试总期限、不扩大队列、不新增扫描。恢复 proposer 回归同时验证重启 round 的窗口、到期 NIL 行为与伪造 QC 在候选切换前被拒绝。这仍不是资源冲突 Abort 仲裁。

以下是安全推导与拟定约束，尚不是已经复现并修复的跨集合漏洞。单个任务中 Commit/Abort 的 quorum 交集证明，只在同一 exact ValidatorSet 内成立；它不能自动证明两个不同委员会中两个不同 TaskId 对同一货币的互斥。当前 shared recovery 不包含本地 PreparedTask 占用，因此仅给 TaskBinding 添加本地来源版本，仍不能证明新成员继承了旧任务占用。

具体推导：旧集合四成员中两名诚实成员仍持有任务 A，保留旧 scope 的签名资格；新集合引入的成员没有这些 private claims，却看到货币仍归原账户。若允许它们直接参与冲突任务 B，两张跨集合证书之间可能没有必要的诚实成员交集。不得用“各自都是 3/4 quorum”跳过这个问题。

拟定交接应在 membership 变化时认证必要的待决资源上下文，而不让每笔普通交易再次共识：

- 旧委员会签署交接承诺前，原子关闭旧 epoch 的新任务准入；已列入承诺的待决任务仍沿原 exact scope 继续 Commit/Abort。
- 交接承诺覆盖签署者所有仍可能形成结果的任务、冻结资源、来源委员会和已知最终事实；遗漏自己已经签票的任务时，诚实成员不能签署。现有 shared 业务状态与不可变 TaskId 事实仍是唯一权威，不建立另一份业务账本。
- 新成员必须先验证交接承诺，并对涉及待决任务的资源施加 fence；冲突集合允许多个待决描述，但不能当作多个同时合法持有排他资源的普通 PreparedTask。
- fence 只有在对应旧 exact scope 的最终 Commit/Abort 证明安装后才能缩减。成员变化不能用当前集合的取消票替代旧任务的结果，也不能因本地没有旧 private plan 而把资源当成空闲。
- 交接签票与封闭准入必须持久、可重启，和已有 frontier seal、shared recovery 及签名 fence 的关系需通过事务级回归证明；不能把一个未认证的本地资源列表当作交接证明。

交集论证的关键是：旧任务的任何合法 quorum 与旧委员会的交接 quorum 必相交至少一个诚实成员；该成员要么已将任务及其已知结果纳入承诺，要么已经持久关闭了未列入任务的准入，因而不能帮助形成遗漏任务的结果。这个论证仍依赖正确的承诺验证、原子封闭和旧任务终态安装，必须完成实现和对抗回归后才能作为交付保证。


## 已实现的取消终态与认证决策恢复（2026-10-04）

已实现 TaskBinding 的单一 Cancelled 终态、同 PreparedTask scope 的 Abort 最终签票和 StateStore 原子证书安装。取消摘要绑定 exact 委员会全部凭证、请求摘要及 TaskId；错请求、错 TaskId、错集合、伪造/不足 quorum、已投另一种最终票、Finalized Commit 或已成功状态都拒绝。安装取消保留请求绑定、连续地址区间及不可逆最终票，只清除活动计划、payment execution、分配待办与本 scope BFT 状态。证书重放无 generation 写入；同请求重提明确拒绝，状态查询返回 cancelled，错误请求摘要仍 unknown。示例重试脚本遇到 cancelled 立即停止。

终态持久保存确定性的 statement，避免把不同 quorum 投票排列写成不同共享结果；取消后的历史委员会按版本复用现有 retained 集合，shared recovery 只输出这些终态必须引用的集合。恢复节点不继承本地资源占用，但可保持取消绑定并验证旧委员会证书重放。这仍不等于待决资源交接。

运行时恢复已有 Abort QC、finality-ready 与最终票；已投最终票的重启会重发该结果。持有本地活动计划的接收者可按验证通过的原 exact 集合 QC 或最终证书接入 Abort，与 Commit 共用同一 BFT session。裸提议、单票和伪造 QC 不能准入该候选。`CertifiedPreparedTask` 事件现在可表示已 durable 的 Commit 或 Abort，接入方须查看摘要或请求状态。

`src/prepared/abort_tests.rs` 覆盖三类资源释放、烧掉的地址不回收、请求替换拒绝、取消签票后的重启、证书重放及跨版本共享恢复，并检查提交最终票/Finalized/成功与坏证书的释放边界。`src/runtime_bft_consensus/task_decision_tests.rs` 用四个独立持久库验证取消最终票恢复、QC 驱动跟随和仅最终证书驱动第四节点释放。该测试显式提供先前已认证 QC，不冒充首次冲突候选准入或真实 QUIC 2∶2 仲裁验收。

尚缺：完整冲突 witness 准入与确定性优先、未持有计划节点的 durable 原委员会上下文和 Abort-only 权限、释放后的事件驱动任务重试、同 TaskId 不同合法冻结计划处理，以及成员待决资源交接。原来的真实 QUIC 2∶2 复现仍应保持阻塞，不能改断言伪称已修复。

恢复决策优先级进一步明确：已观察到的 precommit QC readiness 或已投不可逆最终票优先于较早的另一结果 prevote 锁；两种方向均由运行时恢复回归覆盖。恢复只按本地 validator + scope 索引读取 metadata，不逐任务扫描整个签票库。批量快照校验同版本取消绑定时复用临时 SHA-256 委员会前缀，每个委员会只处理一次成员凭证，不增加持久摘要真值。

任务同步层识别本地活动计划对应的 exact Abort 摘要，以及已持久取消的原委员会 statement；匹配时不再尝试拉取不存在的 Abort 业务源。识别只影响同步，不授予候选准入或签票资格，裸 Abort 提议仍不能投票。Commit/Abort 完成原子安装和必要业务执行后，通过一次运行时通知清除该任务的源拉取与公告重试；不扫描全部任务、不增加后台循环，也不影响最终证书的既有传播。`src/runtime_tasks/tests.rs` 覆盖裸提议不投票、不落盘、不拉取，最终证书驱动终态与重试清理，重启后拒绝错误 TaskId、集合版本和摘要的匹配。

本阶段 Windows 完整门禁曾通过。Linux 默认 20 测试线程的完整运行两次在三个 BFT 测试的五秒建连前置阶段超时，尚未进入业务共识；同组七个测试单独运行全部通过，四测试线程完整运行及 34 节点验收也通过。当前证据指向并发测试时的建连问题，但未定位根因，不能把限制测试并发称为修复，也不能把旧门禁结果当作后续修改的验证。

## 交接业务基线绑定（实施中）

交接根补充认证已完成的业务状态、终态 TaskId 和已确认的连续分配绑定。基线从唯一 SecondState 派生，复用正式状态编码；排除已有 frozen plan 覆盖的本地未分配 Pending 绑定、payment prerequisite 和上一份 fence 正文，不保存第二份余额或归属。首次候选受理校验当前基线，已持久受理的同一候选允许其已覆盖任务完成后继续认证；终态计划只可从该持久候选取回上下文，不接纳未知对端计划。存在已确认分配或终态事实时，即使没有活动计划，也必须绑定交接根。尚未完成网络基线同步和新成员签票准入，不将本项单独当作完整跨集合安全交付。

真实 QUIC 恢复回归还发现 provider 曾用 from_shared_parts 构造传输载荷，会遗漏交接的附带 membership proof；有业务基线而无活动计划的成员变化使该问题暴露。provider 的首次发布和节点重启恢复均统一改用 from_persisted，保留已验证状态需要的一份历史证明。回归保留认证恢复、错成员拒绝和 state/membership 变化后旧 provider 失效的全部断言。

交接中的未决 TaskId 现在固定原委员会版本：新计划准备和隔离冲突验证复用同一 origin 检查，不能借相同请求摘要把旧任务重新准备到当前集合；终态查询/重提仍走原来 succeeded/cancelled 规则。精确上下文查找在交接 BTreeMap 上按 TaskId 范围定位，取消安装和运行时消费也复用该入口。共享恢复回归验证跨 epoch 重新准备被拒绝且无 generation 变化。

业务基线交接会在活动计划清空后继续保留。为避免每次本地 BFT 写入重复验证旧 quorum，认证正文保留临时证明指纹缓存；使用前重新计算正文根与证明字节指纹，检查目标集合，匹配才复用验签结果。缓存不持久化、不参与交接根或共享真值；正文/凭证/签名变化不能沿用缓存。回归从已经校验过的 snapshot 克隆缓存，再替换证明签名，要求独立拒绝。

BFT/public 共存探测改用独立 public 客户端身份，避免同时 bootstrap 替换 peer-manager 方向连接时关闭正在探测的句柄；仍真实 ping BFT 节点，并同时要求两端三个 BFT peers 保持连接。修正后 Windows 与 Linux 默认并行完整门禁均通过，失败现场仍保留；后续源码变更另行执行门禁。

交接业务基线进一步排除已确认分配的本地 allocation_task 待准备源。该字段会在准备后清除，不能决定已确认范围和请求的共享含义；持久状态及恢复载荷仍保留实际需要的源，不改分配证书验证。回归使用同一分配的两个不同合法 quorum 子集，在其中一个节点清理待准备源后，要求两份无活动计划的交接根相同且覆盖验证成功；请求绑定发生变化仍拒绝。此处复用正式 canonical 状态编码，未新增格式或第二份分配真值。
## 冻结源拉取的乱序重试修复

Linux 默认并行门禁复现了正常私有拉取中的 InvalidPreparedTaskSource。检查发现同一版本/摘要的更高 round hint 会丢掉正在拉取的正文；迟到响应、重复分块和旧 unavailable 响应可能被错误归到当前拉取。现保持同一目标的已收字节、来源和 deadline，只推进公告 round；无对应活动请求、旧版本/摘要或旧来源的分块直接丢弃，不授权任何计划。当前来源的重复分块必须与已收字节完全一致才可忽略；篡改内容、越界和未来 offset 继续拒绝。unavailable 也必须匹配当前来源、版本和摘要，才可切换来源。忽略旧响应不刷新 deadline、不额外发请求、不增加缓存/循环。

新增回归在真实 runtime 对象中重排高 round 公告、旧摘要 unavailable、重复/篡改/未来 offset 分块及完成后的迟到分块，验证原拉取继续完成、没有 durable 写入。原静默来源切换回归移至同职责 tests 模块。Windows 最终基线批次失败 2 项（连接超时、CLI 重试耗尽），Linux 失败 1 项（上述拉取拒绝），均保留原日志；该拉取修复不冒充 Windows 连接超时根因。

本轮最终验证：Windows/Linux 默认并行 fmt/check/clippy/all-targets 与仓库 diff-check 通过，两端 40 常规单元、239 主集成及 34 节点目标通过。日志 target/handoff-fetch-final-windows.log、target/handoff-fetch-final-linux.log。原失败日志保留；早前 Windows 间歇连接超时的根因尚未确定。真实 2∶2 回归仍验证阻塞与资源保护，不构成自动取消收敛验收。

## 2026-10-04：首次冲突受理正在接入

复用已有 PreparedTask 持久上下文，加入本地 Commit 权限与冲突取消选择；没有资源占用的已验证竞争计划不创建第二份 claim，不可签 Commit、不可普通本地取消。首次受理必须使用本地当前 exact 委员会，并通过相同业务准备器验证完整签名源、冻结选择与真实资源交集；只对相交任务按 TaskId 选择取消候选。已有 QC、precommit readiness 和不可逆最终签票继续优先，资源只能在取消最终证书原子落盘后释放。

取消提议的摘要绑定请求及 exact 委员会。未知上下文可按需拉取其签名源，完整验证业务与本地交集后才建立 Abort-only 上下文；单独裸提议仍不授权取消签票。未持有 Commit 权限的竞争计划上限为 32，持久化和恢复均检查；没有全局交易排序、新后台扫描或平行请求数据库。证书安装后的相交竞争计划通过一次事件重试，启动恢复有一次补偿检查；相同受理重放不增加持久 generation。

当前受理/权限隔离单元回归通过，首次四节点 QUIC 验收尚未通过：源拉取与仲裁已触发，观察到取消 prevote QC，但 20 秒内未达到全部终态。继续检验迟加入节点的轮次推进和证书持久化，不以门禁替代收敛结果。同 TaskId 不同合法冻结计划、跨节点交接正文合并和新成员基线同步仍未交付。新源格式保持未发布开发版本 1，不保留旧内部快照兼容层。

## 首次四节点自动仲裁验收结果

本轮普通启动实测通过（target/contention-fourth-quic.log，36.84 秒）：Transfer、RegisterAccount、RetirePaymentAddress 三组相反顺序 2∶2 占用均达到低 TaskId 成功、高 TaskId 认证取消，独立请求成功。此前 20 秒窗口中的取消 prevote QC 不够证明最终结果，失败日志继续保留；按迟受理节点和释放后 Commit 各自的完整 proposer 轮转设置 90 秒验收上界，并未降低终态断言。

中途重启实测通过（target/contention-restart-second.log，31.72 秒）：只运行两个 Validator，等完整冲突源受理与 Abort-only BFT metadata 持久后停止；不足三票 quorum 时原资源仍占用、无取消终态。重建四个运行时后自动恢复，三组均达到相同成功/取消结果，落盘重开后检查余额、货币归属、账户与地址 frontier、零活动货币占用以及只读加载 generation 不变。夹具使用 500ms 三阶段窗口；先前默认 2s 窗口的普通四节点结果保留。单元回归同时验证受理重放不写盘、重启后竞争上下文不能签 Commit，以及新过期请求拒绝、有效期内已受理的确切计划在认证释放后可继续获得资源。

这些结果覆盖当前集合、不同 TaskId、相同冻结币选择的真实 2∶2。不代表同 TaskId 不同合法冻结变体、已有 Commit QC 与另一低优先级资源持有者的活性、跨节点成员交接正文合并/传输或新成员业务基线同步已完成。最新 Windows/Linux 完整门禁结果随后记录，尚未重建 release 包或重跑 SCM/systemd。

Windows 最新源码完整门禁通过：fmt/check/clippy(-D warnings)/all-targets，40 常规单元、239 主集成及独立 34 节点发现目标通过；主集成 54.93 秒、34 节点目标 30.04 秒。日志 target/contention-gates-windows.log，diff-check 通过。Linux 完整门禁的初次运行 238/239：新仲裁及中途重启回归通过，但既有 CLI lost-ack/node-switch 回归等待业务终态超时（30 秒），日志 target/contention-gates-linux-failed.log 保留。原输出没有标识具体等待的 TaskId，现只补失败时的任务、节点、peer、持久 BFT 状态及事件诊断，不延长该窗口、删断言或降低并发；正在重新执行完整门禁，不能将新增诊断称为超时根因修复。

最新原生 Linux 全量门禁通过：fmt/check/clippy(-D warnings)/all-targets，40 常规单元、239 主集成和独立 34 节点发现目标通过（主集成 91.19 秒、34 节点 46.70 秒），日志 target/contention-gates-linux-native.log。此前补诊断的一轮临时目录因 shell 提前展开落到 /mnt/c Windows 挂载盘，产生额外持久化超时，已停止并保留 contention-gates-linux-windows-fs-invalid.log；该轮不能计作原生 Linux 验收。现在使用明确 Linux 绝对临时路径，失败现场不随 /tmp 生命周期清理。首轮既有 CLI 超时仍未确定根因，最新通过不等于根治。诊断补充后 Windows fmt/check/clippy 已再次通过（contention-diagnostic-windows.log），最后一次 all-targets 复核结果另记；未改变等待窗口或终态断言。

最终复核：诊断补充后的 Windows all-targets 也全部通过（contention-final-tests-windows.log，40 常规单元、239 主集成、34 节点目标；主集成 60.42 秒，34 节点 32.87 秒）。结合 contention-diagnostic-windows.log 的 fmt/check/clippy 和 contention-gates-linux-native.log 的完整门禁，当前源码两端验证通过，最终 diff-check 通过。未完成协议范围和首轮 CLI 未定位超时仍保留上述边界；没有用此次通过删除失败记录。

## 2026-10-04 至 2026-10-05：已有 Commit QC 的冲突恢复

已验证的 Commit prevote QC 优先于局部 TaskId 选择，最终签票、readiness 和最终证书继续优先。观察者复用既有 BftLocalState 保存 QC；最终 Commit 证书经 exact 原委员会验证后，在同一 readiness 中保护不可逆 Commit 选择，不伪造 prevote QC、不写本节点最终签票、不创建平行证书账本。相交任务改为取消候选，无资源观察者暂停旧取消会话；只有认证取消原子释放资源后，原冻结计划通过现有准备器取得资源，才恢复 Commit 签票。正常无冲突任务仍走原 BFT 驱动，避免重复持久化。

本地提交遇到真实资源占用时，复用同一业务准备器独立验证完整请求和冻结选择，再持久受理无 Commit 权限的上下文，返回 AlreadyPending。有效请求因此在业务状态改变前留下可恢复的验证结果。新过期请求、非法业务和无实际交集仍拒绝；无资源上下文仍受 32 个上限约束，不创建第二份资源索引或扫描循环。

错过早期 QC 的节点可以使用最终 Commit 证书重试原冻结计划。资源仍被占用时不授予权限；已验证最终选择在重启后由 readiness 保护。释放事件和一次启动恢复沿用现有重试路径，待处理证书正文沿用既有有界收件缓存；原已完成会话可以再次响应最终证书。终态任务和 Abort QC 交回原处理路径，不恢复已删除业务源。task_succeeded 的 Some(false) 表示尚未成功，只有 Some(true) 或认证取消才属于终态快捷路径。

四节点 QUIC 回归覆盖 Transfer、RegisterAccount、RetirePaymentAddress：三个节点持有较高 TaskId 的 Commit QC，先达到成功及竞争请求认证取消，再启动仍占用资源的第四节点，确定性覆盖错过全部早期 QC、只收到最终证书的恢复。原 2∶2 中途重启回归同时保留。权限测试验证实际占用中无法取得资源；观察 QC 或最终证书后仍不能签 Commit，包括最终签票；重复观察不增加持久 generation；Pending 请求的网络收件确实记录 QC；认证取消后的迟到 Commit QC 不恢复正文拉取。

最终源码本地门禁通过。Windows 日志 target/commit-qc-reused-final-windows.log：fmt/check/clippy(-D warnings)/all-targets，40 常规单元、240 主集成、独立节点发现目标通过；主集成 72.00 秒、节点目标 38.18 秒。Linux 最新 fmt/check/clippy 见 target/commit-qc-reused-final-linux-failed.log 的前置成功记录，默认并发 all-targets 复核见 target/commit-qc-reused-final-linux-recheck.log：40 常规单元、240 主集成、独立节点目标通过，主集成 53.34 秒、节点目标 44.86 秒。使用原生 Linux checkout 和明确 Linux 临时目录，没有延长测试窗口、降低并发或删除断言。

失败记录保留：第一轮胜者提交后才拉取未知旧请求，业务验证拒绝；第二轮测试在选择取消后读取当前摘要，误把 Abort QC 当作 Commit QC；后续收件测试复现终态清理后的迟到 QC，以及本轮错误的 Some(false) 终态判断，均有针对回归。Linux 最新批次曾为 237/240：静默 peer deadline、CLI 请求超时、单节点提交窗口超时；三项单独复现通过，日志 target/commit-qc-linux-timeout-reproduce.log，完整复核也通过，但这些间歇超时的根因仍未定位，不能称为已根治。此前 CLI lost-ack 超时同样保持未定位结论。

未完成范围：业务状态改变前从未受理的迟到未知竞争源，不能绕过当前业务验证；同 TaskId 不同冻结变体、跨成员交接正文合并和新成员基线同步仍未交付。未重建 release 部署包或重新验收 SCM/systemd。内部格式继续使用未发布开发版本 1，没有旧格式兼容层。

## 2026-10-05：同一请求的冻结候选计划（实现中）

四节点回归已复现：同一签名请求因本地暂时资源占用而冻结不同货币选择，各获两票，无法形成 quorum。当前实现将候选操作保留在同一个 PreparedTask 中，共享签名来源和 TaskId 共识范围；原候选的资源权利在选择新候选后继续保留，恢复时重建候选资源并集。来源拉取按期望计划摘要返回对应候选，最终证书只执行其绑定的一个计划。

没有 QC 或最终投票时，已验证且持有资源的候选按摘要选择；已有 QC、锁和最终决议优先。候选数量有上限，复用现有来源编码和本地快照编码，开发格式版本保持 1。本节记录正在实现的行为，不代表验收通过；阻塞候选、跨成员集合交接和晚到请求仍需完成验证。

候选快照恢复已用既有来源回归验证：对旧计划的 prevote 持久化后加入新计划，重新加载仍占用两种货币选择，最终只执行证书指定计划。handoff 捕获现在将候选展开为按 TaskId 与计划摘要索引的规范条目，消除本地选择、到达顺序和投票阶段对交接摘要的影响；覆盖校验要求包含全部本地候选。远端候选并集交换与新成员业务基线同步仍未完成，这一步只补齐本地规范化与保留义务。

本轮验证现场：最初 Windows 全门禁通过，241 主集成通过、1 忽略；随后增强用例误将只有一个节点见过 blocker 的场景作全共享状态相等断言，已限制为相同基线的 2∶2 用例。新的换轮首选逻辑在已完成任务的旧会话上读取不存在的 PreparedTask，导致 StalePreparedTasks，已改为复用本轮快照中的存在性检查。候选注册不再重复注册默认摘要，也不再额外克隆整张 PreparedTask 映射。

失败日志 frozen-variants-final-gates-windows.log（237 主集成通过、4 失败）与 frozen-variants-final-gates-linux-failed.log（239 通过、2 失败）保留。修正后的 Windows frozen-variants-corrected-gates-windows.log 为 240 通过、1 业务 CLI 重试预算耗尽、1 忽略；该 CLI 单独运行 frozen-variants-cli-reproduce.log 通过，14.71 秒。不能据此宣称超时根因修复。最终源码顺序门禁另记。

当前替代候选只允许原子取得完整资源并集。快照对无权限主计划附带替代候选、无权限替代候选及超出候选上限的组合拒绝恢复；尚未实现的阻塞替代候选路径不能借快照取得签票权利。既有回归同时验证非法权限替换被拒绝、generation 不变。该安全边界补丁需在顺序门禁之后另行复核。

用户尚未确定外部业务流程；先推进核心节点协议，现有账户、支付与货币流转仅作为仓库验收场景，不等同外部业务接入。独立主机仍无可用环境。

顺序门禁 frozen-variants-verified-windows.log 与 frozen-variants-verified-linux.log 两端全部通过：40 常规单元、241 主集成，以及 34 节点发现/业务目标；Windows 主集成 71.89 秒、节点目标 49.88 秒，Linux 主集成 62.28 秒、节点目标 46.93 秒。这份源码尚未包含最后的快照权限组合拒绝补丁。

权限补丁后的 Linux frozen-variants-permissions-linux.log 完整 fmt/check/clippy(-D warnings)/all-targets 通过：40 常规单元、241 主集成、34 节点目标；主集成 52.09 秒、节点目标 52.26 秒。Windows fmt/check/clippy 通过，frozen-variants-permissions-windows.log 为 240 主集成通过、1 CLI reentry 失败、1 忽略，因此最新 Windows 全量门禁仍未通过。

该 reentry 失败现场进一步缩小范围：三个活动节点的 currency frontier 都停在 4，业务 TaskId 9201 仍 Pending、没有 PreparedTask；旧健康成员 2、3 在集合 3 的 CurrencyAllocation(start=4) 上同锁 round 4 摘要，当前 round 6，恢复成员 1 在 round 7 尚无锁。即停滞发生在业务准备之前的局部分配共识，不能直接归因于冻结候选补丁。现场目录 C:\Users\Why23\AppData\Local\Temp\second-cli-network-init-8620-1791132523822694900-19.network 保留。根因和稳定修复仍需继续。

权限补丁后的 Windows 34 节点目标独立复核通过，日志 frozen-variants-permissions-windows-nodes.log，38.12 秒。此结果不抵消 Windows 主集成中保留的 CLI reentry 失败；最新源码的整体 Windows 门禁仍未通过。

## 2026-10-05：运行时遗漏迟到 prevote QC

对上一轮 CLI reentry 失败的检查发现一条可确定复现的接收缺陷：BftDriver 能保存迟到的已验证 prevote QC，但运行时先按旧轮消息将其丢弃。回归使接收者先前进到 round 3、无锁，再接收 round 0 的有效 Digest prevote QC；修复前持久 valid_prevote_qc 为 None，日志 target/late-prevote-qc-reproduction.log。

运行时现在仅对已知验证候选的 Digest prevote QC，在原委员会验签后复用既有最高 QC 持久化接口和会话缓存。迟到证明不回退轮次、不改变锁、不在当前轮补签、不刷新 deadline，也不触发额外广播；重复证明不写盘。回归验证旧证明可随重启恢复，并由后续 proposer 携带解锁证明，伪造旧 QC 不改变 generation。原重启/QC 回归同时保留，target/late-prevote-qc-fixed.log 通过。这证明了接线缺陷及修复，不等同全部 CLI 超时根因已经确认；真实 CLI 与完整门禁需要继续验证。

## 2026-10-05：受阻的冻结替代候选（实现中）

新增四节点回归将较高 TaskId 的竞争任务保留在另一种货币选择上；修复前两任务的冻结来源都无法互相取得资源，30 秒仍未形成终态，target/frozen-variants-blocked-reproduction.log 保留。候选受理现在复用隔离准备器完成一次业务验证，再检查现有资源索引；有占用时返回已有 VerifiedContention，由原仲裁入口将候选操作保存为无资源权利的替代候选，不覆盖持有资源的主计划。

认证 Abort 的相关事件重试和启动重试现在覆盖这种替代候选；释放后重新取得资源仍复用原完整准备与快照原子保存，普通持有候选的旧资源不提前释放。快照恢复允许主计划持有资源、替代候选无资源的组合，签票和最终执行继续检查对应摘要自己的权限；无权限主计划附带替代候选仍拒绝，此路径和不同候选 QC 的保护尚需补齐。当前本节记录实现进度，不能声明所有冻结候选活性已经完成。

受阻候选初步回归通过：target/frozen-variants-blocked-event-fix.log 的真实四节点用例先认证取消较高 TaskId 的持有者，再提交较低 TaskId 的共同冻结候选，2.82 秒完成；target/frozen-variants-blocked-permissions.log 验证替代候选没有 Commit/finality/prevote 签票权利，拒绝签票不写盘，重启保留旧占用，认证释放后才取得候选资源。此前测试将持有者的 Abort 证书事件误当目标任务证书，事件断言已校正为分别验证两种决定。

迟到 QC 接线修复的两端完整门禁已通过：Windows target/late-prevote-qc-gates-windows.log 为 41 常规单元、241 主集成、34 节点目标，主集成 70.14 秒、节点目标 38.81 秒；Linux target/late-prevote-qc-gates-linux.log 同样通过，主集成 44.23 秒、节点目标 48.23 秒。这批不包含随后加入的受阻候选路径，后者完整门禁需要另记。不能用一次门禁通过代替真实 reentry 间歇根因的完整确认。

下一步仍需覆盖：Commit QC/最终证明指向无资源替代候选时的保护和晋升、不同任务在旧保留候选与实际认证选择上资源重叠但决议互不冲突时的恢复、无普通持有计划的多候选上下文。当前实现没有把这些情形当成已完成。

受阻候选批次的完整门禁：Windows target/blocked-frozen-variants-final-windows.log 全部通过，42 常规单元、242 主集成、34 节点目标，主集成 68.09 秒、节点目标 34.66 秒。Linux target/blocked-frozen-variants-gates-linux.log 的 fmt/check/clippy 通过，主集成 240 通过、2 失败：CLI 在节点重启后发 recovery-checkpoint 时连接 QUIC 超时；正常单来源冻结拉取已准备计划但 20 秒未取得全部证书。两份失败现场均保留，尚不能声明 Linux 最新全门禁通过。

既有私有验收探针 examples/mixed_snapshot_probe.rs 增加可选 --bft <task-id>，通过现有公开接口输出指定任务与当前分配 scope 的轮次、锁和最高 prevote QC；不导出业务 payload。用于读取失败快照并区分源拉取、轮次推进和证明接收问题，默认共享 payload 模式保持原用途。

上述 Linux 单来源失败的持久现场显示节点 1、2 停在 round 1、无锁和最高 QC，节点 3、4 尚无该任务的本地 BFT 状态；这不足以证明高轮次失步是原因。现有冻结来源验收循环此前丢弃所有非证书事件，导致超时诊断失去拒绝和发送失败证据；现在每节点保留最近 16 条非证书事件，失败时同时报告现场路径、工作任务是否退出和持久轮次/锁/QC。仅改善失败定位，不扩大超时、不改变共识行为，也不将后续单独通过当成原失败已修复。

诊断补丁的 Windows 三个冻结来源用例通过（target/frozen-source-diagnostic-windows.log），WSL 单来源复测通过（target/frozen-source-diagnostic-linux.log）；两端 all-targets clippy(-D warnings) 通过。这次没有复现原间歇停滞，根因仍未确认。

## 2026-10-05：无资源替代候选的 Commit QC 保护

新增回归让较低 TaskId 持有替代候选的货币，本地节点已对目标旧候选 prevote，另外三个验证者对目标替代候选提供有效 QC。旧 reconcile 只检查主摘要，返回空结果，错误保留目标的 Abort 选择（target/frozen-alternative-qc-reproduction.log）。现在按 QC 摘要解析确切候选，验签后在既有 BFT 状态中原子保存证明并更新仲裁选择；资源冲突仅按认证候选的操作计算，不把其所有本地替代操作都当成认证选择。仍保留所有既有资源占用，不为无资源候选授予签票权利。

运行时接收和等待来源的 QC 重放入口覆盖无资源替代候选，并暂停该任务的旧会话；启动检查阻止在认证选择尚未取得资源时重新启动旧候选。释放后的既有事件重试继续负责完整重新准备和恢复会话，不引入轮询。source_tests 回归验证 QC 可覆盖 TaskId 优先级、重复证明不写盘、无权候选仍不能签 Commit/prevote、重启保留旧资源及认证 Abort 后才重新取得资源。最终证书直接到达、实际认证选择与其他任务的旧保留资源交叉、无持有主计划的多候选路径仍未完成；本节仅声明 QC 保护路径。

QC 补丁的 Windows fmt/check/clippy 和 10 个 prepared 领域单元回归通过。全量主集成为 241 通过、1 失败、1 忽略（target/frozen-alternative-qc-gates-windows.log），失败是受阻四节点冻结用例。保留现场中节点 1 的目标任务已 Some(true)，节点 4 仍 Some(false)、round 10、无锁/最高 QC；节点 1/2/3 已移除该任务的 PreparedTask，诊断保留它们对节点 4 请求返回 InvalidPreparedTaskSource 的历史。现有私有探针的 --bft 模式也打印指定任务终态以区分已提交与缺失。至少可确认存在三个节点先提交、第四个未追上的现场，不能将其描述为所有节点都未共识；迟到者缺少认证冻结来源的恢复仍未解决。探针不记录所缺候选的到达历史，因此不能单凭这一现场断言所有原间歇故障都由此引起。

格式化后的 Linux fmt/check/clippy 通过，主集成 241 通过、1 失败（target/frozen-alternative-qc-formatted-linux.log，66.17 秒）；三个冻结来源用例均通过，但业务 CLI 丢确认/切节点用例在 TaskId 8001 等待全节点成功失败。节点 1/3/4 已 Some(true) 并取得同摘要证书，节点 2 仍未绑定请求、无本地 BFT 状态。现场保留；此处同样确认存在 quorum 成功后单个节点未恢复，不能归为交易没有提交。两端最新全量门禁均未通过，主集成失败后 cargo 未继续执行后续目标，不声称本批 34 节点目标已经复核。

## 2026-10-05：未知认证候选拉取与在线完成来源

接收路径修复两处遗漏：已有其他候选的任务收到未知摘要的 prevote QC 时不再提前跳过来源获取；precommit QC 和最终证书也在原委员会验签后触发相同的有界拉取。已成功/取消的任务以及已知对应来源不另行拉取。未知摘要尚未取得业务/冻结源验证和资源权利时，证明本身不授予签票或执行权限。逻辑归入已有 contention 子模块，不继续扩大 runtime_tasks 主文件。

完成会话在删除 PreparedTask 前，编码确切认证 Commit 候选的唯一冻结源，随后随现有 recent_completed 缓存保留；原委员会版本、scope、摘要必须同时匹配才可提供。来源按需走已有分块接口，不额外广播、不保留未选候选、不保留资源权利。缓存沿用现有 64 个完成 scope 的淘汰限制，来源共享 Arc，发送仅复制当前分块；单来源仍受原约 2 MiB 编码上限约束。它是恢复数据，不能作为第二份业务状态或投票依据。

target/completed-source-cache-tests.log 的 3 个任务接收单元回归通过，包含原 Abort proposal 与过期 allocation source 安全回归及新增未知证明用例；后者验证伪造 QC 不触发拉取、两阶段有效 QC/最终证书触发拉取但不写盘或授予未知摘要权利，以及提交后移除资源上下文仍可按确切版本/摘要取得来源。clippy(-D warnings) 通过（target/completed-source-cache-clippy.log）。这是在线缓存路径；重启后的来源持久保存、淘汰后的认证基线恢复、最终证书指向无资源替代候选的晋升仍需实现，不能把在线缓存当作完整迟到节点恢复。

本批 Windows 和 WSL Linux 完整 fmt/check/clippy(-D warnings)/all-targets 通过，日志分别为 target/completed-source-cache-gates-windows.log（check/clippy 另见本节独立日志）及 target/completed-source-cache-gates-linux.log。两端均为 44 常规单元、242 主集成及 34 节点验收目标通过；Windows 主集成 81.99 秒、节点目标 34.63 秒，Linux 主集成 45.03 秒。git diff --check 通过。该证据覆盖本批已有在线来源与接收用例，不覆盖缓存跨重启/淘汰。发行任务恢复还需要其独立地址区间证明：当前 mark_task_succeeded 会清除 allocation_certificate，而新在线缓存只保留任务冻结源，不能宣称遗漏局部分配的发行节点已可由此恢复；下一步恢复数据必须包含真正必要的原分配证明并复用既有分配验证器。

## 2026-10-05：原子持久化的本地任务恢复记录

上一节独立的在线来源副本已删除。认证 Commit 在同一次业务快照写入中，将确切 finalized 计划及原地址分配证明保存到本地 task_receipts；记录不保存未选候选，commit_authorized=false，不进入资源索引、签票入口、共享业务状态或交接业务摘要。它提供恢复所需的认证冻结操作，不维护第二份业务真值。计划/证明编码复用原 prepared 与 finality 编码器，内部快照格式仍为未发布版本 1，不增加旧格式读取器。

恢复记录同时受 64 项和 128 MiB 编码总量上限约束，按实际提交 generation 淘汰最旧项而非按 TaskId。记录和编码字节不可变，用 Arc 共享；来源按需派生并缓存，既有证明验签缓存绑定 Commit/分配的确切委员会，后续元数据写入不重复哈希整份请求或重复验签。只保留记录确实引用的旧委员会，原历史身份验证继续检查其权限；共享 recovery payload 仍只输出业务实际需要的委员会，不传播这些本地记录。

节点启动从持久记录恢复原有的限速终态证书回应。私有拉取仅在版本、scope、摘要匹配时提供来源；首块同时补 Commit 证书与必要的 allocation 证书，分配来源也可从同一记录中取得。发送端直接读取共享快照，并复用已有分块/边界校验；不再为每个已提交来源请求克隆整本 prepared book 或重新编码整份来源。

target/task-receipts-cold-runtime-tests2.log 验证新建 store 从磁盘冷读、发行记录保留原分配证明、创建新运行时后启动恢复终态证书回应，以及共享 payload 不因本地记录改变。target/task-receipts-prepared-tests.log 的 10 个 prepared 回归通过，现有 crash-boundary 用例增加 finalized 但尚未执行时没有完成记录、恢复执行后记录与业务成功同时出现且无资源占用的断言。target/task-receipts-retention-tests.log 验证 65 次真实认证提交按 generation 保留最新 64 项、被淘汰任务终态不丢失、伪造恢复证明拒绝且不写盘、声明超大正文在读取/分配前拒绝；clippy(-D warnings) 通过（target/task-receipts-clippy3.log）。完整两端门禁仍需按本批另记。

这补上了恢复来源跨重启的保存与提供，不等于全部迟到恢复已经完成：记录淘汰后的认证基线获取、未受理竞争请求在业务状态改变后的处理、历史/过期请求及分配证明的接收安装、无资源替代候选的最终证书晋升、跨成员交接正文并集与新成员基线同步仍需实现和端到端验收。

提交时的证明缓存只从已经完成快照验证的 finalized 计划和原分配证明初始化，并绑定其确切委员会，避免同一次提交重复验签。磁盘解码不初始化该缓存，冷恢复仍独立验证请求、分配关系和两种证明。历史委员会回归现在要求保留已完成恢复记录所引用的原委员会；任务移出 prepared book 不再意味着该委员会已无引用。

首轮本批 Windows 全量测试（target/task-receipts-gates-windows.log）主集成 238 通过、4 失败、1 忽略：两项是上述旧委员会保留预期，已分别通过修正后的定向回归；另两项是受阻冻结候选与资源冲突的并发超时，失败现场和日志保留。冻结候选的四节点均未取得目标 QC，资源冲突现场则已保留高轮 prevote QC 但尚未完成提交；这些证据不足以确认统一根因，不能把定向通过当作完整门禁通过，也不扩大验收超时。

## 2026-10-05：最终证书直接保护无资源替代候选

新增回归不先安装本地 prevote QC，直接提供原委员会的最终 Commit 证书。旧实现因主计划已经持有资源返回空结果，未保护实际认证的替代候选（target/frozen-alternative-finality-reproduction.log）。持久层现在按证书摘要解析确切候选并验签；运行时使用该候选的冻结来源进行完整重新准备，不再按主计划的资源权限跳过。证书只保护认证选择，仍不授予受阻候选资源或签票权限；原占用必须经认证终态释放，随后既有准备入口重新验证并取得资源。不同任务认证选择与旧保留资源交叉时的等待和释放、无持有主计划的多候选上下文仍需完成。

本地恢复记录优化后的 Windows 全门禁通过（target/task-receipts-refined-windows.log）：45 常规单元、242 主集成及节点验收目标通过，主集成 91.91 秒、节点目标 37.88 秒；fmt/check/clippy(-D warnings)/diff 检查通过。随后 WSL Linux 的 fmt/check/clippy 通过，但主集成 239 通过、3 失败（target/task-receipts-refined-linux.log，88.96 秒）：单节点提交未在期限内落盘、认证但不回应的 peer 切换超时、丢确认/切节点业务 CLI 超时。原冻结来源和两项资源冲突用例这次通过。失败后后续节点目标未执行，不能宣称 Linux 完整门禁通过。这批门禁不包含随后新增的最终证书替代候选修复；后者 Windows 11 个 prepared 回归及 clippy 已通过（target/frozen-alternative-finality-fixed.log、target/frozen-alternative-finality-clippy.log）。

最终证书修复批次的完整门禁现已通过：Windows target/frozen-alternative-finality-gates-windows.log 与 WSL Linux target/frozen-alternative-finality-gates-linux.log 均通过 fmt/check/clippy(-D warnings)/all-targets，46 常规单元、242 主集成和真实 34 节点验收通过。Windows 主集成 103.21 秒、节点目标 53.63 秒；Linux 主集成 54.11 秒、节点目标 52.98 秒。旧超时现场仍保留，这次通过不作为已定位或消除全部间歇故障的证据；当前进程检查未发现测试遗留节点，不支持以孤儿子进程解释上一轮失败。业务接入流程待确定，独立主机验收尚无可用环境，均不计为完成。

## 2026-10-05：认证选择互不冲突时等待旧候选占用释放

回归构造 B 持有货币 2、3 两个候选而 QC 选择 3，A 已持有货币 1、无资源替代候选 2 收到最终证书。修复前保护 A 错误返回 InvalidSnapshot（target/crossed-retained-claims-reproduction.log）。Commit 保护现在复用单一证明选择逻辑：finalized/不可变 finality 选择优先于最高 prevote QC；不同 finality 摘要拒绝，无法解析的证据不让任务变成可 Abort。比较资源时只使用双方认证选择，旧保留候选造成的交叉占用保持原状，等待保护任务 Commit，不取消其认证选择，也不提前授予等待任务资源或签票权利。真正认证选择冲突仍拒绝且不写盘。

既有认证终态入口现在同时收集 Abort 和 Commit 所释放资源涉及的等待任务，在持有者被删除前收集、提交执行后沿原事件重试，不新增扫描循环或全局排序。回归验证冷读仍保留三份占用，B 的真实 Commit 输出 A 的重试事件、仅剩 A 旧占用，A 再通过完整准备取得货币 2 并执行最终证书，两任务成功且货币 1 未被移动。无持有主计划的多候选、认证来源尚未受理时的交叉上下文、交接及状态改变后的迟到源仍需后续完成。

单向等待已经有回归证据，双方认证选择都被对方旧保留候选占用形成的环仍未解决；不能在未取得相应认证终态前释放旧权利，也不能用这项等待规则宣称环已可自行消失。后续需要针对该局部依赖给出可验证的安全处理与恢复证据。

本批 Windows 和 WSL Linux 全部 fmt/check/clippy(-D warnings)/all-targets 通过（target/crossed-retained-claims-windows.log、target/crossed-retained-claims-linux.log），均为 47 常规单元、242 主集成及 34 节点验收通过。Windows 主集成 73.91 秒、节点目标 43.54 秒；Linux 主集成 66.21 秒、节点目标 54.42 秒。定向复现/修复日志为 target/crossed-retained-claims-reproduction.log 和 target/crossed-retained-claims-fixed.log；git diff --check 通过。门禁证明本批已实现路径和既有回归，不证明上述环、交接、迟到请求或所有历史间歇超时已经解决。

## 2026-10-05：已受理候选最终证书依赖闭包的原子提交

最终证明现在可以保存在已受理但没有资源权利的确切候选上；选择该候选为主计划不改变其他候选自己的旧占用，签票仍按确切候选权限检查。phase=Finalized 和原 finality_votes 是唯一持久证明，沿原快照编码/验证保存，不增加平行证书表或兼容格式。冷读验证原委员会证书；无资源主候选的其他旧候选仍逐一恢复占用并验证业务约束。

执行从一个已认证任务出发，沿真实旧占用、其他已认证选择及既有 handoff fence 找到局部依赖闭包。缺少任一任务完整最终证书时不修改业务或释放资源；闭包中实际选择有资源重叠时拒绝，即使转账回原所有者也不能双重消费。证书齐全且实际选择资源互不冲突时，复用现有执行器、公共变更和业务快照原子提交，将该闭包一起执行并生成原恢复记录。TaskId 遍历只用于这份局部闭包，不产生普通交易全局顺序。冷恢复和认证 QUIC 补齐证书后执行的回归已通过，范围仅为正文已受理的候选。

普通 Commit 执行也使用同一闭包入口，避免先单独执行与其他已保存最终证明矛盾的选择；外部传入业务状态仍检查 stale-state。受阻证书进入运行时后先保存原 finalized 计划，再事件触发闭包检查；缺证书表示等待，不标记业务完成。原 contender 索引同时复用于 Commit 和闭包的相关唤醒。完成后只恢复该闭包的终态证书回应，并移除其旧会话/待处理消息，不刷新已有回应限速或重扫全部请求。已认证但尚未执行的来源首块提供自己的原最终证书，既有快照/来源版本仍为 1。

定向回归 target/certified-ring-quic.log 已通过：原证书不足时冷读保留旧资源、业务零变更且不写盘；伪造第二证书和无权签票不写盘；两份已认证选择的环冷恢复仅增加一次 business generation，两个成功结果和确切来源记录同时出现；普通 Commit 和冷恢复均拒绝实际选择重叠的两份矛盾证明。两节点真实认证 QUIC 连接把另一份证书送给只保留第一份的跟随节点，运行时闭包执行恢复两笔成功结果、清理旧会话，并继续提供原终态证明。证书由原四成员委员会夹具产生，本用例验证认证传输/执行恢复，不声称仅两节点能产生四成员委员会的 quorum。clippy(-D warnings) 通过（target/certified-ring-quic-clippy.log）；完整两端门禁仍需另记。

第一轮本批 Windows 全量主集成 239 通过、3 失败（target/certified-ring-windows.log）：旧 stale-state 回归的执行错误优先级改变；已有 Commit QC 优先权回归在提交无资源主见证时缺少本地 PaymentExecution，恢复 worker 因 TransferNotEstablished 退出；受阻冻结来源再现全节点高轮次无目标 QC 的超时。前两项正在修正，第三项不据此认定统一根因。认证无资源选择执行时现在仅对缺少的转账前置记录复用原 establishment 验证器，以当前地址状态和账户匹配约束重新建立，再与业务同次提交；普通持有计划缺记录仍拒绝，退役/禁止建立的地址不绕过。普通准备与认证恢复共享同一实现，不复制账户或状态验证逻辑。

缺少本地前置记录修复与原 stale-state/cancellation-closed 语义的定向回归已通过（target/certified-ring-witness-regression.log、target/certified-ring-stale-regression2.log），clippy(-D warnings) 通过。环回归同时验证没有本地建立记录的认证见证可在 Active 地址上执行，而 Retiring 地址不能被最终证书绕过，失败不写盘或改变余额。原 owned 计划即使执行失败，调用者的 prepared book 仍同步保存后的 Finalized 阶段，不能取消。状态改变后缺少原 eligibility 的历史源仍需认证基线方案，不由这项当前状态重建规则替代。

审查持久化最终选择时补上统一快照约束：Finalized 计划与已保存的 precommit readiness 不能有不同摘要。该检查同时约束两个到达方向，拒绝先保存旧 readiness 后切到相反最终证书，以及先保存最终证书后接受相反 QC；不会把旧 prevote 锁当作不可变终态。回归使用原委员会有效签名证明检查矛盾输入拒绝且不写盘，拒绝后冷恢复仍保留原选择，详见本批门禁记录。

本批含最终选择/readiness 检查的 Windows fmt/check/clippy(-D warnings)/all-targets 全部通过（target/certified-ring-immutable-fmt.log、target/certified-ring-immutable-check.log、target/certified-ring-immutable-clippy.log、target/certified-ring-immutable-windows.log），49 常规单元、242 主集成及 34 节点验收通过；主集成 70.78 秒、节点目标 40.14 秒。WSL Linux fmt/check/clippy 与 49 常规单元通过，但全量主集成 239 通过、3 失败（target/certified-ring-immutable-linux.log，77.61 秒）：受阻冻结来源、远端冻结来源和公共检查点发布超时。失败现场保留，原全量失败不被隔离重跑覆盖。隔离运行全部 3 个冻结来源用例通过（target/certified-ring-immutable-linux-source-isolated.log，7.96 秒），公共检查点用例也通过（target/certified-ring-immutable-linux-public-isolated.log，30.52 秒）；这些结果不足以定位或消除间歇故障，不能据此宣称 Linux 全部门禁通过。

当前没有确定真实业务流程和独立主机，外部业务接入和独立主机验收不计为完成。继续开发的实质缺口仍是未受理认证正文的候选安装、跨成员交接正文并集及新成员认证基线、状态改变后的迟到源处理，以及上述间歇同步失败。

Linux 的 34 节点验收单独运行通过（target/certified-ring-immutable-linux-nodes.log，48.79 秒）；它未在失败的 all-targets 命令中执行，故单独记录，不改变该全量命令失败的结论。git diff --check 通过。

## 2026-10-05：正文受理时消费先到的最终证书

新增回归复现了最终证书先于受阻冻结候选正文到达的缺口（target/buffered-finality-admission-reproduction2.log）：运行时已缓存原委员会证书，但正文受理仅处理 prevote QC，候选仍停留 Voting，等待下一次网络转发。受理入口现在在完整来源验证后，优先交给既有最终证书晋升和局部认证闭包恢复入口，不新增证明表、签票或扫描。证书依然经过原委员会和资源冲突检查，受阻时仅保存选择，旧资源继续保留；阻塞者认证 Abort 后沿原重试事件执行，不需要第二次传来该 Commit 证书。该修复不处理状态改变后无法验证的历史源，也不能据此认定上一批 Linux 超时已经定位。

本修复 Windows 定向回归、fmt/check/clippy(-D warnings)/all-targets 通过（target/buffered-finality-admission-fixed.log、target/buffered-finality-admission-windows.log），50 常规单元、242 主集成及 34 节点验收通过，主集成 92.27 秒、节点目标 44.00 秒。Linux fmt/check/clippy 和常规单元通过，主集成 241 通过、公共检查点发布超时 1 项失败（target/buffered-finality-admission-linux.log，76.80 秒），后续节点目标未执行。原公共检查点测试在超时后清理现场且只报告 Elapsed，现改为在停止 worker 后保留失败快照，并记录阶段、节点连接、业务完成/检查点 epoch、prepared 数量和共识事件；成功路径仍清理。它只补充失败证据，不放宽 60 秒期限或改变协议行为。

补充诊断后的 Linux 全部 fmt/check/clippy(-D warnings)/all-targets 通过（target/buffered-finality-diagnostic-linux.log）：50 常规单元、242 主集成及 34 节点验收通过，主集成 45.08 秒、节点目标 49.37 秒。Windows 诊断改动后的 check/clippy/fmt 通过；上述 Windows 完整门禁包含生产修复但在新增失败诊断之前。公共检查点这轮未再复现，原失败日志保留，不能据此声明间歇超时根因已定位或已修复。git diff --check 通过。

## 2026-10-05：没有持有主计划的多冻结候选

扩展既有证书先到回归，构造两个真实持有者分别占用货币 1、2，目标任务只保存无资源见证。旧实现拒绝其第二份合法冻结来源（target/unowned-variants-reproduction.log）。候选验证现在不要求主计划持有资源；仍校验同一签名请求、原委员会、候选容量、当前业务状态与真实占用，已 finalized 计划不能再添加候选。受阻替代候选仍只有来源见证，不获得资源或签票权利；原委员会最终证书可以选中它，阻塞者认证 Abort 后执行确切选择。

同一回归还复现了无资源主见证取得资源时丢失已受理替代候选（target/unowned-variants-retention-reproduction.log）。完整重新准备现在保留原候选列表；存在任何已持有候选或 Finalized 选择时，普通重新准备直接拒绝，避免覆盖旧权限或终态证明。回归冷读验证两种主计划权限、旧候选保留、资源计数、禁止未授权签票与禁止重建最终选择，并核对最终实际移动货币及原恢复记录。这仍不解决已经改变业务状态的历史来源受理，也不替代跨成员业务基线同步。

本批定向回归及 Windows fmt/check/clippy(-D warnings) 通过（target/unowned-variants-final-regression.log、target/unowned-variants-check.log、target/unowned-variants-final-clippy.log）。Windows all-targets 的 50 常规单元通过，主集成 241 通过、受阻冻结来源超时 1 失败、1 忽略（target/unowned-variants-windows.log，102.38 秒），节点目标未在该命令中执行。该失败超时诊断中节点 1 的目标状态尚未成功，而其他三节点成功；随后重建当前 mixed_snapshot_probe 冷读节点 1 的保留快照，确认目标最终成功、无取消（target/unowned-variants-late-snapshot.log）。这证明该次节点后来提交，不证明延迟根因或所有超时已修复。Windows 全部 3 个来源用例隔离运行通过（target/unowned-variants-windows-source-isolated.log，10.58 秒）。Linux 全部 fmt/check/clippy(-D warnings)/all-targets 通过（target/unowned-variants-linux.log），50 常规单元、242 主集成及 34 节点验收通过，主集成 62.25 秒、节点目标 50.96 秒。

Windows 单独补跑 34 节点验收通过（target/unowned-variants-windows-nodes.log，45.88 秒）；不改变原 Windows all-targets 失败的结论。git diff --check 通过。

## 2026-10-05：没有任务绑定的业务基线

回归复现交接承诺遗漏：有 genesis 账户但没有 task binding 的快照，其业务摘要被当作 None（target/handoff-unbound-baseline-reproduction2.log）。基线生成现在只对规范的空 genesis（无账户、货币、支付地址、认证绑定，frontier=1）省略摘要；其他规范化共享状态复用既有状态编码和摘要。临时准备绑定、前置记录及重试来源的排除规则保持原语义，认证分配和终态绑定仍计入。回归验证账户、支付地址、储备/frontier 变化均使承诺失配，省略承诺和不同业务基线激活均拒绝且不写盘，相同基线认证切换可冷恢复。该修复封闭业务基线承诺的遗漏，并不自动传输或安装新成员缺少的业务正文。

Windows 基线定向回归通过（target/handoff-unbound-baseline-fixed.log）。首轮完整主集成 236 通过、6 失败、1 忽略（target/handoff-unbound-baseline-windows.log，93.89 秒）：5 个公开状态/恢复/安全轮换夹具在非空业务基线上直接认证无基线切换，均被 StalePreparedTasks 拒绝，现同步改为调用原 prepare_validator_set_transition 后认证；不保留旧切换格式兼容。另 1 个耗尽分配用例完成超时，无证据将其归因于基线校验。后续门禁按修正后的实际结果另记。

修正夹具后 Windows fmt/check/clippy(-D warnings) 与 51 常规单元通过，但完整主集成 240 通过、支付 CLI/受阻冻结来源超时 2 项失败、1 忽略（target/handoff-unbound-baseline-refined-windows.log，102.23 秒）；5 个基线夹具和耗尽分配用例已通过。Linux 全部 fmt/check/clippy(-D warnings)/all-targets 通过（target/handoff-unbound-baseline-linux.log）：51 常规单元、242 主集成及 34 节点验收通过，主集成 48.40 秒、节点目标 49.83 秒。Windows 两个失败用例隔离运行通过（target/handoff-unbound-baseline-windows-source-isolated.log，4.14 秒；target/handoff-unbound-baseline-windows-cli-isolated.log，16.31 秒），不改变原 Windows 全量失败结论，也不计为间歇超时根因已定位。

Windows 34 节点验收单独运行通过（target/handoff-unbound-baseline-windows-nodes.log，50.46 秒）；git diff --check 通过。此批封闭未绑定业务基线的承诺遗漏，跨成员正文合并/传输和新成员业务基线安装仍未完成。

## 2026-10-05：交接正文的认证分块受理与候选合并

成员切换的紧凑来源继续沿原 CurrencyAllocation 前沿屏障传播；若接收节点不能从本地义务重建所承诺的 handoff root，复用现有认证私有来源拉取传输紧凑成员源和原 handoff 编码，不新增全网广播或业务总排序。正文按 32 KiB 分块，交接仍受 512 MiB 上限，fetch 总缓冲同时限制为一个最大交接正文加原常规拉取预算。超过常规来源上限的输入首块必须是交接 framing；完成时移动缓冲，不复制整份大正文。不可变 handoff 的编码按需缓存，新增候选时失效，同一传输不为每块重新编码所有计划；内部格式和 domain 仍为开发版本 1。

接收端先核对原委员会、成员源摘要、handoff 摘要和业务基线，再对本地尚未验证的正文检查签名授权，并复用唯一准备构造器验证确切冻结操作。新增整项任务或替代候选只保存无资源见证及请求绑定，不导入前置记录、资源、签票权利或未经证明的终态；原本地权限和证明保留。与现有争用受理共用候选合并/容量检查。 staged handoff 必须覆盖本地每份已受理候选和继承义务，全部通过后才原子保存来源上下文并启动原成员切换共识；有遗漏、伪造、授权错误、基线不一致或过期新请求均 fail closed。

定向回归使用超过一个分块的正文，含同一请求的两份冻结计划及接收端未受理的整项任务。真实双向认证 QUIC 从紧凑成员源触发拉取，接收端得到相同交接摘要、旧资源占用计数不变、余额不变、新候选无签票权限；重复正文无写盘。它验证正文受理/传输，不声称两节点可产生原四成员委员会的 quorum，也不证明所有节点彼此独有义务已自动收集完毕。新成员缺少共享业务基线时仍拒绝，业务基线正文安装、签票前的分布式义务并集收集，以及历史状态改变后的源处理仍需后续完成。

本批最初的正文提供仅读取已受理的 pending transition；切换后的提供路径见后续记录，不能将正文提供计为全部跨成员恢复完成。

本批定向认证 QUIC/遗漏及伪造拒绝回归通过（target/handoff-source-quic-final-tests.log），Windows fmt/check/clippy(-D warnings)/all-targets 全部通过（target/handoff-source-windows.log）：52 常规单元、242 主集成及 34 节点验收通过，主集成 98.84 秒、节点目标 46.08 秒。Linux fmt/check/clippy 与 52 常规单元通过，但主集成 239 通过、3 失败（target/handoff-source-linux.log，74.03 秒）：真实 CLI 重启支付、四节点恢复候选切换、公共检查点超时；后续节点目标未在该命令中执行。新的公共检查点诊断定位到 late validator proof catchup：前三节点和公共观察节点持有 epoch 1，第四节点已连接但没有证明，worker 仍存活；它提供补传路径的调查线索，未证明统一根因。三项隔离运行通过（target/handoff-source-linux-public-isolated.log，30.53 秒；target/handoff-source-linux-recovery-isolated.log，14.54 秒；target/handoff-source-linux-cli-isolated.log，46.52 秒），原全量失败不被覆盖。

Linux 34 节点验收单独运行通过（target/handoff-source-linux-nodes.log，46.47 秒）；git diff --check 通过。完整目标仍保留跨节点义务并集收集、切换后正文补取、新成员业务基线安装、历史/迟到源、间歇补传超时与外部业务/独立主机验收。

## 2026-10-05：切换完成后的交接正文提供

pending transition 清除后，原提供入口现在从当前已安装 handoff 的 certifier set 查找同一快照中的持久化切换证明，复用唯一成员源摘要函数，核对原委员会版本、分配前沿范围、源摘要和下一委员会。快照加载已验证正文与证明绑定，不在每个分块重新验签、重建业务基线或复制另一份交接；正文继续使用既有按需编码缓存。只能提供当前已安装交接，不承诺跨后续多次切换的历史正文留存。

原认证 QUIC/受理回归扩展到三票认证激活后重新打开 StateStore，确认 pending 项已删除、所有分块仍与激活前逐字节相同，错误摘要、错误版本范围和正文终点偏移返回不可用，读取不增加 generation（target/handoff-activated-regression.log）。此扩展验证冷恢复提供入口；原真实 QUIC 段仍在激活前执行，不能据此声明切换后的完整网络恢复或新成员业务基线安装已验收。当前真实业务流程尚未确定，且没有独立主机，不虚构这两项外部验收。

本批 Windows fmt/check/clippy(-D warnings)/all-targets 全部通过（target/handoff-activated-windows.log），52 常规单元、242 主集成及 34 节点验收通过，主集成 85.41 秒、节点目标 46.34 秒。Linux fmt/check/clippy 与 52 常规单元通过，主集成 240 通过、2 失败（target/handoff-activated-linux.log，74.80 秒）：公开检查点仍在 late validator proof catchup 阶段超时，第四节点已连接但没有证明；真实 CLI 重启支付用例在恢复检查点提交入口反复收到 GovernanceRejected(Busy)，15 秒重试期限耗尽，此次不是单纯网络超时。日志及失败现场保留，不用隔离重跑覆盖原失败结论。Linux 34 节点验收单独运行通过（target/handoff-activated-linux-nodes.log，48.11 秒）；git diff --check 通过。尚需完成分布式义务并集收集、新成员业务基线安装、历史来源验证与上述补传/Busy 故障排查。

## 2026-10-05：迟到检查点补传与离线拨号队首阻塞

两个行为回归先在旧实现失败：公开检查点用例移除迟到成员连接后的再次发布请求，60 秒内该成员仍没有证明（target/checkpoint-late-reproduction.log）；新增连接回归将真实无响应 UDP 端点置于首位，健康两个远端已运行但本节点出站连接仍为空，恢复请求在 3 秒内持续 Busy（target/checkpoint-busy-reproduction.log）。后者证明顺序拨号可让离线节点阻塞健康 quorum 的建立；不将所有曾出现的 Busy 或网络超时都归于此因。

新 exact-set 出站连接建立后，既有恢复证明补传入口扩展为一次加载快照、分别发送当前集合公开/恢复证明；公开证明可沿用已持久化基线。接收端不变，继续验证原委员会认证并安装。缓存公开证明的操作者广播也记录发送失败，避免丢失诊断。无新增扫描、签名、证明表或定时任务。连接维护从 runtime_bft 主文件收敛到 connectivity 子模块，以最多 4 个并发拨号使用既有认证与全局连接 permit，维护候选仍按完整 PeerRecord 去重，保留同一身份不同端点的尝试能力。连接 quorum 的 Busy 判定保持原连接门槛；不延长请求或测试超时，也不提前宣称治理完成。

两个定向修复回归通过（target/checkpoint-busy-fixed.log，0.40 秒；target/checkpoint-late-fixed.log，30.16 秒）：恢复请求经健康三成员产生实际 quorum 证明，无响应第四成员没有加入已连接成员；迟到公开证明不需要操作者重复发布。完整平台验证结果另记。

首轮 Windows fmt/check/clippy(-D warnings)/all-targets 通过（target/checkpoint-fix-windows.log），52 常规单元、243 主集成及 34 节点验收通过，主集成 73.70 秒、节点 46.33 秒。Linux fmt/check/clippy 和常规单元通过，主集成 239 通过、4 失败（target/checkpoint-fix-linux.log，71.45 秒），34 节点目标未在该命令中执行。CLI 恢复与新拨号回归通过；公开检查点已通过迟到证明追赶，随后在错误身份请求的返回值断言失败。其余 3 个冻结来源用例出现连接丢失/超时，保留失败现场，未确定根因。

扩展同一连接回归，在健康远端 listener 启动前分别提交错误身份公开请求及合法恢复请求：旧治理入口错误身份返回 Busy（target/checkpoint-busy-auth-reproduction.log），证明原响应优先级把身份错误遮住。三类治理入口现在仍计算一次原连接就绪条件，但先用原验证器判定 Unauthorized，合法身份才受 Busy 门槛限制，不取消 quorum 检查。扩展回归修复通过（target/checkpoint-busy-auth-fixed.log），公开用例增加实际错误值诊断。最终平台门禁记录于下。

最终代码 Windows 与 WSL Linux 的 fmt/check/clippy(-D warnings)/all-targets 全部通过（target/checkpoint-fix-final-windows.log、target/checkpoint-fix-final-linux.log）：两端各 52 常规单元、243 主集成及 34 节点验收通过，Windows 主集成 92.36 秒、节点 40.85 秒，Linux 主集成 46.35 秒、节点 49.11 秒。公开检查点完整用例、CLI 重启/恢复流程、新连接回归均通过；git diff --check 通过。首轮失败现场与日志仍保留；最终这轮三个冻结来源用例通过不构成其历史间歇超时根因已定位的证据。本批封闭上述已复现的迟到补传、离线拨号队首阻塞与 Busy 遮蔽身份错误；新成员业务基线安装、分布式义务并集与外部业务/独立主机验收仍不计为完成。

## 2026-10-05：交接中已受理请求的截止资格

新回归复现了已在截止前本地受理的 Transfer 请求，在截止后收到同一请求的另一份合法冻结候选时，handoff admission 错误返回 TaskExpired（target/handoff-deadline-reproduction.log）。交接与争用入口现在共用已受理请求的时间资格判定，要求本地持久 PreparedTask 的完整签名源、请求摘要与原委员会版本匹配；资格不取决于主候选是否持有资源。新候选仍经唯一准备器验证当前业务状态及确切冻结操作，作为 false witness 合并，不继承资源或签票权利。

一个业务回归同时核对：只有 Pending 绑定、没有 PreparedTask 的节点不能借此受理过期请求；交接含另一首次收到的过期任务时整体拒绝且无 generation/候选变化；已受理请求的候选在截止后正常合并，冷读资源计数和余额不变，重复受理不写盘（target/handoff-deadline-final-regression.log）。不增加原请求的平行历史表或新的可信时间戳。这里只补原请求的时间资格；状态已改变后的未知历史来源仍需认证基线/结果证明，不能据此认定四项总目标完成。分布式义务并集、新成员业务基线安装和冻结来源间歇故障继续保留为内部开发任务；外部业务或独立主机不作为这些任务的开发边界。

本批两端 fmt/check/clippy(-D warnings) 与 53 常规单元通过，但完整 all-targets 均失败。Windows 主集成 240 通过、3 失败、1 忽略（target/handoff-deadline-windows.log，98.57 秒）：单节点提交持久成功超时、静默发现节点之后的候选连接超时、跨节点业务重试未全员成功。后者三个节点成功而节点 1 未完成且有连接超时；失败的单节点 CLI 用例还遗留本次 second-cli-submit-validator 进程，核对完整命令行后只停止该进程，保留磁盘现场，使原命令退出 101，不修改失败结论。Linux 主集成 241 通过、2 失败（target/handoff-deadline-linux.log，81.62 秒）：CLI 恢复请求 Busy 重试耗尽，公开检查点停在 late validator connection，第四节点只连到两个原成员、尚未进入正文证明补传阶段。两端三个冻结来源用例均通过，不据此消除稳定性缺口。Windows/Linux 的 34 节点验收分别单独运行通过（target/handoff-deadline-windows-nodes.log，38.05 秒；target/handoff-deadline-linux-nodes.log，51.90 秒）；git diff --check 通过。

已核对当前构建依赖 quinn-proto 0.11.18 的连接代码：其 Timer::Idle 触发 ConnectionError::TimedOut，Display 为 timed out；本项目请求期限产生的字符串则为 protocol request timed out。既有已认证连接发送日志中的直接 timed out 因而提供了传输空闲计时器的调查线索，但没有证明计时器为何触发，不能用该线索冒充 CPU/调度、网络丢包或重复断连的根因。下一次稳定性排查需要保存当前被连接维护入口忽略的拨号错误，并区分连接建立、消息发送和运行时调度阶段。

## 2026-10-05：连接失败证据与失败夹具清理

连接维护不再忽略原拨号错误：每次失败向既有有界事件队列写入 ConnectionFailed，保留目标 NodeId、端点、从加入拨号队列到失败的耗时及原错误。Unauthorized 仍沿原策略加入拒绝缓存，其他失败仍由原 maintenance 重试；成功路径不记录额外事件。不创建业务 scope、证明表、历史日志副本或后台采样循环，网络与快照格式不变。既有真实无响应 UDP 回归扩展为在健康 quorum 受理并认证恢复请求后，收到确切离线端点的失败及至少 3 秒耗时（target/connection-diagnostic-regression.log，5.13 秒），证明诊断覆盖之前被忽略的真实连接失败。

单节点 CLI 提交夹具改为复用现有 RunningNode 生命周期守卫，在读取首行前即持有守卫；请求断言或启动解析异常展开时停止并 wait 本次子进程。它不删除失败快照或密钥。新增原故障回归启动真实节点后在父测试中触发可捕获异常，随后 node-check 成功证明原目录运行锁已释放，并比较 canonical StateRecoveryPayload 确认业务状态保留。原 CLI 提交用例一起通过（target/connection-diagnostic-cli-regressions.log）。这修复测试失败后的进程遗留，不宣称生产节点的连接不稳定已消失；四项内部总目标保持原范围。

该批 Windows 全部门禁通过（target/connection-diagnostic-windows.log）；Linux fmt/check/clippy 与单元通过，但主集成 243 通过、四节点历史恢复候选推进 1 失败（target/connection-diagnostic-linux.log，66.78 秒），后续 34 节点目标未执行。诊断区分出 protocol request timed out 的拨号失败及 connection lost/timed out 的发送失败，尚未解释所有失败成因。

## 2026-10-05：公开发现不能阻塞 Validator 重连

串行维护循环虽然先执行 Validator 拨号，但之后公开 bootstrap 的多个无响应端点会推迟下一轮 Validator 拨号，故“首轮不受阻”不能证明重连不受阻。真实回归先让成员的认证请求超时，再于公开发现仍等待两个 UDP 黑洞时启动该成员；旧实现未在 4 秒内重新建立出站 Validator 连接（target/public-stall-reconnect-reproduction.log），失败现场保留。

维护入口现在并行等待现有 Validator 与公开维护路径，不创建脱离节点的后台任务。原公开退避、全局连接上限、4 路 Validator 拨号、认证和 deadline 不变，Validator 沿原 2 秒间隔独立重试，非 Validator 节点跳过该路径。两项连接回归通过（target/public-stall-reconnect-fixed.log，7.12 秒），包含真实 quorum 的恢复认证。该批只证明并修复重连被公开发现阻塞，不能据此消除历史恢复全量失败或宣布四项内部目标完成。

本批两端 fmt/check/clippy(-D warnings) 通过。Linux all-targets 全部通过（target/public-stall-reconnect-linux.log）：53 常规单元、245 主集成及 34 节点验收通过，主集成 46.93 秒、节点目标 88.18 秒。Windows 常规单元通过，但主集成 244 通过、1 失败、1 忽略（target/public-stall-reconnect-windows.log，77.45 秒）：冲突仲裁用例在提交独立任务时返回 Preparation(Persistence(StalePreparedTasks))，位于 runtime_conflicts.rs 的 submit_legal_task 入口；这次失败不是该位置的网络 deadline。存储 save_with_prepared 保留对预期 prepared 集合的并发一致性比较，提交路径缺少对此临时失配的重新读取/受理处理，不能削弱存储校验来掩盖。四节点历史恢复和三个冻结来源用例在两端这轮均通过，不推断历史间歇故障全部解决。Windows 34 节点目标另行通过（target/public-stall-reconnect-windows-nodes.log，39.94 秒）；原 Windows 全量失败结论保留。git diff --check 通过；测试耗时不是性能基准。

## 2026-10-05：提交中的原子比较失配

原提交入口分别读取业务状态和 PreparedTaskBook，并在后台 finality/commit 改变预期状态或准备任务集合后直接返回临时失配。现在任务簿复用唯一 claims 恢复构造器，从同一快照的 prepared 集合构造；入口只对 StaleState/StalePreparedTasks 最多重新读取三次并重新受理。签名与编码大小在循环外验证，其他错误和持续冲突照常返回，不弱化 CAS、资源排他、finality 或签票边界，也不增加后台重试队列。

确定性回归在读取快照之后写入第一任务的真实 quorum finality，旧快照的独立任务受理确切返回 StalePreparedTasks，真实提交重试后保留第一任务的完整 finality votes 并准备第二任务。随后第一任务原子开户提交使业务状态改变，旧快照受理先返回 StaleState，重试必须重新检查当前业务并拒绝已存在账户；同一 TaskId 的不同请求仍拒绝且无 generation 变化，冷读保留成功终态和独立任务（target/submission-stale-regression.log）。该测试直接制造实际持久化竞态窗口，没有放宽断言或靠隔离重跑掩盖上一轮失败；四项内部目标仍保持原范围。

本批两端 fmt/check/clippy(-D warnings) 与 54 常规单元通过。Linux all-targets 全部通过（target/submission-stale-linux.log），245 主集成及 34 节点验收通过，分别 69.03 秒、54.80 秒。Windows 两项冲突仲裁回归通过，但主集成 244 通过、1 失败、1 忽略（target/submission-stale-windows.log，88.02 秒）：跨任务 CLI 用例中的 PowerShell 示例在 MaxAttempts=4 耗尽；该用例每轮交替访问一个不可用端点和一个正常成员，仅两轮查询正常成员，失败消息未记录最后的查询状态。重新构建现有 mixed_snapshot_probe 后，按确切 TaskId 冷读本次四份保留快照，8004 均 succeeded=Some(true)、cancelled=false（target/submission-stale-cli-cold.log）；这证明请求最终完成，不能说明其何时完成、为何脚本未在预算内观察到成功，不能用迟后成功覆盖原失败或推断所有连接故障消失。Windows 34 节点目标单独通过（target/submission-stale-windows-nodes.log，36.66 秒）；git diff --check 通过。业务基线安装、义务并集与历史状态来源仍未闭合。

## 2026-10-05：不可变交接业务基线正文

新成员安装缺少当时被认证的完整业务基线，而提供端业务前进后不能从最新 state 重建旧摘要。因此 TaskHandoff 捕获时保留摘要对应的规范化 SecondState 正文，和交接冻结计划共用原 512 MiB 限额、现有分块通道及编码缓存。它只保留当前交接锚点需要的不可变恢复数据；正文排除旧 task_handoff 和本地准备进度，后续交接不会累积嵌套历史。开发格式仍为 1，直接更新当前编码，不保留旧交接 reader。委员会认证的交接摘要通过原 business digest 绑定正文，解码拒绝摘要不匹配、空正文/摘要不一致、不规范正文和任何嵌套交接；复用原状态解码器，不实现第二套账户/货币 decoder。

扩展现有非空基线回归，在真实 quorum 激活委员会后完成新的开户提交，重新打开 StateStore，确认当前业务含新账户而留存正文仍准确恢复切换前状态，且交接编码不变。篡改正文拒绝；即便重新计算正文摘要，也不能把基线末尾标记改成递归交接，generation 不变。三项现有基线回归通过（target/handoff-retained-baseline-regression.log）。这提供认证安装和历史正文验证所需的原始状态，不把字节留存计作完整新成员安装、签票安全恢复或四项总目标完成。

本批 Windows/Linux fmt/check/clippy(-D warnings)/all-targets 全部通过（target/handoff-retained-baseline-windows.log、target/handoff-retained-baseline-linux.log）：各 54 常规单元、245 主集成及 34 节点目标通过。Windows 主集成 84.94 秒、节点 37.46 秒，Linux 主集成 69.34 秒、节点 55.37 秒，仅为本机验证耗时。真实认证 QUIC 交接分块受理、已有 shared recovery/rotation、四节点历史恢复、冻结来源及冲突仲裁路径继续通过；git diff --check 通过。先前间歇失败的原始记录不被本轮通过覆盖，完整新成员安装、义务并集和状态改变后的历史受理仍待实现。

## 2026-10-05：认证交接基线的原子安装

新增 StateStore::install_validator_handoff_baseline，以本地可信旧委员会/registry 锚点验证成员来源，复用旧 quorum 对交接根的认证及唯一业务正文 decoder，再登记下一 registry、继承义务绑定与必要历史集合。不复制准备器或重新执行历史业务：旧委员会 quorum 认证完整基线及冻结义务，安装不导入准备资源、签票权利或未认证业务结果。任务请求按本地 AuthorizerSet 验证；同一任务多份冻结候选共用一次原签名验证，并要求完整请求相同。它在空目标的同一排他锁下完成验证和单次原子写入，复用 checkpoint recovery 的初始共享状态写入器，默认 locked、minimum signing version 为下一集合版本。该抽取保持 checkpoint recovery 的原证明/floor 语义。

回归构造真实 1..4 到 1/2/3/5 的 admission 和旧 3/4 quorum，安装完整账户/储备基线与尚未完成的开户义务，冷读核对新成员、历史集合、准确 handoff、pending binding 和未执行账户；PreparedTask、资源权利、签票锁和 BFT state 均未导入，真实新成员 signer 拒绝投票。错误 quorum、损坏正文、错误信任锚点或不受信任 Authorizer 全部在写入前拒绝，已有目标禁止覆盖且 generation 不变。规范空 genesis 分支也验证坏 quorum 拒绝、合法安装 locked 并持久保留切换证明；两种安装都可由既有 shared recovery codec 读回（target/handoff-import-regression.log）。网络拉取/CLI 接入及签票安全恢复仍未闭合，不能将此存储入口计作完整新成员接入或四项总目标完成。

本批两端 fmt/check/clippy(-D warnings)、55 常规单元及安装回归通过，但 all-targets 主套件均失败。Windows 244 通过、1 失败、1 忽略（target/handoff-import-windows.log，104.10 秒）：受阻冻结来源任务超时，失败时四节点均连接另外三成员、诊断无 SendFailed，局部 BFT round 为 8/9 且尚无 prevote QC。按确切 TaskId 冷读本次四份失败快照，均 pending、未取消、停在 round 9 且无 prevote QC（target/handoff-import-frozen-cold.log），此次不能归为迟后成功或直接归因于断连。Linux 244 通过、1 失败（target/handoff-import-linux.log，65.81 秒）：真实 CLI 网络恢复流程连接成员 2 时返回 protocol request timed out。完整失败和现场保留，不把已连接数量当作协议推进证据，也不推断二者统一根因。Windows/Linux 34 节点目标分别单独通过（target/handoff-import-windows-nodes.log，54.94 秒；target/handoff-import-linux-nodes.log，53.85 秒）；git diff --check 通过。
## 2026-10-05：安装后迟到正文的截止资格与当前集合持久化

新成员原子安装交接基线后，已有委员会证书认证的精确冻结正文此前仍因本地没有 PreparedTask 而失去截止资格。回归安装非空基线，在新集合完成无关开户，再于原任务过期后补取原正文，确切复现 TaskExpired（target/handoff-late-body-reproduction.log）。受理现在复用统一资格判断：只有本地已受理的完整请求，或已安装交接中精确 TaskId/plan digest、完整签名请求与原委员会版本匹配的候选，保留原截止资格；Pending 绑定、远端版本提示和未列入的候选不足以延长截止。错误摘要回归在任何本地准备记录形成前拒绝且 generation 不变。

准备计划继续按原委员会版本验证，而写入使用实际活动集合，避免历史正文把 V2 持久化回 V1 或触发 ValidatorRegistryMismatch。仅当精确候选属于已安装交接、调用方交接与持久交接一致且原集合等于实际活动集合或精确留存集合时采用该路径；现有业务与 prepared CAS 保持原样。回归使用真实旧 3/4 finality 证书提交迟到开户，冷读同时保留新集合开户和历史开户，新成员仍 locked、minimum signing version=2、没有签票锁（target/handoff-late-body-fixed.log）。这是存储/准备路径的回归，不证明网络运行时、资源已改变的历史正文或四项总目标闭合。

本批 Windows/Linux fmt/check/clippy(-D warnings)/all-targets 全部通过（target/handoff-late-body-windows.log、target/handoff-late-body-linux.log），各 55 常规单元、245 主集成与 34 节点目标通过。Windows 主集成 104.26 秒、节点 41.54 秒；Linux 主集成 46.86 秒、节点 60.72 秒，仅是本机测试耗时。git diff --check 通过；以前完整套件中的间歇停滞和请求超时记录仍保留，不能以这轮通过宣称全部根因解决。新成员仍缺认证网络安装及被动历史终态推进闭环，交接义务分布式并集仍待实现。

## 2026-10-05：新成员被动应用历史 Commit 终态

扩展原认证基线安装回归，通过新成员 5 的实际 NodeRuntime 消息处理入口接收旧 1..4 的 3/4 finality 证书，复现消息残留在共识队列而任务未提交（target/handoff-passive-finality-reproduction.log）。交接被动终态入口此前仅处理 Abort，现在对已安装交接中精确 digest/request/origin 匹配、已验证正文且拥有提交资源权利的历史 Commit，复用 commit_certified_from_store 的原委员会 finality 验证、原子组件执行和有界 CAS 重试。认证依赖尚未齐全时保留现有持久 Finalized 状态等待既有组件恢复，不创建第二份待办真值。

新成员不属于精确留存原集合时，恢复或正文到达不创建该继承请求的历史投票会话；其身份缺少旧集合 key 不再阻断恢复入口。该限制只适用于已认证继承请求，不放宽现有原成员缺失历史 key 的错误。回归先执行实际启动恢复，再发送不足 quorum 的伪造终态，确认 generation 不变；合法证书通过同一入口被动提交，冷读保留无关新业务、历史成功、locked、minimum signing version=2，无投票锁和 BFT local state（target/handoff-passive-finality-fixed.log）。这是运行时消息入口验证，尚非新成员认证网络拉取的端到端验收；未取得资源权利的历史候选仍待推进。

初版 Windows 全部门禁通过，主集成 245 通过、1 忽略（89.12 秒）、34 节点目标通过（51.82 秒，target/handoff-passive-finality-windows.log）。Linux fmt/check/clippy、55 常规单元通过，但主集成 243 通过、2 失败（61.89 秒，target/handoff-passive-finality-linux.log）：CLI recovery-checkpoint 的 QUIC 连接 Transport(timed out)，冻结来源私有拉取停滞，四节点均连接另外三成员；失败诊断读取时三节点 StalePreparedTasks，剩余节点 round=1、无 QC，不能凭该读取失配直接解释停滞。34 节点目标在该次失败后未执行；失败现场保留。复核将被动 Commit 限定于不属于原委员会的新成员，原成员仍经现有共识会话完成清理，最终限定后的双平台门禁待执行。

限定后两端 fmt/check/clippy(-D warnings)/all-targets 全部通过（target/handoff-passive-finality-final-windows.log、target/handoff-passive-finality-final-linux.log），各 55 常规单元、245 主集成与 34 节点目标通过；Windows 主集成 84.96 秒、节点 50.08 秒，Linux 主集成 46.76 秒、节点 57.31 秒，仅为本机验证耗时。git diff --check 通过。重新构建现有原生 Linux snapshot probe，冷读初版失败的四份确切快照：节点 2 的任务绑定仍 pending、未取消、round=1 且无锁或 QC；其他三节点无任务绑定、无该 scope BFT 状态（target/handoff-passive-finality-linux-cold.log）。这排除该现场迟后成功的解释，也区分于上一批四节点均有计划且 round=9 的失败；不能据最终门禁通过消除该故障或把两种现场视作已确认同一根因。四项总目标继续保持原范围。

## 2026-10-05：正文重试期限不能被无关活动延期

沿失败现场检查正文请求链路，发现节点等待代码在每次共识活动后重建 proposal 时长的相对 sleep。来源全部尝试过后，process_prepared_task_sync 只重试未尝试来源；只有上述 sleep 分支才重开来源轮次并补发公告。因此持续 activity 可无限延后这项必要重试。现保留等待期间的同一绝对期限，并由原共识等待入口与 BFT deadline 合并；到期前置检查及计时分支优先级避免已排队 activity 抢占到期重试。只在已有 pending fetch/announcement 时启用期限，没有额外后台循环、空闲定时唤醒、持久化状态或扩大广播；原 proposal 间隔、来源验证和原始传输 deadline 不变。

调度回归以每 5 毫秒的实际活动通知验证原 40 毫秒绝对期限仍到期，且到期与通知已排队时仍返回重试；无 pending sync 继续由真实通知唤醒。第一版仅依赖 biased sleep 的已到期断言确切失败（target/source-retry-deadline-regression.log），加入到期前置检查后通过（target/source-retry-deadline-fixed.log）。扩展原来源选择回归，确认尝试完两个授权来源后普通重试不能重复拉取，定时重试才重新开放原来源（target/source-retry-cycle-regression.log）。这证明并修复重试饥饿路径，不据此断言此前所有连接超时和冻结停滞都由该问题造成。

本批 Windows/Linux fmt/check/clippy(-D warnings)/all-targets 全部通过（target/source-retry-deadline-windows.log、target/source-retry-deadline-linux.log）：各 56 常规单元、245 主集成与 34 节点目标通过。Windows 主集成 100.42 秒、节点 57.98 秒；Linux 主集成 47.16 秒、节点 59.69 秒，仅为本机验证耗时。git diff --check 通过；旧失败日志和私有现场不清除。继续检查发送链路还发现 broadcast/send_direct 对 TrySendError::Full 与 Closed 都标记连接死亡并移除/关闭 peer，暂时背压因此可触发主动断连；该分支尚待实际队列饱和回归与修复，不能将它直接视作所有历史连接超时的已确认成因。义务并集、新成员认证网络安装及完整历史状态受理仍未闭合，四项总目标保持原范围。

## 2026-10-05：有界出站队列背压不能主动断开存活连接

真实认证 QUIC 回归持有两个有界发送队列的 receiver 来模拟暂时未被消费的消息。第二次定向入队确切触发 Full，旧实现立即将仍存活的 peer 移除，connected_validator_ids 从成员 2 变为空（target/send-backpressure-reproduction.log）。该分支同时存在于 broadcast 与 send_direct，普通消息和 FinalityVote/Certificate 分流都受影响。

两处现在复用 ManagedValidatorBftPeer::enqueue 的唯一队列选择与错误判断。Full 保留既有连接和 alive 标记，只拒绝当次入队，沿现有来源重试/共识重发再尝试；真正 Closed 仍标记死亡、关闭 peer 并释放 permit。不扩大有界队列或 in-flight 上限、不新增消息缓存、后台循环或重连策略，也不放宽 exact-set/privacy、签票或 finality 校验。回归对普通/终态队列各验证定向 Full 与广播 Full 保持连接，消费原队列后同一认证 QUIC peer 继续实际传输并由远端收到准确消息，未再次握手；关闭真实 receiver 后 worker-closed 错误仍清理连接且 permit 回到零（target/send-backpressure-fixed.log）。这修复可复现的主动断连触发，不证明全部历史超时均由队列饱和造成。

本批两端 fmt/check/clippy(-D warnings)、57 常规单元及背压回归通过，但两端完整 all-targets 均失败。Windows 主集成 242 通过、3 失败、1 忽略（100.85 秒，target/send-backpressure-windows.log）：单节点 CLI QUIC 握手 Transport(timed out)、跨节点支付等待 Elapsed、四节点 CLI 未在期限内全员持久提交。三个冻结来源场景均通过；未执行的 34 节点目标单独通过（47.79 秒，target/send-backpressure-windows-nodes.log）。Linux 主集成 245 项全部通过（46.92 秒），随后 34 节点目标因在连接期限内仅有 29/33 条认证连接失败（34.71 秒，target/send-backpressure-linux.log）；独立重跑通过（55.23 秒，target/send-backpressure-linux-nodes.log），不替代原完整失败，也不能据此认定大集合间歇故障消失。失败日志和私有现场保留，git diff --check 通过。义务并集、新成员完整认证网络安装与历史资源变化后的推进仍待完成；四项总目标保持原范围。

## 2026-10-05：新成员认证交接基线网络下载

新成员的正文获取复用恢复分块帧与有界收集器，使用独立 handoff identity 签名 domain，先验证本地可信旧委员会/registry 锚点上的切换 quorum proof，再核对下一集合的请求身份。提供端读取唯一持久快照里的当前已安装不可变交接，要求认证 next set 等于活动集合；不需要恢复检查点或新成员的入站 BFT listener。长度前缀为 8 字节，分块偏移覆盖前缀，总正文上限 512 MiB，不接受短块制造无界请求。没有额外 wire tag、版本递增、业务真值或基线重算。

扩展原子安装回归，以真实 NodeRuntime 查询公开切换证明并跨帧获取正文，下载前提供端已经完成另一笔业务，安装后目标仍保留原认证基线而没有混入该业务。错误签名与 checkpoint 用途签名在交接入口均拒绝，安装继续 locked。复现将正文请求发往公开证明只读会话会连接丢失（target/handoff-network-proof-session-failure.log）；保持专用会话边界，查询后建立新正文连接，完整回归通过（target/handoff-network-regression.log）。此为网络 API 到原子安装链路；操作 CLI、安全签票重新准入及其他三项协议/稳定性缺口仍未完成。

本批 Windows/Linux fmt/check/clippy(-D warnings)、57 常规单元及上述独立安装回归通过，但完整 all-targets 均失败。Windows 主集成 244 通过、1 失败、1 忽略（114.08 秒，target/handoff-network-windows.log）：资源冲突仲裁任务 3101 未提交，四节点均连接另外三成员。失败四份快照冷读（target/handoff-network-conflict-cold.log）仍 pending、未取消；节点 2/4 停在 round 20，无锁或 prevote QC，节点 1/3 无该 scope BFT 状态。这不是迟后成功，也不能仅凭连接数量归为协议健康。Linux 主集成 242 通过、3 失败（62.15 秒，target/handoff-network-linux.log）：活动集合更新时发送 connection lost、相反顺序分配停滞、冻结来源私有拉取停滞。冻结来源任务 3202 的四份冷读（target/handoff-network-linux-frozen-cold.log）显示节点 1/2 pending、未取消、round 1/2 无锁或 QC，节点 3/4 无任务绑定/该 scope 状态；不推断三种失败有同一根因。完整失败日志和现场保留。两端未执行的 34 节点目标单独通过（Windows 42.88 秒、Linux 56.37 秒，target/handoff-network-windows-nodes.log、target/handoff-network-linux-nodes.log），不替代完整套件结果；git diff --check 通过。四项原目标仍未完成。

## 2026-10-05：精确认证历史冲突见证受理

冲突受理原先无条件要求原任务集合等于当前集合，已安装交接精确列入的历史正文仍在 V2 被 TaskOriginMismatch 拒绝。新增回归复现该错误（target/historical-contention-reproduction.log）。受理复用 persistence_validator_set 的原委员会/当前持久化规则，依旧验证精确继承候选、相同持久交接和活动/留存原集合；未列入的历史来源不获例外，实际正文、业务构造和 blocker 校验不变，不新增第二套来源验证。

回归先安装认证 V1 业务基线，再以 V2 提交无关业务，随后受理与另一继承任务冲突的精确旧正文，冷读保留 V2、无关业务、原 digest/version 和无资源见证。未列入的旧请求通过相同入口拒绝且 generation 不变。原 V1 quorum Abort 解除阻挡后，同一存活正文取得资源并由原 V1 Commit 证书完成，目标继续 locked、没有导入 vote locks（target/historical-contention-fixed.log）。这是历史冲突受理/存储推进回归，不证明业务前提已失效的历史正文、网络启动链或四项总目标完成。

本批 Windows fmt/check/clippy(-D warnings)、58 常规单元和历史冲突回归通过，完整主集成 243 通过、2 失败、1 忽略（107.76 秒，target/historical-contention-windows.log）：地址耗尽的并发分配场景 completed 超时，单一来源私有拉取场景未在 8 秒内取得每个节点的 CertifiedPreparedTask 事件。后者诊断已有部分节点最终证书，而四份确切失败快照的后续冷读全部 task 1300 succeeded=true（target/historical-contention-windows-pull-cold.log）；不能将该现场继续描述为未持久提交，尚须区分事件交付缺失与提交晚于测试期限。失败日志和现场保留，未扩大期限或修改测试等待条件。未执行的 Windows 34 节点目标单独通过（45.46 秒，target/historical-contention-windows-nodes.log）。Linux fmt/check/clippy(-D warnings)/all-targets 全部通过（target/historical-contention-linux.log），58 常规单元、独立历史冲突回归、245 主集成（63.81 秒）及 34 节点目标（53.25 秒）通过；git diff --check 通过。这些本机结果不消除既有间歇失败，四项总目标继续保持原范围。

## 2026-10-05：认证组件提交的完成事件

沿上一批 Windows 已持久成功但缺少 CertifiedPreparedTask 事件的现场检查，确认 recover_certified_component 在投票会话外原子提交后调用的完成记录恢复入口仅建立证书转发记录，不报告完成。扩展既有“证书先于受阻正文、Abort 后恢复”回归，精确复现正确证书事件数量为 0 而预期 1（target/component-completion-event-reproduction.log）。该遗漏是确认的独立路径，不据此断言上一批所有超时都由此造成。

运行期恢复现在以已验证 receipt 报告完成，并复用已有 recent_completed/会话 certified_emitted 去重；启动恢复保持静默。原子执行、依赖闭包、资源权利、证书验证和有界转发缓存均不变，无新持久标记或后台扫描。回归涵盖主候选有/无本地资源两种既有场景，精确证书事件一次、重复恢复不重复、冷启动恢复不重放事件。四项总目标仍需继续完成义务并集、操作入口、安全重新准入及其余历史/网络缺口。

本批双平台 fmt/check/clippy(-D warnings)、58 常规单元及认证基线独立回归通过。Windows 完整主集成 244 通过、1 失败、1 忽略（82.80 秒，target/component-completion-event-windows.log）：跨节点业务 CLI 提交脚本耗尽重试预算，未扩大预算或替换原请求；上一批单来源私有拉取和地址耗尽两个场景本轮通过，不据此断言其全部成因已解决。未执行的 Windows 34 节点目标单独通过（40.90 秒，target/component-completion-event-windows-nodes.log）。Linux 完整 all-targets 通过（target/component-completion-event-linux.log）：58 常规单元、独立基线回归、245 主集成（52.20 秒）及 34 节点目标（79.22 秒）。组件事件回归通过（target/component-completion-event-fixed.log），git diff --check 通过；失败日志和现场保留，整体稳定性与四项目标仍未完成。

## 2026-10-05：交接基线安装操作入口

后续交接义务收集工作见本文末尾，操作入口本身不表示分布式并集已完成。

`handoff-install` 使用本地可信旧委员会/registry anchor、下一集合 identity key 和目标 Validator config 的 AuthorizerSet，串联已有公开证明查询、认证正文下载和唯一原子安装器。目标使用既有 runtime 排他锁，非空目标拒绝；证明/身份验证由下载器在私有请求之前负责，入口不复制编码、校验或签票恢复逻辑。规范空 genesis 复用已关闭的查询 peer 对象执行下载器的本地重建分支，无第二次握手或正文请求。Validator config 的 authorizer/timeouts 解析抽为同一 load_parts，节点启动沿原 load 调用，不另建配置读者。

扩展既有 CLI admission/transition 回归：真实 keygen/admission 将成员 5 加入 7→8 集合，认证提供端包含完整账户基线及一个未完成开户请求。错误业务 authorizer 配置在网络下载后拒绝，目标仍无快照；正确配置经真实 handoff-install 安装，冷读保持 V8、原账户、继承绑定且不执行未完成请求，没有 PreparedTask 权利，safety=locked。重复安装拒绝且 generation 不变，随后真实 node-check 能加载新成员 5 且继续 locked（target/handoff-install-cli-regression.log）。本批不授予签票权，不证明多次切换追赶或其他三项缺口完成。

本批 Windows/Linux fmt/check/clippy(-D warnings)/all-targets 全部通过（target/handoff-install-windows.log、target/handoff-install-linux.log）：各 58 常规单元、独立认证基线回归、245 主集成及 34 节点目标通过。Windows 主集成 74.34 秒、节点 39.69 秒；Linux 主集成 49.58 秒、节点 55.21 秒，仅为本机验证耗时。git diff --check 通过。前批间歇失败日志和现场保留，不以单轮通过消除未知成因；签票前义务并集、完整历史变化受理及整体稳定性仍需继续完成。


## 2026-10-05：签票前交接并集的共享收集入口

扩展唯一 handoff admission 实现，增加 collect_transition_handoff：同样验证委员会、registry、前沿、正文根、业务基线、业务签名和 canonical plan，只在收集模式将已验证远端贡献与本地、继承义务规范合并。最终准入仍拒绝遗漏本地义务的根。收集仅原子保存绑定和无资源见证，不创建 governance/BFT 状态、不签票、不授予远端资源权利；已有本地权利和证明保留。

扩展原截止资格回归，构造提供方独有冻结选择与接收方独有开户任务，验证反向收集得到相同正文与根、重复收集 generation 不变、冷读保持本地权利且远端任务无权利。伪造已有候选的原请求、未经受理的过期请求均在写入前拒绝；严格最终准入遗漏拒绝仍成立。定向回归通过（target/handoff-union-admission-regression.log）。这是分布式收集所需的共享原子准入基础，自动交换、恢复及精确根签票屏障仍未实现，四项目标保持原范围。

收集回归进一步复现了封存后同一 TaskId 追加新冻结候选的漏洞：原 save_with_prepared 只检查新任务键，合并第二种 transfer selection 会被写入（target/handoff-union-sealed-variant-reproduction.log）。共享原子写入入口现同时关闭新任务和新候选，按请求/版本/完整 operations 判断，保留原候选阶段与权利推进，不重复计算摘要；检查与写入持同一锁。修复后同一回归拒绝该追加且 generation 不变（target/handoff-union-sealed-variant-fixed.log）。

本批最终 Windows fmt/check/clippy(-D warnings)/all-targets 全部通过（target/handoff-union-admission-final-windows.log）：58 常规单元、独立认证基线回归、245 主集成（93.38 秒）及 34 节点目标（52.35 秒）。Linux fmt/check/clippy、58 常规单元及认证基线回归通过，完整主集成 244 通过、1 失败（63.28 秒，target/handoff-union-admission-linux.log）：冻结来源私有拉取在观察期限内未取得全员完成事件，诊断包含 Transport(timed out)/connection lost。精确失败 PID 2241、任务 3202 的四份快照稍后原生 Linux 冷读均 succeeded=true、cancelled=false（target/handoff-union-admission-linux-frozen-cold.log），因此不能称为始终未提交，也不能据此认定超时根因消失。需继续区分提交延迟与事件可见性；原日志和现场保留。Linux 尚未执行的 34 节点目标单独通过（53.73 秒，target/handoff-union-admission-linux-nodes.log），不替代完整失败。Windows 探针读取 WSL UNC 路径失败的记录单独保留，最终冷读使用本批原生编译探针。git diff --check 通过。自动分布式并集、签票屏障、其余历史处理及稳定性仍待完成。

进一步抽取该失败诊断：节点 1/2/3/4 在超时诊断时均 succeeded=false、cancelled=false，而稍后四份冷读均成功。此现场至少确认了超过观察窗口的迟后提交，不能仅归为完成事件遗漏；发送超时/丢失如何导致延迟仍需查明。

## 2026-10-06：交接收集阶段的原子持久化与签票隔离

CollectingTransition 复用 pending governance 的 64 项上限与唯一成员源/正文编码，收集 root 与绑定、无资源见证沿原 prepared CAS 在同一快照写入；不新增平行日志或正文仓库。同一切换意图更新原记录，重复收集 generation 不变。普通新增准备在收集期间关闭，验证过的后续贡献可以扩大并集；已最终准入的 scope 不允许降级或追加贡献。CAS 保证先前正文覆盖校验仍成立，不在持久化层再次哈希完整业务基线。

收集记录不能产生 BFT target，resume_governance 跳过它；共同 governance subject 校验在注册和签票入口拒绝尚在收集的 scope，覆盖直接 ValidatorSigner 和 NodeRuntime 启动路径。分块正文携带严格阶段标记，冷提供端沿原分块入口输出；接收收集正文只验证、合并及原子保存，不启动切换共识。未知阶段标记在写入前拒绝，内部开发版本仍保持 1，无旧格式 reader 或兼容桥。

扩展既有并集回归证明三份贡献形成同一正文、只留一个当前收集记录、冷恢复后的注册/签票拒绝与普通新增准备关闭（target/handoff-collection-phase-regression.log）；新增正文阶段回归证明两节点独有开户义务在接收端合并，收集阶段和见证同时 generation+1 落盘，无 BFT/vote lock 或远端资源权利，未知标记 generation 不变（target/handoff-collection-transport-regression.log）。自动公告、重启后的交换、quorum 精确根确认与阶段提升，以及所有切换启动入口的收集接入仍未实现，四项目标继续保持原范围。

继续检查原始签票旁路，复现通用 sign_bft_prevote 可给当前收集根投票，切换专用入口的拒绝不足以覆盖该路径（target/handoff-collection-raw-vote-reproduction.log）。将阶段限制补入 lock_bft_prevote/lock_bft_precommit 共用的 value 校验，修复后该根在原子状态写入前拒绝，generation 不变；定向回归通过（target/handoff-collection-raw-vote-fixed.log）。

本批最终 Windows fmt/check/clippy(-D warnings)/all-targets 全部通过（target/handoff-collection-phase-final-windows.log）：59 常规单元、独立认证基线回归、245 主集成（98.87 秒）及 34 节点目标（42.84 秒）。Linux fmt/check/clippy、59 常规单元、认证基线回归与 245 主集成（47.14 秒）通过，但完整 all-targets 失败（target/handoff-collection-phase-linux.log）：34 节点目标 35.92 秒后仅建立 31/33 条认证连接。冻结来源三个主集成场景本轮通过，不因此宣称此前连接丢失、超时或迟后提交的成因已解决；本轮失败日志和私有现场保留。git diff --check 通过。自动贡献公告、quorum 根确认/阶段提升、所有启动入口接入，以及历史与稳定性缺口仍需继续实现，四项目标未完成。

## 2026-10-06：认证贡献自动交换与冷恢复公告

NodeRuntime.collect_validator_set_transition 启动同一收集准入与公告。紧凑 ValidatorSetTransitionSource 加入严格阶段字段，复用原 source 编码器与 exact-set BFT 认证通道；接收收集公告先持久化自身完整义务，再按需拉取远端正文。只有当前前沿的合法源可触发 fetch，避免无效前沿消耗正文拉取预算。新并集持久化成功才公告，重复受理不做回声传播；冷 resume_governance 重新公告已有收集记录。

待办使用原 source sync 的绝对重试期限，普通 BFT 的更早 timeout 不提前重传或重置期限；无相关待办不增加周期唤醒，不新增定时循环/历史正文副本。空 genesis 正文校验改为使用唯一 with_handoff 承诺判定，修复此前强行 Some(digest) 拒绝规范缺省承诺的问题，并去掉正文解码入口另行计算根的重复哈希；开发格式仍为 1。

扩展正文阶段回归，使用真实双向认证 QUIC：两节点各持独有开户任务，连接前公告丢失，冷恢复公告后沿真实 fetch/chunk 管道收敛到同一收集根，原节点保留本地权利而远端计划仍无权利，无 BFT/vote lock（target/handoff-collection-exchange-regression.log）。另验证规范空 genesis 正文可受理且保持收集与无签票状态（target/handoff-collection-empty-body-regression.log）。共用已有传输夹具，未增加同义 fixture。quorum 根确认/阶段提升、operator 与全部切换启动入口接入，以及其他历史/稳定性缺口仍未完成；四项目标不缩减。

本批 Windows/Linux fmt/check/clippy(-D warnings)/all-targets 全部通过（target/handoff-collection-exchange-windows.log、target/handoff-collection-exchange-linux.log）：各 60 常规单元、独立认证基线回归、245 主集成及 34 节点目标通过。Windows 主集成 82.99 秒、节点 42.09 秒；Linux 主集成 62.13 秒、节点 63.44 秒。仅为本机验证，既有失败日志和现场保留；单轮通过不证明间歇故障成因已消失。

## 2026-10-06：下载正文受理的快照一致性与有界 CAS 重试

install_fetched_prepared_task 的普通任务受理原先分别读取验证委员会、业务状态和 PreparedTaskBook。下载期间的并发提交可使这些读取跨越不同快照；StaleState/StalePreparedTasks 又被上层视为来源拒绝，导致已经下载的正文重新选择来源。定向回归通过真实 finalize/recover 提交以及保留提交前快照复现 StaleState 直接返回、没有在新业务条件上重新验证（target/fetched-source-cas-reproduction-error.log）。这证明本地受理失败路径，不把既有 QUIC Transport 错误全部归因于它。

受理从同一共享快照读取委员会、业务和准备记录，复用 from_tasks 与唯一准备/冲突验证器。只有两种 CAS 错误最多重读三次；每次重建候选并重验最新业务条件和精确历史资格。解码与业务签名验证在循环外，不重复下载、签名验算或增加后台循环；其他错误不重试。下载、认证冲突候选及资源释放后的重试调用均沿这个共同入口。将正文解码/受理职责从 runtime_tasks 主文件移至 source_admission，不复制另一套受理逻辑。

回归证明并发 finality 证据变化先被 CAS 拒绝且 generation 不变，重读后受理独立正文并保留原证书；业务提交后重读拒绝账户已存在的请求且不写入，同时仍能受理合法的独立迟到正文，冷读保留已完成账户和待决计划（target/fetched-source-cas-fixed-final.log）。交接根 quorum 确认/阶段提升、完整历史业务失效处理及间歇网络根因仍未完成，四项目标保持原范围。

本批两端 fmt/check/clippy(-D warnings) 通过，新增受理回归两端通过，但完整 all-targets 两端均失败。Windows 常规单元 60 通过、1 失败、1 忽略（23.60 秒，target/fetched-source-cas-windows.log）：认证基线网络回归在公开切换证明查询阶段返回 protocol request timed out，尚未进入正文下载。此前未执行的主集成单独运行，229 通过、16 失败、1 忽略（262.38 秒，target/fetched-source-cas-windows-integration.log），包括冻结来源、连接建立、分配、冲突仲裁与检查点推进超时。34 节点目标也失败（151.08 秒，target/fetched-source-cas-windows-nodes.log）：诊断时 33 个节点已提交，节点 2 未成功且无该任务 BFT 状态；其确切失败快照稍后冷读仍 succeeded=false、cancelled=false（target/fetched-source-cas-windows-large-node2-cold.log），不归为仅完成事件遗漏。

Linux 61 常规单元与独立认证基线回归通过；主集成 242 通过、3 失败（103.06 秒，target/fetched-source-cas-linux.log），均在四节点认证连接建立阶段超时，其中公开检查点场景四节点连接/失败事件均为空，成员切换场景尚未进入切换。不得据此认定是切换刷新或正文受理导致断连。此前未执行的 34 节点目标单独通过（51.45 秒，target/fetched-source-cas-linux-nodes.log），不替代主集成失败；两端 examples 目标通过。Linux 后续测试命令的退出状态汇总 shell 包装发生变量/数值错误，原始 cargo 日志均已有终态结果，以各目标日志为准，不以包装退出码声称整个检查成功。所有失败日志和私有快照保留，未延长期限、缩减场景或重跑替换失败；四项原目标尚未完成。

## 2026-10-06：证书先到、过期正文迟到的受理与直接提交

检查收集中的过期贡献时确认：单个节点自称截止前准备不能证明远端准入，现有 fail-closed 限制保留。另一个真实缺口是原委员会终态证书已经被验证、正文稍后到达时，普通任务仍返回 TaskExpired；分配正文已经接受同等证书资格。真实运行期先交付原集合完整证书、再受理同一签名正文的回归复现该失败（target/certified-source-expiry-reproduction-final.log）。

将既有地址分配的证书资格判断收敛为 has_certified_source，复用 pending_for 的唯一待办范围枚举。只有相同 scope/digest 且通过确切原集合验签的 finality certificate 才使普通任务保留时间资格；完整签名正文、冻结选择与当前业务/资源、历史资格仍沿唯一构造器验证。没有新增证明类型、正文副本、wire tag、版本演进、后台任务或平行真值。

初次修复使正文可执行，但回归进一步发现受理后仍进入普通 BFT 会话产生投票（target/certified-source-expiry-fixed.log）。已有终态证书的正常正文现沿 finalize_prepared_task 和既有认证依赖闭包直接持久提交，并复用原 receipt 完成事件、去重及有界证书回应；不启动新的投票会话。受阻见证仍沿已有仲裁保护/认证组件路径等待，不通过证书直接授予资源。

同一回归验证无证明及其他摘要的合法 quorum 均不能放开过期；正确证书先到时不写业务、绑定、资源或投票，正确正文后到时无需重送证书即可持久完成，发出一次确切完成事件，没有 Vote/FinalityVote、本地 BFT 状态或 vote lock。另在证书到达后通过真实准备/证书提交另一开户请求改变业务，迟到正文仍因账户已存在而拒绝且 generation 不变（target/certified-source-expiry-fixed-final.log）。这不使未证明的收集贡献获得期限例外，也不代表业务失效后的全部历史恢复、交接阶段提升或间歇网络根因已完成；四项原目标不缩减。

本批 Windows fmt/check/clippy(-D warnings)/all-targets 全部通过（target/certified-source-expiry-windows.log）：62 常规单元、独立认证基线回归、245 主集成（73.45 秒）及 34 节点目标（46.26 秒）。Linux fmt/check/clippy、62 常规单元和独立基线回归通过，但完整主集成 244 通过、1 失败（76.11 秒，target/certified-source-expiry-linux.log）。失败的 operator/public-checkpoint 场景实际停在 business task propagation：四节点均已安装 epoch 1 证明、各有三条认证连接，Task 1920 均未完成且 prepared_count=0；还没有发起第二次检查点发布。诊断有 SendFailed/ConnectionFailed 超时，不能称为迟到检查点证明漏补。

该 PID 2243 的四份确切失败快照随后原生 Linux 冷读仍 succeeded=false、cancelled=false（target/certified-source-expiry-linux-business-cold.log）；节点 1 的 allocation(scope start=1) 为 round 4，无锁或 prevote QC，节点 2/3 为 round 1、锁定同一 allocation digest 并各持 prevote QC，节点 4 无该 allocation BFT 状态。因此不将现场归为仅完成事件遗漏或迟后成功，来源传播与推进原因仍需定位。此前未执行的 Linux 34 节点目标单独失败（36.28 秒，target/certified-source-expiry-linux-nodes.log），只建立 32/33 条认证连接；两端 examples 目标通过。git diff --check 通过；所有既有失败日志和私有现场保留，不以本轮 Windows 通过消除未知网络根因。四项原目标继续保持。

### 2026-10-06：接收背压不能主动关闭认证会话

既有发送队列 Full 已保留连接，但 serve_inbound 仍把接收队列饱和转为 Transport 错误，listener 随即关闭健康 BFT peer。真实 QUIC 回归填满实际 inbound 容量 2048，再发送认证消息，旧实现立即终止会话（target/inbound-backpressure-reproduction.log）；因此这是另一个已证实的主动断连触发，不将此前所有超时归因于它。

共同网络接收入口改为等待异步消息回调。exact-set、身份及消息签名校验完成后，runtime 沿已有 Tokio 有界通道 send 等待容量，再唤醒共识；不扩大队列、不增加缓存或重试循环。读流任务仍限制为 8，会话关闭、校验失败和接收器关闭仍沿原错误边界退出。回归验证饱和时会话和 permit 保持，恢复实际 receiver 后原消息送达，第二条消息通过同一 peer 到达；关闭 receiver 时会话报错并释放 permit。接收与发送两项真实连接回归均通过（target/inbound-backpressure-fixed.log，0.35 秒）。四项原目标和未完成的交接阶段提升、历史业务变化处理仍保持。

本批 Windows 与原生 WSL Linux 的 fmt/check/clippy(-D warnings)/all-targets 全部通过（target/inbound-backpressure-windows.log、target/inbound-backpressure-linux.log）。两端各有 63 常规单元及独立认证基线回归通过；主集成各 245 通过（Windows 91.34 秒，Linux 53.21 秒），34 节点目标分别 43.77 秒、86.41 秒通过，examples 通过。背压修复无网络/快照格式变化。git diff --check 通过。此前超时失败日志和私有现场保留；这轮通过不能单独证明全部间歇根因消除，收集阶段提升与历史业务变化后的完整推进仍未完成，四项原目标继续。

### 2026-10-06：新成员真实连接上的历史终态推进

复查原认证业务基线安装测试时发现，它直接把旧证书注入 runtime，没有覆盖真实传输的新成员接收边界。沿相同认证基线夹具新增第二个旧待办，把合法旧委员会证书改由当前成员 1 的真实认证 QUIC 会话投递新成员 5；旧实现跳过收件者，测试在等待到达时失败（target/handoff-terminal-network-reproduction.log）。仅修投递后，历史正文已经本地准备的 Abort 被 Commit 分支提前返回，证书未被动应用（target/handoff-terminal-abort-reproduction-final.log；此前同名无 final 日志为模块可见性编译失败，保留）。再移除手工 Commit 正文准备，证书到达也无法从已安装正文自动恢复（target/handoff-terminal-body-reproduction.log）。三个失败阶段分别证明传输、终态分派和正文恢复缺口。

现有 authority 的 scope 表改为终态证明接收范围，从认证 handoff 与现有完成 relay 派生，复用单一原集合解析器，不增加持久证明表。普通投票、终态单票、私有正文和未知旧任务仍不能投递新成员；只有已认证继承或已有完成 relay 的 PreparedTask 完整 FinalityCertificate 可送当前成员。发送者和原 quorum 仍按确切原集合验证。普通来源、签票、治理及恢复权限不扩展，wire/snapshot 仍为当前版本 1。

继承终态在来源下载之前处理，Abort 先于 Commit 正文分支，沿唯一 install_prepared_abort 原子释放；本地已有正文不会阻断。对于缺少本地准备记录、但认证根已保存确切正文的新成员，原终态验签先于任何写入，原正文沿既有编码器及唯一来源受理路径重验当前业务与冻结选择，再 finalize 并进入认证依赖闭包。复用 receipt、完成事件与去重，不手工复制业务、不授予旧签票权，不重新下载已安装正文，也不绕过业务失效限制。

增强后的原安装回归通过（target/handoff-terminal-body-fixed-final.log，0.74 秒）：已先提交新委员会业务，旧请求此时过期；真实连接拒绝旧单票、私有正文和未知旧任务，准确交付并应用已准备的旧 Abort 和尚未准备的旧 Commit。冷读保留新业务、旧原摘要 receipt，Abort 没有业务效果，prepared/vote lock/BFT 状态为空，新成员仍保持 safety locked。运行期没有旧 Vote/FinalityVote 或来源待办，Commit 完成事件恰好一次。此前不完整的阶段提升、影响业务有效性的历史变化及间歇超时仍需完成，四项原目标不缩减。

本批 Windows fmt/check/clippy(-D warnings)/all-targets 全部通过（target/handoff-terminal-windows-final.log）：63 常规单元及独立认证基线回归、245 主集成（84.30 秒）、34 节点（49.45 秒）、examples。首轮 clippy 的可折叠 if 失败日志保留在 target/handoff-terminal-windows.log，已修正后完整执行全部检查，没有忽略 lint。

Linux fmt/check/clippy、63 常规单元与独立认证基线回归通过，主集成 241 通过、4 失败（54.83 秒，target/handoff-terminal-linux.log）。三个 runtime_bft 场景停在任务提交前的认证连接建立；checkpoint-connectivity 场景在 3 秒请求期限内未退出 Busy，之后观察到连接为 3/4 两个健康成员。这里不能把期限末尾观察到 quorum 等同于整个请求期限内始终可用，也不能归因于旧正文、源队列饱和或本轮终态语义。测试失败后出现的 endpoint-closed worker panic 是伴随输出，不据此认定原始连接失败原因。

此前未执行的 Linux 34 节点目标单独失败（33.78 秒，target/handoff-terminal-linux-nodes.log），只建立 32/33 条认证连接，尚未提交业务任务；examples 单独通过（target/handoff-terminal-linux-examples.log）。PID 2252 的 checkpoint-connectivity 四个确切失败基线通过原生 Linux 的既有 snapshot probe 冷读，均 safety_ready=true、minimum_signing_version=1、recovery_proof=false（target/handoff-terminal-linux-checkpoint-cold-final.log），不能归为已迟后获得证明。第一条冷读 shell 包装失败后检查了实际文件并修正命令，未修改失败快照。日志和私有现场保留，git diff --check 通过；阶段提升与剩余连接故障仍未完成，四项原目标保持。

### 2026-10-06：继承任务已有正文时恢复另一冻结候选

新增单一运行期回归验证原委员会两个冻结支付候选、新成员认证基线安装、新委员会后续业务、旧请求过期和另一候选终态到达。最初的 install_handoff_terminal 只检查 TaskId 存在，已有第一候选就跳过交接正文恢复，第二候选的合法原 quorum 没有被动推进（target/handoff-variant-reproduction.log，0.13 秒）。改为检查确切 candidate 后，原正文进入共同 admit_frozen_variant，却因该函数把旧验证集合用于当前快照写入而拒绝 ValidatorRegistryMismatch（target/handoff-variant-persistence-reproduction.log，0.09 秒）。这不是签名错误或来源未送达。

恢复入口按确切冻结摘要检查已安装的交接正文；缺失候选仍先验原证书，再沿唯一来源受理路径处理。共同变体持久化复用既有 persistence_validator_set，只有确切根内候选与原留存集合匹配才保持当前活动集合进行原子写入。验证、冻结选择、权利恢复、旧投票保护和终态提交均复用既有实现，不新增候选编码器、签名规则、正文仓库、后台循环或格式版本。

修复回归通过（target/handoff-variant-fixed-final.log）：真实认证基线含候选 1/2，新成员只有候选 1 的本地准备；先提交新集合独立业务后，原请求已过期，原集合候选 2 的完整终态可自动恢复并持久完成。冷读仅 currency 2 转给 bob，currency 1/3 仍归 alice；活动集合保持 2，原 receipt 摘要和 certificate 精确匹配候选 2，认证 handoff 字节不变，没有旧 Vote/FinalityVote、投票锁或本地 BFT 状态，完成事件一次，来源待办为空。另验证同一合法签名请求的未列入根的第三冻结选择不能写入，合法新集合 quorum 不能替代旧集合，伪签名拒绝，三种拒绝均 generation 不变。交接收集后的阶段提升和间歇连接故障仍待处理，四项总目标保持。

上一候选恢复批次的 Windows fmt/check/clippy 通过，主集成 244 通过、1 失败、1 忽略（81.64 秒，target/handoff-variant-windows.log），失败为委员会切换与发行共用前沿的 20 秒推进期限。失败的四个精确现场通过既有 snapshot-status 和 snapshot probe 冷读，均集合 2、generation 27、supply 1、frontier 2、Task 1900 succeeded=true（target/handoff-variant-windows-membership-cold.log）；只证明冷读时完成，不能证明期限内完成。单独补跑 Windows 34 节点（40.73 秒）及 examples 通过。Linux fmt/check/clippy、64 个库测试与 245 个主集成通过（46.97 秒），随后 34 节点失败（36.22 秒，target/handoff-variant-linux.log），只有 32/33 条认证连接且尚未提交业务。所有失败日志和确切私有现场保留。

### 2026-10-06：新成员历史回应不得关闭当前连接

扩展既有认证基线安装回归，在原成员 1 与新成员 5 间建立真实双向认证连接。空输入不产生旧证明回应；实际触发条件是新成员被动提交原集合任务后，通过原连接收到当前集合签名、同一 TaskId 的消息。完成缓存回应原集合证明，原发送队列只检查接收方，消息入队后完整身份校验发现本机 5 不属于原集合，发送工作任务遂关闭健康连接。正确触发回归实际复现 connected_validator_ids 从 [1] 变为空（target/handoff-relay-trigger-reproduction-final.log，0.87 秒）；早先空输入断言失败和错误版本的回归日志保留，不能作为此根因证据。

ValidatorBftPeer 的共同入队资格检查 can_send 同时检查来源集合内的本机身份与既有接收范围，广播和定向发送均复用它。不新增权威状态、签名规则、重试循环或队列，不改变真正发送与接收的完整签名校验。新成员保留历史完成 receipt 和确切事件，但没有旧集合发送权限。扩展回归通过（target/handoff-relay-fixed.log）：原成员仍可向新成员交付确切旧终态，旧投票、私有正文及未列入根的旧终态仍不能送达；新成员的旧证明回应被跳过，连接保持，当前集合消息无需重拨即可通过同一反向连接送达。

这定位了一个跨委员会连接关闭根因，不解释此前发生在业务提交前的 34 节点认证连接缺失。交接收集后的 quorum 就绪、安全封口与阶段提升，以及剩余稳定性和历史来源闭环仍需继续完成。

本批 Windows fmt/check/clippy(-D warnings)/all-targets 全部通过（target/handoff-relay-windows.log）：64 个库测试（包含实际认证基线与本批反向连接回归）、245 个主集成，主集成 75.07 秒；34 节点 48.06 秒，examples 通过。主集成的 Windows/Linux 混合网络目标因未设置 SECOND_WSL_BINARY 而按既有配置忽略，本批没有执行该目标。库测试中的独立子进程结果为忽略后显式调用的跨进程文件锁帮助目标，不额外计为认证基线用例。

Linux fmt/check/clippy、64 个库测试通过，主集成 244 通过、1 失败（75.89 秒，target/handoff-relay-linux.log）。失败的公开检查点发布场景停在 business task propagation：epoch 1 证明已安装，尚未发起第二次检查点；有 ConnectionFailed/SendFailed 与传输超时，没有 Rejected。原生 Linux 对 PID 2268 四个精确现场冷读（target/handoff-relay-linux-checkpoint-cold.log）确认 frontier=1、supply=0、任务 1920 未完成；节点 1 仍有分配 round 4、无锁及 prevote QC，不能归为只丢完成事件。此前未执行的 34 节点单独通过（53.31 秒，target/handoff-relay-linux-nodes.log），examples 单独通过（target/handoff-relay-linux-examples.log）；两项目标执行后的 shell 状态汇总变量被 PowerShell 展开，包装器报 unary operator expected，目标结果依据各自完整 Cargo 终态日志确认，不据包装器错误重跑。git diff --check 通过。本批跨委员会根因已修复，但该业务传播超时、其他间歇连接根因与交接阶段提升未完成，四项总目标继续。
### 2026-10-06：保留 BFT 接收失败的原始错误

上一轮 Linux 任务 1920 未完成，发送端只留下 timed out/reset by peer。共同 serve_inbound 把接收错误返回 listener，listener 只用固定原因关闭 peer，未保留错误，无法判断认证、编码、读流或接收器关闭的真实根因。扩展既有真实满队列回归，在接收器关闭后检查原始诊断，实际复现事件为空（target/bft-receive-error-reproduction.log，0.27 秒）。

共同 serve_inbound 复用现有 ConnectionFailed 通道，保留 peer 的已验证传输身份、实际地址、会话耗时及原始错误，再返回相同错误沿原路径关闭。QuicPeer 仅暴露底层现有 remote_address；使用本地已安装 quinn-0.11.12 源码确认该接口，当前没有可调用的 Context7 工具。不增加日志仓库、后台循环、持久化真值或重试，不放宽读流与签名边界。既有真实连接回归验证背压恢复后仍使用同一会话、关闭接收器才失败、permit 释放，且仅有一条确切原始 ConnectionFailed（target/bft-receive-error-fixed.log）。这补足定位证据，不宣称已修复业务传播或初始认证连接超时。
本批原生 Linux fmt/check/clippy(-D warnings)/all-targets 通过（target/bft-receive-error-linux.log）：64 个库测试、245 个主集成（45.19 秒）、34 节点（53.19 秒）及 examples。Windows fmt/check/clippy、64 个库测试、245 个主集成通过（74.13 秒），随后 34 节点在初始发现期限内只有 32/33 条认证连接（target/bft-receive-error-windows.log，30.89 秒）；该失败仍保留，不能因 Linux 通过而视为已解决。

34 节点目标的失败断言原先只报告连接数量。沿现有 wait_for_all 补失败时的初始/重启阶段、实际成员列表和既有运行期事件，正常路径与期限不变，无新测试和后台工作。为取得更直接错误证据而运行修改后的 Windows 目标，结果通过（target/bft-discovery-diagnostic-windows.log，38.78 秒）；这次通过不能替换之前失败。修改后的 Windows fmt/check/clippy 及此前未执行的 examples 通过（target/bft-discovery-diagnostic-checks.log、target/bft-receive-error-windows-examples.log）。交接阶段提升与剩余连接根因仍未完成。
修改后的 Linux fmt/check/clippy 与 34 节点目标通过（target/bft-discovery-diagnostic-linux.log，54.04 秒），两端新诊断均已编译验证；Windows 原始 34 节点失败不会因后续目标通过而改记为成功。所有失败日志与私有现场保留，git diff --check 通过，四项总目标继续。
### 2026-10-06：迟到终态推进期间原子刷新收集根

核对交接收集后进入切换的路径，发现 CollectingTransition 在旧终态提交后仍保留旧业务基线，后续 covers 返回 StaleState；共同快照写入还会因货币前沿推进把该收集记录整体过滤掉。单一回归以两项真实准备的开户任务、事先合法的原委员会终态/分配证书和当前收集意图复现旧基线失效（target/collection-progress-reproduction.log，0.27 秒）。

在唯一 write_next_unlocked 路径、原子编码写入之前更新当前委员会的无投票收集记录，再按既有规则清理其他过期元数据。只有状态或准备集合改变才更新，纯投票元数据不重新捕获。下一委员会、准入和轮换内容继续沿既有构造/验证器保留；新前沿和业务基线由同一待提交状态导出，不单独提交第二次快照。TaskHandoff.capture 抽出共享引用入口，避免复制整份持久状态；来源受理已捕获的新根直接复用，不再次编码基线和见证。已进入共识的 Transition、终态证书和原不可逆投票锁不重写，不引入就绪投票、新签名规则、格式版本或后台循环。

最终回归通过（target/collection-progress-lock-fixed-final.log）：真实 precommit QC 放行本地最终签票，随后启动收集；迟到业务终态提交后冷读基线覆盖当前业务，仅保留另一未完成任务，并精确保留原 receipt；迟到认证分配把前沿从 1 推到 2，收集意图及下一委员会保留，剩余任务的本地准备权和原不可逆投票锁保持。收集记录没有 target、没有本地切换 BFT 状态，直接准入切换仍因 CollectionIncomplete 拒绝且 generation 不变，重复分配证书无重写。既有远端候选并集、权限、过期与原子受理回归也通过（target/collection-progress-union-regression.log）。中间补签票检查遗漏 precommit QC 的 BftFinalityNotReady 和一个夹具类型名称编译失败日志保留，修正夹具后通过，不削弱签票条件。

本修复补交接推进的一项真实前置缺口，收集后的 quorum 就绪、安全封口和自动进入切换仍未完成，连接间歇故障仍保持原失败证据，四项总目标继续。

本批完整验证：Windows fmt/check/clippy(-D warnings)/all-targets 通过（target/collection-progress-windows.log），65 个库测试、245 个主集成及 34 节点目标通过。Linux fmt/check/clippy 与库测试通过，主集成 244 通过、1 失败（target/collection-progress-linux.log）；private-task-pull 在任务完成后收到迟到正文并报告 InvalidPreparedTaskSource。保留的四节点现场冷读均已成功、supply=1、frontier=2（target/collection-progress-linux-source-cold.log）。后续单独执行的 34 节点和 examples 测试通过；包装脚本退出状态处理失败，不将其当作完整门禁通过。

### 2026-10-06：已完成任务的确切迟到正文幂等受理

上述失败后的共同来源入口回归直接复现：任务已成功提交，再安装相同签名请求、原委员会、摘要和冻结选择的正文，被 AlreadySucceeded 分支错误拒绝（target/completed-source-reproduction.log）。该分支现复用持久 receipt 与原正文解码，验证原集合版本、完整签名请求、计划摘要和确切冻结选择；全部匹配才无写入返回成功。正文仍先经过共同解码和授权签名验证，缺失 receipt、错误集合、摘要或选择继续拒绝，不重新准备、不授予资源或旧投票权，不重复事件。

既有认证提交回归验证两种资源持有方式下的重复正文、篡改选择和错误摘要，并检查 generation 与完成事件不变（target/completed-source-fixed.log）。既有历史候选交接回归另验证活动集合已切换、原请求过期后相同 receipt 正文仍幂等受理，当前集合不能冒充来源集合且无额外提交（target/completed-source-historical-fixed.log）。这不是抓包结论；失败现场、共同入口的直接复现及回归分别保留。当前组合源码的两端完整门禁尚待执行，成员切换闭环及其他稳定性问题仍未完成。

本批最终验证：Windows fmt/check/clippy(-D warnings)/all-targets 全部通过（target/completed-source-windows.log）：65 个库测试、245 个主集成（86.33 秒）、34 节点目标（43.16 秒）和 examples；混合 WSL 特定目标保持既有忽略。Linux fmt/check/clippy 与 65 个库测试通过，主集成 244 通过、1 失败（69.36 秒，target/completed-source-linux.log），private-task-pull 本次通过。失败是 cli_network_init 的真实 CLI 场景，在 V3 成员切换、提供端重启及恢复节点再次重启后提交 recovery-checkpoint 返回 protocol request timed out，不是 Busy。精确现场 PID 2250、root second-cli-network-init-2250-1791281652700487161-19 保留；冷读三个提供端均集合 3、generation 50、recovery_serial=1，恢复节点集合 3、generation 8、safety=ready、recovery_serial=1（target/completed-source-linux-cli-cold.log）。只证明冷读时检查点已经推进，不证明请求或响应在期限内完成；具体超时位置仍待诊断，不能宣称修复。随后单独执行未运行的 Linux 34 节点（53.25 秒）及 examples 均通过，两条命令实际退出 0，无包装脚本状态错误。全部运行句柄已终止，原失败日志与私有现场保留；当前代码的两端完整门禁不能记为全绿。

### 2026-10-06：保留请求超时的实际阶段

上一轮恢复检查点 CLI 错误发生在 checkpoint submission，而非 failed to connect，说明客户端已完成连接认证，但原共同 timeout 文本不能区分开流、写入与响应等待。QUIC 共用 deadline 实现保留阶段文本，exchange 三步复用同一绝对期限，仍为五秒总预算；其他读写与认证使用各自原预算。未增加重试、放宽权限、队列或后台日志。

新增真实 QUIC 回归，服务端完成认证并实际读到 Ping 后保留请求、不回复，客户端必须报告 awaiting response；不是仅验证字符串构造。旧代码实际失败并仅给出 protocol request timed out（target/request-stage-reproduction.log，5.02 秒）。该检查证明可以区分已交付请求的响应超时，不能据此宣称上一轮间歇恢复请求的根因已修复。

第一版为 exchange 添加阶段期限但仍嵌套普通读取的独立 timeout，直接回归与首轮 Windows 完整测试均报告 reading framed message 而非等待响应（target/request-stage-fixed.log、target/request-stage-windows.log；完整测试 245 通过、1 失败、1 忽略）。修正后读取沿同一个 read_stream_message_at 共享实现传入请求绝对期限及阶段，不竞争两层计时器；普通入站读取仍保留原五秒读期限。第一版日志保留，不将失败归因于原间歇故障。

修正后的直接真实 QUIC 回归通过（target/request-stage-fixed-final.log，5.14 秒），已送达但不回复的请求明确报告等待响应超时。最终当前源码 Windows/Linux fmt/check/clippy(-D warnings)/all-targets 全部通过（target/request-stage-final-windows.log、target/request-stage-linux.log）：两端 65 个库测试、246 个主集成及 34 节点目标、examples；Windows 主集成 76.29 秒、34 节点 42.81 秒，Linux 主集成 47.12 秒。原恢复 CLI 超时本次未复现，不能以完整门禁通过替代其根因定位；原失败日志和私有现场保留。所有运行句柄已结束，交接收集后的 quorum 就绪、安全封存、自动推进与剩余稳定性排查继续。

### 2026-10-06：交接收集复用同一快照与有界 CAS 重试

收集入口先取 current 状态、再通过 PreparedTaskBook::new 独立取任务集合，两次读取之间的认证终态或冻结候选写入会产生不一致输入。运行期共同 collect_transition 现在复用 from_tasks，从同一快照恢复准备集合；只有 StaleState/StalePreparedTasks 最多重读三次，其他验证错误不重试。每次重试仍沿唯一正文、基线、权限和并集验证器，不扩大旧任务资格或来源传播。

现有 chunked_collection_retains_its_phase_and_cannot_start_membership_voting 回归通过（target/collection-snapshot-regression.log），继续验证远端独有任务无权见证、持久收集阶段、重启恢复、自动合并和禁止提前投票。本项不证明 quorum 就绪、安全封存或自动切换已实现。审查现有封存路径确认：Transition 写入后禁止收集扩大，因此不能采用各节点独立回应一个根后立即封存的方式；不同根的局部封存可能永久阻断并集传播。阶段提升仍需与现有 BFT 锁定和根外旧任务签票边界一起闭合，不得绕过 CollectionIncomplete。

本批 Windows/Linux fmt/check/clippy(-D warnings)/all-targets 全部通过（target/collection-snapshot-windows.log、target/collection-snapshot-linux.log），两端 65 个库测试、246 个主集成、34 节点目标与 examples 通过；Windows 主集成 69.20 秒、34 节点 40.25 秒，Linux 主集成 47.26 秒。git diff --check 通过，所有运行句柄已结束。此批改善并集收集在状态并发推进时的受理，尚不构成成员切换自动闭环或剩余间歇故障的根因证明，原失败现场保持。

### 2026-10-06：认证前沿推进后淘汰过期分配来源待办

扩展原过期分配来源回归，先建立当前前沿的拉取待办，验证未提交时不得清理；完整原证书与正文提交后前沿 1→2，再处理输入仍留有待办，实际复现（target/allocation-sync-reproduction.log，0.25 秒）。只在完成 PreparedTask 时清理同步，遗漏了 CurrencyAllocation 与同 scope 成员切换，是该待办持续唤醒、重试并占用拉取名额的直接原因；不将其当作所有连接故障的解释。

同步层复用单一 superseded_allocation_scope 判断，同时淘汰 fetch/announcement。输入处理从当前共享快照清理，认证分配或委员会推进后在等待前再次清理；过期输入的公告、分块与 unavailable 不再重建下载，投票和完整证明仍送往既有共识验证，旧来源请求仍可服务落后成员。仅版本更旧或同版本 start 更小才淘汰，未知未来范围不由此推断为终态。普通历史 PreparedTask 完全保留，无新增终态证据、写盘、后台循环或连接重拨。

原真实分配回归修复后通过（target/allocation-sync-fixed.log），验证已提交待办消失、晚到不同摘要公告不能复活且 generation 不变。既有来源乱序回归扩展同时检查旧委员会/旧前沿的 fetch 与 announcement 清理，而当前、未来前沿、未来委员会与原 PreparedTask 保留；另有既有沉默来源切换回归通过（target/allocation-sync-boundaries.log）。成员切换的就绪、安全封存和自动推进仍未完成，原间歇传输失败仍保留日志与现场。

本批验证：Windows fmt/check/clippy(-D warnings)/all-targets 全部通过（target/allocation-sync-windows.log）：65 个库测试、246 个主集成（67.74 秒）、34 节点（44.72 秒）及 examples。Linux fmt/check/clippy 与 65 个库测试通过，主集成 245 通过、1 失败（64.52 秒，target/allocation-sync-linux.log）：cli_network_init 的 V3 恢复节点已解锁后提交发行 Task 9201（t000000000000000000000000000023f1），客户端返回 protocol request timed out while awaiting response，具体超时阶段得以确认。精确失败现场 PID 2249、root second-cli-network-init-2249-1791283362684954250-19 保留；冷读恢复节点 generation=8、set=3、safety=ready、frontier=4、supply=3，任务 succeeded=Some(false)，三个提供端 generation=47、set=3、frontier=4、supply=3，任务 succeeded=None，无该前沿 BFT 状态输出（target/allocation-sync-linux-cli-cold.log）。这证明恢复节点持久受理后尚未推进分配，不能归为完成后丢 ACK，也不证明卡在注册还是网络响应；待继续定位。单独补跑未执行的 Linux 34 节点（53.53 秒）及 examples 均退出 0。全部句柄已终止，原失败日志与私有现场保留；两端完整门禁不能记为全绿。

### 2026-10-06：持续入站不能扩大单次共识批次

排查恢复后已受理任务的响应等待超时时，检查入站到共识锁的路径。drain_inbound 原先一直 try_recv 到队列空；持续生产者在读取时补充消息，批次 Vec 没有队列容量约束。真实 mpsc 并发生产回归实际读出单批 6369 条，而现有通道容量为 2048（target/intake-batch-reproduction.log，0.05 秒）。这证明批次内存与随后共识锁占用工作量无界，不证明它就是前一 CLI 超时的实际现场根因。

消费复用通道的初始 len，仅取本轮已排队数量，并据此预留 Vec；新增消息保留在唯一原通道，原认证接收回调的 Notify 负责下一轮。容量、认证、签名、原定时期限和消息处理规则不变，没有新批次配置、线程、队列或空转重试。并发回归完整收到初始及并发的 67584 条消息，每批不超过现有容量；既有真实 QUIC 入站满队列后同连接恢复与关闭原错误诊断、普通/终态出站背压测试也通过（target/intake-batch-fixed.log）。两端完整门禁随后执行，交接阶段提升与原间歇超时继续保持未完成状态。

本批 Windows/Linux fmt/check/clippy(-D warnings)/all-targets 全部通过（target/intake-batch-windows.log、target/intake-batch-linux.log）：两端 66 个库测试、246 个主集成、34 节点目标和 examples；Windows 主集成 94.37 秒、34 节点 55.71 秒，Linux 主集成 47.04 秒。原 CLI 超时本次未复现，不能把这一轮通过当作已确认的关联根因。所有运行句柄已结束，git diff --check 通过，原失败日志和私有现场保持；交接就绪、封存、自动切换及间歇响应等待问题继续。

### 2026-10-06：存储锁等待不得阻塞外部提交的网络线程

原服务端在异步网络任务内直接调用同步 context.submit，包含验签、存储文件锁和共识注册。新增真实 QUIC 回归采用单个 Tokio 执行线程和另一个持有同一存储文件锁的线程；业务完整正文沿正式服务入口提交，同时独立认证连接发送 Ping。原实现让 Ping 等到两秒存储锁释放后才处理，实际失败（target/submission-blocking-reproduction-final.log，2.05 秒）。早前夹具引用不存在 helper 的编译日志单独保留（target/submission-blocking-reproduction.log），不是此行为证据。

共同业务提交服务入口以 spawn_blocking 执行原同步 submit，等待结果后才发原响应，不复制校验/注册实现。现有 listener 的提交 permit 直接移入事务闭包，继续持有至同步事务返回；网络取消不能让待锁后台事务脱离原并发上限。连接、正文分块和五秒请求/三十秒会话期限保持原约束，没有额外重传、队列、服务循环或自行增加网络线程。

修复回归通过（target/submission-blocking-fixed.log、target/submission-blocking-fixed-final.log）：存储锁仍被持有且提交名额为 1 时，独立 Ping 已成功返回；释放锁后原 TaskId 获得 Prepared 响应并在冷读准备集合中存在，事务完成后名额归零。本项证明外部提交的锁等待不再阻塞所在网络执行线程；不证明之前 Linux CLI 超时现场必然由此导致，也不声称全部运行期同步路径或成员切换闭环已完成。

本批 Windows/Linux fmt/check/clippy(-D warnings)/all-targets 全部通过（target/submission-blocking-windows.log、target/submission-blocking-linux.log）：两端 67 个库测试、246 个主集成、34 节点目标与 examples；Windows 主集成 71.40 秒、34 节点 46.76 秒，Linux 主集成 66.90 秒。git diff --check 通过，全部运行句柄已结束。原间歇 CLI 响应超时本次未复现，原失败日志与现场保留；只能确认此次受控存储等待对提交网络线程的阻塞已修复，不能替代治理/共识循环其他同步路径排查或交接阶段提升验收。

### 2026-10-06：治理存储等待与网络调度分离

复用上一轮真实 QUIC 存储锁回归，同时提交合法恢复检查点请求、业务请求和独立 Ping。治理入口仍在网络任务内同步刷新权威并读取存储，阻塞了独立请求；真实复现失败（target/governance-blocking-reproduction.log，2.07 秒）。共同 requests 入口以 spawn_blocking 执行原治理响应计算，三个动作复用同一逻辑；原 connection permit 随请求移入任务并随结果返回，保留至响应结束，网络取消不使存储等待脱离原连接上限。请求与签名正文直接移动，不增加正文复制、队列、后台循环或超时预算。

回归先因夹具仍断言仅一个 permit 而失败，实际两类请求均持有名额（target/governance-blocking-fixed.log）；修正为共享请求计数后通过（target/governance-blocking-fixed-final.log、target/governance-blocking-modules.log）。存储锁仍持有时 Ping 已返回，两个请求均保持名额；释放锁后业务 Prepared 正确持久化，合法恢复请求在未连接 quorum 的条件下仍 Busy，响应结束后计数归零。治理请求按实际职责移至 runtime_governance/requests.rs，父模块保留原入口导出，来源验证和共识逻辑保持原实现。此项不证明前一 Linux 间歇超时关联，也不完成交接就绪、安全封存与自动切换。

本批 Windows/Linux fmt/check/clippy(-D warnings)/all-targets 全部通过（target/governance-blocking-windows.log、target/governance-blocking-linux.log）：两端 67 个库测试、246 个主集成、34 节点目标和 examples；Windows 主集成 82.66 秒、34 节点 44.43 秒，Linux 主集成 47.20 秒。git diff --check 通过，所有运行句柄已结束。原间歇超时本次未复现，失败日志与私有现场仍保留；本批验证不替代运行期其他同步路径排查，也不完成交接就绪、安全封存或自动切换。

### 2026-10-06：交接并集容量与未注册消息窗口分离

新增回归使用两个实际持久任务簿：提供端 33 项独有任务、接收端 1 项本地任务。旧代码实际返回 Persistence(SnapshotTooLarge)（target/handoff-capacity-reproduction-behavior.log）；仅去除受理层限制后仍失败（target/handoff-capacity-fixed.log），定位到快照编码与冷读共同调用的 validate_prepared_finality_proofs 也错误套用 32 scope 窗口。共同校验移除此项数量限制，终态证书、成员、状态绑定、候选和权利校验保持；普通冲突来源准入及未注册消息队列上限不变。

修复后两项交接受理回归通过（target/handoff-capacity-fixed-final.log）：34 项并集完成持久化与冷读，本地提交权保留、所有远端任务为无权见证；交接覆盖冷读集合，持久阶段仍为 CollectingTransition、没有 BFT 投票状态，重复收集不增加 generation。总容量沿既有规范交接正文及快照字节上限，不新增格式、配置或另一份见证状态。本项不完成 quorum 就绪、安全封存或自动切换；两端完整门禁另行执行。

本批 Windows/Linux fmt/check/clippy(-D warnings)/all-targets 全部退出 0（target/handoff-capacity-windows.log、target/handoff-capacity-linux.log）：两端 68 个库测试、246 个主集成、34 节点目标和 examples；Windows 主集成 68.28 秒、34 节点 51.40 秒，Linux 主集成 49.77 秒。所有运行句柄已结束。原间歇 CLI 超时本次未复现，失败日志和现场保留；交接 quorum 就绪、安全封存与自动切换仍未完成。git diff --check 在文档同步后检查。

### 2026-10-06：生产节点共识存储等待与网络轮询隔离

原恢复 CLI 场景在同一源码的原生 WSL 单独连续 8 次通过（原生日志 target/handoff-cli-stress.log），未复现原间歇超时。检查真实 NodeRuntime::run 发现监听、共识、发现及公开同步作为同一 select 任务的分支，不能把“分支”当成独立执行线程。新增生产入口回归：实际 NodeRuntime、真实 QUIC 认证与 Ping，外部线程持有真实 snapshot 文件锁两秒。旧代码在双执行线程上仍失败（target/listener-scheduling-reproduction.log，2.05 秒）；仅创建独立监听任务仍失败（target/listener-scheduling-fixed.log，2.05 秒），因此没有保留该不充分方案。

共同共识 runner 现在在既有阻塞线程池中按顺序执行启动恢复、每轮同步工作及到期重试；监督只等待结果、原 Notify 和原绝对期限，协调器 deadline 在该轮同步工作内取得，不把 mutex 等待留在网络 future。发现流程的同步 authority refresh 也移入同一既有线程池。run 使用 Arc<Self>，现有调用除 CLI 已为 Arc，CLI 包装唯一 runtime 即可；不克隆业务状态、endpoint 或新增配置、通道、永久线程与轮询。主体按运行调度职责移到 runtime_bft_consensus/runner.rs，不继续扩大共识父文件。任务失败向监督显式返回 RuntimeTaskFailed；每轮工作 await 后才进入下一轮，取消不启动后续轮次，在途原子事务仍可以完成。

原双执行线程回归修复后通过（target/listener-scheduling-fixed-final.log）；同一回归加强为单 Tokio 执行线程也通过（target/listener-scheduling-single-thread.log），持锁时生产监听器已完成认证并返回 Ping。解除锁后取消并等待 run 结束，新连接不能再获得应用认证，验证没有脱离监督的监听任务。该回归证明真实节点入口的受控存储等待不再阻塞监听轮询，不证明原历史 CLI 超时必由此造成；成员切换 quorum 就绪、安全封存、自动推进和其他同步路径仍待闭合。

首次本批 Windows 完整门禁未通过（target/listener-scheduling-windows.log）：主集成 242 通过、4 失败、1 忽略，83.51 秒；失败涉及 exhausted_competing_allocation、opposite_submission、allocation backlog 及 business CLI retry budget，不能记为全绿。单独重跑现有 allocation 六项全部通过（target/listener-scheduling-allocation-targeted.log，34.39 秒），另一个 exhausted 直接回归通过（target/listener-scheduling-exhausted.log）。保留 PID 34556 的原失败日志及私有现场；冷读 opposite-allocation-1 generation=27、frontier=6、supply=2，Task 1801 仍 Pending 且没有该 task 的 BFT 状态输出；backlog generation=639、frontier=49、supply=47，只能证明期限内未全部推进，不据此断言负载或某个竞态是这四项的唯一根因。

继续审查认证分配恢复发现确定的竞态：resume_currency_allocations 从 current 状态恢复业务，却通过 PreparedTaskBook::new 再读取另一份准备集合；任何 prepare 错误都清除 allocation_task，包括 StaleState/StalePreparedTasks。新增真实持久回归在取得认证分配快照之后准备另一个任务，再沿恢复共同入口提交旧快照；旧代码实际丢掉已认证分配的正文而没有业务准备（target/allocated-resume-stale-reproduction-behavior.log，0.18 秒；先前 reproduction.log 仅为测试 getter 名称编译错误，不是行为证据）。

恢复共同入口复用 LegalTaskSubmissionContext::submit_verified 的同快照 from_tasks 与最多三次 CAS 重读，不另造执行/占用逻辑；原候选签名、已认证区间、请求绑定与过期规则保持。仅在业务准备及共识注册成功、或确定的 Preparation 业务拒绝后清除正文；持久化和注册错误继续返回且保留正文，不能把临时 stale 当作永久失败清除。回归修复后通过（target/allocated-resume-stale-fixed.log），冷读保留并发任务和新准备任务、原分配证明与地址区间，未执行业务、不再分配新区间，共识已登记确切准备摘要。这不证明前述四个完整门禁失败已全部解决；当前源码两端完整门禁继续执行。

随后 Windows 门禁仍出现两项分配收敛超时（target/listener-scheduling-final-windows.log，主集成 244 通过、2 失败、1 忽略，71.62 秒），不能用直接 stale 回归通过宣称所有负载下的超时根因已查明。共同恢复入口进一步保证所有 PreparationError::Persistence 都保留认证正文，而非只保留两种 stale；磁盘错误或准入封闭也不是永久业务拒绝。

当前源码最终 Windows/Linux fmt/check/clippy(-D warnings)/all-targets 全部退出 0（target/listener-scheduling-current-windows.log、target/listener-scheduling-linux.log）：两端 70 个库测试、246 个主集成、34 节点目标与 examples。Windows 主集成 76.63 秒、34 节点 39.55 秒，Linux 主集成 47.77 秒；原失败日志、私有现场及受控复现保留，全部过程和工具句柄已终止。此前完整并行门禁失败和原间歇 CLI 超时的关联仍未证明，本轮通过不替代该根因排查，也不完成交接 quorum 就绪、安全封存与自动切换。文档同步后执行 git diff --check。

### 2026-10-06：状态查询存储等待不再阻塞 QUIC

扩展现有真实 QUIC 单执行线程回归，在业务提交与治理请求之外并发查询同一签名任务状态。旧共同查询入口同步 load 快照；外部线程持真实 snapshot 文件锁两秒时，独立 Ping 被阻塞至解锁（target/status-storage-reproduction-behavior.log，2.06 秒，实际行为断言失败）。首次 reproduction.log 先触发了等待请求数断言，不用它替代明确的网络阻塞证据。失败日志与现场保留。

共同 serve_legal_task_status_from_request 现在在既有阻塞线程池读取并判定状态，复用 load_shared，避免查询复制全量状态；原活动连接 permit 随请求移入任务，并保持至响应发送结束。沿既有摘要绑定与阶段规则返回状态，无第二份缓存、额外并发配额、定时器或延长期限。相同回归通过（target/status-storage-fixed.log，2.14 秒）：持锁期间独立 Ping 返回、三类存储请求均仍占用名额，解锁后查询及提交完成，名额全部释放，业务准备可冷读。此证据不证明原历史冻结来源超时的全部根因已闭合，成员切换自动推进也仍未完成；完整两端门禁随后执行。

本轮核对的交接开发缺口：GovernanceContext::start_transition 及直接共识入口仍可从本地根开始 Transition，CollectingTransition 的 target 为 None 且共同 governance_subject 禁止它进入签票，目前没有安全的自动阶段推进。TaskHandoff::capture_parts 只捕获 prepared 和继承冻结计划；business_baseline 去掉未分配的 Pending 绑定，并清空保留绑定的 allocation_task。故已受理、未准备的请求正文没有完整交接载体，不能宣称自动汇集了所有未完成任务。另有冻结候选数量仍在 merge_witness、快照共同校验和 decoder 中套用未注册消息窗口，尚未处理。必须一并解决这些明确路径及 quorum 选定根的安全/活性问题；业务接入和独立主机条件不列为这些开发缺口的完成要求。

状态查询修复后的 Windows 完整门禁退出 0（target/status-storage-windows.log，70 个库测试、246 个主集成、34 节点和 examples，主集成 58.72 秒、34 节点 35.87 秒）。同源码 WSL 门禁退出 101（target/status-storage-linux.log）：库和 246 主集成通过，34 节点恢复阶段仅连接 31/33，缺少 ValidatorId 33、34，34.38 秒失败，examples 尚未执行。PID 6247 的全部私有现场保留；原缓存 second-large-validator-discovery-6247-1791299594372021697-0.peers 只有 31 条记录，不将此轮记为两端通过。

### 2026-10-06：认证拨号公布前持久化恢复端点

实际发现流程先在 ValidatorBftRuntime::dial 的 outbound 表公布认证连接并启动发送任务，再由上层 JoinSet 收到拨号结果后记录 PeerStore。34 节点测试在连接数满足时立即取消原生产运行入口，尚未记录的最后几次拨号可以被取消；现场的 31 条缓存与该路径一致，但不据此断言所有历史来源超时属于同一原因。新增实际双节点 QUIC 故障回归，将本测试的 .peers.new 路径设为目录迫使原缓存写入失败：旧代码返回错误却已公布连接（target/discovery-cache-reproduction.log，0.05 秒）。

共同 dial 把唯一缓存写入提前到公布连接之前，在现有阻塞线程池执行并持有原连接名额，发现循环和测试拨号入口不再重复写缓存。首次修复回归又实际发现 PeerStore::record 先更新内存、随后写盘失败；再次相同记录命中 last-entry 无写入返回，冷读仍为空（target/discovery-cache-fixed.log，0.06 秒）。因此共同缓存的三类更新均改为持原 mutex 构造有界候选、唯一原子写入成功后才替换内存；写入失败不能污染内存去重依据，未改变容量、格式或权限规则，角色不变时不复制候选。

最终直接回归通过（target/discovery-cache-fixed-final.log）：失败不公布连接，移除本测试故障后重新完成真实身份握手及写入，连接准确为 ValidatorId 2，独立 PeerStore 冷读恢复相同端点；成功清理只涉及本测试自建目录和文件。当前最终源码完整 Windows/WSL 门禁随后执行，旧失败日志及现场不删除。

当前最终源码两端 fmt/check/clippy(-D warnings) 和 71 个库测试通过，但完整门禁没有通过。Windows 主集成 243 通过、3 失败、1 忽略（target/discovery-cache-windows.log，81.82 秒）：exhausted、opposite allocation 及 65 项 backlog 到期未收敛。PID 35664 全部失败现场保留；正确的带 t 前缀完整 TaskId 冷读见 target/discovery-cache-failure-cold.log：普通任务 1922 已完成，四个 opposite 节点均已完成任务 1801、frontier=6、supply=5；backlog frontier=48、supply=46、generation=627，不能宣称 65 项都完成。冷读只证明稍后持久结果，不证明期限内完成或具体耗时根因。补跑未执行的 Windows 34 节点和 examples 退出 0（target/discovery-cache-windows-remaining.log，34 节点 38.23 秒），六项 allocation 定向测试全部通过（target/discovery-cache-allocation-targeted.log，36.13 秒），不替代此前全量失败。

同源码 WSL 主集成 245 通过、1 失败（target/discovery-cache-linux.log，68.61 秒）：真实 CLI 在向 validator-2 请求恢复检查点时再次 Transport("protocol request timed out while awaiting response")。PID 2272 的 second-cli-network-init-2272-1791300285884357419-19.network 保留；实际冷读见 target/discovery-cache-linux-cli-cold.log，四节点仍为集合 1，节点 2/3/4 recovery_serial=1、generation=23，节点 1 serial=none、generation=14。这是原集合的检查点请求阶段，不能误记成成员轮换后的重入失败；已有检查点持久结果也不能证明响应在五秒内送达。首次含 shell 循环变量的探测参数被剥离，结果为错误路径；日志已用四个确切绝对路径重新取得，未把无快照错误当作冷读证据。

补跑 WSL 的 34 节点恢复及业务目标和 examples 退出 0（target/discovery-cache-linux-remaining.log，34 节点 53.75 秒），确认本轮缓存顺序修复在原生 Linux 上经完整恢复场景验证。所有过程句柄已经结束，私有失败现场与日志保留；完整门禁中的分配期限失败、恢复请求超时以及交接自动阶段推进仍未完成，不能标记总目标完成。文档同步后执行 git diff --check。

### 2026-10-06：交接承诺涵盖已受理的未冻结正文

TaskHandoff.requests 承载现有待分配队列的原签名请求；相同请求出现冻结计划后复用该计划正文。统一摘要、编码、收集原子写入和认证业务基线安装均涵盖这些正文，移除请求必须改变完整根。未冻结正文只恢复既有任务绑定与重试队列，不建立资源围栏或旧集合投票权。非终态继承正文继续随下一份根保留，终态不复活；已认证分配的确切请求摘要可支持温启动恢复正文。

封口后禁止新增未覆盖的待分配正文，复用共同存储 CAS；已受理正文与认证分配仍允许推进。恢复统一使用 submit_verified 检查原期限，Allocating、尚无冻结计划的 AlreadyPending 和候选窗口暂满保留队列；持久化及注册失败也保留正文，确定业务拒绝才清理本地重试。移除旧重复分配注册逻辑，不添加状态表、循环、配置或版本兼容。

旧摘要遗漏与过期恢复均已实际复现，日志 target/handoff-requests-reproduction.log、target/handoff-requests-expiry-reproduction.log 保留。15 项交接与并发恢复定向通过（target/handoff-requests-targeted-final.log）；扩展原真实 QUIC 收集回归，异节点独有待办正文冷启动保留且没有准备、票锁或 BFT 状态（target/handoff-requests-collection-network.log）。新成员认证分块安装及期限拒绝回归覆盖原签名和错误 authorizer 拒绝。原 frontier 场景的封口后新请求要求改为无写入拒绝，已受理任务与成员切换仍需共同收敛（target/handoff-requests-frontier-barrier.log 通过）；原失败保留，不能算作实现已支持封口后的任意新任务。

最终两端完整门禁尚待执行；安全选根及自动阶段推进仍未完成，定向验证不证明间歇检查点响应超时或全部分配收敛问题已经修好。

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

### 委员会交接取舍与剩余实现边界（已选方向，尚未实现）

不采用每个旧成员必须声明全部本地义务的强制屏障：它使一个离线或拒答成员拥有永久否决权。quorum 也不能证明缺席节点没有私有正文，因此认证交接根只承诺已纳入的冻结义务和业务基线，不能宣称涵盖不可知请求。在线可取得的贡献仍须在封根前合并；现有健康四节点回归仍要求完整四任务并集。

遗漏处理必须落到同一个认证切换写入及现有任务受理路径，而不是另造请求仓库或取消共识。实施约束如下：

- 投票前永久关闭该候选根之外的新旧委员会资源所有权；保持对已纳入候选的原有推进权。明确把这条约束纳入持久化签名校验，不能仅依赖运行时停止发送。否则成员签完换届再签遗漏任务，quorum 交点不能阻止旧任务终局。
- 只在原委员会验证切换 quorum 证明后处理遗漏；本地超时、远端公告、单个 proposal 都无权解除占用。检查的是 `(TaskId, candidate_digest)`，不能以根内存在同 TaskId 的另一个变体代替。
- 已有有效 Commit 或 Abort 的 digest QC、finality-ready、终局签名锁、终局证书，以及先前交接承诺都必须保留。发现这些证据与遗漏根矛盾就拒绝安装，不能清锁或回滚业务。现有 `has_commit_evidence` 只覆盖 Commit 保护，不能直接充当完整遗漏许可。
- 对没有上述认证证据的遗漏准备，只允许在认证证明约束下停止旧候选并释放原本地临时资源；保留原作者签名正文、TaskId 和 request digest。已认证连续地址区间与分配证书永久保留，不能重新分配、回收或改写。原请求随后按当前委员会及当前业务状态重新受理，业务失败仍按现有错误/终局规则处理，不保证每个旧请求必然成功。
- 旧候选退出、可重试正文、认证切换证明和新委员会必须原子落盘；重启不能漏掉正文，也不能恢复旧签名权。现有 PreparedTask scope 的本地锁键不带委员会版本，重新受理前必须按认证退出规则处理旧状态，禁止盲删锁。根外迟到旧正文可以验证为证据，但不能重新获得旧委员会资源或投票权。

安全论证依赖旧任务 quorum 与换届 quorum 的诚实交点：诚实换届签名者覆盖其已有义务并封闭根外旧签名权，才允许推出遗漏候选不能新增合法旧终局。尚需逐条在存储、签名、变体、迟到正文和重启回归中证明这些前提；本段不是已完成实现或已经通过的安全证明。新成员空基线导入继续复用原认证恢复载荷；已有业务节点不能借遗漏规则覆盖未知历史前缀。

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

Windows fmt/check/clippy 通过，库测试 88 通过、1 忽略；主集成 245 通过、2 失败、1 忽略（竞争分配收敛、成员变更与分配前沿协调）。WSL 同源静态检查与库测试通过，主集成 246 通过、1 失败（四节点 CLI 重启流程恢复检查点响应超时）。两端另行完成的 34 节点恢复与 examples 均通过。日志分别为 `target/sealed-root-signing-{windows,linux}-gates.log` 与对应 `-remaining.log`；交接遗漏恢复仍未实现，不能将局部签名回归通过当作全部开发完成。

### 2026-10-08：失败启动会话的显式重注册恢复

启动失败会先清除 `needs_start`，却可能没有形成 deadline；过去相同 target 重注册直接返回，合法 Nil QC 推进后也无法继续。统一注册路径现在为未结束且没有 deadline 的会话重新设置启动标记，保留活动期限、投票、锁和完成结果，不添加后台重试。进一步修正根因：重启已有 prevote/precommit 时直接恢复原阶段期限，避免启动提案试图改签已有 Nil 值。最终真实分配回归 `restarted_nil_voter_advances_without_conflicting_proposal_or_deadline_renewal` 在缺少阶段恢复时失败（`target/idle-phase-reproduction.log`），修复后通过正常期限形成合法 Nil QC，进入下一轮，并冷读验证原请求和连续区间提交（`target/idle-phase-fixed.log`）。重复注册不续期或重复写入，完成后旧前沿仍拒绝。最初显式推进 QC 的复现与修复记录保留在 `target/idle-session-{reproduction,fixed}.log`；最终回归已替代该初步用例。此结果不等于解决自动交接并集、遗漏恢复或所有跨平台停滞。

### 2026-10-08：原阶段恢复的最终门禁

Windows 最终源码 fmt/check/clippy(-D warnings) 通过，库 88 通过、1 失败、1 忽略（四节点自动交接完整并集仍失败）；主集成 241 通过、6 失败、1 忽略，涉及分配收敛/耗尽/队列、成员前沿协调、私有来源及业务 CLI。34 节点恢复、examples 通过。日志 `target/idle-phase-windows-gates.log`、`target/idle-phase-windows-remaining.log`。同源归档在 WSL 全部门禁通过：库 89 通过、1 忽略，主集成 247 通过，34 节点恢复及 examples 通过（`target/idle-phase-linux-gates.log`），两端进程已结束。显式重注册的初步 Windows 日志 `target/idle-session-windows-gates.log` 单独保留。完整并集、认证遗漏安全释放与重新受理仍未闭合；不能用 WSL 通过抵消 Windows 失败。
### 2026-10-08：有明确 Nil 阶段的交接贡献收集

取消操作请求及每个唤醒的立即封根。`CollectingTransition` 先交换已验证正文，并使用原 membership/frontier BFT scope 从本次收集起始轮次开始的 Nil 阶段；持久化轮次进入后续轮才提升当前并集。收集 intent 摘要作为固定内存注册键，不累积每份未封根根到候选窗口；它和收集正文摘要都不能获得 digest 投票。根选择只采纳当前权威快照的封根记录，普通任务没有全局排序，也没有新增 scope、定时器或全体旧成员否决。

初始源传播避免再为收集正文中的冻结任务发送重复业务公告；早于交接意图发出的迟到业务拉取仅复用精确已验证见证，未知来源在收集期间暂不取得新资源权。原本地准备/明确提交仍可刷新未封根义务，认证切换后原运行循环独立验证并恢复继承业务。`target/collection-ownership-scope.log` 保存迟到公告扩大权限的真实失败，未放宽原权限断言。

`target/collection-round-domain-final.log` 的三项回归通过：在线四节点 root 含全部 4 项独有任务且不导入远端所有权；运行四节点完成全部业务；一个旧成员离线仍由在线 quorum 切换，离线原任务不变。认证遗漏释放/重新接纳及历史业务前缀恢复仍未闭合；不将有限收集轮次当作不可达正文的存在证明。完整平台门禁另记。

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

### 2026-10-09：历史 Abort 的完成通知与事件驱动恢复

收尾复现确认原历史 Abort 分支缺少业务完成后的运行时处理：原委员会证书合法、取消已落盘，但被释放 fence 阻塞的当前请求仍未获得资源权；有/无本地正文的回归复用既有基线安装、准备器、证书和输入消息入口。另扩展已有真实 QUIC 终态交付夹具，确认历史取消事件缺失。失败证据分别为 `target/closure-abort-waiter-reproduction.log` 与 `target/closure-abort-reproduction.log`。

修复在释放前复用 `contenders_for`，查询本地计划及该 TaskId 的全部认证继承冻结计划，复用既有资源索引并去重等待者；释放后复用完成回调，恢复受影响准备/持久请求及恢复候选。只读取首项继承计划的多变体失败已复现（`target/closure-abort-variants-reproduction.log`），修复后同一回归的四种场景通过（`target/closure-abort-variants-fixed.log`）。Commit receipt 恢复和已验证 Abort 使用同一有界完成缓存，统一移除会话/未来消息并去重完成事件。签名隔离、原委员会 quorum、取消原子提交、TaskId 与资源永久身份规则不变。完整两端门禁及设计闭合证据见开发完整性清单的当前记录；旧分日期未完成结论仅说明当时状态。
