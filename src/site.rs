use crate::history::{BuildMode, CounterEvent, Framework, History, ResetLedger, utc_date};
use anyhow::{Context, Result as SiteResult, ensure};
use flate2::{Compression, write::GzEncoder};
use jiff::{Unit, civil::Date};
use proa_core::{DataLoader, WebContext, WebRenderSync, WriteError};
use proa_macros::{html_sync, xml_sync};
use std::{borrow::Cow, fs, io::Write, path::Path};

const INDEX_BROTLI_LIMIT: usize = 1024;
const INDEX_GZIP_LIMIT: usize = 1280;

pub fn display_url(url: &str) -> Cow<'_, str> {
    let (path, suffix) = url.find(['?', '#']).map_or((url, ""), |i| url.split_at(i));
    for (prefix, target) in [
        (
            "https://crates.io/api/v1/crates/",
            "https://crates.io/crates/",
        ),
        ("https://api.github.com/repos/", "https://github.com/"),
    ] {
        if let Some(tail) = path.strip_prefix(prefix) {
            let mut parts = tail.split('/');
            let (first, second, extra) = (parts.next(), parts.next(), parts.next());
            if first.is_some_and(|s| !s.is_empty())
                && extra.is_none()
                && second != Some("")
                && (target.contains("crates.io") || second.is_some())
            {
                let tail = tail
                    .strip_suffix("/versions")
                    .filter(|_| target.contains("crates.io"))
                    .unwrap_or(tail);
                return format!("{target}{tail}{suffix}").into();
            }
        }
    }
    Cow::Borrowed(url)
}
pub struct Metadata<'a> {
    pub title: &'a str,
    pub description: &'a str,
    pub url: &'a str,
    pub site_name: &'a str,
    pub image_url: &'a str,
}

impl<L: DataLoader> WebRenderSync<L> for Metadata<'_> {
    fn render(self, cx: &mut WebContext<L>) -> std::result::Result<(), WriteError> {
        let url = display_url(self.url);
        let image_url = display_url(self.image_url);
        xml_sync! {
            <meta charset="utf-8" />
            <title>{self.title}</title>
            <meta name="viewport" content="width=device-width,initial-scale=1" />
            <meta name="description" content={self.description} />
            <meta name="theme-color" content="#171717" />
            <link rel="canonical" href={url.as_ref()} />
            <link rel="icon" href="data:," />
            <meta property="og:title" content={self.title} />
            <meta property="og:description" content={self.description} />
            <meta property="og:url" content={url.as_ref()} />
            <meta property="og:type" content="website" />
            <meta property="og:site_name" content={self.site_name} />
            <meta property="og:image" content={image_url.as_ref()} />
            <meta property="og:image:width" content="1200" />
            <meta property="og:image:height" content="630" />
            <meta property="og:image:alt" content={self.title} />
            <meta name="twitter:card" content="summary_large_image" />
        }
        .render(cx)
    }
}

