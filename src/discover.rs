pub use crate::history::crate_from_source;
use crate::history::{History, utc_date};
use anyhow::{Context, Result, ensure};
use jiff::{Timestamp, civil::Date};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    thread,
    time::{Duration, Instant},
};

#[derive(Clone, Copy)]
pub enum SourceKind {
    Crates,
    Github,
}
pub fn normalize_repository(value: &str) -> Option<String> {
    let value = value.trim().strip_prefix("git+").unwrap_or(value.trim());
    let path = value
        .strip_prefix("https://")
        .or_else(|| value.strip_prefix("http://"))?;
    let path = path.split(['?', '#']).next()?.trim_end_matches('/');
    let path = path.split_once("/tree/").map_or(path, |(root, _)| root);
    let path = path.strip_suffix(".git").unwrap_or(path);
    let mut parts = path.split('/');
    let (host, owner, repo) = (parts.next()?, parts.next()?, parts.next()?);
    (!owner.is_empty() && !repo.is_empty() && host.contains('.') && !host.contains('@'))
        .then(|| format!("https://{}", path.to_lowercase()))
}
pub fn collect_pages(
    kind: SourceKind,
    url: &str,
    since: Date,
    mut fetch: impl FnMut(&str) -> Result<String>,
    mut accept: impl FnMut(&Value, Timestamp) -> Result<()>,
) -> Value {
    let github = matches!(kind, SourceKind::Github);
    let limit = if github { 3 } else { 10 };
    let mut report = json!({"url":url,"status":"ok","page_count":0,"item_count":0});
    let mut next = url.to_owned();
    let mut count = 0;
    let result = (|| -> Result<()> {
        for number in 1..=limit {
            if github {
                next = format!("{url}&page={number}");
            }
            let page: Value = serde_json::from_str(&fetch(&next)?)?;
            let rows = page[if github { "items" } else { "crates" }]
                .as_array()
                .context("missing items")?;
            let cursor = if github {
                None
            } else {
                Some(
                    page.pointer("/meta/next_page")
                        .context("missing next_page")?,
                )
            };
            if github
                && page["incomplete_results"]
                    .as_bool()
                    .context("missing completeness flag")?
            {
                report["status"] = "truncated".into();
                report["message"] = "github reported incomplete search results".into();
            }
            report["page_count"] = number.into();
            let mut older = false;
            for row in rows {
                let created = row["created_at"]
                    .as_str()
                    .context("missing creation time")?;
                let at: Timestamp = created.parse()?;
                if utc_date(at) < since {
                    older = true;
                    continue;
                }
                accept(row, at)?;
                count += 1;
            }
            let more = if github {
                rows.len() == 50
            } else {
                !older && cursor.is_some_and(|value| !value.is_null())
            };
            if !more {
                break;
            }
            if number == limit {
                report["status"] = "truncated".into();
                report["message"] = format!("stopped at the {limit}-page limit").into();
                break;
            }
            if let Some(cursor) = cursor {
                let query = cursor
                    .as_str()
                    .context("invalid next_page")?
                    .trim_start_matches("https://crates.io")
                    .trim_start_matches("/api/v1/crates");
                ensure!(query.starts_with('?'), "unexpected next-page URL");
                next = format!("https://crates.io/api/v1/crates{query}");
            }
        }
        Ok(())
    })();
    if let Err(error) = result {
        report["status"] = "error".into();
        report["message"] = error.to_string().into();
    }
    report["item_count"] = count.into();
    report
}
#[derive(Default)]
struct Candidate<'a> {
    names: BTreeSet<String>,
    known_as: BTreeSet<&'a str>,
    sources: BTreeMap<String, (Timestamp, &'static str)>,
}
pub struct Candidates<'a> {
    known: HashMap<String, &'a str>,
    repositories: HashMap<&'a str, String>,
    items: BTreeMap<String, Candidate<'a>>,
}
impl<'a> Candidates<'a> {
    pub fn new(history: &'a History) -> Self {
        let mut result = Self {
            known: HashMap::new(),
            repositories: HashMap::new(),
            items: BTreeMap::new(),
        };
        let frameworks = history.frameworks.iter().map(|f| {
            (
                f.id.as_str(),
                Some(f.repository.as_str()),
                &f.historical_repository_urls,
                &f.crate_names,
            )
        });
        let excluded = history.not_included.iter().map(|f| {
            (
                f.id.as_str(),
                f.repository.as_deref(),
                &f.historical_repository_urls,
                &f.crate_names,
            )
        });
        for (id, repository, aliases, crates) in frameworks.chain(excluded) {
            for url in repository
                .into_iter()
                .chain(aliases.iter().map(String::as_str))
                .filter_map(normalize_repository)
            {
                result.known.insert(url, id);
            }
            for name in crates {
                let name = name.to_lowercase().replace('_', "-");
                result.known.insert(format!("crate:{name}"), id);
            }
            if let Some(url) = repository.and_then(normalize_repository) {
                result.repositories.insert(id, url);
            }
        }
        result
    }
    pub fn push(&mut self, kind: SourceKind, item: &Value, at: Timestamp) -> Result<()> {
        let github = matches!(kind, SourceKind::Github);
        let name_field = if github { "full_name" } else { "id" };
        let name = item[name_field].as_str().context("missing name")?;
        let url = if github {
            item["html_url"]
                .as_str()
                .context("missing repository URL")?
                .to_owned()
        } else {
            format!("https://crates.io/crates/{name}")
        };
        let repository = item["repository"]
            .as_str()
            .or_else(|| github.then_some(url.as_str()));
        let mut repository = repository.and_then(normalize_repository);
        let crate_key = format!("crate:{}", name.to_lowercase().replace('_', "-"));
        let repo_id = repository.as_ref().and_then(|u| self.known.get(u)).copied();
        if let Some(root) = repo_id.and_then(|id| self.repositories.get(id))
            && let Some(url) = &mut repository
        {
            url.clear();
            url.push_str(root);
        }
        let crate_id = self.known.get(&crate_key).copied();
        let key = repository.unwrap_or(crate_key);
        let candidate = self.items.entry(key).or_default();
        candidate
            .known_as
            .extend(repo_id.into_iter().chain(crate_id));
        candidate.names.insert(name.to_owned());
        let meaning = if github {
            "repository-creation"
        } else {
            "crate-name-first-publication"
        };
        candidate.sources.insert(url, (at, meaning));
        Ok(())
    }
    pub fn finish(self) -> Value {
        Value::Array(self.items.into_iter().map(|(key, item)| {
            let earliest = item.sources.values().min_by_key(|(at, _)| *at);
            let sources: Vec<_> = item.sources.iter().map(|(url, (at, meaning))| json!({"url":url,"created_at":at,"timestamp_meaning":meaning})).collect();
            json!({"key":key,"repository":key.starts_with("https://").then_some(&key),"names":item.names,"known_as":item.known_as,"earliest_created_at":earliest.map(|(at,_)|at),"timestamp_meaning":earliest.map(|(_,meaning)|meaning),"sources":sources})
        }).collect())
    }
}
fn http_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .user_agent(
            "dayssincelastrustframework-discovery (+https://dayssincelastrustframework.com)",
        )
        .timeout_global(Some(Duration::from_secs(30)))
        .max_redirects(0)
        .build()
        .into()
}

