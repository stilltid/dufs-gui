# dufs 参数完整清单（v0.46.0）

> **这份清单是给人工审核用的。** 每个字段都不是猜的：从 dufs 源码、`--help`、实起实例
> 三处对出来。
>
> - 对应版本：**dufs 0.46.0**（与本项目 `assets/dufs.exe` 一致；GitHub 上也是 Latest）
> - 复核方法见 §10，任何一条都能在几十秒内重跑一遍
> - 「本程序界面」一列指的是 dufs-gui 目前把它放在哪一页

---

## 0. 一句话地图

| 类别 | 数量 | 说明 |
|---|---|---|
| 位置参数 | **1** | `[serve-path]` |
| 命名选项（`Arg::new`） | **25** | 其中 `--auth-method` 被 `.hide(true)` 藏起来，所以 `--help` 只显示 24 个 |
| clap 自动生成的 | **2** | `-h/--help`、`-V/--version` |
| 环境变量 | **25** | 每个选项一个（`--completions` 没有）；`--help` 里看不到是因为用了 `hide_env` |
| 配置文件独有键 | **1** | `error-page`（没有对应的命令行参数） |

合计可输入的参数：**1 + 25 = 26 个 `Arg::new`**（`dufs --help` 里那 26 行是 24 个可见选项 + `-h` + `-V`）。

---

## 1. 为什么必须看源码才知道「全部」

三处对照的结果：

| 来源 | 能看到什么 | 漏掉了什么 |
|---|---|---|
| `dufs --help` | 24 个可见选项 + `-h`/`-V` | `--auth-method`（`.hide(true)`） |
| dufs README | 24 个环境变量 + 配置示例 | `DUFS_AUTH_METHOD`；配置示例里没有 `error-page` |
| **`src/args.rs`** | **全部 26 个 `Arg::new` 的每个属性**，以及 `Args` 结构体（配置文件的键） | — |

所以：**只读文档会漏 2 样东西**（隐藏参数、配置独有键）。

---

## 2. 命令行参数总表

「可重复」= 能不能在命令行里出现多次（clap 的 `ArgAction::Append`）。
「界面」= 本程序（dufs-gui）放在哪。

