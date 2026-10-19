# 故障排查与验收

[返回 README](../README.md) · [配置参考](configuration.md)

## 先确认正在运行哪个版本

```sh
command -v envwrap
envwrap --version
envwrap --help
```

安装了新版本但仍见旧命令面，通常是 PATH 中另一个二进制先被找到。清除 shell 命令缓存（Bash：`hash -r`），再用新二进制的绝对路径复核。不建议用 alias 拼出旧版兼容行为。

## 配置错误

### 缺少 profile / schema 不符 / TOML 字段错误

```sh
envwrap profile list
envwrap profile show work-v2
envwrap profile create work-v2 --help
```

启动不自动创建配置。旧 profile 不自动迁移；按创建帮助新建不同名称的 schema 2 配置。解析错误会省略字段值，避免把 URL、密码或 seed 带进日志；需要在本机私下检查原始 TOML。

### 文件权限不安全

用 `stat` 检查所选 profile 的 owner 和 mode。仅在确认文件确实属于你、路径正确后修正为用户私有权限，例如 `chmod 600`。不要给整个 HOME 递归改权限，不要通过 sudo 启动 envwrap 来绕过 owner 检查。

### 代理来源变量缺失

`--proxy-env EGRESS_PROXY` 记录的是变量名，而不是创建时的值。每个启动 shell 都要安全提供并导出它；或按配置参考改为私有 URL 存储。不要执行 `env`、`printenv EGRESS_PROXY` 等命令后把全部输出发到 Issue。

### 时区或 locale 不存在

```sh
locale -a
```

选择真实安装的 locale，并确认所选 `/usr/share/zoneinfo/...` 是有效 TZif 文件。需要新增时区/locale 时使用发行版正常管理方式；envwrap 不替你生成宿主 locale，也不会假装不存在的值已经生效。改变 `TZDIR` 不能覆盖 profile 的基底检查。

## 系统能力错误

| 现象 | 排查方向 |
| --- | --- |
| `required helper ... is missing` | 检查 PATH 和 bwrap/ip/nft 安装；程序还会检查 `/usr/bin`、`/usr/sbin` |
| bwrap 不认识 `--ro-overlay` | 已装版本太旧；测试版本是 0.11.1，需要支持只读 overlay |
| user namespace / mount / operation not permitted | 宿主策略、容器嵌套限制或内核能力不允许；不是配置密码错误 |
| dummy、TPROXY、REDIRECT、IPv6 或 sysfs 相关失败 | 核对内核/发行版支持，尤其裁剪内核及受限容器环境 |
| `GLIBC_2.39 not found` | 当前预编译包不兼容该 libc；在目标主机从源码构建，不替换系统 libc |

不要通过禁用全局 AppArmor/SELinux、修改全局防火墙或关闭 IPv6 来“让错误消失”。envwrap 不会做这些改动；需要管理员提供受支持环境时，应在运行程序前明确解决依赖。

## 代理和 DNS 错误

### `configured proxy cannot establish the DNS TCP path; application was not started`

这表示启动预检没有成功通过代理连接到指定 DNS 的 TCP 端口。可能原因：

- 代理客户端未运行，IP/端口错误或不可达；
- 协议与服务不匹配，认证失败；
- SOCKS5/HTTP CONNECT 策略不允许该 DNS IP 或 TCP 53；
- 所选 DNS 仅支持 UDP，或服务没有监听对应 TCP 端口。

此时应用没有被启动。不是“自动转为直连”或“用宿主 DNS 兜底”。选择真正能经该代理访问的 DNS 服务后再运行 doctor；不要因为一次失败就扩大 LAN 白名单。

### doctor 能启动，但 DNS 查询失败

代理建连成功并不代表 resolver 能返回有效答案。检查 resolver 的 TCP DNS 服务和返回码；若有内部 DNS 后缀，确认规则与解析器地址正确。全域后缀 `.` 会把包括根域 NS 探针在内的所有 DNS 交给内部解析器，属于显式直连选择。

### DNS 正常，但某个网站/API 失败

doctor 不是对所有目标地址、代理规则或 Claude 服务的可达性证明。目标可能被上游代理拒绝，服务可能不可用，TLS/账号也可能失败。保存脱敏的错误类型与具体发生阶段，不上传认证头或请求正文。

