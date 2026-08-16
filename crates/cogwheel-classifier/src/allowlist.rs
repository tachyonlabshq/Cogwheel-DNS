//! The protected-domain safety net.
//!
//! A false positive here does not mean "an ad got through" — it means someone's banking site, OS
//! update channel, or captive-portal check stops resolving, and the household blames the DNS box.
//! A statistical model will occasionally be confident and wrong, so a set of domains is placed
//! permanently beyond its reach.
//!
//! This list only ever *prevents* blocking. It is consulted after scoring, so the score is still
//! recorded and still visible in the UI; only the enforcement decision is overridden. That keeps
//! the protection auditable rather than invisible.

/// Registrable domains the classifier may never cause to be blocked, with the subdomains beneath
/// them. Explicit blocklist rules and user-authored rules are unaffected — this bounds the
/// *classifier* only.
const PROTECTED_SUFFIXES: &[&str] = &[
    // DNS bootstrap and connectivity checks. Blocking these can leave a device with no working
    // resolver at all, which is unrecoverable without physical access.
    "one.one.one.one",
    "dns.google",
    "resolver1.opendns.com",
    "cloudflare-dns.com",
    "quad9.net",
    "connectivity-check.ubuntu.com",
    "captive.apple.com",
    "detectportal.firefox.com",
    "msftconnecttest.com",
    "msftncsi.com",
    "gstatic.com",
    // Time. A wrong clock breaks TLS everywhere.
    "pool.ntp.org",
    "ntp.org",
    "time.apple.com",
    "time.windows.com",
    "time.google.com",
    // OS and security updates.
    "apple.com",
    "icloud.com",
    "mzstatic.com",
    "windowsupdate.com",
    "update.microsoft.com",
    "microsoft.com",
    "canonical.com",
    "ubuntu.com",
    "debian.org",
    "archlinux.org",
    "fedoraproject.org",
    "raspberrypi.org",
    "raspberrypi.com",
    // Certificate validation. Blocking OCSP/CRL endpoints breaks TLS in confusing ways.
    "digicert.com",
    "letsencrypt.org",
    "sectigo.com",
    "globalsign.com",
    "identrust.com",
    // Emergency, government and health services.
    "who.int",
    "cdc.gov",
    "nhs.uk",
    "gov.uk",
    "usa.gov",
    "irs.gov",
    // Payment networks and major consumer banking. Not exhaustive by design — see the module note.
    "paypal.com",
    "stripe.com",
    "visa.com",
    "mastercard.com",
    "americanexpress.com",
    "chase.com",
    "bankofamerica.com",
    "wellsfargo.com",
    "citi.com",
    "hsbc.com",
    "barclays.co.uk",
    "santander.com",
];

/// Domains the classifier is not permitted to block.
#[derive(Debug, Clone)]
pub struct Allowlist {
    suffixes: Vec<String>,
}

impl Default for Allowlist {
    fn default() -> Self {
        Self::builtin()
    }
}

impl Allowlist {
    /// The built-in protected set.
    pub fn builtin() -> Self {
        Self {
            suffixes: PROTECTED_SUFFIXES
                .iter()
                .map(|s| (*s).to_string())
                .collect(),
        }
    }

    /// Extend the built-in set with operator-supplied entries.
    ///
    /// Entries are normalised the same way hostnames are; anything unparseable is skipped rather
    /// than rejected wholesale, so one bad line in a config file cannot disable the safety net.
    pub fn with_additional<I, S>(mut self, entries: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: AsRef<str>,
    {
        for entry in entries {
            if let Ok(host) = crate::normalize::normalize(entry.as_ref()) {
                self.suffixes.push(host);
            }
        }
        self.suffixes.sort();
        self.suffixes.dedup();
        self
    }

    /// Whether `host` is protected from classifier-driven blocking.
    ///
    /// Matches the domain itself and anything beneath it, but only on a label boundary — so
    /// `apple.com` protects `store.apple.com` and does not protect `notapple.com`.
    pub fn is_protected(&self, host: &str) -> bool {
        self.suffixes.iter().any(|suffix| {
            host == suffix
                || (host.len() > suffix.len()
                    && host.ends_with(suffix.as_str())
                    && host.as_bytes()[host.len() - suffix.len() - 1] == b'.')
        })
    }

    /// Every protected suffix.
    ///
    /// Exposed so [`crate::adapt`] can enumerate the safety net and assert it still holds before
    /// promoting an adaptation, rather than trusting that it does.
    pub fn suffixes(&self) -> &[String] {
        &self.suffixes
    }

    /// Number of protected entries.
    pub fn len(&self) -> usize {
        self.suffixes.len()
    }

    /// Whether the list is empty.
    pub fn is_empty(&self) -> bool {
        self.suffixes.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protects_exact_domains() {
        let allowlist = Allowlist::builtin();
        assert!(allowlist.is_protected("apple.com"));
        assert!(allowlist.is_protected("chase.com"));
    }

    #[test]
    fn protects_subdomains() {
        let allowlist = Allowlist::builtin();
        assert!(allowlist.is_protected("store.apple.com"));
        assert!(allowlist.is_protected("swdist.apple.com"));
        assert!(allowlist.is_protected("a.b.c.windowsupdate.com"));
    }

    #[test]
    fn does_not_protect_lookalike_suffixes() {
        let allowlist = Allowlist::builtin();
        assert!(!allowlist.is_protected("notapple.com"));
        assert!(!allowlist.is_protected("evil-apple.com"));
        assert!(!allowlist.is_protected("apple.com.evil.net"));
    }

    #[test]
    fn leaves_ordinary_domains_unprotected() {
        let allowlist = Allowlist::builtin();
        assert!(!allowlist.is_protected("doubleclick.net"));
        assert!(!allowlist.is_protected("example.com"));
    }

    #[test]
    fn accepts_operator_additions() {
        let allowlist = Allowlist::builtin().with_additional(["intranet.example.org"]);
        assert!(allowlist.is_protected("intranet.example.org"));
        assert!(allowlist.is_protected("wiki.intranet.example.org"));
    }

    #[test]
    fn skips_unparseable_additions_without_dropping_the_rest() {
        let before = Allowlist::builtin().len();
        let allowlist = Allowlist::builtin().with_additional(["not a domain", "good.example"]);
        assert!(allowlist.is_protected("good.example"));
        assert_eq!(allowlist.len(), before + 1);
    }

    #[test]
    fn builtin_list_is_non_empty() {
        assert!(!Allowlist::builtin().is_empty());
    }
}
