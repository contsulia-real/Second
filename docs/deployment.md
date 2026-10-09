# Windows / Linux 部署包与系统服务

本入口管理一个已初始化节点，不初始化或覆盖数据。二进制目录与私有数据目录分开；安装、停止、重启、卸载服务不删除快照、commit reference、签票记录、配置、rotation keys 或 transport identity。首次部署、证书固定、quorum 和 locked recovery 流程继续见 [node-operations.md](node-operations.md)。不要同时运行同一 Validator identity 的副本。

## 构建与分发

构建机需要对应系统的 Rust 工具链和 Python 3.11+：

```text
python deploy/package.py
# 已经过本地门禁的原生二进制可直接打包：
python deploy/package.py --binary <native-second-binary> --output-directory <new-output-directory>
```

Windows 生成 `second-0.1.0-windows-<architecture>.zip`；Linux 生成对应 `.tar.gz`。包内固定为 `bin/second.exe` 或 `bin/second`，包含平台管理脚本、监督工具、配置模板和操作文档，绝不扫描或携带节点数据、密钥、target 或 Git 目录。输出拒绝覆盖。`manifest.json` 记录每个包内文件的 SHA-256；校验用于检测传输/解压损坏，获取包仍需可信渠道，不把同包的 checksum 当作发布签名。

分别在目标平台原生构建，`second --version` 输出 package version、OS、architecture，打包器核对其与当前源码/系统一致。Linux 当前验证 Ubuntu 26.04/WSL、默认 glibc 链接；不承诺 Alpine/musl、不同架构或任意旧发行版兼容。Windows 系统服务不依赖 Python、Rust 或 PowerShell 常驻：只有安装管理脚本需要 PowerShell 7.3+。Linux 节点由 systemd 直接执行，Python 只用于安装管理。

解压到稳定的程序目录；另建私有数据目录。复制 `deploy/network.example.json`，替换可信 Authorizer 公钥；用 `validator-keygen` 生成各成员 keyring，填入实际可达地址和相对 keyring 路径，随后 `init-network` 初始化一次。模板中的公钥占位符故意不能启动，不捆绑默认密钥；四个 loopback 地址仅适合同机验收。跨主机可达性须独立验证。

## 启动前检查

```text
second node-check <listen-address> <snapshot-base>
```

检查复用 node 的 runtime lock、snapshot/commit 校验、capability/key/config/bootstrap 加载及 QUIC bind，打印 `CHECKED LISTENING ...` 后释放端口和锁退出。已经运行、损坏状态、缺失能力配置或端口冲突直接失败。它不运行共识、不提交业务、不解除 locked safety。与首次 node 启动相同，缺失 transport identity 时可创建 identity/锁文件；已 provision 的目录保持现有证书。检查不是 quorum 或业务 readiness 的证据。

## Windows SCM

以管理员 PowerShell 安装（示例路径支持空格）：

```powershell
./deploy/windows-service.ps1 -Action Plan -Name Second-validator-1 `
  -SecondExe 'C:/Second/bin/second.exe' -SnapshotBase 'D:/SecondData/validator-1/second' -ListenAddress '127.0.0.1:7401'
./deploy/windows-service.ps1 -Action Install -Name Second-validator-1 `
  -SecondExe 'C:/Second/bin/second.exe' -SnapshotBase 'D:/SecondData/validator-1/second' -ListenAddress '127.0.0.1:7401'
./deploy/windows-service.ps1 -Action Status -Name Second-validator-1
./deploy/windows-service.ps1 -Action Stop -Name Second-validator-1
./deploy/windows-service.ps1 -Action Start -Name Second-validator-1
./deploy/windows-service.ps1 -Action Restart -Name Second-validator-1
./deploy/windows-service.ps1 -Action Remove -Name Second-validator-1
```

Install 拒绝已有同名服务；先做 node-check，创建 delayed-auto、独立进程服务，用 `NT SERVICE\<service-name>` 虚拟账户，并只给指定节点目录 Modify、程序目录 Read/Execute 权限。不要把 snapshot base 的父目录设为整个磁盘、共享 deployment 根或含其他私有业务的目录；该父目录就是服务的写权限边界。SCM Running 只在真实节点 bind/能力加载成功后上报。Stop/Shutdown 通过一次性通知停止 node future，没有定时扫描；运行错误上报非零 Stopped，SCM failure action 分别延迟 5/15/60 秒重启，正常 Stop 不自动重启。

日志为节点目录的 `<service-name>-logs/service.log`；单文件超过 1 MiB 时保留一份 `service.previous.log`，记录启动、停止与错误，不写私钥/业务 payload。Status 输出 SCM 身份、路径、状态和 PID。Remove 只移除本工具创建的服务，保留数据、日志和已有 ACL；若要撤销虚拟服务 SID 的 ACL，应由操作员针对这个目录处理。安装中失败只撤销本次服务注册，保留现场与已授予权限，不重置数据。

