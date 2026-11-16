//! Independent process restarts; opposite-side identity is checked, never inferred.
use super::super::{process, world::World};
use serde_json::{Value, json};
use std::path::Path;

async fn identity(world: &mut World) -> Result<u64, String> {
    let host = world.host.as_mut().ok_or("restart: missing host")?;
    host.send(&json!({"action":"identity"})).await?;
    host.expect("host.ack").await?["pid"]
        .as_u64()
        .ok_or("restart: missing host identity".into())
}

pub async fn restart(world: &mut World, process: &str, bridge: &Path, root: &Path) -> Result<Value, String> {
    let host_before = identity(world).await?;
    let bridge_before = world
        .bridge
        .as_ref()
        .and_then(super::super::process::Process::id)
        .ok_or("restart: missing bridge identity")?;
    match process {
        "bridge" => {
            world.screen.take();
            let errors = world.close_bridge().await;
            if !errors.is_empty() {
                return Err(errors.join("; "));
            }
            world.history.push(std::mem::take(&mut world.trace));
            world.offset = 0;
            world.run = None;
            world.remove_private().await?;
            world
                .start_bridge(bridge, root, world.address.ok_or("restart: missing host endpoint")?)
                .await?;
            if identity(world).await? != host_before {
                return Err("restart: bridge restart changed host identity".into());
            }
            if world.bridge.as_ref().and_then(super::super::process::Process::id) == Some(bridge_before) {
                return Err("restart: bridge identity did not change".into());
            }
        }
        "hassmic-host" => restart_host(world, host_before, bridge_before).await?,
        _ => return Err("restart: unsupported process".into()),
    }
    Ok(json!({"process":process,"bridge_generation":world.generation}))
}

async fn restart_host(world: &mut World, previous: u64, bridge: u32) -> Result<(), String> {
    let host = world.host.as_mut().ok_or("restart: missing host")?;
    host.send(&json!({"action":"restart"})).await?;
    let address = process::admit_host(
        &mut world.endpoints,
        host.expect("host.ready"),
        process::readiness_deadline(),
        process::connect,
    )
    .await?;
    if Some(address) != world.address {
        return Err("restart: host endpoint changed".into());
    }
    let ack = host.expect("host.ack").await?;
    if ack["previous_pid"] != previous || ack["pid"].as_u64().is_none_or(|pid| pid == previous) {
        return Err("restart: host identity did not change after collection".into());
    }
    world.control(&json!({"action":"registered"})).await?;
    if world.bridge.as_ref().and_then(super::super::process::Process::id) != Some(bridge) {
        return Err("restart: host restart changed bridge identity".into());
    }
    Ok(())
}

/// Same ordered complete-wake oracle as W1, within the current bridge generation.
pub fn wake(world: &World) -> Result<(), String> {
    let selectors = super::super::selectors::parse(&[
        json!("wyoming.run_started"),
        json!("wyoming.mic_frames {count_gte: 10}"),
        json!("wyoming.run_ended"),
    ])?;
    super::super::selectors::check(&world.trace.records, &selectors, world.run.as_deref())
}

pub async fn group(
    world: &mut World,
    process: &str,
    count: u64,
    path: &Path,
    bridge: &Path,
    root: &Path,
) -> Result<(), String> {
    let data = super::super::observers::bytes(path, 4096, "restart say")?;
    for _ in 0..count {
        restart(world, process, bridge, root).await?;
        super::super::say(world, &data).await?;
        wake(world)?;
        world
            .restarts
            .push(json!({"process":process,"bridge_generation":world.generation,"run_id":world.run}));
    }
    Ok(())
}
