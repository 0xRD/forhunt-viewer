//! Documentation shown in the app (File > Settings, one tab per page):
//! the Markdown files of `docs/`, embedded at build time and drawn by a
//! small renderer (headings, paragraphs, bullets, tables, `code`,
//! **bold**, *italics*, links as plain text). The app does not open a
//! browser: eframe is built without its `links` feature.

use eframe::egui::{self, Color32, FontId, RichText, TextFormat, TextStyle, Ui, text::LayoutJob};

/// (title, Markdown).
pub const PAGES: [(&str, &str); 1] = [(
    "Date formats",
    include_str!("../../../docs/DATE-FORMATS.md"),
)];

/// A block of a page.
#[derive(Debug, PartialEq)]
pub enum Block {
    Heading(usize, String),
    Para(String),
    Bullet(String),
    /// Header cells, then rows.
    Table(Vec<String>, Vec<Vec<String>>),
}

fn cells(line: &str) -> Vec<String> {
    let t = line.trim().trim_start_matches('|').trim_end_matches('|');
    // `\|` is a pipe inside a cell.
    let mut out = vec![String::new()];
    let mut chars = t.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&'|') => {
                out.last_mut().unwrap().push('|');
                chars.next();
            }
            '|' => out.push(String::new()),
            c => out.last_mut().unwrap().push(c),
        }
    }
    out.into_iter().map(|c| c.trim().to_string()).collect()
}

/// Splits a page into blocks. Paragraph and bullet lines that continue on
/// the next lines are joined.
pub fn parse(md: &str) -> Vec<Block> {
    let mut out = Vec::new();
    let lines: Vec<&str> = md.lines().collect();
    let mut i = 0;
    while i < lines.len() {
        let l = lines[i];
        let t = l.trim();
        if t.is_empty() {
            i += 1;
        } else if let Some(h) = t.strip_prefix('#') {
            let level = 1 + h.chars().take_while(|&c| c == '#').count();
            out.push(Block::Heading(
                level,
                h.trim_start_matches('#').trim().into(),
            ));
            i += 1;
        } else if t.starts_with('|') {
            let head = cells(t);
            let mut rows = Vec::new();
            i += 1;
            while i < lines.len() && lines[i].trim().starts_with('|') {
                let r = cells(lines[i]);
                if !r.iter().all(|c| c.chars().all(|x| matches!(x, '-' | ':'))) {
                    rows.push(r);
                }
                i += 1;
            }
            out.push(Block::Table(head, rows));
        } else {
            let bullet = t.starts_with("- ");
            let mut text = t.trim_start_matches("- ").to_string();
            i += 1;
            while i < lines.len() {
                let n = lines[i].trim();
                if n.is_empty() || n.starts_with('#') || n.starts_with('|') || n.starts_with("- ") {
                    break;
                }
                text.push(' ');
                text.push_str(n);
                i += 1;
            }
            out.push(if bullet {
                Block::Bullet(text)
            } else {
                Block::Para(text)
            });
        }
    }
    out
}

/// Inline spans: (text, code, bold, italic). Links keep their text.
pub fn spans(s: &str) -> Vec<(String, bool, bool, bool)> {
    let mut out: Vec<(String, bool, bool, bool)> = Vec::new();
    let (mut bold, mut italic) = (false, false);
    let mut cur = String::new();
    let flush = |cur: &mut String, out: &mut Vec<_>, code, bold, italic| {
        if !cur.is_empty() {
            out.push((std::mem::take(cur), code, bold, italic));
        }
    };
    let b: Vec<char> = s.chars().collect();
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            '`' => {
                flush(&mut cur, &mut out, false, bold, italic);
                let end = b[i + 1..].iter().position(|&c| c == '`').map(|p| i + 1 + p);
                match end {
                    Some(e) => {
                        out.push((b[i + 1..e].iter().collect(), true, bold, italic));
                        i = e + 1;
                    }
                    None => {
                        cur.push('`');
                        i += 1;
                    }
                }
            }
            '*' if b.get(i + 1) == Some(&'*') => {
                flush(&mut cur, &mut out, false, bold, italic);
                bold = !bold;
                i += 2;
            }
            '*' if italic || b.get(i + 1).is_some_and(|c| !c.is_whitespace()) => {
                flush(&mut cur, &mut out, false, bold, italic);
                italic = !italic;
                i += 1;
            }
            '[' => {
                // [text](url) -> text
                let close = b[i..].iter().position(|&c| c == ']').map(|p| i + p);
                match close.filter(|&c| b.get(c + 1) == Some(&'(')) {
                    Some(c) => {
                        cur.extend(&b[i + 1..c]);
                        let end = b[c..]
                            .iter()
                            .position(|&x| x == ')')
                            .map_or(b.len(), |p| c + p);
                        i = end + 1;
                    }
                    None => {
                        cur.push('[');
                        i += 1;
                    }
                }
            }
            c => {
                cur.push(c);
                i += 1;
            }
        }
    }
    flush(&mut cur, &mut out, false, bold, italic);
    out
}