## Linux systemd

先准备已有非 root 用户及该用户独占的节点目录（0700、私钥0600），程序目录对该用户可读可执行。安装时需 root，运行时始终使用指定账户：

```bash
sudo python3 deploy/linux-service.py install --name second-validator-1 \
  --second-exe /opt/second/bin/second --snapshot-base /var/lib/second/validator-1/second \
  --listen-address 127.0.0.1:7401 --user-account second
python3 deploy/linux-service.py status --name second-validator-1
sudo python3 deploy/linux-service.py stop --name second-validator-1
sudo python3 deploy/linux-service.py start --name second-validator-1
sudo python3 deploy/linux-service.py restart --name second-validator-1
sudo python3 deploy/linux-service.py remove --name second-validator-1
# 先检查并打印 unit，不安装；以服务用户或 root 执行：
python3 deploy/linux-service.py render --name second-validator-1 \
  --second-exe /opt/second/bin/second --snapshot-base /var/lib/second/validator-1/second \
  --listen-address 127.0.0.1:7401 --user-account second
```

工具不创建用户、不复制/修补状态、不自动改私钥权限；安装前以指定账户执行 node-check。unit 位于 `/etc/systemd/system/<name>.service`，systemd enable 后开机启动，`Restart=on-failure`、5秒重启间隔、60秒内最多5次启动；`UMask=0077`、`NoNewPrivileges`、只允许节点父目录写入。正常 SIGTERM/SIGINT 返回成功；日志进入 journal，由主机 journal 保留策略管理：`journalctl -u second-validator-1.service`。脚本拒绝接管同名非本工具 unit，卸载不删除数据/账户。WSL 需已启用并运行 systemd；此次安装不修改 WSL 配置。

停止是退出运行时并释放资源，不是等待全部已受理业务完成。安全性继续依赖原子提交和重启恢复；不要通过旧备份覆盖签票状态。变更二进制前停止服务，保留完整当前数据并核对新构建；未发布阶段没有旧 snapshot reader 或跨格式回滚承诺。

