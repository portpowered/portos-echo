use std::{env, error::Error, ffi::OsStr, fs, io::Write, path::Path};

#[path = "build/android.rs"]
mod android;
#[path = "build/entry.rs"]
mod entry;
#[path = "build/notices.rs"]
mod notices;

const HOST: &[&str] = &["audio_file.c", "wake_none.c", "sound_none.c", "whisper_none.c"];
const ATHENA: &[&str] = &["audio_android.c", "wake_pryon.c", "sound_pryon.c", "whisper_pryon.c"];
const LOCAL: &[&str] = &["bluetooth.c", "minimp3.c", "services.c", "random.c"];

fn main() -> Result<(), Box<dyn Error>> {
    let manifest = env::var("CARGO_MANIFEST_DIR")?;
    let workspace = Path::new(&manifest).join("../..");
    let vendor = workspace.join("third_party/hassmic");
    let local = workspace.join("third_party/hassmic-stubs");
    let out = env::var("OUT_DIR")?;
    let out = Path::new(&out);
    let target = env::var("TARGET")?;
    let linux = env::var("CARGO_CFG_TARGET_OS")? == "linux";
    let athena = target == "armv7-linux-androideabi";
    if !(linux || athena) {
        return Err(format!("hassmic does not support {target}").into());
    }

    let pin_path = workspace.join("hassmic.pin");
    for path in [&vendor, &local, &pin_path] {
        writeln!(std::io::stdout(), "cargo:rerun-if-changed={}", path.display())?;
    }
    let pin = fs::read_to_string(pin_path)?;
    let commit = pin
        .lines()
        .find_map(|line| line.strip_prefix("commit="))
        .ok_or("missing hassmic pin")?;
    let build_id = commit.get(..7).ok_or("invalid hassmic pin")?;
    let version = fs::read_to_string(vendor.join("version"))?;
    let version = version.trim();
    writeln!(std::io::stdout(), "cargo:rustc-env=HASSMIC_VERSION={version}")?;
    let version = format!("\"{version}\"");
    let build_id = format!("\"{build_id}\"");
    let mut build = cc::Build::new();
    build
        .include(vendor.join("src/include"))
        .include(vendor.join("src/hassmic"))
        .define("VERSION", version.as_str())
        .define("BUILD", build_id.as_str())
        .flag("-std=gnu11")
        .opt_level(2);

    let main = entry::source(&vendor, out)?;
    notices::write(&vendor, &local, out)?;
    build
        .clone()
        .file(main)
        .define("main", "echo_hassmic_main")
        .try_compile("echo_hassmic_entry")?;
    compile_satellite(&mut build, &vendor, &local, if athena { HOST } else { ATHENA })?;
    if athena {
        android::wifi_dex(Path::new(&manifest), out)?;
        android::link(&vendor, out)?;
    } else {
        for library in ["pthread", "m", "dl"] {
            writeln!(std::io::stdout(), "cargo:rustc-link-lib={library}")?;
        }
    }
    Ok(())
}

fn compile_satellite(
    build: &mut cc::Build,
    vendor: &Path,
    local: &Path,
    other_backend: &[&str],
) -> Result<(), Box<dyn Error>> {
    let manifest = fs::read_to_string(vendor.join("sources.sha256"))?;
    for line in manifest.lines() {
        let (_, paths) = line.split_once("  ").ok_or("invalid hassmic inventory row")?;
        let path = Path::new(paths.split_once("  ").map_or(paths, |(_, stored)| stored));
        if path.extension() == Some(OsStr::new("c")) {
            let basename = path
                .file_name()
                .and_then(OsStr::to_str)
                .ok_or("missing hassmic filename")?;
            if !["main.c", "proto_wyoming.c"].contains(&basename) && !other_backend.contains(&basename) {
                build.file(vendor.join(path));
            }
        }
    }
    build
        .files(LOCAL.iter().map(|file| local.join(file)))
        .try_compile("echo_hassmic")?;
    Ok(())
}
