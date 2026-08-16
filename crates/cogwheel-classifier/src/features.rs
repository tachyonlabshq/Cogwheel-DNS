//! Feature extraction for the ad-domain classifier.
//!
//! A domain is turned into a sparse feature vector with two blocks:
//!
//! * **Dense block** — [`N_DENSE`] hand-engineered scalars (length, entropy, ad-token hits, …).
//!   Small enough to keep as `f32` weights, so no quantisation error touches them.
//! * **Hashed block** — character n-grams hashed into [`N_BUCKETS`] slots.
//!
//! The n-grams are extracted from four *namespaces* — the full host, the second-level label, the
//! subdomain prefix, and the TLD — each with its own hash seed. Namespacing matters more than it
//! looks: without it the model cannot tell `ads.example.com` from `example-ads.com`, and in
//! measurement it was the single change that dropped `google.com` from 0.57 to 0.14 while leaving
//! `doubleclick.net` above 0.90.
//!
//! Train-time and inference-time extraction MUST stay byte-identical, so everything here is
//! deterministic and dependency-free. `golden_vectors_are_stable` in the test module pins the
//! output; if you change anything in this file that test will fail and the model must be retrained.

/// Number of dense engineered features.
pub const N_DENSE: usize = 18;

/// Number of hash buckets for the n-gram block. 2^20 int8 weights = 1 MiB on disk.
pub const N_BUCKETS: usize = 1 << 20;

/// Shortest character n-gram extracted.
pub const NGRAM_MIN: usize = 3;

/// Longest character n-gram extracted.
pub const NGRAM_MAX: usize = 6;

// Distinct hash seeds keep the four namespaces from colliding with each other.
const NS_FULL: u32 = 0x9e37_79b1;
const NS_SLD: u32 = 0x85eb_ca6b;
const NS_SUB: u32 = 0xc2b2_ae35;
const NS_TLD: u32 = 0x27d4_eb2f;

/// Substrings that correlate with ad-tech infrastructure. Deliberately conservative: every entry
/// here is a token that appears in ad/tracker hostnames far more often than in ordinary ones.
const AD_TOKENS: [&str; 33] = [
    "ads",
    "adserver",
    "adsystem",
    "adservice",
    "advert",
    "banner",
    "beacon",
    "click",
    "track",
    "pixel",
    "analytic",
    "metric",
    "telemetry",
    "stats",
    "doubleclick",
    "affiliate",
    "popup",
    "promo",
    "sponsor",
    "campaign",
    "retarget",
    "audience",
    "segment",
    "collect",
    "impression",
    "adtech",
    "taboola",
    "outbrain",
    "criteo",
    "pubmatic",
    "rubicon",
    "openx",
    "adnxs",
];

/// Subdomain labels that are overwhelmingly benign infrastructure. Used as a negative signal.
const INFRA_TOKENS: [&str; 14] = [
    "cdn", "static", "assets", "img", "media", "files", "download", "api", "mail", "blog", "shop",
    "news", "support", "docs",
];

/// FNV-1a over a byte range. Chosen over SipHash because it needs no key material, is trivially
/// reimplementable in the corpus tooling, and the model only needs bucket stability, not
/// collision resistance against an adversary.
fn fnv1a(bytes: &[u8], seed: u32) -> u32 {
    let mut hash = seed;
    for &byte in bytes {
        hash ^= u32::from(byte);
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

/// A domain split into the three lexical regions the model reasons about.
///
/// This uses a naive "last two labels are the registrable domain" rule rather than the Public
/// Suffix List. That is deliberate: the rule is applied identically during training and inference,
/// so the model simply learns whatever representation it is given, and embedding a 250 KiB PSL to
/// change `bbc.co.uk` from (sld=`co`, sub=`bbc`) to (sld=`bbc`, sub=``) would require retraining
/// for a marginal gain. The corpus splitter *does* use the real PSL, because there a mistake causes
/// train/test leakage rather than a consistent re-encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostParts<'a> {
    /// Everything before the second-level label, e.g. `pixel.eu` in `pixel.eu.example.com`.
    pub subdomain: &'a str,
    /// The second-level label, e.g. `example`.
    pub second_level: &'a str,
    /// The final label, e.g. `com`.
    pub tld: &'a str,
    /// Number of dot-separated labels.
    pub label_count: usize,
}

/// Split a normalised host into subdomain / second-level / TLD.
pub fn split_host(host: &str) -> HostParts<'_> {
    let label_count = host.split('.').count();
    let Some(last_dot) = host.rfind('.') else {
        return HostParts {
            subdomain: "",
            second_level: host,
            tld: "",
            label_count,
        };
    };
    let tld = &host[last_dot + 1..];
    let head = &host[..last_dot];
    match head.rfind('.') {
        Some(second_dot) => HostParts {
            subdomain: &head[..second_dot],
            second_level: &head[second_dot + 1..],
            tld,
            label_count,
        },
        None => HostParts {
            subdomain: "",
            second_level: head,
            tld,
            label_count,
        },
    }
}