fn render(component: impl WebRenderSync) -> SiteResult<Vec<u8>> {
    let mut cx = WebContext::new();
    component.render(&mut cx)?;
    Ok(cx.into_output().into_vec())
}
fn page(
    title: &str,
    description: &str,
    url: &str,
    style: impl WebRenderSync,
    body: impl WebRenderSync,
) -> SiteResult<Vec<u8>> {
    render(html_sync! {
        <!DOCTYPE html><html lang="en"><head>
        {Metadata { title, description, url, site_name: "days since last rust framework", image_url: "https://dayssincelastrustframework.com/og.png" }}
        {style}</head><body>{body}</body></html>
    })
}
struct HistoryItem<'a> {
    framework: &'a Framework,
    age: String,
}
impl<L: DataLoader> WebRenderSync<L> for HistoryItem<'_> {
    fn render(self, cx: &mut WebContext<L>) -> Result<(), WriteError> {
        let f = self.framework;
        let description = f
            .description
            .trim_matches(|c: char| c.is_whitespace() || c.is_ascii_punctuation());
        let show = !description.is_empty() && !description.eq_ignore_ascii_case(&f.name);
        let release = f.first_crates_release();
        let git = f.git_release.as_ref();
        let date = f.first_release_date().map(|date| date.to_string());
        let source = release
            .map(|r| r.source.as_str())
            .or_else(|| git.map(|r| r.source.as_str()));
        let yanked = release.is_some_and(|r| r.yanked);
        let prerelease = release.is_some_and(|r| r.prerelease);
        html_sync! {
            <li><a href={display_url(&f.repository).as_ref()}>{f.name.as_str()}</a>
                <small>{if let (Some(source), Some(date)) = (source, date) { html_sync! {
                <a href={display_url(source).as_ref()}>{date}</a>
                {if git.is_some() { html_sync! { " · git only" } }}
                {if yanked { html_sync! { " · yanked" } }}{if prerelease { html_sync! { " · pre-release" } }}
                " · "{self.age}
            }} else { html_sync! { "no release yet" } }}</small>
                <small class="description">{f.classification.label()}{if show { html_sync! { " · "{f.description.as_str()} } }}</small></li>
        }.render(cx)
    }
}
fn quantity(count: i64, unit: &str) -> String {
    let digits = count.to_string();
    let mut number = String::new();
    for (i, digit) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            number.push(',');
        }
        number.push(digit);
    }
    format!("{number} {unit}{}", if count == 1 { "" } else { "s" })
}
fn age(f: &Framework, today: Date) -> SiteResult<String> {
    let Some(date) = f.first_release_date() else {
        return Ok(String::new());
    };
    ensure!(date <= today, "release {date} is after today ({today})");
    let span = date.until((Unit::Year, today))?;
    let (years, months) = (i64::from(span.get_years()), i64::from(span.get_months()));
    let calendar = match (years, months) {
        (0, 0) => "less than a month".into(),
        (0, m) => quantity(m, "month"),
        (y, 0) => quantity(y, "year"),
        (y, m) => format!("{} {}", quantity(y, "year"), quantity(m, "month")),
    };
    Ok(format!(
        "{} · {calendar}",
        quantity(i64::from(date.until((Unit::Day, today))?.get_days()), "day")
    ))
}
fn social_image(counter: &CounterEvent<'_>, days: i64) -> SiteResult<Vec<u8>> {
    use resvg::{tiny_skia, usvg};
    let mut options = usvg::Options::default();
    options.fontdb_mut().load_system_fonts();
    let faces = || options.fontdb.faces().filter(|face| face.monospaced);
    let face = faces()
        .find(|face| {
            face.families
                .iter()
                .any(|(name, _)| name == "DejaVu Sans Mono")
        })
        .or_else(|| faces().next())
        .context("no system monospace font found")?;
    let font = &face.families.first().context("font has no family")?.0;
    let date = utc_date(counter.event.at).to_string();
    let svg = render(html_sync! {
        <svg xmlns="http://www.w3.org/2000/svg" width="1200" height="630">
            <rect width="1200" height="630" fill="white" /><g text-anchor="middle" font-family={font.as_str()} fill="#171717">
        <text x="600" y="310" font-size="180" font-weight="bold">{days}</text>
        <text x="600" y="380" font-size="28">"days since the last rust framework"</text>
        <text x="600" y="425" font-size="18">{counter.framework.name.as_str()}" · "{date}" utc"</text>
        {if counter.pending { html_sync! { <text x="600" y="155" font-size="14">"preview"</text> } }}
        </g></svg>
    })?;
    let tree = usvg::Tree::from_data(&svg, &options).context("parsing social image")?;
    let mut image = tiny_skia::Pixmap::new(1200, 630).context("could not allocate social image")?;
    resvg::render(&tree, tiny_skia::Transform::identity(), &mut image.as_mut());
    image.encode_png().context("encoding social image")
}
#[derive(Debug)]
pub struct FileSize {
    pub path: &'static str,
    pub raw: usize,
    pub gzip: usize,
    pub brotli: usize,
}
pub fn size(path: &'static str, bytes: &[u8]) -> SiteResult<FileSize> {
    let mut gzip = GzEncoder::new(Vec::new(), Compression::best());
    gzip.write_all(bytes)?;
    let mut brotli = brotli::CompressorWriter::new(Vec::new(), 4096, 11, 22);
    brotli.write_all(bytes)?;
    Ok(FileSize {
        path,
        raw: bytes.len(),
        gzip: gzip.finish()?.len(),
        brotli: brotli.into_inner().len(),
    })
}
pub fn build(
    history: &History,
    ledger: &ResetLedger,
    mode: BuildMode,
    today: Date,
    out: &Path,
) -> SiteResult<Vec<FileSize>> {
    let counter = ledger.counter_event(history, mode)?;
    let days = counter.event.days_since(today)?;
    let date = utc_date(counter.event.at).to_string();
    let title = format!("{days} days since the last rust framework");
    let description = format!(
        "how many days since the last new rust web framework was released. latest: {}, {date}.",
        counter.framework.name
    );
    let index = page(
        &title,
        &description,
        "https://dayssincelastrustframework.com/",
        html_sync! { <style>r#"html{color-scheme:light dark;font:16px ui-monospace,SFMono-Regular,Menlo,Consolas,"Liberation Mono",monospace}body{box-sizing:border-box;min-height:100vh;margin:0;padding:1rem;display:grid;place-content:center;text-align:center}h1{font-size:clamp(6rem,25vw,12rem);margin:0}a{color:inherit}small,footer,.credit{font-size:.75rem}footer{margin-top:2rem}.credit{position:fixed;bottom:1rem;left:50%;transform:translateX(-50%);margin:0;white-space:nowrap}"#</style> },
        html_sync! {
            <main>{if counter.pending { html_sync! { <small>"preview"</small> } }}
                <h1>{days}</h1><p>"days since the last rust framework"</p>
                <p><small><a href={display_url(&counter.framework.repository).as_ref()}>{counter.framework.name.as_str()}</a>" · "
            <time datetime={date.as_str()}>{date.as_str()}</time>" utc · "<a href={display_url(&counter.event.source).as_ref()}>"source"</a></small></p>
                <a href="/history/">"history"</a></main>
            <footer><p>"if anything is out of the ordinary, email "<a href="mailto:contact@notes.supply">"contact@notes.supply"</a>" or "<a href="https://github.com/notes-supply/days-since-rust-framework/issues/new">"open an issue"</a></p></footer><p class="credit"><a href="https://github.com/notes-supply/days-since-rust-framework">"github"</a>" · by "<a href="https://portfolio.notes.supply/">"notes"</a></p>
        },
    )?;
    let index_size = size("index.html", &index)?;
    ensure!(
        index_size.brotli <= INDEX_BROTLI_LIMIT && index_size.gzip <= INDEX_GZIP_LIMIT,
        "index.html exceeds page budget: brotli {} B (limit {INDEX_BROTLI_LIMIT} B), gzip {} B (limit {INDEX_GZIP_LIMIT} B)",
        index_size.brotli,
        index_size.gzip
    );
    let mut items = history
        .frameworks
        .iter()
        .map(|f| {
            Ok(HistoryItem {
                framework: f,
                age: age(f, today)?,
            })
        })
        .collect::<SiteResult<Vec<_>>>()?;
    items.sort_by_key(|item| {
        (
            std::cmp::Reverse(item.framework.first_release_date()),
            std::cmp::Reverse(item.framework.first_crates_release().map(|r| r.at)),
            &item.framework.id,
        )
    });
    let history = page(
        "rust web framework history",
        "every rust web framework we know of, with its first release date, days since, and what kind it is.",
        "https://dayssincelastrustframework.com/history/",
        html_sync! { <style>r#"html{color-scheme:light dark;font:16px ui-monospace,SFMono-Regular,Menlo,Consolas,"Liberation Mono",monospace}body{max-width:56rem;margin:3rem auto;padding:0 1rem;text-align:center}a{color:inherit}h1{font-size:1.5rem;margin:1.5rem 0 2.5rem}ul{list-style:none;padding:0}li{margin:2.5rem 0}li>a{font-size:1.1rem;font-weight:600}small{display:block;font-size:.75rem;line-height:1.6;opacity:.65}li small{margin-top:.4rem}.description{max-width:40ch;margin:.5rem auto 0;overflow-wrap:anywhere}"#</style> },
        html_sync! {
            <small><a href="/">"counter"</a></small>{if counter.pending { html_sync! { <small>" preview"</small> } }}
            <h1>"history"</h1><ul>{for item in items { html_sync! { {item} } }}</ul><small><a href="https://github.com/notes-supply/days-since-rust-framework">"github"</a>" · by "<a href="https://portfolio.notes.supply/">"notes"</a></small>
        },
    )?;
    let image = social_image(&counter, days)?;
    let lastmod = today.to_string();
    let sitemap = render(xml_sync! {
        <urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">
            <url><loc>"https://dayssincelastrustframework.com/"</loc><lastmod>{lastmod.as_str()}</lastmod></url>
            <url><loc>"https://dayssincelastrustframework.com/history/"</loc><lastmod>{lastmod.as_str()}</lastmod></url>
        </urlset>
    })?;
    let robots =
        b"User-agent: *\nAllow: /\nSitemap: https://dayssincelastrustframework.com/sitemap.xml\n"
            .to_vec();
    let mut sizes = vec![index_size];
    fs::create_dir_all(out.join("history"))?;
    for (path, content) in [
        ("index.html", index),
        ("history/index.html", history),
        ("og.png", image),
        ("robots.txt", robots),
        ("sitemap.xml", sitemap),
    ] {
        if path != "index.html" {
            sizes.push(size(path, &content)?);
        }
        fs::write(out.join(path), content)?;
    }
    Ok(sizes)
}
