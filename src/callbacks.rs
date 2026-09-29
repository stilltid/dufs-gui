//! 界面回调接线：Slint `callback` → Rust 闭包的分发层。
//!
//! `main` 只管启动流程（参数分支 → 单实例 → 读配置 → 建窗口 → 进事件循环）；
//! 用户点了按钮之后发生什么，全在这里。重活一律转手 `service::`，
//! 这里只做“接线 + 切线程”（`spawn_*` 开头的就是起了后台线程的）。

use crate::i18n;
use crate::i18n::K;
use crate::service::App;
use crate::MainWindow;
use crate::{firewall, folder_picker, service, tray, ui};
#[cfg(windows)]
use crate::drop_target;
use slint::{CloseRequestResponse, ComponentHandle};
use std::path::PathBuf;
use std::sync::Arc;

pub(crate) fn wire_callbacks(ui: &MainWindow, app: &Arc<App>) {
    // ---------- 选择共享路径（文件夹 / 单个文件）----------
    // rfd 没有「文件或文件夹二选一」的对话框，所以界面上是两个按钮。
    // 两者共用一条逻辑（`spawn_pick_share_path`），区别只是调哪个 picker。
    // rfd 会跑一个模态对话框，丢到子线程避免阻塞界面。
    // ★ 拿到路径后必须用 ui::ui_set() 回 UI 线程写属性，
    //   在子线程里直接 ui.set_serve_path() 会"过一会儿才生效"。
    {
        let weak = ui.as_weak();
        let app = app.clone();
        ui.on_pick_share_folder(move || {
            spawn_pick_share_path(weak.clone(), app.clone(), true);
        });
    }
    {
        let weak = ui.as_weak();
        let app = app.clone();
        ui.on_pick_share_file(move || {
            spawn_pick_share_path(weak.clone(), app.clone(), false);
        });
    }

    // ---------- 窗口置顶 ----------
    // SetWindowPos 是一次系统调用，极快，直接在 UI 线程做，不用丢后台。
    // 成功才存盘；失败只提示，不改配置（下次启动不会记住一个没生效的值）。
    {
        let weak = ui.as_weak();
        let app = app.clone();
        ui.on_stay_on_top_changed(move |on| {
            match service::set_topmost(on) {
                Ok(()) => {
                    app.set_stay_on_top(on);
                    app.save();
                    vlog!("窗口置顶{}", if on { "开启" } else { "关闭" });
                }
                Err(e) => ui::set_hint(&weak, e.describe(), true),
            }
        });
    }

    // ---------- 界面语言 ----------
    // 切语言做两件事，都包在 `service::apply_language` 里：
    //   · Slint 侧（所有 @tr 串）—— `select_bundled_translation`。
    //     已创建的窗口会**当场重算全部文案**，所以不需要重启程序。
    //   · Rust 侧（i18n::t 串）—— 那些串已经变成 String 交给界面了，
    //     得由它手动重灌（状态栏 / 底部提示 / 防火墙规则 / 托盘菜单）。
    // ★ 结果**立即存盘**：用户下次打开就是选定的语言。
    {
        let weak = ui.as_weak();
        let app = app.clone();
        ui.on_language_changed(move |idx| {
            let lang = crate::config::Language::from_index(idx);
            let Some(ui) = weak.upgrade() else { return };
            app.set_language(lang);
            app.save();
            service::apply_language(&ui, lang);
        });
    }

    // ---------- 明暗主题 ----------
    // 配色由 `service::apply_theme` 写进内置控件的 `Palette`（一改到底）；
    // 回调同时负责**存盘**，保证下次打开还是这个选择。
    {
        let weak = ui.as_weak();
        let app = app.clone();
        ui.on_theme_changed(move |idx| {
            let mode = crate::config::ThemeMode::from_index(idx);
            let Some(ui) = weak.upgrade() else { return };
            app.set_theme_mode(mode);
            app.save();
            service::apply_theme(&ui, mode);
        });
    }

    // ---------- 手动输入 / 粘贴路径 ----------
    {
        let weak = ui.as_weak();
        let app = app.clone();
        ui.on_path_edited(move |text| {
            let text = text.to_string();
            app.set_serve_path(text.clone());
            // 异步校验路径是否存在（带序号防乱序覆盖）
            service::validate_serve_path(&weak, text);
        });
    }

    // ---------- 监听网卡 ----------
    {
        let weak = ui.as_weak();
        let app = app.clone();
        ui.on_bind_all_toggled(move |checked| {
            if let Some(ui) = weak.upgrade() {
                let _ = app.sync_cfg_from_ui(&ui);
            }
            service::change_bind_all(&app, &weak, checked);
        });
    }

    // ---------- 展开「指定具体网卡…」时先自动重测一遍 ----------
    //
    // 用户点开列表就是要挑网卡，这一瞬间看到的必须是最新列表 ——
    // 否则他刚插上的 USB 网卡不在里面，只会以为软件坏了
    // （他不会想到先点「重新检测网卡」）。
    // 手动的按钮保留着，留给「我就是想强制重查一次」。
    {
        let weak = ui.as_weak();
        let app = app.clone();
        ui.on_expand_bind(move || {
            service::refresh_ips(&app, &weak, "展开具体网卡列表");
        });
    }

    // ---------- 监听网卡（具体地址，多选）----------
    {
        let weak = ui.as_weak();
        let app = app.clone();
        ui.on_bind_toggled(move |index, checked| {
            // 先把界面上的端口 / URL 前缀 / HTTPS 同步进配置，
            // 否则下面刷新出来的地址会用旧值（现了改端口后换网卡，地址还是旧端口）。
            // 注意：运行时网卡选择是禁用的，所以这里不会把运行中的配置改乱。
            // 端口填错时这里只能忽略（刷新预览不是关键路径），真正启动时会报错。
            if let Some(ui) = weak.upgrade() {
                let _ = app.sync_cfg_from_ui(&ui);
            }
            service::change_bind_checked(&app, &weak, index, checked);
        });
    }

    // ---------- 账号列表 ----------
    //
    // 输入走回调直接进 Rust（`service::account_edited`）：界面上的 model 是单向的，
    // 每敲一个字都重建会把光标弄丢，所以平时根本不重建，只在增/删时重建。
    {
        let app = app.clone();
        ui.on_account_edited(move |index, field, value| {
            service::account_edited(&app, index, field, value);
        });
    }
    {
        let app = app.clone();
        ui.on_account_perm_changed(move |index, perm| {
            service::account_perm_changed(&app, index, perm);
        });
    }
    {
        let weak = ui.as_weak();
        let app = app.clone();
        ui.on_account_removed(move |index| {
            service::account_removed(&app, &weak, index);
        });
    }
    {
        let weak = ui.as_weak();
        let app = app.clone();
        ui.on_account_added(move || {
            service::account_added(&app, &weak);
        });
    }

    // ---------- 本程序的详细日志 ----------
    {
        let app = app.clone();
        ui.on_verbose_log_changed(move |on| {
            app.set_verbose_log(on);
            app.save();
            crate::logfile::set_verbose(on);
            flog!("详细日志已{}", if on { "开启" } else { "关闭" });
        });
    }

    // ---------- 本程序日志 / 配置文件的打开与清空 ----------
    {
        let weak = ui.as_weak();
        ui.on_open_log(move || service::open_app_log(&weak));
    }
    {
        let weak = ui.as_weak();
        ui.on_open_log_folder(move || service::open_log_folder(&weak));
    }
    {
        let weak = ui.as_weak();
        ui.on_clear_log(move || service::clear_app_log(&weak));
    }
    {
        let weak = ui.as_weak();
        ui.on_open_config(move || service::open_config_file(&weak));
    }

    // ---------- 恢复默认设置 ----------
    // 确认框是阻塞的，所以要丢后台线程（跟「清除程序数据」一个道理）。
    {
        let weak = ui.as_weak();
        let app = app.clone();
        ui.on_restore_defaults(move |app_baseline| {
            let weak = weak.clone();
            let app = app.clone();
            std::thread::spawn(move || service::restore_defaults(&app, &weak, app_baseline));
        });
    }

    // ---------- 运行中切换访问地址（仅显示）----------
    {
        let weak = ui.as_weak();
        let app = app.clone();
        ui.on_addr_changed(move |value| {
            service::change_addr(&app, &weak, value.as_str());
        });
    }

    // ---------- 刷新网卡列表 ----------
    // 网卡只在启动时枚举一次；插了网线 / 换了 WiFi / 连了 VPN 之后，
    // 用户需要有个地方让它重新扫一遍。
    {
        let weak = ui.as_weak();
        let app = app.clone();
        ui.on_refresh_nics(move || {
            service::refresh_ips(&app, &weak, "点了「重新检测网卡」");
        });
    }

    // ---------- 选证书 / 私钥文件 ----------
    {
        let weak = ui.as_weak();
        let app = app.clone();
        ui.on_pick_tls_file(move |kind| {
            let weak = weak.clone();
            let app = app.clone();
            let kind = kind.to_string();
            ui::set_busy(&weak, true);

            std::thread::spawn(move || {
                let (title, filter) = if kind == "cert" {
                    (
                        i18n::t(K::PickTlsCertTitle),
                        (i18n::t(K::PickTlsCertFilter), &["pem", "crt", "cer", "der"][..]),
                    )
                } else {
                    (
                        i18n::t(K::PickTlsKeyTitle),
                        (i18n::t(K::PickTlsKeyFilter), &["key", "pem"][..]),
                    )
                };

                let picked = folder_picker::pick_file(None, title, filter);

                match picked {
                    Some(path) => {
                        let text = path.to_string_lossy().into_owned();
                        let is_cert = kind == "cert";
                        app.set_text_field(if is_cert { "tls_cert" } else { "tls_key" }, text.clone());
                        app.save();
                        ui::ui_set(&weak, move |ui| {
                            if is_cert {
                                ui.set_tls_cert(text.into());
                            } else {
                                ui.set_tls_key(text.into());
                            }
                            ui.set_busy(false);
                        });
                    }
                    None => ui::set_busy(&weak, false),
                }
            });
        });
    }

    // ---------- 选 dufs 配置文件 / 自定义资源目录 ----------
    {
        let weak = ui.as_weak();
        let app = app.clone();
        ui.on_pick_config(move || {
            let weak = weak.clone();
            let app = app.clone();
            ui::set_busy(&weak, true);

            std::thread::spawn(move || {
                let start_dir = existing_dir(&app.cfg().dufs_config);
                let picked = folder_picker::pick_file(
                    start_dir,
                    i18n::t(K::PickDufsConfigTitle),
                    // 实测 0.46 只认 json / yaml；toml 会报 "invalid type: string"
                    (i18n::t(K::PickDufsConfigFilter), &["json", "yaml", "yml"][..]),
                );
                apply_picked_path(&weak, &app, "dufs_config", picked, |ui, v| {
                    ui.set_dufs_config(v)
                });
            });
        });
    }
    {
        let weak = ui.as_weak();
        let app = app.clone();
        ui.on_pick_assets(move || {
            let weak = weak.clone();
            let app = app.clone();
            ui::set_busy(&weak, true);

            std::thread::spawn(move || {
                let start_dir = existing_dir(&app.cfg().assets);
                let picked = folder_picker::pick_folder(
                    start_dir,
                    i18n::t(K::PickAssetsTitle),
                );
                apply_picked_path(&weak, &app, "assets", picked, |ui, v| ui.set_assets(v));
            });
        });
    }

    // ---------- 查看 dufs 的访问日志（进阶页的「查看」）----------
    // ★ 注意它和「应用 → 打开日志」不是同一个文件:
    //   这个是 **dufs** 记的访问日志(用户配的 --log-file),
    //   那个是本程序自己记的 %TEMP%\dufs-gui.log。
    {
        let weak = ui.as_weak();
        let app = app.clone();
        ui.on_open_access_log(move || {
            // 先把界面上刚填的路径同步进配置，否则点「查看」用的是旧值
            if let Some(ui) = weak.upgrade() {
                let _ = app.sync_cfg_from_ui(&ui);
            }
            service::open_access_log(&app, &weak);
        });
    }

    // ---------- 启动 / 停止 ----------
    {
        let weak = ui.as_weak();
        let app = app.clone();
        ui.on_start_stop(move || {
            if app.is_running() {
                service::stop_service(&app, &weak);
                return;
            } 
            // 回调在主线程执行，可以安全地读界面属性
            if let Some(ui) = weak.upgrade() {
                // 端口框里填错了就只说这一个问题，不要连掉地启动
                if let Err(e) = app.sync_cfg_from_ui(&ui) {
                    ui::set_hint(&weak, e, true);
                    return;
                }
            }
            app.save();
            service::start_service(app.clone(), weak.clone());
            
        });
    }

    // ---------- 放行防火墙（会弹 UAC）----------
    {
        let weak = ui.as_weak();
        let app = app.clone();
        ui.on_add_firewall(move || {
            let weak = weak.clone();
            // ★ 先用界面上的端口，不先同步的话会给"上一次保存的端口"加规则
            if let Some(ui) = weak.upgrade() {
                let _ = app.sync_cfg_from_ui(&ui);
            }
            let port = app.cfg().port;
            ui::set_hint(&weak, i18n::t(K::HintRequestingAdmin), false);
            std::thread::spawn(move || match firewall::request_rule(port) {
                Ok(()) => ui::set_hint(
                    &weak,
                    i18n::tn(K::HintFwRequested, &[&port]),
                    false,
                ),
                Err(e) => ui::set_hint(&weak, e.describe(), true),
            });
        });
    }

    // ---------- 复制地址 ----------
    {
        let weak = ui.as_weak();
        ui.on_copy_url(move || {
            let weak2 = weak.clone();
            let Some(ui) = weak.upgrade() else { return };
            let url = ui.get_url_text().to_string();
            if url.is_empty() {
                return;
            }
            std::thread::spawn(move || match firewall::copy_to_clipboard(&url) {
                Ok(()) => ui::set_hint(&weak2, i18n::t(K::HintUrlCopied), false),
                Err(e) => ui::set_hint(&weak2, e, true),
            });
        });
    }

    // ---------- 浏览器打开 ----------
    {
        let weak = ui.as_weak();
        ui.on_open_browser(move || {
            let Some(ui) = weak.upgrade() else { return };
            let url = ui.get_url_text().to_string();
            if !url.is_empty() {
                firewall::open_in_browser(&url);
            }
        });
    }

    // ---------- 打开共享位置 ----------
    // 目录直接用资源管理器打开；单文件则打开所在目录并选中它。
    {
        let weak = ui.as_weak();
        let app = app.clone();
        ui.on_open_share_location(move || {
            let path = app.cfg().serve_path.trim().to_string();
            if path.is_empty() {
                ui::set_hint(&weak, i18n::t(K::HintServePathEmpty), true);
                return;
            }
            if !std::path::Path::new(&path).exists() {
                ui::set_hint(&weak, i18n::tn(K::HintServePathMissing, &[&path]), true);
                return;
            }
            firewall::reveal_path(&path);
        });
    }

    // ---------- 复制带密码的链接 ----------
    // 手机浏览器打开 `http://user:pass@ip:port/` 就不用再手输密码。
    // 只在开了账号访问、且至少有一个填了用户名+密码的账号时才有意义。
    {
        let weak = ui.as_weak();
        let app = app.clone();
        ui.on_copy_auth_url(move || {
            let Some(ui) = weak.upgrade() else { return };
            let url = ui.get_url_text().to_string();
            if url.is_empty() {
                ui::set_hint(&weak, i18n::t(K::HintNoUrlYet), true);
                return;
            }
            let cfg = app.cfg();
            let Some(authed) = service::auth_url(&cfg, &url) else {
                ui::set_hint(
                    &weak,
                    i18n::t(K::HintNoAccount),
                    true,
                );
                return;
            };
            let who = cfg
                .accounts
                .iter()
                .find(|r| !r.name.trim().is_empty() && !r.secret.trim().is_empty())
                .map(|r| r.name.trim().to_string())
                .unwrap_or_default();
            let weak2 = weak.clone();
            std::thread::spawn(move || {
                match firewall::copy_to_clipboard(&authed) {
                    Ok(()) => ui::set_hint(
                        &weak2,
                        i18n::tn(K::HintAuthUrlCopied, &[&who]),
                        false,
                    ),
                    Err(e) => ui::set_hint(&weak2, e, true),
                }
            });
        });
    }

    // ---------- 防火墙规则：刷新 / 清理 ----------
    {
        let weak = ui.as_weak();
        ui.on_refresh_rules(move || {
            service::refresh_firewall_rules(&weak);
        });
    }
    {
        let weak = ui.as_weak();
        ui.on_cleanup_rules(move || {
            service::cleanup_firewall_rules(&weak);
        });
    }

    // ---------- 清除程序数据 / 卸载 ----------
    {
        let weak = ui.as_weak();
        let app = app.clone();
        ui.on_purge_data(move || {
            let weak = weak.clone();
            let app = app.clone();
            ui::set_busy(&weak, true);
            // 确认框是阻塞的，必须丢子线程（跟选文件夹一个道理）
            std::thread::spawn(move || {
                service::purge_data(&app, &weak);
                ui::set_busy(&weak, false);
            });
        });
    }

    // ---------- 关窗动作 / 是否询问 ----------
    // 下拉框 / 复选框拨动：记住，立即存盘，底部提示跟着变。
    {
        let weak = ui.as_weak();
        let app = app.clone();
        ui.on_close_action_changed(move |idx| {
            let action = crate::config::CloseAction::from_index(idx);
            let confirm = app.cfg().confirm_on_close;
            app.set_close_prefs(action, confirm);
            app.save();
            flog!("关窗默认动作已改为{}", if action == crate::config::CloseAction::Tray { "隐藏到托盘" } else { "直接退出" });
            refresh_close_hint(&app, &weak);
        });
    }
    {
        let weak = ui.as_weak();
        let app = app.clone();
        ui.on_confirm_on_close_changed(move |on| {
            let action = app.cfg().close_action;
            app.set_close_prefs(action, on);
            app.save();
            flog!("关闭时询问已{}", if on { "开启" } else { "关闭" });
            refresh_close_hint(&app, &weak);
        });
    }

    // ---------- 开机自启 / 启动后自动共享 ----------
    // ★ 先做动作、成功才改配置与界面 —— 免得界面显示"已开"但注册表里没写进去。
    {
        let weak = ui.as_weak();
        let app = app.clone();
        ui.on_autostart_changed(move |on, auto_serve, start_in_tray| {
            // 手动关掉"开机自启"时，另外两个单独留着仍然有意义
            // （"自动共享"对双击启动也生效；"静默"体现在注册表命令的 --tray 上），
            // 所以不联动。
            //
            // 安全阀：没开认证就自动共享 = 开机后局域网裸奔，先问一次。
            if auto_serve && !app.cfg().use_auth && !app.cfg().warned_autostart_no_auth {
                let weak2 = weak.clone();
                let app2 = app.clone();
                std::thread::spawn(move || {
                    let ok = folder_picker::confirm(
                        i18n::t(K::AutostartServeTitle),
                        i18n::t(K::AutostartServeWarn),
                    );
                    if !ok {
                        flog!("用户取消了「启动后自动共享」");
                        ui::ui_set(&weak2, move |ui| ui.set_autostart_serve(false));
                        app2.set_autostart(on, false, start_in_tray);
                        app2.save();
                        return;
                    }
                    app2.set_warned_autostart_no_auth();
                    app2.save();
                    // 用户确认了风险，按界面上的状态落地（注意用 on，不是读回配置）
                    apply_autostart(&weak2, &app2, on, true, start_in_tray);
                });
                return;
            }
            apply_autostart(&weak, &app, on, auto_serve, start_in_tray);
        });
    }

    // ---------- 「最近」下拉选了一条路径 ----------
    {
        let weak = ui.as_weak();
        let app = app.clone();
        ui.on_recent_path_picked(move |path| {
            let path = path.to_string();
            if path.trim().is_empty() {
                return;
            }
            if !std::path::Path::new(path.trim()).exists() {
                ui::set_hint(&weak, i18n::tn(K::HintPathGone, &[&path]), true);
                return;
            }
            apply_new_serve_path(&weak, &app, PathBuf::from(path));
            // 下拉归位（默认 -1 = 不显示选中项）
            let weak2 = weak.clone();
            ui::ui_set(&weak2, move |ui| ui.set_recent_index(-1));
        });
    }

    // ---------- 关闭窗口 ----------
    // 「询问」开着：点亮**应用内**确认弹层（不是原生 TaskDialog，理由见 ui/app.slint）；
    // 关了：按默认动作直接执行。
    //
    // ★ 这里绝不能跑任何模态/阻塞的东西 —— 本回调位于 winit 的 CloseRequested
    //   分发路径上，阻塞它会把整个前台输入一起卡住（原生 TaskDialog 就是这么翻车的）。
    {
        let weak = ui.as_weak();
        let app = app.clone();
        ui.window().on_close_requested(move || {
            let Some(ui) = weak.upgrade() else {
                return CloseRequestResponse::HideWindow;
            };
            // 直接读界面属性（回调就在 UI 线程上），
            // 不依赖“配置是否已经同步过”——用户可能刚拨完就点了 X。
            let action = crate::config::CloseAction::from_index(ui.get_close_action_index());
            let confirm = ui.get_confirm_on_close();
            // 只把界面上的当前选择同步进内存，**不存盘**：
            // 「关闭动作 / 是否询问」两个回调本身已经立即存过盘了，
            // 这里再存一次会让「点 X 又取消」也白写一遍配置文件。
            app.set_close_prefs(action, confirm);

            if confirm {
                let default_tray = action == crate::config::CloseAction::Tray;
                ui.set_close_confirm_default_tray(default_tray);
                // ★ 只在弹层**没开着**时重置勾选：用户可能双击了 X，
                //   第二下不能把他刚勾的「不再询问」清掉。
                if !ui.get_close_confirm_visible() {
                    ui.set_close_confirm_never_ask(false);
                }
                ui.set_close_confirm_visible(true);
                // 保留窗口（不关），等用户在弹层上点一个动作
                return CloseRequestResponse::KeepWindowShown;
            }

            apply_close_action(&app, &weak, action);
            CloseRequestResponse::HideWindow
        });
    }

    // ---------- 关窗确认弹层的结果 ----------
    // 0 = 取消，1 = 直接退出，2 = 隐藏到托盘（与 ui/app.slint 的 close-confirm-respond 对齐）
    {
        let weak = ui.as_weak();
        let app = app.clone();
        ui.on_close_confirm_respond(move |choice| {
            let Some(ui) = weak.upgrade() else { return };
            let never = ui.get_close_confirm_never_ask();
            // 先把弹层收起来（changed close-confirm-visible → PopupWindow.close）
            ui.set_close_confirm_visible(false);

            // 取消（0）本身不执行动作；但勾了「不再询问」时要按当前默认动作把询问关掉。
            let resolved = match choice {
                1 => crate::config::CloseAction::Quit,
                2 => crate::config::CloseAction::Tray,
                _ if never => crate::config::CloseAction::from_index(ui.get_close_action_index()),
                _ => return,
            };

            if never {
                // 「不再询问」：关掉询问开关，选了啥啥 / 当前默认动作就是新默认
                app.set_close_prefs(resolved, false);
                app.save();
                ui.set_close_action_index(resolved.to_index());
                ui.set_confirm_on_close(false);
                refresh_close_hint(&app, &weak);
                flog!(
                    "关窗询问已关闭，以后按{}直接执行",
                    if resolved == crate::config::CloseAction::Tray {
                        "隐藏到托盘"
                    } else {
                        "直接退出"
                    }
                );
            }

            // 取消只是（可选地）关掉询问，不执行关窗动作
            if choice == 0 {
                return;
            }
            apply_close_action(&app, &weak, resolved);
        });
    }
}

