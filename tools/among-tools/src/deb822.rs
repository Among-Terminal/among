use crate::Result;
use std::collections::BTreeMap;

pub fn fields(text: &str) -> Result<BTreeMap<String, String>> {
    let normalized = text.replace("\r\n", "\n");
    let cleartext;
    let text = if normalized.starts_with("-----BEGIN PGP SIGNED MESSAGE-----\n") {
        // Parsing signed cleartext does not authenticate it. Only a verifier
        // against the pinned archive key can supply signature trust.
        let (_, body) = normalized
            .split_once("\n\n")
            .ok_or("malformed signed cleartext")?;
        let (body, _) = body
            .split_once("\n-----BEGIN PGP SIGNATURE-----")
            .ok_or("signed cleartext has no signature boundary")?;
        cleartext = body
            .lines()
            .map(|line| line.strip_prefix("- ").unwrap_or(line))
            .collect::<Vec<_>>()
            .join("\n");
        cleartext.as_str()
    } else {
        normalized.as_str()
    };
    let mut fields: BTreeMap<String, String> = BTreeMap::new();
    let mut previous = None;
    for line in text.lines() {
        if line.is_empty() {
            break;
        }
        if line.starts_with([' ', '\t']) {
            let name = previous
                .as_ref()
                .ok_or("orphan Debian field continuation")?;
            let field = fields.get_mut(name).ok_or("missing continued field")?;
            field.push('\n');
            field.push_str(line.trim_start());
            continue;
        }
        let (name, value) = line.split_once(':').ok_or("malformed Debian field")?;
        if name.is_empty()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err("invalid Debian field name".into());
        }
        let name = name.to_ascii_lowercase();
        if fields
            .insert(name.clone(), value.trim_start().to_string())
            .is_some()
        {
            return Err("duplicate Debian field".into());
        }
        previous = Some(name);
    }
    Ok(fields)
}

pub fn required<'a>(fields: &'a BTreeMap<String, String>, name: &str) -> Result<&'a str> {
    fields
        .get(&name.to_ascii_lowercase())
        .map(String::as_str)
        .ok_or_else(|| format!("missing Debian field: {name}").into())
}

pub fn checksum(line: &str) -> Result<(&str, u64, &str)> {
    let fields = line.split_whitespace().collect::<Vec<_>>();
    if fields.len() != 3
        || fields[0].len() != 64
        || !fields[0]
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        || !fields[1].bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err("invalid SHA-256 checksum record".into());
    }
    Ok((fields[0], fields[1].parse()?, fields[2]))
}
