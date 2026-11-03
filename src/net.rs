//! 网络工具：本机可用 IP 优选、端口占用检测

use std::net::{IpAddr, Ipv6Addr, TcpListener};

/// 一个候选网卡地址
#[derive(Clone, Debug)]
pub struct Candidate {
    /// IPv4 或 IPv6 都收（见 `candidates` 的过滤规则）
    pub ip: IpAddr,
    /// 网卡名（用于展示 / 诊断）
    pub name: String,
    /// 可用性得分，越高越可能是手机能连上的那个
    pub score: i32,
}

impl Candidate {
    /// 是否"看着像虚拟网卡"。
    ///
    /// VPN / 虚拟机 / WSL / 代理工具的地址，手机是连不上的。
    /// 如果用户手动选了这种地址，界面上要给出提醒 ——
    /// 否则他会一直纳闷"为什么扫码打不开"。
    pub fn likely_virtual(&self) -> bool {
        self.score < 50
    }
}

/// 本机**全部**网卡地址，不做任何过滤（这正是它和 `candidates` 的区别）。
///
/// 用途：判断"配置里保存的监听地址在本机还在不在"。这里不能复用 `candidates`，
/// 因为候选为了挑出"手机最可能连上的那个"特意剔掉了回环 / link-local 等等 ——
/// 而用户**可以合法地绑那些地址**（比如 `127.0.0.1`）。
pub fn local_addresses() -> Vec<IpAddr> {
    match local_ip_address::list_afinet_netifas() {
        Ok(v) => v.into_iter().map(|(_, ip)| ip).collect(),
        Err(e) => {
            eprintln!("枚举网卡失败: {e}");
            Vec::new()
        }
    }
}

/// 从保存的监听地址里挑出**本机已经没有的**那些。
///
/// 纯函数（`local` 由调用方传进来），方便直接拿合成数据写断言。
///
/// 判据：
///   - **解析不出来的字符串也算用不了**，要挑出来（配置文件被人手改过）
///   - **回环地址（`127.0.0.0/8`、`::1`）一律跳过**：它们永远绑得上，
///     而且用户是可以合法绑它们的；另外 `::1` 在某些网卡枚举实现里压根不出现，
///     按"必须在列表里"判会误伤
///   - 其余地址必须出现在 `local` 里
pub fn missing_binds(bind_ips: &[String], local: &[IpAddr]) -> Vec<String> {
    // ★ 本机一个地址都枚举不出来（拔了网线 / 枚举失败）时**什么都别删** ——
    //   那**不代表**用户选的网卡已经没了，下次插上网线可能还在。
    //   宁可让他看到 dufs 的原始报错，也不要把他的配置删掉。
    if local.is_empty() {
        return Vec::new();
    }

    bind_ips
        .iter()
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .filter(|s| match s.parse::<IpAddr>() {
            Ok(ip) if ip.is_loopback() => false,
            Ok(ip) => !local.contains(&ip),
            Err(_) => true,
        })
        .map(|s| s.to_string())
        .collect()
}

