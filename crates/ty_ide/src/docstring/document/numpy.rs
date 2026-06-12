use indexmap::IndexMap;
use ruff_python_stdlib::identifiers::is_identifier;
use ruff_text_size::TextRange;

use super::SectionKind;
use super::preformatted::PreformattedBlockScanner;
use super::syntax::{ParsedLine, indentation, is_docstring_type_expression, parsed_lines};

pub(super) fn parameter_documentation(raw: &str) -> IndexMap<String, String> {
    let mut parameters = IndexMap::new();

    visit_sections(raw, |kind, _, body| {
        if matches!(kind, SectionKind::Parameters | SectionKind::OtherParameters) {
            extend_parameter_documentation(&mut parameters, body);
        }
    });

    parameters
}

/// Visits recognized top-level NumPy-style sections in source order.
pub(in crate::docstring) fn visit_sections<'a>(
    raw: &'a str,
    mut visit: impl FnMut(SectionKind, TextRange, &[ParsedLine<'a>]),
) {
    let lines = parsed_lines(raw);
    let mut preformatted_blocks = PreformattedBlockScanner::default();
    let mut index = 0;

    while index < lines.len() {
        if preformatted_blocks.consume_preformatted_line(lines[index].text) {
            index += 1;
            continue;
        }

        let Some(header) = parse_section_header(&lines, index) else {
            preformatted_blocks.observe_non_preformatted_line(lines[index].text);
            index += 1;
            continue;
        };
        if header.indent != 0 {
            index += 1;
            continue;
        }

        let (body_end, range) = section_body_end(&lines, header);
        visit(header.kind, range, &lines[header.body_start..body_end]);
        index = body_end;
    }
}

fn section_body_end(lines: &[ParsedLine<'_>], header: NumpySectionHeader) -> (usize, TextRange) {
    let mut body_end = header.body_start;
    let mut range = header.range;
    let mut preformatted_blocks = PreformattedBlockScanner::default();

    while let Some(line) = lines.get(body_end) {
        let previous_body = &lines[header.body_start..body_end];

        if line.text.trim().is_empty()
            && !blank_line_continues_section(previous_body, &lines[body_end..], header)
        {
            break;
        }

        if preformatted_blocks.is_active()
            && preformatted_blocks.consume_preformatted_line(line.text)
        {
            range = TextRange::new(range.start(), line.range.end());
            body_end += 1;
            continue;
        }

        if parse_section_header(lines, body_end).is_some_and(|next| next.indent <= header.indent) {
            break;
        }

        if !line.text.trim().is_empty()
            && !line_belongs_to_body(header, line, previous_body, &lines[body_end + 1..])
        {
            break;
        }

        if !preformatted_blocks.consume_preformatted_line(line.text) {
            preformatted_blocks.observe_non_preformatted_line(line.text);
        }
        range = TextRange::new(range.start(), line.range.end());
        body_end += 1;
    }

    (body_end, range)
}

fn blank_line_continues_section(
    previous_lines: &[ParsedLine<'_>],
    lines: &[ParsedLine<'_>],
    header: NumpySectionHeader,
) -> bool {
    let Some((offset, non_blank_line)) = lines
        .iter()
        .enumerate()
        .find(|(_, line)| !line.text.trim().is_empty())
    else {
        return false;
    };

    if parse_section_header(lines, offset).is_some_and(|next| next.indent <= header.indent) {
        return false;
    }

    line_belongs_to_body(header, non_blank_line, previous_lines, &lines[offset + 1..])
}

fn line_belongs_to_body(
    header: NumpySectionHeader,
    line: &ParsedLine<'_>,
    previous_lines: &[ParsedLine<'_>],
    following_lines: &[ParsedLine<'_>],
) -> bool {
    let line_indent = indentation(line.text);
    if line_indent > header.indent {
        return true;
    }
    if line_indent != header.indent {
        return false;
    }

    match header.kind {
        SectionKind::Parameters
        | SectionKind::KeywordArguments
        | SectionKind::OtherParameters
        | SectionKind::Attributes => named_item_starts(line, following_lines),
        SectionKind::Returns | SectionKind::Yields => {
            return_item_starts(line, previous_lines, following_lines)
        }
        SectionKind::Raises => raise_item_starts(line, following_lines),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct NumpySectionHeader {
    kind: SectionKind,
    indent: usize,
    body_start: usize,
    range: TextRange,
}

fn parse_section_header(lines: &[ParsedLine<'_>], index: usize) -> Option<NumpySectionHeader> {
    let line = lines.get(index)?;
    let underline = lines.get(index + 1)?;
    let indent = indentation(line.text);
    if indentation(underline.text) != indent || !is_underline(underline.text) {
        return None;
    }

    Some(NumpySectionHeader {
        kind: section_kind(line.text)?,
        indent,
        body_start: index + 2,
        range: TextRange::new(line.range.start(), underline.range.end()),
    })
}

fn section_kind(line: &str) -> Option<SectionKind> {
    match line.trim().to_ascii_lowercase().as_str() {
        "parameters" => Some(SectionKind::Parameters),
        "other parameter" | "other parameters" => Some(SectionKind::OtherParameters),
        "attributes" => Some(SectionKind::Attributes),
        "returns" | "return" => Some(SectionKind::Returns),
        "yields" | "yield" => Some(SectionKind::Yields),
        "raises" | "raise" => Some(SectionKind::Raises),
        _ => None,
    }
}

fn is_underline(line: &str) -> bool {
    let line = line.trim();
    line.len() >= 3 && line.chars().all(|char| char == '-')
}

fn named_item_starts(line: &ParsedLine<'_>, following_lines: &[ParsedLine<'_>]) -> bool {
    let trimmed = line.text.trim();
    split_type_separator(trimmed).is_some() || untyped_item_starts(trimmed, line, following_lines)
}

fn untyped_item_starts(
    trimmed: &str,
    line: &ParsedLine<'_>,
    following_lines: &[ParsedLine<'_>],
) -> bool {
    is_item_name(trimmed)
        && following_lines
            .iter()
            .find(|line| !line.text.trim().is_empty())
            .is_some_and(|next| indentation(next.text) > indentation(line.text))
}

fn return_item_starts(
    line: &ParsedLine<'_>,
    previous_lines: &[ParsedLine<'_>],
    following_lines: &[ParsedLine<'_>],
) -> bool {
    let trimmed = line.text.trim();
    split_type_separator(trimmed).is_some()
        || (!previous_lines
            .iter()
            .any(|line| !line.text.trim().is_empty())
            && is_anonymous_return_type(trimmed))
        || (is_anonymous_return_type(trimmed)
            && following_lines
                .iter()
                .find(|line| !line.text.trim().is_empty())
                .is_some_and(|next| indentation(next.text) > indentation(line.text)))
}

fn is_anonymous_return_type(line: &str) -> bool {
    !line.is_empty()
        && !line.ends_with('.')
        && !line.ends_with(':')
        && (is_docstring_type_expression(line) || is_prose_return_type(line))
}

fn is_prose_return_type(line: &str) -> bool {
    line.chars()
        .next()
        .is_some_and(|char| char.is_ascii_lowercase())
        && line
            .chars()
            .all(|char| char.is_ascii_alphanumeric() || matches!(char, '_' | '-' | '.' | ' '))
}

fn raise_item_starts(line: &ParsedLine<'_>, following_lines: &[ParsedLine<'_>]) -> bool {
    let trimmed = line.text.trim();
    parse_raise_item(trimmed).is_some_and(|description| !description.is_empty())
        || untyped_item_starts(trimmed, line, following_lines)
}

fn parse_raise_item(line: &str) -> Option<&str> {
    let (name, description) = line
        .split_once(':')
        .map_or((line.trim(), None), |(name, description)| {
            (name.trim(), Some(description.trim()))
        });
    if !is_item_name(name) {
        return None;
    }

    Some(description.unwrap_or_default())
}

fn extend_parameter_documentation(
    parameters: &mut IndexMap<String, String>,
    lines: &[ParsedLine<'_>],
) {
    let mut current: Option<Vec<(String, String)>> = None;
    let mut item_indent = None;

    for line in lines {
        let trimmed = line.text.trim();
        let line_indent = indentation(line.text);

        if trimmed.is_empty() {
            if let Some(current) = &mut current {
                for (_, description) in current {
                    description.push('\n');
                }
            }
            continue;
        }

        if item_indent.is_none_or(|indent| line_indent == indent)
            && let Some(parameter_group) = parse_parameter_line(trimmed)
        {
            insert_parameter_group(parameters, current.replace(parameter_group));
            item_indent.get_or_insert(line_indent);
            continue;
        }

        if let Some(current) = &mut current {
            if item_indent.is_some_and(|indent| line_indent <= indent) {
                break;
            }
            for (_, description) in current {
                if !description.is_empty() {
                    description.push('\n');
                }
                description.push_str(trimmed);
            }
        } else {
            break;
        }
    }

    insert_parameter_group(parameters, current);
}

fn parse_parameter_line(line: &str) -> Option<Vec<(String, String)>> {
    let name = line.split_once(':').map_or(line, |(name, _)| name).trim();
    let lookup_names = name
        .split(',')
        .map(parameter_lookup_name)
        .collect::<Option<Vec<_>>>()?;

    (!lookup_names.is_empty()).then(|| {
        lookup_names
            .into_iter()
            .map(|name| (name, String::new()))
            .collect()
    })
}

fn parameter_lookup_name(name: &str) -> Option<String> {
    let name = name.trim();
    is_item_name(name).then(|| name.to_string())
}

fn insert_parameter_group(
    parameters: &mut IndexMap<String, String>,
    parameter_group: Option<Vec<(String, String)>>,
) {
    let Some(parameter_group) = parameter_group else {
        return;
    };

    for (name, description) in parameter_group {
        let description = description.trim().to_string();
        if !description.is_empty() {
            parameters.entry(name).or_insert(description);
        }
    }
}

fn split_type_separator(line: &str) -> Option<(&str, &str)> {
    let (name, ty) = line.split_once(':')?;
    if !name.chars().last().is_some_and(char::is_whitespace)
        && !ty.chars().next().is_some_and(char::is_whitespace)
    {
        return None;
    }

    let name = name.trim();
    let ty = ty.trim();
    if !is_item_name(name) || ty.is_empty() {
        return None;
    }

    Some((name, ty))
}

fn is_item_name(name: &str) -> bool {
    name.split(',').all(|part| {
        let part = part.trim();
        let part = part
            .strip_prefix("**")
            .or_else(|| part.strip_prefix('*'))
            .unwrap_or(part);

        !part.is_empty() && part.split('.').all(is_identifier)
    })
}