实现参考：[windows-service 上游](https://docs.rs/windows-service/0.8.1/windows_service/)、[Microsoft SCM 创建服务](https://learn.microsoft.com/en-us/windows-server/administration/windows-commands/sc-create)、[systemd service 规范](https://github.com/systemd/systemd/blob/main/man/systemd.service.xml)。Context7 当前会话不可调用，API 核对使用这些上游文档。

## 显式生命周期验收

验收脚本只创建本次临时节点和唯一 service name；正常结束卸载服务并清理本次临时目录，失败保留私有诊断目录。Linux 临时目录放在指定用户的 home 原生文件系统，避免 WSL 的 `/tmp` 随发行版退出被清除。它用真实 CLI 生成密钥/provision/签名，检查固定证书 QUIC、业务持久成功、正常停止后端口/锁释放、重新启动、强制结束本次服务主进程后的自动重启，以及卸载后任务/状态/身份持续。不会杀其他节点、修改防火墙或 WSL 配置。

```powershell
# 管理员 PowerShell 的完整 Windows SCM 验收：
python deploy/service-smoke.py --second-exe bin/second.exe
# 无管理员权限只验证预检、含空格路径、SCM命令计划及 console拒绝：
python deploy/service-smoke.py --second-exe bin/second.exe --plan-only
```

```bash
sudo python3 deploy/service-smoke.py --second-exe /opt/second/bin/second --user-account second
```

`--plan-only` 不安装服务，不能作为 SCM 生命周期通过的证明。Linux 管理工具拒绝路径中的控制字符、美元符号、引号和反斜线；支持普通空格路径，避免 systemd 的 environment/specifier 解释改变实际参数。

## 连接修复验收（2026-10-09）

公开连接重复 GetPeers、断线后继承过长退避、沉默候选串行阻塞，以及节点逐连接创建 QUIC endpoint 的成本已处理。同地址族的公开/BFT outgoing 连接复用节点原监听 socket/driver，每个 client handle 独立固定远端证书；不同地址族仍创建必要的独立 socket。原认证、连接容量、持久化与请求期限保留。真实 QUIC 回归验证源端口、独立 pin、错 pin 拒绝及已停止节点不再响应。

259 个构建输入两端 SHA-256 一致，Windows/WSL 原生静态门禁通过；同机同时执行默认完整 all-targets 连续两轮，两端每轮各 344 项通过（`target/repair-complete-paired-*.log`、`target/repair-complete-repeat-*.log`）。两端分别原生重建 Release 主程序和 probe，固定摘要见 `target/repair-release-binaries.json`；2 Windows + 2 WSL Linux 的显式混合验收通过，28.82 秒，日志 `target/repair-mixed-release.log`。覆盖双向 pinned QUIC、错误证书拒绝、支付与完整共享状态一致、掉线后的必要 quorum、重启追赶、全新 Linux base 认证恢复、提供方全部重启后的 V2 proof 拉取、签名隔离/恢复，以及恢复成员作为必要 3/4 quorum 完成新业务。验收前后摘要一致，所有本次节点和临时目录已清理。

同一固定二进制随后重跑十分钟持续验收，全部通过（`target/repair-mixed-soak-600.log`）：持续段 601 秒、134 笔转账，含基线共 630.05 秒；四成员轮流停机时由必要 3/4 quorum 完成业务，重启后逐笔精确重提漏收请求，最终完整共享状态一致，超过 receipt 缓存后的最早请求仍成功幂等。客户端观测提交 P50=742、P95=12,934、最大 18,111 毫秒，包含 CLI、探针及故障条件，不是纯协议延迟基准；CPU/RSS 周期采样保存在日志中。前后二进制摘要一致，259 个构建输入未变化，节点和本次临时目录已清理，核对记录为 `target/repair-soak-verification.json`。

从 `target/dist-connectivity-20261009` 解压的 Windows/Linux 原生二进制与上述固定摘要一致，真实管理员 SCM 和 root 管理、why23 运行的 systemd 生命周期均通过（`target/service-connectivity-windows.log`、`target/service-connectivity-linux.log`，核对记录 `target/service-connectivity-verification.json`）。覆盖安装、拒绝覆盖、签名开户/发行、正常停止/启动、强制终止本次服务进程后自动以新 PID 重启、固定身份/业务状态连续、卸载保留数据并释放端口/锁；实际执行完整验收，没有使用 plan-only。临时服务和数据已清理。

更新验收文档后的最新分发目录为 `target/dist-connectivity-soak-20261009`，校验记录 `target/repair-soak-package-verification.json` 核对 manifest、当前文档/脚本、相对链接、无状态/密钥白名单、Linux 归档权限，以及与服务验收原包完全相同的二进制和管理脚本。旧包及历史失败记录保留原边界。独立物理主机网络、整机 boot 和任意负载容量不在本次证据范围内。

## 历史存储阶段收尾验收（2026-10-09）

当前开发范围的逐项核对记录保存在源码仓库 `docs/development-status-2026-10-03.md`，开发审计记录不随部署包分发。移除临时采样、收敛完整/公开状态文件打开与写入路径后，两端 fmt/check/clippy(-D warnings) 通过；Windows 默认全量 343 项通过（`target/stability-open-windows-all.log`），追加两轮默认集成各 247 项通过（46.98/44.26 秒）；WSL 原生默认全量 343 项通过（`target/stability-open-linux-sequential.log`）。258 个构建输入 SHA-256 一致，两端原生 Release 的固定摘要保存在 `target/stability-frozen-binaries.json`。所有原测试期限、线程数、签名和持久化安全约束保留。

含最新存储修复的固定原生部署包位于 `target/dist-stability-frozen-20261009`。两个包解压后的二进制已经分别通过真实管理员 SCM 和 root/systemd 生命周期（`target/service-frozen-windows.log`、`target/service-frozen-linux.log`）：签名开户/发行、正常停止与启动、强制结束本次服务进程后的自动重启、身份及业务状态连续、卸载保留状态。不是 plan-only；临时服务与其数据均已清理。整机 boot、独立物理主机网络未执行。

此前阶段混合集群持续运行 601 秒、完成 103 笔支付（`target/stability-mixed-soak-replay.log`），含轮流成员停机、原签名请求精确重提补齐、超过 64 项 receipt 缓存后的成功幂等。阶段运行期间重建了 Linux 产物，因此该记录不作为固定最终二进制的唯一验收；最终冻结产物的独立持续验收另记。两套默认全量同时叠加到本机时，Linux 短连接期限仍有失败（`target/stability-open-linux-all.log`），不把单独全量通过冒充该压力场景通过，也不作任意负载容量承诺。原三项 Windows 超时阶段没有 Linux 编译，不能将它们归因于编译；采样与完整失败记录见开发核对入口。

最终冻结产物持续验收已经通过：`target/stability-frozen-mixed-soak.log`，基线及持续段共 646.84 秒；持续段 611 秒、132 笔支付。覆盖四成员轮流离线期间的必要 3/4 quorum、重启后逐笔精确重提补齐、完整共享状态一致、超过 receipt 缓存的旧请求成功幂等及 V2 signing fence。客户端观察提交 P50=800、P95=13,402、最大 14,890 毫秒，含 CLI/探针和故障条件，不能当作纯协议基准。运行前后固定摘要一致，所有本次进程/临时目录已清理。

最终分发包位于 `target/dist-stability-final-20261009`，二进制和服务脚本与上述实际服务验收包逐字节一致；仅将本次实际验收记录同步到操作文档。manifest 全量、源码文档/脚本、包内相对链接、无状态/密钥白名单和 Linux 文件权限校验记录为 `target/stability-package-verification.json`。

## 历史源码混合集群复验（2026-10-08）

在 Windows 和 WSL Ubuntu 26.04 分别原生构建当前工作区的 Release 主程序与验收 probe，均使用锁定依赖；两端 255 个构建输入文件的 SHA-256 比对一致。现有 2 Windows + 2 Linux 显式混合验收全部通过，41.78 秒，日志 `target/cross-mixed-acceptance.log`。覆盖双向证书固定 QUIC、错误证书拒绝、开户/支付持久一致、掉线后的必要混合 quorum、重启追赶、全新 Linux base 的认证恢复、健康提供方全部重启后的轮换证明追赶、签名隔离与 V2 恢复、恢复成员作为必要 3/4 quorum 成员完成新业务。原期限与安全断言未改，成功后本次节点和临时目录已清理。

本轮复验没有重跑服务安装、交付包或整机重启；下节的 SCM/systemd 记录仍对应当时产物。用户确认当前只用 WSL，独立物理主机网络不在本次范围内；不以此结果宣称外部网络可达性或任意部署规模已验证。

## 历史验证记录（2026-10-03）

Windows / WSL Ubuntu 26.04 两端 fmt/check/clippy 与完整 all-targets 门禁通过；扩展已有 capability 回归证明 node-check 释放端口与锁、不推进业务 generation、证书与真正启动一致，并拒绝正在运行的 base。真实 2 Windows + 2 Linux release 集群通过支付、离线/重启、持久 membership 追赶、签名恢复和必要 3/4 quorum，场景 39.88 秒。

Linux 原生 release 二进制的 systemd 生命周期实际通过：带空格私有路径、以 why23 非 root 用户运行、拒绝覆盖、签名开户发行持久成功、正常停止/启动、SIGKILL 后新 PID 自动启动、固定证书及成功任务连续、卸载保留状态且释放端口/锁。Windows 原生 release 的非管理员 Plan/真实 node-check/含空格路径/SCM入口拒绝console已通过，服务代码参与全部 Windows build/check/clippy 门禁。随后通过 Windows UAC 启动独立管理员验收进程，真实 SCM 生命周期通过：安装并运行、拒绝覆盖、签名开户发行持久成功、正常停止/启动、强制结束主进程后由 failure actions 自动启动新 PID、固定证书及任务连续、卸载保留状态并释放端口/锁。验收退出码 0，正常清理了本次临时服务和目录；当前 Codex 进程本身仍使用原权限。

日志：`target/deployment-windows-verified.log`、`target/deployment-linux-verified.log`、`target/deployment-mixed-verified.log`、`target/service-linux-lifecycle.log`、`target/service-windows-plan.log`、`target/service-windows-lifecycle.log`。独立 Windows/Linux 主机网络与真实重启开机后的 boot 行为尚未验证；服务配置已 enable/auto，但不把配置当成整机重启证据。

追加交付包验收：从 `target/dist-scm-verified` 解压出的 Windows 包再次通过管理员 SCM 完整生命周期，日志 `target/service-windows-package-repeat.log`；Linux 包也通过非 root 服务账户的完整 systemd 生命周期，日志 `target/service-linux-package-repeat-verified.log`。Linux 首次解压测试使用 root 创建的 0700 父目录，服务账户无法执行程序，安装预检正确拒绝；将本次公开程序包的父目录设为 0755 后重跑通过，未放宽私有节点数据权限。失败现场保留，成功临时服务和节点目录清理。混合 release 集群也再次通过全部场景（35.67 秒），日志 `target/deployment-mixed-repeat.log`；这不是性能基准。补齐记录后的交付包位于 `target/dist-acceptance-final`，只更新文档，二进制与验收所用包一致。

当前只有本机与 WSL，没有可用第二台主机。Windows + WSL 的混合回归属于本机跨系统验收，不能替代独立主机的路由、防火墙与局域网可达性验证；缺少第二台主机不阻塞当前开发。整机重启只用于补证开机自动启动，不是普通停止/启动、异常恢复或部署功能验收的必要操作。本轮不重启用户电脑，不将尚未验证的 boot 行为记为已通过。

后续连续地址分配复查修复了无效请求残留、竞争耗尽 frontier 退出、候选窗口满导致恢复退出三项问题；两端完整本地门禁通过，并重建当前源码的原生 release 二进制。包含这些修复和当前文档的新包位于 `target/dist-allocation-verified`，保留开发版本 0.1.0；服务管理实现未变。此前 SCM/systemd 与混合集群 release 现场记录仍对应服务验收阶段二进制，本次新修复的行为证明来自两端完整 debug 测试，不冒充新包再次完成上述所有 release 现场场景。
