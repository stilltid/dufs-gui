//! 自检模式（`--selftest` / `--selftest-orphan`）
//!
//! 目的：不用手点界面，就能验证整条最容易出错的链路 ——
//!   内嵌释放 -> 启动 -> 端口就绪 -> HTTP 真的能访问 -> 停止 -> 端口释放
//!
//! 结果同时写到 stderr 和 `%TEMP%\dufs-gui-selftest.txt`。
//! 之所以要写文件：release 版带 `windows_subsystem = "windows"`，控制台看不到。

use crate::config::Config;
use crate::dufs;
use crate::net;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::time::{Duration, Instant};

// 端口不写死：装过 WSL / Hyper-V 的机器上，Windows 会**动态**预留端口段，
// 写死的端口可能某次就报 os error 10013（实测踩过一次）。
// 交给系统分配，见 net::free_port()。

fn report_path() -> PathBuf {
    std::env::temp_dir().join("dufs-gui-selftest.txt")
}

/// 自检结果收集器（`--selftest` 与 `--selftest-options` 共用）
pub struct Report {
    path: PathBuf,
    title: String,
    pub lines: Vec<String>,
    failed: bool,
}

impl Report {
    pub fn new(path: PathBuf, title: impl Into<String>) -> Self {
        Self { path, title: title.into(), lines: Vec::new(), failed: false }
    }

    pub fn ok(&mut self, msg: impl AsRef<str>) {
        self.lines.push(format!("  [OK]   {}", msg.as_ref()));
    }

    pub fn fail(&mut self, msg: impl AsRef<str>) {
        self.failed = true;
        self.lines.push(format!("  [FAIL] {}", msg.as_ref()));
    }

    /// 跳过一项（环境不具备，比如没装 openssl）。不算失败。
    pub fn skip(&mut self, msg: impl AsRef<str>) {
        self.lines.push(format!("  [SKIP] {}", msg.as_ref()));
    }

    pub fn info(&mut self, msg: impl AsRef<str>) {
        self.lines.push(format!("         {}", msg.as_ref()));
    }

    pub fn section(&mut self, msg: impl AsRef<str>) {
        self.lines.push(format!("\n== {} ==", msg.as_ref()));
    }

    pub fn finish(self) -> i32 {
        let head = if self.failed {
            format!("\n{}：存在失败项\n", self.title)
        } else {
            format!("\n{}：全部通过\n", self.title)
        };
        let text = format!("{head}{}", self.lines.join("\n"));

        eprintln!("{text}");
        let _ = std::fs::write(&self.path, &text);
        eprintln!("\n完整报告: {}", self.path.display());

        if self.failed { 1 } else { 0 }
    }
}