/// 执行一次「关窗动作」：退出程序 或 隐藏到托盘。
///
/// 窗口关闭回调与「关窗确认」弹层回调**共用这一份**，免得两处逻辑漂移。
fn apply_close_action(
    app: &Arc<App>,
    weak: &slint::Weak<MainWindow>,
    action: crate::config::CloseAction,
) {
    if action == crate::config::CloseAction::Tray {
        flog!("窗口关闭请求 -> 隐藏到托盘（服务继续运行）");
        if let Some(ui) = weak.upgrade() {
            let _ = ui.window().hide();
        }
        return;
    }
    flog!("窗口关闭请求 -> 停止服务并退出");
    service::quit(app, weak);
}

/// 底部提示跟着关窗设置走，别让用户猜 X 会干什么。
fn refresh_close_hint(app: &Arc<App>, weak: &slint::Weak<MainWindow>) {
    let cfg = app.cfg();
    let text = crate::close_hint(cfg.close_action, cfg.confirm_on_close);
    ui::ui_set(weak, move |ui| ui.set_hint_text(text.into()));
}

/// 写/删注册表自启项 + 存盘 + 刷界面。
///
/// `start_in_tray` 只影响写进注册表的那条命令（多一个 `--tray`）：
/// 登录那次静默进托盘，手动双击永远显示窗口。
fn apply_autostart(
    weak: &slint::Weak<MainWindow>,
    app: &Arc<App>,
    on: bool,
    auto_serve: bool,
    start_in_tray: bool,
) {
    if let Err(e) = crate::autostart::set_enabled(on, start_in_tray) {
        // 失败要把界面上的勾退回去，否则显示与实际不符
        let weak2 = weak.clone();
        let cfg = app.cfg();
        ui::set_hint(weak, e, true);
        ui::ui_set(&weak2, move |ui| {
            ui.set_autostart(cfg.autostart);
            ui.set_autostart_serve(cfg.autostart_serve);
            ui.set_start_in_tray(cfg.start_in_tray);
        });
        return;
    }
    app.set_autostart(on, auto_serve, start_in_tray);
    app.save();
    flog!(
        "开机自启已{}；启动后自动共享已{}；静默进托盘已{}",
        if on { "开启" } else { "关闭" },
        if auto_serve { "开启" } else { "关闭" },
        if start_in_tray { "开启" } else { "关闭" }
    );
    let msg = match (on, auto_serve, start_in_tray) {
        (true, true, true) => i18n::t(K::AutostartResultTrayServe),
        (true, true, false) => i18n::t(K::AutostartResultServe),
        (true, false, true) => i18n::t(K::AutostartResultTrayOnly),
        (true, false, false) => i18n::t(K::AutostartResultRunOnly),
        (false, true, _) => i18n::t(K::AutostartResultServeOnly),
        (false, false, _) => i18n::t(K::AutostartResultOff),
    };
    ui::set_hint(weak, msg, false);
}

