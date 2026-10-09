# 节点部署、停机与恢复

正常重启、shared-state recovery 和丢失本地签名安全状态是不同操作。以下流程复用现有 CLI，不生成第二套节点状态或治理入口。

## 初始化与启动

Windows 与 Linux 使用同一 CLI 和协议。分别在各自系统构建，不能把 Windows 的 `.exe` 当作 Linux 可执行文件。当前验证工具链为 Rust 1.98；Linux 需要 C 编译器/链接器（Ubuntu 的 build-essential）和 Python 3（仅监督脚本需要）。源码目录执行：

```text
cargo build --release --locked
```

Windows 产物为 `target/release/second.exe`，Linux 为 `target/release/second`。WSL 中把源码、target 和部署数据放在 Linux 文件系统（例如 `~/Second`），不要用 `/mnt/c` 上的目录验证 Unix 私钥权限或部署存储语义。部署前设置 `umask 077`，目录保持仅操作员可访问；已有私钥文件须为 0600 或更严格，否则 Linux CLI 拒绝读取。不要在 Windows 与 WSL 中同时启动同一 Validator identity 的副本。

准备 DESIGN.md 的 init-network 配置、各 Validator keyring 和可信 Authorizer 公钥，初始化一次：

```text
second init-network <config-json> <deployment-directory>
second node <listen-address> <snapshot-base>
second snapshot-status <snapshot-base>
second ping <address> <nonce> <server-cert-base64>
```

节点 base 为 `<deployment-directory>/validator-<id>/second`。初始化拒绝覆盖已有 deployment，也拒绝复用已存在的 staging；本次初始化失败后尽力清理它创建的 staging，若清理失败则保留残余目录。使用固定且明确可拨的 listen_address。当前部署入口最多 56 个 active Validator，retained-set 也占连接预算。

LISTENING 只证明端点已绑定、capability 已加载，不证明 quorum 或业务最终性。核对并保存 ValidatorId、NodeId 和 transport certificate；业务端每个 endpoint 固定证书。snapshot-status 可在节点运行时读取持久状态；ping 不替代共识业务验收。

Windows PowerShell 7 可监督本机的一组已初始化节点：

```powershell
./examples/run-network.ps1 -SecondExe ./target/debug/second.exe `
    -ConfigFile ./network.json -DeploymentDirectory ./deployment
# 多主机部署时只启动本机成员：
./examples/run-network.ps1 -SecondExe ./target/debug/second.exe `
    -ConfigFile ./network.json -DeploymentDirectory ./deployment -ValidatorIds 1,2
```

脚本不初始化、生成 key 或恢复 snapshot。后台窗口隐藏；每个节点启动最长等待 30 秒，核对配置中的 listen address 与 ValidatorId，打印启动记录并生成本次 endpoint inventory 的路径。stdout/stderr 使用本次独立日志，不覆盖旧日志；操作员按自己的保留策略轮转。

正常调用持续在前台监督，`-RunSeconds 10` 可用于有结束时间的本地 smoke test。监督时某子进程退出，会报错并关闭本次其他子进程，没有自动重启策略。finally 只使用本次创建的 process handle，不按进程名或外部 PID 文件结束进程。整机故障或无法执行 finally 的会话强制终止后仍需检查存活进程。此脚本不提供开机自启、系统服务安装或跨主机编排。

结束子进程属于进程终止，不是事务 drain；持久安全性依靠既有原子提交与重启恢复，不承诺退出前完成所有受理任务。日志、inventory 和进程状态均不是协议真值。

Linux 使用 Python 标准库监督工具，无额外 Python 包：

```bash
umask 077
./target/release/second init-network ./network.json ./deployment
python3 ./examples/run-network.py --second-exe ./target/release/second \
    --config-file ./network.json --deployment-directory ./deployment
# 多主机只选本机成员；有结束时间的 smoke test：
python3 ./examples/run-network.py --second-exe ./target/release/second \
    --config-file ./network.json --deployment-directory ./deployment \
    --validator-ids 1 2 --run-seconds 10
```

