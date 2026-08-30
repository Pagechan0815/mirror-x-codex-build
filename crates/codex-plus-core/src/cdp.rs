use anyhow::{Context, bail};
use serde::Deserialize;
use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, TcpStream};
use std::time::Duration;

const CDP_HTTP_TIMEOUT: Duration = Duration::from_secs(3);
const CDP_PROBE_TIMEOUT: Duration = Duration::from_millis(300);
const CDP_PROBE_MAX_BYTES: usize = 256 * 1024;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct CdpTarget {
    pub id: String,
    #[serde(rename = "type")]
    pub target_type: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub url: String,
    #[serde(default, rename = "webSocketDebuggerUrl")]
    pub web_socket_debugger_url: Option<String>,
}

/// Checks that the loopback port belongs to the Codex desktop app, not merely any HTTP server.
pub(crate) fn endpoint_available(debug_port: u16) -> bool {
    [
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), debug_port),
        SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), debug_port),
    ]
    .into_iter()
    .any(|address| probe_endpoint(address, debug_port))
}

fn probe_endpoint(address: SocketAddr, debug_port: u16) -> bool {
    let Ok(mut stream) = TcpStream::connect_timeout(&address, CDP_PROBE_TIMEOUT) else {
        return false;
    };
    let _ = stream.set_read_timeout(Some(CDP_PROBE_TIMEOUT));
    let _ = stream.set_write_timeout(Some(CDP_PROBE_TIMEOUT));
    let request =
        format!("GET /json HTTP/1.1\r\nHost: 127.0.0.1:{debug_port}\r\nConnection: close\r\n\r\n");
    if stream.write_all(request.as_bytes()).is_err() {
        return false;
    }

    let mut response = Vec::new();
    let mut chunk = [0_u8; 8192];
    while response.len() < CDP_PROBE_MAX_BYTES {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => response.extend_from_slice(&chunk[..read]),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                break;
            }
            Err(_) => return false,
        }
    }
    response_contains_codex_target(&response, debug_port)
}

fn response_contains_codex_target(response: &[u8], debug_port: u16) -> bool {
    let Some(header_end) = response.windows(4).position(|window| window == b"\r\n\r\n") else {
        return false;
    };
    let headers = String::from_utf8_lossy(&response[..header_end]);
    if !headers
        .lines()
        .next()
        .is_some_and(|line| line.starts_with("HTTP/") && line.contains(" 200 "))
    {
        return false;
    }
    let Ok(targets) = serde_json::from_slice::<Vec<CdpTarget>>(&response[header_end + 4..]) else {
        return false;
    };
    targets.iter().any(|target| {
        is_primary_codex_app_target(target)
            && target
                .web_socket_debugger_url
                .as_deref()
                .is_some_and(|url| cdp_websocket_matches_port(url, debug_port))
    })
}

fn cdp_websocket_matches_port(url: &str, expected_port: u16) -> bool {
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return false;
    };
    if !matches!(parsed.scheme(), "ws" | "wss") || parsed.port() != Some(expected_port) {
        return false;
    }
    parsed
        .host_str()
        .and_then(|host| {
            host.trim_start_matches('[')
                .trim_end_matches(']')
                .parse::<IpAddr>()
                .ok()
        })
        .is_some_and(|address| address.is_loopback())
}

pub async fn list_targets(debug_port: u16) -> anyhow::Result<Vec<CdpTarget>> {
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(CDP_HTTP_TIMEOUT)
        .build()
        .context("failed to build CDP HTTP client")?;

    let urls = [
        format!("http://127.0.0.1:{debug_port}/json"),
        format!("http://[::1]:{debug_port}/json"),
    ];
    let mut errors = Vec::new();
    for url in urls {
        match query_targets_url(&client, &url).await {
            Ok(targets) => return Ok(targets),
            Err(error) => errors.push(format!("{url}: {error:#}")),
        }
    }

    bail!(
        "failed to query CDP targets on loopback addresses: {}",
        errors.join("; ")
    )
}

async fn query_targets_url(client: &reqwest::Client, url: &str) -> anyhow::Result<Vec<CdpTarget>> {
    let response = client
        .get(url)
        .send()
        .await
        .context("failed to query CDP targets")?
        .error_for_status()
        .context("CDP target query failed")?;

    response
        .json::<Vec<CdpTarget>>()
        .await
        .context("failed to deserialize CDP targets")
}

pub fn pick_page_target(targets: &[CdpTarget]) -> anyhow::Result<CdpTarget> {
    let mut first_page = None;
    for target in targets
        .iter()
        .filter(|target| is_injectable_page_target(target))
    {
        first_page.get_or_insert(target);
        if is_primary_codex_page_target(target) {
            return Ok(target.clone());
        }
    }

    if let Some(target) = first_page {
        return Ok(target.clone());
    }

    bail!("No injectable page target found")
}

pub fn pick_injectable_codex_page_target(targets: &[CdpTarget]) -> anyhow::Result<CdpTarget> {
    let priorities: [fn(&CdpTarget) -> bool; 4] = [
        is_exact_codex_app_main_target,
        is_primary_codex_app_target,
        is_chatgpt_desktop_page_target,
        is_supported_codex_page_target,
    ];
    for matches_priority in priorities {
        if let Some(target) = targets
            .iter()
            .find(|target| is_injectable_page_target(target) && matches_priority(target))
        {
            return Ok(target.clone());
        }
    }

    bail!("No injectable Codex page target found")
}

