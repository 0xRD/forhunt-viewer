//! Query language: text <-> filter tree ([`Expr`]).
//!
//! ```text
//! query     = [ or_expr ] ;
//! or_expr   = and_expr { "OR" and_expr } ;
//! and_expr  = unary { "AND" unary } ;
//! unary     = "NOT" unary | primary ;
//! primary   = "(" or_expr ")" | condition ;
//! condition = column test [ "cs" | "ci" ] ;
//! column    = name | "*" ;                      (* "*" = any column *)
//! test      = ( "=" | "==" | "!=" | "<>" | ">" | ">=" | "<" | "<=" ) value
//!           | ( "contains" | "startswith" | "endswith" | "regex"
//!             | "matches" ) value
//!           | "between" value "and" value
//!           | "in" "(" [ value { "," value } ] ")"
//!           | "is" [ "not" ] "blank"
//!           | "not" test ;           (* not contains / not in: their own ops *)
//! name      = value ;
//! value     = bare | '"' { char | '""' } '"' | "'" { char | "''" } "'" ;
//! bare      = char { char } ;  (* no space ( ) , = ! < > " ; not AND/OR/NOT *)
//! ```
//!
//! Keywords are case-insensitive. Precedence: NOT > AND > OR. A condition
//! without `cs` / `ci` takes the default case sensitivity given to
//! [`parse`]. Operands are kept as written: display-zone rewriting of time
//! operands (D-038) happens where the tree is evaluated, as for the
//! condition editor. `col not <test>` negates one condition (`Op::Not`,
//! what inverting a chip gives), `NOT col <test>` is the boolean NOT; both
//! select the same rows. Parsing is all or nothing; nesting is bounded
//! ([`MAX_DEPTH`]) and so is the text ([`MAX_LEN`]).

use std::fmt;

use crate::filter::{Expr, Op, Pred, Target};

/// A parse error: byte offset and length of the offending token, and what
/// the parser expected there.
#[derive(Clone, Debug, PartialEq)]
pub struct ParseError {
    pub at: usize,
    pub len: usize,
    /// 0-based character position of `at` (for a caret under the text).
    pub col: usize,
    pub msg: String,
    pub expected: Option<Expected>,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "at character {}: {}", self.col + 1, self.msg)
    }
}

impl std::error::Error for ParseError {}

/// What the parser wanted at an error (drives autocompletion).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Expected {
    /// A column name, `*`, `NOT` or `(`.
    Condition,
    Operator,
    Value,
    /// `(` opening an `in` list.
    ListOpen,
    /// `,` or `)` in an `in` list.
    ListNext,
    /// `AND` / `OR` / `cs` / `ci`, `)` or the end.
    Connective,
    /// A fixed keyword (`and` of between, `blank` of is blank).
    Keyword(&'static str),
}

impl Expected {
    fn text(self, depth: usize) -> String {
        match self {
            Expected::Condition => "a column name, NOT or (".into(),
            Expected::Operator => "an operator (=, !=, contains, not contains, startswith, \
                                   endswith, regex, in, not in, is blank, is not blank, >, >=, \
                                   <, <=, between)"
                .into(),
            Expected::Value => "a value".into(),
            Expected::ListOpen => "( to open the list of values".into(),
            Expected::ListNext => ", or ) in the list of values".into(),
            Expected::Connective if depth > 0 => "AND, OR or )".into(),
            Expected::Connective => "AND, OR or the end of the query".into(),
            Expected::Keyword(k) => k.to_uppercase(),
        }
    }
}

/// Deepest nesting of NOT and parentheses a query may have.
pub const MAX_DEPTH: usize = 128;
/// Longest query text accepted (typed, from history or from a session).
pub const MAX_LEN: usize = 64 << 10;

const KEYWORDS: &[&str] = &[
    "and",
    "or",
    "not",
    "contains",
    "startswith",
    "beginswith",
    "endswith",
    "regex",
    "matches",
    "in",
    "is",
    "blank",
    "between",
    "cs",
    "ci",
];

fn is_special(c: char) -> bool {
    c.is_whitespace() || matches!(c, '(' | ')' | ',' | '=' | '!' | '<' | '>' | '"')
}

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    LParen,
    RParen,
    Comma,
    Sym(&'static str),
    Word(String),
    Str(String),
}

#[derive(Clone, Debug)]
struct Token {
    tok: Tok,
    start: usize,
    end: usize,
}

fn char_col(text: &str, at: usize) -> usize {
    text[..at.min(text.len())].chars().count()
}

fn err(text: &str, at: usize, len: usize, msg: String, expected: Option<Expected>) -> ParseError {
    ParseError {
        at,
        len,
        col: char_col(text, at),
        msg,
        expected,
    }
}

