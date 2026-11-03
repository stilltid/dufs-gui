# 第三方组件与许可（THIRD-PARTY NOTICES）

本项目打包/嵌入了若干第三方软件。分发时（尤其是分发 `dufs-gui.exe`）请连同本文件
一起提供。**本文件不改变本项目自身的许可证**（见 `LICENSING.md`）。

## 1. Slint（GUI 框架）

- 版本：`slint 1.18.0`（含 `i-slint-*`、`slint-build`、`slint-macros`）
- 许可：**三选一** — `GPL-3.0-only` / `LicenseRef-Slint-Royalty-free-2.0` / `LicenseRef-Slint-Software-3.0`
- 版权：© SixtyFPS GmbH
- 本项目选定的分支：**GPL-3.0-only**（见 `LICENSING.md`）
- 来源：<https://github.com/slint-ui/slint>

> 走 GPL 分支 → 分发本程序时按 GPL-3.0 提供完整对应源码即可，
> **不需要** `AboutSlint` 组件或 “Made with Slint” 徽章（那是 Royalty-free 分支的要求）。

## 2. dufs（被封装的文件服务器，二进制被内嵌）

- 版本：`0.47.0`（对应 `src/dufs.rs::DUFS_VERSION`）
- 许可：**MIT 或 Apache-2.0 双许可**（使用者可任选其一）
- 版权：© sigoden
- 来源：<https://github.com/sigoden/dufs>
- 许可正文随仓库提供：
  - `assets/LICENSES/dufs-MIT.txt`
  - `assets/LICENSES/dufs-APACHE-2.0.txt`

`assets/dufs.exe` 是 dufs 官方发布的预编译二进制，被 `include_bytes!` 嵌进主程序，
运行时释放到 `%LOCALAPPDATA%\dufs-gui\bin\`。本项目以**子进程**方式调用它
（没有链接其代码），属于独立程序的聚合。

## 3. Microsoft Visual C++ 运行库（vcruntime140.dll）

- 文件：`assets/vcruntime140.dll`（x64，约 121 KB）
- 版权：© Microsoft Corporation
- 许可：Microsoft Visual C++ 2015–2022 Redistributable 的再发行条款
  （<https://learn.microsoft.com/cpp/windows/latest-supported-vc-redist>）
- 用途：内嵌的 `dufs.exe` 是动态链接 CRT 的；本程序把该 DLL 释放到它旁边
  （app-local 部署），否则用户机器上没装 VC++ 运行库时 dufs 起不来。
- ⚠ **该 DLL 不受本项目许可证覆盖**，其权利归 Microsoft，按微软再发行条款随应用分发。

## 4. Rust 依赖（crates.io）

除上述外，其余依赖均为宽松许可（MIT / Apache-2.0 / Zlib 等），与 GPL-3.0 兼容。
直接依赖一览：

| crate | 版本 | 许可 |
|---|---|---|
| `slint` / `slint-build` | 1.18.0 | GPL-3.0-only OR Slint-Royalty-free-2.0 OR Slint-Software-3.0 |
| `rfd` | 0.17.2 | MIT |
| `windows` / `windows-core` | 0.62.x | MIT OR Apache-2.0 |
| `qrcode` | 0.14.1 | MIT OR Apache-2.0 |
| `local-ip-address` | 0.6.13 | MIT OR Apache-2.0 |
| `sys-locale` | 0.3.2 | MIT OR Apache-2.0 |
| `tray-icon` | 0.25.1 | MIT OR Apache-2.0 |
| `serde` / `serde_json` | 1.x | MIT OR Apache-2.0 |
| `embed-manifest` | 1.5.1 | MIT |
| `raw-window-handle` | 0.6 | MIT OR Apache-2.0 OR Zlib |

> 完整传递依赖清单与各自许可证，请用
> `cargo install cargo-deny && cargo deny check licenses`
> 或 `cargo install cargo-about && cargo about generate about.hbs`
> 重新生成（本文件只列直接依赖）。

## 5. 依赖许可证体检（对 `Cargo.lock` 自动统计）

统计方式（不需要装额外工具）：

```bash
cargo metadata --format-version 1   # 每个 package 都带 license 字段
```

- 依赖包总数 **609**。除 Slint 系（13 个包）外，**全部**是宽松许可：
  MIT / Apache-2.0 / BSD / Zlib / ISC / Unicode-3.0 / BSL-1.0 等。
- **没有** GPL-only、AGPL、SSPL、Commons-Clause、BUSL 等强 copyleft 依赖。
- 两处需要知道、但都不构成障碍的：
  - `r-efi` —— `MIT OR Apache-2.0 OR LGPL-2.1-or-later`（可选宽松分支）；
  - `option-ext` —— `MPL-2.0`（文件级弱 copyleft，本项目未修改它，且 MPL 与 GPL 兼容）。
- 唯一**没有** `license` 字段的包是 `dufs-gui` 自己 —— 那正是本项目**尚未选定**
  的许可证（见 `LICENSING.md`）。

⇒ 结论：**无论最终选 GPL-3.0 还是 MIT/Apache，依赖侧都没有阻碍。**
