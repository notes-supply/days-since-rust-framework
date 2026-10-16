use std::{
    collections::{BTreeSet, HashSet},
    fs,
    path::Path,
};

use anyhow::{Context, Result, bail, ensure};
use jiff::{Timestamp, Unit, civil::Date, tz::TimeZone};
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct History {
    pub frameworks: Vec<Framework>,
    pub not_included: Vec<NotIncluded>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Framework {
    pub id: String,
    pub name: String,
    pub classification: Classification,
    pub description: String,
    pub repository: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub historical_repository_urls: Vec<String>,
    pub first_framework_registry_release: Option<Release>,
    #[serde(default, alias = "precursor_release")]
    pub lineage_first_framework_registry_release: Option<Release>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_release: Option<GitRelease>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub crate_names: BTreeSet<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Classification {
    Backend,
    Frontend,
    FullStack,
}
impl Classification {
    pub fn label(self) -> &'static str {
        match self {
            Self::Backend => "backend",
            Self::Frontend => "frontend",
            Self::FullStack => "full-stack",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Release {
    pub at: Timestamp,
    pub version: Option<String>,
    pub source: String,
    pub yanked: bool,
    pub prerelease: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitRelease {
    pub at: Date,
    pub source: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NotIncluded {
    pub id: String,
    pub repository: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub historical_repository_urls: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub crate_names: BTreeSet<String>,
}

impl Framework {
    pub fn release(&self, kind: ReleaseKind) -> Option<&Release> {
        match kind {
            ReleaseKind::FrameworkRegistryRelease => self.first_framework_registry_release.as_ref(),
            ReleaseKind::LineageRelease => self.lineage_first_framework_registry_release.as_ref(),
        }
    }
    pub fn first_crates_release(&self) -> Option<&Release> {
        [
            self.first_framework_registry_release.as_ref(),
            self.lineage_first_framework_registry_release.as_ref(),
        ]
        .into_iter()
        .flatten()
        .min_by_key(|release| release.at)
    }
    pub fn first_release_date(&self) -> Option<Date> {
        self.first_crates_release()
            .map(|release| utc_date(release.at))
            .or_else(|| self.git_release.as_ref().map(|release| release.at))
    }
}
impl History {
    pub fn load(path: &Path) -> Result<Self> {
        serde_json::from_slice::<Self>(&fs::read(path)?)?.validate()
    }
    pub fn framework(&self, id: &str) -> Option<&Framework> {
        self.frameworks.iter().find(|framework| framework.id == id)
    }
    pub fn validate(self) -> Result<Self> {
        let ids = self.frameworks.iter().map(|f| &f.id);
        let excluded = self.not_included.iter().map(|f| &f.id);
        let unique: HashSet<_> = ids.chain(excluded).collect();
        ensure!(
            unique.len() == self.frameworks.len() + self.not_included.len(),
            "duplicate id"
        );
        for framework in &self.frameworks {
            ensure!(
                framework.git_release.is_none() || framework.first_crates_release().is_none(),
                "git-only framework {} has a crates.io release",
                framework.id
            );
        }
        let json = serde_json::to_string(&self)?;
        let local = ["/home/", "/tmp/", "/Users/", "file:", r"C:\\"];
        ensure!(
            !local.iter().any(|path| json.contains(path)),
            "local path in projection"
        );
        Ok(self)
    }
}

#[derive(Deserialize)]
struct Catalog {
    frameworks: Vec<serde_json::Map<String, serde_json::Value>>,
    excluded_or_editorial_candidates: Vec<serde_json::Map<String, serde_json::Value>>,
}
pub fn import(path: &Path) -> Result<History> {
    use serde_json::Value;
    let catalog: Catalog = serde_json::from_slice(&fs::read(path)?).context("reading catalog")?;
    let mut history = History::default();
    for mut entry in catalog
        .frameworks
        .into_iter()
        .chain(catalog.excluded_or_editorial_candidates)
    {
        let mut names = BTreeSet::new();
        for key in [
            "first_framework_registry_release",
            "lineage_first_framework_registry_release",
            "precursor_release",
            "earliest_registry_publication",
            "component_first_publications",
            "evidence",
        ] {
            if let Some(value) = entry.get(key) {
                collect_crates(value, &mut names);
            }
        }
        entry.insert("crate_names".into(), serde_json::to_value(names)?);
        if let Some(Value::Object(mut identity)) = entry.remove("repository_identity")
            && let Some(urls) = identity.remove("historical_repository_urls")
        {
            entry.insert("historical_repository_urls".into(), urls);
        }
        match entry.get("inclusion").and_then(Value::as_str) {
            Some("borderline" | "exclude") => history
                .not_included
                .push(serde_json::from_value(Value::Object(entry))?),
            Some("include") => {
                if entry.contains_key("lineage_first_framework_registry_release")
                    && let Some(precursor) = entry.remove("precursor_release")
                {
                    let lineage = &entry["lineage_first_framework_registry_release"];
                    ensure!(
                        precursor["at"] == lineage["at"]
                            && precursor["source"] == lineage["source"],
                        "conflicting lineage releases"
                    );
                }
                history
                    .frameworks
                    .push(serde_json::from_value(Value::Object(entry))?);
            }
            _ => bail!("invalid inclusion"),
        }
    }
    history.frameworks.sort_by(|a, b| a.id.cmp(&b.id));
    history.not_included.sort_by(|a, b| a.id.cmp(&b.id));
    history.validate()
}

pub fn crate_from_source(source: &str) -> Option<String> {
    let path = source.strip_prefix("https://crates.io/")?;
    let path = path.strip_prefix("api/v1/").unwrap_or(path);
    let name = path
        .strip_prefix("crates/")?
        .split(['/', '?', '#'])
        .next()?;
    (!name.is_empty()).then(|| name.to_lowercase().replace('_', "-"))
}
fn collect_crates(value: &serde_json::Value, names: &mut BTreeSet<String>) {
    use serde_json::Value;
    match value {
        Value::String(s) => names.extend(crate_from_source(s)),
        Value::Array(items) => items.iter().for_each(|v| collect_crates(v, names)),
        Value::Object(items) => items.values().for_each(|v| collect_crates(v, names)),
        _ => {}
    }
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReleaseKind {
    FrameworkRegistryRelease,
    LineageRelease,
}
#[derive(Debug, Deserialize)]
pub struct ResetLedger {
    pub checked_through: Date,
    #[serde(default, rename = "reset")]
    pub resets: Vec<Reset>,
}
#[derive(Debug, Deserialize)]
pub struct Reset {
    pub framework: String,
    pub event: ReleaseKind,
    pub approval: Approval,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Approval {
    Pending,
    Approved,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuildMode {
    Production,
    Preview,
}
#[derive(Debug)]
pub struct CounterEvent<'a> {
    pub framework: &'a Framework,
    pub event: &'a Release,
    pub pending: bool,
}

impl Release {
    pub fn days_since(&self, today: Date) -> Result<i64> {
        let date = utc_date(self.at);
        ensure!(
            date <= today,
            "counter event {date} is after today ({today})"
        );
        Ok(i64::from(date.until((Unit::Day, today))?.get_days()))
    }
}
pub fn utc_date(at: Timestamp) -> Date {
    at.to_zoned(TimeZone::UTC).date()
}
impl ResetLedger {
    pub fn load(path: &Path) -> Result<Self> {
        Ok(toml::from_str(&fs::read_to_string(path)?)?)
    }
    pub fn counter_event<'a>(
        &self,
        history: &'a History,
        mode: BuildMode,
    ) -> Result<CounterEvent<'a>> {
        let mut latest: Option<CounterEvent<'a>> = None;
        for reset in &self.resets {
            let framework = history
                .framework(&reset.framework)
                .context("unknown framework")?;
            let event = framework
                .release(reset.event)
                .context("missing reset release")?;
            ensure!(
                framework.first_crates_release() == Some(event),
                "reset for {} does not name its first crates.io release",
                framework.id
            );
            let usable = mode == BuildMode::Preview || reset.approval == Approval::Approved;
            if usable && latest.as_ref().is_none_or(|c| event.at > c.event.at) {
                latest = Some(CounterEvent {
                    framework,
                    event,
                    pending: reset.approval == Approval::Pending,
                });
            }
        }
        latest.context(if mode == BuildMode::Production {
            "no approved reset; refusing a production build"
        } else {
            "the reset ledger is empty"
        })
    }
}
