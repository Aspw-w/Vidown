use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

struct Cookie {
    domain: String,
    include_subdomains: bool,
    path: String,
    secure: bool,
    expires: u64,
    name: String,
    value: String,
}

pub struct CookieJar {
    cookies: Vec<Cookie>,
}

impl CookieJar {
    pub fn load(path: &Path) -> std::io::Result<Self> {
        let text = std::fs::read_to_string(path)?;
        let cookies = text
            .lines()
            .filter_map(|line| {
                let line = line.trim_end_matches(['\r', '\n']);
                let line = line.strip_prefix("#HttpOnly_").unwrap_or(line);
                if line.trim().is_empty() || line.starts_with('#') {
                    return None;
                }
                let fields: Vec<&str> = line.split('\t').collect();
                if fields.len() < 7 {
                    return None;
                }
                let domain = fields[0].trim().to_ascii_lowercase();
                Some(Cookie {
                    include_subdomains: fields[1].eq_ignore_ascii_case("TRUE") || domain.starts_with('.'),
                    domain: domain.trim_start_matches('.').to_string(),
                    path: fields[2].to_string(),
                    secure: fields[3].eq_ignore_ascii_case("TRUE"),
                    expires: fields[4].trim().parse().unwrap_or(0),
                    name: fields[5].to_string(),
                    value: fields[6].to_string(),
                })
            })
            .collect();
        Ok(Self { cookies })
    }

    pub fn header_for(&self, url: &str) -> Option<String> {
        let (scheme, rest) = url.split_once("://")?;
        let is_https = scheme.eq_ignore_ascii_case("https");
        let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
        let authority = &rest[..authority_end];
        let host_port = authority.rsplit_once('@').map_or(authority, |(_, host)| host);
        let host = host_port
            .rsplit_once(':')
            .filter(|(_, port)| port.chars().all(|c| c.is_ascii_digit()))
            .map_or(host_port, |(host, _)| host)
            .to_ascii_lowercase();
        let path_part = &rest[authority_end..];
        let path = path_part.split(['?', '#']).next().filter(|path| !path.is_empty()).unwrap_or("/");

        let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |since| since.as_secs());
        let pairs: Vec<String> = self
            .cookies
            .iter()
            .filter(|cookie| {
                let domain_matches = host == cookie.domain
                    || (cookie.include_subdomains && host.ends_with(&format!(".{}", cookie.domain)));
                domain_matches
                    && path.starts_with(&cookie.path)
                    && (!cookie.secure || is_https)
                    && (cookie.expires == 0 || cookie.expires > now)
            })
            .map(|cookie| format!("{}={}", cookie.name, cookie.value))
            .collect();

        (!pairs.is_empty()).then(|| pairs.join("; "))
    }
}
