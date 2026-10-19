# envwrap

**保留原生应用配置的 Linux 透明代理与声明式容器身份运行层。**

```sh
envwrap claude
envwrap run -- git fetch
```

`envwrap claude` 不管理另一套 Claude 安装或账号，不创建独立 HOME。它沿用原来的 Claude 配置、登录来源、项目、PATH 和工具链，在外层建立明确的网络路径和 Linux 容器身份视图。

**首个 Release：`v0.3.0-rc.1`，预发布版。** 本地网络、身份、终端与开发工具回归已通过；真实 Claude 登录后的对话、恢复及 MCP/hooks/插件/skills/OAuth 尚未完成专项验收。可以按下文受控试用，不把 RC 当成已经认证的生产稳定版。

## 文档导航

| 文档 | 内容 |
| --- | --- |
| [运行与隐私边界](docs/runtime.md) | 流量策略、身份支持矩阵、继承内容、能力与限制、已有验证证据 |
| [配置参考](docs/configuration.md) | 完整命令面、schema 2、代理来源、DNS/LAN 规则、身份生命周期、旧版迁移 |
| [架构讲解](docs/architecture.md) | 启动流程、内核转发、宿主 broker、DNS、回调、信号、模块分工 |
| [故障排查](docs/troubleshooting.md) | 配置、依赖、代理、DNS、locale、端口冲突、Claude 原生对照 |
| [维护与发布](docs/releasing.md) | 测试、构建、打包、校验和、版本与发布流程 |
| [版本记录](CHANGELOG.md) | 破坏性变更、修复和已知限制 |
| [首版发布说明](docs/releases/v0.3.0-rc.1.md) | 此次发布范围、安装资产、验收状态 |

## 能保证什么，不能保证什么

| 流量 / 信息 | 行为 |
| --- | --- |
| 普通应用及其子进程的对外 IPv4/IPv6 TCP | 经指定 HTTP CONNECT 或 SOCKS5 代理，不依赖应用读取代理变量 |
| 默认 UDP/TCP DNS | 受控处理，经代理向指定 DNS 服务发送 TCP 查询 |
| 不支持的对外 UDP，例如 QUIC/HTTP3 | 阻断，不改为直连；应用是否回退到 TCP 由应用决定 |
| localhost、明确配置的 LAN、内部 DNS、文件路径 Unix socket | 受信例外；详见运行边界，不宣称全部经过代理 |
| hostname、machine-id、时区、locale、MAC、声明的 DMI 接口 | profile / 真实 namespace 提供一致视图 |
| OS、内核、CPU、内存、磁盘基底、原应用配置 | 保留，不伪造执行能力 |
| HTTPS 正文、账号、请求内容、服务端判断、任意应用遥测 | 不解密、不改写、不承诺控制 |

网络后端失败不会自动退回宿主外网直连。代理凭据不会通过默认子环境或辅助程序 argv 传递，但这不是防止恶意同用户程序读取配置文件或调试进程的安全沙箱。

## 1. 准备运行环境

- Linux；首个二进制资产为 **x86_64 GNU/Linux**。
- 普通用户可用的 user/mount/net/pid/UTS namespaces。
- `bwrap`（bubblewrap，必须支持 `--ro-overlay`；已验证版本 0.11.1）、`ip`（iproute2）、`nft`（nftables）。
- 内核支持 IPv6、dummy 接口、REDIRECT/TPROXY、sysfs 和只读 overlay。
- 已安装你选择的时区与 locale。
- Claude Code 自行安装；直接运行 `claude --version` 应能找到它。

依赖安装属于系统管理员操作；**envwrap 日常启动不用 sudo，也不会修改宿主防火墙/sysctl/安全策略**。仅安装软件包不代表发行版策略一定允许这些功能，以 `doctor` 的实际启动结果为准。

```sh
bwrap --version
bwrap --help
ip -Version
nft --version
locale -a
claude --version
```

旧版 bubblewrap 没有 `--ro-overlay` 时，请先安装发行版支持的新版；不要通过关闭整个系统安全策略来掩盖启动失败。

## 2. 安装或并行试用

### 从 GitHub Release 下载

仓库目前为私有仓库，需要具有访问权限的 GitHub 登录。使用 GitHub CLI：

```sh
gh release download v0.3.0-rc.1 --repo jacek4yang/envwrap \
  --pattern 'envwrap-v0.3.0-rc.1-x86_64-unknown-linux-gnu.tar.gz' \
  --pattern 'SHA256SUMS' --dir envwrap-download
cd envwrap-download
sha256sum --check --ignore-missing SHA256SUMS
tar -xzf envwrap-v0.3.0-rc.1-x86_64-unknown-linux-gnu.tar.gz
./envwrap-v0.3.0-rc.1-x86_64-unknown-linux-gnu/envwrap --version
```

首版 GNU 二进制的最高 GLIBC 符号需求为 **GLIBC_2.39**，还需要 `libgcc_s.so.1`。这只是链接器要求，不代表已在每个 glibc ≥ 2.39 的发行版测试过。构建环境、精确提交、依赖库、二进制哈希在包内 `BUILD-INFO.json`；发行版/内核能力仍须本机诊断。不支持该二进制的主机请从源码构建，不要替换系统 libc。

