//! Turn a confirmed brief into People Data Labs searches (SRS F5, F8).
//!
//! What narrows the search: job titles (any one), title levels (any one,
//! minus excluded titles), fewest years, required tools, Must domains (any
//! one of them), employer types and location. The hiring client, off-limits
//! clients and the leave-out list are always excluded. Must-haves,
//! capabilities, nice-to-have tools, standards, certifications and Plus
//! domains only rank, later. Each location is its own search, so a small market such
//! as Dubai is counted and pulled on its own.
//!
//! PDL stores `job_title` as one exact value, so a phrase only matches the
//! whole title. Titles and levels therefore match as "contains" wildcards,
//! and excluded levels use PDL's level tags where one exists. PDL allows at
//! most 20 wildcards in one search; the caps below keep every search inside
//! that, and the brief cannot be confirmed past them.
//!
//! Pure, so it is tested without a network.

use serde_json::{json, Value};

use crate::{
    domain::{BriefLines, DomainWeight, ToolStatus},
    employer::{normalise_domain, Company},
};

/// At most this many separate location searches per brief.
pub const MAX_LOCATIONS: usize = 10;
/// Label for the single search when remote counts and no city is given.
pub const ANYWHERE: &str = "Anywhere (remote)";
/// PDL refuses a search with more wildcards than this.
pub const MAX_WILDCARDS: usize = 20;
/// Job titles searched (each is one wildcard).
pub const MAX_TITLES: usize = 10;
/// Levels searched (each is one wildcard).
pub const MAX_LEVELS: usize = 6;
/// Excluded titles with no PDL level tag (each is one wildcard).
pub const MAX_PLAIN_EXCLUSIONS: usize = MAX_WILDCARDS - MAX_TITLES - MAX_LEVELS;

#[derive(Debug, Clone, PartialEq)]
pub struct LocationSearch {
    pub label: String,
    pub query: Value,
}

/// PDL industries that stand for each employer type in the brief. Types the
/// resourcer typed in themselves match the industry by phrase.
fn industries(employer_type: &str) -> Vec<&'static str> {
    match employer_type {
        "Payments and neobanks" => vec!["financial services", "banking"],
        "Crypto and digital assets" => vec![
            "financial services",
            "computer software",
            "internet",
            "information technology and services",
        ],
        "Trading firms" => vec![
            "capital markets",
            "investment management",
            "investment banking",
            "financial services",
        ],
        "E-commerce" => vec!["internet", "retail", "consumer goods"],
        "Adtech" => vec!["marketing and advertising", "internet"],
        "Banks" => vec!["banking"],
        "Insurance" => vec!["insurance"],
        "Asset and wealth managers" => vec!["investment management"],
        "Exchanges and market infrastructure" => vec!["capital markets"],
        "VC and private equity" => vec!["venture capital & private equity"],
        "Cybersecurity vendors" => vec!["computer & network security"],
        "Cloud and SaaS" => vec!["computer software"],
        "Big tech and platforms" => vec!["internet"],
        "IT services" => vec!["information technology and services"],
        "Consulting and Big Four" => vec!["management consulting", "accounting"],
        "Telecoms" => vec!["telecommunications"],
        "Gaming and betting" => vec!["computer games", "gambling & casinos"],
        "Government and defence" => vec!["government administration", "defense & space"],
        "Energy and commodities" => vec!["oil & energy", "mining & metals"],
        "Healthcare" => vec!["hospital & health care"],
        _ => vec![],
    }
}

/// PDL's level tag for an excluded title word, when there is one. PDL
/// writes "VP" out as "vice president" in titles, so only the tag finds it.
pub fn level_tag(word: &str) -> Option<&'static str> {
    match word.trim().to_lowercase().as_str() {
        "manager" => Some("manager"),
        "director" => Some("director"),
        "vp" | "vice president" | "svp" | "evp" => Some("vp"),
        "chief" | "cxo" | "c-level" | "c-suite" => Some("cxo"),
        "owner" => Some("owner"),
        "partner" => Some("partner"),
        _ => None,
    }
}

/// Excluded title words that need a wildcard because PDL has no tag for them.
pub fn plain_exclusions(excluded: &[String]) -> Vec<&String> {
    excluded.iter().filter(|w| level_tag(w).is_none()).collect()
}

fn phrase(field: &str, text: &str) -> Value {
    json!({"match_phrase": {field: text.to_lowercase()}})
}

