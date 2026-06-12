use indexmap::IndexMap;
use ruff_python_stdlib::identifiers::is_identifier;
use ruff_text_size::TextRange;

use super::SectionKind;
use super::preformatted::PreformattedBlockScanner;
use super::syntax::{
    ParsedLine, indentation, parse_parenthesized_type, parsed_lines, split_once_unbracketed_colon,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GoogleSectionHeaderKind {
    Supported(SectionKind),
    Unsupported,
}

pub(super) fn parameter_documentation(raw: &str) -> IndexMap<String, String> {
    let mut parameters = IndexMap::new();

    visit_sections(raw, |kind, _, body| {
        if matches!(
            kind,
            SectionKind::Parameters | SectionKind::KeywordArguments | SectionKind::OtherParameters
        ) {
            extend_parameter_documentation(&mut parameters, body);
        }
    });

    parameters
}

/// Visits recognized top-level Google-style sections in source order.
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

        let Some(header) = parse_google_section_like_header(&lines, index) else {
            preformatted_blocks.observe_non_preformatted_line(lines[index].text);
            index += 1;
            continue;
        };
        if header.indent != 0 {
            index += 1;
            continue;
        }

        let (body_end, range) = google_section_body_end(&lines, header);
        if let GoogleSectionHeaderKind::Supported(kind) = header.kind {
            visit(kind, range, &lines[header.body_start..body_end]);
        }
        index = body_end;
    }
}

