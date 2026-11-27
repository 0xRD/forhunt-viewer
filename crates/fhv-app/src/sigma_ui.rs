//! Sigma detection UI (D-102): the toolbar control that runs detection, the
//! detections panel (overview by level and rule, click to filter), the
//! rules of a row in the detail pane, and the Detection rules section of
//! Settings.

use eframe::egui::{self, Color32, RichText, Ui};

use crate::sigma::{Focus, LEVELS, Library, SigmaSettings, level_color};
use crate::tab::Tab;
use std::collections::HashMap;

/// What the toolbar or panel asks the app to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Run,
}

fn level_fg(ui: &Ui, level: u8) -> Color32 {
    let th = crate::theme::current(ui.ctx());
    crate::rules::palette_fg(&th, level_color(level))
}

fn level_bg(ui: &Ui, level: u8) -> Color32 {
    let th = crate::theme::current(ui.ctx());
    crate::rules::palette_bg(&th, level_color(level))
}

/// A small level badge: "HIGH" in the level's colors.
fn badge(ui: &mut Ui, level: u8) {
    let short = match level {
        1 => "INFO",
        2 => "LOW",
        3 => "MED",
        4 => "HIGH",
        5 => "CRIT",
        _ => "",
    };
    ui.label(
        RichText::new(format!(" {short} "))
            .small()
            .strong()
            .color(level_fg(ui, level))
            .background_color(level_bg(ui, level)),
    );
}

/// Why detection cannot run now, if it cannot.
pub fn blocked(t: &Tab, lib: &Library) -> Option<&'static str> {
    if !t.ready() {
        Some("the file is still being indexed")
    } else if lib.loading() {
        Some("the rules are loading")
    } else if lib.download.is_some() {
        Some("the rules are downloading")
    } else if lib.ready().is_none() {
        Some(
            "no detection rules yet: download them or add a rules folder in File > Settings > Detection rules",
        )
    } else {
        None
    }
}

/// Toolbar control: *Detect* (run), progress while running, then the
/// detection count that shows / hides the panel, and *Re-run* when the
/// rules changed since.
pub fn toolbar(ui: &mut Ui, t: &mut Tab, lib: &Library) -> Option<Action> {
    let mut act = None;
    if let Some((text, frac)) = t.sigma.progress() {
        ui.add(
            egui::ProgressBar::new(frac)
                .desired_width(90.0)
                .show_percentage(),
        )
        .on_hover_text(text);
        if ui
            .small_button("×")
            .on_hover_text("stop detection (Esc)")
            .clicked()
        {
            t.sigma.cancel();
        }
        return None;
    }
    let why = blocked(t, lib);
    match &t.sigma.result {
        None => {
            // Without usable rules the button stays enabled: Run opens the
            // rules settings (App::run_sigma).
            let can = t.ready() && !lib.loading() && lib.download.is_none();
            let tip = match &lib.set {
                _ if lib.ready().is_some() => {
                    "Run the Sigma detection rules over the whole file (Ctrl+Shift+D). \
                     Matched rows get a level mark in the row gutter and are listed in the \
                     Detections panel."
                }
                Some(_) => {
                    "No rule loaded with these settings (minimum level, folders): click to open them"
                }
                None => {
                    "No detection rules yet: click to download them or add a rules folder (Settings)"
                }
            };
            let r = ui
                .add_enabled(can, egui::Button::new("Detect"))
                .on_hover_text(tip);
            let r = match why {
                Some(w) => r.on_disabled_hover_text(w),
                None => r,
            };
            if r.clicked() {
                act = Some(Action::Run);
            }
        }
        Some(res) => {
            let n = res.all.len();
            let top = (1..=5u8)
                .rev()
                .find(|&l| !res.level[l as usize].is_empty())
                .unwrap_or(0);
            let label = if n == 0 {
                RichText::new("no detection")
            } else {
                RichText::new(format!("{} detections", crate::app::fmt_n(n)))
                    .color(level_fg(ui, top))
            };
            if ui
                .selectable_label(t.sigma.panel, label)
                .on_hover_text("show / hide the Detections panel")
                .clicked()
            {
                t.sigma.panel = !t.sigma.panel;
            }
            let stale = t.sigma.stale(lib.generation);
            let rerun = if stale {
                egui::Button::new(RichText::new("Re-run").color(ui.visuals().warn_fg_color))
            } else {
                egui::Button::new("Run again")
            };
            let r = ui
                .add_enabled(why.is_none(), rerun)
                .on_hover_text(if stale {
                    "The rules changed since this run (Settings): run again"
                } else {
                    "run the detection again (Ctrl+Shift+D)"
                });
            if r.clicked() {
                act = Some(Action::Run);
            }
        }
    }
    if let Some(e) = &t.sigma.error {
        ui.colored_label(ui.visuals().error_fg_color, "detection failed")
            .on_hover_text(e);
    }
    act
}

