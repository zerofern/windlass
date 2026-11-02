//! `docs/mam-rules.md` lists what must never happen on MAM and the tests that
//! prove it. Every rule names at least one test, and every test named exists.

use std::{error::Error, fs, path::Path};

type TestResult = Result<(), Box<dyn Error>>;

/// The `path::test` references in a piece of the page.
fn references(text: &str) -> Vec<(&str, &str)> {
    text.split('`')
        .skip(1)
        .step_by(2)
        .filter_map(|quoted| quoted.split_once("::"))
        .collect()
}

/// Whether this file defines this test.
fn defines_test(source: &str, name: &str) -> bool {
    let lines: Vec<&str> = source.lines().collect();
    lines.iter().enumerate().any(|(index, line)| {
        let defines = line.trim_start().starts_with(&format!("fn {name}("))
            || line.trim_start().starts_with(&format!("async fn {name}("));
        defines
            && lines
                .get(index.saturating_sub(5)..index)
                .unwrap_or_default()
                .iter()
                .any(|above| above.contains("#[test]") || above.contains("#[tokio::test]"))
    })
}

#[test]
fn every_rule_names_tests_that_exist() -> TestResult {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let page = fs::read_to_string(root.join("docs/mam-rules.md"))?;
    let rules: Vec<&str> = page
        .split("\n- **Never")
        .skip(1)
        .map(|rule| rule.split("\n\n").next().unwrap_or(rule))
        .collect();
    assert!(rules.len() >= 30, "only {} rules", rules.len());
    for rule in &rules {
        let named = references(rule);
        assert!(!named.is_empty(), "a rule names no test: Never{rule}");
        for (path, name) in named {
            let source =
                fs::read_to_string(root.join(path)).map_err(|error| format!("{path}: {error}"))?;
            assert!(
                defines_test(&source, name),
                "{path} has no test {name}, named for: Never{rule}"
            );
        }
    }
    Ok(())
}
