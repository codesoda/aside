//! Path- and line-level classifiers used by the diff-facts rules.
//!
//! Everything here is a pure function of ASCII-lowercased paths and raw line
//! text, so results never depend on locale, platform or iteration order.

/// Languages whose line syntax the content rules understand. Any other file
/// gets an explicit `unknown` coverage outcome for the content rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    Rust,
    Go,
    /// TypeScript and JavaScript (`ts`, `tsx`, `mts`, `cts`, `js`, `jsx`, `mjs`, `cjs`).
    JavaScript,
    Python,
}

impl Language {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::Go => "go",
            Self::JavaScript => "javascript_typescript",
            Self::Python => "python",
        }
    }
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn extension(path: &str) -> Option<String> {
    let name = file_name(path);
    let (stem, ext) = name.rsplit_once('.')?;
    (!stem.is_empty()).then(|| ext.to_ascii_lowercase())
}

fn segments(path: &str) -> impl Iterator<Item = String> + '_ {
    let mut parts: Vec<&str> = path.split('/').collect();
    parts.pop(); // directories only
    parts.into_iter().map(str::to_ascii_lowercase)
}

pub fn language(path: &str) -> Option<Language> {
    match extension(path)?.as_str() {
        "rs" => Some(Language::Rust),
        "go" => Some(Language::Go),
        "ts" | "tsx" | "mts" | "cts" | "js" | "jsx" | "mjs" | "cjs" => Some(Language::JavaScript),
        "py" => Some(Language::Python),
        _ => None,
    }
}

/// Path-based test file detection (language independent).
pub fn is_test_path(path: &str) -> bool {
    let name = file_name(path).to_ascii_lowercase();
    if segments(path).any(|s| matches!(s.as_str(), "test" | "tests" | "__tests__" | "spec")) {
        return true;
    }
    name.ends_with("_test.go")
        || name.ends_with("_test.rs")
        || name == "tests.rs"
        || name.ends_with("_test.py")
        || (name.starts_with("test_") && name.ends_with(".py"))
        || [".test.", ".spec."].iter().any(|m| name.contains(m))
}

pub const DEPENDENCY_MANIFESTS: &[&str] = &[
    "cargo.toml",
    "package.json",
    "go.mod",
    "pyproject.toml",
    "pipfile",
    "setup.cfg",
    "gemfile",
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
    "composer.json",
];

pub const LOCKFILES: &[&str] = &[
    "cargo.lock",
    "package-lock.json",
    "npm-shrinkwrap.json",
    "pnpm-lock.yaml",
    "yarn.lock",
    "bun.lockb",
    "go.sum",
    "pipfile.lock",
    "poetry.lock",
    "uv.lock",
    "gemfile.lock",
    "composer.lock",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DependencyKind {
    Manifest,
    Lockfile,
}

impl DependencyKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Manifest => "manifest",
            Self::Lockfile => "lockfile",
        }
    }
}

pub fn dependency_kind(path: &str) -> Option<DependencyKind> {
    let name = file_name(path).to_ascii_lowercase();
    if LOCKFILES.contains(&name.as_str()) {
        Some(DependencyKind::Lockfile)
    } else if DEPENDENCY_MANIFESTS.contains(&name.as_str())
        || (name.starts_with("requirements") && name.ends_with(".txt"))
    {
        Some(DependencyKind::Manifest)
    } else {
        None
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfigKind {
    /// CI workflow or pipeline definition. Significant even with low churn.
    Ci,
    Container,
    Environment,
    /// YAML/TOML/JSON/INI under a `config`/`configs` directory.
    Application,
}

impl ConfigKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ci => "ci",
            Self::Container => "container",
            Self::Environment => "environment",
            Self::Application => "application",
        }
    }
}

pub fn config_kind(path: &str) -> Option<ConfigKind> {
    let lower = path.to_ascii_lowercase();
    let name = file_name(&lower);
    let ext = extension(&lower);
    let yaml = matches!(ext.as_deref(), Some("yml" | "yaml"));
    if (lower.starts_with(".github/workflows/") || lower.starts_with(".github/actions/")) && yaml
        || name == ".gitlab-ci.yml"
        || lower.starts_with(".circleci/")
        || name == "jenkinsfile"
        || name == "azure-pipelines.yml"
    {
        return Some(ConfigKind::Ci);
    }
    if name == "dockerfile"
        || name.starts_with("dockerfile.")
        || name.ends_with(".dockerfile")
        || ((name.starts_with("docker-compose") || name.starts_with("compose.")) && yaml)
    {
        return Some(ConfigKind::Container);
    }
    if name == ".env" || name.starts_with(".env.") {
        return Some(ConfigKind::Environment);
    }
    if segments(path).any(|s| s == "config" || s == "configs")
        && matches!(
            ext.as_deref(),
            Some("yml" | "yaml" | "toml" | "json" | "ini")
        )
    {
        return Some(ConfigKind::Application);
    }
    None
}

