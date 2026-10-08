//! 网络辅助：局域网 IP 探测 + 访问地址清单计算（纯逻辑可测）

/// 访问地址清单：本机回环恒在首位；lan 开且有局域网 IP → 追加局域网地址；
/// hosts（允许域名）逐个追加。一律不带 http:// 前缀（用户自识别，复制到浏览器即可）；
/// IP 类地址拼端口；域名不拼端口——域名访问走标准 80（前面通常有反代/Caddy 转发到本服务）。
pub fn access_urls(port: u16, lan: bool, lan_ip: Option<&str>, hosts: &[String]) -> Vec<String> {
    let mut urls = vec![format!("localhost:{port}")];
    if lan {
        if let Some(ip) = lan_ip {
            urls.push(format!("{ip}:{port}"));
        }
    }
    for h in hosts {
        urls.push(h.clone());
    }
    urls
}

/// 本机局域网 IPv4：UDP connect 不出去（不发包）只为让协议栈选出网卡 IP。
/// 无网卡/异常返回 None（调用方降级为不显示局域网地址）。
pub fn lan_ipv4() -> Option<String> {
    let s = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    // 223.5.5.5 = 阿里公共 DNS；UDP connect 只选路由不真的发包
    s.connect("223.5.5.5:53").ok()?;
    let ip = s.local_addr().ok()?.ip();
    if ip.is_ipv4() && !ip.is_loopback() { Some(ip.to_string()) } else { None }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn access_urls_localhost_first() {
        let urls = access_urls(18753, false, None, &[]);
        assert_eq!(urls, vec!["localhost:18753"]);
    }

    #[test]
    fn access_urls_lan_off_hides_ip() {
        let urls = access_urls(18753, false, Some("192.168.1.8"), &[]);
        assert_eq!(urls.len(), 1, "lan 关：给了 IP 也不能显示");
    }

    #[test]
    fn access_urls_lan_on_appends_ip() {
        let urls = access_urls(18753, true, Some("192.168.1.8"), &[]);
        assert_eq!(urls[1], "192.168.1.8:18753");
    }

    #[test]
    fn access_urls_hosts_no_port() {
        // 一律不带 http://；域名不带端口（走标准 80，前面有反代转发）；IP/localhost 仍带端口
        let hosts = vec!["docs.example.com".to_string(), "app.example.com".to_string()];
        let urls = access_urls(18753, true, Some("10.0.0.2"), &hosts);
        assert_eq!(urls[2], "docs.example.com");
        assert_eq!(urls[3], "app.example.com");
    }

    #[test]
    fn lan_ipv4_sane_when_present() {
        // 无网环境允许 None；有则必须是公网/内网 IPv4 而非回环
        if let Some(ip) = lan_ipv4() {
            let addr: std::net::Ipv4Addr = ip.parse().expect("必须是 IPv4");
            assert!(!addr.is_loopback());
        }
    }
}