fn lex(text: &str) -> Result<Vec<Token>, ParseError> {
    let mut out = Vec::new();
    let mut it = text.char_indices().peekable();
    while let Some(&(i, c)) = it.peek() {
        if c.is_whitespace() {
            it.next();
            continue;
        }
        let single = |t: Tok| Token {
            tok: t,
            start: i,
            end: i + 1,
        };
        match c {
            '(' => {
                it.next();
                out.push(single(Tok::LParen));
            }
            ')' => {
                it.next();
                out.push(single(Tok::RParen));
            }
            ',' => {
                it.next();
                out.push(single(Tok::Comma));
            }
            '=' | '!' | '<' | '>' => {
                it.next();
                let next = it.peek().map(|x| x.1);
                let two = match (c, next) {
                    ('=', Some('=')) => Some("=="),
                    ('!', Some('=')) => Some("!="),
                    ('<', Some('=')) => Some("<="),
                    ('<', Some('>')) => Some("<>"),
                    ('>', Some('=')) => Some(">="),
                    _ => None,
                };
                let (sym, end) = match (two, c) {
                    (Some(s), _) => {
                        it.next();
                        (s, i + 2)
                    }
                    (None, '=') => ("=", i + 1),
                    (None, '<') => ("<", i + 1),
                    (None, '>') => (">", i + 1),
                    _ => {
                        return Err(err(
                            text,
                            i,
                            1,
                            "unexpected !: write != (not equal) or NOT".into(),
                            None,
                        ));
                    }
                };
                out.push(Token {
                    tok: Tok::Sym(sym),
                    start: i,
                    end,
                });
            }
            '"' | '\'' => {
                it.next();
                let mut s = String::new();
                let mut closed = None;
                while let Some((j, d)) = it.next() {
                    if d == c {
                        if it.peek().map(|x| x.1) == Some(c) {
                            it.next();
                            s.push(c);
                        } else {
                            closed = Some(j + 1);
                            break;
                        }
                    } else {
                        s.push(d);
                    }
                }
                let Some(end) = closed else {
                    return Err(err(
                        text,
                        i,
                        text.len() - i,
                        format!("unterminated quoted text: add a closing {c}"),
                        None,
                    ));
                };
                out.push(Token {
                    tok: Tok::Str(s),
                    start: i,
                    end,
                });
            }
            _ => {
                let mut end = text.len();
                while let Some(&(j, d)) = it.peek() {
                    if is_special(d) {
                        end = j;
                        break;
                    }
                    it.next();
                }
                out.push(Token {
                    tok: Tok::Word(text[i..end].to_string()),
                    start: i,
                    end,
                });
            }
        }
    }
    Ok(out)
}

struct Parser<'a> {
    text: &'a str,
    toks: Vec<Token>,
    i: usize,
    /// Open parentheses (for the expected-token message).
    depth: usize,
    /// NOT and parenthesis levels (bounded by MAX_DEPTH).
    nest: usize,
    columns: &'a [String],
    default_cs: bool,
}