fn rich(ui: &Ui, s: &str, size: Option<f32>) -> LayoutJob {
    let body = TextStyle::Body.resolve(ui.style());
    let mono = TextStyle::Monospace.resolve(ui.style());
    let (text, strong) = (ui.visuals().text_color(), ui.visuals().strong_text_color());
    let code_bg = ui.visuals().code_bg_color;
    let mut job = LayoutJob::default();
    for (t, code, bold, italic) in spans(s) {
        let font = match (code, size) {
            (true, _) => mono.clone(),
            (false, Some(sz)) => FontId::proportional(sz),
            (false, None) => body.clone(),
        };
        job.append(
            &t,
            0.0,
            TextFormat {
                font_id: font,
                color: if bold || size.is_some() { strong } else { text },
                italics: italic,
                background: if code { code_bg } else { Color32::TRANSPARENT },
                ..Default::default()
            },
        );
    }
    job.wrap.max_width = ui.available_width();
    job
}

/// Draws a page.
pub fn show(ui: &mut Ui, md: &str) {
    for (k, b) in parse(md).into_iter().enumerate() {
        match b {
            Block::Heading(level, t) => {
                ui.add_space(if level == 1 { 2.0 } else { 8.0 });
                let size = if level == 1 { 20.0 } else { 16.0 };
                ui.label(rich(ui, &t, Some(size)));
            }
            Block::Para(t) => {
                ui.label(rich(ui, &t, None));
            }
            Block::Bullet(t) => {
                ui.horizontal_top(|ui| {
                    ui.label("•");
                    ui.label(rich(ui, &t, None));
                });
            }
            Block::Table(head, rows) => {
                egui::ScrollArea::horizontal()
                    .id_salt(("doc table", k))
                    .show(ui, |ui| {
                        egui::Grid::new(("doc grid", k))
                            .striped(true)
                            .num_columns(head.len())
                            .show(ui, |ui| {
                                for h in &head {
                                    ui.label(RichText::new(h.replace('`', "")).strong());
                                }
                                ui.end_row();
                                for r in &rows {
                                    for c in r {
                                        let mut job = rich(ui, c, None);
                                        job.wrap.max_width = 360.0;
                                        ui.label(job);
                                    }
                                    ui.end_row();
                                }
                            });
                    });
            }
        }
        ui.add_space(4.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_and_spans() {
        let md = "# Title\n\nSome `code` and\n**bold** text.\n\n- one\n  two\n- three\n\n\
                  | A | B |\n|---|---|\n| `x\\|y` | 2 |\n";
        let b = parse(md);
        assert_eq!(b[0], Block::Heading(1, "Title".into()));
        assert_eq!(b[1], Block::Para("Some `code` and **bold** text.".into()));
        assert_eq!(b[2], Block::Bullet("one two".into()));
        assert_eq!(b[3], Block::Bullet("three".into()));
        assert_eq!(
            b[4],
            Block::Table(
                vec!["A".into(), "B".into()],
                vec![vec!["`x|y`".into(), "2".into()]]
            )
        );
        let s = spans("a `b` **c** *d* [e](http://x) 3 * 4");
        assert_eq!(s[1], ("b".into(), true, false, false));
        assert_eq!(s[3], ("c".into(), false, true, false));
        assert_eq!(s[5], ("d".into(), false, false, true));
        assert_eq!(s.last().unwrap().0, " e 3 * 4");
    }

    #[test]
    fn date_formats_page_parses() {
        let b = parse(PAGES[0].1);
        assert!(matches!(&b[0], Block::Heading(1, t) if t == "Date formats"));
        assert!(b.iter().filter(|x| matches!(x, Block::Table(..))).count() >= 4);
    }
}