/// 启动时：如果配了"自动开始共享"就自动起服务。
///
/// ★ 没开账号认证时先弹一次确认（用户可能很久以前勾的，当时没在意）。
///   确认框放在子线程里 —— 它会阻塞，绝不能在事件循环之前的主线程里弹。
pub(crate) fn maybe_autostart_serve(ui: &MainWindow, app: &Arc<App>) {
    let cfg = app.cfg();
    if !cfg.autostart_serve {
        return;
    }
    if cfg.serve_path.trim().is_empty() {
        flog!("配置了自动共享但没选共享路径，跳过");
        return;
    }
    let need_warn = !cfg.use_auth && !cfg.warned_autostart_no_auth;
    let weak = ui.as_weak();
    let app = app.clone();
    std::thread::spawn(move || {
        // 等主窗口真的出来再弹框（否则对话框可能出现在窗口后面）
        std::thread::sleep(std::time::Duration::from_millis(400));
        if need_warn {
            let ok = folder_picker::confirm(
                i18n::t(K::AutostartServeTitle),
                i18n::t(K::AutostartServeWarnBoot),
            );
            app.set_warned_autostart_no_auth();
            app.save();
            if !ok {
                flog!("用户拒绝了启动时自动共享");
                return;
            }
        }
        flog!("按配置自动开始共享");
        service::start_service(app.clone(), weak);
    });
}