fn google_section_body_end(
    lines: &[ParsedLine<'_>],
    header: GoogleSectionHeader,
) -> (usize, TextRange) {
    let mut body_end = header.body_start;
    let mut range = header.range;
    let mut body_preformatted_blocks = PreformattedBlockScanner::default();
    let mut item_indent = None;

    while let Some(line) = lines.get(body_end) {
        if body_preformatted_blocks.is_active()
            && body_preformatted_blocks.consume_preformatted_line(line.text)
        {
            range = TextRange::new(range.start(), line.range.end());
            body_end += 1;
            continue;
        }

        if line.text.trim().is_empty() {
            if !google_blank_line_continues_section(&lines[body_end..], header, item_indent) {
                break;
            }

            while let Some(blank_line) = lines.get(body_end)
                && blank_line.text.trim().is_empty()
            {
                range = TextRange::new(range.start(), blank_line.range.end());
                body_end += 1;
            }
            continue;
        }

        if google_section_header_ends_body(lines, body_end, header, item_indent) {
            break;
        }

        if !google_line_belongs_to_body(header, line.text, item_indent) {
            break;
        }

        item_indent = item_indent.or_else(|| google_section_item_indent(header, line.text));

        if !body_preformatted_blocks.consume_preformatted_line(line.text) {
            body_preformatted_blocks.observe_non_preformatted_line(line.text);
        }
        range = TextRange::new(range.start(), line.range.end());
        body_end += 1;
    }

    (body_end, range)
}

fn google_blank_line_continues_section(
    lines: &[ParsedLine<'_>],
    header: GoogleSectionHeader,
    item_indent: Option<usize>,
) -> bool {
    let Some((offset, non_blank_line)) = lines
        .iter()
        .enumerate()
        .find(|(_, line)| !line.text.trim().is_empty())
    else {
        return false;
    };

    if indentation(non_blank_line.text) <= header.indent {
        // A blank line disambiguates lowercase section names from same-indent parameters.
        if parse_google_section_like_header(lines, offset).is_some()
            || is_inline_google_section_header(non_blank_line.text)
        {
            return false;
        }

        // Unlike named sections, returns and yields have no item syntax that can distinguish a
        // same-indent body from prose following an empty section.
        if item_indent.is_none()
            && matches!(
                header.kind,
                GoogleSectionHeaderKind::Supported(SectionKind::Returns | SectionKind::Yields)
            )
        {
            return false;
        }
    }

    google_line_belongs_to_body(header, non_blank_line.text, item_indent)
}

fn google_section_header_ends_body(
    lines: &[ParsedLine<'_>],
    index: usize,
    header: GoogleSectionHeader,
    item_indent: Option<usize>,
) -> bool {
    let Some(line) = lines.get(index) else {
        return false;
    };
    let same_indent_named_item =
        same_indent_named_item_takes_precedence(header, line.text, item_indent);
    if indentation(line.text) <= header.indent && is_inline_google_section_header(line.text) {
        return !same_indent_named_item;
    }

    let Some(next) = parse_google_section_like_header(lines, index) else {
        return false;
    };

    next.indent <= header.indent && (next.underlined || !same_indent_named_item)
}

fn google_line_belongs_to_body(
    header: GoogleSectionHeader,
    line: &str,
    item_indent: Option<usize>,
) -> bool {
    let line_indent = indentation(line);
    line_indent > header.indent
        || (line_indent == header.indent
            && item_indent.is_none_or(|indent| indent == line_indent)
            && google_section_item_indent(header, line).is_some())
}

fn same_indent_named_item_takes_precedence(
    header: GoogleSectionHeader,
    line: &str,
    item_indent: Option<usize>,
) -> bool {
    let line_indent = indentation(line);
    matches!(
        header.kind,
        GoogleSectionHeaderKind::Supported(
            SectionKind::Parameters
                | SectionKind::KeywordArguments
                | SectionKind::OtherParameters
                | SectionKind::Attributes
                | SectionKind::Raises
        )
    ) && line_indent == header.indent
        && item_indent.is_none_or(|indent| indent == line_indent)
        && google_section_item_indent(header, line).is_some()
        && (line.trim().chars().next().is_some_and(char::is_lowercase)
            || (matches!(
                header.kind,
                GoogleSectionHeaderKind::Supported(SectionKind::Raises)
            ) && split_once_unbracketed_colon(line.trim())
                .is_some_and(|(name, _)| has_exception_name_suffix(name.trim()))))
}

fn google_section_item_indent(header: GoogleSectionHeader, line: &str) -> Option<usize> {
    let trimmed = line.trim();
    let is_item = match header.kind {
        GoogleSectionHeaderKind::Supported(
            SectionKind::Parameters | SectionKind::KeywordArguments | SectionKind::OtherParameters,
        ) => parse_google_parameter(trimmed).is_some(),
        GoogleSectionHeaderKind::Supported(SectionKind::Attributes | SectionKind::Raises) => {
            split_once_unbracketed_colon(trimmed).is_some_and(|(name, _)| !name.trim().is_empty())
        }
        GoogleSectionHeaderKind::Supported(SectionKind::Returns | SectionKind::Yields) => {
            !trimmed.is_empty()
        }
        GoogleSectionHeaderKind::Unsupported => false,
    };
    is_item.then(|| indentation(line))
}

fn is_google_section_underline(line: &str) -> bool {
    let line = line.trim();
    !line.is_empty() && line.chars().all(|character| matches!(character, '-' | '='))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct GoogleSectionHeader {
    kind: GoogleSectionHeaderKind,
    indent: usize,
    body_start: usize,
    range: TextRange,
    underlined: bool,
}

fn parse_google_section_like_header(
    lines: &[ParsedLine<'_>],
    index: usize,
) -> Option<GoogleSectionHeader> {
    let line = lines.get(index)?;
    let kind = google_section_kind(line.text)?;
    let underline = lines
        .get(index + 1)
        .filter(|line| is_google_section_underline(line.text));

    Some(GoogleSectionHeader {
        kind,
        indent: indentation(line.text),
        body_start: index + 1 + usize::from(underline.is_some()),
        range: underline.map_or(line.range, |underline| {
            TextRange::new(line.range.start(), underline.range.end())
        }),
        underlined: underline.is_some(),
    })
}

fn google_section_kind(line: &str) -> Option<GoogleSectionHeaderKind> {
    let name = line.trim().strip_suffix(':')?.trim();
    google_section_kind_from_name(name)
}

fn google_section_kind_from_name(name: &str) -> Option<GoogleSectionHeaderKind> {
    let normalized = name
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();
    let kind = match normalized.as_str() {
        "args" | "arguments" | "parameters" => {
            GoogleSectionHeaderKind::Supported(SectionKind::Parameters)
        }
        "keyword args" | "keyword arguments" => {
            GoogleSectionHeaderKind::Supported(SectionKind::KeywordArguments)
        }
        "other args" | "other arguments" | "other parameters" => {
            GoogleSectionHeaderKind::Supported(SectionKind::OtherParameters)
        }
        "attributes" => GoogleSectionHeaderKind::Supported(SectionKind::Attributes),
        "return" | "returns" => GoogleSectionHeaderKind::Supported(SectionKind::Returns),
        "yield" | "yields" => GoogleSectionHeaderKind::Supported(SectionKind::Yields),
        "raise" | "raises" => GoogleSectionHeaderKind::Supported(SectionKind::Raises),
        "attention" | "caution" | "danger" | "error" | "example" | "examples" | "hint"
        | "important" | "methods" | "note" | "notes" | "references" | "see also" | "tip"
        | "todo" | "todos" | "warning" | "warnings" | "warns" => {
            GoogleSectionHeaderKind::Unsupported
        }
        _ => return None,
    };
    Some(kind)
}

fn is_inline_google_section_header(line: &str) -> bool {
    let Some((name, description)) = split_once_unbracketed_colon(line.trim()) else {
        return false;
    };
    let name = name.trim();
    let description = description.trim();

    !description.is_empty()
        && !description.starts_with(':')
        && name.chars().next().is_some_and(char::is_uppercase)
        && google_section_kind_from_name(name).is_some()
}

pub(in crate::docstring) fn is_section_like_header(line: &str) -> bool {
    google_section_kind(line).is_some() || is_inline_google_section_header(line)
}

pub(in crate::docstring) fn has_exception_name_suffix(name: &str) -> bool {
    ["Error", "Exception", "Warning"]
        .iter()
        .any(|suffix| name.ends_with(suffix))
}

fn parse_google_parameter(line: &str) -> Option<(&str, &str)> {
    let (name, description) = split_once_unbracketed_colon(line)?;
    let name = name.trim();
    let (display_name, _) = parse_parenthesized_type(name);
    google_parameter_names(display_name).next()?;

    Some((display_name, description.trim()))
}

fn extend_parameter_documentation(parameters: &mut IndexMap<String, String>, lines: &[ParsedLine]) {
    let mut current: Option<(String, String)> = None;
    let mut item_indent = None;

    for line in lines {
        let line = line.text;
        let trimmed = line.trim();
        let line_indent = indentation(line);

        if trimmed.is_empty() {
            if let Some(current) = &mut current {
                if !current.1.is_empty() && !current.1.ends_with('\n') {
                    current.1.push('\n');
                }
                current.1.push('\n');
            }
            continue;
        }

        if item_indent.is_none_or(|indent| line_indent == indent)
            && let Some((names, description)) = parse_google_parameter(trimmed)
        {
            insert_parameter_documentation(
                parameters,
                current.replace((names.to_string(), description.to_string())),
            );
            item_indent.get_or_insert(line_indent);
            continue;
        }

        if let Some(current) = &mut current {
            if !current.1.is_empty() && !current.1.ends_with('\n') {
                current.1.push('\n');
            }
            current.1.push_str(trimmed);
        }
    }

    insert_parameter_documentation(parameters, current);
}

fn insert_parameter_documentation(
    parameters: &mut IndexMap<String, String>,
    parameter: Option<(String, String)>,
) {
    let Some((names, description)) = parameter else {
        return;
    };
    let description = description.trim();
    if description.is_empty() {
        return;
    }
    for name in google_parameter_names(&names) {
        parameters
            .entry(name.to_string())
            .or_insert_with(|| description.to_string());
    }
}

fn google_parameter_names(display_name: &str) -> impl Iterator<Item = &str> {
    display_name
        .split(',')
        .map(str::trim)
        .filter(|name| is_parameter_name(name))
}

pub(in crate::docstring) fn is_parameter_name(name: &str) -> bool {
    let identifier = name
        .strip_prefix("**")
        .or_else(|| name.strip_prefix('*'))
        .unwrap_or(name);

    is_identifier(identifier)
}

pub(in crate::docstring) fn is_dotted_identifier(name: &str) -> bool {
    !name.is_empty() && name.split('.').all(is_identifier)
}

#[cfg(test)]
mod tests {
    use super::{SectionKind, parameter_documentation, visit_sections};

    #[test]
    fn parameter_documentation_preserves_consecutive_blank_lines() {
        let parameters = parameter_documentation(
            "Args:\n    value: First paragraph.\n\n\n        Second paragraph.",
        );

        assert_eq!(
            parameters["value"],
            "First paragraph.\n\n\nSecond paragraph."
        );
    }

    #[test]
    fn parameter_documentation_accepts_same_indent_items() {
        let parameters = parameter_documentation(
            "Arguments:\nfirst: First parameter.\nsecond: Second parameter.\nReturns:\nbool: Result.",
        );

        assert_eq!(parameters.len(), 2);
        assert_eq!(parameters["first"], "First parameter.");
        assert_eq!(parameters["second"], "Second parameter.");
    }

    #[test]
    fn parameter_documentation_accepts_grouped_items() {
        let parameters = parameter_documentation("Args:\n    x, y: Coordinates.");

        assert_eq!(parameters.len(), 2);
        assert_eq!(parameters["x"], "Coordinates.");
        assert_eq!(parameters["y"], "Coordinates.");
    }

    #[test]
    fn parameter_documentation_accepts_other_parameter_sections() {
        for heading in ["Other Args", "Other Arguments", "Other Parameters"] {
            let parameters =
                parameter_documentation(&format!("{heading}:\n    timeout: Maximum wait."));

            assert_eq!(parameters.len(), 1);
            assert_eq!(parameters["timeout"], "Maximum wait.");
        }
    }

    #[test]
    fn parameter_documentation_accepts_parentheses_in_quoted_types() {
        let parameters = parameter_documentation(
            r#"Args:
    value (Literal["("]): Parameter with a quoted parenthesis."#,
        );

        assert_eq!(parameters["value"], "Parameter with a quoted parenthesis.");
    }

    #[test]
    fn parameter_documentation_stops_at_methods_section() {
        let parameters = parameter_documentation(
            "Args:\n    value: Parameter documentation.\nMethods:\n    helper: Method documentation.",
        );

        assert_eq!(parameters.len(), 1);
        assert_eq!(parameters["value"], "Parameter documentation.");
    }

    #[test]
    fn parameter_documentation_stops_at_inline_section_summary() {
        let parameters = parameter_documentation(
            "Args:\n    first: First parameter.\n    last: Last parameter.\n\nReturns: Result.",
        );

        assert_eq!(parameters.len(), 2);
        assert_eq!(parameters["last"], "Last parameter.");
    }

    #[test]
    fn parameter_documentation_stops_at_same_indent_inline_section_summary() {
        let parameters = parameter_documentation(
            "Args:\nfirst: First parameter.\nlast: Last parameter.\nReturns: Result.",
        );

        assert_eq!(parameters.len(), 2);
        assert_eq!(parameters["last"], "Last parameter.");
    }

    #[test]
    fn parameter_documentation_accepts_underlined_section() {
        let parameters = parameter_documentation(
            "Summary.\n\nArgs:\n----\n    value: Parameter documentation.\n\nReturns:\n    bool: Result.",
        );

        assert_eq!(parameters.len(), 1);
        assert_eq!(parameters["value"], "Parameter documentation.");
    }

    #[test]
    fn parameter_documentation_prefers_lowercase_same_indent_parameter() {
        let parameters = parameter_documentation(
            "Args:\nerror:\n    Error documentation.\ncode: Code documentation.\nreturns: Return parameter documentation.\nReturns:\nbool: Result.",
        );

        assert_eq!(parameters.len(), 3);
        assert_eq!(parameters["error"], "Error documentation.");
        assert_eq!(parameters["code"], "Code documentation.");
        assert_eq!(parameters["returns"], "Return parameter documentation.");
    }

    #[test]
    fn section_visiting_preserves_underlined_lowercase_header() {
        let mut returns_body = None;
        visit_sections(
            "Args:\nvalue: Parameter documentation.\nreturns:\n--------\n    bool: Result.",
            |kind, _, body| {
                if kind == SectionKind::Returns {
                    returns_body = Some(
                        body.iter()
                            .map(|line| line.text.to_string())
                            .collect::<Vec<_>>(),
                    );
                }
            },
        );

        assert_eq!(returns_body, Some(vec!["    bool: Result.".to_string()]));
    }

    #[test]
    fn parameter_documentation_stops_at_blank_separated_lowercase_section() {
        let parameters = parameter_documentation(
            "Args:\nvalue: Parameter documentation.\n\nreturns:\n    bool: Result.",
        );

        assert_eq!(parameters.len(), 1);
        assert_eq!(parameters["value"], "Parameter documentation.");
    }
}
