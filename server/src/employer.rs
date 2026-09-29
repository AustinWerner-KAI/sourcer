//! Does someone work at a given company? Used to keep the hiring client's own
//! staff (and off-limits clients' staff) out of searches, shortlists and sends.
//!
//! This is a block list, so it errs towards blocking: a wrongly blocked person
//! costs one candidate; a wrongly contacted one costs a client. It matches on
//! web domain, or on the name once case, accents, punctuation, "The" and legal
//! suffixes are removed, and treats "Acme" and "Acme Payments" as the same.
//! Someone whose employer is unknown is reported as such, never as clear.

use sqlx::PgPool;
use uuid::Uuid;

/// A company to keep out, as held on the client record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Company {
    pub id: Uuid,
    pub name: String,
    pub domain: Option<String>,
    /// The client this role is for (rather than an off-limits client).
    pub hiring: bool,
}

/// The outcome of checking one person against a role's locked-out companies.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Verdict {
    Clear,
    LockedOut,
    /// No current employer on record: a person must check before contact.
    Unknown,
}

const LEGAL_SUFFIXES: &[&str] = &[
    "inc",
    "incorporated",
    "ltd",
    "limited",
    "llc",
    "llp",
    "plc",
    "gmbh",
    "ag",
    "sa",
    "sas",
    "bv",
    "nv",
    "corp",
    "corporation",
    "co",
    "company",
    "fz",
    "fzco",
    "fze",
    "fzllc",
    "dmcc",
];
/// The shorter name must be at least this long to match as a prefix, so
/// "Co" or "AB" never block half the market.
const MIN_PREFIX_CHARS: usize = 4;

/// Letters that do not split into a base letter and an accent.
fn transliterate(c: char) -> Option<&'static str> {
    Some(match c {
        'ł' => "l",
        'ø' => "o",
        'ı' => "i",
        'đ' | 'ð' => "d",
        'ß' => "ss",
        'æ' => "ae",
        'œ' => "oe",
        'þ' => "th",
        '&' => " and ",
        _ => return None,
    })
}

/// Lower-case plain words: "The Acme Payments, Inc." becomes "acme payments",
/// "Société Générale" becomes "societe generale", "Łódź" becomes "lodz".
/// Letters in any script are kept, so non-Latin names still match themselves.
pub fn normalise_name(raw: &str) -> String {
    use unicode_normalization::{char::is_combining_mark, UnicodeNormalization};
    let mut cleaned = String::with_capacity(raw.len());
    for c in raw.to_lowercase().nfkd() {
        if is_combining_mark(c) {
            continue;
        }
        if let Some(t) = transliterate(c) {
            cleaned.push_str(t);
        } else if c.is_alphanumeric() {
            cleaned.push(c);
        } else {
            cleaned.push(' ');
        }
    }
    let mut words: Vec<&str> = cleaned.split_whitespace().collect();
    if words.len() > 1 && words[0] == "the" {
        words.remove(0);
    }
    while words.len() > 1 && LEGAL_SUFFIXES.contains(words.last().unwrap()) {
        words.pop();
    }
    words.join(" ")
}

/// "https://www.Example.com/about" becomes "example.com". `None` if it is not
/// a plausible domain.
pub fn normalise_domain(raw: &str) -> Option<String> {
    let s = raw.trim().to_lowercase();
    let s = s
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_start_matches("www.");
    let host = s.split(['/', '?', '#']).next().unwrap_or("");
    let host = host.split(':').next().unwrap_or("");
    let ok = host.contains('.')
        && !host.starts_with('.')
        && !host.ends_with('.')
        && !host.contains("..")
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-');
    ok.then(|| host.to_string())
}

