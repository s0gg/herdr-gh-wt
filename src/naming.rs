//! Normalization of free-form text (usually LLM output) into branch names and
//! workspace labels.

/// Turn raw model output into a git branch slug: lowercase ASCII letters,
/// digits and single hyphens, at most `MAX_SLUG_LEN` characters.
/// Returns `None` when nothing usable is left.
pub fn sanitize_branch_slug(raw: &str) -> Option<String> {
    let line = first_meaningful_line(raw)?;
    let mut slug = String::with_capacity(line.len());
    for c in line.chars() {
        let c = c.to_ascii_lowercase();
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            slug.push(c);
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let mut slug = slug.trim_matches('-');
    if slug.len() > MAX_SLUG_LEN {
        let cut = &slug[..MAX_SLUG_LEN];
        let cut_on_boundary = slug.as_bytes()[MAX_SLUG_LEN] == b'-';
        slug = match cut.rfind('-') {
            _ if cut_on_boundary => cut,
            Some(i) if i > 0 => &cut[..i],
            _ => cut,
        };
        slug = slug.trim_matches('-');
    }
    (!slug.is_empty()).then(|| slug.to_string())
}

/// Turn raw model output into a single-line workspace label of at most
/// `max_chars` characters. Returns `None` when nothing usable is left.
pub fn sanitize_label(raw: &str, max_chars: usize) -> Option<String> {
    let line = first_meaningful_line(raw)?;
    let label = line.split_whitespace().collect::<Vec<_>>().join(" ");
    if label.is_empty() {
        return None;
    }
    if label.chars().count() <= max_chars {
        return Some(label);
    }
    let mut truncated: String = label.chars().take(max_chars.saturating_sub(1)).collect();
    truncated = truncated.trim_end().to_string();
    truncated.push('…');
    Some(truncated)
}

/// Return `slug`, or `slug-2`, `slug-3`, ... — the first candidate for which
/// `exists` is false.
pub fn unique_branch(slug: &str, exists: impl Fn(&str) -> bool) -> String {
    if !exists(slug) {
        return slug.to_string();
    }
    let mut n = 2;
    loop {
        let candidate = format!("{slug}-{n}");
        if !exists(&candidate) {
            return candidate;
        }
        n += 1;
    }
}

/// The first line that still has content once surrounding whitespace and
/// wrapping quotes/backticks are removed.
fn first_meaningful_line(raw: &str) -> Option<&str> {
    raw.lines()
        .map(strip_wrapping)
        .find(|line| !line.is_empty())
}

fn strip_wrapping(line: &str) -> &str {
    const PAIRS: [(char, char); 5] = [
        ('"', '"'),
        ('\'', '\''),
        ('`', '`'),
        ('「', '」'),
        ('“', '”'),
    ];
    let mut line = line.trim();
    loop {
        let before = line;
        for (open, close) in PAIRS {
            if let Some(inner) = line.strip_prefix(open).and_then(|l| l.strip_suffix(close)) {
                line = inner.trim();
            }
        }
        if line == before {
            return line;
        }
    }
}

pub const MAX_SLUG_LEN: usize = 50;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_keeps_clean_kebab_case() {
        assert_eq!(
            sanitize_branch_slug("fix-login-timeout").as_deref(),
            Some("fix-login-timeout")
        );
    }

    #[test]
    fn slug_uses_first_non_empty_line_and_strips_wrapping() {
        assert_eq!(
            sanitize_branch_slug("\n  `Fix-Login-Timeout`  \nbecause ...").as_deref(),
            Some("fix-login-timeout")
        );
        assert_eq!(
            sanitize_branch_slug("\"add-csv-export\"").as_deref(),
            Some("add-csv-export")
        );
    }

    #[test]
    fn slug_replaces_invalid_characters_and_collapses_hyphens() {
        assert_eq!(
            sanitize_branch_slug("fix/login  timeout__on  SSO!!").as_deref(),
            Some("fix-login-timeout-on-sso")
        );
        assert_eq!(sanitize_branch_slug("--a--b--").as_deref(), Some("a-b"));
    }

    #[test]
    fn slug_drops_non_ascii() {
        assert_eq!(sanitize_branch_slug("ログイン-fix").as_deref(), Some("fix"));
        assert_eq!(sanitize_branch_slug("ログイン"), None);
    }

    #[test]
    fn slug_is_none_for_blank_output() {
        assert_eq!(sanitize_branch_slug(""), None);
        assert_eq!(sanitize_branch_slug("  \n\t "), None);
        assert_eq!(sanitize_branch_slug("``"), None);
    }

    #[test]
    fn slug_is_truncated_on_a_word_boundary() {
        let raw = "implement-the-very-long-feature-name-that-goes-on-and-on-forever";
        let slug = sanitize_branch_slug(raw).unwrap();
        assert!(slug.len() <= MAX_SLUG_LEN, "{slug}");
        assert!(!slug.ends_with('-'), "{slug}");
        assert!(raw.starts_with(&slug), "{slug}");
        assert_eq!(slug, "implement-the-very-long-feature-name-that-goes-on");
    }

    #[test]
    fn slug_without_boundary_is_hard_truncated() {
        let raw = "a".repeat(80);
        assert_eq!(sanitize_branch_slug(&raw).unwrap().len(), MAX_SLUG_LEN);
    }

    #[test]
    fn label_takes_first_line_and_strips_quotes() {
        assert_eq!(
            sanitize_label("\n\"PR #12 Fix login\"\nexplanation", 40).as_deref(),
            Some("PR #12 Fix login")
        );
        assert_eq!(
            sanitize_label("「#3 画面の修正」", 40).as_deref(),
            Some("#3 画面の修正")
        );
        assert_eq!(sanitize_label("`#3 fix`", 40).as_deref(), Some("#3 fix"));
    }

    #[test]
    fn label_collapses_whitespace() {
        assert_eq!(
            sanitize_label("  PR  #1 \t a ", 40).as_deref(),
            Some("PR #1 a")
        );
    }

    #[test]
    fn label_is_truncated_by_characters_with_ellipsis() {
        assert_eq!(
            sanitize_label("あいうえおかきくけこ", 5).as_deref(),
            Some("あいうえ…")
        );
        assert_eq!(sanitize_label("abcde", 5).as_deref(), Some("abcde"));
    }

    #[test]
    fn label_is_none_for_blank_output() {
        assert_eq!(sanitize_label(" \n ", 40), None);
        assert_eq!(sanitize_label("\"\"", 40), None);
    }

    #[test]
    fn unique_branch_appends_counter_until_free() {
        assert_eq!(unique_branch("fix", |_| false), "fix");
        assert_eq!(unique_branch("fix", |b| b == "fix"), "fix-2");
        assert_eq!(
            unique_branch("fix", |b| b == "fix" || b == "fix-2"),
            "fix-3"
        );
    }
}