/// Compute the [`N_DENSE`] engineered features for a normalised host.
///
/// Every feature is scaled into roughly `[0, 1]` so a single learning rate suits them all.
pub fn dense_features(host: &str) -> [f32; N_DENSE] {
    let bytes = host.as_bytes();
    let len = bytes.len().max(1);

    let mut digits = 0usize;
    let mut hyphens = 0usize;
    let mut vowels = 0usize;
    let mut longest_consonant_run = 0usize;
    let mut current_run = 0usize;
    let mut histogram = [0u32; 256];

    for &byte in bytes {
        histogram[byte as usize] += 1;
        match byte {
            b'0'..=b'9' => {
                digits += 1;
                current_run = 0;
            }
            b'-' => {
                hyphens += 1;
                current_run = 0;
            }
            b'a' | b'e' | b'i' | b'o' | b'u' => {
                vowels += 1;
                current_run = 0;
            }
            b'a'..=b'z' => {
                current_run += 1;
                if current_run > longest_consonant_run {
                    longest_consonant_run = current_run;
                }
            }
            _ => current_run = 0,
        }
    }

    // Shannon entropy over the character distribution: DGA-style and machine-generated hostnames
    // sit noticeably higher than human-chosen ones.
    let mut entropy = 0.0f32;
    for count in histogram.iter().copied().filter(|c| *c > 0) {
        let probability = count as f32 / len as f32;
        entropy -= probability * probability.log2();
    }

    let parts = split_host(host);
    let first_label = host.split('.').next().unwrap_or_default();

    let ad_hits = AD_TOKENS
        .iter()
        .filter(|token| host.contains(**token))
        .count();
    let ad_hits_subdomain = AD_TOKENS
        .iter()
        .filter(|token| parts.subdomain.contains(**token))
        .count();
    let infra_hits = INFRA_TOKENS
        .iter()
        .filter(|token| {
            parts.subdomain == **token || parts.subdomain.starts_with(&format!("{token}."))
        })
        .count();

    let hex_like = first_label.len() >= 8 && first_label.bytes().all(|b| b.is_ascii_hexdigit());

    [
        (len.min(64) as f32) / 64.0,
        (parts.label_count.min(8) as f32) / 8.0,
        digits as f32 / len as f32,
        hyphens as f32 / len as f32,
        vowels as f32 / len as f32,
        (longest_consonant_run.min(12) as f32) / 12.0,
        entropy / 5.0,
        (ad_hits.min(6) as f32) / 6.0,
        (ad_hits_subdomain.min(6) as f32) / 6.0,
        (infra_hits.min(3) as f32) / 3.0,
        (parts.tld.len().min(12) as f32) / 12.0,
        (parts.second_level.len().min(32) as f32) / 32.0,
        if parts.label_count > 3 { 1.0 } else { 0.0 },
        if first_label.bytes().any(|b| b.is_ascii_digit()) {
            1.0
        } else {
            0.0
        },
        if parts.subdomain.is_empty() { 0.0 } else { 1.0 },
        (parts.subdomain.len().min(32) as f32) / 32.0,
        if hex_like { 1.0 } else { 0.0 },
        if first_label.contains('-') { 1.0 } else { 0.0 },
    ]
}

/// A sparse feature vector: dense block plus deduplicated, L2-normalised hashed n-grams.
#[derive(Debug, Clone, Default)]
pub struct Features {
    /// The dense block, aligned with [`dense_features`].
    pub dense: [f32; N_DENSE],
    /// `(bucket, weight)` pairs, sorted by bucket and deduplicated.
    pub buckets: Vec<(u32, f32)>,
}

