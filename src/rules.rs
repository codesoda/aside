//! Repository ignore rules: validation, matching and ordered evaluation.
//!
//! Semantics (independent of Git's `.gitignore` parent-directory limits):
//!
//! - Rules are evaluated over each changed file path, in `order`.
//!   The **last** enabled matching rule wins.
//!   So an `include` after an `exclude` re-includes a file under an excluded folder.
//! - `file`: exact repository-relative path.
//! - `directory`: the folder and everything under it (`src/generated` or `src/generated/`).
//! - `glob`:
//!   - `*` matches any characters except `/`. `?` matches one character except `/`.
//!   - `**` as a whole segment matches zero or more segments.
//!   - `\` escapes the next character. `[` is not supported and must be escaped.
//!   - A pattern containing `/` (other than a trailing one) is anchored to the repository root.
//!     A leading `/` also anchors it. A pattern without `/` matches a file or folder name at any depth.
//!   - A pattern that matches a folder also matches everything under it.
//!     A trailing `/` makes the pattern match folders only.
//! - Matching is case-sensitive. Paths use `/` separators.

use anyhow::{Result, bail};

use crate::model::{ExclusionRule, FileScope, RuleAction, RuleKind};

/// Normalise and validate a rule expression. Returns the stored form.
pub fn normalize(kind: RuleKind, expression: &str) -> Result<String> {
    let expr = expression.trim();
    if expr.is_empty() {
        bail!("expression must not be empty");
    }
    if expr.contains('\0') || expr.contains('\n') {
        bail!("expression must not contain control characters");
    }
    match kind {
        RuleKind::File => {
            let path = normalize_path(expr)?;
            if path.ends_with('/') {
                bail!("a file rule can't end with `/`; use a directory rule");
            }
            Ok(path)
        }
        RuleKind::Directory => {
            let path = normalize_path(expr.trim_end_matches('/'))?;
            Ok(path)
        }
        RuleKind::Glob => {
            Glob::compile(expr)?;
            Ok(expr.to_string())
        }
    }
}

/// Normalise a repository path: strip `./` and leading `/`, reject `..` and empty segments.
pub fn normalize_path(raw: &str) -> Result<String> {
    let trimmed = raw.trim_start_matches("./").trim_start_matches('/');
    if trimmed.is_empty() {
        bail!("path must not be empty");
    }
    for seg in trimmed.split('/') {
        if seg.is_empty() {
            bail!("path must not contain empty segments (`//`)");
        }
        if seg == ".." || seg == "." {
            bail!("path must not contain `.` or `..` segments");
        }
    }
    Ok(trimmed.to_string())
}

pub fn matches(kind: RuleKind, expression: &str, path: &str) -> bool {
    match kind {
        RuleKind::File => path == expression,
        RuleKind::Directory => path
            .strip_prefix(expression)
            .is_some_and(|rest| rest.starts_with('/')),
        RuleKind::Glob => Glob::compile(expression).is_ok_and(|g| g.matches(path)),
    }
}

