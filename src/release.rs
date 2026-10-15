//! Release numbering and changelog generation from Conventional Commits.

use std::fmt;

/// A release version, written `vMAJOR.MINOR.PATCH` in tags and the changelog.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

impl Version {
    pub fn new(major: u64, minor: u64, patch: u64) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }

    /// Parses a release tag such as `v1.2.3`; pre-release and other tags are not releases.
    pub fn parse(tag: &str) -> Option<Self> {
        let mut parts = tag.strip_prefix('v')?.split('.');
        let mut next = || -> Option<u64> {
            let part = parts.next()?;
            (!part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
                .then(|| part.parse().ok())
                .flatten()
        };
        let version = Self::new(next()?, next()?, next()?);
        parts.next().is_none().then_some(version)
    }
}

impl fmt::Display for Version {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "v{}.{}.{}", self.major, self.minor, self.patch)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Commit {
    pub hash: String,
    pub subject: String,
    pub body: String,
}

/// A commit subject read as a Conventional Commit; anything else is `kind: None`.
struct Conventional<'a> {
    kind: Option<&'a str>,
    scope: Option<&'a str>,
    breaking: bool,
    description: &'a str,
}

fn conventional(commit: &Commit) -> Conventional<'_> {
    let footer_breaking = commit
        .body
        .lines()
        .any(|line| line.starts_with("BREAKING CHANGE:") || line.starts_with("BREAKING-CHANGE:"));
    let parsed = commit
        .subject
        .split_once(": ")
        .and_then(|(header, description)| {
            let (header, bang) = match header.strip_suffix('!') {
                Some(header) => (header, true),
                None => (header, false),
            };
            let (kind, scope) = match header.split_once('(') {
                Some((kind, rest)) => (kind, Some(rest.strip_suffix(')')?)),
                None => (header, None),
            };
            (!kind.is_empty() && kind.bytes().all(|byte| byte.is_ascii_lowercase())).then_some(
                Conventional {
                    kind: Some(kind),
                    scope: scope.filter(|scope| !scope.is_empty()),
                    breaking: bang || footer_breaking,
                    description,
                },
            )
        });
    parsed.unwrap_or(Conventional {
        kind: None,
        scope: None,
        breaking: footer_breaking,
        description: &commit.subject,
    })
}

/// The next release after `current`: a breaking change bumps the major version, a feature the
/// minor version, and anything else the patch version.
pub fn next_version(current: Option<Version>, commits: &[Commit]) -> Version {
    let current = current.unwrap_or_default();
    let parsed = commits.iter().map(conventional).collect::<Vec<_>>();
    if parsed.iter().any(|commit| commit.breaking) {
        Version::new(current.major + 1, 0, 0)
    } else if parsed.iter().any(|commit| commit.kind == Some("feat")) {
        Version::new(current.major, current.minor + 1, 0)
    } else {
        Version::new(current.major, current.minor, current.patch + 1)
    }
}

/// One changelog section for `version`, grouping commits by kind. Merge commits are left out.
pub fn changelog_section(version: Version, date: &str, commits: &[Commit]) -> String {
    let mut groups: [(&str, Vec<String>); 4] = [
        ("Breaking Changes", Vec::new()),
        ("Features", Vec::new()),
        ("Bug Fixes", Vec::new()),
        ("Other Changes", Vec::new()),
    ];
    for commit in commits {
        if commit.subject.starts_with("Merge ") {
            continue;
        }
        let parsed = conventional(commit);
        let group = if parsed.breaking {
            0
        } else {
            match parsed.kind {
                Some("feat") => 1,
                Some("fix") => 2,
                _ => 3,
            }
        };
        let scope = parsed
            .scope
            .map(|scope| format!("**{scope}:** "))
            .unwrap_or_default();
        let short = commit.hash.chars().take(7).collect::<String>();
        groups[group]
            .1
            .push(format!("- {scope}{} ({short})", parsed.description));
    }
    let mut section = format!("## {version} - {date}\n");
    for (title, entries) in groups.iter().filter(|(_, entries)| !entries.is_empty()) {
        section.push_str(&format!("\n### {title}\n\n{}\n", entries.join("\n")));
    }
    section
}

/// Inserts a new section above the newest release, keeping the title and any introduction.
pub fn insert_section(existing: Option<&str>, section: &str) -> String {
    let Some(existing) = existing.filter(|text| !text.trim().is_empty()) else {
        return format!("# Changelog\n\n{section}");
    };
    match existing
        .match_indices("## ")
        .find(|(index, _)| *index == 0 || existing[..*index].ends_with('\n'))
    {
        Some((index, _)) => format!("{}{section}\n{}", &existing[..index], &existing[index..]),
        None => format!("{}\n\n{section}", existing.trim_end()),
    }
}

