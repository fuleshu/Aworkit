//! Reversible, collision-free representations. Format metadata lives outside
//! source data; arbitrary user keys can never masquerade as format controls.
use serde_json::{Value, json};
use std::collections::BTreeSet;

pub fn table(value: &Value) -> Option<Value> {
    let mut data = value.clone();
    let mut tables = Vec::new();
    pack(&mut data, "", &mut tables, 0);
    if tables.is_empty() {
        return None;
    }
    let packed = json!({"format":"aworkit.table.v1","data":data,"tables":tables,
        "decode":"At each pointer, rows map to columns; merge constants; missing lists [row,column] absent keys. Apply tables in reverse order."});
    (unpack(&packed).as_ref() == Some(value)).then_some(packed)
}

fn escape(key: &str) -> String {
    key.replace('~', "~0").replace('/', "~1")
}

fn pack(value: &mut Value, path: &str, tables: &mut Vec<Value>, depth: usize) {
    if depth > 16 || tables.len() >= 256 {
        return;
    }
    match value {
        Value::Object(map) => {
            for (key, child) in map.iter_mut() {
                pack(child, &format!("{path}/{}", escape(key)), tables, depth + 1);
            }
        }
        Value::Array(rows) => {
            for (i, child) in rows.iter_mut().enumerate() {
                pack(child, &format!("{path}/{i}"), tables, depth + 1);
            }
            if rows.len() < 4 || rows.len() > 8192 || !rows.iter().all(Value::is_object) {
                return;
            }
            let keys: BTreeSet<_> = rows
                .iter()
                .flat_map(|row| row.as_object().unwrap().keys().cloned())
                .collect();
            if keys.len() > 64 || keys.is_empty() {
                return;
            }
            let mut constants = serde_json::Map::new();
            let mut columns = Vec::new();
            for key in keys {
                if rows[0].get(&key).is_some()
                    && rows.iter().all(|r| r.get(&key) == rows[0].get(&key))
                {
                    constants.insert(key.clone(), rows[0][&key].clone());
                } else {
                    columns.push(key);
                }
            }
            let mut missing = Vec::new();
            let dense: Vec<Value> = rows
                .iter()
                .enumerate()
                .map(|(i, row)| {
                    Value::Array(
                        columns
                            .iter()
                            .enumerate()
                            .map(|(j, key)| match row.get(key) {
                                Some(v) => v.clone(),
                                None => {
                                    missing.push(json!([i, j]));
                                    Value::Null
                                }
                            })
                            .collect(),
                    )
                })
                .collect();
            let entry =
                json!({"pointer":path,"columns":columns,"constants":constants,"missing":missing});
            if json!(&dense).to_string().len() + entry.to_string().len() >= value.to_string().len()
            {
                return;
            }
            *value = json!(dense);
            tables.push(entry);
        }
        _ => {}
    }
}

/// Decoder is also the admission validator, including absent-vs-null values.
pub fn unpack(packed: &Value) -> Option<Value> {
    let mut data = packed.get("data")?.clone();
    for table in packed.get("tables")?.as_array()?.iter().rev() {
        let value = data.pointer_mut(table["pointer"].as_str()?)?;
        let missing: BTreeSet<(u64, u64)> = table["missing"]
            .as_array()?
            .iter()
            .map(|cell| Some((cell[0].as_u64()?, cell[1].as_u64()?)))
            .collect::<Option<_>>()?;
        let mut rows = Vec::new();
        for (i, row) in value.as_array()?.iter().enumerate() {
            let mut object = table["constants"].as_object()?.clone();
            let cells = row.as_array()?;
            for (j, key) in table["columns"].as_array()?.iter().enumerate() {
                if !missing.contains(&(i as u64, j as u64)) {
                    object.insert(key.as_str()?.into(), cells.get(j)?.clone());
                }
            }
            rows.push(Value::Object(object));
        }
        *value = json!(rows);
    }
    Some(data)
}

/// Alternating whitespace/nonwhitespace tokens retain all source bytes.
fn pieces(line: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut prior = None;
    for (i, c) in line.char_indices() {
        let space = c.is_whitespace();
        if prior.is_some_and(|p| p != space) {
            out.push(&line[start..i]);
            start = i;
        }
        prior = Some(space);
    }
    if start < line.len() {
        out.push(&line[start..]);
    }
    out
}

pub fn templates(text: &str) -> Option<Value> {
    let lines: Vec<_> = text.split_inclusive('\n').collect();
    if lines.len() < 8 {
        return None;
    }
    let mut segments = Vec::new();
    let mut i = 0;
    let mut changed = false;
    while i < lines.len() {
        let first = pieces(lines[i]);
        if first.len() > 128 {
            segments.push(json!(lines[i]));
            i += 1;
            continue;
        }
        let mut template: Vec<Option<&str>> = first.iter().copied().map(Some).collect();
        let mut rows = vec![first.clone()];
        let mut end = i + 1;
        while end < lines.len() && end - i < 512 {
            let row = pieces(lines[end]);
            if row.len() != first.len() {
                break;
            }
            let next: Vec<_> = template
                .iter()
                .zip(&row)
                .map(|(a, b)| a.filter(|v| v == b))
                .collect();
            let anchors = next
                .iter()
                .flatten()
                .filter(|v| !v.trim().is_empty())
                .count();
            if anchors < 2
                || anchors * 5 < first.iter().filter(|v| !v.trim().is_empty()).count() * 2
            {
                break;
            }
            template = next;
            rows.push(row);
            end += 1;
        }
        if rows.len() >= 4 {
            let variables: Vec<Vec<_>> = rows
                .iter()
                .map(|row| {
                    row.iter()
                        .zip(&template)
                        .filter_map(|(v, t)| t.is_none().then_some(*v))
                        .collect()
                })
                .collect();
            segments.push(json!({"template":template,"values":variables}));
            changed = true;
            i = end;
        } else {
            segments.push(json!(lines[i]));
            i += 1;
        }
    }
    let result = json!({"format":"aworkit.log.v1","decode":"Each row replaces null slots in template in order; concatenate tokens and segments exactly.","segments":segments});
    (changed && expand_templates(&result).as_deref() == Some(text)).then_some(result)
}

pub fn expand_templates(value: &Value) -> Option<String> {
    let mut result = String::new();
    for segment in value["segments"].as_array()? {
        if let Some(text) = segment.as_str() {
            result.push_str(text);
            continue;
        }
        for row in segment["values"].as_array()? {
            let mut cells = row.as_array()?.iter();
            for token in segment["template"].as_array()? {
                result.push_str(if token.is_null() {
                    cells.next()?.as_str()?
                } else {
                    token.as_str()?
                });
            }
            if cells.next().is_some() {
                return None;
            }
        }
    }
    Some(result)
}
