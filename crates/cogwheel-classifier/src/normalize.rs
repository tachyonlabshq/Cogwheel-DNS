//! Host normalisation.
//!
//! Every domain entering the classifier — from the DNS hot path, from the trainer, from the API —
//! goes through [`normalize`] first. Training and inference must agree exactly on the string being
//! featurised, so this is the single place that decision is made.

/// Longest hostname we will consider. Matches the DNS wire-format limit.
pub const MAX_HOST_LEN: usize = 253;

/// Reasons a hostname is not scoreable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NormalizeError {
    /// Empty after trimming.
    Empty,
    /// Longer than [`MAX_HOST_LEN`].
    TooLong,
    /// Contains a byte outside the LDH (letter/digit/hyphen) plus dot alphabet.
    InvalidCharacter,
    /// A label was empty, over 63 bytes, or hyphen-anchored.
    InvalidLabel,
    /// Only one label — not a resolvable public name.
    NotEnoughLabels,
    /// A bare IPv4 literal rather than a name.
    IpLiteral,
}

/// Normalise a hostname for classification.
///
/// * lowercases ASCII
/// * strips a trailing root dot and a leading `www.`
/// * validates the LDH alphabet and per-label rules
/// * rejects IPv4 literals and single-label names
///
/// Internationalised names are expected to arrive already in punycode (`xn--…`), which is what the
/// DNS wire format carries; the `xn--` prefix is left intact so the model can learn from it.
pub fn normalize(host: &str) -> Result<String, NormalizeError> {
    let mut trimmed = host.trim();
    if trimmed.is_empty() {
        return Err(NormalizeError::Empty);
    }
    // The DNS root label is not information the model should see.
    while let Some(stripped) = trimmed.strip_suffix('.') {
        trimmed = stripped;
    }
    if trimmed.is_empty() {
        return Err(NormalizeError::Empty);
    }
    if trimmed.len() > MAX_HOST_LEN {
        return Err(NormalizeError::TooLong);
    }

    let mut lowered = trimmed.to_ascii_lowercase();
    // `www.` carries no signal and would otherwise split one domain across two feature vectors.
    if let Some(stripped) = lowered.strip_prefix("www.") {
        lowered = stripped.to_string();
    }
    if lowered.is_empty() {
        return Err(NormalizeError::Empty);
    }

    let mut label_count = 0usize;
    for label in lowered.split('.') {
        label_count += 1;
        if label.is_empty() || label.len() > 63 {
            return Err(NormalizeError::InvalidLabel);
        }
        if label.starts_with('-') || label.ends_with('-') {
            return Err(NormalizeError::InvalidLabel);
        }
        for byte in label.bytes() {
            if !(byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-') {
                return Err(NormalizeError::InvalidCharacter);
            }
        }
    }
    if label_count < 2 {
        return Err(NormalizeError::NotEnoughLabels);
    }

    // A dotted-quad is a literal address, not a name the lexical model can reason about.
    if lowered.split('.').count() == 4
        && lowered.split('.').all(|label| {
            !label.is_empty() && label.len() <= 3 && label.bytes().all(|byte| byte.is_ascii_digit())
        })
    {
        return Err(NormalizeError::IpLiteral);
    }

    Ok(lowered)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lowercases_and_strips_root_dot() {
        assert_eq!(
            normalize("ADS.Example.COM.").as_deref(),
            Ok("ads.example.com")
        );
    }

    #[test]
    fn strips_leading_www() {
        assert_eq!(normalize("www.example.com").as_deref(), Ok("example.com"));
    }

    #[test]
    fn keeps_www_inside_the_name() {
        assert_eq!(
            normalize("a.www.example.com").as_deref(),
            Ok("a.www.example.com")
        );
    }

    #[test]
    fn preserves_punycode_prefix() {
        assert_eq!(
            normalize("xn--bcher-kva.example").as_deref(),
            Ok("xn--bcher-kva.example")
        );
    }

    #[test]
    fn rejects_ip_literals() {
        assert_eq!(normalize("192.168.1.1"), Err(NormalizeError::IpLiteral));
    }

    #[test]
    fn rejects_single_label() {
        assert_eq!(normalize("localhost"), Err(NormalizeError::NotEnoughLabels));
    }

    #[test]
    fn rejects_empty_and_root() {
        assert_eq!(normalize(""), Err(NormalizeError::Empty));
        assert_eq!(normalize("."), Err(NormalizeError::Empty));
        assert_eq!(normalize("   "), Err(NormalizeError::Empty));
    }

    #[test]
    fn rejects_empty_and_oversized_labels() {
        assert_eq!(normalize("a..b"), Err(NormalizeError::InvalidLabel));
        let long_label = "a".repeat(64);
        assert_eq!(
            normalize(&format!("{long_label}.com")),
            Err(NormalizeError::InvalidLabel)
        );
    }

    #[test]
    fn rejects_hyphen_anchored_labels() {
        assert_eq!(normalize("-bad.com"), Err(NormalizeError::InvalidLabel));
        assert_eq!(normalize("bad-.com"), Err(NormalizeError::InvalidLabel));
    }

    #[test]
    fn rejects_invalid_characters() {
        assert_eq!(
            normalize("ex ample.com"),
            Err(NormalizeError::InvalidCharacter)
        );
        assert_eq!(
            normalize("under_score.com"),
            Err(NormalizeError::InvalidCharacter)
        );
    }

    #[test]
    fn rejects_names_over_the_wire_limit() {
        let host = format!("{}.com", "a".repeat(300));
        assert_eq!(normalize(&host), Err(NormalizeError::TooLong));
    }

    #[test]
    fn accepts_numeric_labels_that_are_not_ip_literals() {
        assert_eq!(
            normalize("1234.example.com").as_deref(),
            Ok("1234.example.com")
        );
        assert_eq!(normalize("1.2.3.4.5").as_deref(), Ok("1.2.3.4.5"));
    }
}
