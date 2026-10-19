# 配置参考

[返回 README](../README.md) · [运行与隐私边界](runtime.md)

## 命令面与参数归属

| 命令 | 作用 |
| --- | --- |
| `envwrap --help` / `--version` | envwrap 自身帮助与版本；不需要 profile |
| `envwrap [--profile NAME] claude [ARGS...]` | 在受控运行环境启动 PATH 中的 Claude；后续参数全部属于 Claude |
| `envwrap [--profile NAME] run -- PROGRAM [ARGS...]` | 运行任意程序；不隐式执行 shell |
| `envwrap [--profile NAME] doctor` | 运行真实 namespace/身份/DNS 诊断，不启动 Claude 对话 |
| `envwrap profile create NAME [OPTIONS]` | 显式创建完整配置；不覆盖同名文件 |
| `envwrap profile create NAME --help` | 查看创建参数，不创建配置 |
| `envwrap profile list` | 列出配置名 |
| `envwrap profile show NAME` | 查看脱敏配置，不输出代理 URL 与 seed |
| `envwrap profile delete NAME` | 删除指定 profile；不删除 Claude 账号、配置、会话或项目 |

默认选择 `default`。`--profile` 放在命令前：`envwrap --profile work claude --resume`。缺少 profile 时失败，不自动创建；没有“找不到就不带保护运行”的行为。

envwrap 自身配置/启动错误返回状态 2；应用成功启动后保留其退出码，应用被信号终止时按 `128 + signal` 表达。错误处理者应区分“程序没有启动”和应用自身返回 2，不能仅凭数值推断来源。

## 创建参数

| 参数 | 必需 | 说明 |
| --- | --- | --- |
| `--proxy-env VAR` | 是 | 明确读取哪个环境变量中的代理 URL；存入配置的是变量名，不是当时的值 |
| `--dns IP:PORT` | 是 | 默认 DNS 的 TCP 上游地址，通过代理访问；IPv6 为 `[IP]:PORT` |
| `--timezone ZONE` | 是 | `/usr/share/zoneinfo` 下已安装的 TZif 时区，例如 `Europe/Berlin` |
| `--locale LOCALE` | 是 | 系统 libc 能创建的已安装 locale；用 `locale -a` 检查 |
| `--hostname NAME` | 否 | 小写 Linux hostname，1–63 字符，字母/数字/连字符，首尾为字母或数字 |
| `--direct CIDR` | 否，可重复 | 显式受信直连范围，不自动放行所有私网 |
| `--internal-dns SUFFIX=IP:PORT` | 否，可重复 | 为明确域名后缀指定受信直连解析器 |
| `--search DOMAIN` | 否，可重复 | resolver 搜索域；不要无意添加不受信域名 |

profile 名称匹配 `[a-z0-9][a-z0-9_-]{0,63}`。不支持通过名称引用其它目录。创建时验证配置、时区和 locale；启动时再验证代理来源与真实运行依赖。

## 存储、权限与备份

路径：`${XDG_CONFIG_HOME:-$HOME/.config}/envwrap/profiles/NAME.toml`。

- 文件必须为当前用户拥有的普通文件，不能给 group/other 开放权限；通常为 `0600`。
- 不接受未知 TOML 字段，不静默忽略拼错的键；schema 版本必须为 `2`。
- 配置读取有 1 MiB 上限。
- 手动编辑后保持私有权限。不要把包含 URL、seed 的完整 profile 放入仓库、Issue、聊天记录或公开备份。
- `profile show` 输出经过脱敏，**不是备份格式**；备份原始私有文件时由操作者负责加密与权限。
- 删除或重建 profile 不会重置 Claude 登录，也不隔离它的会话、缓存或工作目录。

## Schema 2 字段

使用 `profile create` 生成真实配置，然后只修改需要的字段。不提供可复制到多台机器上、共享固定身份的完整示例 profile。

| TOML 字段 | 类型 | 含义 |
| --- | --- | --- |
| `version` | 整数 | 必须是 `2` |
| `name` | 字符串 | 与所选 profile 名称一致 |
| `identity.hostname` | 字符串 | Linux 主机名 |
| `identity.machine_id` | 字符串 | 32 位小写十六进制，不能全部为零 |
| `identity.product_uuid` | 字符串 | 非全零、标准带连字符的小写 UUID；自动创建的是 UUIDv4 |
| `identity.mac` | 字符串 | 本地管理的单播 MAC，冒号分隔六组十六进制 |
| `identity.timezone` | 字符串 | 已安装 IANA 时区路径 |
| `identity.locale` | 字符串 | 已安装 locale |
| `identity.seed` | 字符串 | 64 位小写十六进制，不能全部为零；用于 profile 级 boot ID 派生 |
| `network.dns` | 字符串 | 默认 DNS `IP:PORT` |
| `network.direct` | 字符串数组 | 可选，默认空；受信 CIDR |
| `network.internal_dns` | 表数组 | 可选，默认空；每项有 `suffix` 和 `resolver` |
| `network.search` | 字符串数组 | 可选，默认空；搜索域 |
| `network.proxy.env` | 字符串 | 代理 URL 所在环境变量名；与 `url` 必须二选一 |
| `network.proxy.url` | 字符串 | 私下存储的代理 URL；与 `env` 必须二选一 |

