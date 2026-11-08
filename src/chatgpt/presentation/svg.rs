//! Strict, inert SVG illustration projection. The complete original remains
//! in native evidence; scripts, external resources and event handlers do not.
use anyhow::{Result, bail};
use quick_xml::{
    Reader, Writer,
    events::{BytesStart, Event},
};

fn element(name: &str) -> bool {
    matches!(
        name,
        "svg"
            | "g"
            | "path"
            | "rect"
            | "circle"
            | "ellipse"
            | "line"
            | "polyline"
            | "polygon"
            | "text"
            | "tspan"
            | "title"
            | "desc"
            | "defs"
            | "linearGradient"
            | "radialGradient"
            | "stop"
            | "clipPath"
            | "mask"
            | "pattern"
            | "use"
    )
}
fn attribute(name: &str) -> Option<&str> {
    Some(match name {
        "fontSize" => "font-size",
        "textAnchor" => "text-anchor",
        "dominantBaseline" => "dominant-baseline",
        "fontWeight" => "font-weight",
        "fontFamily" => "font-family",
        "fillOpacity" => "fill-opacity",
        "strokeOpacity" => "stroke-opacity",
        "strokeWidth" => "stroke-width",
        "strokeDasharray" => "stroke-dasharray",
        "strokeLinecap" => "stroke-linecap",
        "strokeLinejoin" => "stroke-linejoin",
        "clipPath" => "clip-path",
        "xmlns"
        | "viewBox"
        | "width"
        | "height"
        | "x"
        | "y"
        | "x1"
        | "y1"
        | "x2"
        | "y2"
        | "cx"
        | "cy"
        | "r"
        | "rx"
        | "ry"
        | "dx"
        | "dy"
        | "d"
        | "points"
        | "fill"
        | "stroke"
        | "opacity"
        | "transform"
        | "id"
        | "font-size"
        | "font-weight"
        | "font-family"
        | "text-anchor"
        | "dominant-baseline"
        | "fill-opacity"
        | "stroke-opacity"
        | "stroke-width"
        | "stroke-dasharray"
        | "stroke-linecap"
        | "stroke-linejoin"
        | "clip-path"
        | "offset"
        | "stop-color"
        | "stop-opacity"
        | "gradientUnits"
        | "gradientTransform"
        | "patternUnits"
        | "patternTransform"
        | "preserveAspectRatio"
        | "href" => name,
        _ => return None,
    })
}
pub(super) fn sanitize(source: &str) -> Result<Vec<u8>> {
    let mut reader = Reader::from_str(source);
    let mut writer = Writer::new(Vec::new());
    let mut skipped = 0;
    let mut depth = 0;
    let mut root = false;
    loop {
        match reader.read_event()? {
            Event::Start(start) | Event::Empty(start) => {
                let is_empty =
                    source.as_bytes().get(reader.buffer_position() as usize - 2) == Some(&b'/');
                let name = start.name();
                if skipped > 0 || !element(name.as_ref()) {
                    if !is_empty {
                        skipped += 1;
                    }
                    continue;
                }
                if depth == 0 {
                    if root || name.as_ref() != "svg" {
                        bail!("SVG requires one svg root");
                    }
                    root = true;
                }
                let name = name.as_ref();
                let mut safe = BytesStart::new(name);
                for attr in start.attributes() {
                    let attr = attr?;
                    let key = attr.key.as_ref();
                    let Some(key) = attribute(key) else { continue };
                    let value = attr.normalized_value(quick_xml::XmlVersion::Implicit1_0)?;
                    let lower = value.to_ascii_lowercase();
                    if (key == "xmlns" && value != "http://www.w3.org/2000/svg")
                        || (key == "href" && !value.starts_with('#'))
                        || lower.contains("javascript:")
                        || lower.contains("data:")
                        || lower.contains("http:") && key != "xmlns"
                        || lower.contains("https:")
                        || lower.contains("//") && key != "xmlns"
                        || lower.contains("url(")
                            && !lower
                                .strip_prefix("url(#")
                                .and_then(|s| s.strip_suffix(')'))
                                .is_some_and(|id| {
                                    !id.is_empty()
                                        && id.chars().all(|c| {
                                            c.is_ascii_alphanumeric()
                                                || matches!(c, '_' | '-' | '.')
                                        })
                                })
                    {
                        continue;
                    }
                    safe.push_attribute((key, value.as_ref()));
                }
                if is_empty {
                    writer.write_event(Event::Empty(safe))?;
                } else {
                    depth += 1;
                    writer.write_event(Event::Start(safe))?;
                }
            }
            Event::End(end) => {
                if skipped > 0 {
                    skipped -= 1;
                    continue;
                }
                depth -= 1;
                writer.write_event(Event::End(end))?;
            }
            Event::Text(text) if skipped == 0 => writer.write_event(Event::Text(text))?,
            Event::GeneralRef(reference) if skipped == 0 => {
                if matches!(reference.as_ref(), "amp" | "lt" | "gt" | "quot" | "apos")
                    || reference.resolve_char_ref()?.is_some()
                {
                    writer.write_event(Event::GeneralRef(reference))?;
                } else {
                    bail!("unsupported SVG entity");
                }
            }
            Event::CData(_) | Event::DocType(_) => {
                bail!("unsupported SVG entity or document content")
            }
            Event::Eof => break,
            _ => {}
        }
    }
    if !root || depth != 0 {
        bail!("incomplete SVG illustration");
    }
    Ok(writer.into_inner())
}