pub fn run(history: &History, since: Date) -> (Value, bool) {
    let agent = http_agent();
    let mut candidates = Candidates::new(history);
    let mut sources = Vec::new();
    let mut last: Option<Instant> = None;
    for (kind, query) in [
        (
            SourceKind::Crates,
            "category=web-programming%3A%3Ahttp-server",
        ),
        (SourceKind::Crates, "keyword=web-framework"),
        (SourceKind::Crates, "q=web%20framework"),
        (SourceKind::Crates, "q=ssr"),
        (SourceKind::Github, "topic%3Aweb-framework"),
        (
            SourceKind::Github,
            "%22web%20framework%22+in%3Aname%2Cdescription",
        ),
    ] {
        let (url, delay) = match kind {
            SourceKind::Crates => (
                format!("https://crates.io/api/v1/crates?sort=new&per_page=100&{query}"),
                1,
            ),
            SourceKind::Github => (
                format!(
                    "https://api.github.com/search/repositories?q={query}+language%3ARust+created%3A%3E%3D{since}&sort=updated&per_page=50"
                ),
                7,
            ),
        };
        sources.push(collect_pages(
            kind,
            &url,
            since,
            |url| {
                if let Some(last) = last {
                    thread::sleep(Duration::from_secs(delay).saturating_sub(last.elapsed()));
                }
                last = Some(Instant::now());
                Ok(agent
                    .get(url)
                    .header("Accept", "application/json")
                    .call()?
                    .body_mut()
                    .with_config()
                    .limit(2 * 1024 * 1024)
                    .read_to_string()?)
            },
            |item, at| candidates.push(kind, item, at),
        ));
    }
    let failed = sources.iter().any(|source| source["status"] != "ok");
    (
        json!({"ran_at":Timestamp::now(), "since":since, "sources":sources, "candidates":candidates.finish()}),
        failed,
    )
}

#[cfg(test)]
mod tls_tests {
    use super::*;

    fn assert_certificate_rejected(agent: &ureq::Agent, url: &str, reason: &str) -> Result<()> {
        let error = agent
            .get(url)
            .call()
            .err()
            .context("invalid certificate accepted")?;
        let diagnostic = format!("{error:?}");
        ensure!(
            diagnostic.contains(reason),
            "expected {reason} for {url}, received {diagnostic}"
        );
        println!("{url}: {diagnostic}");
        Ok(())
    }

    #[test]
    #[ignore = "contacts public TLS fixtures; run explicitly with --ignored --nocapture"]
    fn incident_tls_chains_are_rejected() -> Result<()> {
        let agent = http_agent();
        let mut response = agent.get("https://example.com/").call()?;
        ensure!(response.status().is_success());
        ensure!(!response.body_mut().read_to_string()?.is_empty());
        println!("valid-control: HTTP success and response body read");
        let hosts = [
            "dv-g3b",
            "dv-g3b-aaa",
            "dv-g3c",
            "dv-g3c-aaa",
            "ov-g3b",
            "ov-g3b-aaa",
            "ov-g3c",
            "ov-g3c-aaa",
            "ev-g3b",
            "ev-g3b-aaa",
            "ev-g3c",
            "ev-g3c-aaa",
            "dv-g3b-noct",
            "dv-g3b-aaa-noct",
        ];
        for host in hosts {
            let url = format!("https://{host}-emudhra.we-have-a-problem.com/");
            assert_certificate_rejected(&agent, &url, "CertDistrusted")?;
        }
        for (url, reason) in [
            ("https://wrong.host.badssl.com/", "NotValidForName"),
            ("https://self-signed.badssl.com/", "UnknownIssuer"),
        ] {
            assert_certificate_rejected(&agent, url, reason)?;
        }
        println!(
            "incident certificate rejections: {0}/{0}; invalid controls: 2/2",
            hosts.len()
        );
        Ok(())
    }
}
