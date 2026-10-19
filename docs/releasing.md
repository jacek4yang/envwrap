# 维护与发布

[返回 README](../README.md) · [独立验收标准](acceptance.md) · [Changelog](../CHANGELOG.md)

## 当前发布授权

**0.3.0 已获操作者本机测试确认和正式发布授权，包括 crates.io、GitHub Release 及仓库公开。** 后续版本仍须满足验收和相应发布授权；`cargo login` 本身不代表上传许可。

历史 `v0.3.0-rc.1` 的标签、Release 和资产不可覆盖。新版本使用新标签和新资产。0.3.0 发布时按操作者授权将仓库改为 public；**crates.io 包及其源码同样公开**。

最终发布门槛是独立环境/网络观测与通用行为验收，不再强制要求 Claude 登录或模型对话，也不以 checker 风险评分为零作为标准。未执行的应用体验仍如实标注。

## 开发验证

工具：Rust/Cargo（当前已验证 1.98.1）、rustfmt、Clippy、Python 3.11+、Git，以及运行依赖 bwrap/ip/nft。端到端测试额外需要 setpriv、OpenSSH ssh-agent/ssh-add 和支持 rootless namespaces 的内核。

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets -- --nocapture
cargo build --release --locked
python3 -B scripts/acceptance.py --binary target/release/envwrap
python3 -B scripts/package_release.py --check-docs
```

`tests/cli.rs` 验证配置、私有权限、错误不启动和脱敏；`tests/runtime.rs` 使用真实网络栈和本地受控代理。`tests/fixtures/environment.py` 是独立观测器，不导入 envwrap 实现。Git/SSH、PTY、回调、进程故障与通用入口都做实际操作。

可选传入现有 `claude-code-env-checker` 的路径做只读交叉扫描。它不成为日常运行依赖，不修改参考项目，也不启动真实付费 API 请求。必需场景无法执行要失败，不能跳过后宣称通过。

## 准备提交与 crates.io 包

1. 更新包版本和 Cargo.lock；更新 CHANGELOG、README 和发布说明，保持未发布状态准确。
2. 检查暂存范围，不包含个人 profile、代理凭据、账号文件、Cargo token、日志或临时脚本。
3. 运行上面的完整验证；满足用户测试确认这一外部条件。
4. 提交所有最终源码、测试、文档和维护脚本，保持工作区干净。
5. 准备并检查源码包：

```sh
cargo package --list --locked
cargo publish --dry-run --locked
```

`--dry-run` 验证打包/构建但不上传。不要读取或输出 `~/.cargo/credentials.toml`，由 Cargo 自己使用已登录凭据。包名可用性和所有权以实际 registry 检查为准；一次 search 无结果不等于保留了名称。

Cargo 的 include 清单仅允许项目源码、测试、维护脚本、文档、许可证与清单/锁文件。发布前仍需检查实际清单；二进制/临时配置位于被忽略的 `target/`，不进入 crate。

公开仓库前检查完整 Git 历史（不能只扫描当前目录）、待提交源码、既有 Release 文案与资产，以及 Issue/Actions 附件等。秘密扫描应脱敏输出；私有 profile、代理凭据、原始检测报告、Cargo token 和用户备份不能进入仓库或发布包。Git 作者署名和提交邮箱属于随历史公开的元数据，不会被秘密扫描自动删除。

0.3.0 的公开前检查使用 Gitleaks 8.30.1，并额外比对本机真实身份与私有 profile 身份值；未发现真实秘密或私有配置进入检查范围。工具扫描不是不存在所有敏感信息的数学证明。检查输入/报告留在私有、被忽略的 `target/`，不作为发布资产。


## 构建可追溯 GitHub 资产

必须从本项目自身的干净 Git checkout 打包，不能从另一个仓库内的解压源码借用其 HEAD：

```sh
python3 -B scripts/package_release.py \
  --reference-checker "$HOME/Projects/claude-code-env-checker/target/release/claude-code-env-checker"
```

外部 checker 可省略，但内置独立观测和全部网络场景不可省略。脚本要求 `x86_64-unknown-linux-gnu` 本机工具链、readelf（binutils）和 getconf；它会：

1. 检查文档链接与围栏，运行 fmt、Clippy、全量 Cargo 测试。
2. 锁定依赖构建优化二进制。
3. 对实际优化二进制执行统一独立验收，失败则不生成本次包。
4. 记录提交、工具链、libc、ELF 依赖、程序哈希、验收标准及参考 checker 信息。
5. 生成二进制包、精确提交的源码包与 SHA256SUMS。

```text
target/dist/
  envwrap-v0.3.0-x86_64-unknown-linux-gnu.tar.gz
  envwrap-v0.3.0-source.tar.gz
  SHA256SUMS
```

二进制包含程序、README/LICENSE/CHANGELOG、docs、`BUILD-INFO.json`、`ACCEPTANCE.json`。两个 JSON 关联同一二进制与实际验收，不包含机器原始标识、账号数据或 profile seed。

tar/gzip 元数据规范化，Cargo 依赖锁定；不承诺不同路径/系统/工具链的产物逐字节一致。校验和不是独立签名或安全审计。GNU 二进制兼容条件以该次构建的 ELF/BUILD-INFO 为准，不重命名为未经验证的 ARM/musl 包。

脚本只打包，不提交、不推送、不创建标签、不上传，也不安装到用户 PATH。

## 用户确认后才执行真实发布

确认用户测试通过，检查版本未被发布、分支与源码包一致，然后按此顺序执行：

```sh
# 真实公开上传：没有用户测试确认时不要执行。
cargo publish --locked

# 同一提交的 GitHub 标签与资产。
git push origin main
git tag -a v0.3.0 -m 'envwrap 0.3.0'
git push origin v0.3.0

gh release create v0.3.0 \
  target/dist/envwrap-v0.3.0-x86_64-unknown-linux-gnu.tar.gz \
  target/dist/envwrap-v0.3.0-source.tar.gz \
  target/dist/SHA256SUMS \
  --repo jacek4yang/envwrap --verify-tag --latest \
  --title 'envwrap v0.3.0 — 通用透明运行环境' \
  --notes-file docs/releases/v0.3.0.md
```

版本已经发布时不重新覆盖、不强推。任何修复使用新版本。发布说明中的“待确认”状态应在得到确认后、生成最终提交与资产之前更新，不能临时改文档后上传旧提交的二进制。

## 发布后验证

- 通过 registry 查询确认包版本、仓库信息和 checksum，使用独立 Cargo 安装根目录安装已发布版本并验证，不用本地 `--path` 构建冒充 registry 下载。
- 从 GitHub 重新下载三份资产，执行 `sha256sum --check SHA256SUMS`。
- 核对 BUILD-INFO 的提交与发布标签、程序哈希与 ACCEPTANCE 一致。
- 用下载后的程序再执行独立验收，而不只看 `--version`。
- 确认 Release 不是 draft、版本标记正确、仓库权限未意外改变。

## 本机测试安装与回滚

用户明确请求本机测试时，可在公开发布前执行 `cargo install --path . --locked`。安装前备份旧二进制，配置前私下备份旧 profile；不上传备份或 secrets。用 `command -v envwrap` 和 `envwrap --version` 核对真正入口，再运行 doctor 和已安装应用的非会话启动检查。

回滚使用旧二进制及其对应配置，不把 schema 2 文件交给不支持它的旧版本。不要删除原生应用配置或改为空 HOME 来掩盖问题。