/// hide() 之后 show()，强制整窗重绘一次。
///
/// ★ 为什么非要这么拐：Slint 的 software 渲染器是“脏区部分重绘”，靠 softbuffer
///   的 `present_with_damage` 只贴变化的那几块。而**Windows 上 winit 从不发
///   Occluded 事件**（只有 X11 发），渲染器就不知道窗口藏起来时缓冲已经作废；
///   于是 show 之后只重绘“变脏”的几块，其余保持空白 —— 表现就是全白，
///   鼠标划过哪块哪块才出现（划过恰好把那一块弄脏了）。
///
///   渲染器只在 softbuffer 的缓冲被重建（`age()==0`）时才切到 NewBuffer 全量重绘，
///   而 softbuffer 的 `resize` 对**相同尺寸**是直接 return 的。
///   所以这里把窗口尺寸 +1px 再还原：两次都会重建缓冲，各带出一次全量重绘。
///   （手动拖一下窗口大小能“治好”白屏，就是这个道理。）
fn force_full_repaint(ui: slint::Weak<MainWindow>) {
    let Some(w) = ui.upgrade() else { return };
    let window = w.window();
    // 最小化/最大化时改尺寸会被系统忽略或打乱窗口状态，交给系统自己的重绘
    if window.is_minimized() || window.is_maximized() {
        window.request_redraw();
        return;
    }
    let size = window.size();
    let (width, height) = (size.width, size.height);
    if width == 0 || height == 0 {
        return;
    }

    window.set_size(slint::PhysicalSize::new(width + 1, height));
    slint::Timer::single_shot(std::time::Duration::from_millis(50), move || {
        if let Some(w) = ui.upgrade() {
            w.window().set_size(slint::PhysicalSize::new(width, height));
        }
    });
}

