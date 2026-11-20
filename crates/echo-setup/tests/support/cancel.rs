//! T3 actual Unix terminal interruption; Windows uses the same cancellation owner.
use super::*;
use tokio::io::{AsyncBufReadExt, AsyncReadExt};

#[tokio::test]
async fn t3_ctrl_c_settles_each_provider_and_drains_stage_before_terminal_result() -> Result {
    tokio::task::LocalSet::new()
        .run_until(async {
            for portos in [false, true] {
                for json in [false, true] {
                    tokio::time::timeout(std::time::Duration::from_secs(10), interrupted(portos, json)).await??;
                }
            }
            Ok(())
        })
        .await
}

async fn interrupted(portos: bool, json: bool) -> Result {
    let fixture = Fixture::new()?;
    let (mut child, screen, barrier) = start_prompted(&fixture, json).await?;
    let mut lines = tokio::io::BufReader::new(child.stdout.take().ok_or("missing stdout")?).lines();
    if portos {
        finish_signin(&mut lines, &screen, &barrier, json).await?;
        settings_reply(&screen).await?;
    }
    let (mut account, _) = barrier.accept().await?;
    let mut ready = [0; 5];
    account.read_exact(&mut ready).await?;
    assert_eq!(&ready, b"ready");
    let server = tokio::task::spawn_local(async move {
        screen_prompt(&screen, portos, false)
            .await
            .map_err(|error| error.to_string())
    });
    check_prompt(&lines.next_line().await?.ok_or("missing prompt")?, json, portos)?;
    assert!(
        std::process::Command::new("kill")
            .args(["-INT", &child.id().ok_or("missing child")?.to_string()])
            .status()?
            .success()
    );
    let line = lines.next_line().await?;
    if json {
        let result: serde_json::Value = serde_json::from_str(line.as_deref().ok_or("missing result")?)?;
        assert_eq!(result["ok"], false);
        assert_eq!(result["error"], "setup_cancelled");
        assert!(result["setup"].is_null());
        assert_public(&result.to_string());
    } else {
        assert_eq!(line, None);
    }
    assert_eq!(lines.next_line().await?, None);
    let output = child.wait_with_output().await?;
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8(output.stderr)?;
    assert_public(&stderr);
    assert_eq!(stderr.contains("Setup cancelled. Installed files remain."), !json);
    server.await?.map_err(io::Error::other)?;
    expired_cleanup(&fixture)
}