如果强制 IPv4 的 HTTPS 请求成功，而默认地址族或 IPv6 请求出现 TLS EOF/reset，应对照检查代理的 IPv6 出站能力。原生 SOCKS5 对照必须使用同一个目标 IPv6 地址并保留原始 TLS SNI；只把域名交给 SOCKS5 解析，代理可能选择 IPv4，不能据此证明 IPv6 可用。

透明 TCP 转发的本地握手可能早于上游连接完成。上游 IPv6 失败可能表现为连接后的 EOF/reset，应用的 Happy Eyeballs 不一定会再尝试 IPv4。envwrap 不会自动过滤 AAAA、关闭 IPv6 或绕过代理。`curl -4` 只能帮助定位问题，不是通用修复；应修复代理的 IPv6 路径，并重新验证实际应用。

### QUIC、游戏或其它 UDP 应用不工作

当前只代理外部 TCP；外部非 DNS UDP 被阻断，不实现 SOCKS5 UDP ASSOCIATE。应用支持 TCP 时可由它自行回退；不支持则不属于当前兼容范围。不要承诺全部 UDP 都会通过 SOCKS5。

## 本机服务、MCP、回调

- 子环境与宿主并非共享一个网络 namespace。文件路径 Unix socket 保留；宿主 abstract Unix socket 不桥接。
- 本机 TCP/UDP 与显式 LAN 是受信直连例外。只为确实信任的服务添加范围。
- OAuth 等子程序 TCP listener 发布到宿主 loopback 有发现延迟。刚启动即回连失败时，先确认 listener 已建立、地址族正确、宿主端口没有冲突。
- 宿主同端口已有服务时，会话失败而不是错误地转发到那个服务；关闭冲突实例或使用应用支持的其它端口配置。
- 不发布反向 UDP，不把 IPv6 wildcard 监听自动解释为 IPv4 可用，也不自动将 listener 暴露给宿主 LAN。
- `SSH_AUTH_SOCK` 保留，但 agent 本身必须存在且允许当前用户连接。envwrap 不生成或导入 SSH 私钥。

## Claude 兼容性排查

先做不消耗模型会话的对照：

```sh
claude --version
envwrap --profile work-v2 claude --version
claude --help
envwrap --profile work-v2 claude --help
```

在本机通过 Claude 官方界面检查登录状态；不要把认证文件、完整诊断包或环境转储贴进 Issue。原生未登录时，包装启动不会替你获得登录。

如果原生与包装都出现同样错误，应先定位 Claude 本身或原配置。原生对照可能使用不同网络路径：有敏感请求时，**不要为了排查擅自改为未代理的真实对话**。优先比较版本、帮助、登录来源和配置路径；是否做带请求的对照由操作者明确决定。

不要为“修复”而删除 `~/.claude`、切换空 HOME、清空所有环境或强制关闭遥测。这些做法会改变原配置，不是本项目承诺的兼容模式。

## 退出、Ctrl-C 与残留目录

正常退出会清理会话目录并结束子环境后代进程。Ctrl-C 应交给实际终端程序处理，保留应用自定义退出码。程序退出后，后台任务不会被当成独立服务继续保留。

如果父进程被 SIGKILL，可能留下用户私有 `/tmp/envwrap-*` 文件。确认对应会话已结束、目录属于你且确实由该次运行创建后再删除；不要用通配符批量删除其他正在运行会话的目录。进程终止保证不依赖删除这些目录。

## 部署检查与可选应用体验记录

发布门槛见[独立验收标准](acceptance.md)，不要求 Claude 登录或付费对话。部署到自己的机器后，应记录：

- envwrap、bwrap、目标应用版本，架构与 libc；
- profile 名、代理协议及声明例外，不记录认证 URL；
- 实际代理下的 doctor 结果与支持范围；
- 可选的原应用对话、恢复、MCP/hooks/插件/skills 或 OAuth 体验结果，未执行时保持未执行。

普通程序使用 `envwrap PROGRAM ARGS`；`claude`、`codex` 等不是专用子命令。目标程序未安装时先通过其官方方式安装，不把 PATH 缺失当作代理问题。若程序名与管理命令冲突，使用 `envwrap -- doctor` 这样的显式转义。

独立检测不证明闭源请求正文或服务端行为；也不能用“checker 分数为零”替代真实网络验证。用户要求先测试再发布时，仍应等待明确的测试确认。