/// Path-based generated-file hints, in a fixed order.
pub fn generated_path_hints(path: &str) -> Vec<&'static str> {
    let name = file_name(path).to_ascii_lowercase();
    let mut hints = Vec::new();
    if LOCKFILES.contains(&name.as_str()) {
        hints.push("lockfile");
    }
    if segments(path).any(|s| {
        matches!(
            s.as_str(),
            "generated" | "__generated__" | "dist" | "vendor" | "node_modules"
        )
    }) {
        hints.push("generated_directory");
    }
    if [
        ".min.js", ".min.css", ".pb.go", "_pb2.py", ".g.dart", ".snap",
    ]
    .iter()
    .any(|s| name.ends_with(s))
        || name.contains(".generated.")
    {
        hints.push("generated_file_name");
    }
    hints
}

/// Conventional generated-code header markers (Go's `Code generated ... DO
/// NOT EDIT.` and the cross-language `@generated`).
pub fn is_generated_marker(text: &str) -> bool {
    text.contains("@generated") || (text.contains("Code generated") && text.contains("DO NOT EDIT"))
}

fn is_ident(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$'
}

/// True when `pat` occurs in `line` not preceded by an identifier character.
fn has_token(line: &str, pat: &str) -> bool {
    let bytes = line.as_bytes();
    line.match_indices(pat)
        .any(|(i, _)| i == 0 || !is_ident(bytes[i - 1]))
}

fn is_comment(trimmed: &str, language: Language) -> bool {
    match language {
        Language::Python => trimmed.starts_with('#'),
        _ => trimmed.starts_with("//") || trimmed.starts_with("/*") || trimmed.starts_with('*'),
    }
}

/// What a test-related line contains.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestLine {
    Assertion,
    TestDefinition,
}

impl TestLine {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Assertion => "assertion",
            Self::TestDefinition => "test_definition",
        }
    }
}

/// Whether the assertion rule scans this file at all. Rust keeps unit tests
/// inline in source files, so every Rust file is scanned; for other languages
/// only test files are scanned, so production `assert` calls are not
/// mistaken for tests.
pub fn scans_test_lines(language: Language, is_test: bool) -> bool {
    language == Language::Rust || is_test
}

pub fn test_line(text: &str, language: Language) -> Option<TestLine> {
    let t = text.trim();
    if t.is_empty() || is_comment(t, language) {
        return None;
    }
    match language {
        Language::Rust => {
            if [
                "#[test]",
                "#[tokio::test",
                "#[should_panic",
                "#[test_case",
                "#[rstest",
            ]
            .iter()
            .any(|p| t.starts_with(p))
            {
                Some(TestLine::TestDefinition)
            } else if [
                "assert!(",
                "assert_eq!(",
                "assert_ne!(",
                "assert_matches!(",
                "debug_assert!(",
                "debug_assert_eq!(",
                "debug_assert_ne!(",
            ]
            .iter()
            .any(|p| has_token(t, p))
            {
                Some(TestLine::Assertion)
            } else {
                None
            }
        }
        Language::Go => {
            if ["func Test", "func Benchmark", "func Fuzz", "func Example"]
                .iter()
                .any(|p| t.starts_with(p))
            {
                Some(TestLine::TestDefinition)
            } else if [
                "t.Error(",
                "t.Errorf(",
                "t.Fatal(",
                "t.Fatalf(",
                "t.Fail(",
                "t.FailNow(",
                "assert.",
                "require.",
            ]
            .iter()
            .any(|p| has_token(t, p))
            {
                Some(TestLine::Assertion)
            } else {
                None
            }
        }
        Language::JavaScript => {
            if [
                "it(",
                "test(",
                "it.only(",
                "test.only(",
                "it.each",
                "test.each",
            ]
            .iter()
            .any(|p| t.starts_with(p))
            {
                Some(TestLine::TestDefinition)
            } else if ["expect(", "assert(", "assert.", "expectTypeOf("]
                .iter()
                .any(|p| has_token(t, p))
            {
                Some(TestLine::Assertion)
            } else {
                None
            }
        }
        Language::Python => {
            if t.starts_with("def test") || t.starts_with("async def test") {
                Some(TestLine::TestDefinition)
            } else if t.starts_with("assert ")
                || t.starts_with("assert(")
                || has_token(t, "self.assert")
                || has_token(t, "pytest.raises(")
            {
                Some(TestLine::Assertion)
            } else {
                None
            }
        }
    }
}

