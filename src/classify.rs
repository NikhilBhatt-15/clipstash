//! What a clip looks like (code, link, colour, ...) and the text transforms
//! applied when pasting it.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Text,
    Code,
    Link,
    Path,
    /// A CSS-style colour, with its RGB value for the swatch.
    Color(u8, u8, u8),
    /// A copied image; never produced by `classify`, which only sees text.
    Image,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Text => "Text",
            Kind::Code => "Code",
            Kind::Link => "Link",
            Kind::Path => "Path",
            Kind::Color(..) => "Colour",
            Kind::Image => "Image",
        }
    }

    pub fn is_mono(self) -> bool {
        matches!(self, Kind::Code | Kind::Path | Kind::Color(..))
    }
}

/// Only the head of very large clips is inspected.
const SAMPLE_BYTES: usize = 8 * 1024;

pub fn classify(text: &str) -> Kind {
    let t = text.trim();
    let single = !t.contains('\n');
    if single {
        if let Some(c) = parse_color(t) {
            return c;
        }
        if is_link(t) {
            return Kind::Link;
        }
        if is_path(t) {
            return Kind::Path;
        }
    }
    let mut end = text.len().min(SAMPLE_BYTES);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    if looks_like_code(&text[..end]) {
        Kind::Code
    } else {
        Kind::Text
    }
}

fn is_link(t: &str) -> bool {
    let lower = t.to_ascii_lowercase();
    (lower.starts_with("http://") || lower.starts_with("https://") || lower.starts_with("www."))
        && !t.chars().any(char::is_whitespace)
        && t.len() > 7
}

fn is_path(t: &str) -> bool {
    let b = t.as_bytes();
    let drive = b.len() >= 3 && b[0].is_ascii_alphabetic() && b[1] == b':' && (b[2] == b'\\' || b[2] == b'/');
    let unc = t.starts_with("\\\\") && t.len() > 2;
    (drive || unc) && !t.contains(['<', '>', '|', '"', '\t'])
}

fn parse_color(t: &str) -> Option<Kind> {
    let hex = t.strip_prefix('#')?;
    let digits: Vec<u8> = hex
        .chars()
        .map(|c| c.to_digit(16).map(|d| d as u8))
        .collect::<Option<_>>()?;
    let (r, g, b) = match digits.len() {
        3 | 4 => (digits[0] * 17, digits[1] * 17, digits[2] * 17),
        6 | 8 => (digits[0] << 4 | digits[1], digits[2] << 4 | digits[3], digits[4] << 4 | digits[5]),
        _ => return None,
    };
    Some(Kind::Color(r, g, b))
}

const KEYWORDS: &[&str] = &[
    "fn ", "let ", "const ", "var ", "def ", "class ", "import ", "from ", "return ", "public ", "private ",
    "static ", "function ", "func ", "struct ", "enum ", "impl ", "pub ", "#include", "#define", "using ",
    "namespace ", "package ", "async ", "await ", "SELECT ", "INSERT ", "UPDATE ", "CREATE ", "elif ",
    "else {", "} else", "if (", "for (", "while (", "match ", "switch (", "console.", "print(", "println!",
    "#!/", "export ", "interface ", "type ", "try {", "catch (", "echo ", "sudo ", "git ", "npm ", "cargo ",
    "docker ", "kubectl ", "pip ", "cd ", "$ ",
];

/// Heuristic: punctuation density, indentation, line endings typical of code,
/// and common keywords. Prose rarely scores on more than one of these.
fn looks_like_code(s: &str) -> bool {
    let lines: Vec<&str> = s.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.is_empty() {
        return false;
    }
    let mut score = 0.0f32;
    let n = lines.len() as f32;

    // `;`, `{`, `}` endings are strong evidence; `)` and `:` also end ordinary
    // sentences ("... (room 4)", "Agenda:"), so they count for less.
    let endings: f32 = lines
        .iter()
        .map(|l| {
            let e = l.trim_end();
            if e.ends_with(';') || e.ends_with('{') || e.ends_with('}') {
                3.0
            } else if e.ends_with(')') || e.ends_with(':') && !e.ends_with("::") {
                1.0
            } else {
                0.0
            }
        })
        .sum();
    score += endings / n;

    let indented = lines.iter().filter(|l| l.starts_with("    ") || l.starts_with('\t')).count() as f32;
    if lines.len() > 1 {
        score += indented / n * 2.0;
    }

    // Parentheses are common in prose, so they don't count towards density.
    let symbols = s.chars().filter(|c| matches!(c, '{' | '}' | ';' | '=' | '<' | '>' | '[' | ']')).count() as f32;
    let letters = s.chars().filter(|c| c.is_alphanumeric()).count().max(1) as f32;
    score += (symbols / letters * 10.0).min(2.0);

    let kw = lines
        .iter()
        .filter(|l| {
            let t = l.trim_start();
            KEYWORDS.iter().any(|k| t.starts_with(k))
        })
        .count() as f32;
    score += (kw / n * 3.0).min(2.0);

    for marker in ["=>", "::", "->", "!=", "==", "&&", "||", "();", "</", "/>", "${", "--"] {
        if s.contains(marker) {
            score += 0.4;
        }
    }

    // Prose: long lines of mostly words ending in sentence punctuation.
    let sentences = lines.iter().filter(|l| {
        let e = l.trim_end();
        (e.ends_with('.') || e.ends_with('?') || e.ends_with('!')) && e.split_whitespace().count() > 6
    });
    score -= sentences.count() as f32 / n * 2.5;

    score >= 2.0
}