/// 取某个文件路径所在目录，用来当对话框的初始目录
fn existing_dir(path: &str) -> Option<PathBuf> {
    let p = PathBuf::from(path.trim());
    if p.is_dir() {
        Some(p)
    } else {
        p.parent().filter(|d| d.is_dir()).map(|d| d.to_path_buf())
    }
}

/// 弹「选择共享路径」对话框并落盘。`want_dir` 为 true 选文件夹，false 选单个文件。
///
/// 单独抽出来是因为两个按钮（文件夹 / 文件）走的是同一套流程：
/// 暂停界面 -> 子线程弹框 -> 写配置 -> 回 UI 线程刷新。
fn spawn_pick_share_path(weak: slint::Weak<MainWindow>, app: Arc<App>, want_dir: bool) {
    // 先给个即时反馈：对话框弹出前按钮就变成"处理中…"
    ui::set_busy(&weak, true);

    std::thread::spawn(move || {
        // 初始目录：当前路径是文件夹就用它，是文件就退到它所在的目录
        let start_dir = existing_dir(&app.cfg().serve_path);

        let picked = if want_dir {
            folder_picker::pick_folder(start_dir, i18n::t(K::PickServeDirTitle))
        } else {
            // 不设扩展名过滤：任何文件都能共享，包括没有扩展名的（Makefile、LICENSE）
            folder_picker::pick_file(start_dir, i18n::t(K::PickServeFileTitle), ("", &[][..]))
        };

        match picked {
            Some(path) => apply_new_serve_path(&weak, &app, path),
            // 用户取消：只把 busy 关掉，不要动其它状态
            None => ui::set_busy(&weak, false),
        }
    });
}

