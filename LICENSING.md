# 许可证：**GPL-3.0-only**

本项目以 **GNU 通用公共许可证第 3 版（GPL-3.0-only）** 发布。全文见 [`LICENSE`](LICENSE)。

```
Copyright (C) 2026 stilltid

本程序是自由软件：你可以按自由软件基金会发布的 GNU 通用公共许可证
（第 3 版，或你选择的任何更高版本 —— 但本项目声明为「仅第 3 版」）
的条款重新分发和/或修改它。

本程序分发的目的是希望它有用，但不提供任何担保，甚至没有适销性或
特定用途适用性的默示担保。详见 GNU 通用公共许可证。

你应该已随本程序收到一份 GNU 通用公共许可证副本；如果没有，见
<https://www.gnu.org/licenses/>。
```

> 上面这段是 GPL 建议的标准声明（「Copyright (C) 2026 stilltid」里的作者名与年份
> 可按需修改）。

## 为什么是 GPL-3.0（而不是 MIT/Apache）

GUI 框架 **Slint 1.18** 是三选一许可：

- `GPL-3.0-only`
- `LicenseRef-Slint-Royalty-free-2.0`
- `LicenseRef-Slint-Software-3.0`（商业）

本项目选择走其中的 **GPL-3.0** 分支，因此整体（含本项目自己的代码）以
**GPL-3.0-only** 发布。这条路**免费、且不需要在界面或下载页加任何 Slint 署名**
（署名是 Royalty-free 分支的要求）。

## 对你的义务（如果你是使用者）

- 你可以自由使用、修改、再分发本程序（含商业用途）。
- **再分发时必须**：附上 `LICENSE` 全文；提供**完整对应源码**（含你做的修改）；
  保留版权与许可声明；派生作品同样以 GPL-3.0 授权。
- 不提供任何担保。

## 第三方组件的许可不受影响

本项目的 GPL 只覆盖本项目自己的代码与其组合分发方式，**不改变**第三方各自的许可：

- **Slint** —— 在本项目里按 GPL-3.0 分支使用；
- **dufs**（内嵌二进制）—— MIT 或 Apache-2.0，© sigoden，正文见
  [`assets/LICENSES/`](assets/LICENSES/)；
- **vcruntime140.dll** —— © Microsoft，按 VC++ Redistributable 条款随应用分发，
  **不受本 GPL 覆盖**（属于 GPL-3.0 意义上的「系统库」类组件）。

详见 [`THIRD-PARTY-NOTICES.md`](THIRD-PARTY-NOTICES.md)。依赖树体检结论：
除 Slint 系外全部为宽松许可，与 GPL-3.0 兼容。

## 贡献

除非另有说明，你提交的贡献（PR）将按与本项目相同的 **GPL-3.0-only** 授权。
