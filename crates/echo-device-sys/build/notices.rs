use std::{error::Error, fs, io::Write, path::Path};

pub fn write(vendor: &Path, local: &Path, out: &Path) -> Result<(), Box<dyn Error>> {
    let third_party = vendor.join("src/third_party");
    let mut output = fs::File::create(out.join("notices.txt"))?;
    let mit = fs::read_to_string(vendor.join("LICENSE"))?;
    writeln!(output, "hassmic\n{mit}")?;
    let shims = fs::read_to_string(local.join("LICENSE"))?;
    writeln!(output, "Port OS hassmic shims\n{shims}")?;
    let minimp3 = fs::read_to_string(third_party.join("minimp3.h"))?;
    let (_, cc0) = minimp3.split_once("/*").ok_or("missing minimp3 notice")?;
    let (cc0, _) = cc0.split_once("*/").ok_or("unterminated minimp3 notice")?;
    writeln!(output, "minimp3 (CC0-1.0)\n{}\n", cc0.trim())?;
    for name in ["monocypher.c", "monocypher.h"] {
        let source = fs::read_to_string(third_party.join(name))?;
        let notice = source
            .lines()
            .take_while(|line| line.starts_with("//"))
            .collect::<Vec<_>>()
            .join("\n");
        writeln!(output, "{name}\n{notice}\n")?;
    }
    let rnnoise = third_party.join("rnnoise");
    let copying = fs::read_to_string(rnnoise.join("COPYING"))?;
    writeln!(output, "RNNoise\n{copying}")?;
    let mut headers = std::collections::BTreeSet::new();
    for entry in fs::read_dir(rnnoise)? {
        let path = entry?.path();
        let source = fs::read_to_string(path)?;
        let leading = source
            .lines()
            .take_while(|line| !line.starts_with('#'))
            .collect::<Vec<_>>()
            .join("\n");
        if leading.contains("Copyright") && leading.contains("Redistribution") {
            headers.insert(leading);
        }
    }
    for header in headers {
        writeln!(output, "{}\n", header.trim())?;
    }
    Ok(())
}