/// 新路径落盘 + 刷界面的统一收尾。
///
/// 「选择文件夹… / 选择文件…」和「拖进来」共用这一套，
/// 保证两种入口的行为一字不差：写配置 → 存盘 → 回 UI 线程刷新
/// （路径、有效性、是否文件）→ 清掉提示。
/// 调用方保证 path 是文件或文件夹（picker 选出来的天然是，
/// 拖进来的由 `config::decide_dropped_path` 先过一遍）。
fn apply_new_serve_path(weak: &slint::Weak<MainWindow>, app: &Arc<App>, path: PathBuf) {
    let text = path.to_string_lossy().into_owned();
    // `set_serve_path` 会顺手把这条路径插到「最近使用」的最前面
    app.set_serve_path(text.clone());
    app.save();

    // 形态按实际路径判断，不按入口猜（用户可能手选/拖了怪东西）
    let is_file =
        crate::config::serve_path_kind(&text) == crate::config::ServePathKind::File;
    // 选择 / 拖进来的天然是绝对路径，但**可能是符号链接或 junction** —— dufs 的根会是
    // 链接的目标，所以这一行照样要算（和手输走同一套判断，见 config::serve_path_hint）。
    let real_hint = crate::config::serve_path_hint(&text).unwrap_or_default();
    let recent: Vec<slint::SharedString> = app
        .cfg()
        .recent_paths
        .iter()
        .map(|p| p.as_str().into())
        .collect();

    let text_for_ui: String = text.clone();
    ui::ui_set(weak, move |ui| {
        ui.set_serve_path(text_for_ui.into());
        ui.set_path_valid(true);
        ui.set_path_is_file(is_file);
        ui.set_path_real_hint(real_hint.into());
        ui.set_recent_paths(slint::ModelRc::new(slint::VecModel::from(recent)));
        ui.set_busy(false);
    });
    ui::set_hint(weak, "", false);
}