可先保留旧入口，将候选版本安装为另一个名字：

```sh
install -Dm755 envwrap-v0.3.0-rc.1-x86_64-unknown-linux-gnu/envwrap \
  "$HOME/.local/bin/envwrap-rc"
"$HOME/.local/bin/envwrap-rc" --help
```

### 从源码构建

```sh
git clone git@github.com:jacek4yang/envwrap.git
cd envwrap
git checkout v0.3.0-rc.1
cargo build --release --locked
./target/release/envwrap --help
```

已验证工具链为 Rust 1.98.1。项目使用 edition 2024 和锁定的 `Cargo.lock`；本发布不声称验证了更低的 Rust 最低版本，也不提供未经验证的 musl/ARM 二进制。

后文使用 `envwrap` 作为命令名。并行试用时，将它替换为 `envwrap-rc` 或 `./target/release/envwrap`。

## 3. 显式配置，随后启动

首次运行不会自动生成 profile，也不会查询 GeoIP 或猜测你的出口国家。

### 安全设置代理来源

在 Bash 中交互输入完整代理 URL，避免 URL 进入 shell 历史和命令行参数：

```sh
read -r -s -p 'Proxy URL: ' EGRESS_PROXY
printf '\n'
export EGRESS_PROXY
```

支持 `http://`、`socks5://`、`socks5h://`。代理端点当前必须是 IP 字面量，例如本机代理客户端的 `socks5://127.0.0.1:1080`。IPv6 使用方括号。HTTP CONNECT/SOCKS5 本身不提供链路加密；远程链路保密应由受信的加密隧道客户端解决。

### 创建 profile

下面的 DNS、时区和 locale **只是明确的示例值**。请根据实际出口和已经安装的 locale 自行选择；`C.UTF-8` 是 UTF-8 基础 locale，不代表某个国家。

```sh
envwrap profile create default \
  --proxy-env EGRESS_PROXY \
  --dns 1.1.1.1:53 \
  --timezone Europe/Berlin \
  --locale C.UTF-8

envwrap doctor
envwrap claude
```

`doctor` 成功表示配置和相应运行路径通过了诊断，不等于完成 Claude 真实会话验收。若没有登录，请先在**原生 Claude** 中通过官方流程登录，再用同一 HOME 启动包装版本；不要把登录凭据交给 envwrap。

已有旧 `default` 配置时，创建不同名称，避免覆盖：

```sh
envwrap profile create work-v2 \
  --proxy-env EGRESS_PROXY --dns 1.1.1.1:53 \
  --timezone Europe/Berlin --locale C.UTF-8

envwrap --profile work-v2 doctor
envwrap --profile work-v2 claude
```

代理变量只在当前 shell 导出；下次启动前要重新安全设置，或按[配置参考](docs/configuration.md)把 URL 存进私有 profile。不会自动读取其它 ambient 代理作为备用。

## 4. 日常命令

```sh
envwrap claude --help
envwrap claude --version
envwrap claude --resume
envwrap run -- git fetch
envwrap run -- python3 your_program.py

envwrap --profile work-v2 claude --resume
envwrap profile list
envwrap profile show work-v2
```

- envwrap 的 `--profile` 放在 `claude` / `run` / `doctor` 前。
- `claude` 后的所有参数属于 Claude，原样传递；不会吞掉 Claude 自己的 `--help`。
- `run` 必须使用 `--` 分隔程序和参数。不做变量插值，不隐式加 shell。
- 同一 profile 跨启动稳定，跨程序共享身份；它不是 HOME/账号分区。
- 进程组、标准流与交互终端保留；会话结束时结束其中的后代进程，不用于启动脱离会话的守护程序。
- `profile show` 隐去代理 URL 与 seed，输出不是可回填的配置备份。

## 5. 把候选版本切换为日常入口前

至少完成：

1. 真实代理下 `doctor` 成功。
2. Claude 真实对话、流式输出、退出、`--resume` 正常，且没有重复登录或另一套配置。
3. 你实际使用的 MCP stdio/HTTP、hooks、插件、skills、Git/SSH agent 正常。
4. OAuth/localhost 回调正常；接受 50 ms 间隔端口发现带来的短暂竞态和明确的协议例外。
5. 确认[隐私支持范围](docs/runtime.md)，而不是把它当成全部遥测可控或恶意程序隔离。

验证通过后再安装为正常入口，并检查 PATH 中实际生效的版本：

```sh
# 在源码目录内；会更新 Cargo 管理的用户级二进制。
cargo install --path . --locked
hash -r
command -v envwrap
envwrap --version
```

如果同时存在 `~/.local/bin/envwrap` 和 `~/.cargo/bin/envwrap`，以 PATH 顺序为准，不要仅凭安装成功判断正在使用哪个版本。保留旧二进制用于回滚；schema 2 profile 不会自动兼容旧程序。

## 开发与许可证

```sh
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets -- --nocapture
```

全量测试还需要 Python 3.11+、setpriv、Git、OpenSSH ssh-agent/ssh-add；测试使用隔离的本地端点，不访问公网、不读用户账号密钥。完整发布流程见[维护与发布](docs/releasing.md)。

[MIT License](LICENSE)。
