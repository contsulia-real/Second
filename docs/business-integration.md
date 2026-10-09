# 业务方接入：签名、提交、重试与支付

本指南适用于当前未发布的外部 Authorizer 模型。业务方决定一笔操作是否应发生，Authorizer 签发 LegalTask；Validator 验证可信签发者、业务约束和最终性。Authorizer 密钥不是账户私钥，不建立用户公钥账户或钱包。以下入口均为本地 CLI，复用同一 transaction parser、canonical LegalTask 签名和 QUIC 提交协议。

服务策略与协议职责以 [DESIGN.md 的网络定位与授权边界](../DESIGN.md#11-网络定位与外部服务边界) 为准。保证金不是当前协议的提交门槛，本指南也不提供保证金锁定或自动赔付能力。当前仍须由 Validator 本地 AuthorizerSet 接受签发者；该配置描述节点接受 LegalTask 的签名信任边界，不定义服务商组织或商业策略，也不意味着协议需要内置外部设施的授权流程。

带普通节点运行能力的独立 CLI 钱包方案见 [CLI 钱包节点实现方案](cli-wallet-design.md)。该文档区分可复用能力与尚未实现的私有账户读取，不将本指南已有 CLI 当作完整钱包。

## 1. 配置授权与固定端点

从仓库根目录构建 `cargo build --bin second`，Windows 可执行文件是 `target/debug/second.exe`。

```powershell
$second = (Resolve-Path ./target/debug/second.exe).Path
& $second authorizer-keygen ./business/authorizer.keys.json
```

输出 `AUTHORIZER public_key=<standard-base64>`，私钥只保存到指定文件，不打印或放进命令行。文件已存在时拒绝覆盖。将公钥交给部署者，在 init-network 的 `authorizer_public_keys_base64` 中配置；已有部署使用既有每个 Validator 的本地授权配置，并一致地重启生效。生成密钥不会自动取得写权限，也不修改运行中的授权集合。不要使用 Validator identity/consensus/recovery key 代替业务 Authorizer。

Unix 私钥文件创建为 0600，读取时拒绝 group/other 权限；Windows 继承目录 ACL，业务方须使用自己的受限目录。本地明文 key file 是当前最小工具的边界，没有 HSM、加密密钥库或密钥轮换管理承诺。signed request 同样包含私有业务信息，应受访问控制。

建立 `business/endpoints.json`，每条记录来自部署者提供并核对的 Validator 地址与 transport certificate，示意如下，替换后再运行：

```json
[
  { "address": "127.0.0.1:31001", "certificate_base64": "部署者提供的证书Base64" },
  { "address": "127.0.0.1:31002", "certificate_base64": "另一Validator的证书Base64" }
]
```

每个端点使用自己的固定证书，连接失败时不关闭验证或接受任意证书。public-only 节点不能提交或查询私有任务。重试脚本不会把业务请求发往公开 discovery 或 public gossip。

## 2. 一次生成并保存业务意图

下面 PowerShell 7 片段产生开户、发行、支付三笔独立草稿。每笔业务的 TaskId 与地址只生成一次；失败或超时后保留这些文件，不能重新运行生成片段来替代重试。

```powershell
$ErrorActionPreference = 'Stop'
function New-OpaqueAddress([string]$prefix) {
    $bytes = [System.Security.Cryptography.RandomNumberGenerator]::GetBytes(32)
    $text = [Convert]::ToBase64String($bytes).TrimEnd('=').Replace('+','-').Replace('/','_')
    return $prefix + $text
}
$alice = New-OpaqueAddress 'acct_'
$bob = New-OpaqueAddress 'acct_'
$source = New-OpaqueAddress 'pay_'
$destination = New-OpaqueAddress 'pay_'
$drafts = @{
    onboard = @(
        @{ type='register_account'; account=$alice },
        @{ type='register_account'; account=$bob },
        @{ type='register_payment_address'; address=$source; account=$alice },
        @{ type='register_payment_address'; address=$destination; account=$bob }
    )
    issue = @(@{ type='issue'; recipient=$alice; amount=2 })
    payment = @(@{ type='transfer'; source=$source; destination=$destination; amount=1 })
}
foreach ($name in @('onboard','issue','payment')) {
    $draft = @{
        request_id = [Guid]::NewGuid().ToString('N')
        version = 1
        expires_at = $null
        operations = $drafts[$name]
    } | ConvertTo-Json -Depth 8
    New-Item -ItemType File -Path "./business/$name.unsigned.json" -Value $draft | Out-Null
    & $second transaction-sign ./business/authorizer.keys.json "./business/$name.unsigned.json" "./business/$name.signed.json"
    if ($LASTEXITCODE -ne 0) { throw 'Signing failed; inspect the input before submission.' }
}
```

签名输入严格拒绝未知字段、signature 字段、无效地址、无效 TaskId、非当前 version 和无效操作。签名输出使用既有 canonical payload，不签 JSON 文本或依赖 JSON 字段排列。LegalTask 编码最大 2 MiB；CLI JSON 文件最大 16 MiB。签名和提交的当前格式均为 1，不维护实验格式兼容层。示例使用 null expiry；现有 `expires_at` 阻止新的执行开始，不自动取消已开始的工作，也不影响成功请求的精确重放。

AccountAddress / PaymentAddress 是独立随机 32-byte opaque 地址，分别使用 `acct_` / `pay_` 加 canonical Base64URL-no-pad。账户与支付地址永久不可改绑或复用。支付地址无独立余额；转账改变 Currency 的 Account owner，余额由权威资产状态派生。开户、发行、支付之间必须等待前一笔 succeeded，不能用受理回执作为前置业务已完成的证据。

## 3. 提交与有限重试

复制 keygen 输出的公钥到 `$authorizerPublicKey`，按顺序执行：

```powershell
$authorizerPublicKey = '替换为AUTHORIZER输出的公钥Base64'
foreach ($name in @('onboard','issue','payment')) {
    ./examples/submit-transaction.ps1 -SecondExe $second `
        -TransactionFile "./business/$name.signed.json" `
        -AuthorizerPublicKey $authorizerPublicKey `
        -EndpointsFile ./business/endpoints.json -MaxAttempts 24 -RetrySeconds 1
}
```

脚本对同一 signed request 做查询、必要时提交，并轮换固定端点。一次调用开始时复制请求字节到临时文件，后续源文件编辑不影响本次重试；退出删除该临时副本，原始 signed request 必须由业务方持久保留。最多 24 轮，不启动后台守护、无限轮询或全网 fanout。CLI 每次网络请求使用既有有限 deadline；MaxAttempts 是轮数而非整体秒数，轮内查询和提交可能分别等待 deadline。

默认查询到 prepared/voting/finalized 时等待最终提交；unknown/bound 时允许以相同 signed request 重提。Busy 或网络失败可切换端点。明确 Rejected、无效本地输入会停止，不能靠修改金额、改 TaskId 或换签名偷偷绕过拒绝。重试次数耗尽会报“结果仍不确定”；保留原始请求，之后查询或继续重试。脚本依据当前 CLI 的状态行与错误文本，CLI 属于未发布内部界面，变更时须同步此示例。

也可直接调用现有低层入口，每次固定到一个端点：

```text
second submit <address> <signed-json> <authorizer-public-key-base64> <server-cert-base64>
second task-status <address> <signed-json> <authorizer-public-key-base64> <server-cert-base64>
```

## 4. 状态与错误处理

| 状态/结果 | 已确认含义 | 业务方处理 |
| --- | --- | --- |
| allocating（提交响应） | 请求进入连续 Currency identity 分配协调 | 查询同一请求，不启动依赖它的业务 |
| prepared / pending（提交响应） | 已准备或精确请求已在进行中 | 等待并查询 |
| unknown（查询） | 这个节点没有匹配 TaskId + request digest 的本地事实 | 可查询其他端点或精确重提；不证明全网从未受理，也不暴露其他摘要的存在 |
| bound | 请求摘要已有持久绑定，尚无 prepared 或 succeeded | 可精确重提；可能仍在分配或先前执行未完成，不代表永久失败 |
| prepared / voting | 本地 prepared 或已进入不可逆投票阶段 | 查询等待；不得另建同义任务重复扣款 |
| finalized | Finality 已持久化，业务提交仍待完成/恢复 | 继续查询；还不能宣称余额已改变 |
| succeeded | 这个精确请求已经完成持久业务提交 | 才执行下一笔依赖业务，并保存成功结果 |
| cancelled | 原任务委员会的 Abort 最终证书已经持久安装 | 停止重提；不把取消当成支付成功；确需再次执行业务时须先核对意图 |
| Rejected | 当前提交被拒绝；服务不披露私有业务原因 | 检查签名、授权、TaskId 冲突与业务前置状态；不推断失败请求的历史终态 |
| timeout / connection failure / 重试耗尽 | 客户端不知道结果 | 查询或重提同一 signed request，不能创建新 TaskId 来“补发” |

成功任务重复提交返回 succeeded，不再次转账或发行。相同 TaskId 的不同摘要提交 fail-closed；任务状态对不匹配摘要返回 unknown，避免泄露另一笔私有请求。不存在独立权威 `failed` 状态表，所以脚本不会凭超时或 bound 伪造永久失败结果。

需要新 Currency identity 的请求在写入分配待办前检查区间可分配性；确定无法分配时拒绝，不遗留会阻断节点恢复的待办。并发请求单独可分配不代表全部都能分配：其他请求确认后若剩余地址空间不足，该待办停止分配，保留已有 TaskId 绑定。已获分配证书的请求即使后续业务准备失败，区间仍永久保留；精确重试使用同一区间，不重新占号。地址分配确认不等于业务 succeeded。

分配候选窗口满时，已写入的持久待办保持 bound，节点在其他分配完成后继续推进；提交被拒绝也不证明请求未落盘。保持查询同一个精确请求，不因资源限制重建 TaskId。

## 5. 已验证与尚未交付

回归通过真实 CLI 离线签名，再经四成员 QUIC 网络分三笔开户绑定、发行和支付，核对所有成员从磁盘独立加载的余额。测试 QUIC relay 在完整请求已被 Validator 受理后扣住 accepted 回执，让客户端真实超时；随后向另一个 Validator 精确重提，余额不重复增加。另验证篡改签名内容、相同 TaskId 不同已签操作、文件覆盖与无效签名输入被拒绝。Windows 回归实际运行 PowerShell 示例，从不可用端点切换到另一成员，提交新的支付地址退役任务并等待 succeeded。

这些是仓库内协议和接入工具验收。实际业务方的授权政策、私钥托管、运营发布与业务对账仍需真实接入验收；不把该示例称为完整钱包、SDK 或生产业务系统。
## 冲突与成员交接的当前行为（2026-10-09）

节点已经接通认证冲突候选准入、原委员会 quorum Abort、资源原子释放与受影响请求重试。当前集合的真实四节点 2∶2 冲突、同请求多冻结选币计划、已有 Commit QC 优先、认证依赖闭包与跨成员交接恢复均有行为回归；开发证据保存在源码仓库 `docs/conflict-arbitration.md` 和 `docs/development-status-2026-10-03.md`，不随部署包分发。不能把提交失败或超时当成可重用资源的依据。实际冻结选择与本地默认选择不同时，以验证通过的完整 plan digest 为准；远端选择不替代原始请求授权。

节点内部现在能对被本地资源占用挡住的冻结 source 进行只读完整验证，并报告实际冲突 TaskId。外部提交服务继续返回通用拒绝，避免泄露私有占用；该内部诊断不代表任何任务已经取消，客户端仍不能据此换请求或复用资源。


`cancelled` 来自同一 TaskBinding 的确定性终态，不新增失败结果表。相同请求再次 submit 被拒绝；不匹配摘要的查询继续返回 unknown。PowerShell 重试示例遇到 cancelled 立即报终态并停止，不耗尽预算后误称结果不确定。运行时完成事件可能是 Commit 或 Abort，业务方以 exact 请求状态区分结果。

成员交接保留认证恢复资源 fence：即使恢复节点没有旧普通计划，同资源请求仍被已认证旧任务阻挡。只有原委员会的最终结果可解除 fence；收到正确 Abort 最终证书后查询转为 cancelled，并按被释放的全部冻结计划唤醒等待请求。共享恢复不授予旧委员会签票权，恢复节点仍须经过独立签名恢复围栏。