/// Evaluate ordered rules against a path. Last enabled match wins.
pub fn evaluate(rules: &[ExclusionRule], path: &str) -> FileScope {
    let mut ordered: Vec<&ExclusionRule> = rules.iter().filter(|r| r.enabled).collect();
    ordered.sort_by_key(|r| r.order);
    let matching: Vec<&ExclusionRule> = ordered
        .into_iter()
        .filter(|r| matches(r.kind, &r.expression, path))
        .collect();
    let effective = matching.last();
    FileScope {
        ignored: effective.is_some_and(|r| r.action == RuleAction::Exclude),
        effective_rule_id: effective.map(|r| r.id.clone()),
        matching_rule_ids: matching.iter().map(|r| r.id.clone()).collect(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Literal(char),
    Star,
    Question,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Segment {
    DoubleStar,
    Tokens(Vec<Token>),
}

#[derive(Debug, Clone)]
pub struct Glob {
    segments: Vec<Segment>,
    dir_only: bool,
}

impl Glob {
    pub fn compile(pattern: &str) -> Result<Self> {
        let mut p = pattern.trim();
        if p.is_empty() {
            bail!("pattern must not be empty");
        }
        let dir_only = p.ends_with('/') && !p.ends_with("\\/");
        if dir_only {
            p = p.trim_end_matches('/');
        }
        let anchored_by_slash = p.starts_with('/');
        let p = p.trim_start_matches('/');
        if p.is_empty() {
            bail!("pattern must name something");
        }
        let raw_segments = split_segments(p)?;
        let anchored = anchored_by_slash || raw_segments.len() > 1;
        let mut segments = Vec::new();
        if !anchored {
            segments.push(Segment::DoubleStar);
        }
        for seg in raw_segments {
            if seg.is_empty() {
                bail!("pattern must not contain `//`");
            }
            if seg == "**" {
                if segments.last() != Some(&Segment::DoubleStar) {
                    segments.push(Segment::DoubleStar);
                }
                continue;
            }
            segments.push(Segment::Tokens(tokenize(&seg)?));
        }
        Ok(Self { segments, dir_only })
    }

    /// True if the path, or any folder containing it, matches.
    pub fn matches(&self, path: &str) -> bool {
        let parts: Vec<&str> = path.split('/').collect();
        let last = if self.dir_only {
            parts.len() - 1
        } else {
            parts.len()
        };
        (1..=last).any(|n| match_segments(&self.segments, &parts[..n]))
    }
}

/// Split on unescaped `/`, keeping escapes for the tokenizer.
fn split_segments(p: &str) -> Result<Vec<String>> {
    let mut out = vec![String::new()];
    let mut chars = p.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                let next = chars
                    .next()
                    .ok_or_else(|| anyhow::anyhow!("pattern ends with a lone `\\`"))?;
                let seg = out.last_mut().expect("non-empty");
                seg.push('\\');
                seg.push(next);
            }
            '/' => out.push(String::new()),
            c => out.last_mut().expect("non-empty").push(c),
        }
    }
    Ok(out)
}

fn tokenize(seg: &str) -> Result<Vec<Token>> {
    let mut tokens = Vec::new();
    let mut chars = seg.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => tokens.push(Token::Literal(
                chars.next().expect("escape checked in split"),
            )),
            '*' => {
                // `**` inside a segment (e.g. `a**b`) behaves like `*`.
                if tokens.last() != Some(&Token::Star) {
                    tokens.push(Token::Star);
                }
            }
            '?' => tokens.push(Token::Question),
            '[' | ']' => bail!("character classes are not supported; escape `{c}` as `\\{c}`"),
            c => tokens.push(Token::Literal(c)),
        }
    }
    Ok(tokens)
}

fn match_segments(pattern: &[Segment], parts: &[&str]) -> bool {
    match pattern.split_first() {
        None => parts.is_empty(),
        Some((Segment::DoubleStar, rest)) => {
            (0..=parts.len()).any(|skip| match_segments(rest, &parts[skip..]))
        }
        Some((Segment::Tokens(tokens), rest)) => match parts.split_first() {
            Some((part, remaining)) => {
                let chars: Vec<char> = part.chars().collect();
                match_tokens(tokens, &chars) && match_segments(rest, remaining)
            }
            None => false,
        },
    }
}