impl Parser<'_> {
    fn peek(&self) -> Option<&Token> {
        self.toks.get(self.i)
    }

    fn kw(&self, k: &str) -> bool {
        matches!(self.peek(), Some(Token { tok: Tok::Word(w), .. }) if w.eq_ignore_ascii_case(k))
    }

    fn fail<T>(&self, exp: Expected) -> Result<T, ParseError> {
        let (at, len, found) = match self.peek() {
            Some(t) => (
                t.start,
                t.end - t.start,
                format!("{:?}", &self.text[t.start..t.end]),
            ),
            None => (self.text.len(), 0, "the end of the query".into()),
        };
        Err(err(
            self.text,
            at,
            len,
            format!("expected {}, found {found}", exp.text(self.depth)),
            Some(exp),
        ))
    }

    fn or_expr(&mut self) -> Result<Expr, ParseError> {
        let mut xs = vec![self.and_expr()?];
        while self.kw("or") {
            self.i += 1;
            xs.push(self.and_expr()?);
        }
        Ok(if xs.len() == 1 {
            xs.pop().unwrap_or(Expr::Or(Vec::new()))
        } else {
            Expr::Or(xs)
        })
    }

    fn and_expr(&mut self) -> Result<Expr, ParseError> {
        let mut xs = vec![self.unary()?];
        while self.kw("and") {
            self.i += 1;
            xs.push(self.unary()?);
        }
        Ok(if xs.len() == 1 {
            xs.pop().unwrap_or(Expr::And(Vec::new()))
        } else {
            Expr::And(xs)
        })
    }

    fn unary(&mut self) -> Result<Expr, ParseError> {
        if self.kw("not") {
            self.enter()?;
            self.i += 1;
            let e = Expr::Not(Box::new(self.unary()?));
            self.nest -= 1;
            return Ok(e);
        }
        match self.peek().map(|t| &t.tok) {
            Some(Tok::LParen) => {
                self.enter()?;
                self.i += 1;
                self.depth += 1;
                let e = self.or_expr()?;
                if !matches!(self.peek().map(|t| &t.tok), Some(Tok::RParen)) {
                    return self.fail(Expected::Connective);
                }
                self.depth -= 1;
                self.nest -= 1;
                self.i += 1;
                Ok(e)
            }
            Some(Tok::Word(_) | Tok::Str(_)) => self.condition(),
            _ => self.fail(Expected::Condition),
        }
    }

    fn column(&mut self) -> Result<Target, ParseError> {
        let t = self.toks[self.i].clone();
        self.i += 1;
        let name = match &t.tok {
            Tok::Word(w) if w == "*" => return Ok(Target::Any),
            Tok::Word(w) | Tok::Str(w) => w.clone(),
            _ => unreachable!("condition starts with a word"),
        };
        if let Some(c) = self.columns.iter().position(|c| *c == name) {
            return Ok(Target::Column(c));
        }
        let low = name.to_lowercase();
        if let Some(c) = self.columns.iter().position(|c| c.to_lowercase() == low) {
            return Ok(Target::Column(c));
        }
        let close = close_matches(&name, self.columns);
        let hint = if !close.is_empty() {
            format!("; did you mean {}?", close.join(", "))
        } else if self.columns.len() <= 12 {
            let all: Vec<String> = self.columns.iter().map(|c| quote(c, true)).collect();
            format!(" (columns: {})", all.join(", "))
        } else {
            String::new()
        };
        Err(err(
            self.text,
            t.start,
            t.end - t.start,
            format!("unknown column {name:?}{hint}"),
            None,
        ))
    }

    fn value(&mut self) -> Result<String, ParseError> {
        match self.peek().map(|t| t.tok.clone()) {
            Some(Tok::Str(s)) => {
                self.i += 1;
                Ok(s)
            }
            Some(Tok::Word(w))
                if !["and", "or", "not"]
                    .iter()
                    .any(|k| w.eq_ignore_ascii_case(k)) =>
            {
                self.i += 1;
                Ok(w)
            }
            _ => self.fail(Expected::Value),
        }
    }

    fn keyword(&mut self, k: &'static str) -> Result<(), ParseError> {
        if self.kw(k) {
            self.i += 1;
            Ok(())
        } else {
            self.fail(Expected::Keyword(k))
        }
    }

    fn list(&mut self) -> Result<Vec<String>, ParseError> {
        if !matches!(self.peek().map(|t| &t.tok), Some(Tok::LParen)) {
            return self.fail(Expected::ListOpen);
        }
        self.i += 1;
        let mut vs = Vec::new();
        if matches!(self.peek().map(|t| &t.tok), Some(Tok::RParen)) {
            self.i += 1;
            return Ok(vs);
        }
        loop {
            vs.push(self.value()?);
            match self.peek().map(|t| &t.tok) {
                Some(Tok::Comma) => self.i += 1,
                Some(Tok::RParen) => {
                    self.i += 1;
                    return Ok(vs);
                }
                _ => return self.fail(Expected::ListNext),
            }
        }
    }

    fn condition(&mut self) -> Result<Expr, ParseError> {
        let target = self.column()?;
        let op = self.test()?;
        let mut case_sensitive = self.default_cs;
        if self.kw("cs") || self.kw("ci") {
            case_sensitive = self.kw("cs");
            self.i += 1;
        }
        Ok(Expr::Pred(Pred {
            target,
            op: canon(op),
            case_sensitive,
        }))
    }

    /// Enters one level of NOT / parentheses (bounded: deep nesting is
    /// an error, never a stack overflow).
    fn enter(&mut self) -> Result<(), ParseError> {
        self.nest += 1;
        if self.nest > MAX_DEPTH {
            let at = self.peek().map_or(self.text.len(), |t| t.start);
            return Err(err(
                self.text,
                at,
                1,
                format!("too deeply nested (more than {MAX_DEPTH} levels of NOT and parentheses)"),
                None,
            ));
        }
        Ok(())
    }

    /// The operator and its operand(s).
    fn test(&mut self) -> Result<Op, ParseError> {
        let Some(t) = self.peek().cloned() else {
            return self.fail(Expected::Operator);
        };
        if let Tok::Sym(s) = t.tok {
            self.i += 1;
            let v = self.value()?;
            return Ok(match s {
                "=" | "==" => Op::Equals(v),
                "!=" | "<>" => Op::NotEquals(v),
                ">" => Op::Gt(v),
                ">=" => Op::Ge(v),
                "<" => Op::Lt(v),
                _ => Op::Le(v),
            });
        }
        let Tok::Word(w) = &t.tok else {
            return self.fail(Expected::Operator);
        };
        let w = w.to_lowercase();
        self.i += 1;
        Ok(match w.as_str() {
            "is" => {
                let not = self.kw("not");
                if not {
                    self.i += 1;
                }
                self.keyword("blank")?;
                if not { Op::IsNotBlank } else { Op::IsBlank }
            }
            // `not contains` / `not in` are operators of their own; `not`
            // before any other test negates that condition (Op::Not).
            "not" if self.kw("contains") => {
                self.i += 1;
                Op::NotContains(self.value()?)
            }
            "not" if self.kw("in") => {
                self.i += 1;
                Op::NotIn(self.list()?)
            }
            "not" => {
                self.enter()?;
                let op = self.test()?;
                self.nest -= 1;
                Op::Not(Box::new(op))
            }
            "in" => Op::In(self.list()?),
            "between" => {
                let a = self.value()?;
                self.keyword("and")?;
                Op::Between(a, self.value()?)
            }
            "contains" => Op::Contains(self.value()?),
            "startswith" | "beginswith" => Op::BeginsWith(self.value()?),
            "endswith" => Op::EndsWith(self.value()?),
            "regex" | "matches" => Op::Regex(self.value()?),
            _ => {
                self.i -= 1;
                return self.fail(Expected::Operator);
            }
        })
    }
}

