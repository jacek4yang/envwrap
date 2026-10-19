# envwrap：网络与声明式 Linux 容器身份运行层

## 状态与边界

当前开发版本 `0.3.0` 采用通用 `envwrap PROGRAM` 入口，最终门槛为[独立环境与网络验收](acceptance.md)。Claude 登录/模型对话不是环境保护的证明，也不再是发布前提。本地验收通过后提供操作者测试；按操作者要求，确认之前不发布新 GitHub Release 或 crates.io 包。历史 `v0.3.0-rc.1` 保持不变。

目标：`envwrap PROGRAM` 使用原程序的 HOME、登录、配置、扩展、项目、PATH 和工具链，仅增加明确声明的网络路由和容器身份视图。`envwrap claude`、`envwrap codex`、`envwrap agy` 都遵循相同规则。

不提供：所有遥测内容控制、HTTPS 解密/改写、Claude 二进制修改、实体机伪装、独立 Claude HOME、文件安全沙箱、恶意同用户进程防护。正常遥测与应用现有设置保持不变。账号、请求内容、客户端版本及服务端判断不由 envwrap 决定。

## 依赖与安装

仅 Linux。需要可用的普通用户 user/mount/net/pid/UTS namespace、支持只读 overlay 的 bubblewrap、iproute2、nftables，以及内核 REDIRECT/TPROXY、IPv6、dummy 接口、sysfs 支持。

日常启动不需要 sudo；程序不会修改宿主防火墙、sysctl 或安全策略。依赖缺失、内核/发行版限制、profile 无效时直接失败，不退回未保护运行。

从源码构建：

```sh
cargo build --release --locked
./target/release/envwrap --help
```

独立验收通过后可用 `cargo install --path . --locked` 安装用户级入口；安装不修改原应用配置。首次使用前配置 profile 并在实际代理下运行 doctor。
后文的 `envwrap` 指新二进制。验收阶段请把它替换为 `./target/release/envwrap`，避免误用 PATH 中的旧版本。


## 首次配置

必须显式创建 profile。启动时绝不自动生成身份或根据宿主/出口推测国家、时区、语言。

```sh
# 私下设置 EGRESS_PROXY；不要把带密码的 URL 放在命令参数或共享日志里。
# 支持 http://、socks5://、socks5h://；端点要求 IP 字面量，IPv6 用 [IP]:port。
envwrap profile create default \
  --proxy-env EGRESS_PROXY \
  --dns 1.1.1.1:53 \
  --timezone Europe/Berlin \
  --locale en_US.UTF-8

envwrap doctor
envwrap claude
envwrap claude --resume
envwrap git fetch
```

以上时区、locale、DNS 只是示例，必须改成你明确选择的值。`locale -a` 查看已安装 locale；不存在的 locale 或 TZif 时区文件会拒绝启动。代理必须允许到所选 DNS 服务的 TCP 连接。每次启动先验证这条 CONNECT/SOCKS 路径，不进行 GeoIP 查询，不发送应用数据。`doctor` 另发一个根域 NS 查询，验证实际 DNS 策略的完整链路：默认走代理 TCP DNS，显式内部后缀 `.` 则走相应受信直连解析器。

`--profile NAME` 必须在程序名或管理命令之前。例如：

```sh
envwrap --profile work claude --help
envwrap --profile work python3 script.py
```

任意程序名后的所有参数原样交给程序；不吞掉 `--help`、`--version`、`--resume`。程序路径、参数和环境值支持非 UTF-8，不做变量插值，不加额外 shell。`profile`、`doctor` 是保留管理名，使用 `envwrap -- PROGRAM` 可转义。

## Profile

位置：`${XDG_CONFIG_HOME:-$HOME/.config}/envwrap/profiles/NAME.toml`。文件必须属于当前用户且没有 group/other 权限（通常 0600）。创建不覆盖已有文件。