`env` 变量名需符合常规 ASCII 标识符规则。选定变量不存在时启动失败，不去尝试 `ALL_PROXY`、桌面代理设置或别的 profile。

### 代理来源的两种方式

默认建议保存变量名，由启动 shell/你信任的凭据管理器提供值：

```toml
[network.proxy]
env = "EGRESS_PROXY"
```

也可编辑私有 profile，将上面的 `env` **删除**，替换成 URL。例如不带认证的本地 SOCKS 服务：

```toml
[network.proxy]
url = "socks5://127.0.0.1:1080"
```

这只是配置片段，不是完整 profile。不要同时保留 `env` 和 `url`。

支持 HTTP CONNECT、SOCKS5 无认证或用户名/密码认证；HTTP 使用 Basic 代理认证。userinfo 支持百分号解码。SOCKS5 用户名和密码若启用认证，则各为 1–255 字节。代理地址目前要求 **IP 字面量**，避免为连接代理自身进行不受控的 bootstrap DNS；不接受代理域名、`https://` 代理、路径/查询/fragment（空路径或单个 `/` 除外）。建议始终显式给出端口。

`socks5://` 与 `socks5h://` 在 envwrap 中使用同一受控 DNS 路径：普通 DNS 由你配置的 resolver 处理，然后以目标 IP 建立 SOCKS 连接。不要把其它工具里 `socks5h` 的远端域名解析语义直接套用到这里。

## DNS、LAN 与本机例外

默认所有目标端口 53 的 UDP/TCP 先进入 DNS 策略，包括应用试图向 localhost 或某个其它 DNS IP 发包的情况。默认 resolver 通过代理 TCP 访问；并非把宿主 `/etc/resolv.conf` 原样传进去。

例子：为公司后缀使用本机受信解析器，同时允许明确的内网网段：

```sh
envwrap profile create work \
  --proxy-env EGRESS_PROXY --dns 1.1.1.1:53 \
  --timezone Europe/Berlin --locale C.UTF-8 \
  --direct 192.168.65.0/24 \
  --internal-dns corp.example=127.0.0.53:53 \
  --search corp.example
```

先确认这确实是你信任且能够访问的网络和解析器；上面不是对任意用户都正确的默认配置。

- 内部 DNS 最长后缀优先，大小写不敏感，按域名标签边界匹配；`corp.example` 不匹配 `notcorp.example`。
- 内部 resolver 必须在 loopback 或已明确配置的 `direct` 范围内。
- 后缀 `.` 是**全部域名使用内部解析器**的显式选择，包括 doctor 的根域查询。此时不能宣称 DNS 全部经过 SOCKS5。
- `direct` 不允许默认路由、与保留虚拟地址重叠的范围，以及不支持的组播/link-local 等范围；不要用大网段绕过明确例外设计。
- 本机/LAN/Unix socket 服务是受信边界。它们若再转发到公网，该后续流量不由 envwrap 约束。
- DoH/DoT 的加密正文不改写；TCP 连接仍遵守路由规则。对外 QUIC/其它 UDP 阻断。

## 身份的生命周期

创建时一次性生成 hostname（可指定）、machine-id、product UUID、MAC 和 seed。启动不重新随机化，不从出口 IP 查询国家/城市，不创建缺失字段。换代理 URL不会自动改变身份、时区或语言；出口与声明是否匹配由操作者维护。

同一 profile 在同一次宿主启动下有稳定的派生 boot ID；宿主重新启动后 boot ID 改变。其它保存的身份字段不会因此变化。不同 profile 是不同的声明身份，但仍可访问同一 HOME，所以不是账号隔离工具。

envwrap 覆盖 TZ/LANG/LC_ALL/HOSTNAME，并去除冲突的 TZDIR、LANGUAGE、LC_* 和代理来源变量/常见代理变量；保留应用凭据、CLAUDE_CONFIG_DIR、XDG、SSH_AUTH_SOCK、遥测及更新设置。完整接口范围见[身份支持矩阵](runtime.md#身份支持矩阵)。

## 从旧版本迁移

这是破坏性切换：旧的 export/import/check/wrap、公开文件 sandbox、环境白名单与独立 HOME 不再支持；没有兼容别名和静默转换。

1. 保留旧二进制与私有配置备份。
2. 用不同名称创建 schema 2 profile，例如 `work-v2`。
3. 用新二进制显式选择该 profile，先 doctor，再真实 Claude 验收。
4. 如需直接 `envwrap claude`，完成验证后自行备份旧 `default`，再显式删除并重新创建新的 `default`。
5. 最后更新日常 PATH 入口。

此过程不会替你迁移旧独立 HOME 中的 Claude 登录数据。是否需要恢复那部分应用配置，由操作者通过 Claude 官方方式处理，不复制未知凭据文件。