| 参数 | 短名 | 环境变量 | 默认值 | 可选值 | 可重复 | `--help` 可见 | 本程序界面 |
|---|---|---|---|---|---|---|---|
| `[serve-path]` | — | `DUFS_SERVE_PATH` | `.` | 任意文件或目录路径 | — | ✅（位置参数） | 共享 → 共享的文件夹 / 文件 |
| `-c, --config <file>` | `-c` | `DUFS_CONFIG` | 无 | 路径 | 否 | ✅ | 进阶 → 外部文件 → 配置文件 |
| `-b, --bind <addrs>` | `-b` | `DUFS_BIND` | `0.0.0.0` + `::` | IP 地址（**Windows 上不接受 unix socket**，见 §3.1） | **是**，且支持逗号分隔 | ✅ | 共享 → 监听网卡（默认「所有网卡」；点「指定具体网卡…」展开成**多选**） |
| `-p, --port <port>` | `-p` | `DUFS_PORT` | `5000` | 1–65535 | 否 | ✅ | 共享 → 端口 |
| `--path-prefix <path>` | — | `DUFS_PATH_PREFIX` | 空 | 路径片段 | 否 | ✅ | 进阶 → URL 前缀（和 HTTPS 挨着） |
| `--hidden <value>` | — | `DUFS_HIDDEN` | 空（不隐藏） | 逗号分隔的通配符 | **是**，且支持逗号分隔 | ✅ | 访问与权限 → 目录列表里隐藏 |
| `-a, --auth <rules>` | `-a` | `DUFS_AUTH` | 空（无认证） | 见 §7 | **是** | ✅ | 访问与权限 → 用户名密码 / 高级认证规则 |
| `--auth-method <value>` | — | `DUFS_AUTH_METHOD` | `digest` | `basic`、`digest` | 否 | ❌ **被隐藏** | **未做**（死参数，见 §3.3） |
| `-A, --allow-all` | `-A` | `DUFS_ALLOW_ALL` | 关 | — | 否 | ✅ | 共享 → 允许的操作（第一项） |
| `--allow-upload` | — | `DUFS_ALLOW_UPLOAD` | **关** | — | 否 | ✅ | 共享 → 允许的操作 |
| `--allow-delete` | — | `DUFS_ALLOW_DELETE` | **关** | — | 否 | ✅ | 共享 → 允许的操作 |
| `--allow-search` | — | `DUFS_ALLOW_SEARCH` | **关** | — | 否 | ✅ | 共享 → 允许的操作 |
| `--allow-symlink` | — | `DUFS_ALLOW_SYMLINK` | **关** | — | 否 | ✅ | 共享 → 允许的操作 |
| `--allow-archive` | — | `DUFS_ALLOW_ARCHIVE` | **关** | — | 否 | ✅ | 共享 → 允许的操作 |
| `--allow-hash` | — | `DUFS_ALLOW_HASH` | **关** | — | 否 | ✅ | 共享 → 允许的操作 |
| `--enable-cors` | — | `DUFS_ENABLE_CORS` | 关 | — | 否 | ✅ | 访问与权限 → 允许跨域访问（CORS） |
| `--render-index` | — | `DUFS_RENDER_INDEX` | 关 | — | 否 | ✅ | 进阶 → 静态网站模式（下拉框第 2 项） |
| `--render-try-index` | — | `DUFS_RENDER_TRY_INDEX` | 关 | — | 否 | ✅ | 进阶 → 静态网站模式（第 3 项） |
| `--render-spa` | — | `DUFS_RENDER_SPA` | 关 | — | 否 | ✅ | 进阶 → 静态网站模式（第 4 项） |
| `--assets <path>` | — | `DUFS_ASSETS` | 无 | 目录路径 | 否 | ✅ | 进阶 → 外部文件 → 前端资源 |
| `--log-format <format>` | — | `DUFS_LOG_FORMAT` | dufs 内置格式 | 格式串；**空串 = 不记日志** | 否 | ✅ | 进阶 → 访问日志 → 日志格式 |
| `--log-file <file>` | — | `DUFS_LOG_FILE` | 无（只输出到控制台） | 文件路径 | 否 | ✅ | 进阶 → 访问日志 → 日志文件 |
| `--compress <level>` | — | `DUFS_COMPRESS` | `low` | `none`、`low`、`medium`、`high` | 否 | ✅ | 共享 → 允许的操作（紧接着「允许打包下载」） |
| `--completions <shell>` | — | **无** | 无 | `bash`、`elvish`、`fish`、`powershell`、`zsh` | 否 | ✅ | **有意不做**（打印完就退出） |
| `--tls-cert <path>` | — | `DUFS_TLS_CERT` | 无 | 证书文件路径 | 否 | ✅ | 进阶 → HTTPS → 证书 |
| `--tls-key <path>` | — | `DUFS_TLS_KEY` | 无 | 私钥文件路径 | 否 | ✅ | 进阶 → HTTPS → 私钥 |
| `-h, --help` | `-h` | — | — | — | — | ✅ | **有意不做**（clap 自动生成） |
| `-V, --version` | `-V` | — | — | — | — | ✅ | **有意不做**（clap 自动生成） |

> `--tls-cert` / `--tls-key` 在源码里是 `#[cfg(feature = "tls")]` 包起来的；
> 官方发布的二进制**带 TLS**（本项目实测 HTTPS 可用）。

---

## 3. 逐个说明（按功能分组）

### 3.1 服务入口

#### `[serve-path]`
- **语法**：位置参数，放在命令行最后。`dufs /some/dir`、`dufs /some/file.iso`
- **默认**：`.`（当前目录）
- **★ 可以是一个文件**，不只是目录。此时 dufs 把该文件挂在根路径 `/` 上：
  `GET /` 直接返回文件内容，响应头带 `content-disposition`。
- **★ 单文件模式下写操作是「假成功」**（实测）：`PUT` / `DELETE` 返回 **200 但什么都不做**
  —— 文件既不会被改也不会被删。所以本程序在单文件模式下会把权限类开关置灰且不传（§9）。
- 路径必须先 `sanitize`（源码 `Args::sanitize_path`）：不存在会直接报错退出。

#### `-b, --bind <addrs>`
- **默认**：`0.0.0.0` **和** `::`（IPv6 可用时两个都监听）
- **可重复**：`-b 127.0.0.1 -b 10.0.0.1`，或逗号分隔 `-b 127.0.0.1,10.0.0.1`
- **unix socket 路径只存在于 Unix 平台**：源码里 `BindAddr::SocketPath` 是
  `#[cfg(unix)]`，非 Unix 走 `bail!("Invalid bind address ...")`。实测 Windows 上
  `dufs -b /tmp/x.socket` 直接报错退出：

  ```
  Error: Invalid bind address `C:/Users/.../x.socket`
  ```

  所以 Windows 上 `-b` **只接受 IP 地址**（源码里那个 unix socket 分支在这条编译路径上根本不存在）。