Linux 工具与 Windows 工具使用相同配置、节点 base 和 endpoint inventory 字段；按 argv 传递路径，支持空格。启动等待 30 秒、核对 endpoint/ValidatorId、独立日志、不自动重启的边界一致。Ctrl+C 或发送 SIGTERM 给监督进程会结束其创建的子进程并等待回收；5 秒内未退出的子进程再强制结束。对监督进程 SIGKILL 或主机故障无法执行清理，须另行检查进程。正式服务安装、开机自启、异常重启与日志入口已提供，见 [双平台部署交付](deployment.md)。

跨主机填写真实可达的 listen_address，并放行对应 UDP 端口；loopback 配置只用于同一主机验收。WSL 的本机测试不证明外部主机到 WSL 的防火墙/NAT 可达性，也不代表所有 Linux 发行版已验收。备份、认证恢复、rotation 与 signing fence 继续使用下文同一套 CLI；跨系统复制私钥后需重新核对目标权限，不能同时运行复制出的身份。

## 同一目录排他与正常重启

node 在读取 bootstrap/snapshot 前持有 `<snapshot-base>.runtime.lock` 的 OS 独占锁。public-init、recovery-install、handoff-install 在目标安装期间取得同一锁。父目录先 canonicalize，相对路径/目录别名不会形成另一份同目标的 runtime lock。已有 StateStore 跨进程提交锁继续保护每次原子写入；runtime lock 不进入 snapshot、签名或 shared digest。

