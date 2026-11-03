//! 检查 GitHub 上有没有新版本。
//!
//! ★ 只查、不下载、不替换 exe：用户点「检查更新」时才发请求，
//!   其余时间本程序不联网（启动时也不查）。
//!
//! 两个接口都要问，原因很现实：
//!   - `/releases/latest` **只认正式发布**。仓库里如果只有标了 prerelease 的版本，
//!     它会返回 404（本项目最初就是这样：唯一那条 v0.1.0 是 prerelease）。
//!   - 所以 404 时要回退到 `/releases?per_page=1`，把最新的一条（含预发布）拿来看。
//!     否则用户明明有新版可下，却会看到「还没有发布版本」。
//!
//! 其他取舍：
//!   - 版本号按 `主.次.修订` 的数字比（`0.1.10 > 0.1.9`），绝不做字符串比较；
//!   - 任何解析失败都**不**当成「有新版本」，宁可报错让用户自己去看发布页。

use std::time::Duration;

use crate::i18n::{self, K};

/// 仓库地址直接取 `Cargo.toml` 的 `repository`，改名 / 转移后自动跟上。
const REPO: &str = env!("CARGO_PKG_REPOSITORY");

/// 本程序版本（`Cargo.toml` 的 `version`）
pub fn current_version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

/// 发布页（「打开发布页」用）
pub fn releases_url() -> String {
    format!("{}/releases", REPO.trim_end_matches('/'))
}

/// API 根地址。
///
/// `DUFS_GUI_UPDATE_API` 只用于**本地测试**（指到假服务器，验证解析与状态码分支），
/// 正常使用不会去读它。
fn api_base() -> String {
    if let Ok(url) = std::env::var("DUFS_GUI_UPDATE_API")
        && !url.trim().is_empty()
    {
        return url.trim().trim_end_matches('/').to_string();
    }
    let slug = REPO
        .trim_end_matches('/')
        .rsplit("github.com/")
        .next()
        .unwrap_or(REPO);
    format!("https://api.github.com/repos/{slug}")
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    UpToDate,
    Newer {
        /// 已去掉 `v` 前缀
        version: String,
        /// 该版本在 GitHub 上标了 prerelease（提示文案要说明）
        prerelease: bool,
    },
    /// 仓库里一个发布都没有（正式 / 预发布都没有）
    NoRelease,
}

#[derive(Debug, PartialEq, Eq)]
pub enum CheckError {
    /// 403 / 429：未认证接口按 IP 限流
    RateLimited,
    /// 连不上（DNS / 超时 / TLS / 代理）
    NoNet(String),
    /// 返回了内容但看不懂
    BadResponse(String),
    /// tag 不是 `1.2.3` 这种形状
    BadVersion(String),
    /// 仓库本身不存在（两个端点都 404）
    RepoNotFound,
}

impl CheckError {
    pub fn describe(&self) -> String {
        match self {
            CheckError::RateLimited => i18n::t(K::UpdateRateLimited).to_string(),
            CheckError::NoNet(d) => i18n::tn(K::UpdateNoNet, &[d]),
            CheckError::BadResponse(d) => i18n::tn(K::UpdateBadResponse, &[d]),
            CheckError::BadVersion(v) => i18n::tn(K::UpdateBadVersion, &[v]),
            CheckError::RepoNotFound => i18n::t(K::UpdateRepoNotFound).to_string(),
        }
    }
}

#[derive(serde::Deserialize)]
struct ApiRelease {
    tag_name: String,
    #[serde(default)]
    prerelease: bool,
    #[serde(default)]
    draft: bool,
}

/// 查一次最新版本。**这是本程序唯一的出站请求（最多两次 HTTP）。**
pub fn check() -> Result<Outcome, CheckError> {
    let config = ureq::Agent::config_builder()
        // 两次请求合计不超过 8 秒：网络不通时不能让用户干等
        .timeout_global(Some(Duration::from_secs(8)))
        // 非 2xx 不走 Err 分支，下面按状态码自己分类（404 / 403 的含义不同）
        .http_status_as_error(false)
        // ★ `native-tls` 不会被 ureq 自动选中，必须显式指定。
        //   根证书用**系统**的（SChannel），不把 Mozilla 证书包编进 exe ——
        //   既省体积，也能跟上系统/企业环境的证书更新。
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .provider(ureq::tls::TlsProvider::NativeTls)
                .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                .build(),
        )
        .build();
    let agent: ureq::Agent = config.into();
    let base = api_base();

    // ---------- 1) 正式发布 ----------
    let (code, body) = get(&agent, &format!("{base}/releases/latest"))?;
    match code {
        200 => return judge(&parse_release(&body)?),
        404 => {} // 没有正式发布 → 走下面的预发布回退
        403 | 429 => return Err(CheckError::RateLimited),
        c => return Err(CheckError::BadResponse(format!("HTTP {c}"))),
    }

    // ---------- 2) 回退：最新一条（可能是预发布） ----------
    let (code, body) = get(&agent, &format!("{base}/releases?per_page=1"))?;
    match code {
        200 => {}
        404 => return Err(CheckError::RepoNotFound),
        403 | 429 => return Err(CheckError::RateLimited),
        c => return Err(CheckError::BadResponse(format!("HTTP {c}"))),
    }
    let list: Vec<ApiRelease> =
        serde_json::from_str(&body).map_err(|e| CheckError::BadResponse(e.to_string()))?;
    match list.into_iter().find(|r| !r.draft) {
        Some(rel) => judge(&rel),
        None => Ok(Outcome::NoRelease),
    }
}