- 和 `serve-path` 不同，`-b` 的地址**不做 canonicalize、也不检查存在性**。
- **不接受 `host:port`**：实测 `-b 0.0.0.0:8080` → `Error: Invalid bind address`。
  端口是独立的 `-p`。（本程序用独立端口框，天然避开这个坑。）
- **逗号分隔时 clap 不会 trim**：实测 `-b '127.0.0.1, 10.10.0.20'`（逗号后带空格）→
  ``Error: Invalid bind address ` 10.10.0.20` ``（那个前导空格被原样当成地址的一部分）。
  本程序不拼逗号，不受影响。
- ★★ **只要有一个地址绑不上，整个服务就起不来**。实测 `-b 10.99.99.99 -b 127.0.0.1`
  （前者不是本机地址）：

  ```
  Error: Failed to bind `10.99.99.99:51510`
  ```

  之后**进程直接退出**，连 `127.0.0.1` 也不通。这条规则直接决定了「默认监听」怎么对齐：
  **不能无条件补 `-b ::`**，否则在没有 IPv6 的机器上会把服务搞挂。
- **界面差异**：本程序传 `-b 0.0.0.0` + `-b ::`（仅当自己探测到本机 IPv6 可用），
  与 dufs 默认一致；探测不可用时自动退回只绑 `0.0.0.0`。勾了具体地址时则逐个传
  `-b`（一个地址一个 `-b`），**不拼逗号**（正好避开上面那个「逗号后带空格」的坑）。
- **URL 里的 IPv6 要加方括号**：`http://[fd7a::1]:5000/`。不加的话浏览器分不出
  主机和端口。这是写生成地址的那一方（本程序）必须处理的。

#### `-p, --port <port>`
- 默认 `5000`。范围 1–65535。
- ★ 端口可能被 **Windows 保留**（装过 Hyper-V / WSL / Docker 很常见），此时绑定会报
  `WSAEACCES(10013)`，看起来像「被占用」但任务管理器里找不到占用者。源码用 `net::port_reserved()` 区分这两种情况。

#### `--path-prefix <path>`
- 加了之后所有路由挂在 `/<前缀>/` 下。源码会把首尾的 `/` 去掉（`trim_matches('/')`），并按 URL 编码生成 `uri_prefix`。
- ★ 会影响**对外地址**：本程序的二维码/复制地址必须带上它，否则扫码得到 `400 Invalid Path`。

#### `-c, --config <file>`
- 见 §5。
- **命令行参数优先于配置文件里的同名项**（实测）。

### 3.2 目录列表

#### `--hidden <value>`
- 默认不隐藏任何东西 —— `.git`、`*.log` 都会出现在列表里。
- 逗号分隔，也可重复传。但**三个坑**会让规则**静默失效**（不报错、规则也不生效）：
  - **逗号后带空格**：clap 的 `value_delimiter(',')` 不会 trim，`'tmp, *.log'` 拆出来是
    `tmp` 和 `" *.log"`（带前导空格）→ 第二条**永远匹配不上**；
  - **全角逗号 `，`**：dufs 只按 ASCII `,` 拆 → 整串被当成**一个**模式 → 同样永不匹配
    （中文输入法下这个太容易打出来）；
  - **★ 带路径的模式**（`sub/xxx.log`）：模式**只和条目名比，不含路径**，所以这种写法
    **永远匹配不上**。（实测：根列表和子列表都毫无变化。）
- **通配符是 glob 语法**，不止 `*`：`?`、`[0-9]` 这类字符类也可用
  （源码是 `glob::Pattern::new(pattern).matches(条目名)`，用 `glob` crate 的默认选项）。
- ★ **大小写敏感**：`*.LOG` 藏不住 `a.log` —— Windows 文件系统本身是大小写不敏感的，
  所以这条反直觉。
- ★ 只影响**列表显示**，实测三点：
  - 知道文件名仍然可以**直接访问**（被隐藏的文件 `GET` 依然 200）；
  - 被隐藏的**目录**仍然能进去浏览，里面的内容照常列出（`/subx/` 返回 200、列表完整）；
  - `*.log` 这类模式会一并藏掉**子目录里**的同名文件（匹配的是名字，`*` 不看层级）。
