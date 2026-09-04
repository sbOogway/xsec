//! The trading universe: a plain-text list of base assets, one per line.
//!
//! Blank lines and lines whose first non-whitespace character is `#` are
//! ignored; an inline `# …` after a symbol is stripped; symbols are
//! upper-cased and de-duplicated, keeping the order they first appear. See
//! `universe.txt` at the repo root for the list this run ships with.

use std::{collections::HashSet, path::Path};

use anyhow::{Context, Result, bail};

/// Read and parse a universe file. Errors if the file is missing or unreadable,
/// or if it contains no symbols once comments and blanks are stripped.
pub fn read_universe(path: &Path) -> Result<Vec<String>> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("read universe file {}", path.display()))?;
    let bases = parse_universe(&text);
    if bases.is_empty() {
        bail!("universe file {} contains no symbols", path.display());
    }
    Ok(bases)
}

/// Parse the text of a universe file. See the module docs for the rules.
fn parse_universe(text: &str) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut bases = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        // Strip an inline "SYMBOL  # note" comment, then normalise.
        let symbol = line
            .split_once('#')
            .map_or(line, |(head, _)| head.trim())
            .to_ascii_uppercase();
        if !symbol.is_empty() && seen.insert(symbol.clone()) {
            bases.push(symbol);
        }
    }
    bases
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    fn write(text: &str) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(text.as_bytes()).unwrap();
        f
    }

    #[test]
    fn skips_blank_and_comment_lines() {
        let got = parse_universe("# header\n\nBTC\n   \nETH\n#trailing\n");
        assert_eq!(got, ["BTC", "ETH"]);
    }

    #[test]
    fn strips_inline_comments_and_uppercases() {
        let got = parse_universe("btc   # the big one\nEth\n");
        assert_eq!(got, ["BTC", "ETH"]);
    }

    #[test]
    fn dedupes_keeping_first_seen_order() {
        let got = parse_universe("SOL\nBTC\nSOL\nETH\nbtc\n");
        assert_eq!(got, ["SOL", "BTC", "ETH"]);
    }

    #[test]
    fn missing_file_is_an_error() {
        let err = read_universe(Path::new("does/not/exist.txt")).unwrap_err();
        assert!(err.to_string().contains("read universe file"));
    }

    #[test]
    fn empty_after_filtering_is_an_error() {
        let f = write("# only comments\n\n   \n");
        let err = read_universe(f.path()).unwrap_err();
        assert!(err.to_string().contains("no symbols"));
    }
}