/// 注册原生文件拖放。失败只记日志，不打断启动。
///
/// ★ `show()` 刚返回时原生窗口可能还没建好（winit 把真正的创建推迟到
///   事件循环转起来之后），这时枚举是空的。
///   所以先在后台线程用 `window_finder::wait_main_hwnd` 等 HWND，
///   等到了再切回 UI 线程注册 —— 注册和窗口同线程，最保险；
///   轮询不占 UI 线程，不影响界面秒开。
///
/// ★ 找窗口按 **PID**（`window_finder`），**不按标题** —— 标题是要翻译的
///   界面文案，按标题找的话英文界面下拖放会静默失效。
#[cfg(windows)]
pub(crate) fn install_drop_target(ui: &MainWindow, app: &Arc<App>) {
    let weak_hover = ui.as_weak();
    let weak_drop = ui.as_weak();
    let app_drop = app.clone();

    // ★ 这两个闭包可能在任意线程被调（实践中是 UI 线程），
    //   里面只许用线程安全的操作：ui_set / set_hint / Mutex。
    //   绝不能直接 ui.set_xxx()。
    let on_hover = move |hover: bool| {
        let weak = weak_hover.clone();
        ui::ui_set(&weak, move |ui| ui.set_drag_over(hover));
    };
    let on_paths = move |paths: Vec<PathBuf>| {
        let weak = weak_drop.clone();
        let app = app_drop.clone();
        // ★ 必须丢到后台线程：这个闭包是在 OLE 的 `Drop` 回调里同步调用的，
        //   而下面要做「路径是否存在」的文件系统查询和写 config.json ——
        //   在 OLE 回调里做磁盘 I/O 会让资源管理器等在那里（看起来像卡住）。
        //   丢出去后 OLE 能立即返回，松手体验干脆。
        std::thread::spawn(move || match crate::config::decide_dropped_path(paths) {
            Ok(path) => apply_new_serve_path(&weak, &app, path),
            Err(e) => ui::set_hint(&weak, e, true),
        });
    };
    // 启动时恢复置顶，和拖拽共用这次「HWND 就绪」时机，免得再开一个轮询线程。
    let app_top = app.clone();

    std::thread::spawn(move || {
        // 等原生窗口出现。show() 之后事件循环马上就转起来了，窗口一般在几百
        // 毫秒内建好；等不到就放弃（记日志，不打断）。
        if crate::window_finder::wait_main_hwnd(crate::window_finder::STARTUP_WAIT).is_none() {
            flog!("文件拖放启用失败（不影响其它功能）: 等不到主窗口");
            return;
        }
        // 切回 UI 线程做真正的注册。此时事件循环肯定在跑了，invoke 不会丢。
        let _ = slint::invoke_from_event_loop(move || {
            match drop_target::install(on_hover, on_paths) {
                Ok(()) => vlog!("文件拖放已启用（原生 IDropTarget）"),
                Err(e) => flog!("文件拖放启用失败（不影响其它功能）: {e}"),
            }
            // 顺手恢复用户存的置顶（HWND 已经确认存在，不用再等）。
            service::apply_saved_topmost(&app_top);
        });
    });
}