- **本程序的补充**：传参前会归一化 —— 按半角/全角逗号拆开、逐段去空白、丢掉空段，
  再用半角逗号拼回去（`dufs::norm_hidden`）。所以前两个坑在界面上不会发生；
  第三个坑（带路径）在界面的提示文案里写明了。

### 3.3 访问控制与权限

#### `-a, --auth <rules>`
- 见 §6、§7。**可重复**（多账号就是多个 `-a`）。

#### `--auth-method <basic|digest>`  ← 隐藏参数
- 源码里写了 `.hide(true)`，`--help` 看不到；官方 README 的环境变量清单里也没有它。
- 默认值 `digest`。
- **★ 它是个死参数**：整个 `args.rs` 里 `digest` 只出现在它自己的定义里，
  **没有任何代码路径读取它**。实测 `--auth-method basic` 与默认的 `digest`
  行为、响应头**完全一样**（两个 `WWW-Authenticate` 都发）。
- 结论：**不做也没影响**。（本程序未做，但「额外命令行参数」里写它也不会被拦。）

#### `-A, --allow-all` 与 6 个 `--allow-*`
- `-A` 做的事情就一件：把 **6 个** 布尔值一起置位（源码 `args.rs::parse`）：

  ```rust
  let allow_all = args.allow_all;
  args.allow_upload  = allow_all || get_flag("allow-upload");
  args.allow_delete  = allow_all || get_flag("allow-delete");
  args.allow_search  = allow_all || get_flag("allow-search");
  args.allow_symlink = allow_all || get_flag("allow-symlink");
  args.allow_hash    = allow_all || get_flag("allow-hash");
  args.allow_archive = allow_all || get_flag("allow-archive");
  ```

- **`-A` 不包含**：`--enable-cors`、三种 `--render-*`、`--hidden`、`--auth`、
  `--compress`、`--tls-*`、`--log-*`、`--assets`、`--path-prefix`、`-b`、`-p`。
  （源码里这些各自读自己的 `get_flag(...)`，从未与 `allow_all` 相或。）
  实测：`dufs -A` 时 **CORS 响应头 0 个**。
- **`--allow-search` 是硬开关**：不开时 `?q=` **完全无效**（只当普通目录列表）。
  实测 `?q=aaa&json`：
  - 不开 → `['sub', 'aaa.txt', 'bbb.log']`（全量，没过滤）
  - 开了 → `['aaa.txt', 'sub/aaa-nested.txt']`（真的递归搜索，过滤掉了 `bbb.log`）
- **`--allow-hash`**：控制 `?hash` 查询。不开时 `?hash` 返回 **403**。
- **`--allow-archive`**：控制 `?zip` 打包下载。不开时返回 **404**。
  - ★ 实测 0.46 **只有 `?zip`**：`?tar` 不被识别（返回 `content-type: text/html`，
    实际是普通目录列表页）。别按「zip/tar 都能打」去写代码。
- **`--allow-symlink`**：是否允许跟随**指向共享根之外**的符号链接。
- **`--compress`**：只在 `--allow-archive`（或 `-A`）开着时才有意义。

### 3.4 呈现

#### `--render-index` / `--render-try-index` / `--render-spa`
三者互斥语义（同时传多个时实际行为以源码为准，本程序只允许选一个）：

| 选项 | 目录有 index.html | 目录没有 index.html |
|---|---|---|
| （都不传） | 列目录 | 列目录 |
| `--render-index` | 渲染 | **404** |
| `--render-try-index` | 渲染 | 列目录 |
| `--render-spa` | 渲染 | 单层未知路径回退到 index.html |

- ★ **`--render-spa` 只回退一层**：`/dashboard` → `index.html`；
  `/a/b` → **404**（实测）。

#### `--assets <path>`
- 覆盖 dufs 内置的网页版界面。
- ★ 目录里**必须含 `index.html`**，否则 dufs 启动即失败。
- 里面放一个 **`404.html`** 就能自定义找不到页面时的样子 —— 源码里这是自动推导的
  （见 §5 的 `error-page`）。
- `index.html` 里可用两个占位符：`__INDEX_DATA__`（目录列表数据）、`__ASSETS_PREFIX__`。

#### `--enable-cors`
- 设置 `Access-Control-Allow-Origin: *`。与权限无关，只加响应头。

### 3.5 日志