/// The Detections panel. Returns an action and whether the filter changed.
pub fn panel(ui: &mut Ui, t: &mut Tab, lib: &Library) -> (Option<Action>, bool) {
    let mut act = None;
    let mut changed = false;
    ui.horizontal(|ui| {
        ui.heading("Detections");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui
                .small_button("×")
                .on_hover_text("close the panel")
                .clicked()
            {
                t.sigma.panel = false;
            }
        });
    });
    let Some(res) = t.sigma.result.clone() else {
        if let Some((text, frac)) = t.sigma.progress() {
            ui.add(egui::ProgressBar::new(frac).text("running"));
            ui.label(RichText::new(text).small().weak());
        } else {
            ui.label("Run the detection rules over this file with Detect in the toolbar.");
        }
        return (act, changed);
    };
    let rules = res.set.rules();
    ui.label(
        RichText::new(format!(
            "{} rows matched by {} of {} rules; {} rows read in {:.1} s",
            crate::app::fmt_n(res.all.len()),
            crate::app::fmt_n(res.hits.len() as u64),
            crate::app::fmt_n(rules.len() as u64),
            crate::app::fmt_n(res.read),
            res.ms / 1e3
        ))
        .small()
        .weak(),
    );
    if t.sigma.stale(lib.generation) {
        ui.horizontal(|ui| {
            ui.colored_label(
                ui.visuals().warn_fg_color,
                "The rules changed since this run.",
            );
            if ui.small_button("Re-run").clicked() {
                act = Some(Action::Run);
            }
        });
    }
    if let Some(n) = &res.note {
        ui.label(RichText::new(n).small().color(ui.visuals().warn_fg_color));
    }
    ui.separator();
    // Levels: click to keep the rows at or above a level.
    let focus = t.sigma.focus.clone();
    let mut set_focus: Option<Option<Focus>> = None;
    ui.horizontal_wrapped(|ui| {
        let all = Focus::AtLeast(1);
        if ui
            .selectable_label(
                focus.as_ref() == Some(&all),
                format!("All ({})", crate::app::fmt_n(res.all.len())),
            )
            .on_hover_text("only the rows some rule matched")
            .clicked()
        {
            set_focus = Some(if focus.as_ref() == Some(&all) {
                None
            } else {
                Some(all)
            });
        }
        for l in (2..=5u8).rev() {
            let n = res.count_at_least(l);
            if n == 0 {
                continue;
            }
            let f = Focus::AtLeast(l);
            let name = if l == 5 {
                "critical".to_string()
            } else {
                format!("{}+", LEVELS[l as usize])
            };
            let text =
                RichText::new(format!("{name} ({})", crate::app::fmt_n(n))).color(level_fg(ui, l));
            if ui
                .selectable_label(focus.as_ref() == Some(&f), text)
                .on_hover_text(format!(
                    "only the rows a {} or more severe rule matched",
                    LEVELS[l as usize]
                ))
                .clicked()
            {
                set_focus = Some(if focus.as_ref() == Some(&f) {
                    None
                } else {
                    Some(f)
                });
            }
        }
    });
    ui.add(
        egui::TextEdit::singleline(&mut t.sigma.search)
            .hint_text("find a rule, tag, author…")
            .desired_width(f32::INFINITY),
    );
    let needle = t.sigma.search.to_lowercase();
    // One row per level header or rule: only the visible rows are laid out
    // (thousands of rules can match a large file).
    enum Item {
        Level(u8),
        Rule(usize),
        Credit,
    }
    let mut items = Vec::new();
    let mut last_level = 0;
    for (i, (r, _)) in res.hits.iter().enumerate() {
        let rule = &rules[*r as usize];
        if !needle.is_empty()
            && !rule.title.to_lowercase().contains(&needle)
            && !rule.author.to_lowercase().contains(&needle)
            && !rule.tags.iter().any(|x| x.to_lowercase().contains(&needle))
            && !rule.id.to_lowercase().contains(&needle)
        {
            continue;
        }
        if rule.level != last_level {
            last_level = rule.level;
            items.push(Item::Level(rule.level));
        }
        items.push(Item::Rule(i));
    }
    items.push(Item::Credit);
    // Titles shared by several hit rules (hayabusa-rules converts one Sigma
    // rule per log: Sysmon 1 and Security 4688): those lines name their log.
    let mut seen: HashMap<&str, u32> = HashMap::new();
    for (r, _) in &res.hits {
        *seen.entry(rules[*r as usize].title.as_str()).or_default() += 1;
    }
    let row_h = ui.spacing().interact_size.y;
    egui::ScrollArea::vertical()
        .id_salt("sigma_rules")
        .auto_shrink([false, false])
        .show_rows(ui, row_h, items.len(), |ui, range| {
            for item in &items[range] {
                match item {
                    Item::Level(l) => {
                        ui.add_sized(
                            [ui.available_width(), row_h],
                            egui::Label::new(
                                RichText::new(LEVELS[*l as usize].to_uppercase())
                                    .small()
                                    .strong()
                                    .color(level_fg(ui, *l)),
                            ),
                        );
                    }
                    Item::Rule(i) => {
                        let (r, rows) = &res.hits[*i];
                        let rule = &rules[*r as usize];
                        let f = Focus::Rule(*r);
                        let on = focus.as_ref() == Some(&f);
                        let title = match source_hint(rule) {
                            Some(h) if seen.get(rule.title.as_str()).is_some_and(|&n| n > 1) => {
                                format!("{h} · {}", rule.title)
                            }
                            _ => rule.title.clone(),
                        };
                        let label = match res.bursts.get(r) {
                            // A correlation: its bursts, and the rows in them.
                            Some(b) => format!(
                                "{} ({} × {} rows)",
                                title,
                                crate::app::fmt_n(*b as u64),
                                crate::app::fmt_n(rows.len())
                            ),
                            None => format!("{} ({})", title, crate::app::fmt_n(rows.len())),
                        };
                        let resp = ui
                            .horizontal(|ui| {
                                badge(ui, rule.level);
                                // Long titles are cut (full title on hover): the panel keeps its width.
                                ui.add(egui::Button::selectable(on, label).truncate())
                            })
                            .inner;
                        let resp = resp.on_hover_ui(|ui| rule_card(ui, rule, false));
                        if resp.clicked() {
                            set_focus = Some(if on { None } else { Some(f) });
                        }
                    }
                    Item::Credit => {
                        if res.hits.is_empty() {
                            ui.label(RichText::new("No rule matched this file.").weak());
                        }
                        ui.add_space(8.0);
                        credit(ui);
                    }
                }
            }
        });
    if let Some(f) = set_focus {
        t.sigma.set_focus(f);
        changed = true;
    }
    (act, changed)
}

