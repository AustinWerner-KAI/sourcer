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
        _ => vec![],
    }
}

fn phrase(field: &str, text: &str) -> Value {
    json!({"match_phrase": {field: text.to_lowercase()}})
}

/// At least one of the clauses. PDL refuses `minimum_should_match`, but a
/// bool with only `should` clauses already needs one of them to match.
fn any_of(clauses: Vec<Value>) -> Value {
    json!({"bool": {"should": clauses}})
}

/// A word or phrase anywhere a person describes their work.
fn mentions(text: &str) -> Vec<Value> {
    vec![
        phrase("job_title", text),
        phrase("summary", text),
        json!({"term": {"skills": text.to_lowercase()}}),
    ]
}

/// A place as the brief names it ("London", "London, UK", "Dubai") matched
/// against how PDL stores it ("london, england, united kingdom").
fn place(raw: &str) -> Value {
    let head = raw.split(',').next().unwrap_or(raw).trim();
    any_of(
        [
            "location_locality",
            "location_metro",
            "location_region",
            "location_country",
            "location_name",
        ]
        .iter()
        .map(|f| phrase(f, head))
        .collect(),
    )
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
                .map(|t| phrase("job_title", t))
                .collect(),
        ));
    }
    if !lines.levels.is_empty() {
        must.push(any_of(
            lines
                .levels
                .iter()
                .map(|l| phrase("job_title", l))
                .collect(),
        ));
    }
    must_not.extend(lines.excluded_titles.iter().map(|t| phrase("job_title", t)));
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
        assert!(titles.contains("\"job_title\":\"security engineer\""));
        assert!(!titles.contains("senior"), "titles and levels are separate");
        assert!(text(&must[1]).contains("\"job_title\":\"senior\""));
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
        assert!(not.contains("\"job_title\":\"director\""));
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

    #[test]
    fn locations_are_capped() {
        let mut b = brief();
        b.locations = (0..15).map(|i| format!("City {i}")).collect();
        assert_eq!(plan(&b, &[]).len(), MAX_LOCATIONS);
    }
}
