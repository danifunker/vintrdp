//! The connection dialog's fields, shared by every front-end so they all
//! validate the same way and produce the same [`Config`].

use vintrdp_core::{Config, RdpVersion};

pub const DEPTHS: [u32; 5] = [8, 15, 16, 24, 32];

/// What the user typed; every front-end's dialog edits one of these.
#[derive(Clone, Debug)]
pub struct Form {
    /// `host` or `host:port`
    pub computer: String,
    pub username: String,
    pub password: String,
    pub domain: String,
    pub width: String,
    pub height: String,
    /// Index into [`DEPTHS`]
    pub depth_index: usize,
    /// Force RDP 4 (otherwise RDP 5, falling back to RDP 4 automatically)
    pub rdp4: bool,
}

impl Default for Form {
    fn default() -> Self {
        let c = Config::default();
        Self {
            computer: String::new(),
            username: c.username,
            password: String::new(),
            domain: String::new(),
            width: c.width.to_string(),
            height: c.height.to_string(),
            depth_index: DEPTHS.iter().position(|&d| d == c.depth).unwrap_or(2),
            rdp4: false,
        }
    }
}

impl Form {
    pub fn to_config(&self) -> Result<Config, String> {
        let computer = self.computer.trim();
        if computer.is_empty() {
            return Err("Enter a computer name or address.".into());
        }
        let num = |s: &str, what: &str| -> Result<u32, String> {
            s.trim()
                .parse::<u32>()
                .ok()
                .filter(|n| (200..=8192).contains(n))
                .ok_or_else(|| format!("{what} must be a number between 200 and 8192."))
        };
        let (server, port) = vintrdp_core::parse_server(computer)?;
        Ok(Config {
            server,
            port,
            username: self.username.trim().to_string(),
            password: self.password.clone(),
            domain: self.domain.trim().to_string(),
            width: num(&self.width, "Width")?,
            height: num(&self.height, "Height")?,
            depth: DEPTHS[self.depth_index.min(DEPTHS.len() - 1)],
            version: if self.rdp4 { RdpVersion::V4 } else { RdpVersion::Auto },
            ..Config::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn form_to_config() {
        let f = Form { computer: "nt4:3390".into(), rdp4: true, ..Form::default() };
        let c = f.to_config().unwrap();
        assert_eq!((c.server.as_str(), c.port, c.version), ("nt4", 3390, RdpVersion::V4));
        assert!(Form { width: "12".into(), computer: "x".into(), ..Form::default() }.to_config().is_err());
        assert!(Form::default().to_config().is_err());
    }
}