/// Licence notice: the rules' authors are credited on every detection.
/// The log a rule reads, short (`Sysmon 1`, `Security 4688`), when it names one.
fn source_hint(rule: &forhunt_core::sigma::Rule) -> Option<String> {
    let [ch] = rule.channels()? else {
        return None;
    };
    let low = ch.to_ascii_lowercase();
    let name = if low.contains("sysmon") {
        "Sysmon".to_string()
    } else {
        let last = ch
            .rsplit(['/', '-'])
            .find(|p| !p.is_empty() && !p.eq_ignore_ascii_case("operational"))?;
        let mut c = last.chars();
        c.next()?.to_uppercase().chain(c).collect()
    };
    Some(match rule.event_ids() {
        Some([e]) => format!("{name} {e}"),
        _ => name,
    })
}

pub fn credit(ui: &mut Ui) {
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing.x = 0.0;
        ui.label(
            RichText::new("Rules by their authors (shown on each rule), under the ")
                .small()
                .weak(),
        );
        ui.hyperlink_to(
            RichText::new("Detection Rule License 1.1").small(),
            crate::sigma::LICENSE_URL,
        );
        ui.label(RichText::new(".").small().weak());
    });
}

/// Title, level, author, description, tags and links of a rule.
pub fn rule_card(ui: &mut Ui, rule: &forhunt_core::sigma::Rule, full: bool) {
    ui.horizontal_wrapped(|ui| {
        badge(ui, rule.level);
        ui.strong(&rule.title);
    });
    if !rule.author.is_empty() {
        ui.label(RichText::new(format!("by {}", rule.author)).small());
    }
    if let Some(c) = &rule.corr {
        ui.label(RichText::new(correlation_text(c)).small().italics());
    }
    if !rule.description.is_empty() {
        ui.label(&rule.description);
    }
    let mitre = rule.mitre();
    if !mitre.is_empty() {
        ui.label(RichText::new(format!("ATT&CK: {}", mitre.join(", "))).small());
    }
    if !rule.fields.is_empty() {
        ui.label(
            RichText::new(format!("tests: {}", rule.fields.join(", ")))
                .small()
                .weak(),
        );
    }
    if full {
        if !rule.falsepositives.is_empty() {
            ui.label(
                RichText::new(format!(
                    "false positives: {}",
                    rule.falsepositives.join("; ")
                ))
                .small()
                .weak(),
            );
        }
        for r in rule.references.iter().take(5) {
            if r.starts_with("https://") || r.starts_with("http://") {
                ui.hyperlink_to(RichText::new(r).small(), r);
            }
        }
    }
    ui.label(
        RichText::new(format!(
            "{} · {} · {}",
            rule.id,
            rule.status,
            rule.path
                .file_name()
                .map(|f| f.to_string_lossy().into_owned())
                .unwrap_or_default()
        ))
        .small()
        .weak(),
    )
    .on_hover_text(rule.path.display().to_string());
}