#### `--log-file <file>`
- 默认**不写文件**，只输出到 stdout/stderr。
- ★ 本程序把 dufs 的 stdout/stderr 接到管道里（只保留最后若干行用于报错），
  所以**不填日志文件时访问日志在界面上是看不到的**。

#### `--log-format <format>`
- 不填 = dufs 内置格式：`2026-09-21T14:00:22+08:00 INFO - 127.0.0.1 "GET /a.txt" 200`
- **★ 空字符串 = 关闭 http 访问日志**（本程序「关闭访问日志」复选框就是传一个空参数）。
  实测：传 `--log-format ""` 后日志文件 0 字节。
- 自定义示例：`--log-format '$remote_addr "$request" $status'` → `127.0.0.1 "GET /a.txt" 200`

### 3.6 传输与压缩

#### `--compress <none|low|medium|high>`
- 默认 `low`。只在能打包下载时生效。

#### `--tls-cert` / `--tls-key`
- 两个都要填，只填一个 dufs 会报错。
- 填了之后是 HTTPS；本程序的二维码/复制地址会自动切 `https://`。

### 3.7 CLI 专属（有意不做进界面）

| 参数 | 行为 |
|---|---|
| `--completions <shell>` | 打印 shell 补全脚本然后**退出**，不启动服务 |
| `-h, --help` | 打印帮助后退出 |
| `-V, --version` | 打印版本后退出 |

把它们放进真实启动命令里只会让服务起不来（表现为「等待就绪超时」），
所以本程序的启动前检查会直接拦下这三个。

---

## 4. 环境变量（完整 25 个）

**规则**：所有选项都能用 `DUFS_` 前缀的环境变量设置（官方 README 明说），
`--help` 里看不到是因为用了 `.hide_env(true)`。

| 环境变量 | 对应参数 |
|---|---|
| `DUFS_SERVE_PATH` | `[serve-path]` |
| `DUFS_CONFIG` | `-c, --config` |
| `DUFS_BIND` | `-b, --bind` |
| `DUFS_PORT` | `-p, --port` |
| `DUFS_PATH_PREFIX` | `--path-prefix` |
| `DUFS_HIDDEN` | `--hidden` |
| `DUFS_AUTH` | `-a, --auth` |
| `DUFS_AUTH_METHOD` | `--auth-method`（隐藏死参数；**README 里没写**） |
| `DUFS_ALLOW_ALL` | `-A, --allow-all` |
| `DUFS_ALLOW_UPLOAD` | `--allow-upload` |
| `DUFS_ALLOW_DELETE` | `--allow-delete` |
| `DUFS_ALLOW_SEARCH` | `--allow-search` |
| `DUFS_ALLOW_SYMLINK` | `--allow-symlink` |
| `DUFS_ALLOW_ARCHIVE` | `--allow-archive` |
| `DUFS_ALLOW_HASH` | `--allow-hash` |
| `DUFS_ENABLE_CORS` | `--enable-cors` |
| `DUFS_RENDER_INDEX` | `--render-index` |
| `DUFS_RENDER_TRY_INDEX` | `--render-try-index` |
| `DUFS_RENDER_SPA` | `--render-spa` |
| `DUFS_ASSETS` | `--assets` |
| `DUFS_LOG_FORMAT` | `--log-format` |
| `DUFS_LOG_FILE` | `--log-file` |
| `DUFS_COMPRESS` | `--compress` |
| `DUFS_TLS_CERT` | `--tls-cert` |
| `DUFS_TLS_KEY` | `--tls-key` |

**没有环境变量的**：`--completions`（唯一的例外）、`-h`、`-V`。

### ★ 优先级与危险之处

**命令行 > 环境变量**（clap 的既定顺序）。危险的不是我们显式传的选项，
而是那些**因为界面没勾所以压根不传**的：环境变量就会胜出。

实测（本程序踩过这个坑）：

| 本机环境 | 界面状态 | 实际行为 |
|---|---|---|
| （干净） | 未勾「允许删除」 | `DELETE` → 403，文件保住 ✅ |
| `DUFS_ALLOW_DELETE=true` | 未勾「允许删除」 | `DELETE` → **204，文件真被删了** ❌ |
| `DUFS_AUTH=hacker:pw@/:rw` | 未填认证 | 匿名 `GET` → **401，认证被凭空打开** ⚠️ |

所以本程序在启动 dufs 前会**把子进程环境里所有 `DUFS_*` 变量摘掉**
（`dufs::strip_dufs_env()`，按前缀过滤、大小写不敏感），让界面成为唯一真相来源。