/// 给一个网卡地址打分。返回 `None` 表示**这个地址不该进候选列表**。
///
/// 抽成纯函数是有意的：这块逻辑一旦写错，用户会在界面/二维码里看到一个
/// **永远连不上的地址**，而且他没有任何线索能看出哪里不对。纯函数才能直接
/// 拿合成地址写断言（见「网卡打分」那组用例）。
///
/// 两条经验规则：
///   1. **IPv6 的分一律低于同档次的 IPv4**。IPv4 那套分数（192.168=100、10=90）
///      是长期验证过的；IPv6 多出一堆连不上的情况（link-local 要带 zone、
///      运营商/单位网络的全局地址从局域网内反而绕不出去、一堆虚拟网卡拿 ULA），
///      所以默认只当"备选"。IPv6 单栈机器上仍然会被选出来（那时没有 IPv4 跟它比）。
///      实测踩过：`fdfe:dcba:9876::1`（Meta Quest Link 虚拟网卡的 ULA）给 95 分时
///      **压过了真正的局域网地址 10.10.0.20（90）**，二维码直接指向一张虚拟网卡。
///   2. **虚拟网卡按名字重罚**，但**不删** —— 用户可能确实就是想用 Tailscale。
pub fn score_addr(ip: IpAddr, if_name: &str) -> Option<i32> {
    // 两个协议族通用的排除项
    if ip.is_loopback() || ip.is_unspecified() || ip.is_multicast() {
        return None;
    }

    let mut score = match ip {
        IpAddr::V4(v4) => {
            // 169.254.x.x 是 link-local，说明根本没拿到 DHCP，手机连不上
            if v4.octets()[0] == 169 && v4.octets()[1] == 254 {
                return None;
            }
            match v4.octets() {
                [192, 168, ..] => 100,                        // 家用路由器最常见
                [10, ..] => 90,                               // 公司/校园网
                [172, b, ..] if (16..=31).contains(&b) => 80, // 172.16~31 私有段
                // 198.18.0.0/15 是 RFC 2544 保留的测试段。用它的有两类：
                // Clash 之类代理工具的 fake-ip，以及 **Meta Quest Link** 的虚拟网卡。
                // 两种都连不上，重点排下去。
                [198, 18 | 19, ..] => 5,
                [100, b, ..] if (64..=127).contains(&b) => 10, // 100.64/10 CGNAT / Tailscale
                _ => 50,
            }
        }
        IpAddr::V6(v6) => {
            let seg = v6.segments();
            // fe80::/10 link-local：要用它**必须**带 `%网卡序号`（zone id），
            // 而那个形式塞不进 URL / 二维码（浏览器不认）→ 手机连不上。
            // 和 IPv4 的 169.254 同理，直接排除。
            if (seg[0] & 0xffc0) == 0xfe80 {
                return None;
            }
            if (seg[0] & 0xfe00) == 0xfc00 {
                55 // fc00::/7 ULA：同一局域网内可用，但大量虚拟网卡也用它
            } else if (seg[0] & 0xe000) == 0x2000 {
                45 // 2000::/3 全球单播：能用，但从局域网内可能绕不出去
            } else {
                20 // 其它（未定义段等）不要排前面
            }
        }
    };

    // 虚拟网卡降权：这些名字基本都是虚拟网卡
    const VIRTUAL_HINTS: [&str; 15] = [
        "vethernet",  // Hyper-V / WSL
        "vmware",
        "virtualbox",
        "vbox",
        "wsl",
        "tailscale",
        "zerotier",
        "docker",
        "loopback",
        "clash",
        "radmin",     // Radmin VPN
        "hamachi",
        "vpn",        // 泛匹配各类 VPN 适配器
        "tun",        // 各类 VPN 的 TUN 虚拟网卡
        // Meta Quest Link（虚拟现实头显的联机虚拟网卡）。它占
        // 198.18.0.1 和 ULA fdfe:dcba:9876::1 —— 两个都很像"可用地址"，
        // 手机上却永远连不上。实测就是它把真地址挤到了第二位。
        "meta",
    ];
    if VIRTUAL_HINTS
        .iter()
        .any(|h| if_name.to_lowercase().contains(h))
    {
        score -= 200; // 降到最末，但仍保留（用户可能确实想用 Tailscale）
    }
    Some(score)
}

/// 枚举本机可用于局域网访问的地址（IPv4 + IPv6），按"可用性"从高到低排序。
///
/// 为什么不直接用 `local_ip()`：
/// 装了 VPN / VMware / VirtualBox / WSL / Docker 的机器上，`local_ip()` 常常返回
/// 虚拟网卡的地址，扫码后手机根本连不上。所以这里枚举全部网卡再打分。
pub fn candidates() -> Vec<Candidate> {
    let mut out: Vec<Candidate> = Vec::new();

    let netifas = match local_ip_address::list_afinet_netifas() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("枚举网卡失败: {e}");
            Vec::new()
        }
    };

    for (name, ip) in netifas {
        let Some(score) = score_addr(ip, &name) else {
            continue;
        };

        // 同一 IP 只保留一条（多网卡可能绑同一地址；IPv6 一个网卡常有多个地址）
        if out.iter().any(|c| c.ip == ip) {
            continue;
        }
        out.push(Candidate { ip, name, score });
    }

    // `IpAddr` 自带全序（V4 排在 V6 前），同分时靠它保证排序稳定、可重现
    out.sort_by(|a, b| b.score.cmp(&a.score).then(a.ip.cmp(&b.ip)));
    out
}

/// 本机 IPv6 是否可用。
///
/// dufs 裸跑时默认**同时**监听 IPv4 和 IPv6（`args.rs::default_addrs()` 在
/// `is_ipv6_available()` 为真时返回 `["0.0.0.0", "::"]`），我们「所有网卡」
/// 也要对齐这个行为（见 `dufs::build_args`）。
///
/// ★ 为什么要探测而不是直接传 `-b ::`：dufs **只要有一个地址绑不上，整个服务就起不来**
///   （实测 `-b 10.99.99.99 -b 127.0.0.1` → `Error: Failed to bind` 后进程直接退出），
///   在没有 IPv6 的机器上多传一个 `::` 就是把服务搞挂。
///
/// 做法与 dufs 略有不同：dufs 用 `socket2` **创建一个** IPv6 socket 来判断
/// （`utils::is_ipv6_available`），我们不引新依赖，直接**真的绑一次** `[::]:0`。
/// 这比「能不能建 socket」更贴近我们的需求 —— 我们要知道的正是
/// **dufs 能不能把 `::` 绑起来**。端口写 0 由系统随便分一个，
/// `TcpListener` drop 即释放，不留副作用。
pub fn ipv6_available() -> bool {
    TcpListener::bind((Ipv6Addr::UNSPECIFIED, 0)).is_ok()
}