fn is_codex_app_page_target(target: &CdpTarget) -> bool {
    let Ok(url) = reqwest::Url::parse(target.url.trim()) else {
        return false;
    };
    url.scheme().eq_ignore_ascii_case("app")
        && url.host_str() == Some("-")
        && url.path().eq_ignore_ascii_case("/index.html")
}

pub fn is_injectable_page_target(target: &CdpTarget) -> bool {
    target.target_type == "page"
        && target
            .web_socket_debugger_url
            .as_deref()
            .is_some_and(|url| !url.is_empty())
}

pub fn is_codex_page_target(target: &CdpTarget) -> bool {
    if target.target_type != "page" {
        return false;
    }
    let haystack = format!("{} {}", target.title, target.url).to_lowercase();
    haystack.contains("codex") || is_chatgpt_desktop_page(&target.title, &target.url)
}

pub fn is_primary_codex_page_target(target: &CdpTarget) -> bool {
    is_codex_page_target(target)
        && !is_avatar_overlay_page_target(target)
        && !is_quick_chat_page_target(target)
}

fn is_exact_codex_app_main_target(target: &CdpTarget) -> bool {
    target.url.trim().eq_ignore_ascii_case("app://-/index.html")
}

fn is_primary_codex_app_target(target: &CdpTarget) -> bool {
    is_codex_app_page_target(target) && is_primary_codex_page_target(target)
}

fn is_chatgpt_desktop_page_target(target: &CdpTarget) -> bool {
    is_primary_codex_page_target(target) && is_chatgpt_desktop_page(&target.title, &target.url)
}

fn is_supported_codex_page_target(target: &CdpTarget) -> bool {
    is_primary_codex_page_target(target)
        && (is_codex_app_page_target(target) || is_chatgpt_desktop_page(&target.title, &target.url))
}

pub fn is_avatar_overlay_page_target(target: &CdpTarget) -> bool {
    initial_route(target).is_some_and(|route| route.eq_ignore_ascii_case("/avatar-overlay"))
}

pub fn is_quick_chat_page_target(target: &CdpTarget) -> bool {
    initial_route(target).is_some_and(|route| {
        let route = route.to_ascii_lowercase();
        route == "/chatgpt/quick-chat"
            || route == "/chatgpt/quick-chat-prewarm"
            || route.starts_with("/chatgpt/quick-chat/")
    })
}

fn initial_route(target: &CdpTarget) -> Option<String> {
    if !is_injectable_page_target(target) || !is_codex_app_page_target(target) {
        return None;
    }
    let url = reqwest::Url::parse(target.url.trim()).ok()?;
    url.query_pairs()
        .find(|(key, _)| key.eq_ignore_ascii_case("initialRoute"))
        .map(|(_, value)| value.into_owned())
}

fn is_chatgpt_desktop_page(title: &str, url: &str) -> bool {
    let title = title.trim().to_ascii_lowercase();
    let url = url.trim().to_ascii_lowercase();
    title == "chatgpt"
        && (url == "https://chatgpt.com"
            || url.starts_with("https://chatgpt.com/")
            || url == "https://chat.openai.com"
            || url.starts_with("https://chat.openai.com/")
            || url.starts_with("data:text/html"))
}

#[cfg(test)]
mod endpoint_tests {
    use super::*;
    use std::net::TcpListener;
    use std::thread;

    fn serve_once(build_body: impl FnOnce(u16) -> String) -> (u16, thread::JoinHandle<()>) {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let body = build_body(port);
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            stream.write_all(response.as_bytes()).unwrap();
        });
        (port, handle)
    }

    #[test]
    fn endpoint_available_accepts_codex_desktop_target() {
        let (port, server) = serve_once(|port| {
            format!(
                r#"[{{"id":"codex","type":"page","title":"Codex","url":"app://-/index.html","webSocketDebuggerUrl":"ws://127.0.0.1:{port}/devtools/page/1"}}]"#
            )
        });
        assert!(endpoint_available(port));
        server.join().unwrap();
    }

    #[test]
    fn endpoint_available_rejects_ordinary_http_and_browser_targets() {
        let (port, server) = serve_once(|port| {
            format!(
                r#"[{{"id":"chrome","type":"page","title":"New Tab","url":"chrome://newtab","webSocketDebuggerUrl":"ws://127.0.0.1:{port}/devtools/page/1"}}]"#
            )
        });
        assert!(!endpoint_available(port));
        server.join().unwrap();
    }

    #[test]
    fn endpoint_available_rejects_non_main_codex_app_target() {
        let (port, server) = serve_once(|port| {
            format!(
                r#"[{{"id":"other","type":"page","title":"Codex","url":"app://-/other.html","webSocketDebuggerUrl":"ws://127.0.0.1:{port}/devtools/page/1"}}]"#
            )
        });
        assert!(!endpoint_available(port));
        server.join().unwrap();
    }

    #[test]
    fn endpoint_available_rejects_quick_chat_only_target() {
        let (port, server) = serve_once(|port| {
            format!(
                r#"[{{"id":"quick-chat","type":"page","title":"Codex","url":"app://-/index.html?initialRoute=%2Fchatgpt%2Fquick-chat-prewarm","webSocketDebuggerUrl":"ws://127.0.0.1:{port}/devtools/page/1"}}]"#
            )
        });
        assert!(!endpoint_available(port));
        server.join().unwrap();
    }
}
