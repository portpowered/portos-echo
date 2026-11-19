use serde_json::Value;

#[derive(Clone, Copy)]
pub(super) enum Request<'a> {
    List(Option<&'a str>),
    Read(&'a str),
    Write(&'a str, &'a str),
}

impl<'a> Request<'a> {
    pub(super) fn decode(name: &str, args: &'a Value) -> Result<Self, &'static str> {
        let object = args.as_object().ok_or("invalid_arguments")?;
        match name {
            "notes_list" if object.is_empty() => Ok(Self::List(None)),
            "notes_list" if object.len() == 1 && object.contains_key("directory") => {
                let path = object["directory"].as_str().ok_or("invalid_arguments")?;
                validate(path, false)?;
                Ok(Self::List(Some(path)))
            }
            "notes_read" if object.len() == 1 && object.contains_key("path") => {
                let path = object["path"].as_str().ok_or("invalid_arguments")?;
                validate(path, true)?;
                Ok(Self::Read(path))
            }
            "notes_write" if object.len() == 2 && object.contains_key("path") && object.contains_key("content") => {
                let path = object["path"].as_str().ok_or("invalid_arguments")?;
                let content = object["content"].as_str().ok_or("invalid_arguments")?;
                validate(path, true)?;
                if content.len() as u64 > super::NOTE_BYTES {
                    return Err("note_too_large");
                }
                Ok(Self::Write(path, content))
            }
            _ => Err("invalid_arguments"),
        }
    }
}

pub(super) fn validate(path: &str, file: bool) -> Result<(), &'static str> {
    let parts: Vec<_> = path.split('/').collect();
    if parts.len() > if file { 4 } else { 3 }
        || parts.iter().any(|part| !segment(part))
        || (file && path.rsplit_once('.').is_none_or(|(_, extension)| extension != "md"))
    {
        return Err("invalid_path");
    }
    Ok(())
}

pub(super) fn segment(value: &str) -> bool {
    !matches!(value, "" | "." | "..")
        && value.len() <= 255
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"._-".contains(&byte))
}