/// 「选择…」对话框返回后的统一收尾：写配置 + 回 UI 线程刷界面 + 关掉 busy。
/// 取消时只关 busy，不动其它状态。
fn apply_picked_path(
    weak: &slint::Weak<MainWindow>,
    app: &Arc<App>,
    field: &'static str,
    picked: Option<PathBuf>,
    set_ui: fn(&MainWindow, slint::SharedString),
) {
    match picked {
        Some(path) => {
            let text = path.to_string_lossy().into_owned();
            app.set_text_field(field, text.clone());
            app.save();
            ui::ui_set(weak, move |ui| {
                set_ui(ui, text.into());
                ui.set_busy(false);
            });
        }
        None => ui::set_busy(weak, false),
    }
}

// ============================================================
// 托盘
// ============================================================

pub(crate) fn spawn_tray(ui: &MainWindow, app: &Arc<App>) {
    let weak_show = ui.as_weak();
    let weak_stop = ui.as_weak();
    let weak_copy = ui.as_weak();
    let weak_quit = ui.as_weak();

    let app_stop = app.clone();
    let app_quit = app.clone();

    tray::spawn(tray::Actions {
        // 打开界面
        show: Box::new(move || {
            let weak = weak_show.clone();
            let _ = slint::invoke_from_event_loop(move || {
                if let Some(ui) = weak.upgrade() {
                    let _ = ui.window().show();
                    ui.window().set_minimized(false);
                    // 光 show() 不会把界面画回来，见 force_full_repaint 的注释。
                    force_full_repaint(ui.as_weak());
                    // 光 show() 不会置顶，再显式抢一下前台
                    firewall::focus_main_window();
                } else {
                    flog!("托盘「打开界面」：UI 已销毁");
                }
            });
        }),

        // 停止服务
        stop: Box::new(move || {
            let (app, weak) = (app_stop.clone(), weak_stop.clone());
            let _ = slint::invoke_from_event_loop(move || {
                service::stop_service(&app, &weak);
            });
        }),

        // 复制访问地址（不用先打开界面）
        copy_url: Box::new(move || {
            let weak = weak_copy.clone();
            let _ = slint::invoke_from_event_loop(move || {
                let Some(ui) = weak.upgrade() else { return };
                let url = ui.get_url_text().to_string();
                if url.is_empty() {
                    // 还没启动过服务，没有地址可拷
                    ui::set_hint(&weak, i18n::t(K::HintNoUrlYet), true);
                    return;
                }
                let weak2 = weak.clone();
                std::thread::spawn(move || match firewall::copy_to_clipboard(&url) {
                    Ok(()) => ui::set_hint(&weak2, i18n::t(K::HintUrlCopied), false),
                    Err(e) => ui::set_hint(&weak2, e, true),
                });
            });
        }),

        // 退出
        quit: Box::new(move || {
            let (app, weak) = (app_quit.clone(), weak_quit.clone());
            let _ = slint::invoke_from_event_loop(move || {
                service::quit(&app, &weak);
            });
        }),
    });
}