```sh
envwrap profile create work --help
envwrap profile list
envwrap profile show work
envwrap profile delete work
```

`show` 隐去整个代理 URL 和身份派生 seed；输出用于检查，不是可直接重新导入的备份。直接编辑私有 TOML 文件修改配置。`network.proxy` 只能设置 `env = "变量名"` 或 `url = "代理地址"` 之一；不从其它环境变量猜备用代理。

Schema 2 的必需字段为 `version`、`name`、完整 `identity`、`network.proxy`、`network.dns`。创建命令一次性生成 hostname（可显式指定）、machine-id、product UUID、MAC 和派生 seed，跨启动稳定。同一 profile 用于不同应用时共享身份，不是账号或 HOME 分区。

可选网络字段：

- `direct = ["192.168.65.0/24", "fd12:3456::/64"]`：显式受信直连范围；默认没有任何私网范围。禁止默认路由、保留虚拟网段重叠、组播及 IPv6 link-local 范围。
- `internal_dns = [{ suffix = "corp.example", resolver = "127.0.0.53:53" }]`：该后缀及子域使用指定直连 DNS；最长后缀优先，大小写不敏感，按标签边界匹配。resolver 必须是宿主 loopback 或属于 `direct`。`.` 是显式全域规则，意味着所有 DNS 走这个受信本机解析器。
- `search = ["corp.example"]`：明确的 resolver 搜索域，默认空。

对应创建参数可重复：`--direct CIDR`、`--internal-dns SUFFIX=IP:PORT`、`--search DOMAIN`。

旧 profile 不做静默转换。旧 export/import/check/wrap、公开 sandbox、环境白名单、默认独立 HOME 和强制遥测禁用策略不属于新产品。

已有旧 `default` 时，新建不同名称（例如 `work-v2`），用 `envwrap --profile work-v2 ...` 验证。若最终需要直接 `envwrap claude`，请先自行私下备份旧 profile，再显式删除并重建 schema 2 的 `default`。这些操作不迁移、复制或删除 Claude 自身配置。

## 网络行为

| 流量 | 路径 |
| --- | --- |
| 外部 IPv4/IPv6 TCP | 私有网络 namespace 内核 REDIRECT → 宿主 broker → 指定 HTTP CONNECT/SOCKS5 代理 |
| UDP/TCP 目标端口 53 | 先执行 DNS 策略；默认通过代理向配置 DNS 发 TCP 查询；不因 loopback/LAN 直连例外而绕过 |
| 显式内部 DNS 后缀 | 向指定受信解析器直连；UDP/TCP 客户端保持相应协议，无备用宿主解析 |
| 子环境 localhost TCP/UDP | 优先子环境本机服务；不存在时访问宿主本机服务 |
| 显式 LAN 范围 TCP/UDP | 宿主直连，不宣称经过代理 |
| 其它外部 UDP、ICMP | 不提供对外通路；不伪造成功回应 |
| 文件路径 Unix socket | 原路径保留；SSH agent、MCP、本机服务属于受信例外；不提供宿主 abstract Unix socket 桥接 |

TLS 原样穿过代理；应用自行使用 DoH/DoT 时，其连接仍按 TCP 路由，但不改写加密的 DNS 内容。受信本机/LAN/Unix socket 服务可能再次转发请求，envwrap 不控制其后续流量。

虚拟接口 `eth0`：`198.18.0.2/30`、`fd23:656e:7677::2/64`；同时保留真实工作的 `lo`。这些地址与网关只属于本次私有 namespace，不修改宿主路由。真实内核负责 TCP 状态、流控、分片；不模拟 TCP/IP 栈。

辅助程序保留仅用于 namespace 内网络/挂载的能力；应用启动前清空 permitted/effective/inheritable/ambient capabilities 并启用 no-new-privileges。网络组件失败终止会话，无宿主外网直连回退。

### 宿主访问子程序回调