/// The detail pane's section for the selected row: the rules it matched.
pub fn detail(ui: &mut Ui, t: &Tab, row: u32) {
    let Some(res) = &t.sigma.result else {
        return;
    };
    let rs = res.row_rules(row);
    if rs.is_empty() {
        return;
    }
    egui::CollapsingHeader::new(
        RichText::new(format!(
            "Sigma: {} rule{}",
            rs.len(),
            if rs.len() == 1 { "" } else { "s" }
        ))
        .strong(),
    )
    .id_salt("sigma_detail")
    .default_open(true)
    .show(ui, |ui| {
        // Per rule its level, name and author (the full card on hover), in a
        // short scrolled list: the row's fields stay in view however many
        // rules matched.
        let row_h = ui.spacing().interact_size.y;
        egui::ScrollArea::vertical()
            .id_salt("sigma_detail_rules")
            .max_height(row_h * 7.0)
            .auto_shrink([false, true])
            .show(ui, |ui| {
                for r in &rs {
                    let rule = &res.set.rules()[*r as usize];
                    let resp = ui
                        .vertical(|ui| {
                            ui.spacing_mut().item_spacing.y = 1.0;
                            ui.horizontal(|ui| {
                                badge(ui, rule.level);
                                ui.add(
                                    egui::Label::new(RichText::new(&rule.title).strong())
                                        .truncate(),
                                );
                            });
                            if !rule.author.is_empty() {
                                ui.add(
                                    egui::Label::new(
                                        RichText::new(format!("by {}", rule.author)).small().weak(),
                                    )
                                    .truncate(),
                                );
                            }
                            ui.add_space(3.0);
                        })
                        .response;
                    resp.on_hover_ui(|ui| rule_card(ui, rule, true));
                }
            });
    });
    ui.separator();
}