---

## 5. 配置文件（`-c`）

配置文件反序列化到的就是同一个 `Args` 结构体：

```rust
#[serde(default)]
#[serde(rename_all = "kebab-case")]
pub struct Args { ... }
```

所以**配置文件能写的键 = 全部选项的 kebab-case 名**，而且额外多一个**没有命令行参数的键**：

| 键 | 有对应的命令行参数吗 | 说明 |
|---|---|---|
| `error-page` | **没有** | 自定义「找不到页面」用。★ 启动时会**自动**从 `--assets` 目录里的 `404.html` 推导（源码：`let p = assets_path.join("404.html"); if p.exists() { args.error_page = Some(p) }`），所以一般不需要手写它 |

其余键（与参数一一对应）：`serve-path`、`bind`、`port`、`path-prefix`、`hidden`、
`auth`、`allow-all`、`allow-upload`、`allow-delete`、`allow-search`、`allow-symlink`、
`allow-archive`、`allow-hash`、`enable-cors`、`render-index`、`render-try-index`、
`render-spa`、`assets`、`log-format`、`log-file`、`compress`、`tls-cert`、`tls-key`。

**格式要求**：
- ★ **只认 `.json` / `.yaml` / `.yml`**。传 `.toml` 会报
  `invalid type: string ..., expected struct Args` 然后退出（实测 dufs 0.46）。
- `hidden` 支持字符串或数组；`auth` 支持字符串或数组。

**示例（YAML）**：

```yaml
serve-path: '.'
bind: 0.0.0.0
port: 5000
path-prefix: /dufs
hidden:
  - tmp
  - '*.log'
auth:
  - admin:admin@/:rw
  - user:pass@/src:rw,/share
  - '@/'          # 匿名规则；按 YAML 规范需要加引号
allow-upload: true
allow-delete: true
allow-search: true
allow-symlink: true
allow-archive: true
allow-hash: true
enable-cors: true
render-try-index: true
assets: ./assets/
log-format: '$remote_addr "$request" $status'
log-file: ./dufs.log
compress: low
tls-cert: cert.pem
tls-key: key.pem
```

> **命令行参数优先于配置文件里的同名项**（实测）。

---

## 6. 权限模型：是「两层」的（最容易误解的地方）

dufs 的权限**不是**一个开关决定的，而是两层：

| 层 | 由谁决定 | 作用 |
|---|---|---|
| **能力层** | 全局的 `--allow-*` / `-A` | 这个服务**在能力上**允许做什么（上限） |
| **身份层** | `-a` 认证规则里的 `:rw` / `:ro`、以及匿名规则 | **这个人**实际能到哪一步（只能是上限的子集） |

实测证据（同一个服务，`-a 'guest:gp@/pub:rw'`）：

| 命令 | guest 写 `/pub`（规则给了 `:rw`） |
|---|---|
| 只有 `-a 'guest:gp@/pub:rw'` | **403** ❌ |
| 加上 `--allow-upload` | **201** ✅ |
| 加上 `--allow-upload` 但写规则没给的 `/` | **403** ✅（规则确实在起约束） |

也就是说：**认证规则里的 `:rw` 单独没有用**，全局 `--allow-upload`
（或 `--allow-delete` 等）必须开着，否则规则再宽也写不进去。

第二个实测：**`:ro` 规则能压过 `-A`**（`dufs -A -a 'admin:pw@/:ro'`）：

| 谁 | 结果 |
|---|---|
| 匿名 | **401** |
| `admin`（规则 `:ro`） | `PUT` → 403、`DELETE` → 403 |

而且带凭据请求 `?json` 时，里面报的是**该用户的实际权限**
（`allow_upload: false`、`user: "admin"`），不是全局那份 ——
所以网页版 UI 的按钮是跟着这个用户能做什么变的。

> 补充：源码里 `AccessControl::default()`（**完全没配认证时**）给匿名的是
> `AccessPerm::ReadWrite`，但那只是身份层；实际能不能写仍然由能力层的
> `--allow-*` 决定 —— 这解释了「为什么 dufs 什么都不配时是纯只读」。

---

## 7. 认证规则语法（`-a`）

### 格式

```
账号@路径[:权限][,更多路径...]
```

