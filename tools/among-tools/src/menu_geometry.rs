use crate::{storage, Result};

pub type Bounds = [i64; 4];

fn tuples_to_json(text: &str) -> Result<String> {
    let mut quoted = false;
    let mut escaped = false;
    let mut output = String::new();
    for character in text.chars() {
        if quoted {
            output.push(character);
            if escaped {
                escaped = false;
            } else if character == '\\' {
                escaped = true;
            } else if character == '"' {
                quoted = false;
            }
        } else {
            match character {
                '"' => {
                    quoted = true;
                    output.push(character);
                }
                '(' => output.push('['),
                ')' => output.push(']'),
                _ => output.push(character),
            }
        }
    }
    if quoted {
        return Err("incomplete quoted menu entry".into());
    }
    Ok(output)
}

pub fn row_samples(text: &str, pane: u64, label: &str) -> Result<Vec<Option<Bounds>>> {
    let mut samples = Vec::new();
    for line in text.split_inclusive('\n') {
        if !line.ends_with('\n') || !line.starts_with("among[") {
            continue;
        }
        let Some((_, value)) = line.split_once("]: menu_items=") else {
            continue;
        };
        let json = storage::parse_json(tuples_to_json(value.trim_end())?.as_bytes())?;
        let rows = json.as_array().ok_or("menu rows are not an array")?;
        let mut bounds = None;
        for row in rows {
            let row = row
                .as_array()
                .filter(|row| row.len() == 2)
                .ok_or("invalid menu row")?;
            let target = row[0].as_u64().ok_or("invalid menu target")?;
            let entries = row[1].as_array().ok_or("invalid menu entries")?;
            for entry in entries {
                let entry = entry.as_str().ok_or("invalid menu entry")?;
                if target != pane {
                    continue;
                }
                let Some(value) = entry.strip_prefix(&format!("{label}:true:")) else {
                    continue;
                };
                let Some((x, value)) = value.split_once('+') else {
                    continue;
                };
                let Some((y, value)) = value.split_once('+') else {
                    continue;
                };
                let Some((width, height)) = value.split_once('x') else {
                    continue;
                };
                if let (Ok(x), Ok(y), Ok(width), Ok(height)) = (
                    x.parse::<i64>(),
                    y.parse::<i64>(),
                    width.parse::<i64>(),
                    height.parse::<i64>(),
                ) {
                    if width > 0 && height > 0 {
                        bounds = Some([x, y, width, height]);
                    }
                }
            }
        }
        samples.push(bounds);
    }
    Ok(samples)
}

pub fn settled_row(text: &str, pane: u64, label: &str) -> Result<Option<Bounds>> {
    let samples = row_samples(text, pane, label)?;
    Ok(
        if samples.len() >= 2 && samples.last() == samples.get(samples.len() - 2) {
            samples.last().copied().flatten()
        } else {
            None
        },
    )
}
