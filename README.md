# envwrap

**保留原生应用配置的 Linux 透明代理与声明式容器身份运行层。**

```sh
envwrap claude
envwrap codex
envwrap agy
envwrap git fetch
```

`envwrap PROGRAM [ARGS...]` 启动 PATH 中的任意程序。**Claude、Codex、agy 都不是 envwrap 子命令，没有专用分支。** envwrap 不代为安装应用、不创建独立 HOME、不替换账号配置；仅增加明确的网络路径和一组一致的 Linux 容器身份接口。

当前正式版本 **0.3.0**。支持通用入口、SOCKS5h 域名解析和独立环境/网络验收。[GitHub Release](https://github.com/jacek4yang/envwrap/releases/tag/v0.3.0) 提供源码与二进制；[crates.io 收录状态](https://crates.io/crates/envwrap) 独立于 GitHub 发布，未收录时使用下面的 Git 安装方式。历史 `v0.3.0-rc.1` 保持不变。

## 文档导航

| 文档 | 内容 |
| --- | --- |
| [独立验收标准](docs/acceptance.md) | 最终发布门槛、checker 交叉检测、逐项判定与证据 |
| [运行与隐私边界](docs/runtime.md) | 流量策略、身份矩阵、继承内容、权限与限制 |
| [配置参考](docs/configuration.md) | 命令、schema 2、代理来源、DNS/LAN、身份生命周期 |
| [架构讲解](docs/architecture.md) | namespace、内核转发、broker、DNS、回调与进程管理 |
| [故障排查](docs/troubleshooting.md) | 配置、依赖、代理、DNS、locale、应用兼容性 |
| [维护与发布](docs/releasing.md) | 验证、源码/二进制打包、GitHub 与 crates.io 发布 |
| [版本记录](CHANGELOG.md) | 当前变化与历史发布 |

## 能保证什么，不能保证什么

| 流量 / 信息 | 行为 |
| --- | --- |
| 普通应用及其子进程的对外 IPv4/IPv6 TCP | 经指定 HTTP CONNECT 或 SOCKS5 代理，不依赖应用读取代理变量 |
| UDP/TCP DNS | 推荐 `socks5h`：A/AAAA 使用会话内路由地址，连接时交给代理解析域名；其它记录经代理访问指定 DNS |
| 不支持的对外 UDP，例如 QUIC/HTTP3 | 阻断，不直连；应用是否回退 TCP 由应用自身决定 |
| localhost、明确配置的 LAN/内部 DNS、文件路径 Unix socket | 受信例外，不宣称全部经过代理 |
| hostname、machine-id、时区、locale、MAC、声明 DMI 接口 | 按 profile / 真实 namespace 提供一致视图 |
| OS、内核、CPU、内存、磁盘基底、原应用配置 | 保留，不伪造系统能力 |
| HTTPS 正文、账号、请求内容、服务端判断、任意应用遥测 | 不解密、不改写、不承诺控制 |

网络后端失败不会自动退回宿主外网直连。这不是不可信程序的文件安全沙箱，也不防止恶意同用户程序读取用户文件或调试进程。

## 1. 依赖

- Linux；发布打包路径目前验证的是 x86_64 GNU/Linux。
- 普通用户可用的 user/mount/net/pid/UTS namespaces。
- `bwrap`：必须支持 `--ro-overlay`，已验证 bubblewrap 0.11.1。
- `ip`（iproute2）、`nft`（nftables）。
- 内核支持 IPv6、dummy 接口、REDIRECT/TPROXY、sysfs 和只读 overlay。
- 已安装所选时区和 locale；目标应用自行安装并在 PATH 中。

```sh
bwrap --version
bwrap --help
ip -Version
nft --version
locale -a
```

系统依赖/locale 安装可能需要管理员权限；**envwrap 日常运行不用 sudo，不修改宿主防火墙、sysctl 或安全策略**。依赖不满足时失败，不绕过保护继续启动。

## 2. 安装

直接从正式 Git 标签安装，不依赖 envwrap 已被 crates.io 收录：

```sh
cargo install --git https://github.com/jacek4yang/envwrap --tag v0.3.0 --locked
hash -r
command -v envwrap
envwrap --version
```

已验证工具链为 Rust 1.98.1，使用 edition 2024 和 `Cargo.lock`。存在多个 envwrap 安装时，以 PATH 顺序为准。升级前保留旧二进制和私有配置备份。

或在检出的源码目录安装：

```sh
cargo install --path . --locked
```

仅当 crates.io 已收录此版本时，也可执行：

```sh
cargo install envwrap --version 0.3.0 --locked
```


也可以从 [GitHub Release](https://github.com/jacek4yang/envwrap/releases/tag/v0.3.0) 下载二进制与 `SHA256SUMS`。GNU 二进制的 libc 要求以该包的 `BUILD-INFO.json` 为准；之前的 RC 包最高 GLIBC 符号需求为 2.39，不能当作通用静态二进制。仓库和发布资产公开可读。

## 3. 显式配置

首次运行不自动创建 profile，不查询 GeoIP，不猜测你的出口城市。

以 Bash 为例，安全输入代理 URL，避免写进命令历史：

```sh
read -r -s -p 'Proxy URL: ' EGRESS_PROXY
printf '\n'
export EGRESS_PROXY
```

**推荐 `socks5h://127.0.0.1:10808`，让代理负责域名解析和目标地址族选择。** 同时支持 `socks5://` 和 `http://`。代理端点须为 IP 字面量，IPv6 使用方括号。HTTP CONNECT/SOCKS5 自身不加密代理认证或链路；远程链路保密需要受信的加密隧道客户端。

下面示例明确选择洛杉矶时区、美国英语和一个默认 DNS。请按实际需要选择，不是运行时自动推断：

```sh
envwrap profile create default \
  --proxy-env EGRESS_PROXY \
  --dns 1.1.1.1:53 \
  --timezone America/Los_Angeles \
  --locale en_US.UTF-8

envwrap doctor
envwrap claude
```

`en_US.UTF-8` 必须真实安装。没有“洛杉矶专属语言”；这里选择美国英语，洛杉矶时区会按时区数据库自动处理冬夏令时。**设置时区/语言不会改变代理真实出口 IP 或其地理位置。**

如果不想每次导出代理变量，可在私有 TOML 的 `[network.proxy]` 中删去 `env`，改成：

```toml
[network.proxy]
url = "socks5h://127.0.0.1:10808"
```

这是配置片段，不是完整 profile。文件保持 `0600`，不提交仓库。配置位置、其它字段与备份方式见[配置参考](docs/configuration.md)。已有旧 `default` 时先备份，或创建 `work-v2` 并用 `--profile work-v2` 选择；创建命令不会覆盖旧文件。

### SOCKS5h 与 SOCKS5 的区别

| URL | 域名与地址选择 |
| --- | --- |
| `socks5h://`（推荐） | A/AAAA 查询返回会话内路由地址；连接时发送 SOCKS5 域名目标，由代理解析、选择 IPv4/IPv6。不会为这些查询访问 `1.1.1.1`，也不做出口探测 |
| `socks5://` | DNS 经代理传送给 `network.dns`；应用连接真实 IP。默认 `dns_preference = "auto"`，每次启动经代理向 `https://api64.ipify.org/` 查询出口地址族 |
| `http://` | DNS 经代理传送给 `network.dns`，默认保留双栈 |

SOCKS5 自动偏好：观测到 IPv4 出口时，双栈域名的 AAAA 查询返回无地址；观测到 IPv6 时对 A 查询做相应处理。**单栈域名保留其唯一地址族**，不会为 IPv6 字面量做 IPv4 替换。观察值只是该次请求的出口，不代表所有目标或所有节点的能力；代理切换后下一次启动重新探测。探测只发送固定 HTTPS 请求、不携带应用认证、不记录返回 IP，失败则不启动应用，不直连兜底。

不需要外部出口查询时，在现有 `[network]` 表内设 `dns_preference = "ipv4"`、`"ipv6"` 或 `"dual"`，也可在创建时用 `--dns-preference`；`socks5h` 保持 `"auto"`，地址选择由代理负责。

**兼容性边界：** SOCKS5h 的 DNS 地址是路由标记，不是服务器真实地址；不能跨会话保存使用，不适合要求真实 DNS 地址或自行校验 DNSSEC 的应用。HTTPS/SVCB 的 IP hints 返回 NODATA，促使客户端使用 A/AAAA；TXT、MX、NS 等仍通过配置的 DNS 解析。不存在的域名通常在代理连接阶段失败，而不是先得到 NXDOMAIN。IP 字面量、`/etc/hosts` 和自带 DoH/DoT 得到的地址仍按 IP 转发。显式内部 DNS 规则优先，不受合成地址或地址族偏好影响。详见[配置参考](docs/configuration.md)。

配置后可直接验证，不必给 curl 加 `-4`：

```sh
envwrap curl https://ip.sb
```


## 4. 通用命令规则

```sh
envwrap claude --resume
envwrap codex --help
envwrap agy
envwrap git fetch
envwrap python3 your_program.py
envwrap --profile work-v2 claude --version

# profile 和 doctor 是 envwrap 保留管理命令。
# 如果真正要运行的程序恰好叫 doctor，用 -- 转义：
envwrap -- doctor --help
```

- envwrap 的选项必须放在程序名前，例如 `--profile work-v2`。
- 程序名后所有参数属于程序，包括 `--help`、`--version`、`--profile`，不再重新解析。
- `--` 不是普通程序启动的必需项；它用于转义保留名或以 `-` 开头的可执行路径。
- 支持非 UTF-8 程序路径与参数；不做变量插值，不加隐式 shell。
- `run` 不再是 envwrap 子命令。旧 `envwrap run -- git ...` 改为 `envwrap git ...`。
- 找不到目标应用会失败，不自动安装，也不退回未保护启动。

管理配置：

```sh
envwrap profile list
envwrap profile show default
envwrap profile create work-v2 --help
envwrap --profile work-v2 doctor
```

同一 profile 跨启动稳定、跨程序共享身份；不是 HOME 或账号分区。`show` 隐去 URL 和 seed，不能作为可回填的配置备份。

## 5. 独立环境验收，而不是依赖 Claude 闭源内部

最终验收标准为 `envwrap-independent-v2`：独立程序通过系统调用、procfs/sysfs、文件和真实 socket 观测运行环境，再按明确支持范围判断；分别覆盖 HTTP CONNECT、SOCKS5、SOCKS5h 及 DNS 偏好。**不以 Claude 登录、一次对话或外部 checker 的风险分数为零作为发布门槛。**

```sh
cargo build --release --locked
python3 -B scripts/acceptance.py --binary target/release/envwrap

# 可选：用已有 checker 交叉核对；只读离线扫描，不修改那个项目。
python3 -B scripts/acceptance.py --binary target/release/envwrap \
  --reference-checker "$HOME/Projects/claude-code-env-checker/target/release/claude-code-env-checker"
```

环境、TCP/DNS/UDP、故障不直连、终端/信号/进程树、真实 Git/SSH agent 仍是强制验收项。继承项与缺失的条件性接口单独标记，不把 `NOT_EXECUTED` 当作通过。详细矩阵、报告隐私和本机使用前检查见[独立验收标准](docs/acceptance.md)。

闭源应用的对话与扩展可以由操作者补充体验测试，不能据此证明其全部遥测行为。使用前仍应针对**自己配置的代理**运行 `doctor`，确认原应用已安装且有正常登录/配置；登录由应用官方流程处理，envwrap 不索要账号密钥。

## 开发与许可证

```sh
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets -- --nocapture
python3 -B scripts/package_release.py --check-docs
```

测试需要 Python 3.11+、setpriv、Git、OpenSSH ssh-agent/ssh-add 及运行依赖。测试新建隔离网络和临时 HOME/profile，不访问公网或真实账号，不改变用户的实际 profile。

[MIT License](LICENSE)。
