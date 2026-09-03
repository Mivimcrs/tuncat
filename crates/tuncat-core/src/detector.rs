//! Adapter enumeration (via `GetAdaptersAddresses`) and health probing.

use anyhow::Result;
use std::time::Duration;

/// One network adapter as reported by Windows.
#[derive(Debug, Clone)]
pub struct AdapterInfo {
    /// The connection name shown in `ncpa.cpl` (matches ICS connection names).
    pub friendly_name: String,
    /// Device/driver description.
    pub description: String,
    /// OperStatus == Up.
    pub oper_up: bool,
    /// Has a default gateway (typical for physical Internet adapters).
    pub has_gateway: bool,
    /// First IPv4 address, if any.
    pub ipv4: Option<String>,
}

impl AdapterInfo {
    /// Case-insensitive keyword match against friendly name and description.
    pub fn matches(&self, keyword: &str) -> bool {
        let k = keyword.to_lowercase();
        self.friendly_name.to_lowercase().contains(&k)
            || self.description.to_lowercase().contains(&k)
    }
}

/// Enumerate all adapters with IPv4 info.
///
/// The friendly names returned here match the `Name` property of ICS
/// `NetConnectionProps`, which is what the pulse logic relies on.
pub fn list_adapters() -> Result<Vec<AdapterInfo>> {
    use windows::Win32::Foundation::{ERROR_BUFFER_OVERFLOW, NO_ERROR};
    use windows::Win32::NetworkManagement::IpHelper::{
        GetAdaptersAddresses, GET_ADAPTERS_ADDRESSES_FLAGS,
    };
    use windows::Win32::NetworkManagement::Ndis::IF_OPER_STATUS;
    use windows::Win32::Networking::WinSock::{AF_INET, AF_UNSPEC};
    use windows::core::PWSTR;

    const GAA_FLAG_SKIP_ANYCAST: u32 = 0x0002;
    const GAA_FLAG_SKIP_MULTICAST: u32 = 0x0004;
    const GAA_FLAG_INCLUDE_GATEWAYS: u32 = 0x0080;
    let flags = GET_ADAPTERS_ADDRESSES_FLAGS(
        GAA_FLAG_SKIP_ANYCAST | GAA_FLAG_SKIP_MULTICAST | GAA_FLAG_INCLUDE_GATEWAYS,
    );

    // First call to learn the required buffer size.
    let mut size: u32 = 16 * 1024;
    let mut buffer: Vec<u8>;
    let mut head: *mut windows::Win32::NetworkManagement::IpHelper::IP_ADAPTER_ADDRESSES_LH;
    loop {
        buffer = vec![0u8; size as usize];
        head = buffer.as_mut_ptr()
            as *mut windows::Win32::NetworkManagement::IpHelper::IP_ADAPTER_ADDRESSES_LH;
        let code = unsafe {
            GetAdaptersAddresses(AF_UNSPEC.0 as u32, flags, None, Some(head), &mut size)
        };
        if code == NO_ERROR.0 {
            break;
        }
        if code == ERROR_BUFFER_OVERFLOW.0 {
            continue; // buffer resized per `size`, retry
        }
        anyhow::bail!("GetAdaptersAddresses failed: {}", code);
    }

    let mut out = Vec::new();
    let mut cur = head;
    while !cur.is_null() {
        let a = unsafe { &*cur };

        let friendly_name =
            unsafe { PWSTR(a.FriendlyName.0).to_string() }.unwrap_or_default();
        let description =
            unsafe { PWSTR(a.Description.0).to_string() }.unwrap_or_default();
        let oper_up = a.OperStatus == IF_OPER_STATUS(1); // IfOperStatusUp

        let has_gateway = unsafe {
            let mut g = a.FirstGatewayAddress;
            let mut found = false;
            while !g.is_null() {
                let ga = &*g;
                if ga.Address.lpSockaddr.is_null() {
                    g = ga.Next;
                    continue;
                }
                let sa = &*ga.Address.lpSockaddr;
                if sa.sa_family == AF_INET {
                    found = true;
                    break;
                }
                g = ga.Next;
            }
            found
        };

        let ipv4 = unsafe {
            let mut u = a.FirstUnicastAddress;
            let mut ip = None;
            while !u.is_null() {
                let ua = &*u;
                if !ua.Address.lpSockaddr.is_null() {
                    let sa = &*ua.Address.lpSockaddr;
                    if sa.sa_family == AF_INET {
                        let sin = &*(ua.Address.lpSockaddr
                            as *const windows::Win32::Networking::WinSock::SOCKADDR
                            as *const windows::Win32::Networking::WinSock::SOCKADDR_IN);
                        let b = sin.sin_addr.S_un.S_un_b;
                        ip = Some(format!(
                            "{}.{}.{}.{}",
                            b.s_b1, b.s_b2, b.s_b3, b.s_b4
                        ));
                        break;
                    }
                }
                u = ua.Next;
            }
            ip
        };

        out.push(AdapterInfo {
            friendly_name,
            description,
            oper_up,
            has_gateway,
            ipv4,
        });

        cur = a.Next;
    }
    Ok(out)
}

