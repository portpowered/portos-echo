//! Authenticate the public APK before decoding its single pinned native member.
use crate::{Failure, Result, integrity::digest, release};
use flate2::bufread::DeflateDecoder;
use std::io::Read;

const URL: &str = "https://github.com/topjohnwu/Magisk/releases/download/v30.7/Magisk-v30.7.apk";
const ARCHIVE: &str = "e0d32d2123532860f97123d927b1bb86c4e08e6fd8a48bfc6b5bee0afae9ebd5";
const MEMBER: &[u8] = b"lib/armeabi-v7a/libmagiskpolicy.so";
macro_rules! pin {
    ($digest:literal) => {
        const SHA256: &str = $digest;
        pub(super) const CHECKSUM_FIX: &str = concat!("Expected SHA-256 ", $digest, ".");
    };
}
pin!("e5fafc1fa9486950ce9ba476f5723754d260014515ced72b4560238e6c560f3a");
const LIMIT: usize = 1024 * 1024;
const INVALID: Failure = Failure("magiskpolicy_archive_invalid");

pub(super) async fn fetch() -> Result<Vec<u8>> {
    let url = reqwest::Url::parse(URL).map_err(|_| INVALID)?;
    let apk = release::download(&release::client()?, url, false, 64 * 1024 * 1024)
        .await
        .map_err(|_| Failure("magiskpolicy_download_failed"))?;
    verify(&apk, ARCHIVE, SHA256)
}

fn verify(apk: &[u8], archive_digest: &str, tool_digest: &str) -> Result<Vec<u8>> {
    if digest(apk) != archive_digest {
        return Err(Failure("magiskpolicy_archive_checksum_mismatch"));
    }
    let member = member(apk).ok_or(INVALID)?;
    if digest(&member) != tool_digest {
        return Err(Failure("magiskpolicy_checksum_mismatch"));
    }
    Ok(member)
}

fn number<const N: usize>(bytes: &[u8], start: usize) -> Option<[u8; N]> {
    bytes.get(start..start.checked_add(N)?)?.try_into().ok()
}

fn short(bytes: &[u8], start: usize) -> Option<usize> {
    Some(usize::from(u16::from_le_bytes(number(bytes, start)?)))
}

fn long(bytes: &[u8], start: usize) -> Option<usize> {
    usize::try_from(u32::from_le_bytes(number(bytes, start)?)).ok()
}

fn directory(apk: &[u8]) -> Option<(&[u8], usize, usize)> {
    let first = apk.len().saturating_sub(22 + usize::from(u16::MAX));
    let tail = apk.get(first..)?;
    let offset = tail.windows(4).rposition(|bytes| bytes == b"PK\x05\x06")? + first;
    let end = apk.get(offset..)?;
    let count = short(end, 10)?;
    if end.len() != 22 + short(end, 20)?
        || long(end, 4)? != 0
        || short(end, 8)? != count
        || count == usize::from(u16::MAX)
    {
        return None;
    }
    let start = long(end, 16)?;
    if start.checked_add(long(end, 12)?)? != offset {
        return None;
    }
    Some((apk.get(start..offset)?, start, count))
}

fn member(apk: &[u8]) -> Option<Vec<u8>> {
    let (mut entries, start, count) = directory(apk)?;
    let mut found = None;
    for _ in 0..count {
        if !entries.starts_with(b"PK\x01\x02") || short(entries, 34)? != 0 {
            return None;
        }
        let name_end = 46 + short(entries, 28)?;
        let name = entries.get(46..name_end)?;
        let end = name_end + short(entries, 30)? + short(entries, 32)?;
        if name == MEMBER {
            if found.is_some() {
                return None;
            }
            found = Some(extract(apk.get(..start)?, entries.get(..end)?)?);
        }
        entries = entries.get(end..)?;
    }
    if entries.is_empty() { found } else { None }
}