/// The UTC calendar date of a Unix timestamp as `YYYY-MM-DD` (Howard Hinnant's civil-from-days).
pub fn utc_date(unix_secs: u64) -> String {
    let days = (unix_secs / 86_400) as i64 + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

#[cfg(test)]
mod tests {
    use super::{Commit, Version, changelog_section, insert_section, next_version, utc_date};

    #[test]
    fn dates_are_utc_calendar_days() {
        assert_eq!(utc_date(0), "1970-01-01");
        assert_eq!(utc_date(951_782_400), "2000-02-29");
        assert_eq!(utc_date(1_791_936_000 + 86_399), "2026-10-14");
        assert_eq!(utc_date(4_107_542_400), "2100-03-01");
    }

    fn commit(hash: &str, subject: &str, body: &str) -> Commit {
        Commit {
            hash: hash.to_owned(),
            subject: subject.to_owned(),
            body: body.to_owned(),
        }
    }

    #[test]
    fn versions_parse_and_print_with_a_v_prefix() {
        assert_eq!(Version::parse("v1.2.3"), Some(Version::new(1, 2, 3)));
        assert_eq!(Version::parse("1.2.3"), None);
        assert_eq!(Version::parse("v1.2"), None);
        assert_eq!(Version::parse("v1.2.3-rc.1"), None);
        assert_eq!(Version::new(0, 4, 0).to_string(), "v0.4.0");
    }

    #[test]
    fn the_largest_change_decides_the_bump() {
        let base = Some(Version::new(1, 4, 2));
        let fix = commit("a", "fix(cli): handle empty input", "");
        let feat = commit("b", "feat: add export", "");
        let breaking = commit("c", "feat(api)!: drop v1 routes", "");
        let breaking_footer = commit("d", "refactor: rename", "BREAKING CHANGE: renamed X");
        let chore = commit("e", "chore: tidy", "");

        assert_eq!(
            next_version(base, std::slice::from_ref(&chore)),
            Version::new(1, 4, 3)
        );
        assert_eq!(
            next_version(base, std::slice::from_ref(&fix)),
            Version::new(1, 4, 3)
        );
        assert_eq!(
            next_version(base, &[fix.clone(), feat.clone()]),
            Version::new(1, 5, 0)
        );
        assert_eq!(
            next_version(base, &[feat.clone(), breaking]),
            Version::new(2, 0, 0)
        );
        assert_eq!(
            next_version(base, &[breaking_footer]),
            Version::new(2, 0, 0)
        );
        // The first release starts from 0.0.0.
        assert_eq!(next_version(None, &[feat]), Version::new(0, 1, 0));
        assert_eq!(next_version(None, &[chore]), Version::new(0, 0, 1));
    }

    #[test]
    fn changelog_groups_commits_by_type_with_scopes_and_short_hashes() {
        let section = changelog_section(
            Version::new(0, 2, 0),
            "2026-10-15",
            &[
                commit("1111111aaaa", "feat(cli): add export", ""),
                commit("2222222bbbb", "fix: handle empty input", ""),
                commit("3333333cccc", "feat!: drop legacy format", ""),
                commit("4444444dddd", "docs: document export", ""),
                commit("5555555eeee", "Merge branch 'x'", ""),
                commit("6666666ffff", "update readme", ""),
            ],
        );

        assert_eq!(
            section,
            "## v0.2.0 - 2026-10-15\n\
             \n\
             ### Breaking Changes\n\
             \n\
             - drop legacy format (3333333)\n\
             \n\
             ### Features\n\
             \n\
             - **cli:** add export (1111111)\n\
             \n\
             ### Bug Fixes\n\
             \n\
             - handle empty input (2222222)\n\
             \n\
             ### Other Changes\n\
             \n\
             - document export (4444444)\n\
             - update readme (6666666)\n"
        );
    }

    #[test]
    fn sections_are_inserted_newest_first_under_the_title() {
        let section = "## v0.2.0 - 2026-10-15\n\n### Features\n\n- add export (1111111)\n";

        assert_eq!(
            insert_section(None, section),
            format!("# Changelog\n\n{section}")
        );
        let existing = "# Changelog\n\nIntro text.\n\n## v0.1.0 - 2026-10-01\n\n- first\n";
        assert_eq!(
            insert_section(Some(existing), section),
            format!("# Changelog\n\nIntro text.\n\n{section}\n## v0.1.0 - 2026-10-01\n\n- first\n")
        );
        let untitled = "## v0.1.0 - 2026-10-01\n\n- first\n";
        assert_eq!(
            insert_section(Some(untitled), section),
            format!("{section}\n{untitled}")
        );
    }
}