子程序 IPv4/IPv6 TCP loopback listener 自动发布到宿主相同 loopback 地址/端口，用于浏览器 OAuth 和本机开发回调。通配地址 listener 只发布到宿主相应地址族的 loopback，不自动扩大为宿主 LAN 暴露；IPv6 listener 不额外假定 IPv4 双栈语义。

这是 listener 发现与转发，不是 bind/listen 系统调用拦截：发现轮询间隔为 50 ms，实际可见时间受调度/控制链路影响，刚 listen 后立即从宿主连接存在短暂竞态。宿主端口冲突使会话失败，不转发到已有的其它服务。UDP 服务不提供反向端口发布。独立回调测试覆盖可用路径；不能描述为与共享宿主网络完全等价，实际应用可补充体验检查。

## 身份支持矩阵

| 字段/接口 | 来源与边界 |
| --- | --- |
| gethostname、UTS、`/proc/sys/kernel/hostname`、`/etc/hostname`、HOSTNAME | profile.hostname，一致 |
| `/etc/machine-id`、已有 `/var/lib/dbus/machine-id` 及其规范目标 | profile.machine_id，一致；不创建本来不存在的宿主 D-Bus 目录 |
| TZ、`/etc/timezone`、`/etc/localtime` symlink 和 TZif 内容 | profile.timezone，同一已安装时区；移除冲突的 TZDIR，验证 libc 与无 TZ 的文件读取者计算一致 |
| LANG、LC_ALL、其它 LC_* / LANGUAGE | profile.locale；先验证已安装，清除冲突的 ambient 值 |
| 网卡、MAC、地址、路由、netlink/ioctl/procfs/sysfs | 私有 namespace 的真实内核视图；MAC 来自 profile |
| 已存在的 `/sys/devices/virtual/dmi/id` 与 `/sys/class/dmi/id` 别名 | 明示 envwrap Linux container；product UUID/serial、board/chassis 标识来自稳定 profile；不声称物理 OEM/BIOS |
| `/proc/sys/kernel/random/boot_id` | profile seed 与真实宿主 boot ID 派生；同 profile 同次宿主启动稳定 |
| OS、内核、架构、CPU、内存、磁盘、执行性能/uptime | 继承真实基底，不伪造能力或容量 |
| 用户 HOME、应用配置、任意其它文件、硬件其它接口、应用遥测正文 | 不承诺覆盖；文件仍可包含原身份，主动搜寻不在威胁模型中 |

`/etc` 使用定向只读 overlay 提供身份/hosts/NSS/resolver 文件，不复制整个系统配置。`/sys` 重新挂载当前网络 namespace 的 sysfs，并仅替换声明的 DMI 目录；CPU、磁盘等基底继续可读。专用 nscd 缓存目录被屏蔽以避免其 Unix socket 绕过受控 DNS；不隐藏整个 `/run`、`/tmp` 或 HOME。

## 凭据与兼容性

仅父进程 broker 持有上游凭据；bootstrap 经私有 Unix socket 传递，不放入辅助程序 argv/env。子环境移除明确代理来源变量和大小写形式的 HTTP_PROXY/HTTPS_PROXY/ALL_PROXY/NO_PROXY/FTP_PROXY。其它应用凭据、CLAUDE_CONFIG_DIR、XDG、SSH_AUTH_SOCK、供应商端点、遥测和更新设置保留。

这防止常规误继承，不防同用户恶意读取 profile、父进程环境或进程调试。用户在 Claude 自己的设置中配置的代理/端点仍是用户设置，envwrap 不静默改写它们。

HTTP CONNECT 和 SOCKS5 本身不加密代理认证或隧道内容。应用 TLS 保持端到端；若需要保护宿主到远程代理之间的链路，使用受信的本机加密隧道客户端，不把这些协议描述成加密 VPN。

