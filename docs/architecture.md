# 架构讲解

[返回 README](../README.md) · [产品边界](runtime.md)

## 设计目标

一个运行引擎服务任意命令，Claude、Codex、agy 都只是被启动的普通程序，无专用子命令分支。职责是**明确网络路径 + 一组一致的容器身份接口**，不是文件访问控制、账号隔离、TLS 中间人或模拟整台物理机器。

使用真实 Linux 网络栈处理 TCP、UDP、流控与分片，避免维护另一套用户态 TCP/IP 栈。namespace 内规则隔离于宿主；宿主不需要被改成全局代理。

## 进程与流量关系

```text
用户终端
  |
  +-- envwrap 父进程（宿主网络 namespace）
       |-- profile / 原应用环境 / 生命周期监督
       |-- broker：持有代理凭据，执行代理或明确直连策略
       |-- localhost 回调发布
       |
       +-- bubblewrap / 运行辅助进程
            |  私有 user + mount + network + PID + UTS namespaces
            |  定向身份挂载；真实 lo / eth0 / 内核 TCP/IP
            |
            +-- Claude 或指定命令及其后代
                 |  无网络/挂载 capabilities；no-new-privileges
                 |
                 +-- 对外 TCP -> 内核 REDIRECT -> 私有 relay
                 |              -> 私有 Unix broker 通道 -> HTTP CONNECT / SOCKS5
                 |
                 +-- UDP/TCP DNS -> DNS 策略
                 |                 -> 默认代理 TCP resolver / 明确内部直连 resolver
                 |
                 +-- 明确 loopback / LAN -> 受信直连路径
                 +-- 其它对外 UDP -> 丢弃
```

文件路径 Unix socket 保留原有可访问性，不保证经 broker。host abstract Unix socket 不提供桥接。

## 一次启动的顺序

1. **参数解析。** 选择现有 profile；第一个程序名后的参数不再由 envwrap 解释。普通启动不需要分隔符；`-- PROGRAM` 用于转义保留管理名或以连字符开头的程序路径。
2. **静态和基底验证。** 校验 schema/权限/字段，检查已安装 TZif 和 locale，定位 bwrap/ip/nft。任何失败都不启动应用。
3. **代理路径预检。** 从明确来源取得代理配置，通过代理尝试连到默认 DNS TCP 地址。此步不查询 GeoIP，不发送应用请求；也不等于验证了 DNS 响应内容。
4. **建立宿主 broker。** 创建用户私有运行目录和鉴权 Unix 通道。凭据只用于宿主 broker，不进入辅助程序 argv/env。
5. **构建 namespace 和声明视图。** 原根目录及 HOME 保留；对 `/etc` 作定向只读 overlay；提供 profile hostname/machine-id/TZ 等视图；挂载适合新网络 namespace 的 sysfs。
6. **建立真实内核网络。** 创建 lo、dummy eth0、IPv4/IPv6 地址、路由及仅作用于该 namespace 的 nft REDIRECT/TPROXY 规则。
7. **准备应用。** 经私有控制通道接收原应用 argv/env，清理明确冲突的身份/代理变量；清空能力并启用 no-new-privileges，然后启动真实程序。
8. **监督与退出。** 父进程、运行辅助进程和 broker 协同处理信号与关闭。网络辅助链路失败结束会话，不把应用搬回宿主网络继续运行。

## TCP 与 DNS 为什么不是普通环境变量代理

普通 `ALL_PROXY` 只有应用主动支持才生效。这里应用发出的连接先经过私有 namespace 的内核路由/转发规则：TCP 捕获原目标地址后交给宿主 broker，broker 才连接指定代理。

TCP 的重传、拥塞控制、窗口和分片由 Linux 处理。转发层负责双向流、背压、半关闭、取消；建连/握手有时限，已建立的连接不因同一个建连时限被误判为空闲超时。

DNS 不是让代理库按各自默认方式解析。应用看到受控 resolver 视图，常规 NSS 使用 `files dns`；专用 nscd 缓存路径被屏蔽，避免该 socket 绕过 DNS 路径。端口 53 查询进入统一规则：默认代理 TCP，显式内部后缀则直连对应受信 resolver。`/etc/hosts` 仍可参与本地名称解析。

