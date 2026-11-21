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
    pub(super) adb: Adb,
    screen: u16,
    pub(super) deadline: Instant,
    account: super::account::AccountOwner,
    launch: Option<(String, String)>,
}

impl InstalledConnection {
    /// Read the authored config without loading any referenced credentials.
    /// # Errors
    /// Fixed selection, config, private-file or absolute deadline categories.
    pub async fn read(options: &Options, deadline: Instant) -> Result<Self> {
        bounded(deadline, async {
            let mut adb = Adb::discover(options.adb.as_deref())?;
            adb.select(options.serial.as_deref()).await?;
            Self::selected(adb, deadline, None).await
        })
        .await
    }

    pub(crate) async fn selected(adb: Adb, deadline: Instant, launch: Option<(String, String)>) -> Result<Self> {
        bounded(deadline, async {
            let raw = private_read(&adb, "setup-config").await?;
            Ok(Self {
                adb,
                screen: screen_port(raw.expose())?,
                deadline,
                account: super::account::AccountOwner::default(),
                launch,
            })
        })
        .await
    }

    pub(super) async fn start(&self) -> Result<()> {
        bounded(self.deadline, async {
            let version = if let Some((command, version)) = &self.launch {
                self.adb.shell(command).await.map_err(|_| Failure("start_failed"))?;
                Some(version.as_str())
            } else {
                if !self.adb.script("start").await?.0 {
                    return Err(Failure("start_failed"));
                }
                None
            };
            crate::lifecycle::require_started(&self.adb, version).await
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

    /// Drive the existing shell account owner on this same selected device.
    /// Poll concurrently with screen observation to present the public prompt.
    /// The absolute connection deadline also bounds the local adb child. Drop
    /// cancels that child; it does not establish remote command termination.
    /// Cancellation/failure refuses further account commands until
    /// `settle_account` verifies the stopped remote boundary. The guided setup
    /// owner must await that result before claiming terminal cleanup.
    /// # Errors
    /// Fixed account/deadline categories, never account labels or command logs.
    pub async fn account(&self, command: super::AccountCommand) -> Result<()> {
        self.account.run(&self.adb, command, self.deadline).await
    }

    /// Present only authenticated public prompts while awaiting the authored
    /// account completion frame. Returned failures settle ambiguous account
    /// ownership and drain owned forwards before reaching the caller.
    /// Dropping this future cancels local work and releases forwards; the caller
    /// must still call `settle_account` before claiming remote cleanup.
    /// # Errors
    /// Fixed account, observation, presentation or cleanup categories. Cleanup
    /// refusals take priority over the original failure; never raw device bytes.
    pub async fn guide_account(
        &self,
        command: super::AccountCommand,
        now_unix: u64,
        present: impl FnMut(&super::Prompt) -> Result<()>,
    ) -> Result<()> {
        super::account::guide(
            self,
            command,
            self.deadline,
            now_unix,
            present,
            &crate::Cancellation::default(),
        )
        .await
        .map_err(|error| command.context(error))
    }

    pub(super) async fn recover_account(&self) -> Result<()> {
        if self.account.unsettled() {
            let cleanup = Instant::now() + std::time::Duration::from_secs(10);
            self.settle_account(cleanup).await.map_err(|error| {
                if error == Failure("setup_timeout") {
                    Failure("setup_account_unsettled")
                } else {
                    error
                }
            })?;
        }
        Ok(())
    }

    /// Settle an ambiguous/cancelled account attempt through the existing stop
    /// owner on the retained device. This deliberately stops a running bridge.
    /// The shell must acquire its lifecycle lock and prove bridge absence; an
    /// active remote signin, stale lock or unsafe process therefore refuses.
    /// Use a separate bounded cleanup deadline after the setup deadline expires.
    /// Do not start another screen/Codex owner unless this succeeds. A new
    /// connection is not a substitute for settlement of this connection.
    /// # Errors
    /// Fixed busy, unsettled or deadline categories, without dependency output.
    pub async fn settle_account(&self, deadline: Instant) -> Result<()> {
        self.account.settle(&self.adb, deadline).await
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
    // The bridge owns optional screen packs. Setup only needs the local listener and pairing path.
    #[serde(rename = "packs", default)]
    _packs: serde::de::IgnoredAny,
    // Lip-sync timing is the bridge's too.
    #[serde(rename = "mouth_delay_ms", default)]
    _mouth_delay_ms: serde::de::IgnoredAny,
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