/// "Paste trimmed": drops leading/trailing blank lines, trailing spaces on
/// each line, and the indentation shared by every line. That is what you want
/// when moving a snippet out of a deeply nested block.
pub fn dedent(text: &str) -> String {
    let lines: Vec<&str> = text.lines().map(|l| l.trim_end()).collect();
    let first = lines.iter().position(|l| !l.is_empty()).unwrap_or(lines.len());
    let last = lines.iter().rposition(|l| !l.is_empty()).map_or(first, |i| i + 1);
    let body = &lines[first..last];
    let common = body
        .iter()
        .filter(|l| !l.is_empty())
        .map(|l| l.len() - l.trim_start_matches([' ', '\t']).len())
        .min()
        .unwrap_or(0);
    let mut out = String::with_capacity(text.len());
    for (i, l) in body.iter().enumerate() {
        if i > 0 {
            out.push('\n');
        }
        // `common` counts bytes of ASCII whitespace, so slicing is safe.
        out.push_str(l.get(common..).unwrap_or(""));
    }
    out
}

/// Windows text controls expect CRLF; lone LF or CR become CRLF.
pub fn to_crlf(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + text.len() / 16);
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                out.push_str("\r\n");
            }
            '\n' => out.push_str("\r\n"),
            c => out.push(c),
        }
    }
    out
}

/// One-line label for the list: the first non-blank line, cut to `max` chars.
pub fn title(text: &str, max: usize) -> String {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    let mut out: String = line.chars().map(|c| if c == '\t' { ' ' } else { c }).take(max).collect();
    if line.chars().count() > max {
        out.push('…');
    }
    out
}

/// Like `title`, but while searching prefers the first line that contains a
/// search term, cut so the hit is visible ("…start of the hit").
pub fn title_matching(text: &str, terms: &[Vec<char>], max: usize) -> String {
    let first = title(text, max);
    if terms.is_empty() || !match_ranges(&first.chars().collect::<Vec<_>>(), terms).is_empty() {
        return first;
    }
    for line in text.lines() {
        let chars: Vec<char> = line.trim().chars().map(|c| if c == '\t' { ' ' } else { c }).collect();
        let Some(&(start, _)) = match_ranges(&chars, terms).first() else { continue };
        // Keep a little context before the hit when it's far into the line.
        let from = start.saturating_sub(24);
        let mut out = String::new();
        if from > 0 {
            out.push('…');
        }
        out.extend(chars[from..].iter().take(max));
        if chars.len() - from > max {
            out.push('…');
        }
        return out;
    }
    first
}

pub fn line_count(text: &str) -> usize {
    text.lines().count().max(1)
}

/// "just now", "5m ago", "3h ago", "yesterday", "12d ago".
pub fn relative_time(then_ms: i64, now_ms: i64) -> String {
    let s = (now_ms - then_ms).max(0) / 1000;
    match s {
        0..=44 => "just now".into(),
        45..=3599 => format!("{}m ago", (s + 30) / 60),
        3600..=86_399 => format!("{}h ago", s / 3600),
        86_400..=172_799 => "yesterday".into(),
        _ => format!("{}d ago", s / 86_400),
    }
}

