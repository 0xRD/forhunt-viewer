//! Query bar (D-045): the column filters as query text, and query text
//! applied back to them. Applying a query replaces the column filters: each
//! top-level AND term that one column's filter can hold (a condition, an OR
//! of conditions on one column, an exact `in` list) becomes its chip; the
//! rest (cross-column OR, NOT of a group...) is kept as one residual tree,
//! shown as a "query" chip. Editing chips regenerates the text.

use eframe::egui::{self, Color32, Key, RichText, Ui};
use fhv_engine::columns::Kind;
use fhv_engine::filter::{Expr, Op, Pred, Target};
use fhv_engine::query::{self, ParseError};

use crate::settings::Settings;
use crate::tab::{ColFilter, Tab};

const ERR: Color32 = Color32::from_rgb(230, 90, 80);
const HISTORY_MAX: usize = 30;

#[derive(Default)]
pub struct QueryState {
    pub text: String,
    /// Edited since it was last applied or regenerated.
    pub dirty: bool,
    /// The filters changed: regenerate the text.
    pub stale: bool,
    pub error: Option<QueryError>,
    /// Terms of the applied query that the column filters cannot hold,
    /// operands as written (display zone).
    pub residual: Option<Expr>,
    pub residual_text: String,
    /// Put the keyboard focus in the bar (shortcut).
    pub focus: bool,
    suggest: (usize, Vec<String>),
    suggest_for: Option<String>,
}

#[derive(Clone, Debug)]
pub enum QueryError {
    Parse(ParseError),
    /// Parsed, but a condition is invalid (bad regex, not a date...).
    Invalid(String),
}

impl std::fmt::Display for QueryError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            QueryError::Parse(e) => e.fmt(f),
            QueryError::Invalid(m) => f.write_str(m),
        }
    }
}

/// The current column filters and residual query as query text.
pub fn state_text(t: &Tab, default_cs: bool) -> String {
    let mut terms = t.column_terms(false);
    terms.extend(t.query.residual.clone());
    if terms.is_empty() {
        return String::new();
    }
    query::print(&Expr::And(terms), &t.table.columns, default_cs).unwrap_or_default()
}

/// Time operands of `e` rewritten in UTC, as the condition editor's are.
pub fn to_utc(t: &Tab, e: Expr) -> Expr {
    match e {
        Expr::And(xs) => Expr::And(xs.into_iter().map(|x| to_utc(t, x)).collect()),
        Expr::Or(xs) => Expr::Or(xs.into_iter().map(|x| to_utc(t, x)).collect()),
        Expr::Not(x) => Expr::Not(Box::new(to_utc(t, *x))),
        Expr::Pred(mut p) => {
            if let Target::Column(c) = p.target
                && let Kind::Time(o) = t.kind(c)
            {
                p.op = t.tz_display().op_to_utc(&p.op, o);
            }
            Expr::Pred(p)
        }
        x => x,
    }
}

/// Parses, validates and applies `text`; on error nothing changes.
pub fn apply(t: &mut Tab, text: &str, default_cs: bool) -> Result<(), QueryError> {
    let e = query::parse(text, &t.table.columns, default_cs).map_err(QueryError::Parse)?;
    if let Some(e) = &e {
        t.engine
            .validate(&to_utc(t, e.clone()))
            .map_err(|x| QueryError::Invalid(x.to_string()))?;
    }
    let (filters, residual) = decompose(e, &t.filters);
    t.filters = filters;
    set_residual(t, residual);
    t.requery();
    Ok(())
}

pub fn set_residual(t: &mut Tab, residual: Option<Expr>) {
    t.query.residual_text = residual
        .as_ref()
        .and_then(|r| query::print(r, &t.table.columns, false))
        .unwrap_or_default();
    t.query.residual = residual;
}

/// Splits a parsed query into column filters and the residual tree.
/// Quick filters of `old` that the query still holds stay quick filters.
pub fn decompose(e: Option<Expr>, old: &[ColFilter]) -> (Vec<ColFilter>, Option<Expr>) {
    let mut fs = vec![ColFilter::default(); old.len()];
    let conj = match e {
        None => Vec::new(),
        Some(Expr::And(xs)) => xs,
        Some(x) => vec![x],
    };
    let mut conj: Vec<Option<Expr>> = conj.into_iter().map(Some).collect();
    for (c, o) in old.iter().enumerate() {
        if o.quick.is_empty() {
            continue;
        }
        let q = Op::Contains(o.quick.clone());
        let want = if o.quick_not { q.negated() } else { q };
        let same = |x: &Option<Expr>| {
            matches!(x, Some(Expr::Pred(Pred {
                target: Target::Column(k),
                op,
                case_sensitive,
            })) if *k == c && *op == want && *case_sensitive == o.case_sensitive)
        };
        if let Some(slot) = conj.iter_mut().find(|x| same(x)) {
            *slot = None;
            fs[c].quick = o.quick.clone();
            fs[c].quick_not = o.quick_not;
            fs[c].case_sensitive = o.case_sensitive;
        }
    }
    let mut rest: Vec<Expr> = conj
        .into_iter()
        .flatten()
        .filter(|x| !place(&mut fs, x))
        .collect();
    let residual = match rest.len() {
        0 => None,
        1 => rest.pop(),
        _ => Some(Expr::And(rest)),
    };
    (fs, residual)
}

