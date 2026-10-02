//! Widening a brief: the relaxing moves that More searches are made of.
//!
//! A wider search is the confirmed brief with moves applied, and the moves can
//! only relax: a required tool becomes nice to have, a Must domain becomes
//! Plus, titles, levels, employer types or locations are added, or fewer years
//! are asked for. They are applied here in code, so neither Claude nor a
//! resourcer can narrow the search or touch the brief's own choices (excluded
//! titles, leave-out list, must-haves).

use serde::Deserialize;
use serde_json::{json, Value};

use crate::{
    domain::{BriefLines, CountLocation, DomainWeight, ToolStatus},
    plan,
};

/// The employer types Sourcer knows how to search. Each maps to People Data
/// Labs industries in `plan::industries`; the brief editor offers the same list.
pub const EMPLOYER_TYPES: &[&str] = &[
    "Payments and neobanks",
    "Crypto and digital assets",
    "Trading firms",
    "E-commerce",
    "Adtech",
    "Banks",
    "Insurance",
    "Asset and wealth managers",
    "Exchanges and market infrastructure",
    "VC and private equity",
    "Cybersecurity vendors",
    "Cloud and SaaS",
    "Big tech and platforms",
    "IT services",
    "Consulting and Big Four",
    "Telecoms",
    "Gaming and betting",
    "Government and defence",
    "Energy and commodities",
    "Healthcare",
];

const MAX_ITEM_CHARS: usize = 120;

/// Relaxing moves, as Claude gives them or as a wider search stores them.
#[derive(Debug, Default, Deserialize)]
pub struct Reply {
    #[serde(default)]
    pub tools_to_nice: Vec<String>,
    #[serde(default)]
    pub domains_to_plus: Vec<String>,
    #[serde(default)]
    pub add_titles: Vec<String>,
    #[serde(default)]
    pub add_levels: Vec<String>,
    /// Read leniently; absent means keep.
    #[serde(default)]
    pub min_years: Option<Value>,
    #[serde(default)]
    pub add_employer_types: Vec<String>,
    #[serde(default)]
    pub add_locations: Vec<String>,
    #[serde(default)]
    pub allow_remote: bool,
}

/// The search as it ran, in plain words for Claude.
pub fn situation(b: &BriefLines, counts: &[CountLocation]) -> Value {
    let tools = |st: ToolStatus| -> Vec<&str> {
        b.tools
            .iter()
            .filter(|t| t.status == Some(st))
            .map(|t| t.name.as_str())
            .collect()
    };
    let domains = |w: DomainWeight| -> Vec<&str> {
        b.domains
            .iter()
            .filter(|d| d.weight == w)
            .map(|d| d.name.as_str())
            .collect()
    };
    json!({
        "role_summary": b.analysis,
        "narrows": {
            "titles": b.titles,
            "levels": b.levels,
            "excluded_titles": b.excluded_titles,
            "min_years": b.min_years,
            "required_tools": tools(ToolStatus::Required),
            "must_domains": domains(DomainWeight::Must),
            "employer_types": b.employer_types,
            "locations": b.locations,
            "remote": b.remote,
        },
        "only_ranks": {
            "must_haves": b.must_haves,
            "nice_to_have_tools": tools(ToolStatus::Nice),
            "plus_domains": domains(DomainWeight::Plus),
        },
        "allowed_employer_types": EMPLOYER_TYPES,
        "counts": counts.iter().map(|c| json!({"location": c.label, "people": c.total})).collect::<Vec<_>>(),
    })
}

fn clean(s: &str) -> Option<String> {
    let s = s.trim();
    (!s.is_empty()).then(|| s.chars().take(MAX_ITEM_CHARS).collect())
}

/// Add new entries after the old ones, without repeats, up to `max`.
fn widen(old: &[String], new: &[String], max: usize) -> Vec<String> {
    let mut out = old.to_vec();
    for s in new.iter().filter_map(|s| clean(s)) {
        if out.len() >= max {
            break;
        }
        if !out.iter().any(|o| o.eq_ignore_ascii_case(&s)) {
            out.push(s);
        }
    }
    out
}

fn same(a: &str, b: &str) -> bool {
    a.trim().eq_ignore_ascii_case(b.trim())
}