/// 发一次 GET，把状态码和响应体一起返回（状态码由调用方分类）。
fn get(agent: &ureq::Agent, url: &str) -> Result<(u16, String), CheckError> {
    let mut resp = agent
        .get(url)
        // GitHub 要求带 User-Agent，不带会直接 403
        .header(
            "User-Agent",
            concat!("dufs-gui/", env!("CARGO_PKG_VERSION")),
        )
        .header("Accept", "application/vnd.github+json")
        .call()
        .map_err(|e| CheckError::NoNet(e.to_string()))?;
    let code = resp.status().as_u16();
    let text = resp
        .body_mut()
        .read_to_string()
        .map_err(|e| CheckError::BadResponse(e.to_string()))?;
    Ok((code, text))
}

fn parse_release(body: &str) -> Result<ApiRelease, CheckError> {
    serde_json::from_str(body).map_err(|e| CheckError::BadResponse(e.to_string()))
}

/// 把一个 release 判成「已最新 / 有新版本 / 版本号看不懂」。
fn judge(rel: &ApiRelease) -> Result<Outcome, CheckError> {
    let tag = rel.tag_name.trim();
    match is_newer(current_version(), tag) {
        Some(true) => Ok(Outcome::Newer {
            version: strip_v(tag).to_string(),
            prerelease: rel.prerelease,
        }),
        Some(false) => Ok(Outcome::UpToDate),
        None => Err(CheckError::BadVersion(tag.to_string())),
    }
}

/// 去掉可能存在的 `v` / `V` 前缀。
fn strip_v(s: &str) -> &str {
    s.strip_prefix('v')
        .or_else(|| s.strip_prefix('V'))
        .unwrap_or(s)
}

/// 解析 `1.2.3` / `v1.2.3` / `1.2`（缺的段按 0 补）。
///
/// 只接受纯数字段：带预发布后缀（`0.2.0-beta`）或任何非数字内容一律返回 `None`，
/// 免得把 `2024-final` 这种 tag 误判成版本号。
pub fn parse_version(s: &str) -> Option<(u64, u64, u64)> {
    let s = strip_v(s.trim());
    if s.is_empty() {
        return None;
    }
    let mut out = [0u64; 3];
    let mut n = 0;
    for seg in s.split('.') {
        if n == 3 || seg.is_empty() || !seg.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        out[n] = seg.parse().ok()?;
        n += 1;
    }
    (n > 0).then_some((out[0], out[1], out[2]))
}