fn col_pred(x: &Expr) -> Option<(usize, Op, bool)> {
    match x {
        Expr::Pred(Pred {
            target: Target::Column(c),
            op,
            case_sensitive,
        }) => Some((*c, op.clone(), *case_sensitive)),
        _ => None,
    }
}

/// Puts one AND term into its column's filter, if that filter can hold it.
fn place(fs: &mut [ColFilter], x: &Expr) -> bool {
    if let Some((c, op, cs)) = col_pred(x) {
        let Some(f) = fs.get_mut(c) else {
            return false;
        };
        // An exact list is the value-list selection (D-035).
        if cs && matches!(op, Op::In(_) | Op::NotIn(_)) && f.values.is_none() {
            f.values = Some(op);
            return true;
        }
        let used = !f.quick.is_empty() || !f.conds.is_empty();
        if (f.conds.is_empty() || !f.any) && (!used || f.case_sensitive == cs) {
            f.case_sensitive = cs;
            f.any = false;
            f.conds.push(op);
            return true;
        }
        return false;
    }
    if let Expr::Or(xs) = x
        && let Some(ps) = xs.iter().map(col_pred).collect::<Option<Vec<_>>>()
        && ps.len() >= 2
    {
        let (c, cs) = (ps[0].0, ps[0].2);
        if ps.iter().all(|p| p.0 == c && p.2 == cs)
            && let Some(f) = fs.get_mut(c)
            && f.conds.is_empty()
            && (f.quick.is_empty() || f.case_sensitive == cs)
        {
            f.case_sensitive = cs;
            f.any = true;
            f.conds = ps.into_iter().map(|p| p.1).collect();
            return true;
        }
    }
    false
}

fn submit(t: &mut Tab, settings: &mut Settings) {
    let text = t.query.text.clone();
    match apply(t, &text, settings.query_case_sensitive) {
        Ok(()) => {
            t.query.error = None;
            t.query.dirty = false;
            let q = text.trim().to_string();
            if !q.is_empty() {
                settings.query_history.retain(|h| *h != q);
                settings.query_history.insert(0, q);
                settings.query_history.truncate(HISTORY_MAX);
                settings.save();
            }
        }
        Err(e) => t.query.error = Some(e),
    }
}

fn accept(ui: &Ui, t: &mut Tab, id: egui::Id, item: &str) {
    let from = t.query.suggest.0.min(t.query.text.len());
    t.query.text.truncate(from);
    t.query.text.push_str(item);
    if !item.ends_with('(') {
        t.query.text.push(' ');
    }
    t.query.dirty = true;
    t.query.error = None;
    if let Some(mut st) = egui::TextEdit::load_state(ui.ctx(), id) {
        let n = t.query.text.chars().count();
        st.cursor.set_char_range(Some(egui::text::CCursorRange::one(
            egui::text::CCursor::new(n),
        )));
        st.store(ui.ctx(), id);
    }
    ui.memory_mut(|m| m.request_focus(id));
}

