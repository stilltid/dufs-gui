//! 极简文件日志。
//!
//! 为什么需要它：release 版是 `windows_subsystem = "windows"`，没有控制台，
//! eprintln! 的内容看不到。出问题时用户只能描述"按钮没反应"，
//! 有个日志文件就能直接定位。
//!
//! 日志位置：`%TEMP%\dufs-gui.log`
//!
//! ★ 它和 dufs 的**访问日志**是两回事：
//!   访问日志(`--log-file`)是 dufs 记的“谁访问了什么”，给对方看的；
//!   这份是本程序记的“自己干了什么”，纯排障用。
//!
//! 两档日志：
//!   `flog!`  → 关键事件（启动/停止/失败/命令行），**总是记**；
//!   `vlog!`  → 细节（地址、网卡、拖放注册、环境变量清理……），
//!              只在“详细日志”开着时记（开关在「应用」页）。

use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

static LOCK: Mutex<()> = Mutex::new(());

/// 是否记录 `vlog!`（详细日志）。启动时由配置初始化，用户拨开关时改。
static VERBOSE: AtomicBool = AtomicBool::new(true);

pub fn set_verbose(on: bool) {
    VERBOSE.store(on, Ordering::Relaxed);
}

pub fn is_verbose() -> bool {
    VERBOSE.load(Ordering::Relaxed)
}

pub fn log_path() -> PathBuf {
    std::env::temp_dir().join("dufs-gui.log")
}

/// 追加一行日志（带毫秒时间戳）。任何失败都静默忽略 —— 日志不该影响主流程。
pub fn log(msg: impl AsRef<str>) {
    let _guard = LOCK.lock();

    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);

    let line = format!("[{ms}] {}\n", msg.as_ref());

    // 同时往 stderr 打一份，debug 构建下直接可见
    eprint!("{line}");

    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path())
    {
        let _ = f.write_all(line.as_bytes());
    }
}

/// 清空日志（每次启动时调用，避免文件无限增长）
pub fn reset() {
    let _ = std::fs::write(log_path(), b"");
}

/// 便捷宏：log!("x = {}", y)
/// 关键事件用这个 —— 无论“详细日志”开不开都会记。
#[macro_export]
macro_rules! flog {
    ($($arg:tt)*) => {
        $crate::logfile::log(format!($($arg)*))
    };
}

/// 详细日志：只在“详细日志”开着时记。
/// 用法同 `flog!`。
#[macro_export]
macro_rules! vlog {
    ($($arg:tt)*) => {
        if $crate::logfile::is_verbose() {
            $crate::logfile::log(format!($($arg)*))
        }
    };
}