/// Canonical form of a condition operator: `Not` of an operator with a
/// natural opposite is that opposite, `Not(Not(x))` is `x` (same rows).
pub fn canon(op: Op) -> Op {
    match op {
        Op::Not(x) => canon(*x).negated(),
        x => x,
    }
}

/// Column names close to `name` (case-insensitive edit distance or
/// substring), best first, at most 3, quoted as the query needs them.
pub fn close_matches(name: &str, columns: &[String]) -> Vec<String> {
    let low = name.to_lowercase();
    let mut scored: Vec<(usize, usize, &String)> = columns
        .iter()
        .enumerate()
        .filter_map(|(i, c)| {
            let cl = c.to_lowercase();
            let d = levenshtein(&low, &cl);
            let max = (low.chars().count().max(cl.chars().count()) / 3).max(2);
            let sub = !low.is_empty() && (cl.contains(&low) || low.contains(&cl));
            (d <= max || sub).then_some((if sub { d.min(1) } else { d }, i, c))
        })
        .collect();
    scored.sort();
    let mut out: Vec<String> = Vec::new();
    for (_, _, c) in scored {
        let q = quote(c, true);
        if !out.contains(&q) {
            out.push(q);
        }
        if out.len() == 3 {
            break;
        }
    }
    out
}

fn levenshtein(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            cur.push(
                (prev[j] + usize::from(ca != *cb))
                    .min(prev[j + 1] + 1)
                    .min(cur[j] + 1),
            );
        }
        prev = cur;
    }
    prev[b.len()]
}

/// Parses a query over `columns` (header names; resolved exactly, then
/// case-insensitively, first match). `Ok(None)` for an empty query.
pub fn parse(text: &str, columns: &[String], default_cs: bool) -> Result<Option<Expr>, ParseError> {
    if text.len() > MAX_LEN {
        return Err(err(
            text,
            0,
            0,
            format!("query too long ({} bytes, at most {MAX_LEN})", text.len()),
            None,
        ));
    }
    let toks = lex(text)?;
    if toks.is_empty() {
        return Ok(None);
    }
    let mut p = Parser {
        text,
        toks,
        i: 0,
        depth: 0,
        nest: 0,
        columns,
        default_cs,
    };
    let e = p.or_expr()?;
    if p.i < p.toks.len() {
        return p.fail(Expected::Connective);
    }
    Ok(Some(normalize(e)))
}

/// Flattens AND in AND and OR in OR and unwraps one-element groups (the
/// shape [`parse`] returns; same rows).
pub fn normalize(e: Expr) -> Expr {
    fn group(xs: Vec<Expr>, and: bool) -> Expr {
        let mut out = Vec::with_capacity(xs.len());
        for x in xs {
            match (normalize(x), and) {
                (Expr::And(ys), true) if !ys.is_empty() => out.extend(ys),
                (Expr::Or(ys), false) if !ys.is_empty() => out.extend(ys),
                (y, _) => out.push(y),
            }
        }
        if out.len() == 1 {
            out.pop().unwrap_or(Expr::And(Vec::new()))
        } else if and {
            Expr::And(out)
        } else {
            Expr::Or(out)
        }
    }
    match e {
        Expr::And(xs) => group(xs, true),
        Expr::Or(xs) => group(xs, false),
        Expr::Not(x) => Expr::Not(Box::new(normalize(*x))),
        Expr::Pred(p) => Expr::Pred(Pred {
            op: canon(p.op),
            ..p
        }),
        x => x,
    }
}