- 账号：`用户名:密码`；或**空**（`@/...`）表示**匿名**规则
- 密码可以是明文，也可以是 `$6$` 开头的哈希原文（dufs 自己识别；别的 `$...$` 会被当成明文，见 §7 末尾的陷阱）
- 路径必须以 `/` 开头（源码用 `s.find("@/")` 切分账号与路径）
- 权限只有两种：**`:rw`**（读写）、**`:ro`**（只读，**可省略**）
  - ★ **没有 `:r`**。写成 `:r` 会让 dufs 直接报 `Error: Invalid auth value` 退出。
  - ★ **空密码不合法**：`-a 'user:@/'` dufs 直接拒绝启动（实测进程都没起来）。
  - ★ **密码里可以带冒号**：dufs 按**第一个**冒号切 user:pass（HTTP Basic 本来就这么定），
    `-a 'user:p:a@/'` 用密码 `p:a` 能登录，只用 `p` 进不去。用户名里**不能**有冒号（会认错人）。
- 一个账号的多个路径用**逗号**分隔：
  `admin:pw@/dir1:rw,/dir2`（`/dir2` 省略权限 = 只读）

### 界面现状（dufs-gui）

界面上只有「账号」列表（可视化，一行一个账号/路径/权限，
同一个用户名写多行 = 它的多个路径，用户名留空 = 匿名）。
以前还有个「专家模式」原文框（非空时覆盖列表），**已经删掉**：
老配置里的规则在加载时自动转成列表行（分号/管道符拆、省略权限=只读），
转完原文清空不再写回。转出来的坏行（没密码的死规则、同名不同密码、`:r` 非法权限）
会被校验拦下，而且行是可见的，直接在界面上改。
`$6$` 哈希可以直接贴进密码框；别的 `$...$` 格式会被当成明文（dufs 只认 `$6$`），
启动前校验会拦下来。

### 多账号：两种写法（都实测通过）

```bash
# 写法一：多个 -a （dufs 自己这么用；-a 是 Append 型，可重复）
dufs -a 'admin:pw@/:rw' -a 'guest:gp@/pub:rw,/:ro'

# 写法二：一个 -a 里用竖线分隔 —— dufs 自己就会拆（源码 fn split_rules 按 '|' 切）
dufs -a 'admin:pw@/:rw|guest:gp@/pub:rw,/:ro'
```

### ★ `;` 不是 dufs 的分隔符

实测 `dufs -a 'admin:pw@/:rw;guest:gp@/pub'` 直接报：

