# 2026-10-02 审计整改

对应 `audit-2026-10-02.md` 的七项问题。本页描述当前开发实现；最终验收以本地检查和行为回归为准。

## 地址分配

Issue 与 LeakRepair 的 u64 连续 identity 不再由本地准备顺序决定。原始签名任务先进入 `CurrencyAllocation { validator_set_version, start }` 的局部共识；digest 绑定集合版本、frontier、数量与完整请求摘要。quorum 确认区间后，节点原子保存任务区间和推进后的 frontier，然后进入既有 PreparedTask 共识。普通交易不经过分配共识，也不引入全局交易排序。

多个候选可以竞争同一 frontier；BFT driver 复用既有投票锁、解锁 QC 和最终性规则。未知候选的 proposal、votes 与 certificates 有界等待源任务校验，随后重放。任务拉取支持分配候选；业务计划源同时提供分配证书，使遗漏早期消息的节点补齐分配。

区间属于完整请求，取消准备保留已经确认的区间，同一任务重试仍使用该区间。其他任务不能使用它；失败任务也不能退回 frontier。提交结果增加 `allocating`，同一待处理请求重试返回 `already-pending`。分配证据保存在本地快照，不进入共享恢复摘要，避免合法但不同的签名集合造成摘要分歧。

新增四节点回归覆盖相反提交顺序、待处理任务重启、最终余额和共享恢复载荷一致性。成员切换绑定当前 currency frontier，与分配候选竞争同一个 scope，复用既有投票锁作为 epoch seal：分配胜出后必须在新 frontier 重建切换，切换胜出后待分配任务在新集合继续。已过期的新分配源拒绝入场，已持久化受理或有合法分配证书的任务允许补齐。

## 网络与任务源

应用协议增加五秒 deadline，覆盖 QUIC 建连与认证、请求交换、消息读入、响应写入及 delivery wait。首个请求也有限时；提交处理设置三十秒上限。正常已注册连接可以等待下一项请求，不通过空闲轮询维持活性。

私有任务拉取为当前源设置 deadline；超时或断连后清空部分载荷并尝试其他已连接 validator。每个有效 chunk 更新 deadline，重试轮次可以重新遍历已尝试源。

## 有界诊断与 peer 缓存

Coordinator 最多保留 256 条近期诊断事件，满时淘汰最早记录。业务结果与证书继续由持久化状态负责，不依赖 CLI 消费事件。

peer 文件只是发现缓存：格式损坏按空缓存启动；真实 I/O 错误继续报告。写入先同步 staging 文件再原子替换正式文件。

## 读取与服务成本

StateStore 与 PublicStateStore 缓存不可变、已验证结果；每次读取仍检查 commit reference 和两个槽位元数据，跨实例写入或损坏会使缓存失效。写入端编码验证成功后直接发布相同值的缓存，避免下一次读把刚写入的完整快照再次解码验证。

公开页面共享按 generation 构造的视图；proof/delta 请求不构建完整页面视图。私有恢复 provider 使用已验证快照中的 checkpoint digest 判定有效性，避免每个 chunk 重建完整恢复载荷。完整快照写入仍为 O(N)，没有增加 WAL、迁移层或平行权威状态。

## 防止签名状态回退

快照 `.commit` 是持久提交点，包含 generation、快照 checksum 和自身 checksum。主槽同步完成后，在独占 store lock 下直接写入并同步固定 80 字节引用，再尽力写 mirror。引用撕裂由自身 checksum 检出并 fail closed；不依赖 Windows rename 元数据持久性。读入只接受与已发布引用精确匹配的槽位；mirror 落后且主槽损坏时 fail closed，不能把旧 vote lock 或旧 trust floor 当作当前状态。相同已提交 generation 的有效 mirror 仍可恢复。

`cargo run --example audit_liveness` 已改为回归检查，验证确认区间下的乱序准备、silent peer deadline、损坏 peer 缓存启动和 Windows stale mirror 拒绝签名回退。原审计报告保留当时基线的证据，不代表整改后的行为。

## 本地验收（2026-10-03）

`cargo fmt --all -- --check`、`cargo check --all-targets`、`cargo clippy --all-targets -- -D warnings`、`cargo test --all-targets`、`git diff --check` 均通过。集成测试 222 项通过，单元测试及其既有进程隔离检查通过；默认标记 ignored 的进程入口由父测试实际调用。`cargo run --example audit_liveness` 四项回归全部通过。

此前全量并发运行发现测试窗口过短：单节点 CLI 等待由 4 秒改为 10 秒，四节点 CLI 等待由 20 秒改为 30 秒、测试 BFT phase 配置由 250 ms 改为 1 秒，调度器验证窗口由 1 秒改为 5 秒。协议默认 deadline 未改变；调度器单跑 0.18 秒通过，最终默认并发全量集成测试 14.93 秒通过。
