//! 构建脚本：编译 Slint 界面 + 把 exe 图标嵌进去。
//!
//! ★ 为什么这里要自己写 `.res`：
//!   给 exe 嵌图标的标准做法是写个 `.rc` 再用 `rc.exe` 编译。但本机没装
//!   Windows SDK（也没有 `windres`，rustup 的 `llvm-tools` 里也不含 `llvm-rc`），
//!   所以改成：**自己生成 `.res` 二进制，再用链接器的 `/RES:` 选项塞进去**。
//!   `.res` 格式很简单（就是「资源头 + 原始数据」的平铺），而且 MSVC 的
//!   `link.exe` 原生支持 `/RES:`，于是 `cargo build --release` 就能直接出带图标的 exe，
//!   不需要额外的后处理步骤。
//!
//! `.ico` 由独立打包工具生成（`cargo run --bin icon_tool -- export <dir>`），
//! 绘制代码在 `src/icon.rs` —— 全项目只有那一处图标来源。

use std::path::Path;

fn main() {
    // 把 ui/app.slint 编译成 Rust 代码,之后在 main.rs 里用 slint::include_modules!() 引入。
    //
    // ★ 两个配置项都**必须**有，各自解决一个问题：
    //
    // 1. `with_bundled_translations("translations")`：把 translations/<语言>/
    //    LC_MESSAGES/<crate 名>.po 里的译文**编译进 exe**。
    //    单 exe、不依赖运行库是本项目的卖点，所以译文不能是启动时读盘的外部文件。
    //    目录布局是 Slint 定的（gettext 惯例），必须是
    //    `translations/en/LC_MESSAGES/dufs-gui.po`：
    //      · 语言目录名就是传给 select_bundled_translation() 的名字；
    //      · 文件名必须等于 CARGO_PKG_NAME（`dufs-gui`），否则找不到；
    //      · 中文不需要 .po —— 中文是 @tr() 里的**源串**（见 2）。
    //
    // 2. `with_default_translation_context(None)`：默认行为是把**所在组件名**当成
    //    msgctxt 写进查找键（i-slint-compiler/lib.rs 的 ComponentName）。
    //    那样 .po 里每条翻译都要写 `msgctxt "SharePage"` 之类，
    //    而同一个中文串在另一个组件里就得再写一条 —— 极易漏。
    //    用 None 后查找键里没有 context，.po 就是干净的 `msgid 中文 / msgstr 英文`。
    //    ★ 代价：同一个中文串只能有一种英文。目前所有重复串的英文都一样，
    //      将来真需要按上下文区分再改回 ComponentName 并给 .po 补 msgctxt。
    let config = slint_build::CompilerConfiguration::new()
        .with_bundled_translations("translations")
        .with_default_translation_context(slint_build::DefaultTranslationContext::None);
    slint_build::compile_with_config("ui/app.slint", config).expect("编译 ui/app.slint 失败");

    // ★ 界面已拆成多个文件(app.slint + theme + components/* + pages/*),
    //   所以要盯住整个 ui/ 目录。
    //   Cargo 对目录的 rerun-if-changed 是**递归**扫描的,一条就够。
    //   只盯着 app.slint 的话,改了分页文件不会触发重新编译 ——
    //   表现是「改了 .slint 但界面没变」,很难一眼看出是缓存问题。
    println!("cargo:rerun-if-changed=ui");
    println!("cargo:rerun-if-changed=ui/app.slint");
    // 译文改了也要重编（slint-build 自己也会打印加载过的 .po，这行是给
    // 「新增一个语言目录」这种情况兜底的）。
    println!("cargo:rerun-if-changed=translations");
    println!("cargo:rerun-if-changed=assets/dufs.exe");
    println!("cargo:rerun-if-changed=assets/icon.ico");
    println!("cargo:rerun-if-changed=assets/app.manifest");

    // 关窗确认框用的 TaskDialogIndirect 住在 comctl32 v6 里：
    // manifest 里声明依赖后加载器才会给 v6，否则进程启动即 0xc0000139。
    // （纯 Rust 实现，不需要 rc.exe/mt.exe。）
    if std::env::var("CARGO_CFG_WINDOWS").is_ok() {
        embed_manifest::embed_manifest_file("assets/app.manifest")
            .expect("嵌入 app.manifest 失败");
    }

    embed_icon_notice();
}

/// 图标不在这里嵌。
///
/// ★ 踩过的坑：本机没有 `rc.exe`/`windres`/`llvm-rc`，于是试过在 build.rs 里
/// 自己生成 `.res` 再给链接器加 `/RES:` 参数 —— **完全没生效，而且是静默的**。
/// 原因是 PATH 里的 `link.exe` 其实是 **Git 自带的 coreutils `link`**（造硬链接用的），
/// 不是 MSVC 链接器，它把所有不认识的参数一律忽略（连 `/NOSUCHLIB_XYZ.lib` 都不报错）。
///
/// 所以改成在**打包阶段**用 Windows 资源 API 原地写入：
/// ```text
/// cargo run --bin icon_tool -- export <dir>   # 用绘制代码生成 icon.ico + 预览图
/// cargo run --bin icon_tool -- embed <exe>    # 把 icon.ico 写进那个 exe
/// ```
/// 详见 `src/icon.rs::embed_into_exe`。
fn embed_icon_notice() {
    if !Path::new("assets/icon.ico").exists() {
        println!("cargo:warning=assets/icon.ico 不存在：构建能过，但 exe 会是系统默认图标");
    }
}