/// 完整生命周期自检
pub fn run() -> i32 {
    let mut r = Report::new(report_path(), "dufs-gui 自检");
    r.lines.push(format!("dufs-gui 自检  (dufs {})", dufs::DUFS_VERSION));

    // ---------- 1. 准备共享目录 ----------
    r.section("准备共享目录");
    let share = std::env::temp_dir().join("dufs-gui-selftest-share");
    let prepared = std::fs::create_dir_all(&share).is_ok();
    if prepared {
        r.ok(format!("共享目录: {}", share.display()));
    } else {
        r.fail(format!("无法创建 {}", share.display()));
    }

    // 放一个特征文件，用来验证"目录列表确实服务的是这个目录"
    let marker = "dufs-gui-selftest-marker.txt";
    if std::fs::write(share.join(marker), b"hello from dufs-gui selftest\n").is_ok() {
        r.ok(format!("写入特征文件 {marker}"));
    } else {
        r.fail("写入特征文件失败");
    }

    // ---------- 2. 释放内嵌的 dufs ----------
    r.section("释放内嵌 dufs");
    let exe = match dufs::ensure_dufs() {
        Ok(p) => {
            let size = std::fs::metadata(&p).map(|m| m.len()).unwrap_or(0);
            if size > 1_000_000 {
                r.ok(format!("已释放: {} ({} 字节)", p.display(), size));
            } else {
                r.fail(format!("释放出的文件太小: {size} 字节"));
            }
            p
        }
        Err(e) => {
            r.fail(format!("释放失败: {e}"));
            return r.finish();
        }
    };

    // ---------- 3. 本机 IP 探测 ----------
    r.section("本机 IP 探测");
    // 先把所有网卡的原始名字打出来，方便诊断"为什么选了这个 IP"
    match local_ip_address::list_afinet_netifas() {
        Ok(list) => {
            for (name, ip) in &list {
                r.lines.push(format!("         网卡 {name:24} {ip}"));
            }
        }
        Err(e) => r.fail(format!("枚举网卡失败: {e}")),
    }

    let candidates = net::candidates();
    let ips: Vec<std::net::IpAddr> = candidates.iter().map(|c| c.ip).collect();
    if ips.is_empty() {
        r.fail("没有探测到任何可用的局域网 IPv4 地址（拔网线了？）");
    } else {
        r.ok(format!("优选结果（第一个就是会生成二维码的地址）: {}", ips[0]));
        for c in candidates.iter() {
            r.lines.push(format!(
                "         {}  score={:<5} {}{}",
                c.ip,
                c.score,
                c.name,
                if c.likely_virtual() { "  <- 虚拟网卡" } else { "" }
            ));
        }
    }

    // ---------- 4. 端口预检 ----------
    r.section("端口预检");
    let test_port = net::free_port();
    if net::port_in_use(test_port) {
        r.fail(format!("测试端口 {test_port} 已被占用，换一个再试"));
        return r.finish();
    }
    r.ok(format!("端口 {test_port} 空闲"));

    // ---------- 5. 生成参数并启动 ----------
    r.section("启动 dufs");
    let cfg = Config {
        serve_path: share.to_string_lossy().into_owned(),
        port: test_port,
        allow_upload: true,
        allow_search: true,
        allow_delete: false,
        ..Config::default()
    };

    let args = dufs::build_args(&cfg);
    r.ok(format!("命令行参数: {}", args.join(" ")));

    let (mut child, _job, log) = match dufs::start(&cfg, &exe) {
        Ok(v) => {
            r.ok(format!("进程已启动, pid={}", v.0.id()));
            v
        }
        Err(e) => {
            r.fail(format!("启动失败: {e}"));
            return r.finish();
        }
    };

    // ---------- 6. 等待就绪 ----------
    r.section("等待服务就绪");
    match dufs::wait_ready(&mut child, "", test_port, &log, Duration::from_secs(10)) {
        Ok(()) => r.ok("端口在超时内变为可连接"),
        Err(e) => {
            r.fail(format!("等待就绪失败: {e}"));
            dufs::stop(&mut child);
            return r.finish();
        }
    }

    // ---------- 7. HTTP 真实访问 ----------
    r.section("HTTP 访问验证");
    match http_get(test_port, "/") {
        Ok(body) => {
            if body.starts_with("HTTP/1.1 200") || body.starts_with("HTTP/1.0 200") {
                r.ok("首页返回 200");
            } else {
                r.fail(format!("首页状态码异常: {}", body.lines().next().unwrap_or("")));
            }
            r.lines.push(format!("         响应长度: {} 字节", body.len()));
            // 仅作参考：dufs 首页是动态渲染的，文件名不一定出现在 HTML 里，
            // 所以拿它当"信息"而不当"判定"。判定用下面的 ?json。
            r.lines.push(format!(
                "         [信息] 首页 HTML 里{}特征文件名",
                if body.contains(marker) { "包含" } else { "不包含" }
            ));
        }
        Err(e) => r.fail(format!("HTTP 请求失败: {e}")),
    }

    // dufs 的首页是动态渲染的，文件名不一定出现在首屏。
    // 用它的 JSON 接口 `?json` 来判断"服务的到底是哪个目录"才可靠。
    match http_get(test_port, "/?json") {
        Ok(body) => {
            if body.contains(marker) {
                r.ok(format!("?json 目录列表里出现了 {marker}（说明服务的是正确的目录）"));
            } else {
                r.fail("?json 里找不到特征文件，共享目录可能不对");
                r.lines.push(format!(
                    "         响应片段: {}",
                    body.chars().take(400).collect::<String>().replace('\n', " ")
                ));
            }
            // 顺便验证权限参数真的生效了
            if body.contains("\"allow_upload\": true") {
                r.ok("权限参数生效: allow_upload = true");
            } else {
                r.fail("--allow-upload 未生效");
            }
            if body.contains("\"allow_delete\": false") {
                r.ok("权限参数生效: allow_delete = false（默认安全）");
            } else {
                r.fail("allow_delete 应为 false");
            }
        }
        Err(e) => r.fail(format!("HTTP 请求失败: {e}")),
    }

    // ---------- 8. 停止并确认端口释放 ----------
    r.section("停止与清理");
    let t0 = Instant::now();
    dufs::stop(&mut child);
    r.ok(format!("已 kill + wait，耗时 {:?}", t0.elapsed()));

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut released = false;
    while Instant::now() < deadline {
        if !net::port_in_use(test_port) {
            released = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    if released {
        r.ok(format!("端口 {test_port} 已释放"));
    } else {
        r.fail(format!("端口 {test_port} 仍被占用，dufs 可能没被杀掉"));
    }

    // ---------- 9. 二维码可读性 ----------
    r.section("二维码生成");
    // ★ 用产品代码同一个函数拼地址，不要在这里另写一份 format!(),
    //   否则两边会慢慢跑偏（URL 前缀就是这麽漏掉的）。
    let qr_ip: std::net::IpAddr = ips
        .first()
        .copied()
        .unwrap_or(std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST));
    let url = net::access_url(qr_ip, test_port, false, "");
    match crate::qr::make_image(&url) {
        Some(_) => r.ok(format!("已为 {url} 生成二维码图片")),
        None => r.fail("二维码生成失败"),
    }
    // 额外导出一张 BMP，方便肉眼/扫码验证
    match write_qr_bmp(&url) {
        Ok(p) => r.ok(format!("二维码已导出为 BMP 供肉眼验证: {}", p.display())),
        Err(e) => r.fail(format!("导出 BMP 失败: {e}")),
    }

    r.finish()
}

/// 孤儿进程测试：启动 dufs 后直接 abort 本进程，**不做任何清理**。
///
/// 如果 Job Object 生效，dufs 会被内核连带杀死；如果没生效，
/// 就会在系统里留下一个占着端口的 dufs 进程。
/// 验证方法：跑完这个模式后执行 `tasklist | findstr dufs`，应该什么都没有。
pub fn run_orphan() -> ! {
    let share = std::env::temp_dir().join("dufs-gui-selftest-share");
    let _ = std::fs::create_dir_all(&share);

    let orphan_port = net::free_port();

    let cfg = Config {
        serve_path: share.to_string_lossy().into_owned(),
        port: orphan_port,
        ..Config::default()
    };

    let exe = dufs::ensure_dufs().expect("释放 dufs 失败");
    let (mut child, job, log) = dufs::start(&cfg, &exe).expect("启动 dufs 失败");
    dufs::wait_ready(&mut child, "", orphan_port, &log, Duration::from_secs(10))
        .expect("等待就绪失败");

    let pid = child.id();
    let text = format!(
        "dufs pid={pid} 已在端口 {orphan_port} 上运行。\n\
         现在强制 abort 本进程（模拟被任务管理器强杀），不做任何清理。\n\
         随后执行 `tasklist | findstr dufs` —— 应该什么都查不到，\n\
         那就说明 Job Object 生效了。\n"
    );
    eprintln!("{text}");
    let _ = std::fs::write(report_path(), text);

    // 保持 job 存活，然后硬崩
    std::mem::forget(job);
    std::process::abort();
}

/// 发一个最简 HTTP GET，返回响应全文（截断到 8KB）
fn http_get(port: u16, path: &str) -> Result<String, String> {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).map_err(|e| e.to_string())?;
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|e| e.to_string())?;

    let req = format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\n\r\n");
    stream.write_all(req.as_bytes()).map_err(|e| e.to_string())?;

    let mut buf = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&chunk[..n]);
                if buf.len() > 8192 {
                    break;
                }
            }
            Err(_) => break,
        }
    }
    Ok(String::from_utf8_lossy(&buf).into_owned())
}

