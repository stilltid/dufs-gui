//! 图标工具（打包用，不属于程序运行时）
//!
//! 图标是**代码画**的（`src/icon.rs`），所以需要这么一个小工具把绘制结果导出成
//! `.ico`，再写进 exe。两件事都不该由主程序带在身上，因此单独一个二进制。
//!
//! 用法：
//! ```text
//! # 1) 生成 icon.ico（顺便导出三种构图的尺寸对比预览图，方便先看效果）
//! cargo run --release --bin icon_tool -- export /tmp/ico
//! cp /tmp/ico/icon.ico assets/icon.ico
//!
//! # 2) 构建主程序后，把图标写进它的 exe（★ 每次重新构建后都要做）
//! cargo build --release
//! cargo run --release --bin icon_tool -- embed target/release/dufs-gui.exe
//! ```
//!
//! `embed` 的第二个参数（ico 路径）可省略，默认用 `assets/icon.ico`。

use dufs_gui::icon;
use std::path::{Path, PathBuf};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let usage = "用法：\n  \
         icon_tool export <输出目录>              # 生成 icon.ico + 尺寸对比预览图\n  \
         icon_tool embed <目标 exe> [ico 路径]    # 把图标写进 exe（默认 assets/icon.ico）";

    let result = match args.first().map(|s| s.as_str()) {
        Some("export") => {
            let Some(dir) = args.get(1).map(PathBuf::from) else {
                eprintln!("{usage}");
                std::process::exit(2);
            };
            icon::export_previews(&dir).map(|files| {
                for f in files {
                    println!("{}", f.display());
                }
            })
        }
        Some("embed") => {
            let Some(exe) = args.get(1).map(PathBuf::from) else {
                eprintln!("{usage}");
                std::process::exit(2);
            };
            let ico = args
                .get(2)
                .map(PathBuf::from)
                .unwrap_or_else(|| Path::new("assets/icon.ico").to_path_buf());
            icon::embed_into_exe(&exe, &ico).map(|n| {
                println!("已把 {n} 个尺寸的图标写进 {}", exe.display());
            })
        }
        _ => {
            eprintln!("{usage}");
            std::process::exit(2);
        }
    };

    if let Err(e) = result {
        eprintln!("图标工具失败: {e}");
        std::process::exit(1);
    }
}
