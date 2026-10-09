# PreparedTask 最终性证据恢复

## 复现与修复边界

`tests/integration/runtime_finality_recovery.rs` 使用四份独立持久状态和实际签名。H1、H2、B 通过真实 prevote/precommit 签名形成原委员会 QC，接受 QC 后签出业务最终性票；B 拥有合法三票证书，但不向诚实节点发送任何消息。H3 未见过任务或 QC。随后丢弃原 signer、store 和 QC 内存对象，从磁盘冷加载 H1/H2，创建新的诚实 QUIC 运行时，仅连接 H1、H2、H3。H3 在任务过期后获取冻结源。

修复前该测试在 12 秒的有界观察内无法形成终局，H3 持续记录该任务的 UnregisteredScope。修复后必须实际形成三票 FinalityCertificate，签名者不含 B，声明与原决定相同，三个诚实节点均持久提交业务，并清理活动 BFT 状态。该测试不是纯模型，也不代表任意异步网络中的有界活性证明。

运行：`cargo test --test integration restarted_finality_voters_finish_without_hidden_byzantine_vote -- --nocapture`。

## 改动与不变量

- 完整 Precommit QC 保存于既有 BftLocalState，复用现有 BFT 消息编码。它和 readiness 共用原持久化事务；PreparedTask 最终性锁落盘后继续保留该状态。冷启动恢复会话的 finality_qc，使用已有有界会话、重发期限和网络路径，不增加共识轮次或后台循环。
- 冷解码核验原委员会、协议版本、scope、Precommit 阶段、round、readiness digest、冻结候选或精确 Abort 上下文，以及原 quorum 签名。已锁定或已 ready 的不同决定不能被新的 QC 替换；重复同一 QC 不产生新的 generation 或写入。
- 对未知的过期 PreparedTask，仅有效的同 scope、同冻结 digest、原委员会 Precommit QC 或最终性证书可恢复时间准入。普通票、Prevote QC、不足 quorum、错摘要/任务均不能恢复。原始任务签名、业务校验、资源占用及 commit_authorized 检查继续执行，QC 不能当作业务已经提交的证明。
- q 阈值、FinalityCertificate 验证、先落盘再签票、业务原子提交及 validator_safety_ready/minimum_signing_validator_set_version/原委员会密钥检查保持不变。

`src/runtime_tasks/source_admission/expiry_tests.rs` 同时检查 Precommit/最终性证据下的过期恢复和已变化业务的拒绝。既有成员交接、签名围栏、错误最终决定及持久化回归继续作为完整本地门禁的一部分。

## 保留、清理及资源

QC 只作为活动任务的本地安全证据，不是第二份业务真值。不能因超时、未收到 B 的票或缓存容量而删掉活动 QC。任务沿现有路径持久完成认证 Commit 或 Abort 后，现有任务删除事务清理对应 BFT 状态。提交证书后的执行阻塞仍保留活动状态；历史 TaskReceipt 的有界策略不因此变成长期证明档案。

没有新增 fsync 事务：证据写入原 readiness/最终性锁事务。新增一份 Precommit QC，以及最终性阶段继续保留的既有 BFT 元数据和可能存在的 Prevote QC。当前完整快照持久化会反复编码、写入这些活动证据，并写镜像；不能将成本仅算为一次 QC 写入。

TaskId 为 32 字节、证书恰含 q 票时，既有 Precommit QC 消息编码分别为 n=4/q=3：307 B；n=10/q=7：595 B；n=34/q=23：1747 B，另加可选字段标记和存储长度。网络消息格式不变，恢复使用原来的 QC 重发；单份证据验签为 O(q)，不扫描 Currency。当前完整快照校验会复验活动 QC，因此每次写入还增加 O(mq) 的证据校验工作；没有声称这部分 CPU 免费。活动 m 个任务的新增证据为 O(mq)，重复广播本身不增加持久写入。

## 不作出的保证

