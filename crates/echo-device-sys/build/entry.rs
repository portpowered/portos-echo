use std::{
    error::Error,
    fs,
    path::{Path, PathBuf},
};

// Keep the pinned source available for comparisons. These build-only changes
// stop the shared dispatcher from selecting excluded optional services.
pub fn source(vendor: &Path, out: &Path) -> Result<PathBuf, Box<dyn Error>> {
    let mut source = fs::read_to_string(vendor.join("src/hassmic/main.c"))?;
    for (upstream, satellite) in [
        (
            "static const struct proto *proto = &proto_esphome;",
            "static const struct proto *proto = &proto_wyoming;",
        ),
        (
            "case 'P': proto = !strcmp(optarg, \"wyoming\") ? &proto_wyoming : &proto_esphome; break;",
            "case 'P': if (strcmp(optarg, \"wyoming\")) { fputs(\"hassmic: only Wyoming is available\\n\", stderr); return 2; } break;",
        ),
        (
            "case 'T': { char tok[160]; sendspin_init(); sendspin_pairing_token(tok, sizeof tok); puts(tok); return 0; }",
            "case 'T': fputs(\"hassmic: Sendspin is unavailable\\n\", stderr); return 2;",
        ),
        ("static int ota_port = 28929;", "static int ota_port = 0;"),
        ("int core_web_port = 28931;", "int core_web_port = 0;"),
        ("core_sendspin_port = 28928;", "core_sendspin_port = 0;"),
    ] {
        if source.matches(upstream).count() != 1 {
            return Err(format!("hassmic main no longer contains exactly one `{upstream}`").into());
        }
        source = source.replacen(upstream, satellite, 1);
    }
    let main = out.join("hassmic_main.c");
    fs::write(&main, source)?;
    Ok(main)
}
