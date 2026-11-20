//! Read only the installed config and the one screen credential. Never open
//! account, grant or Codex files, and never accept a host path as a device path.
use super::wake::bounded;
use crate::{Failure, Result, adb::Adb, cli::Options};
use echo_contracts::secret::Secret;
use serde::Deserialize;
use tokio::time::Instant;

/// Device selection is retained across config/pairing reads. Neither private
/// bytes nor the selected identity implement Debug/Display through this owner.
pub struct InstalledConnection {
    adb: Adb,
    screen: u16,
    deadline: Instant,
}

impl InstalledConnection {
    /// Read the authored config without loading any referenced credentials.
    /// # Errors
    /// Fixed selection, config, private-file or absolute deadline categories.
    pub async fn read(options: &Options, deadline: Instant) -> Result<Self> {
        bounded(deadline, async {
            let mut adb = Adb::discover(options.adb.as_deref())?;
            adb.select(options.serial.as_deref()).await?;
            let raw = private_read(&adb, "setup-config").await?;
            let screen = screen_port(raw.expose())?;
            Ok(Self { adb, screen, deadline })
        })
        .await
    }

    /// Shell launch supplies no health override, so the bridge uses 16901.
    #[must_use]
    pub fn remote_ports(&self) -> [u16; 2] {
        [self.screen, 16901]
    }

    /// Allocate on the already selected device, even if discovery changes.
    /// # Errors
    /// Fixed forward, cleanup or deadline categories.
    pub async fn forward(&self) -> Result<super::ForwardSession> {
        super::ForwardSession::selected(self.adb.clone(), self.remote_ports(), self.deadline).await
    }

    /// Call after the bridge's screen owner creates its pairing file. Absence
    /// is a refusal, never permission to generate/replace a credential here.
    /// # Errors
    /// Fixed private-file, pairing or deadline categories; never file bytes.
    pub async fn pairing(&self) -> Result<Secret<String>> {
        bounded(self.deadline, async {
            let raw = private_read(&self.adb, "setup-pairing").await?;
            let value = raw.expose().trim();
            if value.is_empty() || value.len() > 16_384 || value.chars().any(char::is_control) {
                return Err(Failure("setup_pairing_invalid"));
            }
            Ok(Secret::new(value.to_owned()))
        })
        .await
    }
}

async fn private_read(adb: &Adb, operation: &str) -> Result<Secret<String>> {
    let (success, output) = adb.script(operation).await?;
    if !success {
        return Err(Failure("setup_private_unavailable"));
    }
    Ok(Secret::new(output))
}

fn screen_port(raw: &str) -> Result<u16> {
    // Reuse the bridge's locked TOML decoder, but never run its host-native
    // Path::is_absolute checks against Android paths on a Windows computer.
    // This is a screen projection, not validation of the full runtime config.
    let config: Config = toml::from_str(raw).map_err(|_| Failure("setup_config_invalid"))?;
    if config.screen.listen.ip() != std::net::Ipv4Addr::LOCALHOST
        || config.screen.listen.port() == 0
        || config.screen.pairing_token_file != "/data/local/echo-bridge/private/screen-pairing.txt"
    {
        return Err(Failure("setup_config_invalid"));
    }
    Ok(config.screen.listen.port())
}

#[derive(Deserialize)]
struct Config {
    screen: Screen,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Screen {
    #[serde(default = "default_listen")]
    listen: std::net::SocketAddr,
    pairing_token_file: String,
}

fn default_listen() -> std::net::SocketAddr {
    (std::net::Ipv4Addr::UNSPECIFIED, 16900).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn installed_config_projects_only_loopback_screen_and_fixed_pairing() {
        let base = include_str!("../../../../examples/bridge.echo.toml");
        assert_eq!(screen_port(base), Ok(16900));
        assert_eq!(screen_port(&base.replace("16900", "17300")), Ok(17300));
        for raw in [
            base.replace("127.0.0.1:16900", "0.0.0.0:16900"), // lint-rules: allow-literal inert rejected config fixture
            base.replace("127.0.0.1:16900", "127.0.0.1:0"),
            base.replace("screen-pairing.txt", "codex-home/auth.json"),
            base.replace("screen-pairing.txt", "../private/screen-pairing.txt"),
            format!("{base}\nSYNTHETIC-PRIVATE-PATH"),
            base.replace("listen =", "token = 'SYNTHETIC-TOKEN'\nlisten ="),
        ] {
            assert_eq!(screen_port(&raw), Err(Failure("setup_config_invalid")));
        }
    }
}