/// The text anywhere inside a keyword field such as `job_title`.
fn contains(field: &str, text: &str) -> Value {
    let mut pattern = String::from("*");
    for c in text.trim().to_lowercase().chars() {
        if matches!(c, '*' | '?' | '\\') {
            pattern.push('\\');
        }
        pattern.push(c);
    }
    pattern.push('*');
    json!({"wildcard": {field: pattern}})
}

/// At least one of the clauses. PDL refuses `minimum_should_match`, but a
/// bool with only `should` clauses already needs one of them to match.
fn any_of(clauses: Vec<Value>) -> Value {
    json!({"bool": {"should": clauses}})
}

/// A word or phrase anywhere a person describes their work. These are text
/// fields (or the skills list), so a phrase matches inside them.
fn mentions(text: &str) -> Vec<Value> {
    vec![
        phrase("headline", text),
        phrase("summary", text),
        phrase("job_summary", text),
        json!({"term": {"skills": text.to_lowercase()}}),
    ]
}

/// PDL's metro for a US city, from its canonical list (US only). The metro
/// takes in the boroughs and suburbs a city name alone misses, such as
/// Brooklyn for New York.
fn us_metro(city: &str) -> Option<&'static str> {
    Some(match city {
        "new york" | "new york city" | "nyc" => "new york, new york",
        "san francisco" => "san francisco, california",
        "los angeles" => "los angeles, california",
        "chicago" => "chicago, illinois",
        "boston" => "boston, massachusetts",
        "washington" | "washington dc" | "washington d.c." => "washington, district of columbia",
        "seattle" => "seattle, washington",
        "austin" => "austin, texas",
        "miami" => "miami, florida",
        "dallas" => "dallas, texas",
        "denver" => "denver, colorado",
        "atlanta" => "atlanta, georgia",
        _ => return None,
    })
}

/// A place as the brief names it ("London", "London, UK", "Dubai") matched
/// against how PDL stores it ("london, england, united kingdom"). PDL's
/// location fields hold one exact value each.
///
/// A city that shares its name with a US state means the city: "New York"
/// is the city and its metro, never the whole state. "New York State"
/// means the state.
fn place(raw: &str) -> Value {
    let head = raw.split(',').next().unwrap_or(raw).trim().to_lowercase();
    if let Some(state) = head.strip_suffix(" state") {
        return any_of(vec![phrase("location_region", state.trim())]);
    }
    let metro = us_metro(&head);
    let city = match head.as_str() {
        "nyc" | "new york city" => "new york",
        "washington dc" | "washington d.c." => "washington",
        h => h,
    };
    let mut fields = vec![phrase("location_locality", city)];
    if let Some(m) = metro {
        fields.push(phrase("location_metro", m));
    } else {
        // Not a known US city: it may be a region (an emirate, a state) or a country.
        fields.push(phrase("location_region", city));
    }
    fields.push(phrase("location_country", city));
    fields.push(phrase("location_name", city));
    any_of(fields)
}