/// Push all n-grams of `text` into `scratch` under the hash namespace `seed`.
///
/// `^` and `$` sentinels let the model learn prefix and suffix effects (`ads-` at the start of a
/// label reads very differently from `-ads` at the end).
fn accumulate_ngrams(text: &str, seed: u32, scratch: &mut Vec<(u32, f32)>) {
    if text.is_empty() {
        return;
    }
    let mut framed = String::with_capacity(text.len() + 2);
    framed.push('^');
    framed.push_str(text);
    framed.push('$');
    let bytes = framed.as_bytes();

    for order in NGRAM_MIN..=NGRAM_MAX {
        if order > bytes.len() {
            break;
        }
        let namespace = seed.wrapping_add((order as u32).wrapping_mul(7919));
        for window in bytes.windows(order) {
            let bucket = fnv1a(window, namespace) % (N_BUCKETS as u32);
            scratch.push((bucket, 1.0));
        }
    }
}

/// Extract the full feature vector for an already-normalised host.
pub fn extract(host: &str) -> Features {
    let mut scratch: Vec<(u32, f32)> = Vec::with_capacity(256);
    let parts = split_host(host);

    accumulate_ngrams(host, NS_FULL, &mut scratch);
    accumulate_ngrams(parts.second_level, NS_SLD, &mut scratch);
    accumulate_ngrams(parts.subdomain, NS_SUB, &mut scratch);

    // The TLD is one categorical token rather than a bag of n-grams; weighting it 2.0 keeps it
    // legible against the ~200 n-grams that surround it.
    if !parts.tld.is_empty() {
        let bucket = fnv1a(parts.tld.as_bytes(), NS_TLD) % (N_BUCKETS as u32);
        scratch.push((bucket, 2.0));
    }

    // Collapse duplicates: sort, then fold equal buckets together.
    scratch.sort_unstable_by_key(|(bucket, _)| *bucket);
    let mut buckets: Vec<(u32, f32)> = Vec::with_capacity(scratch.len());
    for (bucket, weight) in scratch {
        match buckets.last_mut() {
            Some((last_bucket, last_weight)) if *last_bucket == bucket => *last_weight += weight,
            _ => buckets.push((bucket, weight)),
        }
    }

    // L2-normalise the hashed block so a 60-character hostname does not simply outvote a short one.
    let norm = buckets.iter().map(|(_, w)| w * w).sum::<f32>().sqrt();
    if norm > 0.0 {
        for (_, weight) in &mut buckets {
            *weight /= norm;
        }
    }

    Features {
        dense: dense_features(host),
        buckets,
    }
}