/// 端口是否已被占用。
///
/// 注意：这是"预检"，bind 成功后立刻释放，存在竞态窗口，
/// 但足以在启动 dufs 之前给用户一个友好的提示，而不是把 dufs 的报错原样抛出去。
pub fn port_in_use(port: u16) -> bool {
    // 绑定 127.0.0.1 就够：如果别的进程已监听 0.0.0.0:port，这里同样会失败
    TcpListener::bind(("127.0.0.1", port)).is_err()
}

/// 端口是否落在 Windows 的"保留端口段"里。
///
/// 装过 Hyper-V / WSL / Docker 的机器上，系统会预留一大批 TCP 端口段
/// （`netsh int ipv4 show excludedportrange protocol=tcp`）。绑这些端口会拿到
/// `WSAEACCES (10013)`，报错信息和"端口被占用"一模一样 —— 如果直接透传，
/// 用户会去任务管理器里找一个根本不存在的占用者。
///
/// 这里用一个简易判据：能 bind 但 bind 报"权限不允许"的，就是保留段。
#[cfg(windows)]
pub fn port_reserved(port: u16) -> bool {
    match TcpListener::bind(("127.0.0.1", port)) {
        Ok(_) => false,
        Err(e) => e.raw_os_error() == Some(10013), // WSAEACCES
    }
}

#[cfg(not(windows))]
pub fn port_reserved(_port: u16) -> bool {
    false
}

/// 把配置里填的 URL 前缀规范成可以直接拼进 URL 的路径段。
///
/// 处理四件事（都是实测 dufs 0.46 的结果）：
///   1. **先整体 trim**：因为 `build_args()` 用的也是 `cfg.path_prefix.trim()`，
///      两边必须一致 —— 否则用户只打了几个空格时，dufs 那边没收到前缀，
///      而地址里却多出个 `/%20%20%20/`，又扫出一个 400。
///   2. 去掉首尾多余的 `/`。`myfiles`、`/myfiles`、`myfiles/` 在 dufs 眼里等价，
///      但直接拼会得到 `//myfiles/` 这种地址。
///   3. 段内做百分号编码。dufs 自己打印的就是
///      `http://ip:port/%E6%88%91%E7%9A%84%E6%96%87%E4%BB%B6/`，
///      而且实测发原始 UTF-8 字节或未编码的空格过去，它返回 400。
///   4. 保留段内的 `/`，允许 `a/b` 这种多级前缀。
fn encode_path_prefix(prefix: &str) -> String {
    prefix
        .trim()
        .split('/')
        .filter(|seg| !seg.is_empty()) // 顺便吃掉空段（首尾/重复的斜杠）
        .map(encode_segment)
        .collect::<Vec<_>>()
        .join("/")
}

/// 单段百分号编码。只放过 URL 里的 unreserved 字符，其余一律 `%XX`。
/// 这样 `?` `#` `%` 这些会改变 URL 语义的字符也会被安全地转义。
fn encode_segment(seg: &str) -> String {
    let mut out = String::with_capacity(seg.len());
    for b in seg.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// 拼出手机可以访问的完整地址。
///
/// 三个参数一个都不能少，少任何一个用户都会拿到一个打不开的地址：
///   - `https`：配了 TLS 证书时必须用 https，否则浏览器连不上；
///   - `path_prefix`：就是 dufs 的 `--path-prefix`，**必须出现在地址里** ——
///     dufs 只在这个前缀下提供服务，访问根路径会返回 `400 Invalid Path`。
///     之前这里只拼 `ip:port`，导致带前缀时二维码扫出来是 400。
///
/// 有前缀时结尾补一个 `/`，与 dufs 自己打印的
/// `Listening on http://ip:port/myfiles/` 保持一致（带不带都能访问，
/// 但二维码里给个规范形式更省心）。
pub fn access_url(ip: IpAddr, port: u16, https: bool, path_prefix: &str) -> String {
    let scheme = if https { "https" } else { "http" };
    let host = host_for_url(ip);
    let prefix = encode_path_prefix(path_prefix);
    if prefix.is_empty() {
        format!("{scheme}://{host}:{port}")
    } else {
        format!("{scheme}://{host}:{port}/{prefix}/")
    }
}

/// URL 里的主机部分。
///
/// ★ IPv6 **必须**加方括号。`http://fd7a::1:5000/` 会被当成「主机名 fd7a、
///   端口却解析不出来」的非法地址，手机扫码直接打不开；
///   正确写法是 `http://[fd7a::1]:5000/`。
pub fn host_for_url(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(v4) => v4.to_string(),
        IpAddr::V6(v6) => format!("[{v6}]"),
    }
}