```
Error: Invalid auth value `admin:pw@/:rw;guest:gp@/pub
```

**本程序界面里提示的「多个账号用分号」是 dufs-gui 自己帮你拆的**
（`build_args()` 按 `;` 或 `|` 拆成多个 `-a`），不是 dufs 的语法。
看 dufs 官方文档时要用 `|`。

### 匿名规则

- `-a '@/'` = 允许匿名访问 `/`（权限可省略 = 只读）；要读写写 `-a '@/pub:rw'`
- **匿名规则只能有一条**（源码：`no duplicate anonymous rules`）

### ★ 密码可以是 `$6$` 哈希（推荐了解的用法）

源码里判断 `if pass.starts_with("$6$")`，用 `sha_crypt::ShaCrypt::SHA512` 校验。
也就是**密码字段可以直接填 SHA-512 crypt 哈希**：

```bash
H=$(openssl passwd -6 -salt abcdefgh 'yourpassword')
dufs -a "admin:$H@/:rw"
# 实测：用明文 yourpassword 请求 → 200；错误密码 → 401
```

**代价（重要）**：用哈希密码时，401 只发 **`Basic`** 挑战（**不发 `Digest`**）——
因为 digest 认证需要明文参与计算，哈希没法参与。而 Basic 是把
`用户名:密码` 做 base64 送上网络，**在明文 HTTP 下等于裸奔**。

| 方案 | 磁盘/命令行里有明文吗 | 网络上是明文吗 |
|---|---|---|
| 明文密码 | **有** | Digest：不是；Basic：是 |
| `$6$` 哈希 | 没有 | **只能用 Basic → 是**（除非上 HTTPS） |

---

## 8. 实测行为怪癖（写代码别想当然）

| 现象 | 说明 |
|---|---|
| 单文件 serve-path 的写操作「假成功」 | `PUT`/`DELETE` 返回 **200 但什么都不做**，文件不变也不被删 |
| `?q=` 不开 `--allow-search` 时完全无效 | 只返回普通目录列表，不报错也不过滤 |
| `?hash` 不开 `--allow-hash` | **403** |
| `?zip` 不开 `--allow-archive` | **404** |
| `--render-spa` 只回退一层 | `/dashboard` → index.html；`/a/b` → **404** |
| `-c` 只认 `.json` / `.yaml` | `.toml` 报 `invalid type: string ...` 退出 |
| `--assets` 目录必须含 `index.html` | 否则启动即失败 |
| `--log-format ''` | 表示**关闭** http 访问日志（不是「用默认格式」） |
| `-A` 不包含 CORS | 实测 `-A` 时 CORS 响应头 0 个 |
| `:rw` 规则需要全局开关配合 | 见 §6 |
| `:ro` 压过 `-A` | 见 §6 |
| `--auth-method` 无任何效果 | 死参数，见 §3.3 |
| `?` 的返回形态会嗅探 User-Agent | 带 curl/wget 的 UA 得到纯文本目录列表；带浏览器 UA 得到网页 UI —— **HTML 里不含文件名**（文件名由 JS 再拉 `?json`）。实测：浏览器 UA 请求 `?q=aaa` 时 HTML 里一个文件名都搜不到，所以**不能靠抓 HTML 来验证搜索** |
| HEAD 请求会跳过目录遍历 | 0.46 的改动（release notes: Skip directory walking in HEAD requests） |

---

## 9. 与本程序（dufs-gui）的对照

### 9.1 覆盖情况

| 分类 | 数量 | 本程序 |
|---|---|---|
| 位置参数 | 1 | ✅ 共享页（文件夹或单个文件） |
| 可见命名选项 | 24 | ✅ 全部图形化 |
| 隐藏选项 `--auth-method` | 1 | ❌ 未做（死参数，无影响） |
| clap 自动生成 `-h`/`-V` | 2 | ❌ 有意不做 |
| `--completions` | 1 | ❌ 有意不做 |
| **另有逃生舱** | — | 「进阶 → 额外命令行参数」可透传任意参数（重复项会被启动前检查拦下） |

### 9.2 已知差异（不是 bug，是取舍）

| 项 | dufs 原生 | 本程序 |
|---|---|---|
| `-b` | 支持多个 IP（逗号分隔或重复传） | **也支持多选**（展开后是两列平铺的勾选列表，一个地址一个 `-b`）；不支持逗号分隔写法 |
| 默认监听 | `0.0.0.0` + `::` | `0.0.0.0` + `::`（**已对齐**；IPv6 探测不可用时自动退回只绑 `0.0.0.0`） |
| 默认权限 | **纯只读**（`--allow-*` 全关） | **上传 + 搜索默认开**（为了「手机传文件」主场景） |
| 环境变量 | 可用 `DUFS_*` 配置一切 | 启动前把 `DUFS_*` 全部摘掉，只认界面 |
| 配置文件里的 `tls-*` / `path-prefix` | 生效 | 界面感知不到配置文件内容，显示的地址可能仍是 `http://` |

### 9.3 本程序补充的行为

- 单文件模式下**不传**权限类选项、界面置灰（因为写操作是假成功）
- `--allow-all` 与逐项权限互斥显示（开了 `-A`，逐项就置灰）
- `--allow-all` 与逐项权限互斥显示；本程序另有「只读模式」快捷开关
  （勾上 = 把 6 个逐项全部不传；dufs 没有对应参数，和「一键放开」正好是一对）
- `-A` 与「按目录只读」同时开时，界面会说明「不冲突，规则更严」

---

## 10. 怎么复核这份清单

```bash
# ① 参数与选项全貌（注意：--auth-method 不会出现在这里）
dufs --help

# ② 隐藏参数、环境变量名、默认值、可重复性 —— 只能看源码
#    https://github.com/sigoden/dufs/blob/main/src/args.rs
#    关注：Arg::new(...) 的 .env()/.default_value()/.action()/.hide()，
#         以及 Args 结构体的字段（= 配置文件的键）

# ③ 环境变量（官方 README 的 Environment variables 一节）
#    https://github.com/sigoden/dufs/blob/main/README.md

# ④ 本项目的自动化对照（真起 dufs、用 HTTP 验行为）
dufs-gui.exe --selftest-options
#    结果写 %TEMP%\dufs-gui-selftest-options.txt（295 项断言）
```

**版本变了怎么办**：`src/args.rs` 是本清单的唯一权威来源，重新对一遍
`Arg::new` 的个数和 `Args` 的字段即可；本项目的 `--selftest-options`
会把「行为变了」这件事直接报出来。
