//! 选项自检（`--selftest-options`）
//!
//! 目的：回答本项目最要紧的那个问题 ——
//! **「界面上的开关拨了，到底有没有真的变成 dufs 的命令行参数、并且真的生效？」**
//!
//! 分三段：
//!   1. 参数拼装（纯函数，逐项黄金用例，`build_args` 的输出必须一字不差）
//!   2. 启动前校验（`Config::validate` 该拦的必须拦住）
//!   3. 访问地址拼装（协议 + URL 前缀 —— 曾经漏过，导致二维码扫出来是 400）
//!   4. 真实行为（起真的 dufs，用 HTTP 去验证每个选项确实起作用了）
//!
//! 第 3 段是重点：只看参数拼对了不够，还得确认 dufs 那边真的按预期行为。
//! 例如 `--hidden` 参数拼对了但目录列表里还能看到文件，那就是没生效。
//!
//! 端口统一用 52100 段：实测本机 `netsh int ipv4 show excludedportrange protocol=tcp`
//! 把 51938-52037 / 52184-52283 等段留给了 Hyper-V，落在保留段里的端口会拿到
//! `WSAEACCES(10013)`，所以这里避开。

use crate::config::{Compress, Config, Perm, RenderMode};
use crate::dufs;
use crate::net;
use crate::selftest::Report;

use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, TcpStream};
use std::path::{Path, PathBuf};
use std::process::Child;
use std::time::Duration;

/// 黄金用例里用的假目录（不会真的去访问，只参与拼参数）
const FOLDER: &str = r"C:\share";

// ============================================================
// 入口
// ============================================================

pub fn run() -> i32 {
    let mut r = Report::new(
        std::env::temp_dir().join("dufs-gui-selftest-options.txt"),
        "dufs-gui 选项自检",
    );
    r.lines.push(format!("dufs-gui 选项自检  (dufs {})", dufs::DUFS_VERSION));

    golden_args(&mut r);
    validations(&mut r);
    drop_decide(&mut r);
    serve_path_resolution(&mut r);
    bind_config_compat(&mut r);
    account_list(&mut r);
    restore_defaults(&mut r);
    access_urls(&mut r);

    // 真实行为测试需要先准备共享目录
    let fx = match Fixture::prepare(&mut r) {
        Some(f) => f,
        None => return r.finish(),
    };
    behaviors(&mut r, &fx);

    r.finish()
}

// ============================================================
// 1. 参数拼装黄金用例
// ============================================================

/// 造一个账号行（测试用）：acc("admin", "pw", "/", "rw")
///
/// ★ 权限参数保持 `&str` 是为了让用例读起来就是**配置文件里的字面量**
///   （断言的期望值也是 `:rw` / `:ro` 这种线格式）；
///   用 `from_dufs` 转，写错字直接在测试里 panic —— 而不是静默换一个权限。
fn acc(name: &str, secret: &str, path: &str, perm: &str) -> crate::config::AccountRow {
    crate::config::AccountRow {
        name: name.to_string(),
        secret: secret.to_string(),
        path: path.to_string(),
        perm: Perm::from_dufs(perm).expect("测试用例里的权限字面量写错了"),
    }
}

/// 一个黄金用例：改配置 -> 期望的完整命令行
struct Case {
    name: &'static str,
    mutate: fn(&mut Config),
    /// 期望的参数，空格分隔；`{F}` 会被替换成共享目录
    expect: &'static str,
}

fn golden_args(r: &mut Report) {
    r.section("1. 参数拼装（黄金用例）");

    // 注意：期望值是「一字不差」的完整命令行，故意写得很啰嗦，
    // 因为这里要抓的就是"少传一个 / 多传一个 / 顺序错了"这类问题。
    let cases: Vec<Case> = vec![
        Case {
            name: "默认（上传+搜索，无删除）",
            mutate: |_| {},
            expect: r"-b 0.0.0.0 -p 5000 --allow-upload --allow-search {F}",
        },
        Case {
            name: "只读模式压制所有 allow-*",
            mutate: |c| c.readonly_mode = true,
            expect: r"-b 0.0.0.0 -p 5000 {F}",
        },
        Case {
            name: "--allow-all 接管所有 allow-*（压缩级别照传）",
            mutate: |c| {
                c.allow_all = true;
                c.allow_delete = false;
                c.allow_archive = false;
            },
            expect: r"-b 0.0.0.0 -p 5000 --allow-all --compress low {F}",
        },
        Case {
            name: "权限全开 + 压缩级别",
            mutate: |c| {
                c.allow_delete = true;
                c.allow_archive = true;
                c.allow_hash = true;
                c.allow_symlink = true;
                c.compress = Compress::High;
            },
            expect: r"-b 0.0.0.0 -p 5000 --allow-upload --allow-search --allow-delete --allow-archive --allow-hash --allow-symlink --compress high {F}",
        },
        Case {
            name: "没勾打包下载时压缩级别不传",
            mutate: |c| c.compress = Compress::High,
            expect: r"-b 0.0.0.0 -p 5000 --allow-upload --allow-search {F}",
        },
        Case {
            name: "只读模式下压缩级别也不传",
            mutate: |c| {
                c.readonly_mode = true;
                c.allow_archive = true;
                c.compress = Compress::Medium;
            },
            expect: r"-b 0.0.0.0 -p 5000 {F}",
        },
        Case {
            name: "CORS 不受只读模式影响",
            mutate: |c| {
                c.readonly_mode = true;
                c.enable_cors = true;
            },
            expect: r"-b 0.0.0.0 -p 5000 --enable-cors {F}",
        },
        Case {
            name: "绑定具体网卡 + 自定义端口",
            mutate: |c| {
                c.bind_ips = vec!["192.168.1.5".into()];
                c.port = 8080;
            },
            expect: r"-b 192.168.1.5 -p 8080 --allow-upload --allow-search {F}",
        },
        Case {
            name: "勾了多个地址 -> 一个地址一个 -b（dufs 的 -b 可以重复传）",
            mutate: |c| {
                c.bind_ips = vec!["10.1.2.3".into(), "192.168.1.5".into()];
            },
            expect: r"-b 10.1.2.3 -b 192.168.1.5 -p 5000 --allow-upload --allow-search {F}",
        },
        Case {
            name: "勾了 IPv6 地址 -> 原样给 -b（方括号只用在 URL 里，不用在这里）",
            mutate: |c| {
                c.bind_ips = vec!["fd7a:115c:a1e0::5a36:2377".into()];
            },
            expect: r"-b fd7a:115c:a1e0::5a36:2377 -p 5000 --allow-upload --allow-search {F}",
        },
        Case {
            name: "列表里混了空项 -> 跳过空项，其余照常（配置被人手改过）",
            mutate: |c| {
                c.bind_ips = vec![String::new(), "10.1.2.3".into()];
            },
            expect: r"-b 10.1.2.3 -p 5000 --allow-upload --allow-search {F}",
        },
        Case {
            name: "所有网卡 + 本机有 IPv6 -> 补一个 -b ::（对齐 dufs 裸跑默认）",
            mutate: |c| {
                c.bind_ips = Vec::new();
                c.ipv6_available = true;
            },
            expect: r"-b 0.0.0.0 -b :: -p 5000 --allow-upload --allow-search {F}",
        },
        Case {
            name: "所有网卡 + 本机没 IPv6 -> 只绑 0.0.0.0（多传 :: 会让服务起不来）",
            mutate: |c| {
                c.bind_ips = Vec::new();
                c.ipv6_available = false;
            },
            expect: r"-b 0.0.0.0 -p 5000 --allow-upload --allow-search {F}",
        },
        Case {
            name: "指定了某张网卡 -> 只绑那一个，不额外补 ::（用户选的就是唯一监听地址）",
            mutate: |c| {
                c.bind_ips = vec!["10.1.2.3".into()];
                c.ipv6_available = true;
            },
            expect: r"-b 10.1.2.3 -p 5000 --allow-upload --allow-search {F}",
        },
        Case {
            name: "URL 前缀 + 隐藏内容",
            mutate: |c| {
                c.path_prefix = "files".into();
                c.hidden = "tmp,*.log".into();
            },
            expect: r"-b 0.0.0.0 -p 5000 --path-prefix files --hidden tmp,*.log --allow-upload --allow-search {F}",
        },
        Case {
            name: "隐藏路径：逗号后带空格会被归一化（否则那条规则静默失效）",
            mutate: |c| {
                c.hidden = "tmp, *.log".into();
            },
            expect: r"-b 0.0.0.0 -p 5000 --hidden tmp,*.log --allow-upload --allow-search {F}",
        },
        Case {
            name: "隐藏路径：全角逗号也认（中文输入法很容易打出来）",
            mutate: |c| {
                c.hidden = "tmp，*.log".into();
            },
            expect: r"-b 0.0.0.0 -p 5000 --hidden tmp,*.log --allow-upload --allow-search {F}",
        },
        Case {
            name: "隐藏路径：首尾空白与多余空项都被吃掉",
            mutate: |c| {
                c.hidden = "  tmp ,, *.log  ".into();
            },
            expect: r"-b 0.0.0.0 -p 5000 --hidden tmp,*.log --allow-upload --allow-search {F}",
        },
        Case {
            name: "隐藏路径：全是空白/逗号 = 没填（不传 --hidden）",
            mutate: |c| {
                c.hidden = " , ,, ".into();
            },
            expect: r"-b 0.0.0.0 -p 5000 --allow-upload --allow-search {F}",
        },
        Case {
            name: "静态站点：render-index",
            mutate: |c| c.render_mode = RenderMode::Index,
            expect: r"-b 0.0.0.0 -p 5000 --allow-upload --allow-search --render-index {F}",
        },
        Case {
            name: "静态站点：render-try-index",
            mutate: |c| c.render_mode = RenderMode::TryIndex,
            expect: r"-b 0.0.0.0 -p 5000 --allow-upload --allow-search --render-try-index {F}",
        },
        Case {
            name: "静态站点：render-spa",
            mutate: |c| c.render_mode = RenderMode::Spa,
            expect: r"-b 0.0.0.0 -p 5000 --allow-upload --allow-search --render-spa {F}",
        },
        Case {
            name: "认证：单个账号",
            mutate: |c| {
                c.use_auth = true;
                c.accounts = vec![acc("admin", "pw", "/", "rw")];
            },
            expect: r"-b 0.0.0.0 -p 5000 --allow-upload --allow-search -a admin:pw@/:rw {F}",
        },
        Case {
            name: "认证：账号权限写 :ro（不能写 :r，dufs 会拒绝启动）",
            mutate: |c| {
                c.use_auth = true;
                c.accounts = vec![acc("admin", "pw", "/", "ro")];
                c.allow_upload = false;
                c.allow_search = false;
            },
            expect: r"-b 0.0.0.0 -p 5000 -a admin:pw@/:ro {F}",
        },
        Case {
            name: "--allow-all 时压缩级别也要传（以前会被静默丢掉）",
            mutate: |c| {
                c.allow_all = true;
                c.compress = Compress::High;
            },
            expect: r"-b 0.0.0.0 -p 5000 --allow-all --compress high {F}",
        },
        Case {
            name: "认证：账号列表拼出多个 -a（一个账号一个）",
            mutate: |c| {
                c.use_auth = true;
                c.accounts = vec![
                    acc("admin", "pw", "/", "rw"),
                    acc("guest", "gp", "/pub", "ro"),
                ];
            },
            expect: r"-b 0.0.0.0 -p 5000 --allow-upload --allow-search -a admin:pw@/:rw -a guest:gp@/pub:ro {F}",
        },
        Case {
            name: "认证：同一账号写多行会合并成一个 -a（路径用逗号连）",
            mutate: |c| {
                c.use_auth = true;
                c.accounts = vec![
                    acc("admin", "pw", "/", "rw"),
                    acc("admin", "pw", "/pub", "ro"),
                ];
            },
            expect: r"-b 0.0.0.0 -p 5000 --allow-upload --allow-search -a admin:pw@/:rw,/pub:ro {F}",
        },
        Case {
            name: "认证：密码里带冒号原样透传（dufs 按第一个冒号切，合法）",
            mutate: |c| {
                c.use_auth = true;
                c.accounts = vec![acc("user", "p:a", "/", "rw")];
            },
            expect: r"-b 0.0.0.0 -p 5000 --allow-upload --allow-search -a user:p:a@/:rw {F}",
        },
        Case {
            name: "认证：匿名账号拼成 @ 开头（用户名留空）",
            mutate: |c| {
                c.use_auth = true;
                c.accounts = vec![acc("", "", "/pub", "ro")];
            },
            expect: r"-b 0.0.0.0 -p 5000 --allow-upload --allow-search -a @/pub:ro {F}",
        },
        Case {
            name: "认证：$6$ 哈希原样透传（不拆不改）",
            mutate: |c| {
                c.use_auth = true;
                c.accounts = vec![acc(
                    "admin",
                    "$6$abcdefgh$KQeXafAQAaOoKTevphVU215RvJdgzyfASRasIOuh12hO8u0r1bGW92ZnTmC9IjsiQ8VPiTXBiZF49dFL1U4wX/",
                    "/",
                    "rw",
                )];
            },
            expect: r"-b 0.0.0.0 -p 5000 --allow-upload --allow-search -a admin:$6$abcdefgh$KQeXafAQAaOoKTevphVU215RvJdgzyfASRasIOuh12hO8u0r1bGW92ZnTmC9IjsiQ8VPiTXBiZF49dFL1U4wX/@/:rw {F}",
        },
        Case {
            name: "HTTPS：证书 + 私钥",
            mutate: |c| {
                c.tls_cert = r"C:\c\cert.pem".into();
                c.tls_key = r"C:\c\key.pem".into();
            },
            expect: r"-b 0.0.0.0 -p 5000 --allow-upload --allow-search --tls-cert C:\c\cert.pem --tls-key C:\c\key.pem {F}",
        },
        Case {
            name: "访问日志：文件 + 格式",
            mutate: |c| {
                c.log_file = r"C:\c\access.log".into();
                c.log_format = "MYFMT $status $request".into();
            },
            expect: r"-b 0.0.0.0 -p 5000 --allow-upload --allow-search --log-file C:\c\access.log --log-format MYFMT $status $request {F}",
        },
        Case {
            name: "dufs 配置文件（-c）排在最前",
            mutate: |c| c.dufs_config = r"C:\c\dufs.json".into(),
            expect: r"-c C:\c\dufs.json -b 0.0.0.0 -p 5000 --allow-upload --allow-search {F}",
        },
        Case {
            name: "自定义前端资源（--assets）",
            mutate: |c| c.assets = r"C:\c\assets".into(),
            expect: r"-b 0.0.0.0 -p 5000 --allow-upload --allow-search --assets C:\c\assets {F}",
        },
        Case {
            name: "额外参数在最前 + 目录在最后",
            mutate: |c| c.extra_args = "--allow-hash --compress none".into(),
            expect: r"--allow-hash --compress none -b 0.0.0.0 -p 5000 --allow-upload --allow-search {F}",
        },
    ];

    for case in cases {
        let mut cfg = Config {
            serve_path: FOLDER.into(),
            ..Config::default()
        };
        (case.mutate)(&mut cfg);

        let got = dufs::build_args(&cfg).join(" ");
        let want = case.expect.replace("{F}", FOLDER);
        if got == want {
            r.ok(case.name);
        } else {
            r.fail(case.name);
            r.info(format!("期望: {want}"));
            r.info(format!("实际: {got}"));
        }
    }

    // 关闭日志：`--log-format` 后面必须跟一个**空字符串**，
    // dufs 靠「格式为空」表示不记录 http 日志。参数里带空元素，
    // 用 join(" ") 对比会看不出来，所以单独断言。
    {
        let cfg = Config {
            serve_path: FOLDER.into(),
            log_disabled: true,
            ..Config::default()
        };
        let args = dufs::build_args(&cfg);
        let ok = args
            .iter()
            .position(|a| a == "--log-format")
            .is_some_and(|i| args.get(i + 1).is_some_and(|v| v.is_empty()));
        if ok {
            r.ok("关闭访问日志：--log-format 传空字符串");
        } else {
            r.fail("关闭访问日志：--log-format 后面应当是一个空字符串");
            r.info(format!("实际参数: {args:?}"));
        }
    }
    {
        // 没开这个开关、且格式框也空着时，不应该出现 --log-format
        let cfg = Config {
            serve_path: FOLDER.into(),
            ..Config::default()
        };
        let args = dufs::build_args(&cfg);
        if args.iter().any(|a| a == "--log-format") {
            r.fail("没关日志、格式也空时不应该传 --log-format");
            r.info(format!("实际参数: {args:?}"));
        } else {
            r.ok("没关日志、格式也空时不传 --log-format");
        }
    }
    // 单文件模式：权限 / 打包 / 隐藏 / 渲染这些开关对单个文件没有意义，
    // build_args 应当直接省略（CORS 例外，它管 GET 响应头，照样有用）。
    //
    // ★ 用一个真实存在的文件当 serve_path：build_args 靠
    //   `serve_path_kind()` 判断形态，假路径会被当成 Invalid 而不是 File。
    {
        let file = std::env::temp_dir().join("dufs-gui-opts-single-golden.txt");
        let _ = std::fs::write(&file, b"x");
        let path = file.to_string_lossy().into_owned();

        let dropped = [
            "--allow-upload",
            "--allow-search",
            "--allow-delete",
            "--allow-archive",
            "--compress",
            "--allow-hash",
            "--allow-symlink",
            "--allow-all",
            "--hidden",
            "--render-spa",
        ];
        let mut check_omitted = |tag: &str, cfg: &Config| {
            let args = dufs::build_args(cfg);
            let bad: Vec<&&str> = dropped.iter().filter(|d| args.iter().any(|a| a == **d)).collect();
            let cors_kept = args.iter().any(|a| a == "--enable-cors");
            if bad.is_empty() && cors_kept {
                r.ok(format!("单文件（{tag}）：无意义开关被省略，CORS 保留"));
            } else {
                r.fail(format!("单文件（{tag}）：不该出现的参数 {bad:?}，CORS 保留={cors_kept}"));
                r.info(format!("实际参数: {args:?}"));
            }
        };

        // 分支一：逐项全开（走 else 分支）
        check_omitted(
            "逐项全开",
            &Config {
                serve_path: path.clone(),
                allow_upload: true,
                allow_search: true,
                allow_delete: true,
                allow_archive: true,
                compress: Compress::High,
                allow_hash: true,
                allow_symlink: true,
                hidden: "*.log".into(),
                render_mode: RenderMode::Spa,
                enable_cors: true,
                ..Config::default()
            },
        );
        // 分支二：--allow-all（走 allow_all 分支）
        check_omitted(
            "--allow-all",
            &Config {
                serve_path: path,
                allow_all: true,
                compress: Compress::High,
                hidden: "*.log".into(),
                render_mode: RenderMode::Spa,
                enable_cors: true,
                ..Config::default()
            },
        );
    }
}