/// Apply Claude's relaxing moves to the brief. Only ever relaxes.
pub fn relax(old: &BriefLines, r: &Reply) -> BriefLines {
    let mut b = old.clone();
    for t in &mut b.tools {
        if t.status == Some(ToolStatus::Required)
            && r.tools_to_nice.iter().any(|n| same(n, &t.name))
        {
            t.status = Some(ToolStatus::Nice);
        }
    }
    for d in &mut b.domains {
        if d.weight == DomainWeight::Must && r.domains_to_plus.iter().any(|n| same(n, &d.name)) {
            d.weight = DomainWeight::Plus;
        }
    }
    b.titles = widen(&old.titles, &r.add_titles, plan::MAX_TITLES);
    b.levels = widen(&old.levels, &r.add_levels, plan::MAX_LEVELS);
    if let (Some(now), Some(v)) = (old.min_years, &r.min_years) {
        let proposed = match v {
            Value::Null => Some(None),
            Value::Number(n) => n.as_f64().map(|y| Some(y.round() as i32)),
            Value::String(t) => t.trim().parse::<f64>().ok().map(|y| Some(y.round() as i32)),
            _ => None,
        };
        match proposed {
            Some(None) => b.min_years = None,
            Some(Some(y)) if y <= 0 => b.min_years = None,
            Some(Some(y)) if y < now => b.min_years = Some(y),
            _ => {}
        }
    }
    let allowed: Vec<String> = r
        .add_employer_types
        .iter()
        .filter_map(|t| EMPLOYER_TYPES.iter().find(|k| same(k, t)))
        .map(|k| k.to_string())
        .collect();
    // No employer types searches every industry; adding one would narrow it.
    if !old.employer_types.is_empty() {
        b.employer_types = widen(
            &old.employer_types,
            &allowed,
            EMPLOYER_TYPES.len().max(old.employer_types.len()),
        );
    }
    // A remote-only brief has no locations; adding cities would narrow it.
    if !old.locations.is_empty() {
        b.locations = widen(&old.locations, &r.add_locations, plan::MAX_LOCATIONS);
    }
    b.remote = old.remote || r.allow_remote;
    b
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{BriefDomain, BriefTool};

    fn brief() -> BriefLines {
        BriefLines {
            titles: vec!["Cloud Security Engineer".into()],
            levels: vec!["Senior".into()],
            excluded_titles: vec!["Director".into()],
            min_years: Some(7),
            must_haves: vec!["AWS security".into()],
            domains: vec![
                BriefDomain {
                    name: "Cloud security".into(),
                    weight: DomainWeight::Must,
                },
                BriefDomain {
                    name: "Custody".into(),
                    weight: DomainWeight::Plus,
                },
            ],
            tools: vec![
                BriefTool {
                    name: "AWS".into(),
                    status: Some(ToolStatus::Required),
                },
                BriefTool {
                    name: "Deep Instinct".into(),
                    status: Some(ToolStatus::Required),
                },
                BriefTool {
                    name: "CyberArk".into(),
                    status: Some(ToolStatus::Replacing),
                },
            ],
            locations: vec!["Dubai".into()],
            employer_types: vec!["Trading firms".into()],
            leave_out: vec!["Some Bank".into()],
            ..Default::default()
        }
    }

    fn reply() -> Reply {
        Reply {
            tools_to_nice: vec!["deep instinct".into(), "CyberArk".into(), "Okta".into()],
            domains_to_plus: vec!["Cloud security".into(), "Custody".into()],
            add_titles: vec![
                "DevSecOps Engineer".into(),
                "cloud security engineer".into(),
            ],
            add_levels: vec!["Lead".into()],
            min_years: Some(json!(5)),
            add_employer_types: vec!["Adtech".into(), "Tier-1 banks".into()],
            add_locations: vec!["Abu Dhabi".into()],
            allow_remote: false,
        }
    }

    #[test]
    fn relaxing_moves_are_applied() {
        let b = relax(&brief(), &reply());
        assert_eq!(b.tools[0].status, Some(ToolStatus::Required), "AWS stays");
        assert_eq!(b.tools[1].status, Some(ToolStatus::Nice));
        assert_eq!(
            b.tools[2].status,
            Some(ToolStatus::Replacing),
            "never turned into a nice to have"
        );
        assert_eq!(b.domains[0].weight, DomainWeight::Plus);
        assert_eq!(b.titles, ["Cloud Security Engineer", "DevSecOps Engineer"]);
        assert_eq!(b.levels, ["Senior", "Lead"]);
        assert_eq!(b.min_years, Some(5));
        assert_eq!(
            b.employer_types,
            ["Trading firms", "Adtech"],
            "unknown types ignored"
        );
        assert_eq!(b.locations, ["Dubai", "Abu Dhabi"]);
        assert_eq!(
            b.excluded_titles,
            ["Director"],
            "the resourcer's own choices stay"
        );
        assert_eq!(b.leave_out, ["Some Bank"]);
        assert_eq!(b.must_haves, ["AWS security"]);
    }

    #[test]
    fn a_reply_can_never_narrow() {
        let mut r = Reply {
            min_years: Some(json!(12)),
            ..Default::default()
        };
        let b = relax(&brief(), &r);
        assert_eq!(b, brief(), "more years is ignored and nothing else moves");
        let mut open = brief();
        open.min_years = None;
        r.min_years = Some(json!(3));
        assert_eq!(relax(&open, &r).min_years, None, "no limit stays no limit");
        r.min_years = Some(Value::Null);
        assert_eq!(relax(&brief(), &r).min_years, None, "null drops the limit");
        let mut remote = brief();
        remote.locations.clear();
        remote.remote = true;
        r.add_locations = vec!["London".into()];
        assert!(
            relax(&remote, &r).locations.is_empty(),
            "remote-only stays everywhere"
        );
        let mut any_employer = brief();
        any_employer.employer_types.clear();
        r.add_employer_types = vec!["Adtech".into()];
        assert!(
            relax(&any_employer, &r).employer_types.is_empty(),
            "every industry stays every industry"
        );
    }

    #[test]
    fn additions_respect_the_search_caps() {
        let r = Reply {
            add_titles: (0..20).map(|i| format!("Title {i}")).collect(),
            add_levels: (0..20).map(|i| format!("Level {i}")).collect(),
            ..Default::default()
        };
        let b = relax(&brief(), &r);
        assert_eq!(b.titles.len(), plan::MAX_TITLES);
        assert_eq!(b.levels.len(), plan::MAX_LEVELS);
        assert!(
            crate::roles::problems(&b).is_empty(),
            "{:?}",
            crate::roles::problems(&b)
        );
    }
}