fn match_tokens(tokens: &[Token], chars: &[char]) -> bool {
    match tokens.split_first() {
        None => chars.is_empty(),
        Some((Token::Star, rest)) => {
            (0..=chars.len()).any(|skip| match_tokens(rest, &chars[skip..]))
        }
        Some((Token::Question, rest)) => !chars.is_empty() && match_tokens(rest, &chars[1..]),
        Some((Token::Literal(c), rest)) => {
            chars.first() == Some(c) && match_tokens(rest, &chars[1..])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(id: &str, order: i64, action: RuleAction, kind: RuleKind, expr: &str) -> ExclusionRule {
        ExclusionRule {
            id: id.into(),
            action,
            kind,
            expression: normalize(kind, expr).unwrap(),
            enabled: true,
            order,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    fn glob(p: &str, path: &str) -> bool {
        Glob::compile(p).unwrap().matches(path)
    }

    #[test]
    fn prd_precedence_example() {
        let rules = vec![
            rule(
                "r1",
                1,
                RuleAction::Exclude,
                RuleKind::Directory,
                "src/generated/",
            ),
            rule("r2", 2, RuleAction::Exclude, RuleKind::Glob, "**/*.snap"),
            rule(
                "r3",
                3,
                RuleAction::Include,
                RuleKind::File,
                "src/generated/manual.rs",
            ),
        ];
        let types = evaluate(&rules, "src/generated/types.rs");
        assert!(types.ignored);
        assert_eq!(types.effective_rule_id.as_deref(), Some("r1"));
        let manual = evaluate(&rules, "src/generated/manual.rs");
        assert!(!manual.ignored);
        assert_eq!(manual.effective_rule_id.as_deref(), Some("r3"));
        assert_eq!(manual.matching_rule_ids, vec!["r1", "r3"]);
        assert!(evaluate(&rules, "tests/output.snap").ignored);
        let api = evaluate(&rules, "src/api.rs");
        assert!(!api.ignored && api.effective_rule_id.is_none());
    }

    #[test]
    fn order_field_decides_not_vector_position() {
        let rules = vec![
            rule(
                "inc",
                2,
                RuleAction::Include,
                RuleKind::File,
                "vendor/README.md",
            ),
            rule("exc", 1, RuleAction::Exclude, RuleKind::Directory, "vendor"),
        ];
        assert!(!evaluate(&rules, "vendor/README.md").ignored);
        assert!(evaluate(&rules, "vendor/lib/x.c").ignored);
    }

    #[test]
    fn disabled_rules_are_skipped() {
        let mut r = rule("r", 1, RuleAction::Exclude, RuleKind::File, "a.txt");
        r.enabled = false;
        assert!(!evaluate(&[r], "a.txt").ignored);
    }

    #[test]
    fn directory_rules_match_descendants_only() {
        assert!(matches(RuleKind::Directory, "vendor", "vendor/a/b.c"));
        assert!(!matches(RuleKind::Directory, "vendor", "vendor"));
        assert!(!matches(RuleKind::Directory, "vendor", "vendored/a.c"));
    }

    #[test]
    fn glob_semantics() {
        assert!(glob("**/*.snap", "tests/output.snap"));
        assert!(glob("**/*.snap", "a.snap"));
        assert!(glob("*.snap", "deep/nested/a.snap"));
        assert!(!glob("*.snap", "a.snapshot"));
        assert!(glob("generated/**", "generated/x/y.rs"));
        assert!(!glob("generated/**", "src/generated/x.rs"));
        assert!(glob("node_modules", "web/node_modules/pkg/index.js"));
        assert!(glob("/Cargo.lock", "Cargo.lock"));
        assert!(!glob("/Cargo.lock", "sub/Cargo.lock"));
        assert!(glob("src/*.rs", "src/main.rs"));
        assert!(!glob("src/*.rs", "src/a/main.rs"));
        assert!(glob("src/**/*.rs", "src/main.rs"));
        assert!(glob("src/**/*.rs", "src/a/b/main.rs"));
        assert!(glob("?.txt", "a.txt"));
        assert!(!glob("?.txt", "ab.txt"));
        assert!(glob("build/", "build/out.js"));
        assert!(!glob("build/", "build"));
        assert!(glob("a\\*b", "a*b"));
        assert!(!glob("a\\*b", "axb"));
        assert!(!glob("Readme.md", "README.md"));
    }

    #[test]
    fn rejects_invalid_expressions() {
        assert!(normalize(RuleKind::Glob, "src/[ab].rs").is_err());
        assert!(normalize(RuleKind::Glob, "trailing\\").is_err());
        assert!(normalize(RuleKind::Glob, "a//b").is_err());
        assert!(normalize(RuleKind::File, "../etc/passwd").is_err());
        assert!(normalize(RuleKind::File, "").is_err());
        assert!(normalize(RuleKind::Directory, "/").is_err());
        assert_eq!(
            normalize(RuleKind::Directory, "./src/gen/").unwrap(),
            "src/gen"
        );
        assert!(normalize(RuleKind::Glob, "src/\\[x\\].rs").is_ok());
    }
}
