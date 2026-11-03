//! The GitHub releases Atom feed (`<repository>/releases.atom`). Newjen reads this instead of
//! the GitHub API, which rate-limits and fails more often.

use semver::Version;

/// One published release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: Version,
    /// The git tag, always `v<version>`.
    pub tag: String,
    /// The entry's `<updated>` text, an RFC 3339 time, when the feed gave one.
    pub published: Option<String>,
}

impl Release {
    pub(super) fn zip_name(&self) -> String {
        format!("Newjen-{}-arm64.zip", self.version)
    }
}

/// Every entry whose tag is `v<semver>`. Entries with any other tag are dropped.
pub fn parse_feed(xml: &str) -> Vec<Release> {
    let mut releases = Vec::new();
    let mut rest = xml;
    while let Some(start) = find_tag_open(rest, "entry") {
        let body = &rest[start..];
        let end = body.find("</entry>").unwrap_or(body.len());
        if let Some(release) = parse_entry(&body[..end]) {
            releases.push(release);
        }
        rest = &body[end..];
    }
    releases
}

/// The highest version above `current`. A prerelease is only offered to someone who already
/// runs one, so a stable install never moves onto an alpha, beta or release candidate.
pub fn newest_after(releases: Vec<Release>, current: &Version) -> Option<Release> {
    let on_prerelease = !current.pre.is_empty();
    releases
        .into_iter()
        .filter(|release| {
            release.version > *current && (on_prerelease || release.version.pre.is_empty())
        })
        .max_by(|a, b| a.version.cmp(&b.version))
}

fn parse_entry(entry: &str) -> Option<Release> {
    let tag = links(entry).find_map(|href| {
        href.split_once("/releases/tag/")
            .map(|(_, tag)| tag.to_owned())
    })?;
    let version = tag.strip_prefix('v')?.parse::<Version>().ok()?;
    if tag != format!("v{version}") {
        return None;
    }
    Some(Release {
        version,
        tag,
        published: element_text(entry, "updated"),
    })
}

fn links(entry: &str) -> impl Iterator<Item = String> + '_ {
    let mut rest = entry;
    std::iter::from_fn(move || {
        let start = find_tag_open(rest, "link")?;
        let tag = &rest[start..];
        let end = tag.find('>').unwrap_or(tag.len());
        let attributes = &tag[..end];
        rest = &tag[end..];
        Some(attribute(attributes, "href").unwrap_or_default())
    })
    .filter(|href| !href.is_empty())
}

/// The index just past `<name` where the tag really is `name` (not `named`).
fn find_tag_open(text: &str, name: &str) -> Option<usize> {
    let open = format!("<{name}");
    let mut from = 0;
    while let Some(found) = text[from..].find(&open) {
        let after = from + found + open.len();
        match text[after..].chars().next() {
            Some(c) if c == '>' || c == '/' || c.is_whitespace() => return Some(after),
            _ => from = after,
        }
    }
    None
}

fn attribute(attributes: &str, name: &str) -> Option<String> {
    let needle = format!("{name}=");
    let mut from = 0;
    while let Some(found) = attributes[from..].find(&needle) {
        let at = from + found;
        let preceded_by_space = at == 0 || attributes[..at].ends_with(char::is_whitespace);
        let value_start = at + needle.len();
        let quote = attributes[value_start..].chars().next();
        if preceded_by_space && let Some(quote @ ('"' | '\'')) = quote {
            let value = &attributes[value_start + 1..];
            let end = value.find(quote)?;
            return Some(decode_entities(&value[..end]));
        }
        from = value_start;
    }
    None
}

fn element_text(entry: &str, name: &str) -> Option<String> {
    let start = find_tag_open(entry, name)?;
    let body = &entry[start..];
    let body = &body[body.find('>')? + 1..];
    let text = &body[..body.find(&format!("</{name}>"))?];
    Some(decode_entities(text.trim())).filter(|text| !text.is_empty())
}

fn decode_entities(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}