会话退出时，其 PID namespace 内的应用和后代进程一并结束；不用于启动脱离会话的常驻守护程序。应用不继承网络/挂载能力，启用 no-new-privileges，不能在会话内通过 setuid 程序提权。正常退出清理运行目录；SIGKILL 无法执行父进程清理，可能留下私有 `/tmp/envwrap-*` 文件，但进程树仍被终止。

## 验证与发布门槛

已执行：

```sh
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets -- --nocapture
cargo build --release --locked
```

当前 **8 项测试通过**：6 项 CLI 配置/失败/脱敏回归，2 项最终入口的完整网络、身份与生命周期场景（HTTP CONNECT、SOCKS5 各一轮）。测试额外需要 Python 3.11+、util-linux 的 setpriv、Git、OpenSSH ssh-agent/ssh-add；不访问公网，所有服务位于测试新建的外层网络 namespace，不改宿主策略。

- 透明 TCP v4/v6，预读、半关闭、连接建立后代理拒绝、增量流式传输，以及跨越 10 秒建连时限的空闲连接。
- 受控 DNS、内部后缀大小写/标签边界、EDNS/TC/持久 TCP；最大 EDNS 响应不会破坏 IPv4 转发。
- 显式本机/LAN v4/v6 例外，零长度与最大 UDP 数据报，对外 UDP 阻断，代理失败时对外 TCP 无直连回退、必要路径失败不启动应用。
- hostname/machine-id/DMI 别名、时区内容/路径/libc、locale、netlink/ioctl/procfs/sysfs 一致；身份和派生 boot ID 跨启动稳定，宿主文件未修改。
- 原 HOME、自定义配置、遥测设置、文件写入；真实 OpenSSH agent 协议与真实 Git clone/对象/工作树。
- 宿主→子环境 localhost v4/v6 回调；真实 PTY 的三路标准流、输入、SIGWINCH、Ctrl-C 和自定义退出码；SIGTERM/SIGKILL/辅助进程丢失后的进程树清理与描述符关闭。
- 回归已捕获并修复：Ctrl-C 提前杀死 bwrap reaper 导致退出码被覆盖、TZDIR 导致时区矛盾、误放在选项位置的代理 URL 被打印到错误日志。

release 二进制另经实际运行验证非 UTF-8 argv/env、字面量 `${HOME}` 不展开、二进制 stdin/stdout/stderr 和退出码传递；临时 smoke 脚本与测试 profile 已清理。

历史 RC 阶段曾对真实 Claude Code 2.1.302 做原生/包装的版本、帮助、认证状态和配置访问路径对照。该记录仅是应用兼容性证据，不证明闭源遥测，也不构成新验收标准的强制项。

当前最终验收直接运行普通程序，核对声明字段与 profile、有意继承字段与宿主、受控网络端点与故障行为。新增独立 host/child 观测覆盖 syscalls/procfs/sysfs、DMI 别名、冬夏时区、namespace/capabilities、CPU/内存/磁盘/用户配置；可选参考 checker 的离线字段交叉核对已实际执行。详情和报告格式见[独立验收](acceptance.md)。

实际部署仍须提供有效 profile、已安装的时区/locale、可用代理及目标程序。`doctor` 检查该部署的真实链路。应用登录由原应用管理，envwrap 不代为登录，不索要用户密钥。

## 当前切换与历史

- 旧 import/export/check/wrap、公开文件 sandbox、独立 HOME、环境白名单、强制遥测禁用和 telemetry-lab 已移除。
- 通用 `PROGRAM` 启动取代 RC 的 `run --` / Claude 专用分支；profile CRUD 与 doctor 保留。
- rootless 内核 REDIRECT/TPROXY + 宿主 broker；没有 TUN、TLS 中间人或自制 TCP/IP 栈依赖。
- 可识别的 Linux 容器身份与明确支持矩阵，不承诺“所有遥测由我们决定”。
- 发布历史与当前未发布变更见 [CHANGELOG](../CHANGELOG.md)。