/// Same name, or one is the other's opening words ("Acme" / "Acme Payments").
fn names_match(a: &str, b: &str) -> bool {
    let (a, b) = (normalise_name(a), normalise_name(b));
    if a.is_empty() || b.is_empty() {
        return false;
    }
    if a == b {
        return true;
    }
    let (short, long) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    short.len() >= MIN_PREFIX_CHARS && long.starts_with(&format!("{short} "))
}

/// True when an employer (name, and domain if known) is this company.
pub fn is_company(employer: &str, employer_domain: Option<&str>, company: &Company) -> bool {
    if let (Some(a), Some(b)) = (
        employer_domain.and_then(normalise_domain),
        company.domain.as_deref().and_then(normalise_domain),
    ) {
        if a == b || a.ends_with(&format!(".{b}")) {
            return true;
        }
    }
    names_match(employer, &company.name)
}

/// Check every current job a person has: their headline employer and any
/// job with no end date. `jobs` are (employer name, employer domain).
pub fn check<'a>(
    jobs: impl IntoIterator<Item = (&'a str, Option<&'a str>)>,
    companies: &[Company],
) -> Verdict {
    let mut any = false;
    for (name, domain) in jobs {
        if name.trim().is_empty() && domain.is_none() {
            continue;
        }
        any = true;
        if companies.iter().any(|c| is_company(name, domain, c)) {
            return Verdict::LockedOut;
        }
    }
    if any {
        Verdict::Clear
    } else {
        Verdict::Unknown
    }
}

/// Companies whose staff are always kept out of a role: the client the role
/// is for, and every off-limits client. Callers cannot opt out of this list.
pub async fn locked_out(
    pool: &PgPool,
    org_id: Uuid,
    role_id: Uuid,
) -> anyhow::Result<Vec<Company>> {
    let rows: Vec<(Uuid, String, Option<String>, bool)> = sqlx::query_as(
        "SELECT c.id, c.name, c.domain, c.id IS NOT DISTINCT FROM r.client_id
         FROM client c LEFT JOIN role r ON r.id = $2 AND r.org_id = $1
         WHERE c.org_id = $1 AND (c.off_limits OR c.id = r.client_id)
         ORDER BY lower(c.name), c.id",
    )
    .bind(org_id)
    .bind(role_id)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(id, name, domain, hiring)| Company {
            id,
            name,
            domain,
            hiring,
        })
        .collect())
}