/// The query bar row (and its error / suggestions below it).
pub fn ui(ui: &mut Ui, t: &mut Tab, settings: &mut Settings) {
    let id = egui::Id::unique(("query bar", t.path.clone()));
    let had_focus = ui.memory(|m| m.has_focus(id));
    if t.query.stale && !(had_focus && t.query.dirty) {
        t.query.text = state_text(t, settings.query_case_sensitive);
        t.query.dirty = false;
        t.query.error = None;
        t.query.stale = false;
    }
    if had_focus && t.query.suggest_for.as_deref() != Some(t.query.text.as_str()) {
        t.query.suggest = query::complete(&t.query.text, &t.table.columns);
        t.query.suggest_for = Some(t.query.text.clone());
    }
    let tab_key = had_focus
        && !t.query.suggest.1.is_empty()
        && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Tab));
    if tab_key {
        let first = t.query.suggest.1[0].clone();
        accept(ui, t, id, &first);
    }
    ui.horizontal(|ui| {
        let lbl = ui.label("Query");
        if ui
            .toggle_value(&mut settings.query_case_sensitive, "Aa")
            .on_hover_text(
                "match case in conditions without cs / ci (end a condition with cs or ci \
                 to choose for that condition)",
            )
            .changed()
        {
            t.query.stale = true;
            settings.save();
        }
        let w = (ui.available_width() - 150.0).max(200.0);
        let r = ui
            .add(
                egui::TextEdit::singleline(&mut t.query.text)
                    .id(id)
                    // Tab completes while there are suggestions.
                    .event_filter(egui::EventFilter {
                        horizontal_arrows: true,
                        vertical_arrows: true,
                        tab: !t.query.suggest.1.is_empty(),
                        ..Default::default()
                    })
                    .font(egui::TextStyle::Monospace)
                    .hint_text(
                        "Channel = Security AND (EventID in (4624, 4625) OR NOT User \
                         startswith SYSTEM)   (Ctrl+L)",
                    )
                    .desired_width(w),
            )
            .labelled_by(lbl.id);
        if std::mem::take(&mut t.query.focus) {
            r.request_focus();
        }
        if r.changed() {
            t.query.dirty = true;
            t.query.error = None;
        }
        let enter = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
        let esc = r.lost_focus() && ui.input(|i| i.key_pressed(Key::Escape));
        if esc {
            // Back to the filters in force.
            t.query.dirty = false;
            t.query.stale = true;
        }
        if ui
            .button("Apply")
            .on_hover_text("apply the query (Enter); Esc goes back to the filters in force")
            .clicked()
            || enter
        {
            submit(t, settings);
        }
        ui.menu_button("History", |ui| {
            if settings.query_history.is_empty() {
                ui.label(RichText::new("no query applied yet").weak());
            }
            for q in settings.query_history.clone() {
                if ui.button(RichText::new(&q).monospace()).clicked() {
                    t.query.text = q;
                    submit(t, settings);
                    ui.close();
                }
            }
        });
    });
    if let Some(e) = &t.query.error {
        let msg = format!("{e}. The filters are unchanged.");
        match e {
            QueryError::Parse(p) => {
                let n = t
                    .query
                    .text
                    .get(p.at..p.at + p.len)
                    .map_or(1, |s| s.chars().count().max(1));
                let caret = format!("{}{}", " ".repeat(p.col + 6), "^".repeat(n));
                ui.label(
                    RichText::new(format!("Query {}\n{caret}", t.query.text))
                        .monospace()
                        .color(ERR),
                );
            }
            QueryError::Invalid(_) => {}
        }
        ui.label(RichText::new(msg).color(ERR));
    }
    if had_focus && !t.query.suggest.1.is_empty() {
        let mut pick = None;
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("Tab:").weak());
            let items = &t.query.suggest.1;
            for s in items.iter().take(10) {
                if ui.small_button(RichText::new(s).monospace()).clicked() {
                    pick = Some(s.clone());
                }
            }
            if items.len() > 10 {
                ui.label(RichText::new(format!("+{} more", items.len() - 10)).weak());
            }
        });
        if let Some(s) = pick {
            accept(ui, t, id, &s);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f(conds: Vec<Op>, any: bool, cs: bool) -> ColFilter {
        ColFilter {
            conds,
            any,
            case_sensitive: cs,
            ..Default::default()
        }
    }

    fn cols() -> Vec<String> {
        ["Time", "EventId", "User", "Message"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    }

    fn split(q: &str, old: &[ColFilter]) -> (Vec<ColFilter>, Option<Expr>) {
        decompose(query::parse(q, &cols(), false).unwrap(), old)
    }

    #[test]
    fn column_terms_become_chips() {
        let old = vec![ColFilter::default(); 4];
        let (fs, rest) = split(
            "EventId in (4624, 4625) AND (User = a OR User = b) AND Message contains x \
             AND Message regex y AND User in (SYSTEM) cs",
            &old,
        );
        assert!(rest.is_none());
        assert_eq!(fs[0], ColFilter::default());
        assert_eq!(
            fs[1],
            f(
                vec![Op::In(vec!["4624".into(), "4625".into()])],
                false,
                false
            )
        );
        assert_eq!(
            fs[2],
            ColFilter {
                values: Some(Op::In(vec!["SYSTEM".into()])),
                ..f(
                    vec![Op::Equals("a".into()), Op::Equals("b".into())],
                    true,
                    false
                )
            }
        );
        assert_eq!(
            fs[3],
            f(
                vec![Op::Contains("x".into()), Op::Regex("y".into())],
                false,
                false
            )
        );
    }

    #[test]
    fn other_terms_stay_a_query() {
        let old = vec![ColFilter::default(); 4];
        let (fs, rest) = split(
            "User = a AND (User = b OR EventId = 1) AND NOT Message contains x \
             AND User = c cs",
            &old,
        );
        assert_eq!(fs[2], f(vec![Op::Equals("a".into())], false, false));
        let printed = query::print(&rest.unwrap(), &cols(), false).unwrap();
        assert_eq!(
            printed,
            "(User = b OR EventId = 1) AND NOT Message contains x AND User = c cs"
        );
    }

    #[test]
    fn quick_filters_are_kept() {
        let mut old = vec![ColFilter::default(); 4];
        old[2].quick = "jd".into();
        let (fs, rest) = split("User contains jd AND User contains x", &old);
        assert!(rest.is_none());
        assert_eq!(fs[2].quick, "jd");
        assert_eq!(fs[2].conds, vec![Op::Contains("x".into())]);
        // An inverted quick filter (chip click) stays one too.
        old[2].quick_not = true;
        let (fs, rest) = split("User not contains jd AND User not regex y", &old);
        assert!(rest.is_none());
        assert!(fs[2].quick_not);
        assert_eq!(fs[2].conds, vec![Op::Not(Box::new(Op::Regex("y".into())))]);
    }
}