/// Char ranges (start, end) where any of `terms` occurs, ASCII case-insensitively,
/// merged and sorted. Used to highlight search hits.
pub fn match_ranges(text: &[char], terms: &[Vec<char>]) -> Vec<(usize, usize)> {
    let mut ranges = Vec::new();
    for term in terms.iter().filter(|t| !t.is_empty()) {
        if term.len() > text.len() {
            continue;
        }
        for i in 0..=text.len() - term.len() {
            if text[i..i + term.len()].iter().zip(term).all(|(a, b)| a.eq_ignore_ascii_case(b)) {
                ranges.push((i, i + term.len()));
            }
        }
    }
    ranges.sort_unstable();
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for (s, e) in ranges {
        match merged.last_mut() {
            Some(last) if s <= last.1 => last.1 = last.1.max(e),
            _ => merged.push((s, e)),
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_code() {
        let rust = "fn main() {\n    let x = 5;\n    println!(\"{x}\");\n}";
        let py = "def add(a, b):\n    return a + b\n";
        let js = "const f = (x) => x * 2;";
        let shell = "cargo build --release";
        let json = "{\n  \"name\": \"clipstash\",\n  \"version\": [0, 2]\n}";
        let html = "<div class=\"row\">\n  <span>hi</span>\n</div>";
        let sql = "SELECT id, text FROM clips WHERE pinned = 1;";
        let cs = "public class Foo\n{\n    private int x = 0;\n}";
        for c in [rust, py, js, shell, json, html, sql, cs] {
            assert_eq!(classify(c), Kind::Code, "{c:?}");
        }
    }

    #[test]
    fn prose_is_text() {
        let prose = "Hi Sam, thanks for the notes from yesterday. I will send the revised draft by Friday afternoon.\nLet me know if that works for you.";
        assert_eq!(classify(prose), Kind::Text);
        assert_eq!(classify("hello"), Kind::Text);
        assert_eq!(classify("Meeting moved to 3pm (room 4)"), Kind::Text);
        assert_eq!(classify("Agenda:\n- budget review\n- hiring plan (Q3)\n- open questions"), Kind::Text);
    }

    #[test]
    fn links_paths_colours() {
        assert_eq!(classify("https://example.com/a?b=c"), Kind::Link);
        assert_eq!(classify("  www.rust-lang.org  "), Kind::Link);
        assert_eq!(classify("C:\\Users\\me\\file.txt"), Kind::Path);
        assert_eq!(classify("\\\\server\\share"), Kind::Path);
        assert_eq!(classify("#ff8800"), Kind::Color(255, 136, 0));
        assert_eq!(classify("#0f0"), Kind::Color(0, 255, 0));
        assert_eq!(classify("#12345"), Kind::Text);
        assert_eq!(classify("#hashtag"), Kind::Text);
    }

    #[test]
    fn dedent_strips_common_indent_and_blank_edges() {
        let src = "\n\n        if x {\n            y();   \n\n        }\n  \n";
        assert_eq!(dedent(src), "if x {\n    y();\n\n}");
        assert_eq!(dedent("\tone\n\t\ttwo"), "one\n\ttwo");
        assert_eq!(dedent("   "), "");
    }

    #[test]
    fn crlf_normalisation() {
        assert_eq!(to_crlf("a\nb\r\nc\rd"), "a\r\nb\r\nc\r\nd");
        assert_eq!(to_crlf("plain"), "plain");
    }

    #[test]
    fn titles() {
        assert_eq!(title("\n\n   first line  \nsecond", 50), "first line");
        assert_eq!(title("abcdef", 3), "abc…");
        assert_eq!(title("a\tb", 10), "a b");
    }

    #[test]
    fn title_shows_matching_line_while_searching() {
        let code = "fn main() {\n    for line in input.lines() {\n    }\n}";
        let terms = |q: &str| q.split_whitespace().map(|t| t.chars().collect()).collect::<Vec<Vec<char>>>();
        assert_eq!(title_matching(code, &terms(""), 50), "fn main() {");
        assert_eq!(title_matching(code, &terms("main"), 50), "fn main() {");
        assert_eq!(title_matching(code, &terms("LINES"), 50), "for line in input.lines() {");
        let long = format!("{}needle", "x".repeat(100));
        assert_eq!(title_matching(&long, &terms("needle"), 50), format!("…{}needle", "x".repeat(24)));
        assert_eq!(title_matching(code, &terms("absent"), 50), "fn main() {");
    }

    #[test]
    fn relative_times() {
        let now = 1_000_000_000;
        assert_eq!(relative_time(now - 10_000, now), "just now");
        assert_eq!(relative_time(now - 5 * 60_000, now), "5m ago");
        assert_eq!(relative_time(now - 3 * 3_600_000, now), "3h ago");
        assert_eq!(relative_time(now - 30 * 3_600_000, now), "yesterday");
        assert_eq!(relative_time(now - 5 * 86_400_000, now), "5d ago");
    }

    #[test]
    fn match_ranges_merge_and_ignore_case() {
        let text: Vec<char> = "Hello hello world".chars().collect();
        let terms = vec!["HELLO".chars().collect(), "lo w".chars().collect()];
        // "HELLO" hits 0..5 and 6..11; "lo w" hits 9..13 and merges with the second.
        assert_eq!(match_ranges(&text, &terms), vec![(0, 5), (6, 13)]);
    }
}