/// The searches to run for a confirmed brief, one per location.
pub fn plan(lines: &BriefLines, locked_out: &[Company]) -> Vec<LocationSearch> {
    let mut must: Vec<Value> = Vec::new();
    let mut must_not: Vec<Value> = Vec::new();

    if !lines.titles.is_empty() {
        must.push(any_of(
            lines
                .titles
                .iter()
                .take(MAX_TITLES)
                .map(|t| contains("job_title", t))
                .collect(),
        ));
    }
    if !lines.levels.is_empty() {
        must.push(any_of(
            lines
                .levels
                .iter()
                .take(MAX_LEVELS)
                .map(|l| contains("job_title", l))
                .collect(),
        ));
    }
    let mut tags: Vec<&str> = lines
        .excluded_titles
        .iter()
        .filter_map(|w| level_tag(w))
        .collect();
    tags.sort_unstable();
    tags.dedup();
    if !tags.is_empty() {
        must_not.push(json!({"terms": {"job_title_levels": tags}}));
    }
    must_not.extend(
        plain_exclusions(&lines.excluded_titles)
            .into_iter()
            .take(MAX_PLAIN_EXCLUSIONS)
            .map(|w| contains("job_title", w)),
    );
    if let Some(years) = lines.min_years.filter(|y| *y > 0) {
        must.push(json!({"range": {"inferred_years_experience": {"gte": years}}}));
    }

    for tool in lines
        .tools
        .iter()
        .filter(|t| t.status == Some(ToolStatus::Required))
    {
        must.push(any_of(mentions(&tool.name)));
    }

    let must_domains: Vec<Value> = lines
        .domains
        .iter()
        .filter(|d| d.weight == DomainWeight::Must)
        .flat_map(|d| {
            let mut m = mentions(&d.name);
            m.push(phrase("job_company_industry", &d.name));
            m
        })
        .collect();
    if !must_domains.is_empty() {
        must.push(any_of(must_domains));
    }

    let mut employer: Vec<Value> = Vec::new();
    for t in &lines.employer_types {
        let known = industries(t);
        if known.is_empty() {
            employer.push(phrase("job_company_industry", t));
        }
        for i in known {
            let term = json!({"term": {"job_company_industry": i}});
            if !employer.contains(&term) {
                employer.push(term);
            }
        }
    }
    if !employer.is_empty() {
        must.push(any_of(employer));
    }

    for c in locked_out {
        must_not.push(phrase("job_company_name", &c.name));
        if let Some(d) = c.domain.as_deref().and_then(normalise_domain) {
            must_not.push(json!({"term": {"job_company_website": d}}));
        }
    }
    must_not.extend(
        lines
            .leave_out
            .iter()
            .map(|c| phrase("job_company_name", c)),
    );

    let base = |location: Option<&str>| {
        let mut m = must.clone();
        if let Some(l) = location {
            m.push(place(l));
        }
        json!({"bool": {"must": m, "must_not": must_not}})
    };

    if lines.locations.is_empty() {
        // Remote only (the brief cannot be confirmed without a location or remote).
        return vec![LocationSearch {
            label: ANYWHERE.into(),
            query: base(None),
        }];
    }
    lines
        .locations
        .iter()
        .take(MAX_LOCATIONS)
        .map(|l| LocationSearch {
            label: l.clone(),
            query: base(Some(l)),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_offered_employer_type_maps_to_industries() {
        let editor = include_str!("../../web/src/screens/BriefEditor.tsx");
        for t in crate::retune::EMPLOYER_TYPES {
            assert!(!industries(t).is_empty(), "{t} has no industry");
            assert!(
                editor.contains(&format!("\"{t}\"")),
                "{t} is not offered in the brief editor"
            );
        }
    }
    use crate::domain::{BriefDomain, BriefTool};
    use uuid::Uuid;

    fn brief() -> BriefLines {
        BriefLines {
            titles: vec!["Security Engineer".into(), "DevSecOps Engineer".into()],
            min_years: Some(7),
            frameworks: vec!["DORA".into()],
            certifications: vec!["CISSP".into()],
            analysis: "Hands-on role.".into(),
            levels: vec!["Senior".into(), "Lead".into()],
            excluded_titles: vec!["Director".into()],
            must_haves: vec!["Cloud security".into()],
            capabilities: vec!["Mentoring".into()],
            domains: vec![
                BriefDomain {
                    name: "Privileged access".into(),
                    weight: DomainWeight::Must,
                },
                BriefDomain {
                    name: "Custody".into(),
                    weight: DomainWeight::Plus,
                },
            ],
            tools: vec![
                BriefTool {
                    name: "Okta".into(),
                    status: Some(ToolStatus::Required),
                },
                BriefTool {
                    name: "Terraform".into(),
                    status: Some(ToolStatus::Nice),
                },
                BriefTool {
                    name: "CyberArk".into(),
                    status: Some(ToolStatus::Replacing),
                },
            ],
            locations: vec!["New York".into(), "Dubai".into()],
            remote: false,
            employer_types: vec!["Trading firms".into(), "Payments and neobanks".into()],
            leave_out: vec!["Some Bank".into()],
        }
    }

    fn client() -> Company {
        Company {
            id: Uuid::nil(),
            name: "Client A".into(),
            domain: Some("https://www.client-a.com/".into()),
            hiring: true,
        }
    }

    fn text(v: &Value) -> String {
        v.to_string()
    }

    #[test]
    fn one_search_per_location() {
        let p = plan(&brief(), &[client()]);
        assert_eq!(
            p.iter().map(|s| s.label.as_str()).collect::<Vec<_>>(),
            ["New York", "Dubai"]
        );
        assert!(text(&p[1].query).contains("\"location_locality\":\"dubai\""));
        assert!(!text(&p[1].query).contains("new york"));
    }

    #[test]
    fn required_tools_and_must_domains_narrow_the_rest_only_rank() {
        let q = text(&plan(&brief(), &[])[0].query);
        for narrows in [
            "okta",
            "privileged access",
            "senior",
            "lead",
            "security engineer",
            "devsecops engineer",
        ] {
            assert!(q.contains(narrows), "{narrows} should be in the search");
        }
        for ranks in [
            "terraform",
            "cyberark",
            "custody",
            "mentoring",
            "cloud security",
            "dora",
            "cissp",
            "hands-on",
        ] {
            assert!(!q.contains(ranks), "{ranks} should only rank");
        }
    }

    #[test]
    fn a_title_and_a_level_must_both_match() {
        let q = plan(&brief(), &[]).remove(0).query;
        let must = q["bool"]["must"].as_array().unwrap();
        let titles = text(&must[0]);
        assert!(titles.contains("{\"wildcard\":{\"job_title\":\"*security engineer*\"}}"));
        assert!(!titles.contains("senior"), "titles and levels are separate");
        assert!(text(&must[1]).contains("{\"wildcard\":{\"job_title\":\"*senior*\"}}"));
    }

    #[test]
    fn fewest_years_narrows_and_none_does_not() {
        let q = text(&plan(&brief(), &[])[0].query);
        assert!(
            q.contains("\"inferred_years_experience\":{\"gte\":7}"),
            "{q}"
        );
        let mut b = brief();
        b.min_years = None;
        assert!(!text(&plan(&b, &[])[0].query).contains("inferred_years_experience"));
        b.min_years = Some(0);
        assert!(!text(&plan(&b, &[])[0].query).contains("inferred_years_experience"));
    }

    #[test]
    fn locked_out_companies_and_leave_out_are_excluded() {
        let q = plan(&brief(), &[client()]).remove(0).query;
        let not = text(&q["bool"]["must_not"]);
        assert!(not.contains("\"job_company_name\":\"client a\""));
        assert!(
            not.contains("\"job_company_website\":\"client-a.com\""),
            "{not}"
        );
        assert!(not.contains("some bank"));
        assert!(
            not.contains("{\"terms\":{\"job_title_levels\":[\"director\"]}}"),
            "{not}"
        );
    }

    #[test]
    fn employer_types_become_industries_without_repeats() {
        let q = plan(&brief(), &[]).remove(0).query;
        let s = text(&q);
        assert!(s.contains("capital markets") && s.contains("banking"));
        assert_eq!(s.matches("\"financial services\"").count(), 1);
        let mut own = brief();
        own.employer_types = vec!["Insurance".into()];
        let s = text(&plan(&own, &[]).remove(0).query);
        assert!(s.contains("\"job_company_industry\":\"insurance\""));
    }

    #[test]
    fn a_place_matches_by_its_first_part() {
        let q = text(&place("London, UK"));
        assert!(q.contains("\"location_locality\":\"london\""));
        assert!(q.contains("\"location_country\":\"london\""));
        assert!(!q.contains("uk"));
        assert!(q.contains("\"location_region\":\"london\""));
    }

    #[test]
    fn new_york_means_the_city_and_its_metro_not_the_state() {
        for raw in ["New York", "New York, NY", "NYC", "new york city"] {
            let q = text(&place(raw));
            assert!(
                q.contains("\"location_locality\":\"new york\""),
                "{raw}: {q}"
            );
            assert!(
                q.contains("\"location_metro\":\"new york, new york\""),
                "{raw}: {q}"
            );
            assert!(
                !q.contains("location_region"),
                "{raw}: the whole state: {q}"
            );
        }
        let state = text(&place("New York State"));
        assert_eq!(
            state,
            "{\"bool\":{\"should\":[{\"match_phrase\":{\"location_region\":\"new york\"}}]}}"
        );
        // Regions that are not also a city still match as regions.
        assert!(text(&place("California")).contains("\"location_region\":\"california\""));
        assert!(text(&place("Abu Dhabi")).contains("\"location_region\":\"abu dhabi\""));
        let dc = text(&place("Washington DC"));
        assert!(dc.contains("\"location_metro\":\"washington, district of columbia\""));
        assert!(
            !dc.contains("location_region"),
            "not Washington State: {dc}"
        );
    }

    #[test]
    fn remote_only_is_one_search_anywhere() {
        let mut b = brief();
        b.locations.clear();
        b.remote = true;
        let p = plan(&b, &[]);
        assert_eq!(p.len(), 1);
        assert_eq!(p[0].label, ANYWHERE);
        assert!(!text(&p[0].query).contains("location_name"));
    }

    /// Every key PDL accepts in a query. Anything else (such as
    /// `minimum_should_match` or `boost`) is refused with a 400.
    fn only_pdl_clauses(v: &Value, path: &str) {
        const QUERIES: [&str; 10] = [
            "term",
            "terms",
            "exists",
            "bool",
            "match",
            "range",
            "match_phrase",
            "wildcard",
            "prefix",
            "match_all",
        ];
        const BOOL: [&str; 4] = ["must", "must_not", "should", "filter"];
        let obj = v
            .as_object()
            .unwrap_or_else(|| panic!("{path} is not a query"));
        for (k, inner) in obj {
            assert!(
                QUERIES.contains(&k.as_str()),
                "{path}.{k} is not allowed by PDL"
            );
            if k == "bool" {
                for (clause, list) in inner.as_object().unwrap() {
                    assert!(
                        BOOL.contains(&clause.as_str()),
                        "{path}.bool.{clause} is not allowed by PDL"
                    );
                    for (i, q) in list.as_array().unwrap().iter().enumerate() {
                        only_pdl_clauses(q, &format!("{path}.bool.{clause}[{i}]"));
                    }
                }
            }
        }
    }

    #[test]
    fn every_search_uses_only_what_pdl_accepts() {
        for s in plan(&brief(), &[client()]) {
            only_pdl_clauses(&s.query, &s.label);
        }
        let mut remote = brief();
        remote.locations.clear();
        remote.remote = true;
        for s in plan(&remote, &[]) {
            only_pdl_clauses(&s.query, &s.label);
        }
    }

    fn wildcards(v: &Value) -> usize {
        text(v).matches("\"wildcard\"").count()
    }

    #[test]
    fn excluded_levels_use_pdl_tags_and_other_words_a_wildcard() {
        let mut b = brief();
        b.excluded_titles = [
            "Manager",
            "Director",
            "Head of",
            "VP",
            "Chief",
            "vice president",
        ]
        .map(String::from)
        .to_vec();
        let q = plan(&b, &[]).remove(0).query;
        let not = text(&q["bool"]["must_not"]);
        assert!(
            not.contains(
                "{\"terms\":{\"job_title_levels\":[\"cxo\",\"director\",\"manager\",\"vp\"]}}"
            ),
            "{not}"
        );
        assert!(not.contains("{\"wildcard\":{\"job_title\":\"*head of*\"}}"));
        assert!(
            !not.contains("*vp*"),
            "PDL writes VP out, so a wildcard never matches"
        );
    }

    #[test]
    fn tools_and_domains_match_inside_text_fields_not_the_exact_title() {
        let q = text(&plan(&brief(), &[])[0].query);
        assert!(q.contains("{\"match_phrase\":{\"summary\":\"okta\"}}"));
        assert!(q.contains("{\"match_phrase\":{\"headline\":\"okta\"}}"));
        assert!(q.contains("{\"term\":{\"skills\":\"okta\"}}"));
        assert!(!q.contains("{\"match_phrase\":{\"job_title\""), "{q}");
    }

    #[test]
    fn wildcard_text_is_escaped() {
        assert_eq!(
            contains("job_title", " C++ *Lead?\\ "),
            json!({"wildcard": {"job_title": "*c++ \\*lead\\?\\\\*"}})
        );
    }

    #[test]
    fn a_full_brief_stays_within_pdls_wildcard_limit() {
        let mut b = brief();
        b.titles = (0..15).map(|i| format!("Title {i}")).collect();
        b.levels = (0..9).map(|i| format!("Level {i}")).collect();
        b.excluded_titles = (0..9).map(|i| format!("Not {i}")).collect();
        b.excluded_titles.push("Director".into());
        let q = plan(&b, &[client()]).remove(0).query;
        assert_eq!(wildcards(&q), MAX_WILDCARDS);
        assert!(text(&q).contains("title 9") && !text(&q).contains("title 10"));
    }

    #[test]
    fn locations_are_capped() {
        let mut b = brief();
        b.locations = (0..15).map(|i| format!("City {i}")).collect();
        assert_eq!(plan(&b, &[]).len(), MAX_LOCATIONS);
    }
}