fn extract(locals: &[u8], entry: &[u8]) -> Option<Vec<u8>> {
    let offset = long(entry, 42)?;
    let local = locals.get(offset..)?;
    let flags = short(entry, 8)?;
    let size = long(entry, 24)?;
    let compressed = long(entry, 20)?;
    let name_end = 30 + short(local, 26)?;
    let data = name_end + short(local, 28)?;
    if !local.starts_with(b"PK\x03\x04")
        || flags & !0x080e != 0
        || short(local, 6)? != flags
        || short(entry, 10)? != 8
        || short(local, 8)? != 8
        || local.get(30..name_end)? != MEMBER
        || size == 0
        || size > LIMIT
        || compressed == 0
        || compressed > LIMIT
        || (flags & 8 == 0
            && (long(local, 14)? != long(entry, 16)? || long(local, 18)? != compressed || long(local, 22)? != size))
    {
        return None;
    }
    let input = local.get(data..data.checked_add(compressed)?)?;
    let mut decoder = DeflateDecoder::new(input);
    let mut output = Vec::new();
    (&mut decoder).take((size + 1) as u64).read_to_end(&mut output).ok()?;
    (output.len() == size && decoder.total_in() == compressed as u64).then_some(output)
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use std::io::Write;

    fn apk(payload: &[u8], copies: u16) -> Vec<u8> {
        let mut writer = flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::fast());
        writer.write_all(payload).unwrap();
        let compressed = writer.finish().unwrap();
        let size = u32::try_from(payload.len()).unwrap().to_le_bytes();
        let packed = u32::try_from(compressed.len()).unwrap().to_le_bytes();
        let name = u16::try_from(MEMBER.len()).unwrap().to_le_bytes();
        let mut local = vec![0; 30];
        local[..4].copy_from_slice(b"PK\x03\x04");
        local[8] = 8;
        local[18..22].copy_from_slice(&packed);
        local[22..26].copy_from_slice(&size);
        local[26..28].copy_from_slice(&name);
        local.extend_from_slice(MEMBER);
        local.extend(compressed);

        let mut entry = vec![0; 46];
        entry[..4].copy_from_slice(b"PK\x01\x02");
        entry[10] = 8;
        entry[20..24].copy_from_slice(&packed);
        entry[24..28].copy_from_slice(&size);
        entry[28..30].copy_from_slice(&name);
        entry.extend_from_slice(MEMBER);
        let offset = u32::try_from(local.len()).unwrap();
        for _ in 0..copies {
            local.extend_from_slice(&entry);
        }
        let length = u32::try_from(local.len()).unwrap() - offset;
        let mut end = [0; 22];
        end[..4].copy_from_slice(b"PK\x05\x06");
        end[8..10].copy_from_slice(&copies.to_le_bytes());
        end[10..12].copy_from_slice(&copies.to_le_bytes());
        end[12..16].copy_from_slice(&length.to_le_bytes());
        end[16..20].copy_from_slice(&offset.to_le_bytes());
        local.extend(end);
        local
    }

    pub(in crate::install) fn fixture(archive_matches: bool, member_matches: bool) -> Result<Vec<u8>> {
        let content = b"controlled ARM32 policy asset";
        let apk = apk(content, 1);
        let archived = if archive_matches {
            digest(&apk)
        } else {
            ARCHIVE.to_owned()
        };
        let expected = if member_matches {
            digest(content)
        } else {
            SHA256.to_owned()
        };
        verify(&apk, &archived, &expected)
    }

    #[test]
    fn a2_policy_authenticates_the_container_before_extracting_the_member() {
        let public = b"controlled ARM32 policy asset";
        let archive = apk(public, 1);
        assert_eq!(
            verify(&archive, &digest(&archive), &digest(public)).as_deref(),
            Ok(public.as_slice())
        );
        assert_eq!(
            verify(b"not a ZIP", &digest(&archive), SHA256),
            Err(Failure("magiskpolicy_archive_checksum_mismatch"))
        );
        assert_eq!(
            verify(&archive, &digest(&archive), SHA256),
            Err(Failure("magiskpolicy_checksum_mismatch"))
        );
        assert_eq!(CHECKSUM_FIX, format!("Expected SHA-256 {SHA256}."));
    }

    #[test]
    fn a2_policy_rejects_duplicate_names_and_invalid_directory_or_member_offsets() {
        let data = apk(b"controlled policy", 2);
        assert_eq!(verify(&data, &digest(&data), SHA256), Err(INVALID));
        let data = apk(b"controlled policy", 1);
        let end = data.len() - 22;
        let entry = long(&data, end + 16).unwrap();
        for index in [end + 16, entry + 42, entry + 20] {
            let mut malformed = data.clone();
            malformed[index..index + 4].fill(255);
            assert_eq!(verify(&malformed, &digest(&malformed), SHA256), Err(INVALID));
        }
    }
}
