use std::{
    env,
    error::Error,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::Command,
};

pub fn link(vendor: &Path, out: &Path) -> Result<(), Box<dyn Error>> {
    let cc = compiler()?;
    let stubs = vendor.join("devices/athena/stubs");
    stand_in(&cc, &stubs, out, "libpryon", |_| true)?;
    // Keep the pinned inflate imports without exporting the fork's old linker
    // and unwind helpers into the Rust link. Workspace Opus already has a provider.
    stand_in(&cc, &stubs, out, "libz", |name| name.starts_with("inflate"))?;
    let mut cargo = std::io::stdout().lock();
    writeln!(cargo, "cargo:rustc-link-search=native={}", out.display())?;
    for lib in ["pryon", "z", "OpenSLES", "m", "dl"] {
        writeln!(cargo, "cargo:rustc-link-lib=dylib={lib}")?;
    }
    Ok(())
}

pub fn wifi_dex(manifest: &Path, out: &Path) -> Result<(), Box<dyn Error>> {
    let build = manifest.join("build/wifi");
    for name in ["EchoWifi.java", "dex.py"] {
        writeln!(
            std::io::stdout(),
            "cargo:rerun-if-changed={}",
            build.join(name).display()
        )?;
    }
    for name in ["ANDROID_HOME", "ANDROID_SDK_ROOT"] {
        writeln!(std::io::stdout(), "cargo:rerun-if-env-changed={name}")?;
    }
    let status = Command::new("python3").arg(build.join("dex.py")).arg(out).status()?;
    if !status.success() {
        return Err("could not build Android Wi-Fi helper".into());
    }
    Ok(())
}

fn compiler() -> Result<PathBuf, Box<dyn Error>> {
    for key in ["ANDROID_NDK_HOME", "ANDROID_NDK_LATEST_HOME"] {
        writeln!(std::io::stdout(), "cargo:rerun-if-env-changed={key}")?;
    }
    let ndk = env::var_os("ANDROID_NDK_HOME")
        .or_else(|| env::var_os("ANDROID_NDK_LATEST_HOME"))
        .ok_or("set ANDROID_NDK_HOME to the installed Linux NDK 27.3.13750724")?;
    let ndk = PathBuf::from(ndk);
    let properties = fs::read_to_string(ndk.join("source.properties"))?;
    if !properties.lines().any(|line| {
        line.split_once('=')
            .is_some_and(|(key, value)| key.trim() == "Pkg.Revision" && value.trim() == "27.3.13750724")
    }) {
        return Err("hassmic requires Android NDK 27.3.13750724".into());
    }
    Ok(ndk.join("toolchains/llvm/prebuilt/linux-x86_64/bin/armv7a-linux-androideabi24-clang"))
}

fn stand_in(
    compiler: &Path,
    manifests: &Path,
    out: &Path,
    library: &str,
    include: impl Fn(&str) -> bool,
) -> Result<(), Box<dyn Error>> {
    let manifest = fs::read_to_string(manifests.join(format!("{library}.syms")))?;
    let asm = out.join(format!("{library}.s"));
    let mut file = fs::File::create(&asm)?;
    writeln!(file, ".syntax unified\n.arch armv7-a\n.text")?;
    for line in manifest.lines() {
        let (kind, name) = line.split_once(' ').ok_or("invalid stub declaration")?;
        if name.is_empty() || !name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_') {
            return Err(format!("invalid stub symbol `{name}`").into());
        }
        if include(name) {
            match kind {
                "T" => writeln!(file, ".globl {name}\n.type {name}, %function\n{name}: bx lr")?,
                "U" => writeln!(file, ".pushsection .data\n.word {name}\n.popsection")?,
                _ => return Err(format!("invalid stub declaration `{line}`").into()),
            }
        }
    }
    let soname = format!("{library}.so");
    let status = Command::new(compiler)
        .args([
            "-shared",
            "-nostdlib",
            "-Wl,--hash-style=sysv",
            &format!("-Wl,-soname,{soname}"),
        ])
        .arg(asm)
        .arg("-o")
        .arg(out.join(&soname))
        .status()?;
    if !status.success() {
        return Err(format!("cannot generate {soname}: {status}").into());
    }
    Ok(())
}