/// Check a saved person against a role, from what is on record now. Every
/// shortlist and send must call this (and pass the result to
/// `policy::may_send`), because people move jobs after they were found.
pub async fn check_person(
    pool: &PgPool,
    org_id: Uuid,
    role_id: Uuid,
    person_id: Uuid,
) -> anyhow::Result<Verdict> {
    let companies = locked_out(pool, org_id, role_id).await?;
    let jobs: Vec<(String, Option<String>)> = sqlx::query_as(
        "SELECT coalesce(current_employer, ''), current_employer_domain FROM person
         WHERE id = $1 AND org_id = $2
           AND (current_employer IS NOT NULL OR current_employer_domain IS NOT NULL)
         UNION ALL
         SELECT employer, employer_domain FROM employment
         WHERE person_id = $1 AND org_id = $2 AND end_date IS NULL",
    )
    .bind(person_id)
    .bind(org_id)
    .fetch_all(pool)
    .await?;
    Ok(check(
        jobs.iter().map(|(n, d)| (n.as_str(), d.as_deref())),
        &companies,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil;

    fn acme() -> Company {
        Company {
            id: Uuid::nil(),
            name: "Acme Payments Ltd".into(),
            domain: Some("acmepay.com".into()),
            hiring: true,
        }
    }

    #[test]
    fn names_match_regardless_of_case_punctuation_suffix_and_the() {
        for e in [
            "Acme Payments",
            "ACME PAYMENTS, INC.",
            "acme-payments llc",
            "The Acme Payments Company",
        ] {
            assert!(is_company(e, None, &acme()), "{e} should match");
        }
    }

    #[test]
    fn short_and_long_forms_of_a_name_match() {
        for e in ["Acme", "Acme Payments Group", "Acme Payments (UK)"] {
            assert!(is_company(e, None, &acme()), "{e} should match");
        }
    }

    #[test]
    fn different_companies_do_not_match() {
        for e in ["Payments Acme", "Acmeco", "Acm", "", "   "] {
            assert!(!is_company(e, None, &acme()), "{e:?} should not match");
        }
        let block = Company {
            name: "Block".into(),
            domain: None,
            ..acme()
        };
        assert!(!is_company("Blockchain.com", None, &block));
        let co = Company {
            name: "Co".into(),
            domain: None,
            ..acme()
        };
        assert!(
            !is_company("Co Ventures", None, &co),
            "too short to prefix-match"
        );
    }

    #[test]
    fn accents_are_ignored() {
        let sg = Company {
            name: "Société Générale".into(),
            domain: None,
            ..acme()
        };
        assert!(is_company("Societe Generale", None, &sg));
    }

    #[test]
    fn other_scripts_and_special_letters_match() {
        let lodz = Company {
            name: "Łódź Tech".into(),
            domain: None,
            ..acme()
        };
        assert!(is_company("Lodz Tech", None, &lodz));
        let bank = Company {
            name: "Мой Банк".into(),
            domain: None,
            ..acme()
        };
        assert!(is_company("МОЙ БАНК", None, &bank));
        assert!(!is_company("Другой Банк", None, &bank));
        assert_eq!(normalise_name("Ørsted A/S"), "orsted a s");
    }

    #[test]
    fn domains_match_including_subdomains() {
        assert!(is_company(
            "Something Else",
            Some("https://www.AcmePay.com/careers"),
            &acme()
        ));
        assert!(is_company("x", Some("uk.acmepay.com"), &acme()));
        assert!(!is_company("x", Some("notacmepay.com"), &acme()));
    }

    #[test]
    fn domains_are_cleaned_or_refused() {
        assert_eq!(
            normalise_domain(" https://www.Example.com:443/a?b ").as_deref(),
            Some("example.com")
        );
        for bad in [
            "",
            "example",
            ".com",
            "exa mple.com",
            "example.com.",
            "a..com",
        ] {
            assert_eq!(normalise_domain(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn any_current_job_counts_and_unknown_is_not_clear() {
        let c = [acme()];
        assert_eq!(check([("Kraken", None)], &c), Verdict::Clear);
        assert_eq!(
            check([("Kraken", None), ("Acme Payments", None)], &c),
            Verdict::LockedOut,
            "a second current job at the client"
        );
        assert_eq!(check([("", Some("acmepay.com"))], &c), Verdict::LockedOut);
        assert_eq!(check([("", None)], &c), Verdict::Unknown);
        assert_eq!(check(std::iter::empty(), &c), Verdict::Unknown);
    }

    #[tokio::test]
    async fn a_saved_person_who_moved_to_the_client_is_locked_out() {
        let Some(pool) = testutil::pool().await else {
            return;
        };
        let org = testutil::org(&pool).await;
        let (role, _) = testutil::role_with_brief(&pool, org).await;
        let person: Uuid = sqlx::query_scalar(
            "INSERT INTO person (org_id, full_name, current_employer) VALUES ($1, 'P', 'Kraken') RETURNING id",
        )
        .bind(org)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(
            check_person(&pool, org, role, person).await.unwrap(),
            Verdict::Clear
        );
        // A current job at the role's client (testutil's "Test Client").
        sqlx::query(
            "INSERT INTO employment (org_id, person_id, employer, employer_domain)
             VALUES ($1, $2, 'Other', 'test-client.example')",
        )
        .bind(org)
        .bind(person)
        .execute(&pool)
        .await
        .unwrap();
        assert_eq!(
            check_person(&pool, org, role, person).await.unwrap(),
            Verdict::LockedOut
        );
    }
}