/// Pick the TUN adapter: first "up" adapter matching any keyword.
pub fn find_tun<'a>(
    adapters: &'a [AdapterInfo],
    keywords: &[String],
) -> Option<&'a AdapterInfo> {
    adapters
        .iter()
        .filter(|a| a.oper_up)
        .find(|a| keywords.iter().any(|k| a.matches(k)))
}

/// Pick the public adapter: first "up" adapter with a gateway matching any
/// keyword; falls back to the first "up" gateway-holding adapter.
pub fn find_public<'a>(
    adapters: &'a [AdapterInfo],
    keywords: &[String],
) -> Option<&'a AdapterInfo> {
    let candidates: Vec<&AdapterInfo> = adapters
        .iter()
        .filter(|a| a.oper_up && a.has_gateway)
        .collect();
    candidates
        .iter()
        .copied()
        .find(|a| keywords.iter().any(|k| a.matches(k)))
        .or_else(|| candidates.first().copied())
}

/// Outcome of one health probe.
#[derive(Debug, Clone)]
pub enum ProbeResult {
    /// HTTP 2xx received; carries round-trip latency.
    Healthy(Duration),
    /// Request failed; carries a short human-readable reason.
    Unhealthy(String),
}

/// Probe `url` with a timeout. Blocking; run off the UI thread.
pub fn probe(url: &str, timeout_sec: u64) -> ProbeResult {
    let started = std::time::Instant::now();
    let timeout = Duration::from_secs(timeout_sec.max(1));
    let agent = ureq::AgentBuilder::new()
        .timeout(timeout)
        .redirects(0)
        .build();
    match agent.get(url).call() {
        Ok(resp) if resp.status() >= 200 && resp.status() < 300 => {
            ProbeResult::Healthy(started.elapsed())
        }
        Ok(resp) => ProbeResult::Unhealthy(format!("HTTP {}", resp.status())),
        Err(e) => {
            let reason = match &e {
                ureq::Error::Status(code, _) => format!("HTTP {}", code),
                ureq::Error::Transport(t) => {
                    let kind = t.kind().to_string();
                    // Distinguish DNS failure from connectivity failure in logs.
                    let detail = t
                        .message()
                        .map(|m| m.to_string())
                        .unwrap_or_else(|| kind.clone());
                    if kind.contains("dns") {
                        format!("DNS 失败: {}", detail)
                    } else {
                        format!("连接失败: {}", detail)
                    }
                }
            };
            ProbeResult::Unhealthy(reason)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keyword_match_is_case_insensitive() {
        let a = AdapterInfo {
            friendly_name: "vgate0".into(),
            description: "Mihomo Virtual Adapter".into(),
            oper_up: true,
            has_gateway: false,
            ipv4: None,
        };
        assert!(a.matches("MIHOMO"));
        assert!(a.matches("vgate"));
        assert!(!a.matches("wlan"));
    }

    #[test]
    fn find_tun_prefers_up_adapters() {
        let ads = vec![
            AdapterInfo {
                friendly_name: "mihomo".into(),
                description: String::new(),
                oper_up: false,
                has_gateway: false,
                ipv4: None,
            },
            AdapterInfo {
                friendly_name: "Meta Tunnel".into(),
                description: String::new(),
                oper_up: true,
                has_gateway: false,
                ipv4: None,
            },
        ];
        let kws = vec!["mihomo".to_string(), "meta tunnel".to_string()];
        assert_eq!(
            find_tun(&ads, &kws).map(|a| a.friendly_name.as_str()),
            Some("Meta Tunnel")
        );
    }
}
