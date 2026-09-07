//! Validate bounded feed XML and recover only complete top-level fields/entries.
//! DTDs are rejected; this stage never resolves entities or fetches resources.
use quick_xml::{Reader, events::Event};
use std::borrow::Cow;

pub(super) struct Prepared<'a> {
    pub xml: Cow<'a, str>,
    pub incomplete: bool,
}

/// Unknown XML remains available to the generic text extractor. Known feeds use
/// a bounded event scan before feed-rs, preventing excessive parser recursion.
pub(super) fn prepare(body: &str, required: bool) -> Result<Option<Prepared<'_>>, String> {
    let mut reader = Reader::from_str(body);
    reader.config_mut().expand_empty_elements = true;
    let mut stack: Vec<String> = Vec::new();
    let mut known = false;
    let mut doctype = false;
    let mut nodes = 0usize;
    let mut checkpoint = (0usize, Vec::<String>::new());
    loop {
        match reader.read_event() {
            Ok(Event::Start(start)) => {
                if known && stack.is_empty() {
                    break;
                }
                if stack.is_empty() && !known {
                    known = matches!(start.local_name().as_ref(), b"rss" | b"feed" | b"RDF");
                    if !known {
                        return if required {
                            Err("Feed response has no RSS or Atom root element.".into())
                        } else {
                            Ok(None)
                        };
                    }
                    if doctype {
                        return Err("Feed XML containing a DTD is unsupported; external entities are not resolved.".into());
                    }
                }
                nodes += 1;
                if nodes > 100_000 || stack.len() >= 64 {
                    return Err("Feed XML exceeds the element or nesting limit.".into());
                }
                stack.push(String::from_utf8_lossy(start.name().as_ref()).into_owned());
                if container(&stack) {
                    checkpoint = (reader.buffer_position() as usize, stack.clone());
                }
            }
            Ok(Event::End(_)) => {
                stack.pop();
                if container(&stack) {
                    checkpoint = (reader.buffer_position() as usize, stack.clone());
                }
            }
            Ok(Event::DocType(_)) => {
                doctype = true;
                if known {
                    return Err("Feed XML containing a DTD is unsupported; external entities are not resolved.".into());
                }
            }
            Ok(Event::Eof) if known && stack.is_empty() => {
                return Ok(Some(Prepared {
                    xml: Cow::Borrowed(body),
                    incomplete: false,
                }));
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    if !known {
        return if required {
            Err("Feed XML is empty or malformed before its root element.".into())
        } else {
            Ok(None)
        };
    }
    if checkpoint.0 == 0 {
        return Err("Feed XML contains no recoverable complete fields or entries.".into());
    }
    let mut recovered = body[..checkpoint.0].to_owned();
    for name in checkpoint.1.iter().rev() {
        recovered.push_str(&format!("</{name}>"));
    }
    Ok(Some(Prepared {
        xml: Cow::Owned(recovered),
        incomplete: true,
    }))
}

/// Only these containers can hold feed metadata or complete entry siblings.
fn container(stack: &[String]) -> bool {
    let local = |name: &str| name.rsplit(':').next().unwrap_or(name).to_owned();
    matches!(stack, [root] if matches!(local(root).as_str(), "feed" | "RDF"))
        || matches!(stack, [root, channel] if local(root) == "rss" && local(channel) == "channel")
}