// ============================================================
// 2b. 拖放采用规则（config::decide_dropped_path，纯函数）
// ============================================================

/// 拖进来的一组路径 → 采用哪一个。不用真的去拖文件，直接拿合成路径测。
fn drop_decide(r: &mut Report) {
    r.section("2b. 拖放采用规则（一次只能一个）");

    let tmp = std::env::temp_dir();
    let real_file = tmp.join("dufs-gui-opts-drop-file.txt");
    let real_dir = tmp.join("dufs-gui-opts-drop-dir");
    let _ = std::fs::write(&real_file, b"x");
    let _ = std::fs::create_dir_all(&real_dir);
    let missing = tmp.join("dufs-gui-opts-drop-nope-9f3a");

    // (说明, 输入, 期望采用?) —— 期望采用时，断言采用的正是输入的那个。
    let cases: Vec<(&str, Vec<PathBuf>, bool)> = vec![
        ("空列表 -> 拒绝", vec![], false),
        (
            "两个路径 -> 全部拒绝（不猜第几个）",
            vec![real_file.clone(), real_dir.clone()],
            false,
        ),
        ("不存在的路径 -> 拒绝", vec![missing], false),
        ("单个文件 -> 采用", vec![real_file.clone()], true),
        ("单个文件夹 -> 采用", vec![real_dir.clone()], true),
    ];
    for (name, paths, should_accept) in cases {
        let want = if should_accept { Some(paths[0].clone()) } else { None };
        match (crate::config::decide_dropped_path(paths), want) {
            (Ok(got), Some(w)) if got == w => r.ok(name),
            (Err(_), None) => r.ok(name),
            (Ok(got), _) => r.fail(format!("{name}（不该采用，却采用了 {got:?}）")),
            (Err(e), _) => r.fail(format!("{name}（不该拒绝，却拒绝了：{e}）")),
        }
    }

    let _ = std::fs::remove_file(&real_file);
    let _ = std::fs::remove_dir(&real_dir);
}

// ============================================================
// 2. 启动前校验
// ============================================================

