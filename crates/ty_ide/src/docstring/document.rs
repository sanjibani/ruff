use indexmap::IndexMap;

pub(in crate::docstring) mod google;
pub(super) mod preformatted;
pub(super) mod rst;
pub(in crate::docstring) mod syntax;

/// Canonical docstring sections shared by supported formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq, strum_macros::EnumIter)]
pub(in crate::docstring) enum SectionKind {
    Parameters,
    KeywordArguments,
    OtherParameters,
    Attributes,
    Returns,
    Yields,
    Raises,
}

impl SectionKind {
    pub(super) const fn heading(self) -> &'static str {
        match self {
            SectionKind::Parameters => "Parameters",
            SectionKind::KeywordArguments => "Keyword Arguments",
            SectionKind::OtherParameters => "Other Parameters",
            SectionKind::Attributes => "Attributes",
            SectionKind::Returns => "Returns",
            SectionKind::Yields => "Yields",
            SectionKind::Raises => "Raises",
        }
    }
}

/// Returns docs for all parameters recognized in the given docstring.
pub(super) fn parameter_documentation(raw: &str) -> IndexMap<String, String> {
    let mut parameters = rst::parameter_documentation(raw);
    let normalized = super::documentation_trim(raw);
    for (name, description) in google::parameter_documentation(&normalized) {
        parameters.entry(name).or_insert(description);
    }
    parameters
}