验收针对稳定原委员会、足够诚实成员可用、冻结源和所需业务依赖可恢复的情形。本次不改变 TaskHandoff、委员会退役、状态恢复的安全围栏或最终性安全语义。旧成员退出、原签名密钥退役或本地安全状态丢失后，不能凭保留的公钥/QC制造旧委员会签名；这些场景仍需独立协议审查。修复不证明任意资源阻塞均可终结，不实现认证任务覆盖切点或钱包证明。

本地验收必须包括 fmt、check、clippy、all-targets 测试及 Windows–WSL 混合测试；测试通过不能替代 BFT 安全证明。

## 本次验证记录

- Windows 故障复现：生产修复前，主回归 0 通过、1 失败，12.35 秒；修复后 1 通过，首次 0.83 秒。Linux 原生同一回归 1 通过，0.39 秒。耗时是这些单次运行的观测，不是性能基准。
- 过期恢复回归同时覆盖最终性证书和 Precommit QC；错误证据及已变化业务均拒绝，且不改变 generation。
- 最终复核扩展既有冲突回归，确认保留旧轮次 Precommit QC 后本地轮次前进、再收到最终性证书时，原 reconciliation 会将 readiness 改成当前轮次并触发 `InvalidSnapshot`。该构造实际失败（0.15 秒）；修复只在尚未 ready 时初始化 readiness，已认证的轮次和决定保持不变。
- 混合门禁首次因缺少 Linux `mixed_snapshot_probe` 示例产物失败。根因是只构建了主程序；补建当前源码的 `--bins --examples` 后，真实混合测试 1 通过、0 忽略，41.88 秒，包含四进程支付、掉线追赶、原生钱包查询、安全恢复/换钥和恢复成员参与新 3/4 quorum。
- 并行构建期间一个既有 3 秒限时 runner 回归超时；单独复核 0.52 秒通过，停止并行构建后完整单元测试 106 通过、1 子进程辅助测试忽略（父测试实际运行该辅助进程）。未据此修改生产调度或放宽测试超时；诊断输出补充事件和持久任务状态。资源竞争是推测，未将一次成功复跑当作已确定根因。
- 轮次边界修复后，默认并发全量测试中的既有 5 秒分配回归超时：日志显示前沿已达到上限、后续业务仍待处理；独立复核 0.98 秒通过。该轮曾并行清理临时文件，增加磁盘负载；这也是候选原因而非已证明根因。不修改生产调度、不放宽超时，停止并行工作后完整复核。
- 最终门禁：`cargo fmt --all -- --check`、`cargo check --all-targets`、`cargo clippy --all-targets -- -D warnings` 均通过；设置 `RUST_TEST_THREADS=1` 后执行完整 `cargo test --all-targets`，单元测试 106 通过、上述辅助测试 1 忽略（71.02 秒），集成测试 251 通过、0 忽略（259.02 秒，包含必做混合验收），34 Validator 恢复测试 1 通过（14.25 秒）。`git diff --check` 通过。串行通过不证明默认并发测试的超时原因已查明。
- 最终源码的 Linux 原生隐藏证书回归再次通过（0.40 秒）；旧轮次 QC 冲突回归修复后通过（0.22 秒）。
- 结束前核对运行前的 Windows 临时目录清单，清理本轮新增夹具及首次定向测试的 13 个已确认残留文件；保留之前存在的其他文件。移除 WSL 本轮失败夹具与临时构建目录，检查无本轮 Validator/worker 残留；保留仓库原有 `target` 构建缓存。`AGENTS.md` 内容哈希与开始时一致，其既有未提交修改不纳入提交。

本次直接修改未发布的内部快照编码，不提供旧格式兼容读取。原有工作区改动不参与本次提交；开发测试快照需要按当前格式重新生成。

混合验收准备：Windows `cargo build --bins --examples`；WSL 从同一工作区执行 `cargo build --bins --examples --target-dir /home/why23/.cache/second-finality-validation-target`。设置 `SECOND_WSL_DISTRO=Ubuntu-26.04`、`SECOND_WSL_BINARY=/home/why23/.cache/second-finality-validation-target/debug/second`，执行 `cargo test --all-targets`。Linux 目标目录是本次临时验证产物，验证结束后清理，复跑需重建。