/// `s` as a query token: bare when it reads back as the same word, else
/// double-quoted with `"` doubled.
pub fn quote(s: &str, column: bool) -> String {
    let bare = !s.is_empty()
        && !s.starts_with('\'')
        && !s.chars().any(is_special)
        && !KEYWORDS.iter().any(|k| s.eq_ignore_ascii_case(k))
        && !(column && s == "*");
    if bare {
        s.to_string()
    } else {
        format!("\"{}\"", s.replace('"', "\"\""))
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Ctx {
    Top,
    And,
    Not,
}

/// Prints a tree as query text that [`parse`] reads back as
/// `normalize(expr)`. None when the tree holds row sets ([`Expr::Rows`]),
/// an empty group or an unknown column.
pub fn print(expr: &Expr, columns: &[String], default_cs: bool) -> Option<String> {
    let mut s = String::new();
    print_into(expr, columns, default_cs, Ctx::Top, 0, &mut s)?;
    Some(s)
}

fn print_into(
    e: &Expr,
    cols: &[String],
    dcs: bool,
    ctx: Ctx,
    depth: usize,
    out: &mut String,
) -> Option<()> {
    // Deeper than a query can be: not printable (and no deep recursion).
    if depth > 2 * MAX_DEPTH {
        return None;
    }
    let d = depth + 1;
    match e {
        Expr::Or(xs) | Expr::And(xs) if xs.is_empty() => return None,
        Expr::Or(xs) | Expr::And(xs) if xs.len() == 1 => {
            return print_into(&xs[0], cols, dcs, ctx, d, out);
        }
        Expr::Or(xs) => {
            let paren = ctx != Ctx::Top;
            if paren {
                out.push('(');
            }
            for (i, x) in xs.iter().enumerate() {
                if i > 0 {
                    out.push_str(" OR ");
                }
                print_into(x, cols, dcs, Ctx::Top, d, out)?;
            }
            if paren {
                out.push(')');
            }
        }
        Expr::And(xs) => {
            let paren = ctx == Ctx::Not;
            if paren {
                out.push('(');
            }
            for (i, x) in xs.iter().enumerate() {
                if i > 0 {
                    out.push_str(" AND ");
                }
                print_into(x, cols, dcs, Ctx::And, d, out)?;
            }
            if paren {
                out.push(')');
            }
        }
        Expr::Not(x) => {
            out.push_str("NOT ");
            print_into(x, cols, dcs, Ctx::Not, d, out)?;
        }
        Expr::Pred(p) => out.push_str(&print_pred(p, cols, dcs)?),
        Expr::Rows(_) => return None,
    }
    Some(())
}

/// One condition as query text.
pub fn print_pred(p: &Pred, columns: &[String], default_cs: bool) -> Option<String> {
    let col = match &p.target {
        Target::Any => "*".to_string(),
        Target::Column(c) => quote(columns.get(*c)?, true),
    };
    let op = canon(p.op.clone());
    let case = match (p.case_sensitive, default_cs) {
        (true, false) => " cs",
        (false, true) => " ci",
        _ => "",
    };
    Some(format!("{col} {}{case}", print_test(&op, 0)?))
}

fn print_test(op: &Op, depth: usize) -> Option<String> {
    if depth > MAX_DEPTH {
        return None;
    }
    let v = |s: &str| quote(s, false);
    let list = |vs: &[String]| {
        let q: Vec<String> = vs.iter().map(|s| v(s)).collect();
        format!("({})", q.join(", "))
    };
    Some(match op {
        Op::Not(x) => format!("not {}", print_test(x, depth + 1)?),
        Op::Equals(a) => format!("= {}", v(a)),
        Op::NotEquals(a) => format!("!= {}", v(a)),
        Op::Contains(a) => format!("contains {}", v(a)),
        Op::NotContains(a) => format!("not contains {}", v(a)),
        Op::BeginsWith(a) => format!("startswith {}", v(a)),
        Op::EndsWith(a) => format!("endswith {}", v(a)),
        Op::IsBlank => "is blank".into(),
        Op::IsNotBlank => "is not blank".into(),
        Op::In(vs) => format!("in {}", list(vs)),
        Op::NotIn(vs) => format!("not in {}", list(vs)),
        Op::Regex(a) => format!("regex {}", v(a)),
        Op::Gt(a) => format!("> {}", v(a)),
        Op::Ge(a) => format!(">= {}", v(a)),
        Op::Lt(a) => format!("< {}", v(a)),
        Op::Le(a) => format!("<= {}", v(a)),
        Op::Between(a, b) => format!("between {} and {}", v(a), v(b)),
    })
}

/// Completion candidates at the end of `text`: the byte offset where the
/// word being typed starts (replace from there) and the candidates.
pub fn complete(text: &str, columns: &[String]) -> (usize, Vec<String>) {
    // The word being typed: a bare word, or an unterminated "quoted name.
    let mut from = text.len();
    for (i, c) in text.char_indices().rev() {
        if is_special(c) {
            break;
        }
        from = i;
    }
    let open_quote = {
        let n = text.matches('"').count();
        n % 2 == 1
    };
    if open_quote && let Some(q) = text.rfind('"') {
        from = q;
    }
    let partial = text[from..].trim_start_matches('"').to_lowercase();
    let expected = match parse(&text[..from], columns, false) {
        Ok(Some(_)) => Some(Expected::Connective),
        Ok(None) => Some(Expected::Condition),
        Err(e) if e.at >= from && e.len == 0 => e.expected,
        Err(_) => None,
    };
    let cands: Vec<String> = match expected {
        Some(Expected::Condition) => {
            let mut v: Vec<String> = columns.iter().map(|c| quote(c, true)).collect();
            v.dedup();
            v.push("NOT".into());
            v
        }
        Some(Expected::Operator) => [
            "=",
            "!=",
            "contains",
            "not contains",
            "startswith",
            "endswith",
            "regex",
            "in (",
            "not in (",
            "is blank",
            "is not blank",
            ">",
            ">=",
            "<",
            "<=",
            "between",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect(),
        Some(Expected::Connective) => ["AND", "OR", "cs", "ci"]
            .iter()
            .map(|s| s.to_string())
            .collect(),
        Some(Expected::Keyword(k)) => vec![k.to_uppercase()],
        _ => Vec::new(),
    };
    let items = cands
        .into_iter()
        .filter(|c| {
            let c = c.trim_start_matches('"').to_lowercase();
            c.starts_with(&partial) && (partial.is_empty() || c != partial)
        })
        .collect();
    (from, items)
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn cols() -> Vec<String> {
        [
            "Channel",
            "EventId",
            "Info",
            "User",
            "Event ID",
            "TimeCreated",
            "not",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    }

    fn pred(c: usize, op: Op) -> Expr {
        Expr::Pred(Pred {
            target: Target::Column(c),
            op,
            case_sensitive: false,
        })
    }

    fn p(q: &str) -> Expr {
        parse(q, &cols(), false).unwrap().unwrap()
    }

    fn e(q: &str) -> ParseError {
        parse(q, &cols(), false).unwrap_err()
    }

    #[test]
    fn issue_example() {
        let got = p(
            r#"Channel = "Security" AND (EventId in (4624, 4625) OR Info contains "failed") AND NOT User startswith "SYSTEM""#,
        );
        let want = Expr::And(vec![
            pred(0, Op::Equals("Security".into())),
            Expr::Or(vec![
                pred(1, Op::In(vec!["4624".into(), "4625".into()])),
                pred(2, Op::Contains("failed".into())),
            ]),
            Expr::Not(Box::new(pred(3, Op::BeginsWith("SYSTEM".into())))),
        ]);
        assert_eq!(got, want);
    }

    #[test]
    fn precedence_not_and_or() {
        assert_eq!(
            p("User = a OR User = b AND NOT User = c"),
            Expr::Or(vec![
                pred(3, Op::Equals("a".into())),
                Expr::And(vec![
                    pred(3, Op::Equals("b".into())),
                    Expr::Not(Box::new(pred(3, Op::Equals("c".into())))),
                ]),
            ])
        );
        assert_eq!(
            p("(User = a or User = b) and user = c"),
            Expr::And(vec![
                Expr::Or(vec![
                    pred(3, Op::Equals("a".into())),
                    pred(3, Op::Equals("b".into())),
                ]),
                pred(3, Op::Equals("c".into())),
            ])
        );
    }

    #[test]
    fn operators() {
        let cases: Vec<(&str, Expr)> = vec![
            ("EventId=4624", pred(1, Op::Equals("4624".into()))),
            ("EventId == 4624", pred(1, Op::Equals("4624".into()))),
            ("EventId <> 1", pred(1, Op::NotEquals("1".into()))),
            ("EventId != 1", pred(1, Op::NotEquals("1".into()))),
            ("Info CONTAINS x", pred(2, Op::Contains("x".into()))),
            ("Info not contains x", pred(2, Op::NotContains("x".into()))),
            ("Info startswith x", pred(2, Op::BeginsWith("x".into()))),
            ("Info beginswith x", pred(2, Op::BeginsWith("x".into()))),
            ("Info endswith x", pred(2, Op::EndsWith("x".into()))),
            (r"Info regex '\d+'", pred(2, Op::Regex(r"\d+".into()))),
            ("Info matches a.b", pred(2, Op::Regex("a.b".into()))),
            ("Info in ()", pred(2, Op::In(vec![]))),
            ("Info not in (a)", pred(2, Op::NotIn(vec!["a".into()]))),
            ("Info is blank", pred(2, Op::IsBlank)),
            ("Info IS NOT BLANK", pred(2, Op::IsNotBlank)),
            ("EventId > 1", pred(1, Op::Gt("1".into()))),
            ("EventId >= 1", pred(1, Op::Ge("1".into()))),
            ("EventId < 1", pred(1, Op::Lt("1".into()))),
            ("EventId <= 1", pred(1, Op::Le("1".into()))),
            (
                r#"TimeCreated between 2024-03-01 and "2024-03-02 10:00""#,
                pred(
                    5,
                    Op::Between("2024-03-01".into(), "2024-03-02 10:00".into()),
                ),
            ),
            (
                "Info not startswith x",
                pred(2, Op::Not(Box::new(Op::BeginsWith("x".into())))),
            ),
            (
                "Info not between a and b",
                pred(2, Op::Not(Box::new(Op::Between("a".into(), "b".into())))),
            ),
            (
                "EventId not > 5",
                pred(1, Op::Not(Box::new(Op::Gt("5".into())))),
            ),
            // NOT of an operator with an opposite is that opposite.
            ("Info not = x", pred(2, Op::NotEquals("x".into()))),
            ("Info not is blank", pred(2, Op::IsNotBlank)),
            ("Info not not in (a)", pred(2, Op::In(vec!["a".into()]))),
            ("Info not not regex a", pred(2, Op::Regex("a".into()))),
            (
                "NOT Info regex a",
                Expr::Not(Box::new(pred(2, Op::Regex("a".into())))),
            ),
            (r#""Event ID" = 1"#, pred(4, Op::Equals("1".into()))),
            ("\"not\" = 1", pred(6, Op::Equals("1".into()))),
            ("eventid = 1", pred(1, Op::Equals("1".into()))),
            (
                r#"Info = "say ""hi""""#,
                pred(2, Op::Equals("say \"hi\"".into())),
            ),
            ("Info = O'Brien", pred(2, Op::Equals("O'Brien".into()))),
            (
                r"Info = C:\Windows\x.exe",
                pred(2, Op::Equals(r"C:\Windows\x.exe".into())),
            ),
            (
                "* contains mimikatz",
                Expr::Pred(Pred {
                    target: Target::Any,
                    op: Op::Contains("mimikatz".into()),
                    case_sensitive: false,
                }),
            ),
        ];
        for (q, want) in cases {
            assert_eq!(p(q), want, "{q}");
        }
    }

    #[test]
    fn case_modifiers_and_default() {
        let cs = |c: bool| {
            Expr::Pred(Pred {
                target: Target::Column(3),
                op: Op::Equals("a".into()),
                case_sensitive: c,
            })
        };
        assert_eq!(p("User = a cs"), cs(true));
        assert_eq!(p("User = a ci"), cs(false));
        assert_eq!(parse("User = a", &cols(), true).unwrap(), Some(cs(true)));
        assert_eq!(
            parse("User = a CI", &cols(), true).unwrap(),
            Some(cs(false))
        );
        // `cs` as the value itself.
        assert_eq!(
            p("User = cs cs"),
            Expr::Pred(Pred {
                target: Target::Column(3),
                op: Op::Equals("cs".into()),
                case_sensitive: true,
            })
        );
    }

    #[test]
    fn errors_have_position_and_expected_token() {
        let x = e("User = a AND");
        assert_eq!(
            (x.at, x.col, x.expected),
            (12, 12, Some(Expected::Condition))
        );
        assert!(x.msg.contains("found the end of the query"), "{}", x.msg);
        let x = e("User foo a");
        assert_eq!((x.at, x.expected), (5, Some(Expected::Operator)));
        assert!(x.msg.contains("found \"foo\""));
        let x = e("(User = a");
        assert_eq!(x.expected, Some(Expected::Connective));
        assert!(x.msg.contains("AND, OR or )"));
        let x = e("User = a User = b");
        assert_eq!((x.at, x.expected), (9, Some(Expected::Connective)));
        let x = e("User = AND x");
        assert_eq!(x.expected, Some(Expected::Value));
        let x = e("Info in (a b)");
        assert_eq!(x.expected, Some(Expected::ListNext));
        let x = e("Info between a or b");
        assert_eq!(x.expected, Some(Expected::Keyword("and")));
        let x = e("Info is nothing");
        assert_eq!(x.expected, Some(Expected::Keyword("blank")));
        let x = e("User = \"abc");
        assert_eq!(x.at, 7);
        assert!(x.msg.contains("unterminated"));
        let x = e("User ! a");
        assert_eq!(x.at, 5);
        // Character position, not byte offset.
        let x = e("Info = \"é\" ??");
        assert_eq!((x.at, x.col), (12, 11));
        assert!(e(")").msg.contains("expected a column name"));
    }

    #[test]
    fn unknown_column_lists_close_matches() {
        let x = e("Evnt ID = 1");
        assert!(x.msg.contains("unknown column \"Evnt\""), "{}", x.msg);
        let x = e("\"Evnt ID\" = 1");
        assert!(x.msg.contains("did you mean \"Event ID\""), "{}", x.msg);
        let x = e("Chanel = 1");
        assert!(x.msg.contains("did you mean Channel"), "{}", x.msg);
        let x = e("zzzzzz = 1");
        assert!(x.msg.contains("(columns: Channel, EventId"), "{}", x.msg);
        assert_eq!((x.at, x.len), (0, 6));
    }

    #[test]
    fn deep_nesting_is_an_error_not_a_crash() {
        let n = 100_000;
        for q in [
            format!("{}User = a", "NOT ".repeat(n)),
            format!("{}User = a{}", "(".repeat(n), ")".repeat(n)),
            format!("{}User = a", "(NOT ".repeat(n)),
            format!("User {}= a", "not ".repeat(n)),
        ] {
            let x = parse(&q, &cols(), false).unwrap_err();
            assert!(
                x.msg.contains("too deeply nested") || x.msg.contains("too long"),
                "{}",
                x.msg
            );
        }
        let x = parse(
            &format!("{}User = a", "NOT ".repeat(MAX_DEPTH + 1)),
            &cols(),
            false,
        )
        .unwrap_err();
        assert!(x.msg.contains("too deeply nested"));
        assert_eq!(x.at, 4 * MAX_DEPTH);
        let q = format!("{}User = a{}", "(".repeat(MAX_DEPTH), ")".repeat(MAX_DEPTH));
        assert!(parse(&q, &cols(), false).is_ok());
        let ok = format!("{}User = a", "NOT ".repeat(MAX_DEPTH));
        let e = parse(&ok, &cols(), false).unwrap().unwrap();
        assert_eq!(print(&e, &cols(), false), Some(ok));
        // A deeper tree built by hand is not printed (bounded recursion).
        let mut deep = pred(0, Op::IsBlank);
        for _ in 0..10 * MAX_DEPTH {
            deep = Expr::Not(Box::new(deep));
        }
        assert!(print(&deep, &cols(), false).is_none());
        let long = format!("User = \"{}\"", "x".repeat(MAX_LEN));
        assert!(
            parse(&long, &cols(), false)
                .unwrap_err()
                .msg
                .contains("too long")
        );
    }

    #[test]
    fn empty_query() {
        assert_eq!(parse("   ", &cols(), false).unwrap(), None);
    }

    #[test]
    fn printer_quotes_what_needs_it() {
        let q = print(
            &Expr::And(vec![
                pred(4, Op::Equals("a b".into())),
                pred(
                    6,
                    Op::In(vec!["".into(), "x,y".into(), "and".into(), "q\"".into()]),
                ),
                Expr::Not(Box::new(Expr::Or(vec![
                    pred(0, Op::IsBlank),
                    pred(5, Op::Between("2024-03-01".into(), "2024-03-02".into())),
                ]))),
            ]),
            &cols(),
            false,
        )
        .unwrap();
        assert_eq!(
            q,
            r#""Event ID" = "a b" AND "not" in ("", "x,y", "and", "q""") AND NOT (Channel is blank OR TimeCreated between 2024-03-01 and 2024-03-02)"#
        );
        assert!(print(&Expr::And(vec![]), &cols(), false).is_none());
        assert!(print(&pred(99, Op::IsBlank), &cols(), false).is_none());
    }

    #[test]
    fn completion() {
        let c = cols();
        assert_eq!(complete("Ch", &c), (0, vec!["Channel".to_string()]));
        let (f, v) = complete("Channel = x AND Ev", &c);
        assert_eq!(f, 16);
        assert_eq!(v, vec!["EventId".to_string(), "\"Event ID\"".to_string()]);
        let (_, v) = complete("Channel st", &c);
        assert_eq!(v, vec!["startswith".to_string()]);
        let (_, v) = complete("Channel = x ", &c);
        assert_eq!(v, vec!["AND", "OR", "cs", "ci"]);
        let (f, v) = complete("Channel = x AND \"Event", &c);
        assert_eq!(f, 16);
        assert_eq!(v, vec!["EventId".to_string(), "\"Event ID\"".to_string()]);
        assert!(complete("Channel = ", &c).1.is_empty());
        assert_eq!(complete("Info between 1 a", &c).1, vec!["AND"]);
    }

    // ---- round trip ------------------------------------------------------

    fn arb_text() -> impl Strategy<Value = String> {
        prop_oneof![
            "[a-zA-Z0-9_.:/-]{0,8}",
            "[ a-z\"',()=!<>é\\\\]{0,8}",
            Just("and".to_string()),
            Just("NOT".to_string()),
            Just("*".to_string()),
            Just("cs".to_string()),
            Just("'x".to_string()),
        ]
    }

    fn arb_op() -> impl Strategy<Value = Op> {
        let t = arb_text;
        prop_oneof![
            t().prop_map(Op::Equals),
            t().prop_map(Op::NotEquals),
            t().prop_map(Op::Contains),
            t().prop_map(Op::NotContains),
            t().prop_map(Op::BeginsWith),
            t().prop_map(Op::EndsWith),
            Just(Op::IsBlank),
            Just(Op::IsNotBlank),
            prop::collection::vec(t(), 0..4).prop_map(Op::In),
            prop::collection::vec(t(), 0..4).prop_map(Op::NotIn),
            t().prop_map(Op::Regex),
            t().prop_map(Op::Gt),
            t().prop_map(Op::Ge),
            t().prop_map(Op::Lt),
            t().prop_map(Op::Le),
            (t(), t()).prop_map(|(a, b)| Op::Between(a, b)),
        ]
        .prop_recursive(2, 4, 1, |inner| inner.prop_map(|o| Op::Not(Box::new(o))))
    }

    fn arb_expr(ncols: usize) -> impl Strategy<Value = Expr> {
        let leaf = (
            prop_oneof![
                4 => (0..ncols).prop_map(Target::Column),
                1 => Just(Target::Any)
            ],
            arb_op(),
            any::<bool>(),
        )
            .prop_map(|(target, op, case_sensitive)| {
                Expr::Pred(Pred {
                    target,
                    op,
                    case_sensitive,
                })
            });
        leaf.prop_recursive(4, 24, 4, |inner| {
            prop_oneof![
                prop::collection::vec(inner.clone(), 1..4).prop_map(Expr::And),
                prop::collection::vec(inner.clone(), 1..4).prop_map(Expr::Or),
                inner.prop_map(|x| Expr::Not(Box::new(x))),
            ]
        })
    }

    fn arb_cols() -> impl Strategy<Value = Vec<String>> {
        // Distinct names, including ones that need quoting.
        prop::collection::btree_set(
            prop_oneof![
                "[A-Za-z][A-Za-z0-9_]{0,6}",
                "[A-Z][a-z]{0,3} [A-Z][a-z]{0,3}",
                Just("not".to_string()),
                Just("*".to_string()),
                Just("a\"b".to_string()),
                Just("".to_string()),
            ],
            1..6,
        )
        .prop_map(|s| s.into_iter().collect::<Vec<_>>())
        .prop_filter("case-insensitively distinct", |v: &Vec<String>| {
            let mut l: Vec<String> = v.iter().map(|s| s.to_lowercase()).collect();
            l.sort();
            l.dedup();
            l.len() == v.len()
        })
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(512))]

        #[test]
        fn parse_print_round_trip(
            (cols, tree) in arb_cols().prop_flat_map(|c| {
                let n = c.len();
                (Just(c), arb_expr(n))
            }),
            dcs in any::<bool>(),
        ) {
            let text = print(&tree, &cols, dcs).expect("printable");
            let back = parse(&text, &cols, dcs)
                .unwrap_or_else(|e| panic!("{text}: {e}"))
                .expect("not empty");
            let norm = normalize(tree);
            prop_assert_eq!(&back, &norm, "{}", text);
            // A normalized tree reads back exactly, and printing is stable.
            prop_assert_eq!(print(&back, &cols, dcs), Some(text));
        }

        #[test]
        fn parse_never_panics(s in "[ a-zA-Z()=!<>,\"'*]{0,40}") {
            let _ = parse(&s, &cols(), false);
            let _ = complete(&s, &cols());
        }
    }
}