/// Settings > Detection rules. Returns true when a setting changed.
pub fn settings(ui: &mut Ui, s: &mut SigmaSettings, lib: &mut Library) -> bool {
    let mut changed = false;
    ui.label(
        RichText::new(
            "Sigma rules run on demand over a tab (Detect in the toolbar). Nothing is downloaded \
             until you ask.",
        )
        .small(),
    );
    ui.add_space(4.0);
    ui.strong("Public rule sets");
    ui.label(
        RichText::new(
            "Downloaded from GitHub on request (newest version), kept on this computer, never \
             shared. Each set has its own licence; every detection names the rule's author.",
        )
        .small(),
    );
    for src in crate::sigma::CATALOG {
        ui.add_space(2.0);
        let inst = lib.installed.get(src.slug).cloned();
        ui.horizontal_wrapped(|ui| {
            let mut on = s.sets.iter().any(|x| x == src.slug);
            let r = ui
                .add_enabled(inst.is_some(), egui::Checkbox::new(&mut on, ""))
                .on_hover_text("use these rules")
                .on_disabled_hover_text("download them first");
            if r.changed() {
                s.sets.retain(|x| x != src.slug);
                if on {
                    s.sets.push(src.slug.to_string());
                }
                changed = true;
            }
            ui.hyperlink_to(RichText::new(src.name).strong(), src.home);
            ui.hyperlink_to(RichText::new(src.licence).small(), src.licence_url);
        });
        ui.indent(src.slug, |ui| {
            ui.label(RichText::new(src.about).small());
            ui.horizontal_wrapped(|ui| {
                match &inst {
                    Some(i) => {
                        ui.label(
                            RichText::new(format!(
                                "Downloaded {}: {} rule files{}",
                                i.downloaded.get(..10).unwrap_or(&i.downloaded),
                                crate::app::fmt_n(i.files as u64),
                                if i.commit.is_empty() {
                                    String::new()
                                } else {
                                    format!(", commit {}", &i.commit[..i.commit.len().min(10)])
                                }
                            ))
                            .small(),
                        );
                    }
                    None => {
                        ui.label(RichText::new("Not downloaded.").small().weak());
                    }
                }
                match &lib.download {
                    Some(d) if d.slug == src.slug => {
                        crate::appearance::spinner(ui);
                        ui.label(format!(
                            "downloading… {:.1} MB ({:.0} s)",
                            d.bytes.load(std::sync::atomic::Ordering::Relaxed) as f64 / 1e6,
                            d.started.elapsed().as_secs_f64()
                        ));
                        if ui.small_button("Cancel").clicked() {
                            lib.cancel_download();
                        }
                    }
                    other => {
                        let label = if inst.is_some() { "Update" } else { "Download" };
                        if ui
                            .add_enabled(other.is_none(), egui::Button::new(label).small())
                            .on_hover_text(format!("{} ({}, HTTPS)", src.archive, src.size))
                            .clicked()
                        {
                            lib.start_download(src.slug);
                            // A first download is meant to be used.
                            if inst.is_none() && !s.sets.iter().any(|x| x == src.slug) {
                                s.sets.push(src.slug.to_string());
                                changed = true;
                            }
                        }
                        // Remove asks once: the set has to be downloaded again.
                        let ask = egui::Id::unique(("sigma_remove", src.slug));
                        let asking = ui.data(|d| d.get_temp::<bool>(ask)).unwrap_or(false);
                        if inst.is_some() && !asking {
                            if ui
                                .add(egui::Button::new("Remove").small())
                                .on_hover_text("delete the downloaded rules")
                                .clicked()
                            {
                                ui.data_mut(|d| d.insert_temp(ask, true));
                            }
                        } else if inst.is_some() {
                            ui.label(RichText::new("Delete these rules?").small());
                            if ui.add(egui::Button::new("Delete").small()).clicked() {
                                ui.data_mut(|d| d.remove::<bool>(ask));
                                match lib.remove(src.slug) {
                                    Ok(()) => {
                                        s.sets.retain(|x| x != src.slug);
                                        changed = true;
                                    }
                                    Err(e) => {
                                        lib.message =
                                            Some((src.slug, Err(format!("cannot delete: {e}"))))
                                    }
                                }
                            }
                            if ui.add(egui::Button::new("Keep").small()).clicked() {
                                ui.data_mut(|d| d.remove::<bool>(ask));
                            }
                        }
                    }
                }
            });
            if let Some((slug, m)) = &lib.message
                && *slug == src.slug
            {
                match m {
                    Ok(m) => {
                        ui.label(RichText::new(format!("Installed: {m}.")).small());
                    }
                    Err(e) => {
                        ui.colored_label(ui.visuals().error_fg_color, e);
                    }
                }
            }
        });
    }
    ui.add_space(6.0);
    ui.strong("Your own rules");
    ui.label(
        RichText::new(
            "Folders of Sigma rules (.yml), searched recursively; a config/expand/ folder in them \
             gives values to %placeholders%. The forhunt skill's rules folder works too.",
        )
        .small(),
    );
    let mut remove = None;
    for (i, f) in s.folders.iter().enumerate() {
        ui.horizontal(|ui| {
            if ui
                .small_button("×")
                .on_hover_text("remove this folder")
                .clicked()
            {
                remove = Some(i);
            }
            // How this folder's rules use their logsource.
            let cur = s.profiles.get(f).cloned().unwrap_or_else(|| "auto".into());
            let mut pick = cur.clone();
            egui::ComboBox::from_id_salt(("sigma_profile", i))
                .width(110.0)
                .selected_text(profile_label(&cur))
                .show_ui(ui, |ui| {
                    for p in ["auto", "sigma", "as-written"] {
                        ui.selectable_value(&mut pick, p.to_string(), profile_label(p))
                            .on_hover_text(profile_tip(p));
                    }
                })
                .response
                .on_hover_text(profile_tip(&cur));
            if pick != cur {
                if pick == "auto" {
                    s.profiles.remove(f);
                } else {
                    s.profiles.insert(f.clone(), pick);
                }
                changed = true;
            }
            ui.label(f.display().to_string());
        });
    }
    if let Some(i) = remove {
        let f = s.folders.remove(i);
        s.profiles.remove(&f);
        changed = true;
    }
    ui.horizontal(|ui| {
        if ui.button("Add rules folder…").clicked()
            && let Some(p) = rfd::FileDialog::new().pick_folder()
            && !s.folders.contains(&p)
        {
            s.folders.push(p);
            changed = true;
        }
        if ui
            .add_enabled(!lib.loading(), egui::Button::new("Reload rules"))
            .on_hover_text("read the rule files again (after editing them on disk)")
            .clicked()
        {
            lib.reload();
        }
    });
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.label("Run rules from level");
        let mut lvl = s.min_level.clamp(1, 5);
        egui::ComboBox::from_id_salt("sigma_min_level")
            .selected_text(LEVELS[lvl as usize])
            .show_ui(ui, |ui| {
                for l in 1..=5u8 {
                    ui.selectable_value(&mut lvl, l, LEVELS[l as usize]);
                }
            });
        if lvl != s.min_level {
            s.min_level = lvl;
            changed = true;
        }
    });
    changed |= ui
        .checkbox(&mut s.noisy, "Include noisy rules")
        .on_hover_text(
            "rules Hayabusa leaves out by default (noisy_rules.txt): many false positives",
        )
        .changed();
    ui.horizontal(|ui| {
        ui.label("Fill rows from level");
        let mut lvl = s.tint_level.min(5);
        egui::ComboBox::from_id_salt("sigma_tint")
            .selected_text(if lvl == 0 {
                "never"
            } else {
                LEVELS[lvl as usize]
            })
            .show_ui(ui, |ui| {
                ui.selectable_value(&mut lvl, 0, "never");
                for l in 1..=5u8 {
                    ui.selectable_value(&mut lvl, l, LEVELS[l as usize]);
                }
            });
        if lvl != s.tint_level {
            s.tint_level = lvl;
            changed = true;
        }
    })
    .response
    .on_hover_text(
        "Detected rows always get a level mark in the row gutter; these also get a light fill.",
    );
    // What is loaded.
    ui.add_space(4.0);
    if let Some(e) = &lib.load_error {
        ui.colored_label(ui.visuals().error_fg_color, e);
    }
    if lib.loading() {
        ui.horizontal(|ui| {
            crate::appearance::spinner(ui);
            ui.label("loading rules…");
        });
    } else if let (Some(set), Some(rep)) = (&lib.set, &lib.report) {
        ui.label(
            RichText::new(format!(
                "{} rules loaded from {} files.",
                crate::app::fmt_n(set.len() as u64),
                crate::app::fmt_n(rep.files as u64)
            ))
            .small(),
        );
        if !rep.skipped.is_empty() {
            let s: Vec<String> = rep
                .skipped
                .iter()
                .map(|(k, v)| format!("{v} {k}"))
                .collect();
            ui.label(
                RichText::new(format!("Skipped: {}.", s.join(", ")))
                    .small()
                    .weak(),
            );
        }
        if !rep.errors.is_empty() {
            egui::CollapsingHeader::new(
                RichText::new(format!("{} rule files not loaded", rep.errors.len()))
                    .color(ui.visuals().warn_fg_color),
            )
            .id_salt("sigma_errors")
            .show(ui, |ui| {
                egui::ScrollArea::vertical()
                    .max_height(140.0)
                    .show(ui, |ui| {
                        for (p, e) in &rep.errors {
                            ui.label(RichText::new(format!("{}: {e}", p.display())).small());
                        }
                    });
            });
        }
    }
    changed
}