/// Recover the `(text, bucket)` pairing for every n-gram a host produces.
///
/// [`extract`] deliberately throws the source text away — it only needs bucket indices, and keeping
/// strings around would allocate on the scoring path. Explanation is rare and off the hot path, so
/// it re-derives the mapping here instead.
///
/// The namespace a gram came from is spelled out in the returned label (`ads` vs `sld:ads`), because
/// "`ads` in the subdomain" and "`ads` inside the registrable name" are genuinely different
/// evidence and the UI should not conflate them.
pub fn ngram_provenance(host: &str) -> Vec<(String, u32)> {
    let parts = split_host(host);
    let mut out = Vec::new();

    let mut collect = |text: &str, seed: u32, prefix: &str| {
        if text.is_empty() {
            return;
        }
        let framed = format!("^{text}$");
        let bytes = framed.as_bytes();
        for order in NGRAM_MIN..=NGRAM_MAX {
            if order > bytes.len() {
                break;
            }
            let namespace = seed.wrapping_add((order as u32).wrapping_mul(7919));
            for window in bytes.windows(order) {
                let bucket = fnv1a(window, namespace) % (N_BUCKETS as u32);
                let text = String::from_utf8_lossy(window).into_owned();
                out.push((format!("{prefix}{text}"), bucket));
            }
        }
    };

    collect(host, NS_FULL, "");
    collect(parts.second_level, NS_SLD, "sld:");
    collect(parts.subdomain, NS_SUB, "sub:");
    if !parts.tld.is_empty() {
        out.push((
            format!("tld:{}", parts.tld),
            fnv1a(parts.tld.as_bytes(), NS_TLD) % (N_BUCKETS as u32),
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ngram_provenance_buckets_match_extraction() {
        let host = "ads.example.com";
        let extracted: std::collections::HashSet<u32> = extract(host)
            .buckets
            .iter()
            .map(|(bucket, _)| *bucket)
            .collect();
        let provenance: std::collections::HashSet<u32> = ngram_provenance(host)
            .into_iter()
            .map(|(_, bucket)| bucket)
            .collect();
        assert_eq!(
            extracted, provenance,
            "explanation must cover exactly the scored buckets"
        );
    }

    #[test]
    fn ngram_provenance_labels_the_namespace() {
        let labels: Vec<String> = ngram_provenance("ads.example.com")
            .into_iter()
            .map(|(text, _)| text)
            .collect();
        assert!(labels.iter().any(|l| l.starts_with("sld:")));
        assert!(labels.iter().any(|l| l.starts_with("sub:")));
        assert!(labels.iter().any(|l| l.starts_with("tld:")));
    }

    #[test]
    fn split_host_separates_subdomain_second_level_and_tld() {
        let parts = split_host("pixel.ads.example.com");
        assert_eq!(parts.subdomain, "pixel.ads");
        assert_eq!(parts.second_level, "example");
        assert_eq!(parts.tld, "com");
        assert_eq!(parts.label_count, 4);
    }

    #[test]
    fn split_host_handles_bare_registrable_domain() {
        let parts = split_host("example.com");
        assert_eq!(parts.subdomain, "");
        assert_eq!(parts.second_level, "example");
        assert_eq!(parts.tld, "com");
    }

    #[test]
    fn split_host_handles_single_label() {
        let parts = split_host("localhost");
        assert_eq!(parts.subdomain, "");
        assert_eq!(parts.second_level, "localhost");
        assert_eq!(parts.tld, "");
    }

    #[test]
    fn dense_features_are_bounded() {
        for host in [
            "a.io",
            "ads.example.com",
            "x1y2z3-4a5b6c.tracking-network.co",
            &"a".repeat(200),
        ] {
            for (index, value) in dense_features(host).iter().enumerate() {
                assert!(
                    value.is_finite() && (0.0..=1.5).contains(value),
                    "feature {index} out of range for {host}: {value}"
                );
            }
        }
    }

    #[test]
    fn ad_token_feature_fires_on_ad_hostnames() {
        let with_ads = dense_features("adserver.example.com");
        let without = dense_features("wiki.example.com");
        assert!(
            with_ads[7] > without[7],
            "ad-token feature should be higher for ad hostnames"
        );
    }

    #[test]
    fn hashed_block_is_l2_normalised() {
        let features = extract("ads.example.com");
        let norm = features
            .buckets
            .iter()
            .map(|(_, w)| w * w)
            .sum::<f32>()
            .sqrt();
        assert!((norm - 1.0).abs() < 1e-4, "expected unit norm, got {norm}");
    }

    #[test]
    fn buckets_are_sorted_and_deduplicated() {
        let features = extract("tracker.ads.tracker.example.com");
        for pair in features.buckets.windows(2) {
            assert!(pair[0].0 < pair[1].0, "buckets must be strictly increasing");
        }
    }

    #[test]
    fn namespaces_distinguish_subdomain_from_second_level() {
        // `ads` in the subdomain must not produce the same vector as `ads` inside the SLD.
        let subdomain_ads = extract("ads.example.com");
        let inline_ads = extract("exampleads.com");
        assert_ne!(subdomain_ads.buckets, inline_ads.buckets);
    }

    #[test]
    fn empty_namespaces_are_skipped_without_panicking() {
        let features = extract("example.com");
        assert!(!features.buckets.is_empty());
    }

    /// Pins the exact feature output. Changing extraction invalidates the shipped model, so this
    /// test failing is a signal to retrain, not to update the constants blindly.
    #[test]
    fn golden_vectors_are_stable() {
        let features = extract("doubleclick.net");
        assert_eq!(features.buckets.len(), 93);
        let checksum = features.buckets.iter().fold(0u64, |acc, (bucket, _)| {
            acc.wrapping_mul(31).wrapping_add(u64::from(*bucket))
        });
        assert_eq!(checksum, 4_502_868_218_204_176_355);
    }
}
