# 独立环境与网络最终验收

[返回 README](../README.md) · [运行边界](runtime.md)

## 标准：envwrap-independent-v2

验收对象是 envwrap 对普通程序提供的**实际环境与网络行为**，不是 Claude 闭源实现。Claude 登录、真实模型对话、MCP/插件内部行为不再是发布的强制前提；它们可以作为可选的应用体验记录，未执行仍记为未执行。

独立检测也不能证明应用内部没有其它遥测。通过本标准表示已声明接口和网络政策有实际观测证据，不表示控制了全部请求正文、服务端判断或宿主历史文件。

## 强制矩阵

| 类别 | 实际观测与判定 |
| --- | --- |
| 主机名 | gethostname、uname、`/etc/hostname`、procfs、环境变量与 profile 一致 |
| 持久身份 | machine-id、已有 D-Bus 别名及规范目标一致；DMI 的 class/devices 两条路径与声明一致 |
| boot 身份 | 与宿主不同，同一 profile 重启稳定；不把宿主 boot ID 原样传入 |
| 时区与语言 | TZ、TZDIR 清理、timezone/localtime 路径及 TZif 内容、libc 冬夏固定时刻、无 TZ 文件消费者、LANG/LC_ALL/字符集一致 |
| 网卡与地址 | sys/class、sys/devices/virtual/net、procfs、if_nameindex、netlink/ioctl 实际视图一致，只出现该 namespace 的接口 |
| 权限与 namespace | user/mount/net/PID/UTS 与宿主不同；应用能力清空、no-new-privileges 生效 |
| 有意继承 | OS、内核、CPU 型号/flags/affinity、内存总量、磁盘设备/容量、用户、HOME/PATH/原配置与基线一致，不仅检查非空 |
| 外部 TCP | 普通 socket 的 IPv4/IPv6 连接真实经 HTTP CONNECT/SOCKS5；流式传输、背压、半关闭、拒绝及建连时限后的空闲连接 |
| DNS | UDP/TCP、libc getaddrinfo、内部后缀边界/大小写、EDNS/TC/持久 TCP、最大报文；默认不退回宿主 DNS |
| SOCKS 解析语义 | SOCKS5h 的 A/AAAA 映射实际连接到不同域名服务，不误合并共享 IP 域名；未知地址/代理拒绝不改投目标；内部 DNS 优先；SOCKS5 双栈偏好、单栈保留、出口探测未验证时不启动 |
| UDP 与例外 | 对外非 DNS UDP 阻断；本机/LAN IPv4/IPv6、零长/最大报文、子环境本机监听正常 |
| 故障 | 代理不可用或必要链路失败不启动应用；运行中代理失败不建立公网直连；辅助进程丢失终止会话 |
| 生命周期 | 真实 PTY、标准流、输入、窗口变化、Ctrl-C、自定义退出码、SIGTERM/SIGKILL、后代和描述符清理 |
| 开发工具与通用入口 | 真实 OpenSSH agent、Git clone/对象/工作树；任意程序启动、选项归属、保留名转义、非 UTF-8 可执行路径 |
| 宿主不变 | 核对身份/时区等宿主读取结果未被改写；临时 profile 与运行文件隔离 |

所有强制检查必须成功，不通过则脚本返回非零，发布脚本不生成本次资产。硬件上本来没有的 DMI / D-Bus 条件性接口记录为 `UNSUPPORTED`，并检查子环境没有伪造新增别名；不能把权限不足当成“已隐藏”。

## checker 的作用与已知盲区

只读参考项目 `claude-code-env-checker` 的实际采集逻辑，不恢复 telemetry-lab，也不把外部 checker 加入 envwrap 的生产依赖。

可选交叉扫描使用 `scan --compact --include-sensitive --no-commands`，在测试专用 HOME/配置及隔离网络内分别运行原生与包装版本。原始 JSON 仅在进程内比较，不保存、不打印。核对 hostname、machine-id/boot ID/DMI、时区冲突、locale、接口/MAC/resolver，以及应继承的内核/CPU/内存。

重要区别：

- checker 的 `scan` 是离线观测，**本身不能证明流量真正经过代理**；网络部分必须使用真实 socket 和受控端点。
- checker 的风险分数包含国别、物理机/虚拟机、继承项等策略，不等于 envwrap 的产品合同；本项目不追求分数为零。
- checker 的 DMI 读取主要位于 `/sys/class/dmi/id`。独立探针额外检查 `/sys/devices/virtual/dmi/id`，避免单一路径盲区。
- 只检查 child 与 host “不同”不够；声明字段还必须匹配 profile，有意继承字段必须保持一致。
- checker 的 section probe 可以失败但命令仍返回 0；交叉适配器检查实际字段状态和 schema，而不是仅看进程退出码。
- 未提供 checker 时，参考项记为 `NOT_EXECUTED`；内置独立探针与全部网络场景仍必须运行。显式提供但启动/解析/字段失败时，不悄悄忽略。

## 执行

```sh
cargo build --release --locked
python3 -B scripts/acceptance.py --binary target/release/envwrap \
  --report target/acceptance.json
```

使用已有参考工具：

```sh
python3 -B scripts/acceptance.py --binary target/release/envwrap \
  --reference-checker "$HOME/Projects/claude-code-env-checker/target/release/claude-code-env-checker" \
  --report target/acceptance-with-checker.json
```

报告路径必须不存在，文件创建为 `0600`。stdout 为 JSON，stderr 为场景日志；退出 0 代表所有必需场景通过，1 表示场景失败/未完成，2 表示调用或报告生成错误。日志仅涉及测试生成的配置，独立原始观测先变成指纹再离开进程，发布报告只保留判定，不包含机器原始标识或 profile seed。

报告记录实际二进制 SHA-256、版本、标准、HTTP CONNECT/SOCKS5/SOCKS5h 三种场景、条件性接口、参考 checker 版本/哈希和应用会话未执行状态。离线测试明确设置 SOCKS5 的地址族模式，另验证自动探测错误时不执行程序；真实出口服务成功观察属于部署 smoke，不伪造为离线证明。不要拿其它二进制的报告代替当前包。发布包包含 `ACCEPTANCE.json`，并在 `BUILD-INFO.json` 关联同一验收。

## 本机正常使用前

发布测试使用隔离端点，证明程序政策；不替用户判断其真实代理服务是否可用。部署后还需要：

```sh
envwrap --version
envwrap doctor
envwrap claude --version
# 或其它实际已安装程序：
envwrap codex --version
```

`doctor` 验证选定 profile 的真实启动和 DNS 策略链路，但不是整个最终验收套件。配置必须完整，代理实际可用，时区/locale 真实安装。城市/语言是声明配置，不改变代理出口地理位置。

本机 TCP 回调的 50 ms 发现间隔、受信直连服务、文件路径 Unix socket 和真实系统基底继承仍是明确边界，不因更换验收标准而消失。

## 结果解释

- `PASS`：该必需行为实际执行并满足断言。
- `INTENTIONALLY_INHERITED`：实际核对与宿主基线一致，符合合同，不是泄漏失败。
- `UNSUPPORTED`：基底没有该条件性接口，明确记录，不能冒充检查成功。
- `NOT_EXECUTED` / `INCOMPLETE`：没有完成；若属于必需项则整体失败。
- `OUT_OF_SCOPE`：闭源载荷、服务端决策等不由环境验收证明。

用户要求发布前先本机测试时，即使上述自动验收通过，也等待用户确认后再发布；不把 `cargo login` 当作立即上传许可。