/// "value_count of TargetUserName >= 20 within 30 min, per Computer".
fn correlation_text(c: &forhunt_core::sigma::corr::Corr) -> String {
    use forhunt_core::sigma::corr::{CmpOp, CorrKind};
    let op = match c.op {
        CmpOp::Gt => ">",
        CmpOp::Ge => "≥",
        CmpOp::Lt => "<",
        CmpOp::Le => "≤",
        CmpOp::Eq => "=",
        CmpOp::Ne => "≠",
    };
    let what = match (c.kind, &c.field) {
        (CorrKind::EventCount, _) => format!("events {op} {}", c.n),
        (CorrKind::ValueCount, Some(f)) => format!("distinct {f} {op} {}", c.n),
        (CorrKind::ValueSum, Some(f)) => format!("sum of {f} {op} {}", c.n),
        (CorrKind::ValueAvg, Some(f)) => format!("average {f} {op} {}", c.n),
        (CorrKind::Temporal, _) => format!("all of {}", c.refs.join(", ")),
        (CorrKind::TemporalOrdered, _) => format!("in order: {}", c.refs.join(" → ")),
        (k, _) => k.name().to_string(),
    };
    let mins = c.timespan as f64 / 600_000_000.0;
    let span = if c.timespan >= i64::MAX / 8 {
        "over the whole file".to_string()
    } else if mins >= 60.0 {
        format!("within {:.0} h", mins / 60.0)
    } else {
        format!("within {mins:.0} min")
    };
    let by = if c.group_by.is_empty() {
        String::new()
    } else {
        format!(", per {}", c.group_by.join(", "))
    };
    format!("Correlation: {what} {span}{by}")
}

fn profile_label(p: &str) -> &'static str {
    match p {
        "sigma" => "Sigma",
        "as-written" => "As written",
        _ => "Auto",
    }
}

fn profile_tip(p: &str) -> &'static str {
    match p {
        "sigma" => {
            "Every rule is run on the logs its logsource names (process_creation: Sysmon 1 and Security 4688...), even rules that name a Channel."
        }
        "as-written" => {
            "The detection alone decides: logsource is ignored (rules written for one log, with Channel and EventID)."
        }
        _ => {
            "Rules that name a Channel run as written (Hayabusa, forhunt rules); the others run on the logs their logsource names (SigmaHQ and most public rules)."
        }
    }
}