## localhost 与回调

子程序连接 localhost 时，优先访问子环境内服务；不存在时再走宿主 loopback 路径，以兼容本机 MCP、代理客户端与开发服务。

反向方向不能靠 `--unshare-net` 自动工作：浏览器在宿主，而 OAuth listener 在子环境。辅助进程发现子环境 TCP listener，宿主侧在对应 loopback 地址/端口建立转发。通配监听只发布到相应地址族 loopback，不扩大到宿主 LAN。

**限制：** 发现轮询间隔 50 ms，实际延迟还取决于调度。刚监听就立即回连可能遇到竞态；宿主端口已占用则失败；不提供 UDP 反向发布，不假定 IPv6 listener 自动覆盖 IPv4。因此仍需要真实 OAuth/本机服务验收。

## 身份并不是全机克隆

profile 提供稳定声明，接口尽量一致：UTS/hostname 文件/环境、machine-id 的已存在别名、TZ/TZif、locale、网卡 MAC、DMI 声明、派生 boot ID。

DMI 明示 `envwrap` / `Linux container`，不冒充某个厂商实体机。OS、内核、CPU、内存、磁盘和执行性能继续来自真实系统。应用仍能读自己的原配置和许多宿主文件；文件中可能含原身份。隐藏一切历史信息既不是当前功能，也不能从几个覆盖字段推导出来。

## 权限与故障边界

- 辅助程序只在新建 user/network/mount namespace 中使用建网和挂载所需能力；应用能力清空。
- 原文件系统可写部分仍按原用户权限工作。这不是不可信代码执行沙箱。
- 代理失败不建立目标公网直连；显式受信本机/LAN 例外是另一类政策，不能混为 fail-open。
- 同 UID 恶意程序可能读用户配置/进程信息、通过受信服务间接联网；这不在隔离保证内。
- 正常退出清理运行目录。父进程 SIGKILL 无法运行清理代码，可能留下私有 `/tmp/envwrap-*`；PID namespace 与父死亡处理负责终止后代，而不是依赖临时目录清理来阻止残留程序。
- 不修改 TLS、不安装根证书、不替换 Claude 可执行文件、不强行禁用正常遥测。

## 源码地图

| 模块 | 职责 |
| --- | --- |
| `src/cli.rs` | 公共参数、profile CRUD、doctor 应用内探针 |
| `src/profile.rs` | schema、字段/权限验证、私有持久化、脱敏展示 |
| `src/runtime.rs` | 预检、bubblewrap 启动、控制通道、信号/退出监督 |
| `src/identity.rs` | 声明文件、别名挂载素材、环境处理、身份预检 |
| `src/kernel.rs` | namespace 内真实接口、路由、nft 与透明 socket |
| `src/network.rs` | 子环境 TCP/UDP/DNS 的捕获、转发和生命周期 |
| `src/broker.rs` | 宿主侧路由政策、上游代理与受信直连 |
| `src/callback.rs` | 子环境 listener 发现、宿主 loopback 发布 |
| `src/dns.rs` | DNS 协议处理、后缀路由、报文边界 |
| `src/upstream.rs` / `src/proxy.rs` | HTTP CONNECT/SOCKS5 握手和地址解析 |
| `src/wire.rs` | 私有通道协议、帧和共享边界 |
| `src/transport.rs` / `src/relay.rs` | 可取消 I/O、时限、背压、双向流与半关闭 |
| `tests/cli.rs` | 配置、失败不启动、私有权限与脱敏回归 |
| `tests/runtime.rs` / `tests/fixtures/` | 真正的 rootless 网络、身份、PTY、进程与开发工具验收 |

最终门槛是[独立环境与网络验收](acceptance.md)：独立系统调用/文件观测与真实 socket 测试，可选使用外部 checker 交叉验证。应用会话未执行状态不再阻塞环境验收，也不被描述为已通过。