锁争用直接拒绝，不等待或结束现有节点。锁文件保留，不是 PID 或存活标记；文件句柄关闭后锁释放，可正常重启。运行期间不要删除该文件，否则不同文件对象可能形成不同锁。实现复用标准库 [File::try_lock](https://doc.rust-lang.org/std/fs/struct.File.html#method.try_lock)，没有新依赖。

这条锁只保护同一 snapshot base，不保护复制到其他目录/主机的同一 Validator key。禁止同时运行同一 identity 的副本。

正常重启使用同一最新完整目录和 listen address。保留全部 snapshot slots、commit reference、vote-lock/BFT state、安全 floor、Validator keyring/config、rotation keys、transport identity、bootstrap 和 peer cache；不要挑选文件拼装快照，也不要通过删 key/config 把出错 Validator 降级。

停机后备份整个目录。旧备份不自动构成本地签名安全连续性：备份之后可能已经签过新票，旧 vote-lock 不能证明没有这些签票。无法证明 safety 连续时，应走 locked recovery，不直接把旧目录作为可签 Validator 启动。节点目录与备份含私钥、私有业务状态，须保持受限访问。

## 新成员安装交接基线

新成员加入已认证的下一集合后，在空 destination base 配置其 `.validator.keys.json` 和 `.validator.json`。使用本地可信的旧委员会完整 snapshot 作为 trust anchor，提供当前交接提供端的固定 QUIC 证书：

```text
second handoff-install <provider-address> <empty-destination-base> <trusted-old-base> <provider-cert-base64>
second snapshot-status <destination-base>
second node-check <listen-address> <destination-base>
```

命令查询该旧委员会的公开切换证明，下载当前已安装交接所承诺的业务基线，以目标配置的业务授权集合验证交接请求，再原子安装。目标身份须属于证书的下一集合，不能因为尚未属于旧集合而拒绝新成员。公开证明会话不传私有正文，非空交接另开绑定签名的正文连接；规范空 genesis 无需第二次握手。缺失证明、错误身份、业务请求授权错误、非空目标或正在运行的目标均不会覆盖目标状态。该入口用于提供端当前已安装的单次交接，不自动绕过缺失的多次切换历史。

成功输出 `HANDOFF-INSTALLED ... safety=locked`，继承请求仍待正文受理/原委员会终态证明，未导入旧资源权利或签票记录。`node-check` 成功仅证明节点配置能加载；不能当作已获投票权。安装后继续遵守下述本地签名恢复/rotation fence 规则，不能直接修改 safety-ready。

新成员同样走现有恢复围栏：为安装后的当前集合生成自己的新 consensus key rotation，由健康成员认证下一次切换，再由健康成员发布该集合的恢复检查点。节点持有新私钥、独立轮换证书和匹配的恢复证明后，runtime 自动恢复签名。五节点回归已覆盖真实正文下载、非空业务基线安装、轮换前拒签、旧成员完成轮换与检查点、新成员恢复后提交并实际签署业务；不要求导入旧委员会的本地签票历史。不同本地业务基线的覆盖安装仍不属于这个空目标入口。

## shared-state recovery

1. 停止故障节点，保留原目录供诊断，选择全新 destination base。
2. 准备来自可信当前完整 snapshot 的 ValidatorSet/Registry trust anchor；不能从陌生 discovery 建立治理信任。原 identity/recovery key 仍可信时，将对应 keyring 安全放到 destination 的 `.validator.keys.json` 路径。
3. 健康成员 operator 发起恢复检查点；等待 quorum proof 后安装。ACCEPTED 是受理回执，不能当作 final；source 暂无 proof 时可以之后重试 fetch。

治理入口检查当前 active 集合的连接 quorum，不要求故障成员在线；启动连接尚未建立到 quorum 时返回 Busy，可有限重试。连接数量只是受理前置条件，不是合法投票数，最终性仍须通过原有 quorum certificate 验证。

```text
second recovery-checkpoint <healthy-address> <healthy-operator-base> <healthy-cert-base64>
second recovery-install <healthy-address> <empty-destination-base> <trusted-base> <healthy-cert-base64>
second snapshot-status <destination-base>
```

安装复用现有认证下载、exact ValidatorSet 和 checkpoint 验证，输出 `RECOVERY-INSTALLED ... safety=locked`。目标正在运行、已有完整快照或其完整快照损坏时拒绝，不自动覆盖、删除或重置。不要修改 floor、清除 vote-lock 或伪造 safety-ready 来绕过隔离。

shared recovery 不恢复丢失的本地签票历史。业务状态恢复成功不授予原 signing domain 的投票权。原 identity/recovery authority 仍可信时，用现有 `validator-rotate <destination-base> <identity|recovery> <rotation-request>` 生成新 consensus key rotation，由健康 operator 通过 transition-build/submit 接纳到 V+1。恢复节点配置既有 Validator config 和 bootstrap 后可以启动，在安全条件齐全前保持 locked。

runtime 自动检查三项证据：不含恢复节点旧-domain 票的合法 rotation transition、匹配当前 shared state 的 V+1 recovery proof、本地 V+1 consensus private key。三者齐全才持久恢复 safety-ready，并把 minimum signing version 固定到 V+1；不再为 V 或更早 retained task 代签。无法形成 quorum 时保持 locked，不用运维流程绕过协议。

### 从 locked 恢复到可参与新版本业务

在恢复目标停机时准备 rotation，确保新私钥已存在于既有 rotation-key log，再启动该目标；健康 operator 构造并提交 transition。plan JSON 使用现有字段：

```json
{ "rotation_request_files": ["rotation.request"] }
```

文件路径相对 plan 所在目录解析；rotation.request 是下面第一条命令的输出，不是手写或未经认证的 key。其余命令参数分别来自可信健康 operator 和固定端点证书：

```text
second validator-rotate <recovered-base> identity <rotation-request-file>
second node <recovered-listen-address> <recovered-base>
second validator-transition-build <healthy-operator-base> <plan-json> <transition-source>
second validator-transition-submit <healthy-address> <healthy-operator-base> <transition-source> <healthy-cert-base64>
```

等待各在线节点 snapshot-status 显示 validator_set=V+1，此时恢复目标仍须 safety=locked。transition finality 后、下一条请求前允许停止并重启恢复目标；pending safety evidence 和本地新 key 会从持久目录恢复。然后由健康 operator 请求新集合的 recovery proof：

恢复目标错过 transition 首次广播时，健康成员在 fresh authenticated BFT dial 上，向落后一代的接收方补发刚完成的 membership source 与 finality certificate。BFT 握手报告的 active ValidatorSet version 与 channel binding、role、ValidatorId 一起由既有 identity key 签名；该字段仅决定是否需要补发，不授予身份、membership 或投票权。已经进入当前集合的成员不再收到旧 scope，避免重启后仅有 current authority 时因旧消息关闭合法连接。source/证书复用既有 completed-scope cache，不创建另一份治理状态或新 wire 类型。接收方仍验证 exact current set、transition/rotation 语义、currency frontier 和 quorum。此补发仅覆盖进入当前集合的最近一次 transition，使用既有 64 个 completed scope 的有界内存缓存，不承诺任意跨代或健康成员全体重启后的历史追赶；需要这类恢复时，应从可信当前集合重新取得恢复状态与对应授权流程。

```text
second recovery-checkpoint <healthy-address> <healthy-operator-base> <healthy-cert-base64>
second snapshot-status <recovered-base>
```

等待目标显示新集合、认证 recovery serial 和 safety=ready，再提交依赖它的新业务。没有人工 unlock CLI，不编辑 snapshot flag。新建 authenticated exact-set BFT outbound 连接时，成员只向该接收方补发当前 active 集合的既有 attached recovery proof；接收方沿用原有签名、quorum、状态匹配和 floor 校验。这个有界连接事件补齐重启节点漏收的证明，不重新认证同一状态、不全网周期广播、不传输另一份私有状态或丢失的 signer history。

identity/recovery authority 也丢失或怀疑泄露时，应永久退休旧 ValidatorId，以新 ValidatorId 与全新三把 key admission，不能恢复旧身份投票权。具体 membership 计划由实际部署者决定，此工具不自动更改治理。

## 验证

公开连接维护区分新邻居发现与已建立连接丢失：未发现新候选时保留指数退避，连接数下降则在既有维护轮次立即重新尝试，避免小网络因达不到八个目标连接而延迟重连。outbound 使用经身份认证并持久化的 pinned endpoint，bootstrap 只查询一次邻居；inbound 仍向远端查询其自身广告的监听地址。没有新增高频轮询或扩大公开 Peers 响应上限。

bootstrap 候选同时最多四路探测，健康候选不用串行等首个沉默节点的请求期限。同一 NodeId 同时只探测一个记录，失败后仍可尝试该身份的另一完整 endpoint/certificate 记录；实际连接继续受全局 128 条预算、目标数与完整身份认证约束。并行探测随当前 bootstrap 调用推进，没有新增独立后台拨号循环。

节点公开及 BFT 的同地址族拨号复用已经绑定的监听 QUIC endpoint 和 driver，各 client handle 独立保存证书 pin；无需为每条连接新建 UDP socket。不同地址族需要独立 socket 时，在现有阻塞线程池创建，worker 持有原连接名额到结束。NodeId、BFT 身份、缓存持久化以及请求期限保持原约束，没有常驻端点池或第二份连接真值。

NodeRuntime 释放时显式关闭该共享 endpoint，使仍持有 outbound handle 的公开服务任务不能继续服务已停止节点；单条 peer 的 close 仍只关闭对应连接，不关闭其他对端或监听端点。

真实进程回归验证同一目录通过路径别名重复启动、public-init、recovery-install 被锁拒绝；进程终止后重新启动，证书与已成功任务保持一致。Windows 与 Linux 四节点 CLI 回归分别实际执行对应监督脚本，核对四条启动记录和 inventory 中的固定证书，并在脚本退出后重启同一节点，验证端口/锁释放及身份连续。Linux 还选取单个成员监督，发送真实 SIGTERM 后等待监督进程正常退出，再重启该成员。Unix 签名回归验证 Authorizer 私钥生成权限为 0600，改为 0644 后拒绝签名且不生成输出。真实 CLI 还完成 recovery-checkpoint→等待认证 proof→空目标 recovery-install，比较完整 canonical shared payload，核对 safety=locked，并验证重复安装被拒绝且 generation 不变。现有 safety-recovery 回归继续承担恢复后签名隔离验证，LISTENING 不冒充恢复后可投票证据。

四成员 CLI 生命周期进一步验证完整 reentry：只有三个健康成员在线时认证 V1 恢复状态；故障成员在新空目录恢复，CLI 生成新 consensus key rotation；目标离线期间健康成员依次认证 V2（目标轮换）与 V3（另一个成员轮换），随后健康成员全部重启；目标上线按版本拉取磁盘 proof 到 V3，保持 locked；在 V3 recovery proof 前再次重启目标，认证当前 proof 后自动恢复 signing fence=3，V1 旧 key 与 V2 正确轮换 key 均被 fence 拒绝。随后停止 Validator 4，仅恢复成员与两个健康成员在线，提交一个全新 TaskId 的发行任务，三者全部持久成功且余额只增加一次。因此恢复成员对这个 3/4 quorum 是必要的，没有用连接数或 ready flag 替代参与业务的证据。

### Windows / WSL 混合集群验收入口

`mixed_windows_linux_validators_pay_and_recover_into_required_quorum` 是必须执行的本地验收，已纳入 Windows 普通全量测试，禁止 ignored 或因环境缺失静默跳过。它需要 Windows、已有 WSL Linux 工具链、Python 3，以及可双向访问 localhost UDP 的 mirrored 网络。它不修改 WSL 设置、路由或防火墙；普通 NAT 网络或独立主机须另按真实可达地址部署。两端必须使用同一份当前源码，先在 Linux 原生源码目录构建主程序和本地验收 probe：

```bash
cargo build --release --locked --bin second --example mixed_snapshot_probe
```

随后在 Windows 仓库 PowerShell 中执行，Linux binary 路径应替换为实际构建产物：

```powershell
$env:SECOND_WSL_DISTRO = 'Ubuntu-26.04'
$env:SECOND_WSL_BINARY = '/path/to/native-checkout/target/release/second' # 替换为实际 Linux 原生构建路径。
cargo test --release --test integration mixed_windows_linux -- --nocapture
# 上述变量同样必须提供给普通 Windows 全量门禁。
cargo test --all-targets
```

同一集群的持续运行验收可在该命令前设置 `$env:SECOND_MIXED_SOAK_SECONDS = '600'`。显式验收至少执行 96 笔交替支付，并持续到指定秒数（上限 3600）；每 16 笔轮流停一个成员，三成员必要 quorum 完成四笔业务后重启。业务客户端依次向重启成员精确重提遗漏的原签名请求，逐笔等待持久成功，再核对完整共享状态；这是正式接入的精确重试路径，不要求节点自动广播全部历史。每笔仍使用原 45 秒判定期限，检查余额、V2 签名围栏和证书连续；超过 64 项 receipt 缓存后精确重放首笔，必须保持成功幂等。周期采样本次节点的 CPU/RSS，输出客户端观察的提交延迟。该选项只影响显式测试，不改变节点配置或新增生产监控循环。

测试初始化一个集合，先将尚未启动的 Validator 3/4 目录复制到 Linux 私有临时目录并限制权限；Validator 1/2 使用原生 Windows 二进制和 NTFS 本地目录，3/4 使用原生 Linux 二进制和 Linux 本地目录。每个节点独立持有自己的状态与 runtime lock。测试直接使用跨系统 QUIC，没有 TCP 代理、证书 bypass 或共享的活动 snapshot 目录。

Windows 父测试持有本次 Windows 子进程句柄；WSL worker 持有本次 Linux 子进程，正常结束、父管道 EOF 或异常时回收这些子进程。成功后清理本次临时部署；失败保留带 `MIXED-ARTIFACTS` 路径的私有现场用于诊断。日志不输出私钥；诊断目录仍含私有状态，不作为公开附件传播。

各系统通过原生 StateStore 读取自己的磁盘，复用 StateRecoveryPayload 的现有 canonical encoder/decoder 比较完整 shared state；不通过 WSL UNC 共享目录跨系统打开运行中的 store。本地 `mixed_snapshot_probe` 仅用于这项验收，其管道输出含私有业务状态，不是面向用户或网络的查询接口。

验收检查双向固定证书 QUIC 与错误证书拒绝；Windows 单源提交开户、发行、支付，全体余额/地址绑定和 canonical shared payload 一致；停 Windows 1 后，Windows 2 + Linux 3/4 形成必要 3/4 quorum；Windows 1 重启补齐成功任务。随后 Linux 3 离线，由其余混合成员认证恢复状态，恢复到全新 Linux base；故意在三个健康成员完成 V2 并全部重启后才上线恢复目标，读取持久 membership proof 后仍 locked。中间重启，再取得 V2 recovery proof 自动恢复签名安全。再次重启检查 transport identity/证书连续，停止 Linux 4，仅 Windows 1/2 + 恢复后的 Linux 3 完成新业务，核对精确余额、签名 floor 和完整 canonical shared payload。

M0 专属验收同时运行原生 Windows 与 Linux 钱包，每个钱包只配置另一操作系统的一个固定证书端点，不允许回退到本系统节点。复用同一混合集群和真实账户密钥，各自完成 balance、assets、address list、history；目标账户持有 257 份 Currency，资产必须跨三页完整枚举。检查真实账户、余额与数量、终页、地址/历史及来源说明，比较两端完整资产/地址/历史清单；不比较独立查询的本地 generation 或 nonce，不把两节点相同结果称为独立 BFT 证明。夹具私有文件只放入原临时目录，沿原成功清理路径回收。

2026-10-09 从最新源码重建 Windows / WSL Ubuntu 26.04 原生 Release，257 个源码输入 SHA-256 一致；上述完整场景 1 项通过、0 失败，31.53 秒，日志 `target/closure-mixed-release.log`。此前 2026-10-08 验收 41.78 秒，日志 `target/cross-mixed-acceptance.log`，只对应当时源码。Linux 数据位于原生文件系统，全部原有期限和断言保留；成功后停止本次子进程并清理临时节点目录。验收只证明同机跨系统场景，独立主机路由、防火墙和整机 boot 行为未验证。

2026-10-09 M0 补验并强制门禁：从当前生产源码重建两端原生 Release，205 个生产构建输入在 CRLF→LF 归一化后 SHA-256 一致（target/m0-mixed-source-verification.json）。Release 混合验收 1 项通过、0 失败、0 忽略，25.83 秒（target/m0-required-mixed-release.log）；普通 Windows all-targets 的 250 项主集成全部通过，包含混合场景、0 忽略，44.86 秒（target/m0-required-mixed-all-targets.log）。库 105 项、34 节点目标及 examples 同样通过，fmt/check/clippy(-D warnings)/diff 检查通过。故意移除 SECOND_WSL_BINARY 的负向验收立即退出 101、1 失败、0 忽略，证明缺少环境不能被当作通过（target/m0-required-mixed-missing-env.log）。本次没有修改生产代码、测试期限或安全断言；成功现场与 owned 子进程清理完毕。该验收是同机跨系统支付/恢复基线，不是专门的账户查询跨系统对抗测试，也不覆盖独立物理主机或整机 boot。

随后加入上述 M0 专属钱包调用的 Release 场景 29.68 秒通过（target/m0-query-mixed-release.log），两端主程序及 probe 摘要与此前重建产物相同。默认 Windows 全量库 105、主集成 250 通过，包含混合查询；但后续 34 节点目标在 PeerStore 文件未找到与重启发现阶段失败（target/m0-query-mixed-all-targets.log）。单独重跑通过（11.76 秒，target/m0-query-discovery-diagnostic.log）只提供定位线索，不能覆盖原失败。当前整套验收未全绿；混合查询成功现场已清理，34 节点失败现场保留，不延长期限或将其标为 ignored。

### 2026-10-09：PeerStore 重启并发写入修复

上述失败继续排查后复现：旧 runtime 被 abort 后，其已启动的 spawn_blocking 缓存写入仍可继续；新 runtime 加载同一路径产生独立 PeerStore，原先只有各实例的 records 锁，两个实例同时截断、写入并 rename 同一 `.peers.new`。受控双线程、各 512 次写入的回归测试在修改前产生 14 次 NotFound；这解释了后台缓存失败使重启发现退出的故障路径。

PeerStore 现在复用 persistence::slot::shared_path_lock，加载文件及写入 staging/fsync/rename 共用同路径进程内锁。不同缓存路径仍独立，编码和内存校验不增加磁盘读写；写入失败仍不发布新缓存或认证连接。缓存保持 best-effort，不保证独立实例内存视图合并；CLI 的跨进程 runtime 目录锁继续负责进程隔离。没有新增锁文件、共识状态或 owner 索引。

修改后并发回归与现有缓存测试 4 项通过，既有 cache_write_failure_cannot_publish_an_authenticated_validator_connection 通过。完整门禁结果另记，不能用这些定向通过替代全量验收。

修复后最终验收：fmt/check/clippy(-D warnings)/diff 通过；默认 Windows all-targets 全部通过，库 106 项（跨进程锁 helper 的直接入口仍 ignored，但由父测试启动的子进程实际通过）、主集成 250 项且 0 忽略（45.40 秒）、34 节点重启发现目标 1 项（32.60 秒），examples 通过。没有改变线程数、期限或断言。修复后的两端原生 Release、205 个归一化生产构建输入一致；独立 Release 混合场景 1 项通过、0 忽略，33.89 秒，包含 Windows→Linux 与 Linux→Windows 专属 M0 钱包查询。此前失败是实际失败，本次用并发复现、根因修复和重新全量通过闭环，而非仅凭单独重跑改判。

清理记录：本次确认的 9 个 target/m0-* 临时日志、源码归档与摘要文件、1043 个已结束测试的临时文件，以及 `/home/why23/second-m0-mixed-20261009-8281392` 临时原生构建目录（261116364 字节）已删除；历史段落中的这些日志路径只标识当时运行，不再指向保留附件。最终复核本轮新增临时项为零、两端 mixed 临时目录为空、本次节点和测试进程已停止。未删除既有 target 编译缓存、未改变用户 WSL 配置或其他服务。上面的构建路径示例为占位符，后续验收需提供实际同源原生构建产物，不依赖这份已删除的临时目录。

### Membership 持久追赶边界

追赶由 fresh BFT 握手中签名绑定的更高集合版本触发，复用 GetValidatorSetTransitionProof 的公开证明接口，不再依赖 completed scope 内存缓存。客户端与临时只读服务均限制每次 64 步与 5 秒总期限；逐步原子落盘，失败后通过既有 maintenance 重新选择来源或续取下一批，没有新增后台轮询。证明缺失、无有效 quorum、版本不连续或本地 currency frontier 不符时停止，保持 signing locked。

这不是业务状态补齐：本地 anchor、连续证明和每步 frontier 必须可验证；不能凭版本提示信任未知 identity，不能跳过状态差异，也不能从远端复制私有状态来凑齐 frontier。若业务发行使中间 frontier 与本地恢复状态不同，应走认证状态恢复流程。解锁还要求本地持有最新 consensus key、独立 rotation quorum evidence，以及与本地状态匹配的当前集合 recovery proof；失去 identity/recovery authority 的节点应退休并重新 admission。

## 原生系统服务

已有监督脚本保留为前台操作入口。正式部署可使用 Windows SCM 原生 service host 或 Linux systemd，每个服务只管理单个已有节点，数据与程序分开，完整步骤、打包、node-check 与验收命令见 [deployment.md](deployment.md)。正常服务停止释放端口/锁；不承诺 drain 所有受理事务，原子提交与 signing safety 恢复规则不变。