/// 远端是否比当前新。任一侧解析失败 → `None`：调用方**不能**当成「有新版本」。
pub fn is_newer(current: &str, remote: &str) -> Option<bool> {
    Some(parse_version(remote)? > parse_version(current)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn 版本号只认数字段() {
        assert_eq!(parse_version("0.1.0"), Some((0, 1, 0)));
        assert_eq!(parse_version("v0.1.0"), Some((0, 1, 0)));
        assert_eq!(parse_version("V0.1.0"), Some((0, 1, 0)));
        assert_eq!(parse_version(" 1.2 "), Some((1, 2, 0)));
        assert_eq!(parse_version("3"), Some((3, 0, 0)));
        // 多于三段 / 预发布 / 非数字 / 空段 / 空串
        assert_eq!(parse_version("1.2.3.4"), None);
        assert_eq!(parse_version("0.2.0-beta.1"), None);
        assert_eq!(parse_version("abc"), None);
        assert_eq!(parse_version("1..2"), None);
        assert_eq!(parse_version(""), None);
    }

    #[test]
    fn 版本比较按数字而不是字符串() {
        // 字符串比较会认为 "0.1.10" < "0.1.9"，这里必须反过来
        assert_eq!(is_newer("0.1.0", "0.1.10"), Some(true));
        assert_eq!(is_newer("0.1.9", "0.1.10"), Some(true));
        assert_eq!(is_newer("0.1.99", "0.2.0"), Some(true));
        assert_eq!(is_newer("0.1.0", "v0.1.1"), Some(true));
        // 相同 / 远端更旧 → 不算更新
        assert_eq!(is_newer("0.1.0", "0.1.0"), Some(false));
        assert_eq!(is_newer("0.2.0", "0.1.9"), Some(false));
        // 解析不了 → None（不能当成有新版本）
        assert_eq!(is_newer("0.1.0", "not-a-version"), None);
        assert_eq!(is_newer("not-a-version", "0.1.1"), None);
    }

    #[test]
    fn 地址从仓库元数据派生() {
        // 只用默认（不读环境变量）的路径核对形状
        assert!(releases_url().starts_with("https://github.com/"));
        assert!(releases_url().ends_with("/releases"));
    }

    /// 按顺序应答 N 个请求的本地 HTTP 服务器，返回 base URL。
    fn fake_server(responses: Vec<(&'static str, String)>) -> String {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
        let port = listener.local_addr().expect("addr").port();
        std::thread::spawn(move || {
            for (status, body) in responses {
                let Ok((mut s, _)) = listener.accept() else {
                    return;
                };
                let mut buf = [0u8; 8192];
                let _ = s.read(&mut buf); // 路径与请求头都不重要
                let len = body.len();
                let resp = format!(
                    "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n\
                     Content-Length: {len}\r\nConnection: close\r\n\r\n{body}"
                );
                let _ = s.write_all(resp.as_bytes());
            }
        });
        format!("http://127.0.0.1:{port}")
    }

    /// 给 `check()` 指定 base URL 跑一次，跑完把环境变量清掉。
    ///
    /// ★ 所有分支放在**同一个测试**里顺序跑：`DUFS_GUI_UPDATE_API` 是进程级环境变量，
    ///   拆成多个 test 会并行打架。
    fn check_against(base: &str) -> Result<Outcome, CheckError> {
        // SAFETY: 只有这个测试碰这个变量，且同一测试内是串行的。
        unsafe { std::env::set_var("DUFS_GUI_UPDATE_API", base) };
        let out = check();
        unsafe { std::env::remove_var("DUFS_GUI_UPDATE_API") };
        out
    }

    #[test]
    fn 完整http路径_六种情况都能分对() {
        // 1) 正式发布里有更新的版本
        let base = fake_server(vec![("200 OK", r#"{"tag_name":"v99.0.0"}"#.to_string())]);
        assert_eq!(
            check_against(&base),
            Ok(Outcome::Newer {
                version: "99.0.0".into(),
                prerelease: false
            })
        );

        // 2) 正式发布就是当前版本 → 已是最新
        let same = format!(r#"{{"tag_name":"v{}"}}"#, current_version());
        let base = fake_server(vec![("200 OK", same)]);
        assert_eq!(check_against(&base).ok(), Some(Outcome::UpToDate));

        // 3) 没有正式发布（404），但有一条更新的预发布 → 仍要提示（否则永远看不到）
        let base = fake_server(vec![
            ("404 Not Found", String::new()),
            (
                "200 OK",
                r#"[{"tag_name":"v99.1.0","prerelease":true}]"#.to_string(),
            ),
        ]);
        assert_eq!(
            check_against(&base),
            Ok(Outcome::Newer {
                version: "99.1.0".into(),
                prerelease: true
            })
        );

        // 4) 正式 / 预发布都没有 → NoRelease
        let base = fake_server(vec![
            ("404 Not Found", String::new()),
            ("200 OK", "[]".to_string()),
        ]);
        assert_eq!(check_against(&base).ok(), Some(Outcome::NoRelease));

        // 5) 限流：错误种类要能区分（文案不同）
        let base = fake_server(vec![("403 Forbidden", String::new())]);
        assert_eq!(check_against(&base), Err(CheckError::RateLimited));

        // 6) 两个端点都 404 → 仓库不存在（不是"没有发布"）
        let base = fake_server(vec![
            ("404 Not Found", String::new()),
            ("404 Not Found", String::new()),
        ]);
        assert_eq!(check_against(&base), Err(CheckError::RepoNotFound));
    }

    /// 真连一次 GitHub（`cargo test -- --ignored` 才会跑）。
    ///
    /// 不放进普通测试：网络不通、被墙、或 GitHub 限流时会把构建搞红。
    /// 但它能验证假服务器测不到的那一段 —— **TLS（SChannel）真的能用**。
    /// 只断言"拿到一个合法结论"，不钉版本号（发了新版本后这个测试不该变红）。
    #[test]
    #[ignore = "需要网络：cargo test -- --ignored 手动跑"]
    fn 真连github() {
        match check() {
            Ok(out) => assert!(matches!(
                out,
                Outcome::UpToDate | Outcome::Newer { .. } | Outcome::NoRelease
            )),
            // 手动跑时网络被代理/限流拦住也属于预期，不能算代码错
            Err(e @ (CheckError::NoNet(_) | CheckError::RateLimited)) => {
                eprintln!("跳过（网络原因）：{}", e.describe());
            }
            Err(e) => panic!("真实响应解析失败：{}", e.describe()),
        }
    }
}
