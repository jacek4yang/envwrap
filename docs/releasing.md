# 维护与发布

[返回 README](../README.md) · [Changelog](../CHANGELOG.md)

## 版本与发布承诺

- 包版本在 `Cargo.toml`，锁文件提交入库；标签使用 `v` 加包版本。
- 首版为 `0.3.0-rc.1`，GitHub 标记 **Pre-release**，不标记 Latest 稳定版。
- 候选版编号递增；已经发布的标签/资产不覆盖、不强推。修复使用新版本和新校验和。
- 真实 Claude 生产专项验收未完成前，不因测试全绿而去掉候选版标记。
- GitHub 发布不等于 crates.io 发布，也不自动改变私有仓库可见性或用户已安装的程序。

## 开发验证

工具：Rust/Cargo（本版验证 1.98.1）、rustfmt、Clippy、Python 3.11+、Git，以及运行层的 bwrap/ip/nft。端到端测试还需要 setpriv、OpenSSH ssh-agent/ssh-add、支持 rootless namespaces 的内核。

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets -- --nocapture
python3 scripts/package_release.py --check-docs
```

`tests/cli.rs` 验证显式配置、私有权限、失败不启动、身份持久化与脱敏。`tests/runtime.rs` 在外层私有网络 namespace 中启动真实 HTTP CONNECT/SOCKS5、本机 TCP/UDP、DNS、SSH agent、Git daemon，再从最终 envwrap 入口运行应用。这不是用 mock 模拟路由成功，也不调用真实 Claude 付费 API。

测试依赖不足应报告失败，而不是跳过关键网络场景后声称全部通过。不要为跑测试修改宿主防火墙/sysctl 或把 envwrap 以 root 运行。

## 发布前需要更新的内容

1. 在 `Cargo.toml` 更新版本，运行 Cargo 更新 `Cargo.lock` 的本地包版本，提交锁文件。
2. 更新 `CHANGELOG.md`、README 的版本/资产示例、对应 `docs/releases/` 发布说明。
3. 检查没有把私有 profile、代理 URL、账号认证文件、运行日志或临时探针提交进去。
4. 完成全量本地验证；稳定版还必须完成真实 Claude 对话/恢复和实际扩展的人工验收。
5. 提交所有应纳入版本控制的源代码、测试、文档和发布脚本，保持工作区干净。

当前流程由维护者执行本地验证与打包；不宣称已有 GitHub Actions 的全发行版兼容认证。

## 一条命令生成发布资产

在项目目录执行：

```sh
python3 scripts/package_release.py
```

脚本要求干净 checkout 和 `x86_64-unknown-linux-gnu` 本机 Rust 工具链；还需要 `readelf`（binutils）、GNU libc 的 `getconf`。它会：

1. 检查本地 Markdown 链接/标题与代码围栏。
2. 执行格式、Clippy、全部 Cargo 测试。
3. `cargo build --release --locked --target x86_64-unknown-linux-gnu`。
4. 用**实际优化二进制**再跑 HTTP CONNECT 与 SOCKS5 两套完整网络/身份/生命周期/开发工具场景。
5. 收集源码提交、编译器、libc、ELF 库/符号要求、二进制哈希与已执行检查。
6. 在 `target/dist/` 生成二进制包、该提交的完整源码包和 `SHA256SUMS`。

它不会安装、上传、创建标签、提交代码或读取真实用户 profile。源码包来自 `git archive` 的精确提交，二进制包只含程序、公开文档、许可证和构建信息；不包含 HOME、目标目录缓存、测试运行秘密或用户配置。

```text
target/dist/
  envwrap-v0.3.0-rc.1-x86_64-unknown-linux-gnu.tar.gz
  envwrap-v0.3.0-rc.1-source.tar.gz
  SHA256SUMS
```

二进制包内有 `envwrap`、`README.md`、`LICENSE`、`CHANGELOG.md`、`docs/`、`BUILD-INFO.json`。tar/gzip 元数据做了规范化，Cargo 依赖锁定；这支持追溯和按同一来源重建，**不承诺不同路径/系统/编译器产物逐字节一致**。校验和用于完整性与一致性，不是独立签名或第三方安全审计。

首版 GNU 包构建于 glibc 2.43 环境，ELF 最高 GLIBC 符号要求为 2.39；需要 `libgcc_s.so.1`。其它架构、musl 和更旧 libc 没有经过本次二进制验证，不重命名资产来假装支持。

## 推送与创建 Release

先推送已验证提交，再创建指向同一提交的标签。以首版为例：

```sh
git push origin main
git tag -a v0.3.0-rc.1 -m 'envwrap 0.3.0-rc.1'
git push origin v0.3.0-rc.1

gh release create v0.3.0-rc.1 \
  target/dist/envwrap-v0.3.0-rc.1-x86_64-unknown-linux-gnu.tar.gz \
  target/dist/envwrap-v0.3.0-rc.1-source.tar.gz \
  target/dist/SHA256SUMS \
  --repo jacek4yang/envwrap --verify-tag --prerelease --latest=false \
  --title 'envwrap v0.3.0-rc.1 — 首个预发布版本' \
  --notes-file docs/releases/v0.3.0-rc.1.md
```

以上是维护者操作，不是普通用户安装步骤。检查远端是否已有同名标签/Release，避免误覆盖。发布工具认证失败时处理 GitHub 认证，不把 token 写进命令历史、配置示例或发布说明。

## 发布后验证

从 GitHub **重新下载**资产到独立目录，不用本地原件代替下载验证：

```sh
gh release download v0.3.0-rc.1 --repo jacek4yang/envwrap \
  --dir target/release-download
cd target/release-download
sha256sum --check SHA256SUMS
tar -xzf envwrap-v0.3.0-rc.1-x86_64-unknown-linux-gnu.tar.gz
./envwrap-v0.3.0-rc.1-x86_64-unknown-linux-gnu/envwrap --version
./envwrap-v0.3.0-rc.1-x86_64-unknown-linux-gnu/envwrap --help
```

还应核对 `BUILD-INFO.json` 的 commit 与发布标签一致，运行至少一个实际运行场景；本项目首次发布使用完整 SOCKS5 fixture 验证下载后的程序。核对 Release 是已发布而非 draft、候选版标记正确、资产齐全，仓库权限没有被意外改变。

## 后续生产验收与回滚

[人工验收清单](troubleshooting.md#生产使用前的人工验收记录)全部通过后，才把新程序切到日常 PATH。安装是操作者的明确动作；发布脚本不覆盖旧二进制、不更改旧 profile。

回滚时切回保存的旧二进制与对应配置；旧程序不理解 schema 2 时，不能给它同一份新 profile 并期待自动降级。任何版本都不要用删除原生 Claude 配置或改为空 HOME 来掩盖兼容问题。
