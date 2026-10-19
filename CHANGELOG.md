# Changelog

## 0.3.0 — 2026-10-19

- 通用 `envwrap PROGRAM [ARGS...]` 入口：Claude、Codex、agy 无专用分支；移除 `run --` 子命令，保留管理名可用 `-- PROGRAM` 转义。
- 推荐真正的 `socks5h://` 域名代理解析：会话内 A/AAAA 路由映射、SOCKS5 域名目标、内部 DNS 优先；明确合成 IP、DNSSEC/NXDOMAIN、HTTPS/SVCB 和 DoH/字面量边界。
- `socks5://` 默认经代理访问证书验证的 HTTPS 出口服务，按观察到的地址族选择 DNS 偏好；新增 `--dns-preference` / `network.dns_preference` 显式模式。保留单栈域名，探测失败不启动、不猜测、不直连。
- 新增实际文件操作回归，覆盖程序参数归属、保留名和非 UTF-8 可执行路径。
- 最终门槛改为 `envwrap-independent-v2`，覆盖 HTTP CONNECT、SOCKS5、SOCKS5h 和 DNS 偏好；不再以 Claude 登录/对话或风险评分为门槛。
- 独立系统调用/procfs/sysfs/文件观测与 host-child 比较，验证别名一致、冬夏时区、namespace/capabilities、真实硬件/配置继承及宿主不变。
- 可选只读 `claude-code-env-checker` 离线交叉验证；原始敏感 JSON 不落盘、不输出；必需字段不可用不算通过。
- 发布脚本复用独立验收，二进制包附 `ACCEPTANCE.json`；报告关联实际二进制哈希。
- 操作者本机测试确认后的首个 GitHub 非预发布版本，仓库经当前文件、完整历史及历史资产敏感数据检查后公开。crates.io 独立上传，以注册表实际收录状态为准。


## 0.3.0-rc.1 — 2026-10-19

首个 GitHub Release；标记为预发布候选版，不是已完成 Claude 生产专项验收的稳定版。

### 新运行模型

- `envwrap claude` 与 `envwrap run -- ...` 共用运行引擎，保留原 HOME、应用配置、登录来源、工具链与应用环境。
- 普通用户 namespace + 真实 Linux REDIRECT/TPROXY 网络；外部 TCP 经 HTTP CONNECT / SOCKS5，默认 DNS 经代理 TCP，外部非 DNS UDP 阻断。
- 显式 localhost/LAN/内部 DNS 例外；文件路径 Unix socket 保留；宿主 loopback TCP 回调转发。
- schema 2 私有 profile：启动前明确配置代理来源、DNS、时区、locale；稳定生成身份，不在运行时猜测或重新随机化。
- hostname、machine-id、时区、locale、网卡和声明 DMI 接口的一致视图，保留真实系统能力。
- doctor 运行真实身份与 DNS 策略诊断；诊断成功不等于 Claude 登录会话通过。

### 破坏性删除

- 旧 export/import/check/wrap 拼装入口与环境白名单。
- 公开文件 sandbox、默认独立 HOME、强制遥测禁用。
- telemetry-lab 产品代码、旧解析器与不再适用的测试。
- 旧 profile 的隐式兼容；不静默迁移，不保留兼容别名。

### 修复与防护

- 终端 Ctrl-C 不再因提前终止 bwrap reaper 而覆盖应用自定义退出码。
- 清除冲突的 TZDIR，避免环境时区与 `/etc/localtime` 计算不一致。
- 参数位置错误时不把误传的代理 URL 输出到错误日志；profile 展示隐藏 URL 和 seed。
- 应用清空 capabilities、启用 no-new-privileges，不继承代理来源变量/辅助网络权限。
- 网络故障不回退到宿主外网直连；退出、辅助进程丢失和强制终止场景纳入进程树清理验证。
- DNS 诊断文案描述实际配置策略，不把显式内部 DNS 直连误称为代理链路。

### 文档与交付

- 中文 README、完整配置参考、架构说明、故障排查、维护与发布流程。
- MIT 许可证正文；锁定依赖的源码构建。
- 干净提交打包脚本：文档链接、格式、Clippy、全量测试、优化二进制 HTTP/SOCKS5 实测后生成源码包、GNU/Linux x86_64 二进制包、构建信息与 SHA-256 校验和。

### 验证与已知限制

- 8 项自动回归：6 项 CLI 与 2 项 rootless 端到端场景；覆盖 IPv4/IPv6、DNS、UDP、身份、PTY/信号/退出、真实 Git/SSH agent。
- 真实 Claude Code 2.1.302 的版本、帮助、认证状态及配置路径已有原生/包装对照；当时为未登录状态。
- 尚未验证真实 Claude 登录后的对话、恢复及 MCP/hooks/插件/skills/OAuth；不自动替换用户已有安装。
- 代理端点只支持 IP 字面量；不支持 HTTPS 代理、SOCKS5 UDP ASSOCIATE 或对外 QUIC。
- localhost TCP 回调存在 50 ms 轮询发现窗口；不保证与共享宿主网络完全等价。
- 不承诺控制任意遥测、隐藏所有宿主信息、防恶意同用户程序或提供文件安全沙箱。

旧仓库 `main` 的包版本为 0.1.0，但此前没有 GitHub Release。此次是新运行模型的首次正式打包交付，不补造旧版本发布记录。
