//! Session Day titles: `Topic - facet`, where the facet names what sets one
//! Session Day apart within the Topic. A facet never repeats the Topic; a day
//! with nothing beyond its Topic has no facet and is titled by the Topic
//! alone.

/// Separator between the Topic and a day's facet in a Session Day title.
const SEPARATOR: &str = " - ";

/// A model-proposed facet, checked against the Topic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum FacetCheck {
    /// The facet is distinct from the Topic.
    Distinct(String),
    /// The facet equals or begins with the Topic. `remainder` is what is left
    /// once the leading Topic is removed — `None` when nothing is.
    RepeatsTopic { remainder: Option<String> },
}

pub(super) fn check_facet(facet: &str, topic: &str) -> FacetCheck {
    let facet = facet.trim();
    match topic_prefix_end(facet, topic) {
        None => FacetCheck::Distinct(facet.to_string()),
        Some(end) => {
            let remainder = facet[end..].trim_start_matches(|c: char| !c.is_alphanumeric());
            FacetCheck::RepeatsTopic {
                remainder: (!remainder.is_empty()).then(|| remainder.trim_end().to_string()),
            }
        }
    }
}

/// The correction sent back when a facet repeats the Topic.
pub(super) fn repeat_error(facet: &str, topic: &str) -> String {
    format!(
        "title {facet:?} repeats the Topic {topic:?}; title must be only this day's facet, without the Topic"
    )
}

pub(super) fn compose(topic: &str, facet: Option<&str>) -> String {
    match facet {
        Some(facet) => format!("{topic}{SEPARATOR}{facet}"),
        None => topic.to_string(),
    }
}

/// The facet of a stored Session Day title composed from `topic`: every
/// leading copy of the Topic removed (titles written before facets were
/// checked may repeat it), `None` when nothing else remains.
pub(super) fn facet_of<'a>(title: &'a str, topic: &str) -> Option<&'a str> {
    let mut rest = title.trim();
    while let Some(end) = topic_prefix_end(rest, topic) {
        rest = rest[end..].trim_start_matches(|c: char| !c.is_alphanumeric());
    }
    (!rest.is_empty()).then_some(rest)
}

/// Byte offset in `facet` just past a leading copy of `topic`, comparing
/// words case-insensitively and ignoring separators and punctuation. `None`
/// when `facet` does not begin with the whole Topic.
fn topic_prefix_end(facet: &str, topic: &str) -> Option<usize> {
    let topic_words: Vec<String> = words(topic).map(|(_, word)| word.to_lowercase()).collect();
    if topic_words.is_empty() {
        return None;
    }
    let mut facet_words = words(facet);
    let mut end = 0;
    for expected in &topic_words {
        let (start, word) = facet_words.next()?;
        if word.to_lowercase() != *expected {
            return None;
        }
        end = start + word.len();
    }
    Some(end)
}

/// Maximal alphanumeric runs with their byte offsets.
fn words(text: &str) -> impl Iterator<Item = (usize, &str)> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|word| !word.is_empty())
        .map(move |word| (word.as_ptr() as usize - text.as_ptr() as usize, word))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repeats(remainder: Option<&str>) -> FacetCheck {
        FacetCheck::RepeatsTopic {
            remainder: remainder.map(str::to_string),
        }
    }

    #[test]
    fn a_distinct_facet_passes() {
        assert_eq!(
            check_facet(" Layout fixes ", "Module Picker"),
            FacetCheck::Distinct("Layout fixes".into())
        );
        assert_eq!(
            check_facet("Module Pickers overview", "Module Picker"),
            FacetCheck::Distinct("Module Pickers overview".into()),
            "whole words only"
        );
        assert_eq!(
            check_facet("Fix the Module Picker", "Module Picker"),
            FacetCheck::Distinct("Fix the Module Picker".into()),
            "only a leading Topic repeats it"
        );
    }

    #[test]
    fn a_leading_topic_is_detected_ignoring_case_and_separators() {
        for facet in [
            "Module Picker - layout fixes",
            "module picker: layout fixes",
            "Module-Picker — layout fixes",
            "[Module Picker] layout fixes",
        ] {
            assert_eq!(
                check_facet(facet, "Module Picker"),
                repeats(Some("layout fixes")),
                "{facet}"
            );
        }
    }

    #[test]
    fn a_facet_equal_to_the_topic_leaves_nothing() {
        assert_eq!(check_facet("Module Picker", "Module Picker"), repeats(None));
        assert_eq!(
            check_facet("module picker.", "Module Picker"),
            repeats(None)
        );
    }

    #[test]
    fn composition_omits_the_separator_without_a_facet() {
        assert_eq!(compose("Archive", Some("Setup")), "Archive - Setup");
        assert_eq!(compose("Archive", None), "Archive");
    }

    #[test]
    fn stored_titles_yield_their_facet() {
        assert_eq!(facet_of("Archive - Setup", "Archive"), Some("Setup"));
        assert_eq!(facet_of("Archive", "Archive"), None);
        assert_eq!(facet_of("Other title", "Archive"), Some("Other title"));
        assert_eq!(
            facet_of("Archive - Archive - Setup", "Archive"),
            Some("Setup"),
            "a legacy doubled Topic is removed too"
        );
        assert_eq!(
            facet_of("Archive - Archives overview", "Archive"),
            Some("Archives overview")
        );
    }
}