/// 把二维码写成 24 位 BMP（零依赖，方便直接看图验证）
fn write_qr_bmp(url: &str) -> Result<PathBuf, String> {
    let code = qrcode::QrCode::new(url.as_bytes()).map_err(|e| e.to_string())?;
    let modules = code.width();
    let colors = code.to_colors();

    const SCALE: usize = 6;
    const QUIET: usize = 4;
    let side = (modules + QUIET * 2) * SCALE;

    // BMP 行宽必须是 4 的倍数
    let row_raw = side * 3;
    let padding = (4 - row_raw % 4) % 4;
    let row_size = row_raw + padding;
    let pixel_bytes = row_size * side;
    let file_size = 54 + pixel_bytes;

    let mut out = Vec::with_capacity(file_size);

    // BITMAPFILEHEADER
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&(file_size as u32).to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&54u32.to_le_bytes());

    // BITMAPINFOHEADER
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(side as i32).to_le_bytes());
    out.extend_from_slice(&(side as i32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&24u16.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&(pixel_bytes as u32).to_le_bytes());
    out.extend_from_slice(&2835i32.to_le_bytes());
    out.extend_from_slice(&2835i32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());

    // BMP 是自下而上存储的
    for y in (0..side).rev() {
        let my = y / SCALE;
        let in_y = my >= QUIET && my < QUIET + modules;
        for x in 0..side {
            let mx = x / SCALE;
            let in_x = mx >= QUIET && mx < QUIET + modules;
            let dark = in_x
                && in_y
                && colors[(my - QUIET) * modules + (mx - QUIET)] == qrcode::types::Color::Dark;
            let v: u8 = if dark { 0 } else { 255 };
            // BGR
            out.push(v);
            out.push(v);
            out.push(v);
        }
        // BMP 每行要补齐到 4 字节对齐
        out.resize(out.len() + padding, 0);
    }

    let path = std::env::temp_dir().join("dufs-gui-selftest-qr.bmp");
    std::fs::write(&path, out).map_err(|e| e.to_string())?;
    Ok(path)
}