/// Signature-looking public declaration. Purely syntactic: it cannot tell a
/// crate-private module's `pub fn` from a real public API, which is why the
/// rule is an attention hint and states that limitation.
pub fn public_signature(text: &str, language: Language) -> bool {
    match language {
        Language::Rust => {
            let t = text.trim_start();
            [
                "pub fn ",
                "pub async fn ",
                "pub const fn ",
                "pub unsafe fn ",
                "pub struct ",
                "pub enum ",
                "pub trait ",
                "pub type ",
                "pub const ",
                "pub static ",
                "pub mod ",
                "pub use ",
            ]
            .iter()
            .any(|p| t.starts_with(p))
        }
        // Go and Python visibility depends on column-0 declarations.
        Language::Go => {
            let rest = if let Some(r) = text.strip_prefix("func ") {
                match r.strip_prefix('(') {
                    // Method: skip the receiver.
                    Some(r) => r.split_once(')').map(|(_, r)| r.trim_start()),
                    None => Some(r),
                }
            } else {
                text.strip_prefix("type ")
            };
            rest.and_then(|r| r.chars().next())
                .is_some_and(|c| c.is_ascii_uppercase())
        }
        Language::JavaScript => text.trim_start().starts_with("export "),
        Language::Python => ["def ", "async def ", "class "].iter().any(|p| {
            text.strip_prefix(p)
                .and_then(|r| r.chars().next())
                .is_some_and(|c| c.is_ascii_alphabetic())
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn languages_are_detected_by_extension_only() {
        assert_eq!(language("src/a.rs"), Some(Language::Rust));
        assert_eq!(language("x/Y.TSX"), Some(Language::JavaScript));
        assert_eq!(language("a.rb"), None);
        assert_eq!(language(".rs"), None);
        assert_eq!(language("Makefile"), None);
    }

    #[test]
    fn test_paths() {
        assert!(is_test_path("api/identity_test.go"));
        assert!(is_test_path("tests/cli.rs"));
        assert!(is_test_path("app/src/x.test.ts"));
        assert!(is_test_path("pkg/test_x.py"));
        assert!(!is_test_path("src/testing.rs"));
        assert!(!is_test_path("src/contest.go"));
    }

    #[test]
    fn dependency_config_and_generated() {
        assert_eq!(
            dependency_kind("Cargo.toml"),
            Some(DependencyKind::Manifest)
        );
        assert_eq!(
            dependency_kind("app/pnpm-lock.yaml"),
            Some(DependencyKind::Lockfile)
        );
        assert_eq!(
            dependency_kind("requirements-dev.txt"),
            Some(DependencyKind::Manifest)
        );
        assert_eq!(dependency_kind("src/cargo.rs"), None);
        assert_eq!(
            config_kind(".github/workflows/ci.yml"),
            Some(ConfigKind::Ci)
        );
        assert_eq!(config_kind("Dockerfile"), Some(ConfigKind::Container));
        assert_eq!(
            config_kind("deploy/config/app.yaml"),
            Some(ConfigKind::Application)
        );
        assert_eq!(config_kind("src/config.rs"), None);
        assert_eq!(
            generated_path_hints("app/src/generated/X.ts"),
            vec!["generated_directory"]
        );
        assert!(generated_path_hints("src/main.rs").is_empty());
        assert!(is_generated_marker(
            "// Code generated by protoc. DO NOT EDIT."
        ));
    }

    #[test]
    fn test_lines_per_language() {
        assert_eq!(
            test_line("    assert_eq!(a, b);", Language::Rust),
            Some(TestLine::Assertion)
        );
        assert_eq!(
            test_line("#[test]", Language::Rust),
            Some(TestLine::TestDefinition)
        );
        assert_eq!(test_line("// assert!(x)", Language::Rust), None);
        assert_eq!(test_line("my_assert!(x)", Language::Rust), None);
        assert_eq!(
            test_line("\tt.Fatalf(\"x\")", Language::Go),
            Some(TestLine::Assertion)
        );
        assert_eq!(
            test_line("func TestX(t *testing.T) {", Language::Go),
            Some(TestLine::TestDefinition)
        );
        assert_eq!(
            test_line("expect(x).toBe(1)", Language::JavaScript),
            Some(TestLine::Assertion)
        );
        assert_eq!(
            test_line("  assert x == 1", Language::Python),
            Some(TestLine::Assertion)
        );
        assert_eq!(test_line("x = 1", Language::Python), None);
    }

    #[test]
    fn public_signatures() {
        assert!(public_signature("pub fn scan(", Language::Rust));
        assert!(!public_signature("pub(crate) fn scan(", Language::Rust));
        assert!(public_signature("func (c *Client) Run() {", Language::Go));
        assert!(!public_signature("func (c *Client) run() {", Language::Go));
        assert!(!public_signature("\tfunc Inner() {", Language::Go));
        assert!(public_signature("type Store struct {", Language::Go));
        assert!(public_signature(
            "export function a() {}",
            Language::JavaScript
        ));
        assert!(public_signature("def run(x):", Language::Python));
        assert!(!public_signature("def _private(x):", Language::Python));
        assert!(!public_signature("    def method(self):", Language::Python));
    }
}
