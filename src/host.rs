//! Facts about the machine smtui runs on and how the user reached it.

use crate::config::ScpConfig;

#[derive(Debug, Clone)]
pub struct HostInfo {
    pub user: String,
    pub hostname: String,
    /// Server address and port taken from `$SSH_CONNECTION`, when running over SSH.
    pub ssh_server: Option<(String, u16)>,
    pub in_tmux: bool,
}

/// Where a remote user should point `scp` to reach this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScpTarget {
    pub user: String,
    pub host: String,
    pub port: u16,
}

impl HostInfo {
    pub fn detect() -> Self {
        let user = std::env::var("USER")
            .ok()
            .filter(|u| !u.is_empty())
            .or_else(|| whoami::username().ok())
            .unwrap_or_else(|| "user".into());
        let hostname = whoami::hostname().unwrap_or_else(|_| "localhost".into());
        let ssh_server = std::env::var("SSH_CONNECTION").ok().and_then(|v| parse_ssh_connection(&v));
        let in_tmux = std::env::var_os("TMUX").is_some_and(|v| !v.is_empty());
        Self { user, hostname, ssh_server, in_tmux }
    }

    /// The fictional `deploy@edge1` server of `--demo`.
    pub fn demo() -> Self {
        let real = Self::detect();
        Self { user: "deploy".into(), hostname: "edge1".into(), ssh_server: None, in_tmux: real.in_tmux }
    }

    /// `user@host` with the short hostname (up to the first dot), used in the status bar.
    pub fn label(&self) -> String {
        let short = self.hostname.split('.').next().filter(|s| !s.is_empty()).unwrap_or(&self.hostname);
        format!("{}@{short}", self.user)
    }

    /// Config overrides first, then the address from `$SSH_CONNECTION`, then the hostname.
    pub fn scp_target(&self, cfg: &ScpConfig) -> ScpTarget {
        let user = cfg.user.clone().unwrap_or_else(|| self.user.clone());
        let (host, port) = match (&cfg.host, &self.ssh_server) {
            (Some(h), Some((_, p))) => (h.clone(), cfg.port.unwrap_or(*p)),
            (Some(h), None) => (h.clone(), cfg.port.unwrap_or(22)),
            (None, Some((h, p))) => (h.clone(), cfg.port.unwrap_or(*p)),
            (None, None) => (self.hostname.clone(), cfg.port.unwrap_or(22)),
        };
        ScpTarget { user, host, port }
    }
}

/// `$SSH_CONNECTION` is `client_ip client_port server_ip server_port`.
fn parse_ssh_connection(value: &str) -> Option<(String, u16)> {
    let parts: Vec<&str> = value.split_whitespace().collect();
    if parts.len() != 4 {
        return None;
    }
    Some((parts[2].to_string(), parts[3].parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(ssh: Option<(&str, u16)>) -> HostInfo {
        HostInfo {
            user: "deploy".into(),
            hostname: "edge1".into(),
            ssh_server: ssh.map(|(h, p)| (h.into(), p)),
            in_tmux: false,
        }
    }

    #[test]
    fn parses_ssh_connection() {
        assert_eq!(
            parse_ssh_connection("10.0.0.5 52344 10.0.0.1 2222"),
            Some(("10.0.0.1".into(), 2222))
        );
        assert_eq!(parse_ssh_connection("garbage"), None);
    }

    #[test]
    fn scp_target_precedence() {
        let none = ScpConfig::default();
        assert_eq!(host(None).scp_target(&none).host, "edge1");
        let t = host(Some(("10.0.0.1", 2222))).scp_target(&none);
        assert_eq!((t.host.as_str(), t.port), ("10.0.0.1", 2222));
        let cfg = ScpConfig { host: Some("prod".into()), user: Some("ops".into()), port: None };
        let t = host(Some(("10.0.0.1", 2222))).scp_target(&cfg);
        assert_eq!((t.user.as_str(), t.host.as_str(), t.port), ("ops", "prod", 2222));
    }
}