/// 2d. 共享路径「实际会被共享到哪里」
///
/// 依据（dufs 源码）：`Args::sanitize_path` =
/// `env::current_dir().push(path)` 之后 `canonicalize`。也就是
/// **相对路径按进程的工作目录解析**，并且**符号链接会被解析成目标**。
/// 本程序启动 dufs 时没有指定工作目录（子进程继承我们的），所以这里算出来的就是
/// dufs 一定会用的那一个 —— 界面据此把它显式写给用户看。
///
/// 注意 `serve_path_hint` 的设计意图是**只在解析结果和用户填的不是同一处时才提示**，
/// 所以「不提示」的用例和「提示」的用例都要断言，否则很容易写成一直提示（噪音）或
/// 一直不提示（等于没做）。
fn serve_path_resolution(r: &mut Report) {
    r.section("2d. 共享路径「实际共享到哪里」（相对路径 / 符号链接）");

    let tmp = std::env::temp_dir();
    let tmp_s = tmp.to_string_lossy().into_owned();

    // ---- 基本：解析出绝对路径，且不带 verbatim 前缀 ----
    let real_tmp = crate::config::real_serve_path(&tmp_s);
    check(r, "临时目录：能解析出真实路径", real_tmp.is_some());
    if let Some(real) = &real_tmp {
        check(
            r,
            "解析结果不带 Windows 的 verbatim 前缀（\\\\?\\）",
            !real.starts_with(r"\\?\"),
        );
        check(r, "解析结果是一个存在的目录", Path::new(real).is_dir());
    }

    // ---- 相对路径：必须提示，且解析到工作目录 ----
    let cwd = std::env::current_dir().map(|p| p.to_string_lossy().into_owned());
    let dot_hint = crate::config::serve_path_hint(".");
    check(r, r#"相对路径 "." 会给出「实际共享」提示"#, dot_hint.is_some());
    if let (Some(hint), Ok(cwd)) = (&dot_hint, &cwd) {
        let cwd_real = crate::config::real_serve_path(cwd).unwrap_or_default();
        check(
            r,
            r#"相对路径 "." 解析到当前工作目录，而不是字面量 "."#,
            hint.eq_ignore_ascii_case(&cwd_real),
        );
    }

    // ---- 已经是规范形式：不该提示（否则每一行都多一句废话）----
    if let Ok(cwd) = &cwd {
        check(
            r,
            "绝对路径且已规范 -> 不提示（同一处路径不该刷屏）",
            crate::config::serve_path_hint(cwd).is_none(),
        );
        check(
            r,
            "结尾多一个分隔符 -> 不提示（C:\\a\\b\\ 与 C:\\a\\b 是同一处）",
            crate::config::serve_path_hint(&format!("{}\\", cwd.trim_end_matches('\\')))
                .is_none(),
        );
    }

    // ---- 无效路径 / 空路径：不提示（界面另有红字）----
    check(
        r,
        "不存在的路径 -> 不提示",
        crate::config::serve_path_hint(r"C:\definitely-not-here-9f3a").is_none(),
    );
    check(r, "空路径 -> 不提示", crate::config::serve_path_hint("   ").is_none());

    // ---- 符号链接：dufs 的根是**目标**，所以必须提示 ----
    // 用 junction（`mklink /J`）：不需要管理员权限，也不需要开发者模式。
    let jtarget = tmp.join("dufs-gui-opts-jtarget");
    let jlink = tmp.join("dufs-gui-opts-jlink");
    let _ = std::fs::remove_dir(&jlink);
    let _ = std::fs::create_dir_all(&jtarget);
    let made = crate::firewall::console_command("cmd")
        .arg("/c")
        .arg("mklink")
        .arg("/J")
        .arg(&jlink)
        .arg(&jtarget)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    if made {
        let link_s = jlink.to_string_lossy().into_owned();
        let link_real = crate::config::real_serve_path(&link_s);
        let target_real = crate::config::real_serve_path(&jtarget.to_string_lossy());
        check(
            r,
            "符号链接：解析结果是**目标目录**，不是链接本身",
            link_real.is_some()
                && target_real.is_some()
                && link_real
                    .as_deref()
                    .zip(target_real.as_deref())
                    .map(|(a, b)| a.eq_ignore_ascii_case(b))
                    .unwrap_or(false),
        );
        check(
            r,
            "符号链接：会给出「实际共享」提示（根变了，用户必须知道）",
            crate::config::serve_path_hint(&link_s).is_some(),
        );
        let _ = std::fs::remove_dir(&jlink);
    } else {
        r.skip("符号链接用例（本机建不出 junction，跳过）");
    }
    let _ = std::fs::remove_dir(&jtarget);
}

/// 2e. 监听地址从「一个字符串」改成「数组」之后的**旧配置兼容**。
///
/// 老版本写的是 `"bind_ip": "10.0.0.5"`，新版本是 `"bind_ips": [...]`。
/// 如果只按 `Vec<String>` 反序列化，老配置会**解析失败**，然后被
/// `#[serde(default)]` 悄悄换成默认值 —— 用户看到的是「我选的网卡怎么没了」，
/// 而且不会有任何报错。所以两种形状都必须能读进来（见 `config::de_string_or_seq`）。
fn bind_config_compat(r: &mut Report) {
    r.section("2e. 监听地址的内存格式与旧配置兼容");

    // (说明, JSON, 期望解析出来的数组)
    let cases: Vec<(&str, &str, Vec<String>)> = vec![
        (
            "旧格式：一个字符串",
            r#"{"bind_ip":"10.0.0.5"}"#,
            vec!["10.0.0.5".to_string()],
        ),
        (
            "旧格式：空字符串 = 所有网卡",
            r#"{"bind_ip":""}"#,
            Vec::new(),
        ),
        (
            "新格式：数组",
            r#"{"bind_ips":["10.0.0.5","192.168.1.7"]}"#,
            vec!["10.0.0.5".to_string(), "192.168.1.7".to_string()],
        ),
        (
            "新格式：空数组 = 所有网卡",
            r#"{"bind_ips":[]}"#,
            Vec::new(),
        ),
        (
            "字段缺失 = 所有网卡",
            r#"{"port":5000}"#,
            Vec::new(),
        ),
        (
            "数组里有空项会被滤掉",
            r#"{"bind_ips":["","10.0.0.5","  "]}"#,
            vec!["10.0.0.5".to_string()],
        ),
        (
            "IPv6 地址也能存",
            r#"{"bind_ips":["fd7a:115c:a1e0::1"]}"#,
            vec!["fd7a:115c:a1e0::1".to_string()],
        ),
    ];

    for (name, json, want) in cases {
        match serde_json::from_str::<Config>(json) {
            Ok(c) => {
                if c.bind_ips == want {
                    r.ok(name);
                } else {
                    r.fail(name);
                    r.info(format!("期望: {want:?}"));
                    r.info(format!("实际: {:?}", c.bind_ips));
                }
            }
            Err(e) => {
                r.fail(name);
                r.info(format!("解析失败: {e}"));
            }
        }
    }
}

/// 2g. 可视化账号列表：拼装、校验、迁移（全是纯函数，直接断言）
///
/// 背景：dufs 定义账号只有 `-a` 这一个入口；界面上的账号列表只是它的可视化写法。
/// 拼装规则（`dufs::build_accounts_args`）：一个账号一个 `-a`，同账号多行合并，
/// 用户名空 = 匿名。校验规则（`config::validate_accounts`）：行号从 1 开始，
/// 因为一条坏规则会让 dufs 整个起不来，必须指明是哪一行。
fn account_list(r: &mut Report) {
    r.section("2g. 可视化账号列表（拼装 / 校验 / 迁移）");

    // ---------- 拼装 ----------
    let b = |rows: Vec<crate::config::AccountRow>| dufs::build_accounts_args(&rows);
    check(
        r,
        "拼装：空列表 = 空（不传 -a）",
        b(vec![]).is_empty(),
    );
    check(
        r,
        "拼装：匿名多行合并成一条 @ 规则",
        b(vec![acc("", "", "/pub", "ro"), acc("", "", "/", "ro")])
            == vec!["@/pub:ro,/:ro".to_string()],
    );
    check(
        r,
        "拼装：顺序按首次出现（稳定可重现）",
        b(vec![acc("b", "p", "/", "rw"), acc("a", "p", "/", "rw")])
            == vec!["b:p@/:rw".to_string(), "a:p@/:rw".to_string()],
    );
    check(
        r,
        "拼装：空路径的行被跳过（不污染输出）",
        b(vec![acc("a", "p", "", "rw"), acc("a", "p", "/", "rw")])
            == vec!["a:p@/:rw".to_string()],
    );

    // ---------- 校验：合法的放行 ----------
    let v = |rows: Vec<crate::config::AccountRow>| crate::config::validate_accounts(&rows).is_empty();
    check(
        r,
        "校验：正常的多账号列表能过",
        v(vec![
            acc("admin", "pw", "/", "rw"),
            acc("admin", "pw", "/pub", "ro"),
            acc("guest", "gp", "/pub", "ro"),
            acc("", "", "/open", "ro"),
        ]),
    );
    check(r, "校验：空列表能过（等于没配账号）", v(vec![]));
    check(
        r,
        "校验：全空的行直接跳过（刚点添加还没填）",
        v(vec![acc("", "", "", "rw")]),
    );

    // ---------- 校验：非法的拦住 ----------
    //
    // ★ 断言是**结构化**的：只看「哪一行、哪一类问题」，不看文案。
    //   以前这里写的是 `e.contains("第 1 行")` —— 文案一改（或一翻译）自检就红，
    //   而且那本质上是拿中文当断言依据。见 `config::AccountProblem`。
    use crate::config::{AccountProblemKind as K, validate_accounts};
    // 期望：第 `row` 行（**0 基**）报出 `want` 这一种问题
    let hits_at = |rows: Vec<crate::config::AccountRow>, row: usize, want: K| {
        validate_accounts(&rows)
            .iter()
            .any(|p| p.index == row && p.kind == want)
    };
    check(
        r,
        "校验：用户名里有 @ 被拦下并指明行号",
        hits_at(
            vec![acc("a@b", "p", "/", "rw")],
            0,
            K::NameHasSyntaxChar('@'),
        ),
    );
    check(
        r,
        "校验：同名不同密码被拦下并指明两行",
        // 第 0 行是好的、第 1 行报冲突，且指回第 0 行 —— 行号口径一起钉住
        hits_at(
            vec![acc("a", "p1", "/", "rw"), acc("a", "p2", "/pub", "ro")],
            1,
            K::NameConflictingSecret {
                other: 0,
                name: "a".to_string(),
            },
        ),
    );
    check(
        r,
        "校验：匿名行填了密码被拦下",
        hits_at(
            vec![acc("", "secret", "/pub", "ro")],
            0,
            K::AnonymousWithSecret,
        ),
    );
    check(
        r,
        "校验：路径不以 / 开头被拦下",
        hits_at(
            vec![acc("a", "p", "pub", "rw")],
            0,
            K::PathNotAbsolute {
                path: "pub".to_string(),
            },
        ),
    );
    check(
        r,
        "校验：路径为空被拦下",
        hits_at(vec![acc("a", "p", "", "rw")], 0, K::PathMissing),
    );
    check(
        r,
        "校验：$5$ 哈希被拦下（dufs 只认 $6$，别的会被当明文）",
        hits_at(
            vec![acc("a", "$5$xyz$abc", "/", "rw")],
            0,
            K::UnsupportedHash,
        ),
    );
    check(
        r,
        "校验：$6$ 哈希放行",
        v(vec![acc(
            "a",
            "$6$abcdefgh$KQeXafAQAaOoKTevphVU215RvJdgzyfASRasIOuh12hO8u0r1bGW92ZnTmC9IjsiQ8VPiTXBiZF49dFL1U4wX/",
            "/",
            "rw",
        )]),
    );
    // ★ 权限现在是枚举：**非法值在配置加载阶段就被 serde 拒了**，根本走不到
    //   validate_accounts（那条检查已经删掉）。用例位置前移，覆盖面不变 ——
    //   而且这是更好的一道门：连"构造出一个非法权限"都不可能。
    check(
        r,
        "校验：非法权限在配置加载阶段就被拒（枚举，不再靠 validate 兜）",
        serde_json::from_str::<crate::config::AccountRow>(r#"{"perm":"xx"}"#).is_err(),
    );
    check(
        r,
        "校验：密码里的冒号放行（dufs 按第一个冒号切，实测能登录）",
        v(vec![acc("user", "p:a", "/", "rw")]),
    );
    check(
        r,
        "校验：密码里的 @ 照样拦（会破坏 @/ 的切分）",
        hits_at(
            vec![acc("user", "p@a", "/", "rw")],
            0,
            K::SecretHasSyntaxChar('@'),
        ),
    );
    check(
        r,
        "校验：匿名行（用户名留空）= 用户名相关检查全部跳过",
        // 匿名行不查用户名、也不查密码是否为空，只要求密码为空
        v(vec![acc("", "", "/pub", "ro")]),
    );
    check(
        r,
        "校验：一次能报出多行的问题（不是只报第一条）",
        validate_accounts(&[acc("a@b", "p", "/", "rw"), acc("c", "p", "bad", "rw")]).len() == 2,
    );

    // ---------- 迁移：老用户名/密码框 -> 列表首行 ----------
    let mut c: Config =
        serde_json::from_str(r#"{"username":"admin","password":"pw"}"#).unwrap();
    check(
        r,
        "迁移前：username 还在、accounts 还是空的",
        c.username == "admin" && c.accounts.is_empty(),
    );
    c.migrate_legacy();
    check(
        r,
        "迁移：老用户名/密码变成列表首行（/ + rw）",
        c.accounts == vec![acc("admin", "pw", "/", "rw")],
    );
    // 只读模式下迁移出来的是 :ro
    let mut c2: Config = serde_json::from_str(
        r#"{"username":"admin","password":"pw","readonly_mode":true}"#,
    )
    .unwrap();
    c2.migrate_legacy();
    check(
        r,
        "迁移：只读模式下迁移出来的是 :ro",
        c2.accounts == vec![acc("admin", "pw", "/", "ro")],
    );
    // 列表非空时不动老框（用户两边都写了，以列表为准）
    let mut c3: Config = serde_json::from_str(
        r#"{"username":"old","accounts":[{"name":"new","secret":"p","path":"/","perm":"rw"}]}"#,
    )
    .unwrap();
    c3.migrate_legacy();
    check(
        r,
        "迁移：列表非空时不动（以列表为准，不覆盖）",
        c3.accounts == vec![acc("new", "p", "/", "rw")],
    );
    // 幂等
    c.migrate_legacy();
    check(
        r,
        "迁移：第二次调用什么都不做（username 已空）",
        c.accounts == vec![acc("admin", "pw", "/", "rw")],
    );

    // ---------- 旧原文框 → 列表（原文框 UI 已删，只剩这条迁移路）----------
    let mut c4: Config = serde_json::from_str(
        r#"{"auth_rules":"admin:pw@/:rw;guest:gp@/pub","accounts":[{"name":"dead","secret":"x","path":"/","perm":"rw"}]}"#,
    )
    .unwrap();
    c4.migrate_legacy();
    check(
        r,
        "迁移：原文非空时转成行（分号拆 + 省略权限=只读）",
        c4.accounts
            == vec![
                acc("admin", "pw", "/", "rw"),
                acc("guest", "gp", "/pub", "ro"),
            ],
    );
    check(r, "迁移：转完原文清空（不再写回）", c4.auth_rules.is_empty());
    // 管道符分隔也能转
    let mut c5: Config =
        serde_json::from_str(r#"{"auth_rules":"a:b@/x:ro|@/open"}"#).unwrap();
    c5.migrate_legacy();
    check(
        r,
        "迁移：管道符分隔 + 匿名行也能转",
        c5.accounts
            == vec![acc("a", "b", "/x", "ro"), acc("", "", "/open", "ro")],
    );
    // 原文为空时不动列表
    let mut c6: Config = serde_json::from_str(
        r#"{"auth_rules":"","accounts":[{"name":"keep","secret":"p","path":"/","perm":"rw"}]}"#,
    )
    .unwrap();
    c6.migrate_legacy();
    check(
        r,
        "迁移：原文为空时不动列表",
        c6.accounts == vec![acc("keep", "p", "/", "rw")],
    );
    // 落盘往返：迁完序列化，行在、原文消失（skip_serializing）。
    // 这就是"下次启动配置还在"的保证（文件里不再有原文框的影子）。
    let json = serde_json::to_string(&c4).unwrap();
    check(
        r,
        "迁移：落盘后有行、无原文（往返保证持久化）",
        json.contains("guest") && !json.contains("auth_rules"),
    );
}

fn validations(r: &mut Report) {
    r.section("2. 启动前校验（该拦的必须拦住）");

    let tmp = std::env::temp_dir();
    let real_folder = tmp.to_string_lossy().into_owned();

    // 供 assets 用例使用
    let assets_ok = tmp.join("dufs-gui-opts-assets");
    let assets_bad = tmp.join("dufs-gui-opts-assets-empty");
    let _ = std::fs::create_dir_all(&assets_ok);
    let _ = std::fs::write(assets_ok.join("index.html"), b"<h1>x</h1>");
    let _ = std::fs::create_dir_all(&assets_bad);

    // 配置文件用例
    let cfg_json = tmp.join("dufs-gui-opts-cfg.json");
    let cfg_toml = tmp.join("dufs-gui-opts-cfg.toml");
    let _ = std::fs::write(&cfg_json, br#"{"port":52113}"#);
    let _ = std::fs::write(&cfg_toml, b"port = 52113\n");

    // TLS 存在性校验用的假文件（真的证书由 Fixture 用 openssl 生成，环节在更后面）
    let dummy_cert = tmp.join("dufs-gui-opts-dummy-cert.pem");
    let dummy_key = tmp.join("dufs-gui-opts-dummy-key.pem");
    let _ = std::fs::write(&dummy_cert, b"-----BEGIN CERTIFICATE-----\n");
    let _ = std::fs::write(&dummy_key, b"-----BEGIN PRIVATE KEY-----\n");

    let base = Config {
        serve_path: real_folder,
        ..Config::default()
    };

    // (说明, 期望被拦?, 修改配置)
    type Mut = Box<dyn Fn(&mut Config)>;
    let cases: Vec<(&str, bool, Mut)> = vec![
        ("共享路径为空 -> 拦", true, Box::new(|c: &mut Config| c.serve_path = "  ".into())),
        (
            "共享路径不存在 -> 拦",
            true,
            Box::new(|c: &mut Config| c.serve_path = r"C:\definitely-not-here-9f3a".into()),
        ),
        ("端口 0 -> 拦", true, Box::new(|c: &mut Config| c.port = 0)),
        (
            "HTTPS 只填证书不填私钥 -> 拦",
            true,
            Box::new(|c: &mut Config| c.tls_cert = r"C:\c\cert.pem".into()),
        ),
        (
            "HTTPS 只填私钥不填证书 -> 拦",
            true,
            Box::new(|c: &mut Config| c.tls_key = r"C:\c\key.pem".into()),
        ),
        (
            "旧原文规则能解析成行（分号分隔 + 省略权限=只读）",
            false,
            Box::new(|c: &mut Config| {
                let rows = crate::config::parse_auth_rules_to_accounts(
                    "admin:pw@/:rw;guest:gp@/pub",
                );
                c.use_auth = true;
                c.accounts = rows;
            }),
        ),
        (
            "旧原文规则里的 :r 转出来照样被拦（权限非法）",
            true,
            Box::new(|c: &mut Config| {
                let rows =
                    crate::config::parse_auth_rules_to_accounts("admin:pw@/:r");
                c.use_auth = true;
                c.accounts = rows;
            }),
        ),
        (
            "旧原文规则里的死规则（没密码）转出来被拦（密码不能为空）",
            true,
            Box::new(|c: &mut Config| {
                let rows =
                    crate::config::parse_auth_rules_to_accounts("admin@/path");
                c.use_auth = true;
                c.accounts = rows;
            }),
        ),
        (
            "旧原文规则里的同名不同密码转出来被拦（启动前指明）",
            true,
            Box::new(|c: &mut Config| {
                let rows = crate::config::parse_auth_rules_to_accounts(
                    "a:pw1@/x;a:pw2@/y",
                );
                c.use_auth = true;
                c.accounts = rows;
            }),
        ),
        (
            "dufs 配置文件不存在 -> 拦",
            true,
            Box::new(|c: &mut Config| c.dufs_config = r"C:\nope-9f3a.json".into()),
        ),
        (
            "dufs 配置是 .toml -> 拦（0.46 实测不支持）",
            true,
            Box::new({
                let p = cfg_toml.to_string_lossy().into_owned();
                move |c: &mut Config| c.dufs_config = p.clone()
            }),
        ),
        (
            "资源目录不存在 -> 拦",
            true,
            Box::new(|c: &mut Config| c.assets = r"C:\nope-9f3a-assets".into()),
        ),
        (
            "资源目录缺 index.html -> 拦",
            true,
            Box::new({
                let p = assets_bad.to_string_lossy().into_owned();
                move |c: &mut Config| c.assets = p.clone()
            }),
        ),
        (
            "dufs 配置是 .json -> 放行",
            false,
            Box::new({
                let p = cfg_json.to_string_lossy().into_owned();
                move |c: &mut Config| c.dufs_config = p.clone()
            }),
        ),
        (
            "资源目录有 index.html -> 放行",
            false,
            Box::new({
                let p = assets_ok.to_string_lossy().into_owned();
                move |c: &mut Config| c.assets = p.clone()
            }),
        ),
        (
            "HTTPS 证书私钥都填 -> 放行",
            false,
            Box::new({
                let (a, b) = (
                    cfg_json.to_string_lossy().into_owned(),
                    cfg_toml.to_string_lossy().into_owned(),
                );
                move |c: &mut Config| {
                    c.tls_cert = a.clone();
                    c.tls_key = b.clone();
                }
            }),
        ),
        (
            "勾了「启用账号访问」但一个账号都没填 -> 拦（否则实际无任何认证）",
            true,
            Box::new(|c: &mut Config| {
                c.use_auth = true;
            }),
        ),
        (
            "启用认证且账号列表里有账号 -> 放行",
            false,
            Box::new(|c: &mut Config| {
                c.use_auth = true;
                c.accounts = vec![acc("admin", "pw", "/", "rw")];
            }),
        ),
        (
            "启用认证但列表全空 -> 拦（原文框已删，没有第二条路）",
            true,
            Box::new(|c: &mut Config| {
                c.use_auth = true;
            }),
        ),
        (
            "HTTPS 证书路径不存在 -> 拦",
            true,
            Box::new({
                let (a, b) = (
                    dummy_cert.to_string_lossy().into_owned(),
                    dummy_key.to_string_lossy().into_owned(),
                );
                move |c: &mut Config| {
                    c.tls_cert = a.clone();
                    c.tls_key = format!("{b}.not-here");
                }
            }),
        ),
        (
            "HTTPS 证书私钥都存在 -> 放行",
            false,
            Box::new({
                let (a, b) = (
                    dummy_cert.to_string_lossy().into_owned(),
                    dummy_key.to_string_lossy().into_owned(),
                );
                move |c: &mut Config| {
                    c.tls_cert = a.clone();
                    c.tls_key = b.clone();
                }
            }),
        ),
        // ★ serve-path 是**文件**也要放行 —— dufs 本来就支持，
        //   早先 Config::validate 只认 is_dir()，把「共享单个文件」堵在了界面外。
        (
            "共享路径是单个文件 -> 放行",
            false,
            Box::new({
                let p = dummy_cert.to_string_lossy().into_owned();
                move |c: &mut Config| c.serve_path = p.clone()
            }),
        ),
        ("正常配置（文件夹）-> 放行", false, Box::new(|_: &mut Config| {})),
    ];

    for (name, should_fail, mutate) in cases {
        let mut cfg = base.clone();
        (mutate)(&mut cfg);
        match (cfg.validate(), should_fail) {
            (Err(_), true) | (Ok(()), false) => r.ok(name),
            (Ok(()), true) => {
                r.fail(format!("{name}（实际放行了）"));
            }
            (Err(e), false) => {
                r.fail(format!("{name}（实际被拦：{e}）"));
            }
        }
    }

    // ---------- 重复选项检测（check_options）----------
    // dufs 对重复选项是直接报错退出（clap 的 cannot be used multiple times），
    // 而「额外命令行参数」是原文透传，用户很容易把界面已有的选项再抄一遍。
    let conflict_cases: [(&str, &[&str], bool); 7] = [
        (
            "额外参数重复了界面上的 --allow-archive -> 拦",
            &["-b", "0.0.0.0", "-p", "5000", "--allow-archive", "--allow-archive"],
            true,
        ),
        (
            "额外参数重复了界面的端口（-p 与 --port 混用）-> 拦",
            &["-p", "5000", "--port", "6000"],
            true,
        ),
        (
            "--port=5000 与 -p 混用也算重复 -> 拦",
            &["--port=5000", "-p", "6000"],
            true,
        ),
        (
            "额外参数里放 --completions -> 拦（它只会打印然后退出）",
            &["--completions", "bash"],
            true,
        ),
        (
            "--hidden 重复是 dufs 允许的 -> 放行",
            &["--hidden", "a", "--hidden", "b"],
            false,
        ),
        (
            "-a 重复是 dufs 允许的（多账号）-> 放行",
            &["-a", "x:p@/", "-a", "y:q@/"],
            false,
        ),
        (
            "正常参数组合 -> 放行",
            &["-b", "0.0.0.0", "-p", "5000", "--allow-upload", r"C:\share"],
            false,
        ),
    ];
    for (name, args, should_err) in conflict_cases {
        let owned: Vec<String> = args.iter().map(|s| s.to_string()).collect();
        match (dufs::check_options(&owned), should_err) {
            (Err(_), true) | (Ok(()), false) => r.ok(name),
            (Ok(()), true) => r.fail(format!("{name}（实际放行了）")),
            (Err(e), false) => {
                r.fail(format!("{name}（实际被拦：{e}）"));
            }
        }
    }

    // 接线检查：`check_options` 必须真的被 `dufs::start()` 调上。
    // 冲突时应该在落盘/起进程之前就返回 Err。
    {
        let cfg = Config {
            serve_path: std::env::temp_dir().to_string_lossy().into_owned(),
            port: 52123,
            bind_ips: vec!["127.0.0.1".into()],
            allow_archive: true,
            extra_args: "--allow-archive".into(), // 与界面上的开关重复
            ..Config::default()
        };
        let args = dufs::build_args(&cfg);
        if let Ok(exe) = dufs::ensure_dufs() {
            // ★ 结构断言：不看文案，直接匹配错误的种类。
            //   以前是 `e.contains("重复")` —— 拿中文当断言依据（见 config::AccountProblem）。
            match dufs::start(&cfg, &exe) {
                Err(dufs::StartError::Options(dufs::OptionConflict::Duplicate { .. })) => {
                    r.ok("接线：dufs::start() 会先生成重复选项检查并拒绝启动")
                }
                Err(e) => {
                    r.fail(format!(
                        "接线：dufs::start() 报了错但不是「重复选项」：{e}"
                    ));
                }
                Ok((mut child, _, _)) => {
                    dufs::stop(&mut child);
                    r.fail(format!(
                        "接线：重复选项竟然没被阻止，真的把 dufs 起来了。参数: {}",
                        args.join(" ")
                    ));
                }
            }
        } else {
            r.skip("接线：拿不到 dufs 可执行文件，跳过");
        }
    }
}

// ============================================================
// 2c. 恢复默认设置（两套基准的差别）
// ============================================================

/// `Config::restored()` 的断言。
///
/// 两套基准只差两个字段,但那两个字段决定了“点完还能不能用手机传文件”,
/// 所以每一条都要钉住。同时验证一个关键点:**共享路径和本软件的设置不能被清掉**。
fn restore_defaults(r: &mut Report) {
    r.section("2c. 恢复默认设置（两套基准）");

    // 先造一份“被改乱了”的配置:权限全开 + 认证 + TLS + 端口网卡都改过,
    // 同时把本软件的设置也设成“非默认值”以便验证它们会被保留。
    let messy = Config {
        serve_path: FOLDER.into(),
        port: 54321,
        bind_ips: vec!["10.1.2.3".into()],
        allow_all: true,
        allow_upload: false,
        allow_search: false,
        allow_delete: true,
        allow_archive: true,
        allow_hash: true,
        allow_symlink: true,
        readonly_mode: true,
        compress: Compress::High,
        enable_cors: true,
        hidden: "*.log".into(),
        use_auth: true,
        accounts: vec![acc("u", "p", "/", "rw")],
        auth_rules: "a:b@/:rw".into(),
        path_prefix: "pfx".into(),
        render_mode: RenderMode::Spa,
        tls_cert: "c.pem".into(),
        tls_key: "k.pem".into(),
        log_file: "l.log".into(),
        log_format: "fmt".into(),
        log_disabled: true,
        assets: "as".into(),
        dufs_config: "c.json".into(),
        extra_args: "--x".into(),
        // 本软件的设置:恢复默认**不应该**动它们
        close_action: crate::config::CloseAction::Tray,
        confirm_on_close: false,
        stay_on_top: true,
        autostart: true,
        autostart_serve: true,
        recent_paths: vec![FOLDER.to_string()],
        window_x: Some(11),
        window_y: Some(22),
        window_width: Some(777.0),
        window_height: Some(444.0),
        window_maximized: true,
        verbose_log: false,
        ..Config::default()
    };

    use crate::config::Baseline;
    let app = messy.restored(Baseline::AppDefaults);
    let dufs = messy.restored(Baseline::DufsNative);

    let cases: Vec<(&str, bool)> = vec![
        (
            "本程序默认：允许上传 + 允许搜索",
            app.allow_upload && app.allow_search,
        ),
        ("本程序默认：不允许删除", !app.allow_delete),
        (
            "dufs 原生默认：纯只读（上传/搜索都关）",
            !dufs.allow_upload && !dufs.allow_search,
        ),
        (
            "两套都清掉 allow-all / 打包 / hash / 链接",
            !app.allow_all && !app.allow_archive && !app.allow_hash && !app.allow_symlink && !dufs.allow_all,
        ),
        (
            "两套都清掉只读模式标记",
            !app.readonly_mode && !dufs.readonly_mode,
        ),
        (
            "两套都清掉认证与认证规则",
            !app.use_auth
                && app.accounts.is_empty()
                && app.auth_rules.is_empty()
                && !dufs.use_auth,
        ),
        (
            "两套都清掉 URL 前缀 / 渲染模式 / CORS / 隐藏",
            app.path_prefix.is_empty()
                && app.render_mode == RenderMode::Default
                && !app.enable_cors
                && app.hidden.is_empty()
                && dufs.render_mode == RenderMode::Default,
        ),
        (
            "两套都清掉 TLS / 日志 / 资源 / -c / 额外参数",
            app.tls_cert.is_empty()
                && app.tls_key.is_empty()
                && app.log_file.is_empty()
                && app.log_format.is_empty()
                && !app.log_disabled
                && app.assets.is_empty()
                && app.dufs_config.is_empty()
                && app.extra_args.is_empty(),
        ),
        (
            "两套都把端口/网卡恢复默认",
            app.port == 5000
                && app.bind_ips.is_empty()
                && dufs.port == 5000
                && dufs.bind_ips.is_empty(),
        ),
        (
            "两套都保留共享路径（清掉它就没东西可分享了）",
            app.serve_path == FOLDER && dufs.serve_path == FOLDER,
        ),
        (
            "两套都不动本软件的设置（置顶/关闭行为/询问/自启/窗口状态/详细日志）",
            app.close_action == crate::config::CloseAction::Tray
                && !app.confirm_on_close
                && app.autostart
                && app.autostart_serve
                && app.recent_paths.len() == 1
                && app.window_x == Some(11)
                && app.window_y == Some(22)
                && app.window_width == Some(777.0)
                && app.window_height == Some(444.0)
                && app.window_maximized
                && app.stay_on_top
                && !app.verbose_log
                && dufs.stay_on_top
                && !dufs.verbose_log,
        ),
    ];
    for (name, ok) in cases {
        check(r, name, ok);
    }

    // 交叉验证:光看字段不够,恢复后的配置拼出来的命令行也得符合预期
    let app_args = dufs::build_args(&app).join(" ");
    check(
        r,
        "本程序默认拼出的命令行带着 --allow-upload / --allow-search",
        app_args.contains("--allow-upload") && app_args.contains("--allow-search"),
    );
    check(
        r,
        "本程序默认拼出的命令行里没有 --allow-delete",
        !app_args.contains("--allow-delete"),
    );
    let dufs_args = dufs::build_args(&dufs).join(" ");
    check(
        r,
        "dufs 原生默认拼出的命令行里一个 --allow-* 都没有（纯只读）",
        !dufs_args.contains("--allow-upload")
            && !dufs_args.contains("--allow-search")
            && !dufs_args.contains("--allow-delete")
            && !dufs_args.contains("--allow-all"),
    );
}

// ============================================================
// 3. 访问地址拼装（这块曾经是盲区）
// ============================================================
/// 断言「界面上会显示 / 二维码会编码」的那个地址。
///
/// ★ 这组用例是修 bug 时专门补的：带 `--path-prefix` 或开了 HTTPS 时，
///   参传对了、dufs 行为也对，但显示给用户的地址是错的。
fn access_urls(r: &mut Report) {
    r.section("3. 访问地址拼装（协议 + URL 前缀）");

    let ip: IpAddr = "192.168.1.7".parse().unwrap();

    // (说明, https, 端口, 前缀, 期望的完整地址)
    let cases: Vec<(&str, bool, u16, &str, &str)> = vec![
        ("无前缀 + http", false, 5000, "", "http://192.168.1.7:5000"),
        ("普通前缀", false, 5000, "myfiles", "http://192.168.1.7:5000/myfiles/"),
        ("前缀带前导斜杠", false, 5000, "/myfiles", "http://192.168.1.7:5000/myfiles/"),
        ("前缀带尾斜杠", false, 5000, "myfiles/", "http://192.168.1.7:5000/myfiles/"),
        ("前缀首尾都有斜杠", false, 5000, "/myfiles/", "http://192.168.1.7:5000/myfiles/"),
        ("多级前缀保留内部斜杠", false, 5000, "a/b", "http://192.168.1.7:5000/a/b/"),
        ("只有斜杠视为无前缀", false, 5000, "///", "http://192.168.1.7:5000"),
        ("两边空白不算前缀", false, 5000, "   ", "http://192.168.1.7:5000"),
        ("空格要百分号编码", false, 5000, "a b", "http://192.168.1.7:5000/a%20b/"),
        ("中文要百分号编码", false, 5000, "我的文件", "http://192.168.1.7:5000/%E6%88%91%E7%9A%84%E6%96%87%E4%BB%B6/"),
        ("问号/井号要编码", false, 5000, "a?b#c", "http://192.168.1.7:5000/a%3Fb%23c/"),
        ("已有百分号要再编码", false, 5000, "100%", "http://192.168.1.7:5000/100%25/"),
        ("HTTPS 无前缀", true, 8443, "", "https://192.168.1.7:8443"),
        ("HTTPS + 前缀", true, 8443, "myfiles", "https://192.168.1.7:8443/myfiles/"),
        ("HTTPS + 中文前缀", true, 8443, "我的文件", "https://192.168.1.7:8443/%E6%88%91%E7%9A%84%E6%96%87%E4%BB%B6/"),
    ];

    for (name, https, port, prefix, expect) in cases {
        let got = net::access_url(ip, port, https, prefix);
        if got == expect {
            r.ok(name);
        } else {
            r.fail(name);
            r.info(format!("期望: {expect}"));
            r.info(format!("实际: {got}"));
        }
    }

    // ---------- IPv6 地址：方括号少一个，扫码就是打不开 ----------
    //
    // `http://fd7a::1:5000/` 不是合法 URL（host 和 port 分不开），
    // 正确写法必须把 IPv6 用方括号包起来。
    let v6: IpAddr = "fd7a:115c:a1e0::5a36:2377".parse().unwrap();
    let v6_cases: Vec<(&str, bool, u16, &str, &str)> = vec![
        (
            "IPv6 无前缀要加方括号",
            false,
            5000,
            "",
            "http://[fd7a:115c:a1e0::5a36:2377]:5000",
        ),
        (
            "IPv6 + 前缀",
            false,
            5000,
            "myfiles",
            "http://[fd7a:115c:a1e0::5a36:2377]:5000/myfiles/",
        ),
        (
            "IPv6 + HTTPS",
            true,
            8443,
            "",
            "https://[fd7a:115c:a1e0::5a36:2377]:8443",
        ),
    ];
    for (name, https, port, prefix, expect) in v6_cases {
        let got = net::access_url(v6, port, https, prefix);
        if got == expect {
            r.ok(name);
        } else {
            r.fail(name);
            r.info(format!("期望: {expect}"));
            r.info(format!("实际: {got}"));
        }
    }
    check(
        r,
        "IPv6 回环 [::1] 同样要方括号",
        net::access_url("::1".parse().unwrap(), 5000, false, "") == "http://[::1]:5000",
    );

    // ---------- 网卡列表的过滤规则 ----------
    //
    // 现在 IPv4 和 IPv6 都收，但「不可用的地址」两种协议族各有各的定义，
    // 这里把「不该出现的」逐条钉死 —— 一旦哪天过滤写漏了，用户会看到
    // 一个永远连不上的地址被摆在下拉框里。
    let real = net::candidates();
    check(
        r,
        "网卡列表里没有回环地址",
        !real.iter().any(|c| c.ip.is_loopback()),
    );
    check(
        r,
        "网卡列表里没有 IPv4 link-local（169.254，说明没拿到 DHCP）",
        !real.iter().any(|c| match c.ip {
            IpAddr::V4(v4) => v4.octets()[0] == 169 && v4.octets()[1] == 254,
            _ => false,
        }),
    );
    check(
        r,
        "网卡列表里没有 IPv6 link-local（fe80:: 必须带 %zone，塞不进二维码）",
        !real.iter().any(|c| match c.ip {
            IpAddr::V6(v6) => (v6.segments()[0] & 0xffc0) == 0xfe80,
            _ => false,
        }),
    );
    check(
        r,
        "网卡列表里没有组播 / 未指定地址",
        !real.iter().any(|c| c.ip.is_multicast() || c.ip.is_unspecified()),
    );
    r.info(format!(
        "本机候选地址（IPv4 {} 个 / IPv6 {} 个）: {}",
        real.iter().filter(|c| matches!(c.ip, IpAddr::V4(_))).count(),
        real.iter().filter(|c| matches!(c.ip, IpAddr::V6(_))).count(),
        real.iter()
            .map(|c| c.ip.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    ));

    // ---------- 网卡打分（纯函数，可以拿合成地址直接测）----------
    //
    // ★ 这组断言是一次真实回归换来的：调 Stage B 时给 IPv6 ULA 打了 95 分，
    //   结果 Meta Quest Link 虚拟网卡的 `fdfe:dcba:9876::1` 压过了真正的局域网
    //   地址 `10.10.0.20`（90 分），界面和二维码直接指向一张手机连不上的网卡。
    //   现在用纯函数把「谁该排在谁前面」钉死。
    let sc = |ip: &str, name: &str| net::score_addr(ip.parse().unwrap(), name);
    check(
        r,
        "打分：回环 / 未指定 / 组播 一律不收",
        sc("127.0.0.1", "x").is_none()
            && sc("::1", "x").is_none()
            && sc("0.0.0.0", "x").is_none()
            && sc("224.0.0.1", "x").is_none(),
    );
    check(
        r,
        "打分：IPv4 link-local（169.254，没拿到 DHCP）不收",
        sc("169.254.1.1", "以太网").is_none(),
    );
    check(
        r,
        "打分：IPv6 link-local（fe80:: 必须带 %zone）不收",
        sc("fe80::1234", "以太网").is_none(),
    );
    check(
        r,
        "打分：家用 192.168 最高分",
        sc("192.168.1.5", "以太网") == Some(100),
    );
    check(
        r,
        "打分：10.x 次高",
        sc("10.1.2.3", "以太网") == Some(90),
    );
    check(
        r,
        "打分：虚拟网卡按名字重罚-200",
        sc("192.168.1.5", "Tailscale") == Some(-100),
    );
    check(
        r,
        "打分：Meta Quest Link（虚拟头显网卡）会被名字罚下去",
        sc("fdfe:dcba:9876::1", "Meta") == Some(55 - 200),
    );
    {
        let ula_meta = sc("fdfe:dcba:9876::1", "Meta").unwrap();
        let real_lan = sc("10.10.0.20", "以太网").unwrap();
        check(
            r,
            "打分：Meta 的 ULA 不会压过真正的局域网地址（实测回归）",
            ula_meta < real_lan,
        );
        let ula = sc("fd00::1", "以太网").unwrap();
        let global = sc("2400:cb00::1", "以太网").unwrap();
        check(
            r,
            "打分：任何 IPv6 都排在 10.x（90）/ 172.16-31（80）后面",
            ula < 80 && global < 80,
        );
        check(
            r,
            "打分：IPv6 单栈（只有 IPv6）时仍然选得出地址",
            ula > 0 && global > 0,
        );
    }

    // ---------- 「本机还有没有这个监听地址」（纯函数）----------
    //
    // 用户选的网卡可能在某次拔网线 / 关 VPN 之后就没了。这条规则同时用在两处：
    // 启动时清理一次，启动服务前再查一次（否则 dufs 会甩回一句
    // `Error: Failed to bind`，用户完全看不懂）。
    {
        use crate::net::missing_binds;
        let mk = |v: &[&str]| -> Vec<String> { v.iter().map(|s| s.to_string()).collect() };
        let local: Vec<IpAddr> = ["10.0.0.5", "192.168.1.7"]
            .iter()
            .map(|s| s.parse().unwrap())
            .collect();

        check(
            r,
            "失效检测：本机有的地址不算失效",
            missing_binds(&mk(&["10.0.0.5"]), &local).is_empty(),
        );
        check(
            r,
            "失效检测：本机没有的地址算失效",
            missing_binds(&mk(&["10.99.99.99"]), &local) == mk(&["10.99.99.99"]),
        );
        check(
            r,
            "失效检测：回环地址永远不算失效（可以合法绑 127.0.0.1）",
            missing_binds(&mk(&["127.0.0.1", "127.1.2.3", "::1"]), &local).is_empty(),
        );
        check(
            r,
            "失效检测：解析不出来的字符串算失效（配置文件被人手改过）",
            missing_binds(&mk(&["not-an-ip"]), &local) == mk(&["not-an-ip"]),
        );
        check(
            r,
            "失效检测：空项和空格跳过",
            missing_binds(&mk(&["", "   "]), &local).is_empty(),
        );
        check(
            r,
            "失效检测：混在一起时只挑出失效的，并保持原顺序",
            missing_binds(&mk(&["10.0.0.5", "10.99.99.99", "192.168.1.7", "1.2.3.4"]), &local)
                == mk(&["10.99.99.99", "1.2.3.4"]),
        );
        check(
            r,
            "失效检测：首尾空格不影响判断",
            missing_binds(&mk(&["  10.0.0.5  "]), &local).is_empty(),
        );
        check(
            r,
            "失效检测：本机枚举不出来任何地址时（拔了网线）→ 什么都不删，宁可让 dufs 报错",
            missing_binds(&mk(&["10.0.0.5"]), &[]).is_empty(),
        );
    }

    // ---------- 平铺两栏（纯函数）----------
    //
    // 界面上的取下标算法是「左栏第 i 项 = 扁平第 2i 项、右栏第 j 项 = 第 2j+1 项」，
    // 必须和这里严格一致。错位的话用户勾 A 会绑上 B，而且**不会有任何报错**。
    {
        use crate::service::tile_columns;
        let mk = |v: &[&str]| -> Vec<String> { v.iter().map(|s| s.to_string()).collect() };

        // ★ 局部变量千万别叫 `r` —— 那样会把外层的 `Report` 引用遮蔽掉
        let five = mk(&["a", "b", "c", "d", "e"]);
        let (left, right) = tile_columns(&five);
        check(
            r,
            "平铺：5 项 -> 左栏 a,c,e / 右栏 b,d",
            left == mk(&["a", "c", "e"]) && right == mk(&["b", "d"]),
        );
        // 按界面的下标公式反查，必须能一一对上（这是真正要紧的那条）
        let back: Vec<String> = (0..five.len())
            .map(|k| {
                if k % 2 == 0 {
                    left[k / 2].clone()
                } else {
                    right[k / 2].clone()
                }
            })
            .collect();
        check(r, "平铺：按 2i / 2j+1 反查能还原成原顺序", back == five);

        let (l0, r0) = tile_columns(&[]);
        check(r, "平铺：空列表 -> 两栏都空", l0.is_empty() && r0.is_empty());
        let (l1, r1) = tile_columns(&mk(&["only"]));
        check(
            r,
            "平铺：1 项 -> 只在左栏，右栏为空",
            l1 == mk(&["only"]) && r1.is_empty(),
        );
        let (l2, r2) = tile_columns(&mk(&["x", "y"]));
        check(
            r,
            "平铺：2 项 -> 左右各一个",
            l2 == mk(&["x"]) && r2 == mk(&["y"]),
        );
    }

    // ---------- 勾选监听的规则（纯函数）----------
    //
    // 抽出来是因为这套规则有 5 种情况，手点界面很容易漏掉一两种 ——
    // 行数多的那几种（"取消最后一个具体地址回到所有网卡"）尤其容易漏。
    {
        use crate::service::toggle_bind;
        let none: Vec<String> = vec![];
        let one = vec!["10.1.2.3".to_string()];
        let two = vec!["10.1.2.3".to_string(), "192.168.1.5".to_string()];

        let mut ck = |name: &str, got: Vec<String>, want: Vec<String>| {
            if got == want {
                r.ok(name);
            } else {
                r.fail(name);
                r.info(format!("期望: {want:?}"));
                r.info(format!("实际: {got:?}"));
            }
        };
        ck(
            "勾选：勾「所有网卡」会清掉其它所有勾选",
            toggle_bind(&two, 0, true, None),
            none.clone(),
        );
        ck(
            "勾选：取消「所有网卡」不会让列表变空（一个都不绑 dufs 起不来）",
            toggle_bind(&none, 0, false, None),
            none.clone(),
        );
        ck(
            "勾选：「所有网卡」下勾一个具体地址 = 只绑它",
            toggle_bind(&none, 1, true, Some("10.1.2.3")),
            one.clone(),
        );
        ck(
            "勾选：已经有具体地址时再勾一个 = 追加（这就是多选）",
            toggle_bind(&one, 3, true, Some("192.168.1.5")),
            two.clone(),
        );
        ck(
            "勾选：重复勾同一个不会重复添加",
            toggle_bind(&one, 1, true, Some("10.1.2.3")),
            one.clone(),
        );
        ck(
            "勾选：取消其中一个只去掉它",
            toggle_bind(&two, 1, false, Some("10.1.2.3")),
            vec!["192.168.1.5".to_string()],
        );
        ck(
            "勾选：取消最后一个具体地址 = 回到「所有网卡」",
            toggle_bind(&one, 1, false, Some("10.1.2.3")),
            none.clone(),
        );
        ck(
            "勾选：下标越界（取不到地址）时什么都不做",
            toggle_bind(&two, 9, true, None),
            two.clone(),
        );
    }

    // ---------- 「访问地址」下拉框到底给哪些选项 / 选中哪个 ----------
    // 这是「运行中切地址」的核心逻辑：切的是显示，不是监听。
    // 关键是**可达性**：绑定单网卡时只能给那一项，否则切过去扫出来是死链接。
    let cand = |ip: &str, name: &str, score: i32| net::Candidate {
        ip: ip.parse().unwrap(),
        name: name.to_string(),
        score,
    };
    let cs = vec![
        cand("192.168.1.5", "以太网", 100),
        cand("172.26.208.1", "vEthernet (WSL)", -120),
    ];

    // (说明, bind, preferred, 期望列表长度, 期望选中 index, 期望列表首项 IP)
    let choice_cases: [(&str, &str, &str, usize, usize, &str); 6] = [
        ("所有网卡 + 从没选过 -> 用评分最高的", "", "", 2, 0, "192.168.1.5"),
        (
            "所有网卡 + 选过第 2 项 -> 尊重选择（运行中切地址）",
            "",
            "172.26.208.1",
            2,
            1,
            "192.168.1.5",
        ),
        (
            "所有网卡 + 保存的地址已不存在 -> 回落第 0 项",
            "",
            "10.99.99.99",
            2,
            0,
            "192.168.1.5",
        ),
        (
            "绑定单网卡 -> 列表里只有它（不能切到连不通的地址）",
            "192.168.1.5",
            "",
            1,
            0,
            "192.168.1.5",
        ),
        (
            "绑定单网卡 -> 忽略 preferred（以监听为准）",
            "192.168.1.5",
            "172.26.208.1",
            1,
            0,
            "192.168.1.5",
        ),
        (
            "绑定了一个不在候选里的地址 -> 也要显示出来",
            "10.99.99.99",
            "",
            1,
            0,
            "10.99.99.99",
        ),
    ];
    // 多地址绑定的可达性（Stage C）：列表里应该**只有**勾上的那些
    let two = vec!["192.168.1.5".to_string(), "172.26.208.1".to_string()];
    let (list, idx) = crate::service::address_choices(&two, "", &cs);
    check(
        r,
        "多地址绑定 -> 列表里是勾上的那两个",
        list.len() == 2
            && list[0].ip.to_string() == "192.168.1.5"
            && list[1].ip.to_string() == "172.26.208.1",
    );
    check(r, "多地址绑定 -> 默认显示第一个", idx == 0);
    let (_, idx2) = crate::service::address_choices(&two, "172.26.208.1", &cs);
    check(
        r,
        "多地址绑定 -> 两个都可达，所以可以在它们之间切显示",
        idx2 == 1,
    );
    let three = vec![
        "192.168.1.5".to_string(),
        "10.99.99.99".to_string(),
        "172.26.208.1".to_string(),
    ];
    let (list3, _) = crate::service::address_choices(&three, "", &cs);
    check(
        r,
        "多地址绑定 -> 没探测到的地址也照样列出来（否则用户看不到自己绑了什么）",
        list3.len() == 3 && list3[1].ip.to_string() == "10.99.99.99",
    );

    for (name, bind, preferred, want_len, want_idx, want_first) in choice_cases {
        // 空串 = 「所有网卡」= 空数组；否则是"只绑这一个"
        let binds: Vec<String> = if bind.is_empty() {
            Vec::new()
        } else {
            vec![bind.to_string()]
        };
        let (list, index) = crate::service::address_choices(&binds, preferred, &cs);
        let first_ok = list.first().is_some_and(|c| c.ip.to_string() == want_first);
        if list.len() == want_len && index == want_idx && first_ok {
            r.ok(name);
        } else {
            r.fail(name);
            r.info(format!(
                "期望: 长度 {want_len}, index {want_idx}, 首项 {want_first}"
            ));
            r.info(format!(
                "实际: 长度 {}, index {index}, 首项 {:?}",
                list.len(),
                list.first().map(|c| c.ip.to_string())
            ));
        }
    }

    // ---------- 防火墙规则列表的解析（纯函数）----------
    // netsh 的字段名是随系统语言变的，所以只能搜“我们自己起的规则名”。
    // 这里直接拿中文 / 英文两种输出验证。
    {
        let zh = "\n规则名称:                             dufs-gui-5000\n---------------\n已启用:                               是\n\n\
                  规则名称:                             dufs-gui-8080\n---------------\n已启用:                               否\n\n\
                  规则名称:                             ChatGPT\n";
        let en = "\nRule Name:                            dufs-gui-5000\n---------------\nEnabled:                              Yes\n\n\
                  Rule Name:                            dufs-gui-8080\n";
        for (label, text) in [("中文输出", zh), ("英文输出", en)] {
            let got = crate::firewall::parse_rules(text);
            let want = vec!["dufs-gui-5000".to_string(), "dufs-gui-8080".to_string()];
            if got == want {
                r.ok(format!("防火墙规则解析（{label}）：能抽出两条规则"));
            } else {
                r.fail(format!("防火墙规则解析（{label}）不对"));
                r.info(format!("期望: {want:?}"));
                r.info(format!("实际: {got:?}"));
            }
        }
        // 重复出现要能去重；无关内容不能混进来
        let messy = "dufs-gui-5000 ... dufs-gui-5000 ... dufs-gui-x ... dufs-gui-\n dufs-gui-80\n";
        let got = crate::firewall::parse_rules(messy);
        if got == vec!["dufs-gui-5000".to_string(), "dufs-gui-80".to_string()] {
            r.ok("防火墙规则解析：会去重，且不会把 dufs-gui-x 这种错进去");
        } else {
            r.fail("防火墙规则解析的去重/边界处理不对");
            r.info(format!("实际: {got:?}"));
        }
        if crate::firewall::parse_rules("没有与指定标准相匹配的规则。\n").is_empty() {
            r.ok("防火墙规则解析：没有规则时返回空列表");
        } else {
            r.fail("防火墙规则解析：无规则时应当返回空列表");
        }
    }

    // 兜底情形：一个候选都没有（拔了网线）
    let (list, index) = crate::service::address_choices(&[], "", &[]);
    if list.is_empty() && index == 0 {
        r.ok("没有任何候选地址 -> 列表为空（界面会提示未检测到 IP）");
    } else {
        r.fail("没有任何候选地址时应当返回空列表");
    }

    // 绑定了一个不在候选里的地址时，不能把它误判成虚拟网卡（否则界面会弹黄字）
    let (list, _) = crate::service::address_choices(&["10.99.99.99".to_string()], "", &cs);
    if list.first().is_some_and(|c| !c.likely_virtual()) {
        r.ok("指定的监听地址不会被误报成虚拟网卡");
    } else {
        r.fail("指定的监听地址被误判成虚拟网卡了（会多出一句黄字警告）");
    }
}

// ============================================================
// 4. 真实行为
// ============================================================

/// 测试用到的目录与文件
struct Fixture {
    /// 基础共享目录：marker.txt + tmp.log + sub/nested.txt
    base: PathBuf,
    /// 含 index.html 的目录
    with_index: PathBuf,
    /// 不含 index.html 的目录
    no_index: PathBuf,
    /// 自定义资源目录（含 index.html）
    assets: PathBuf,
    /// 自签证书（openssl 可用时才有）
    tls: Option<(PathBuf, PathBuf)>,
}

const MARKER: &str = "dufs-gui-opts-marker.txt";

impl Fixture {
    fn prepare(r: &mut Report) -> Option<Self> {
        r.section("4. 准备测试数据");

        let tmp = std::env::temp_dir();
        let base = tmp.join("dufs-gui-opts-share");
        let with_index = tmp.join("dufs-gui-opts-index");
        let no_index = tmp.join("dufs-gui-opts-noindex");
        let assets = tmp.join("dufs-gui-opts-assets");

        let mut ok = true;
        for dir in [&base, &with_index, &no_index, &assets] {
            if std::fs::create_dir_all(dir).is_err() {
                r.fail(format!("无法创建 {}", dir.display()));
                ok = false;
            }
        }
        if !ok {
            return None;
        }

        let write = |p: PathBuf, body: &[u8]| -> bool { std::fs::write(&p, body).is_ok() };
        let files_ok = write(base.join(MARKER), b"hello from options selftest\n")
            && write(base.join("tmp.log"), b"should be hidden\n")
            && write(base.join("sub_nested.txt"), b"nested\n")
            && write(with_index.join("index.html"), b"<h1>INDEX-MARKER</h1>")
            && write(with_index.join("other.txt"), b"x\n")
            && write(no_index.join("other.txt"), b"y\n");

        if files_ok {
            r.ok(format!("测试数据就绪: {}", base.display()));
        } else {
            r.fail("写入测试文件失败");
            return None;
        }

        // 自签证书：有 openssl 才做 HTTPS 实测
        let tls = make_self_signed(&tmp);
        match &tls {
            Some((c, _)) => r.ok(format!("已生成自签证书: {}", c.display())),
            None => r.skip("没找到 openssl，HTTPS 实测将跳过（参数拼装已覆盖）"),
        }

        Some(Self { base, with_index, no_index, assets, tls })
    }

    fn base_str(&self) -> String {
        self.base.to_string_lossy().into_owned()
    }
}

fn behaviors(r: &mut Report, fx: &Fixture) {
    r.section("5. 真实行为（起真的 dufs，用 HTTP 验证）");

    // ---------- 基准服务：用来做"没开这个选项时应该是另一副样子"的对照 ----------
    let base_port = net::free_port();
    let baseline = Config {
        serve_path: fx.base_str(),
        port: base_port,
        bind_ips: vec!["127.0.0.1".into()],
        ..Config::default()
    };
    let baseline_srv = match Server::start(&baseline) {
        Ok(s) => {
            r.ok(format!("基准服务已启动（端口 {base_port}）"));
            s
        }
        Err(e) => {
            r.fail(format!("基准服务启动失败: {e}"));
            return;
        }
    };

    // 基准：默认权限
    match request(base_port, "/?json", None) {
        Ok(resp) => {
            check(r, "基准：默认 allow_upload=true", resp.body.contains("\"allow_upload\": true"));
            check(
                r,
                "基准：默认 allow_delete=false（安全默认值）",
                resp.body.contains("\"allow_delete\": false"),
            );
            check(r, "基准：列表里能看到 tmp.log", resp.body.contains("tmp.log"));
        }
        Err(e) => r.fail(format!("基准服务 HTTP 失败: {e}")),
    }
    // 基准：没开 allow-hash / allow-archive / cors 时应该是拒绝的
    // ★ 注意文件名要用 MARKER 常量 —— 之前写死 "/marker.txt" 而实际文件叫
    //   dufs-gui-opts-marker.txt，导致 404 被当成了「不是 403」的失败。
    match request(base_port, &format!("/{MARKER}?hash"), None) {
        Ok(resp) => check(r, "基准：未开 allow-hash 时 ?hash 被拒（403）", resp.status == 403),
        Err(e) => r.fail(format!("基准 ?hash 请求失败: {e}")),
    }
    match request(base_port, "/?zip", None) {
        Ok(resp) => check(r, "基准：未开 allow-archive 时 ?zip 不可用（404）", resp.status == 404),
        Err(e) => r.fail(format!("基准 ?zip 请求失败: {e}")),
    }
    match request(base_port, "/", None) {
        Ok(resp) => check(
            r,
            "基准：未开 CORS 时没有 access-control 响应头",
            !resp.headers.to_ascii_lowercase().contains("access-control"),
        ),
        Err(e) => r.fail(format!("基准 / 请求失败: {e}")),
    }
    drop(baseline_srv);

    // ---------- 逐个选项 ----------
    hidden(r);
    allow_hash(r, fx);
    allow_archive(r, fx);
    path_prefix(r, fx);
    render_modes(r, fx);
    multi_account_auth(r, fx);
    hashed_account_auth(r, fx);
    colon_password_auth(r, fx);
    put_create_vs_overwrite(r);
    log_file(r, fx);
    extra_args_cors(r, fx);
    bind_one_nic(r, fx);
    multi_bind(r, fx);
    config_file(r, fx);
    assets(r, fx);
    readonly_and_allow_all(r, fx);
    allow_all_scope(r, fx);
    readonly_with_auth(r, fx);
    https(r, fx);
    log_disabled(r, fx);
    serve_single_file(r, fx);
    env_override_stripped(r, fx);
}

/// `DUFS_*` 环境变量不能绕过界面设置（本轮实测发现的问题）
///
/// dufs 支持用环境变量配**所有**选项，而 clap 的优先级是「命令行 > 环境变量」。
/// 危险的是那些界面**没勾所以压根不传**的选项：以前本机环境里一个
/// `DUFS_ALLOW_DELETE=true` 就能让界面上的「未勾选删除」失效
/// （实测 `DELETE` 返回 204、文件真的没了）。
/// `dufs::start()` 现在会清掉子进程的 `DUFS_*`，这条用例把它钉住。
fn env_override_stripped(r: &mut Report, fx: &Fixture) {
    const VICTIM: &str = "dufs-gui-opts-env-victim.txt";
    let victim = fx.base.join(VICTIM);
    let _ = std::fs::write(&victim, b"please do not delete me\n");

    let port = net::free_port();
    let cfg = Config {
        serve_path: fx.base_str(),
        port,
        bind_ips: vec!["127.0.0.1".into()],
        // 界面这两项都是关的 ⇒ build_args 不会传对应的 --allow-*
        allow_delete: false,
        allow_upload: false,
        ..Config::default()
    };

    // 污染本进程环境，模拟「用户机器上恰好有一个 DUFS_* 变量」。
    //
    // SAFETY: `set_var` 在 2024 edition 里是 unsafe（可能与别的线程 getenv 竞争）。
    //   这里安全：自检是单线程顺序流程，除 dufs 输出排空线程外没有别的线程，
    //   而排空线程只读管道、不碰环境变量。变量在 start() 返回后立刻摘掉。
    unsafe { std::env::set_var("DUFS_ALLOW_DELETE", "true") };
    let started = Server::start(&cfg);
    unsafe { std::env::remove_var("DUFS_ALLOW_DELETE") };

    let srv = match started {
        Ok(s) => s,
        Err(e) => return r.fail(format!("环境变量隔离用例启动失败: {e}")),
    };

    match request_with(port, "DELETE", &format!("/{VICTIM}"), None, None) {
        Ok(resp) => {
            let status = resp.status;
            check(
                r,
                &format!(
                    "环境变量：DUFS_ALLOW_DELETE=true 但界面未勾删除 → DELETE 被拒（{status}）"
                ),
                status == 403,
            );
        }
        Err(e) => r.fail(format!("环境变量隔离用例 DELETE 失败: {e}")),
    }
    check(r, "环境变量：被保护的文件夹真还在", victim.is_file());

    drop(srv);
    let _ = std::fs::remove_file(&victim);
}

/// 共享路径是「单个文件」（dufs 的 serve-path 文件/目录通吃）
///
/// 要验证的不只是「能启动」，还有两个界面依赖的细节：
///   1. 根路径 `/` 直接返回文件内容（所以用户扫码打开就是那个文件）；
///   2. **写操作是「假成功」**：dufs 返回 200，但文件既没被改也没被删。
///      第 2 条正是「单文件模式下要把权限项置灰」的依据 ——
///      如果哪天 dufs 改成真的删除，这个断言会失败，提醒我们重新考虑置灰策略。
fn serve_single_file(r: &mut Report, fx: &Fixture) {
    let file = fx.base.join(MARKER);
    let body = "hello from options selftest\n";
    let port = net::free_port();
    let cfg = Config {
        serve_path: file.to_string_lossy().into_owned(),
        port,
        bind_ips: vec!["127.0.0.1".into()],
        // 故意权限全开：能开的都开，写操作照样不生效
        allow_all: true,
        ..Config::default()
    };
    let srv = match Server::start(&cfg) {
        Ok(s) => s,
        Err(e) => return r.fail(format!("单文件 serve-path 启动失败: {e}")),
    };

    match request(port, "/", None) {
        Ok(resp) => {
            check(r, "单文件：能启动，且 `/` 直接返回 200", resp.status == 200);
            check(r, "单文件：根路径返回的就是文件内容", resp.body == body);
            check(
                r,
                "单文件：响应头带 content-disposition（浏览器是下载/预览而不是目录页）",
                resp.headers.to_ascii_lowercase().contains("content-disposition"),
            );
        }
        Err(e) => r.fail(format!("单文件 GET / 失败: {e}")),
    }
    match request(port, &format!("/{MARKER}"), None) {
        Ok(resp) => check(
            r,
            "单文件：按文件名访问也能拿到",
            resp.status == 200 && resp.body == body,
        ),
        Err(e) => r.fail(format!("单文件按文件名 GET 失败: {e}")),
    }

    // ---- 写操作「假成功」----
    let before = std::fs::read(&file).unwrap_or_default();

    match request_with(port, "PUT", &format!("/{MARKER}"), Some(b"OVERWRITTEN"), None) {
        Ok(resp) => check(
            r,
            &format!("单文件：PUT 返回 {}（dufs 的假成功：200 但什么都没写）", resp.status),
            resp.status == 200,
        ),
        Err(e) => r.fail(format!("单文件 PUT 失败: {e}")),
    }
    let after_put = std::fs::read(&file).unwrap_or_default();
    check(r, "单文件：PUT 之后文件内容没变（所以「允许上传」无意义）", after_put == before);

    match request_with(port, "DELETE", &format!("/{MARKER}"), None, None) {
        Ok(resp) => check(
            r,
            &format!("单文件：DELETE 返回 {}（同样是假成功）", resp.status),
            resp.status == 200,
        ),
        Err(e) => r.fail(format!("单文件 DELETE 失败: {e}")),
    }
    check(
        r,
        "单文件：DELETE 之后文件还在（所以「允许删除」无意义，置灰不会藏掉真能力）",
        file.is_file(),
    );

    drop(srv);
}

/// `--hidden`
/// `--hidden`：**只影响列表显示，不影响直接访问**。
///
/// 这一组每条都是实测出来的（dufs 0.46 + glob crate 的默认选项）。其中几条
/// **跟直觉不符**，所以特别值得钉住：
///   1. 模式只和**条目名**比，**不含路径** —— 所以 `subx/deep.log` 这种写法
///      "永远匹配不上"，而且是**静默**的（实测：根列表和子列表都没变化）；
///   2. `*.log` 能隐藏**子目录里**的 `.log`（因为 `*` 能匹配任意名字）；
///   3. **大小写敏感** —— Windows 文件系统不敏感，所以这条反直觉：
///      `*.LOG` **藏不住** `a.log`；
///   4. 逗号后带空格 / 用全角逗号都会让规则**静默失效**，
///      所以 `dufs::build_args` 会先归一化（见 `norm_hidden`）。
fn hidden(r: &mut Report) {
    // 自己造一个布局已知的目录 —— fixture 的布局测不了"子目录里的同名文件"这些细节
    let dir = std::env::temp_dir().join("dufs-gui-opts-hidden");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::create_dir_all(dir.join("subx"));
    for (name, content) in [
        ("a.log", "a"),
        ("B.LOG", "B"),
        ("c1.txt", "c"),
    ] {
        let _ = std::fs::write(dir.join(name), content);
    }
    let _ = std::fs::write(dir.join("subx").join("deep.log"), "d");
    let _ = std::fs::write(dir.join("subx").join("keep.txt"), "k");

    // 起一次 dufs，取回三个观察点：根列表 / 子目录列表 / 直接访问被隐藏文件的响应码
    let probe = |hidden: &str| -> Option<(Vec<String>, Vec<String>, u16)> {
        let port = net::free_port();
        let cfg = Config {
            serve_path: dir.to_string_lossy().into_owned(),
            port,
            bind_ips: vec!["127.0.0.1".into()],
            hidden: hidden.to_string(),
            ..Config::default()
        };
        let srv = Server::start(&cfg).ok()?;
        let root = json_names(&request(port, "/?json", None).ok()?.body);
        let sub = json_names(&request(port, "/subx/?json", None).ok()?.body);
        let deep = request(port, "/subx/deep.log", None)
            .map(|r| r.status)
            .unwrap_or(0);
        drop(srv);
        Some((root, sub, deep))
    };

    let has = |list: &[String], n: &str| list.iter().any(|x| x == n);
    let Some((base_root, base_sub, _)) = probe("") else {
        return r.fail("--hidden 对照组启动失败");
    };
    check(
        r,
        "--hidden 对照（不传）：根列表和子列表都全",
        has(&base_root, "a.log") && has(&base_root, "subx") && has(&base_sub, "deep.log"),
    );

    // ---- 名字匹配 + 直接访问不受影响 ----
    if let Some((root, _sub, deep)) = probe("a.log") {
        check(
            r,
            "--hidden：命中的文件从列表里消失（a.log 没了，别的还在）",
            !has(&root, "a.log") && has(&root, "B.LOG") && has(&root, "c1.txt"),
        );
        check(
            r,
            "--hidden：★ 知道文件名仍然可以直接访问（只影响列表，不拦请求）",
            deep == 200,
        );
    } else {
        r.fail("--hidden a.log 启动失败");
    }

    // ---- 模式只和"名字"比 ----
    if let Some((root, sub, _)) = probe("*.log") {
        check(
            r,
            "--hidden：*.log 能藏掉子目录里的 deep.log（* 匹配任意名字）",
            !has(&root, "a.log") && !has(&sub, "deep.log") && has(&sub, "keep.txt"),
        );
    } else {
        r.fail("--hidden *.log 启动失败");
    }
    if let Some((root, sub, _)) = probe("subx/deep.log") {
        check(
            r,
            "--hidden：★ 带路径的写法（subx/deep.log）永远匹配不上 —— 模式只比名字，静默失效",
            has(&root, "a.log") && has(&sub, "deep.log"),
        );
    } else {
        r.fail("--hidden subx/deep.log 启动失败");
    }
    // ---- 隐藏目录：列表里没有，但仍能进去浏览 ----
    if let Some((root, sub, _)) = probe("subx") {
        check(
            r,
            "--hidden：目录被隐藏后不在根列表里，但 /subx/ 仍然能进、里面照常列出",
            !has(&root, "subx") && has(&sub, "deep.log") && has(&sub, "keep.txt"),
        );
    } else {
        r.fail("--hidden subx 启动失败");
    }
    // ---- 大小写敏感（Windows 上反直觉）----
    if let Some((root, _, _)) = probe("*.LOG") {
        check(
            r,
            "--hidden：★ 大小写敏感 —— *.LOG 藏不住 a.log（Windows 文件系统却是大小写不敏感的）",
            has(&root, "a.log") && !has(&root, "B.LOG"),
        );
    } else {
        r.fail("--hidden *.LOG 启动失败");
    }
    // ---- 通配符语法来自 glob crate ----
    if let Some((root, _, _)) = probe("c?.txt") {
        check(r, "--hidden：? 匹配单个字符（c?.txt 藏住 c1.txt）", !has(&root, "c1.txt"));
    } else {
        r.fail("--hidden c?.txt 启动失败");
    }
    if let Some((root, _, _)) = probe("c[0-9].txt") {
        check(
            r,
            "--hidden：字符类 [0-9] 也可用（glob 语法，不止 * 和 ?）",
            !has(&root, "c1.txt"),
        );
    } else {
        r.fail("--hidden c[0-9].txt 启动失败");
    }
    // ---- 归一化真的端到端生效（逗号后带空格不再静默失效）----
    if let Some((root, _, _)) = probe("a.log, B.LOG") {
        check(
            r,
            "--hidden：★ 逗号后带空格也照样生效（归一化后两条规则都命中）",
            !has(&root, "a.log") && !has(&root, "B.LOG") && has(&root, "c1.txt"),
        );
    } else {
        r.fail("--hidden 带空格启动失败");
    }
    if let Some((root, _, _)) = probe("a.log，B.LOG") {
        check(
            r,
            "--hidden：★ 全角逗号也认（中文输入法下很容易打出来）",
            !has(&root, "a.log") && !has(&root, "B.LOG"),
        );
    } else {
        r.fail("--hidden 全角逗号启动失败");
    }

    let _ = std::fs::remove_dir_all(&dir);
}

/// 把 `?json` 的响应解析成文件名列表（`paths[].name`）。
///
/// 用解析而不是 `body.contains("a.log")` 做断言：后者在测"**不**该出现"时
/// 会被别的字段误伤（响应里还有 `href` 之类），假阴性/假阳性都很难查。
fn json_names(body: &str) -> Vec<String> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
        return Vec::new();
    };
    v["paths"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|p| p["name"].as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default()
}

/// `--allow-hash`
fn allow_hash(r: &mut Report, fx: &Fixture) {
    let port = net::free_port();
    let cfg = Config {
        serve_path: fx.base_str(),
        port,
        bind_ips: vec!["127.0.0.1".into()],
        allow_hash: true,
        ..Config::default()
    };
    let srv = match Server::start(&cfg) {
        Ok(s) => s,
        Err(e) => return r.fail(format!("--allow-hash 启动失败: {e}")),
    };
    match request(port, &format!("/{MARKER}?hash"), None) {
        Ok(resp) => {
            let hash = resp.body.trim().to_string();
            check(r, "--allow-hash：?hash 返回 200", resp.status == 200);
            check(
                r,
                "--allow-hash：返回的是 64 位十六进制 sha256",
                hash.len() == 64 && hash.chars().all(|c| c.is_ascii_hexdigit()),
            );
            // 和 certutil 算出来的对一下，确认不是随便返回的字符串
            match certutil_sha256(&fx.base.join(MARKER)) {
                Some(expect) => check(
                    r,
                    "--allow-hash：sha256 与 certutil 计算结果一致",
                    expect.eq_ignore_ascii_case(&hash),
                ),
                None => r.skip("没找到 certutil，跳过 sha256 数值比对"),
            }
        }
        Err(e) => r.fail(format!("--allow-hash HTTP 失败: {e}")),
    }
    drop(srv);
}

/// `--allow-archive` + `--compress`
fn allow_archive(r: &mut Report, fx: &Fixture) {
    let port = net::free_port();
    let cfg = Config {
        serve_path: fx.base_str(),
        port,
        bind_ips: vec!["127.0.0.1".into()],
        allow_archive: true,
        compress: Compress::High,
        ..Config::default()
    };
    let srv = match Server::start(&cfg) {
        Ok(s) => s,
        Err(e) => return r.fail(format!("--allow-archive 启动失败: {e}")),
    };
    match request(port, "/?zip", None) {
        Ok(resp) => {
            check(r, "--allow-archive：?zip 返回 200", resp.status == 200);
            check(
                r,
                "--allow-archive：Content-Type 是 application/zip",
                resp.headers.to_ascii_lowercase().contains("application/zip"),
            );
            check(r, "--allow-archive：响应体是 ZIP（PK\\x03\\x04 头）", has_zip_header(&resp.raw_body));
        }
        Err(e) => r.fail(format!("--allow-archive HTTP 失败: {e}")),
    }
    drop(srv);
}

/// `--path-prefix`
/// `--path-prefix`
///
/// ★ 不只验证「dufs 服务在前缀上」，还要验证
///   「**界面会显示的那个地址**能打开」—— 这是两件事，
///   曾经参数传对了、dufs 行为也对，但给用户显示的地址没有前缀。
fn path_prefix(r: &mut Report, fx: &Fixture) {
    let port = net::free_port();
    let cfg = Config {
        serve_path: fx.base_str(),
        port,
        bind_ips: vec!["127.0.0.1".into()],
        path_prefix: "myfiles".into(),
        ..Config::default()
    };
    let srv = match Server::start(&cfg) {
        Ok(s) => s,
        Err(e) => return r.fail(format!("--path-prefix 启动失败: {e}")),
    };

    match request(port, "/myfiles/?json", None) {
        Ok(resp) => check(r, "--path-prefix：dufs 确实服务在 /myfiles/ 上", resp.status == 200),
        Err(e) => r.fail(format!("--path-prefix HTTP 失败: {e}")),
    }

    // ---------- 关键断言：界面会显示的那个地址，能不能直接打开 ----------
    let shown = net::access_url(Ipv4Addr::LOCALHOST.into(), port, false, &cfg.path_prefix);
    let shown_path = shown
        .strip_prefix(&format!("http://127.0.0.1:{port}"))
        .unwrap_or("");
    match request(port, shown_path, None) {
        Ok(resp) => check(
            r,
            &format!("--path-prefix：界面会显示 {shown}，直接访问它应当是 200"),
            resp.status == 200,
        ),
        Err(e) => r.fail(format!("--path-prefix 访问显示地址失败: {e}")),
    }

    // 对照：不带前缀的根路径打不开 —— 旧 bug 下二维码扫出来的就是这个结果
    match request(port, "/", None) {
        Ok(resp) => check(
            r,
            &format!(
                "--path-prefix：对照——根路径返回 {}（旧 bug 下扫码看到的就是这个）",
                resp.status
            ),
            resp.status == 400,
        ),
        Err(e) => r.fail(format!("--path-prefix 根路径请求失败: {e}")),
    }

    drop(srv);

    // ---------- 中文前缀：验证我们的百分号编码能与 dufs 的解码对上 ----------
    // 这一条很关键：编码规则不能只是「我以为」，必须拿真 dufs 碰一下。
    let port = net::free_port();
    let cfg = Config {
        serve_path: fx.base_str(),
        port,
        bind_ips: vec!["127.0.0.1".into()],
        path_prefix: "我的 文件".into(),
        ..Config::default()
    };
    let srv = match Server::start(&cfg) {
        Ok(s) => s,
        Err(e) => return r.fail(format!("中文前缀启动失败: {e}")),
    };
    let shown = net::access_url(Ipv4Addr::LOCALHOST.into(), port, false, &cfg.path_prefix);
    let shown_path = shown
        .strip_prefix(&format!("http://127.0.0.1:{port}"))
        .unwrap_or("");
    match request(port, shown_path, None) {
        Ok(resp) => check(
            r,
            &format!(
                "中文前缀：界面会显示 {shown}，dufs 认这个地址（状态 {}）",
                resp.status
            ),
            resp.status == 200,
        ),
        Err(e) => r.fail(format!("中文前缀访问失败: {e}")),
    }
    drop(srv);
}

/// `--render-index` / `--render-try-index` / `--render-spa`
fn render_modes(r: &mut Report, fx: &Fixture) {
    // render-index：有 index.html -> 200 渲染；没有 -> 404
    let port = net::free_port();
    let cfg = Config {
        serve_path: fx.with_index.to_string_lossy().into_owned(),
        port,
        bind_ips: vec!["127.0.0.1".into()],
        render_mode: RenderMode::Index,
        ..Config::default()
    };
    if let Ok(srv) = Server::start(&cfg) {
        match request(port, "/", None) {
            Ok(resp) => check(
                r,
                "--render-index：有 index.html 时渲染它",
                resp.status == 200 && resp.body.contains("INDEX-MARKER"),
            ),
            Err(e) => r.fail(format!("--render-index HTTP 失败: {e}")),
        }
        drop(srv);
    } else {
        r.fail("--render-index 启动失败");
    }

    let port = net::free_port();
    let cfg = Config {
        serve_path: fx.no_index.to_string_lossy().into_owned(),
        port,
        bind_ips: vec!["127.0.0.1".into()],
        render_mode: RenderMode::Index,
        ..Config::default()
    };
    if let Ok(srv) = Server::start(&cfg) {
        match request(port, "/", None) {
            Ok(resp) => check(r, "--render-index：没有 index.html 时返回 404", resp.status == 404),
            Err(e) => r.fail(format!("--render-index(404) HTTP 失败: {e}")),
        }
        drop(srv);
    } else {
        r.fail("--render-index(404) 启动失败");
    }

    // render-try-index：没有 index.html 时退回目录列表
    let port = net::free_port();
    let cfg = Config {
        serve_path: fx.no_index.to_string_lossy().into_owned(),
        port,
        bind_ips: vec!["127.0.0.1".into()],
        render_mode: RenderMode::TryIndex,
        ..Config::default()
    };
    if let Ok(srv) = Server::start(&cfg) {
        match request(port, "/", None) {
            Ok(resp) => {
                let ok = resp.status == 200 && resp.body.contains("other.txt");
                check(
                    r,
                    &format!(
                        "--render-try-index：没有 index.html 时退回目录列表（状态 {}）",
                        resp.status
                    ),
                    ok,
                );
                if !ok {
                    // 把真实响应落盘，免得只能猜「7273 字节到底是什么」
                    let dump = std::env::temp_dir().join("dufs-gui-opts-tryindex-body.html");
                    let _ = std::fs::write(&dump, &resp.raw_body);
                    r.info(format!(
                        "响应前 200 字节: {}",
                        resp.body.chars().take(200).collect::<String>().replace('\n', " ")
                    ));
                    r.info(format!("完整响应已存到: {}", dump.display()));
                }
            }
            Err(e) => r.fail(format!("--render-try-index HTTP 失败: {e}")),
        }
        drop(srv);
    } else {
        r.fail("--render-try-index 启动失败");
    }

    // render-spa：一层未知路径回退到 index.html
    // ★ 只测一层：实测 dufs 对 /a/b 这种两层以上未知路径返回 404（见界面上的说明）
    let port = net::free_port();
    let cfg = Config {
        serve_path: fx.with_index.to_string_lossy().into_owned(),
        port,
        bind_ips: vec!["127.0.0.1".into()],
        render_mode: RenderMode::Spa,
        ..Config::default()
    };
    if let Ok(srv) = Server::start(&cfg) {
        match request(port, "/dashboard", None) {
            Ok(resp) => check(
                r,
                "--render-spa：一层未知路径返回 index.html",
                resp.status == 200 && resp.body.contains("INDEX-MARKER"),
            ),
            Err(e) => r.fail(format!("--render-spa HTTP 失败: {e}")),
        }
        // 顺便把"深层路径返回 404"也固化下来，免得以后误以为是 bug
        match request(port, "/a/b", None) {
            Ok(resp) => check(
                r,
                "--render-spa：两层以上未知路径返回 404（dufs 的既定行为）",
                resp.status == 404,
            ),
            Err(e) => r.fail(format!("--render-spa 深层路径请求失败: {e}")),
        }
        drop(srv);
    } else {
        r.fail("--render-spa 启动失败");
    }
}

/// `-a` 认证规则（多账号 / 只读账号）
/// `--log-file` + `--log-format`
fn log_file(r: &mut Report, fx: &Fixture) {
    let port = net::free_port();
    let log_path = std::env::temp_dir().join("dufs-gui-opts-access.log");
    let _ = std::fs::remove_file(&log_path);

    let cfg = Config {
        serve_path: fx.base_str(),
        port,
        bind_ips: vec!["127.0.0.1".into()],
        log_file: log_path.to_string_lossy().into_owned(),
        log_format: "MYFMT $status $request".into(),
        ..Config::default()
    };
    let srv = match Server::start(&cfg) {
        Ok(s) => s,
        Err(e) => return r.fail(format!("--log-file 启动失败: {e}")),
    };

    let _ = request(port, &format!("/{MARKER}"), None);
    // 给日志落盘一点时间
    std::thread::sleep(Duration::from_millis(400));

    match std::fs::read_to_string(&log_path) {
        Ok(text) => {
            check(r, "--log-file：日志文件已生成", true);
            let want = format!("MYFMT 200 GET /{MARKER}");
            check(
                r,
                "--log-format：自定义格式生效",
                text.contains(&want),
            );
            if !text.contains("MYFMT") {
                r.info(format!("日志内容: {}", text.lines().next().unwrap_or("(空)")));
            }
        }
        Err(e) => r.fail(format!("--log-file：读不到日志文件 {e}")),
    }
    drop(srv);
}

/// 「额外命令行参数」透传（用 --enable-cors 验证真的生效）
fn extra_args_cors(r: &mut Report, fx: &Fixture) {
    let port = net::free_port();
    let cfg = Config {
        serve_path: fx.base_str(),
        port,
        bind_ips: vec!["127.0.0.1".into()],
        extra_args: "--enable-cors".into(),
        ..Config::default()
    };
    let srv = match Server::start(&cfg) {
        Ok(s) => s,
        Err(e) => return r.fail(format!("额外参数透传启动失败: {e}")),
    };
    match request(port, "/?json", None) {
        Ok(resp) => check(
            r,
            "额外参数透传：--enable-cors 真的生效了",
            resp.headers.to_ascii_lowercase().contains("access-control-allow-origin: *"),
        ),
        Err(e) => r.fail(format!("额外参数透传 HTTP 失败: {e}")),
    }
    drop(srv);
}

/// 多地址绑定（Stage C 新增）：勾两个网卡，两个都要能连，别的连不上。
///
/// 这条必须真的起一次 dufs：dufs 的 `-b` 是 append 型（`-b a -b b` 两个都绑），
/// 但"参数拼对了"和"两个真的都通"是两回事 —— 而且 dufs 只要有一个地址绑不上
/// 就整个服务起不来，所以必须实连验证。
fn multi_bind(r: &mut Report, fx: &Fixture) {
    let ips: Vec<Ipv4Addr> = net::candidates()
        .into_iter()
        .filter_map(|c| match c.ip {
            IpAddr::V4(v4) => Some(v4),
            _ => None,
        })
        .collect();
    if ips.len() < 2 {
        r.skip("多地址绑定实测（本机可用 IPv4 不足 2 个，跳过）");
        return;
    }
    let (a, b) = (ips[0], ips[1]);
    let port = net::free_port();
    let cfg = Config {
        serve_path: fx.base_str(),
        port,
        bind_ips: vec![a.to_string(), b.to_string()],
        ..Config::default()
    };
    match Server::start(&cfg) {
        Ok(srv) => {
            check(r, &format!("多地址绑定：第一个地址 {a} 能连"), connect_ok(a, port));
            check(r, &format!("多地址绑定：第二个地址 {b} 能连"), connect_ok(b, port));
            // 关键反证：只绑了这两个，就没绑 0.0.0.0 —— 回环地址应该连不上
            check(
                r,
                "多地址绑定：没在列表里的 127.0.0.1 连不上（证明不是偷偷绑了 0.0.0.0）",
                !connect_ok(Ipv4Addr::LOCALHOST, port),
            );
            drop(srv);
        }
        Err(e) => r.fail(format!("多地址绑定启动失败: {e}")),
    }
}

/// 绑定具体网卡（`-b <ip>`）：只有那个地址能连上
fn bind_one_nic(r: &mut Report, fx: &Fixture) {
    // 这个用例专门测 IPv4 单网卡绑定，所以挑第一个 IPv4 候选
    // （列表里现在也含 IPv6，见 net::candidates）
    let Some(cand) = net::candidates()
        .into_iter()
        .find(|c| matches!(c.ip, IpAddr::V4(_)))
    else {
        r.skip("没有探测到局域网 IPv4，跳过绑定网卡实测");
        return;
    };
    let IpAddr::V4(ip) = cand.ip else {
        r.skip("候选里没有 IPv4，跳过绑定网卡实测");
        return;
    };
    let port = net::free_port();

    let cfg = Config {
        serve_path: fx.base_str(),
        port,
        bind_ips: vec![ip.to_string()],
        ..Config::default()
    };
    let srv = match Server::start(&cfg) {
        Ok(s) => s,
        Err(e) => return r.fail(format!("绑定 {ip} 启动失败: {e}")),
    };

    check(r, &format!("绑定网卡：{ip}:{port} 可连接"), connect_ok(ip, port));
    check(
        r,
        &format!("绑定网卡：只监听 {ip}，127.0.0.1:{port} 连不上"),
        !connect_ok(Ipv4Addr::LOCALHOST, port),
    );
    drop(srv);

    // ---- IPv6：「所有网卡」时应该 IPv4 和 IPv6 都能连 ----
    //
    // 这条必须真的连一次。只断言「命令行里拼了 -b ::」不够，因为 dufs 是
    // **每个地址都得绑成功**才起得来（实测 `-b 10.99.99.99 -b 127.0.0.1` →
    // `Error: Failed to bind` 后进程退出），所以探测判断一旦写错方向，
    // 表现是「服务整个起不来」或者「IPv6 静默不可达」—— 两种都只有实连才能发现。
    if net::ipv6_available() {
        let port6 = net::free_port();
        let cfg6 = Config {
            serve_path: fx.base_str(),
            port: port6,
            bind_ips: Vec::new(),
            ipv6_available: true,
            ..Config::default()
        };
        match Server::start(&cfg6) {
            Ok(srv6) => {
                check(
                    r,
                    "所有网卡 + 有 IPv6：IPv4 依然能连",
                    connect_ok(Ipv4Addr::LOCALHOST, port6),
                );
                check(
                    r,
                    "所有网卡 + 有 IPv6：[::1] 也能连（以前这里是不通的）",
                    connect_ok_v6(port6),
                );
                drop(srv6);
            }
            Err(e) => r.fail(format!("所有网卡 + 有 IPv6：启动失败: {e}")),
        }
    } else {
        r.skip("IPv6 实测（本机 IPv6 不可用，跳过）");
    }
}

/// `-c` dufs 配置文件：配置生效，且命令行优先于配置文件
fn config_file(r: &mut Report, fx: &Fixture) {
    let port = net::free_port();
    let cfg_path = std::env::temp_dir().join("dufs-gui-opts-cfg-e2e.json");
    let body = format!(
        r#"{{"bind":"127.0.0.1","port":{port},"path-prefix":"cfgfromfile","allow-delete":true}}"#
    );
    if std::fs::write(&cfg_path, body).is_err() {
        return r.fail("写测试用配置文件失败");
    }

    // (a) 只用配置文件里的 path-prefix
    let cfg = Config {
        serve_path: fx.base_str(),
        port,
        bind_ips: vec!["127.0.0.1".into()],
        dufs_config: cfg_path.to_string_lossy().into_owned(),
        ..Config::default()
    };
    if let Ok(srv) = Server::start(&cfg) {
        match request(port, "/cfgfromfile/?json", None) {
            Ok(resp) => {
                check(r, "-c：配置文件里的 path-prefix 生效", resp.status == 200);
                check(
                    r,
                    "-c：配置文件里的 allow-delete 生效",
                    resp.body.contains("\"allow_delete\": true"),
                );
            }
            Err(e) => r.fail(format!("-c HTTP 失败: {e}")),
        }
        drop(srv);
    } else {
        r.fail("-c 启动失败");
    }

    // (b) 界面上填的 path-prefix 覆盖配置文件里的
    let port = net::free_port();
    let body = format!(r#"{{"bind":"127.0.0.1","port":{port},"path-prefix":"cfgfromfile"}}"#);
    let _ = std::fs::write(&cfg_path, body);
    let cfg = Config {
        serve_path: fx.base_str(),
        port,
        bind_ips: vec!["127.0.0.1".into()],
        path_prefix: "cliwins".into(),
        dufs_config: cfg_path.to_string_lossy().into_owned(),
        ..Config::default()
    };
    if let Ok(srv) = Server::start(&cfg) {
        match request(port, "/cliwins/?json", None) {
            Ok(resp) => check(r, "-c：界面上的 path-prefix 覆盖配置文件", resp.status == 200),
            Err(e) => r.fail(format!("-c 覆盖 HTTP 失败: {e}")),
        }
        match request(port, "/cfgfromfile/?json", None) {
            Ok(resp) => check(
                r,
                "-c：被覆盖的旧前缀不再可用",
                resp.status != 200,
            ),
            Err(e) => r.fail(format!("-c 覆盖(负例) HTTP 失败: {e}")),
        }
        drop(srv);
    } else {
        r.fail("-c 覆盖用例启动失败");
    }
}

/// `--assets`
fn assets(r: &mut Report, fx: &Fixture) {
    let port = net::free_port();
    let cfg = Config {
        serve_path: fx.base_str(),
        port,
        bind_ips: vec!["127.0.0.1".into()],
        assets: fx.assets.to_string_lossy().into_owned(),
        ..Config::default()
    };
    match Server::start(&cfg) {
        Ok(srv) => {
            match request(port, "/", None) {
                Ok(resp) => check(
                    r,
                    "--assets：带 index.html 的资源目录可以正常起服务",
                    resp.status == 200,
                ),
                Err(e) => r.fail(format!("--assets HTTP 失败: {e}")),
            }
            drop(srv);
        }
        Err(e) => r.fail(format!("--assets 启动失败: {e}")),
    }
}

/// 只读模式 / allow-all 对权限的实际影响
/// `--allow-all` 到底管哪几项（实测钉住，不只是看源码）
///
/// 源码（`args.rs`）里它只把 **6 个**布尔值一起置位：
/// upload / delete / search / symlink / hash / archive。
/// CORS、渲染模式、隐藏内容、认证都**不在**其中 —— 各自读自己的开关。
///
/// 另外钉一个反直觉的交互：认证规则里的 `:ro` **能压过** `-A`。
/// 对界面的意义：用户同时开了「放开全部操作」和「按目录只读」，
/// 不要以为是冲突配置 —— 规则就是要更严。
fn allow_all_scope(r: &mut Report, fx: &Fixture) {
    // ---------- ① -A 不包含 CORS ----------
    let port = net::free_port();
    let cfg = Config {
        serve_path: fx.base_str(),
        port,
        bind_ips: vec!["127.0.0.1".into()],
        allow_all: true,
        enable_cors: false, // 明确关着
        ..Config::default()
    };
    let srv = match Server::start(&cfg) {
        Ok(s) => s,
        Err(e) => return r.fail(format!("-A 范围用例启动失败: {e}")),
    };

    match request(port, "/", None) {
        Ok(resp) => {
            let has_cors = resp.headers.to_ascii_lowercase().contains("access-control");
            check(
                r,
                "--allow-all：不会顺手打开 CORS（CORS 是独立开关）",
                !has_cors,
            );
        }
        Err(e) => r.fail(format!("-A 范围用例 GET / 失败: {e}")),
    }
    match request(port, &format!("/{MARKER}?hash"), None) {
        Ok(resp) => check(
            r,
            "--allow-all：?hash 可用（说明 -A 包含 allow-hash）",
            resp.status == 200,
        ),
        Err(e) => r.fail(format!("-A 范围用例 ?hash 失败: {e}")),
    }
    match request(port, "/?zip", None) {
        Ok(resp) => check(
            r,
            "--allow-all：?zip 可用（说明 -A 包含 allow-archive）",
            resp.status == 200,
        ),
        Err(e) => r.fail(format!("-A 范围用例 ?zip 失败: {e}")),
    }
    drop(srv);

    // ---------- ② 认证规则的 :ro 压过 -A ----------
    let port2 = net::free_port();
    let cfg2 = Config {
        serve_path: fx.base_str(),
        port: port2,
        bind_ips: vec!["127.0.0.1".into()],
        allow_all: true,
        // 规则说这个账号只读 —— 它应当比 -A 更严
        use_auth: true,
        accounts: vec![acc("admin", "pw", "/", "ro")],
        ..Config::default()
    };
    let srv2 = match Server::start(&cfg2) {
        Ok(s) => s,
        Err(e) => return r.fail(format!("-A + 规则用例启动失败: {e}")),
    };

    match request(port2, "/?json", Some(("admin", "pw"))) {
        Ok(resp) => {
            check(
                r,
                "认证规则的 :ro 压过 -A：带凭据的 ?json 报 allow_delete=false",
                resp.body.contains("\"allow_delete\": false"),
            );
            check(
                r,
                "认证规则的 :ro 压过 -A：带凭据的 ?json 报 user=admin（身份确实生效了）",
                resp.body.contains("\"user\": \"admin\""),
            );
        }
        Err(e) => r.fail(format!("-A + 规则用例 ?json 失败: {e}")),
    }
    match request(port2, "/", None) {
        Ok(resp) => check(
            r,
            "配了认证规则时匿名被拒（401）—— -A 不会把匿名放进来",
            resp.status == 401,
        ),
        Err(e) => r.fail(format!("-A + 规则用例匿名校验失败: {e}")),
    }
    drop(srv2);
}

fn readonly_and_allow_all(r: &mut Report, fx: &Fixture) {
    // 只读：界面上「允许上传」是勾着的，但只读模式要把它压掉
    let port = net::free_port();
    let cfg = Config {
        serve_path: fx.base_str(),
        port,
        bind_ips: vec!["127.0.0.1".into()],
        allow_upload: true,
        readonly_mode: true,
        ..Config::default()
    };
    if let Ok(srv) = Server::start(&cfg) {
        match request(port, "/?json", None) {
            Ok(resp) => check(
                r,
                "只读模式：即使勾了「允许上传」也被压制",
                resp.body.contains("\"allow_upload\": false"),
            ),
            Err(e) => r.fail(format!("只读模式 HTTP 失败: {e}")),
        }
        drop(srv);
    } else {
        r.fail("只读模式启动失败");
    }

    // allow-all：界面上「允许删除」没勾，但 allow-all 要接管
    let port = net::free_port();
    let cfg = Config {
        serve_path: fx.base_str(),
        port,
        bind_ips: vec!["127.0.0.1".into()],
        allow_delete: false,
        allow_all: true,
        ..Config::default()
    };
    if let Ok(srv) = Server::start(&cfg) {
        match request(port, "/?json", None) {
            Ok(resp) => {
                check(
                    r,
                    "allow-all：即使没勾「允许删除」也变成 true",
                    resp.body.contains("\"allow_delete\": true"),
                );
                check(
                    r,
                    "allow-all：上传 / 搜索 / 打包一并打开",
                    resp.body.contains("\"allow_upload\": true")
                        && resp.body.contains("\"allow_search\": true")
                        && resp.body.contains("\"allow_archive\": true"),
                );
            }
            Err(e) => r.fail(format!("allow-all HTTP 失败: {e}")),
        }
        drop(srv);
    } else {
        r.fail("allow-all 启动失败");
    }
}

/// HTTPS：自签证书 + `--tls-cert/--tls-key`
fn https(r: &mut Report, fx: &Fixture) {
    let Some((cert, key)) = &fx.tls else {
        r.skip("HTTPS 实测跳过（缺 openssl）");
        return;
    };
    if !command_exists("curl") {
        r.skip("HTTPS 实测跳过（缺 curl，无法发 TLS 请求）");
        return;
    }

    let port = net::free_port();
    let cfg = Config {
        serve_path: fx.base_str(),
        port,
        bind_ips: vec!["127.0.0.1".into()],
        tls_cert: cert.to_string_lossy().into_owned(),
        tls_key: key.to_string_lossy().into_owned(),
        ..Config::default()
    };

    // dufs 起 HTTPS 后，127.0.0.1 的明文探测会失败，所以这里不能用 wait_ready 之外的判据。
    // wait_ready 内部是 TCP 连接（不做 TLS 握手），TLS 端口同样能连上，可以复用。
    let srv = match Server::start(&cfg) {
        Ok(s) => s,
        Err(e) => return r.fail(format!("HTTPS 启动失败: {e}")),
    };

    // ★ 用界面会显示的那个地址去请求：开了 TLS 就应该是 https，不能是 http
    let shown = net::access_url(Ipv4Addr::LOCALHOST.into(), port, true, &cfg.path_prefix);
    match curl_status(&format!("{shown}/?json")) {
        Ok(code) => check(
            r,
            &format!("HTTPS：界面会显示 {shown}，直接访问它应当能通（状态 {code}）"),
            code == 200,
        ),
        Err(e) => r.fail(format!("HTTPS 请求失败: {e}")),
    }
    drop(srv);

    // ---------- HTTPS + URL 前缀 组合（两个因素同时出错的那种） ----------
    let port = net::free_port();
    let cfg = Config {
        serve_path: fx.base_str(),
        port,
        bind_ips: vec!["127.0.0.1".into()],
        tls_cert: cert.to_string_lossy().into_owned(),
        tls_key: key.to_string_lossy().into_owned(),
        path_prefix: "myfiles".into(),
        ..Config::default()
    };
    let srv = match Server::start(&cfg) {
        Ok(s) => s,
        Err(e) => return r.fail(format!("HTTPS + 前缀 启动失败: {e}")),
    };
    let shown = net::access_url(Ipv4Addr::LOCALHOST.into(), port, true, &cfg.path_prefix);
    match curl_status(&format!("{shown}?json")) {
        Ok(code) => check(
            r,
            &format!("HTTPS + 前缀：界面会显示 {shown}，直接访问它应当能通（状态 {code}）"),
            code == 200,
        ),
        Err(e) => r.fail(format!("HTTPS + 前缀 请求失败: {e}")),
    }
    drop(srv);
}

/// 「只读模式 + 用户名密码」必须能起来。
///
/// ★ 这是本次修的 bug：那条路径曾经拼出 `-a user:pw@/:r`，而 dufs 只认
///   `:rw`/`:ro`，于是服务直接 `Error: Invalid auth value` 退出。
///   只验证参数拼装不够 —— 这里拿真 dufs 跟一下。
/// 可视化账号列表的真实行为：多账号 + 匿名，用的全是**列表**拼出来的 -a
///（不是原文框）。这是"简单框升级成多账号"的核心承诺：
/// 界面上配的多个账号，dufs 那边真的按各自的权限生效。
///
/// ★ 这里纠正过一个错误理解（实测 dufs 0.46 得出）：目录列表**不会**因为
///   "你没权限"就返回 401/403 —— 它永远返回 200，但**只列出你能看的条目**
///   （过滤显示）。所以"规则生没生效"要看**列表内容**，不是状态码。
///   直接访问单个文件时才是 401（没登录）/ 403（权限不够）/ 200。
fn multi_account_auth(r: &mut Report, _fx: &Fixture) {
    let dir = std::env::temp_dir().join("dufs-gui-opts-multi-auth");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::create_dir_all(dir.join("pub"));
    let _ = std::fs::create_dir_all(dir.join("open"));
    let _ = std::fs::write(dir.join("secret.txt"), b"s");
    let _ = std::fs::write(dir.join("pub").join("note.txt"), b"n");
    let _ = std::fs::write(dir.join("open").join("welcome.txt"), b"w");

    let port = net::free_port();
    let cfg = Config {
        serve_path: dir.to_string_lossy().into_owned(),
        port,
        bind_ips: vec!["127.0.0.1".into()],
        use_auth: true,
        accounts: vec![
            acc("admin", "pw", "/", "rw"),
            acc("guest", "gp", "/pub", "ro"),
            acc("", "", "/open", "ro"),
        ],
        ..Config::default()
    };
    let srv = match Server::start(&cfg) {
        Ok(s) => s,
        Err(e) => return r.fail(format!("多账号启动失败: {e}")),
    };

    let status = |path: &str, auth: Option<(&str, &str)>| {
        request(port, path, auth).map(|r| r.status).unwrap_or(0)
    };
    let names = |path: &str, auth: Option<(&str, &str)>| {
        request(port, path, auth)
            .map(|r| json_names(&r.body))
            .unwrap_or_default()
    };
    let has = |list: &[String], n: &str| list.iter().any(|x| x == n);
    let anon: Option<(&str, &str)> = None;

    // ---- 列表是过滤显示（永远 200），看内容才能证明规则生效 ----
    let anon_root = names("/?json", anon);
    check(
        r,
        "多账号：匿名看 / 只看到 open（过滤显示，不是 401）",
        status("/?json", anon) == 200 && anon_root == vec!["open".to_string()],
    );
    let guest_root = names("/?json", Some(("guest", "gp")));
    check(
        r,
        "多账号：guest 看 / 能看到 pub 和 open，但看不到 secret.txt",
        status("/?json", Some(("guest", "gp"))) == 200
            && has(&guest_root, "pub")
            && has(&guest_root, "open")
            && !has(&guest_root, "secret.txt"),
    );
    let admin_root = names("/?json", Some(("admin", "pw")));
    check(
        r,
        "多账号：admin 看 / 全都能看到",
        has(&admin_root, "secret.txt") && has(&admin_root, "pub"),
    );

    // ---- 直接访问文件：401 / 403 / 200 ----
    check(
        r,
        "多账号：匿名直接拿 secret.txt 返回 401（要先登录）",
        status("/secret.txt", anon) == 401,
    );
    check(
        r,
        "多账号：guest 直接拿 secret.txt 返回 403（规则没给 /）",
        status("/secret.txt", Some(("guest", "gp"))) == 403,
    );
    check(
        r,
        "多账号：guest 能拿 /open 里面的文件（匿名规则对所有登录用户也可见）",
        status("/open/welcome.txt", Some(("guest", "gp"))) == 200,
    );
    check(
        r,
        "多账号：guest 能进 /pub（规则内，只读）",
        status("/pub/note.txt", Some(("guest", "gp"))) == 200,
    );

    // ---- 写权限（allow_upload 默认开着，所以这里纯粹测"规则里的读写"）----
    // ★ 必须 PUT **新文件**：dufs 的 PUT 只能新建（201），覆盖已存在文件一律 403
    //   —— 那是 PUT 自身的语义（实测无认证时也一样），和账号权限无关。
    //   拿覆盖写去测"只读拦不拦"会蒙对（403 但原因错了）。
    let put_guest =
        request_with(port, "PUT", "/pub/guest-new.txt", Some(b"g"), Some(("guest", "gp")))
            .map(|r| r.status)
            .unwrap_or(0);
    check(
        r,
        "多账号：guest 新建文件被拒 403（它是只读，规则生效）",
        put_guest == 403,
    );
    let put_admin =
        request_with(port, "PUT", "/admin-new.txt", Some(b"a"), Some(("admin", "pw")))
            .map(|r| r.status)
            .unwrap_or(0);
    check(
        r,
        "多账号：admin 新建文件成功 201（它是读写，规则生效）",
        put_admin == 201,
    );

    drop(srv);
    let _ = std::fs::remove_dir_all(&dir);
}

/// 冒号密码的真实行为：`user:p:a` 这种密码，dufs 按第一个冒号切，
/// 用 `p:a` 能登录（前面起 dufs 裸测过，这里走一遍我们自己的拼装）。
fn colon_password_auth(r: &mut Report, fx: &Fixture) {
    let port = net::free_port();
    let cfg = Config {
        serve_path: fx.base_str(),
        port,
        bind_ips: vec!["127.0.0.1".into()],
        use_auth: true,
        accounts: vec![acc("user", "p:a", "/", "rw")],
        ..Config::default()
    };
    let srv = match Server::start(&cfg) {
        Ok(s) => s,
        Err(e) => return r.fail(format!("冒号密码启动失败: {e}")),
    };
    let ok = request(port, "/?json", Some(("user", "p:a")))
        .map(|r| r.status)
        .unwrap_or(0);
    check(r, "冒号密码：用 `p:a` 能登录（200）", ok == 200);
    let half = request(port, "/?json", Some(("user", "p")))
        .map(|r| r.status)
        .unwrap_or(0);
    check(r, "冒号密码：只用前半截 `p` 进不去（401）", half == 401);
    drop(srv);
}

/// `$6$` 哈希账号的真实行为（经由可视化列表，不是原文框）。
///
/// 哈希是原样透传的（拼装不断不改），这里验证"dufs 那边真的认"：
/// 用明文密码能登录（200），用错密码进不去（401）。
fn hashed_account_auth(r: &mut Report, fx: &Fixture) {
    // openssl passwd -6 -salt abcdefgh pw 的输出（固定盐，保证可重现）
    const HASH: &str =
        "$6$abcdefgh$KQeXafAQAaOoKTevphVU215RvJdgzyfASRasIOuh12hO8u0r1bGW92ZnTmC9IjsiQ8VPiTXBiZF49dFL1U4wX/";
    let port = net::free_port();
    let cfg = Config {
        serve_path: fx.base_str(),
        port,
        bind_ips: vec!["127.0.0.1".into()],
        use_auth: true,
        accounts: vec![acc("admin", HASH, "/", "rw")],
        ..Config::default()
    };
    let srv = match Server::start(&cfg) {
        Ok(s) => s,
        Err(e) => return r.fail(format!("哈希账号启动失败: {e}")),
    };
    let ok = request(port, "/?json", Some(("admin", "pw")))
        .map(|r| r.status)
        .unwrap_or(0);
    check(r, "$6$ 哈希账号：用明文密码能登录（200）", ok == 200);
    let bad = request(port, "/?json", Some(("admin", "wrong")))
        .map(|r| r.status)
        .unwrap_or(0);
    check(r, "$6$ 哈希账号：错密码进不去（401）", bad == 401);
    drop(srv);
}

/// dufs PUT 的语义：只能**新建**（201），覆盖已存在文件一律 403（内容不变）。
///
/// ★ 这跟认证**没关系** —— 无认证 + `--allow-upload` 照样如此（实测过）。
/// 之所以单独钉一条：拿"覆盖写"去测"有没有上传权限"会**蒙对**。
/// 403 的原因可能是"没权限"，也可能是"dufs 根本不让覆盖"，
/// 不分开测就分不清。（multi_account_auth 里 guest 那条就差点蒙对。）
/// （单文件模式是另一个极端：PUT 返回 200 但什么都不写 —— "假成功"，见 serve_single_file。）
fn put_create_vs_overwrite(r: &mut Report) {
    let dir = std::env::temp_dir().join("dufs-gui-opts-put-semantics");
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::create_dir_all(&dir);

    let port = net::free_port();
    let cfg = Config {
        serve_path: dir.to_string_lossy().into_owned(),
        port,
        bind_ips: vec!["127.0.0.1".into()],
        ..Config::default() // allow_upload = true
    };
    let srv = match Server::start(&cfg) {
        Ok(s) => s,
        Err(e) => return r.fail(format!("PUT 语义对照启动失败: {e}")),
    };
    let put_new = request_with(port, "PUT", "/put-new-file.txt", Some(b"new"), None)
        .map(|r| r.status)
        .unwrap_or(0);
    check(r, "PUT 新建文件成功 201（allow_upload 开着）", put_new == 201);
    let put_over = request_with(port, "PUT", "/put-new-file.txt", Some(b"over"), None)
        .map(|r| r.status)
        .unwrap_or(0);
    check(
        r,
        "PUT 覆盖已存在文件被拒 403（dufs 不让覆盖，跟认证无关）",
        put_over == 403,
    );
    let body = request(port, "/put-new-file.txt", None)
        .map(|r| r.body)
        .unwrap_or_default();
    check(r, "被拒后文件内容没变（还是第一次写进去的）", body == "new");
    drop(srv);
    let _ = std::fs::remove_dir_all(&dir);
}

fn readonly_with_auth(r: &mut Report, fx: &Fixture) {
    let port = net::free_port();
    let cfg = Config {
        serve_path: fx.base_str(),
        port,
        bind_ips: vec!["127.0.0.1".into()],
        use_auth: true,
        accounts: vec![acc("admin", "pw", "/", "ro")],
        allow_upload: false,
        allow_search: false,
        ..Config::default()
    };

    let srv = match Server::start(&cfg) {
        Ok(s) => s,
        Err(e) => {
            return r.fail(format!(
                "只读 + 用户名密码 启动失败（曾经拼出非法的 :r 就是这个后果）: {e}"
            ));
        }
    };
    r.ok("只读 + 用户名密码：dufs 能正常启动（拼出的是 :ro）");

    match request(port, "/?json", None) {
        Ok(resp) => check(r, "只读 + 密码：不带凭据返回 401", resp.status == 401),
        Err(e) => r.fail(format!("只读 + 密码 匿名请求失败: {e}")),
    }
    match request(port, "/?json", Some(("admin", "pw"))) {
        Ok(resp) => {
            check(r, "只读 + 密码：带凭据可访问", resp.status == 200);
            check(
                r,
                "只读 + 密码：没开写开关，dufs 报 allow_upload=false",
                resp.body.contains("\"allow_upload\": false"),
            );
        }
        Err(e) => r.fail(format!("只读 + 密码 带凭据请求失败: {e}")),
    }
    drop(srv);
}

/// 「关闭访问日志」要真的什么都不记（`--log-format ''`）
fn log_disabled(r: &mut Report, fx: &Fixture) {
    let port = net::free_port();
    let log_path = std::env::temp_dir().join("dufs-gui-opts-nolog.log");
    let _ = std::fs::remove_file(&log_path);

    let cfg = Config {
        serve_path: fx.base_str(),
        port,
        bind_ips: vec!["127.0.0.1".into()],
        log_file: log_path.to_string_lossy().into_owned(),
        log_disabled: true,
        ..Config::default()
    };
    let srv = match Server::start(&cfg) {
        Ok(s) => s,
        Err(e) => return r.fail(format!("关闭日志 启动失败: {e}")),
    };

    let _ = request(port, &format!("/{MARKER}"), None);
    std::thread::sleep(Duration::from_millis(400));

    match std::fs::read_to_string(&log_path) {
        Ok(text) => check(
            r,
            &format!("关闭访问日志：请求过之后日志仍是空的（{} 字节）", text.len()),
            text.trim().is_empty(),
        ),
        // dufs 也可能压根不建文件，同样算通过
        Err(_) => r.ok("关闭访问日志：dufs 没有建日志文件"),
    }
    drop(srv);
}

// ============================================================
// 辅助：启动 / 停止一个 dufs 实例
// ============================================================

struct Server {
    child: Child,
    job: Option<dufs::JobHandle>,
}

impl Server {
    fn start(cfg: &Config) -> Result<Self, String> {
        let exe = dufs::ensure_dufs()?;
        let (mut child, job, log) = dufs::start(cfg, &exe).map_err(|e| e.describe())?;
        dufs::wait_ready(&mut child, &cfg.bind_ips.join(","), cfg.port, &log, Duration::from_secs(10))?;
        Ok(Self { child, job: Some(job) })
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        // kill + wait，然后把 job 句柄也放掉，确保下一个用例不会撞端口。
        // stop() 内部已经 wait 过，端口随进程退出立即释放。
        dufs::stop(&mut self.child);
        self.job = None;
    }
}

// ============================================================
// 辅助：HTTP 请求（手写，零依赖）
// ============================================================

struct Resp {
    status: u16,
    headers: String,
    body: String,
    raw_body: Vec<u8>,
}

fn request(port: u16, path: &str, auth: Option<(&str, &str)>) -> Result<Resp, String> {
    request_with(port, "GET", path, None, auth)
}

/// 指定 HTTP 方法 / 请求体的版本。`body` 为 `None` 时不发 Content-Length。
///
/// 拆出这个方法是为了验证「单文件模式下 dufs 的写操作是假成功」（PUT/DELETE 返回 200
/// 但文件不变）—— 那是界面把权限项置灰的依据。
fn request_with(
    port: u16,
    method: &str,
    path: &str,
    body: Option<&[u8]>,
    auth: Option<(&str, &str)>,
) -> Result<Resp, String> {
    let addr = ("127.0.0.1", port);
    let mut stream =
        TcpStream::connect(addr).map_err(|e| format!("连接 127.0.0.1:{port} 失败: {e}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|e| e.to_string())?;

    // ★ User-Agent 必须像 curl。
    //   dufs 会嗅探 UA：认出 curl/wget 时返回适合命令行看的精简目录列表，
    //   其它 UA（含完全不发 UA）则返回那个 7KB 的浏览器 UI 页面（文件名
    //   由 JS 再拉 ?json 渲染）。不带 UA 时 "other.txt" 不在 HTML 里，
    //   目录列表相关的断言会莫名其妙地失败。
    let mut req = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nUser-Agent: curl/8.0.0\r\nConnection: close\r\n"
    );
    if let Some(b) = body {
        req.push_str(&format!("Content-Length: {}\r\n", b.len()));
    }
    if let Some((user, pass)) = auth {
        req.push_str(&format!(
            "Authorization: Basic {}\r\n",
            base64(format!("{user}:{pass}").as_bytes())
        ));
    }
    req.push_str("\r\n");
    stream.write_all(req.as_bytes()).map_err(|e| e.to_string())?;
    if let Some(b) = body {
        stream.write_all(b).map_err(|e| e.to_string())?;
    }

    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if buf.len() > 64 * 1024 {
                    break;
                }
            }
            Err(_) => break,
        }
    }

    // 拆出 header 和 body
    let split = buf
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|i| i + 4)
        .ok_or_else(|| "响应里找不到 header 结束标记".to_string())?;
    let head = String::from_utf8_lossy(&buf[..split]).into_owned();
    let raw_body = buf[split..].to_vec();

    let status = head
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse::<u16>().ok())
        .unwrap_or(0);

    Ok(Resp {
        status,
        headers: head,
        body: String::from_utf8_lossy(&raw_body).into_owned(),
        raw_body,
    })
}

/// ZIP 的本地文件头签名。
///
/// 不能直接看 body 开头：`?zip` 的响应是 `Transfer-Encoding: chunked`，
/// 手写 TCP 拿到的是「块长度\r\n数据」，第一个字节是长度而不是 "PK"。
/// 所以要在原始字节里找签名。
fn has_zip_header(raw: &[u8]) -> bool {
    raw.windows(4).any(|w| w == b"PK\x03\x04")
}

/// 极简 base64（只为 Basic 认证，避免为这一件事拉依赖）
fn base64(input: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in input.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

/// 某个地址:端口能不能连上（1 秒超时）
fn connect_ok(ip: Ipv4Addr, port: u16) -> bool {
    use std::net::SocketAddr;
    let addr = SocketAddr::from((ip, port));
    TcpStream::connect_timeout(&addr, Duration::from_secs(1)).is_ok()
}

/// 同上，但连 IPv6 回环（`[::1]`）。
fn connect_ok_v6(port: u16) -> bool {
    use std::net::SocketAddr;
    let addr = SocketAddr::from((Ipv6Addr::LOCALHOST, port));
    TcpStream::connect_timeout(&addr, Duration::from_secs(1)).is_ok()
}

fn check(r: &mut Report, name: &str, ok: bool) {
    if ok {
        r.ok(name);
    } else {
        r.fail(name);
    }
}

// ============================================================
// 辅助：外部命令
// ============================================================

fn command_exists(name: &str) -> bool {
    crate::firewall::console_command(name)
        .arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// 用 certutil 算文件 sha256（Windows 自带）
fn certutil_sha256(path: &Path) -> Option<String> {
    let out = crate::firewall::console_command("certutil")
        .args(["-hashfile"])
        .arg(path)
        .arg("SHA256")
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&out.stdout);
    text.lines()
        .map(|l| l.trim().replace(' ', ""))
        .find(|l| l.len() == 64 && l.chars().all(|c| c.is_ascii_hexdigit()))
}

/// 用 curl 发一个 HTTPS 请求，只要状态码（自签证书所以 -k）
fn curl_status(url: &str) -> Result<u16, String> {
    let out = crate::firewall::console_command("curl")
        .args(["-sk", "-o", "NUL", "-w", "%{http_code}", "--max-time", "8", url])
        .output()
        .map_err(|e| e.to_string())?;
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .parse::<u16>()
        .map_err(|_| format!("curl 没返回状态码（{}）", String::from_utf8_lossy(&out.stderr).trim()))
}

/// 生成自签证书；没有 openssl 就返回 None
fn make_self_signed(dir: &Path) -> Option<(PathBuf, PathBuf)> {
    if !command_exists("openssl") {
        return None;
    }
    let cert = dir.join("dufs-gui-opts-cert.pem");
    let key = dir.join("dufs-gui-opts-key.pem");

    let out = crate::firewall::console_command("openssl")
        .args([
            "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1",
            "-subj", "/CN=localhost",
            "-keyout",
        ])
        .arg(&key)
        .arg("-out")
        .arg(&cert)
        .output()
        .ok()?;

    if out.status.success() && cert.is_file() && key.is_file() {
        Some((cert, key))
    } else {
        None
    }
}
