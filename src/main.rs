#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
mod config;
mod core;
mod ed;
mod kinds;
mod matching;
mod ops;
mod resolve;
mod detect;
mod langs;
mod syntax;
mod testing;
mod util;
mod view;

use config::Config;
use core::fsops;
use core::index::Index;
use core::sys::{self, Drive, Known};
use core::watch::Watcher;
use slint::platform::Key;
use slint::{ComponentHandle, Model, ModelRc, SharedString, VecModel, Weak};
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use util::*;

slint::include_modules!();

const ROW_H: f32 = 30.0;

pub struct Entry { pub name: String, pub dir: bool, pub size: u64, pub mtime: i64, pub hidden: bool, pub k: String, pub cat: &'static str, pub kind: String }
pub struct Tab { pub path: String, pub back: Vec<String>, pub fwd: Vec<String>, pub ed: Option<Box<ed::EditBuf>> }

pub struct State {
    pub cfg: Config,
    pub known: Known,
    pub drives: Vec<Drive>,
    pub index: Index,
    pub watcher: Watcher,
    pub idx_status: (usize, bool),
    pub tabs: Vec<Tab>,
    pub tab: usize,
    pub cwd: String,
    pub entries: Vec<Entry>,
    pub view: Vec<usize>,                 // indices into `entries`, in display order
    pub pos: HashMap<String, usize>,      // entry key -> position in `view`
    pub sel: HashSet<String>,
    pub cur: Option<usize>,               // position in `view`
    pub anchor: Option<usize>,
    pub sort_key: i32,                    // 0 name, 1 modified, 2 type, 3 size
    pub sort_dir: i32,
    pub filter: String,
    pub rows: Rc<VecModel<EntryRow>>,
    // go bar
    pub bar_base: String,
    pub cands: Vec<resolve::Res>,
    pub idle_paths: Vec<String>,
    pub cand_sel: usize,
    pub resolve_note: String,
    pub typeahead: (String, i64),
    pub toast: String,
    pub renaming: Option<String>,
    pub clip: Option<ops::Clip>,
    pub undo: Vec<ops::Undo>,
    pub armed: Option<usize>,
    pub media: view::Media,
}

type Ctx = Option<(Weak<App>, State)>;
thread_local! { static CTX: RefCell<Ctx> = const { RefCell::new(None) }; }

/// run something against the app and its state (UI thread only)
pub fn with<R>(f: impl FnOnce(&App, &mut State) -> R) -> Option<R> {
    CTX.with(|c| {
        let mut b = c.borrow_mut();
        let (w, s) = b.as_mut()?;
        let ui = w.upgrade()?;
        Some(f(&ui, s))
    })
}
/// same, but callable from any thread
pub fn post(f: impl FnOnce(&App, &mut State) + Send + 'static) {
    let _ = slint::invoke_from_event_loop(move || { with(f); });
}

fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let (a, b) = (a.to_lowercase(), b.to_lowercase());
    let (mut ia, mut ib) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (ia.peek().copied(), ib.peek().copied()) {
            (None, None) => return std::cmp::Ordering::Equal,
            (None, _) => return std::cmp::Ordering::Less,
            (_, None) => return std::cmp::Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let (mut na, mut nb) = (String::new(), String::new());
                while let Some(c) = ia.peek().copied().filter(|c| c.is_ascii_digit()) { na.push(c); ia.next(); }
                while let Some(c) = ib.peek().copied().filter(|c| c.is_ascii_digit()) { nb.push(c); ib.next(); }
                let (na, nb) = (na.trim_start_matches('0'), nb.trim_start_matches('0'));
                let o = na.len().cmp(&nb.len()).then_with(|| na.cmp(nb));
                if o != std::cmp::Ordering::Equal { return o; }
            }
            (Some(x), Some(y)) => {
                if x != y { return x.cmp(&y); }
                ia.next();
                ib.next();
            }
        }
    }
}

// ---------------------------------------------------------------- view building

fn build_view(st: &mut State) {
    let ft = st.filter.to_lowercase();
    let mut v: Vec<usize> = (0..st.entries.len())
        .filter(|&i| {
            let e = &st.entries[i];
            (st.cfg.opts.hidden || !e.hidden) && (ft.is_empty() || e.k.contains(&ft) || matching::match_name(&matching::norm_tok(&st.filter), &e.name).is_some())
        })
        .collect();
    let (key, dir) = (st.sort_key, st.sort_dir);
    let es = &st.entries;
    v.sort_by(|&a, &b| {
        let (x, y) = (&es[a], &es[b]);
        if x.dir != y.dir { return if x.dir { std::cmp::Ordering::Less } else { std::cmp::Ordering::Greater }; }
        let o = match key {
            1 => x.mtime.cmp(&y.mtime),
            2 => x.kind.cmp(&y.kind),
            3 => x.size.cmp(&y.size),
            _ => natural_cmp(&x.name, &y.name),
        }
        .then_with(|| natural_cmp(&x.name, &y.name));
        if dir < 0 { o.reverse() } else { o }
    });
    st.pos = v.iter().enumerate().map(|(p, &i)| (es[i].k.clone(), p)).collect();
    st.view = v;
}

fn row_of(st: &State, p: usize) -> EntryRow {
    let e = &st.entries[st.view[p]];
    let img = view::thumb_for(st, e, 40);
    EntryRow {
        name: e.name.as_str().into(), dir: e.dir,
        size: if e.dir { view::dir_size_text(st, e).into() } else { fmt_size(e.size).into() },
        has_thumb: img.is_some(), thumb: img.unwrap_or_default(),
        date: fmt_date(e.mtime).into(), kind: e.kind.as_str().into(),
        selected: st.sel.contains(&e.k), hidden: e.hidden,
        cut: st.clip.as_ref().map(|c| c.cut && c.paths.iter().any(|p| eq(p, &join(&st.cwd, &e.name)))).unwrap_or(false),
        renaming: st.renaming.as_deref() == Some(e.k.as_str()),
    }
}

pub fn push_rows(ui: &App, st: &mut State) {
    view::push_list(ui, st);
    view::request_visible(ui, st);
}

/// repaint only the rows whose selection changed
fn sync_selection(ui: &App, st: &State, old: &HashSet<String>) {
    for k in old.symmetric_difference(&st.sel) { view::refresh_item(ui, st, k); }
}

pub fn push_status(ui: &App, st: &State) {
    ui.set_has_sel(!st.sel.is_empty());
    ui.set_can_paste(st.clip.is_some());
    ui.set_can_undo(!st.undo.is_empty());
    let n = st.view.len();
    let selc = st.sel.len();
    let size: u64 = st.sel.iter().filter_map(|k| st.pos.get(k)).map(|&p| &st.entries[st.view[p]]).filter(|e| !e.dir).map(|e| e.size).sum();
    let mut l = format!("{} items", n);
    if st.entries.len() != n { l += &format!(" (of {})", st.entries.len()); }
    if selc > 0 { l += &format!("  ·  {} selected", selc); if size > 0 { l += &format!("  ·  {}", fmt_size(size)); } }
    ui.set_status_left(l.into());
    let ix = if st.idx_status.1 { format!("{} folders indexed", st.idx_status.0) } else { "indexing…".to_string() };
    ui.set_status_right((if st.toast.is_empty() { ix } else { st.toast.clone() }).into());
}

fn apply_opts(ui: &App, cfg: &config::Config) {
    let o = &cfg.opts;
    ui.set_opt_hidden(o.hidden); ui.set_opt_sizes(o.folder_sizes); ui.set_opt_lone(o.lone); ui.set_opt_pane(o.pane); ui.set_opt_motion(o.motion);
    ui.set_opt_theme(if o.theme == "light" { 1 } else { 0 });
    let th = ui.global::<Theme>();
    th.set_light(o.theme == "light");
    let m = |ms: i64| if o.motion { ms } else { 0 } as i64;
    th.set_t_fast(m(90)); th.set_t(m(150)); th.set_t_slow(m(240));
}

pub fn push_chrome_tabs(ui: &App, st: &State) {
    let tabs: Vec<TabRow> = st.tabs.iter().enumerate().map(|(i, t)| match &t.ed {
        Some(e) => TabRow { title: e.name.as_str().into(), dirty: e.dirty(), edit: true, active: i == st.tab },
        None => TabRow { title: (if leaf(&t.path).is_empty() { t.path.as_str() } else { leaf(&t.path) }).into(), dirty: false, edit: false, active: i == st.tab },
    }).collect();
    ui.set_tabs(ModelRc::new(VecModel::from(tabs)));
}

pub fn push_chrome(ui: &App, st: &mut State) {
    push_chrome_tabs(ui, st);
    // breadcrumbs: the folder, or the file when an editor tab is active
    let t = &st.tabs[st.tab];
    let shown = if t.ed.is_some() { t.path.clone() } else { st.cwd.clone() };
    let crumbs: Vec<SharedString> = segs(&shown).iter().map(|s| (*s).into()).collect();
    ui.set_crumbs(ModelRc::new(VecModel::from(crumbs)));
    let editing = t.ed.is_some();
    ui.set_can_back(!editing && !t.back.is_empty());
    ui.set_can_fwd(!editing && !t.fwd.is_empty());
    ui.set_can_up(!editing && parent(&st.cwd).is_some());
    ui.set_sort_key(st.sort_key);
    ui.set_sort_dir(st.sort_dir);
    // sidebar
    let home = &st.known.home;
    let mut rows: Vec<SideRow> = Vec::new();
    let mut y = 2.0f32;
    let (mut hi_y, mut hi_on) = (0.0f32, false);
    let hdr = |l: &str, add: &str| SideRow { label: l.into(), sub: add.into(), path: SharedString::new(), kind: 0, active: false, meter: 0.0 };
    rows.push(hdr("Pinned", "pin")); y += 34.0;
    for p in &st.cfg.pins {
        let active = eq(p, &st.cwd);
        if active && !hi_on { hi_y = y; hi_on = true; }
        rows.push(SideRow { label: leaf(p).into(), sub: SharedString::new(), path: p.as_str().into(), kind: 1, active, meter: 0.0 });
        y += 30.0;
    }
    rows.push(hdr(&format!("Aliases  {}", st.cfg.aliases.len()), "alias")); y += 34.0;
    if st.cfg.opts.aliases_open {
        for a in &st.cfg.aliases {
            let active = eq(&a.path, &st.cwd);
            if active && !hi_on { hi_y = y; hi_on = true; }
            rows.push(SideRow { label: a.name.as_str().into(), sub: short_path(&a.path, home).into(), path: a.path.as_str().into(), kind: 2, active, meter: 0.0 });
            y += 30.0;
        }
    }
    rows.push(hdr("Drives", "")); y += 34.0;
    for d in &st.drives {
        let used = d.total.saturating_sub(d.free);
        let prefix = drive_prefix(&d.path);
        let active = st.cwd.to_lowercase().starts_with(&d.path.to_lowercase());
        rows.push(SideRow { label: format!("{}{}", prefix, if d.label.is_empty() { String::new() } else { format!(" {}", d.label) }).into(), sub: format!("{} free", fmt_size(d.free)).into(), path: d.path.as_str().into(), kind: 3, active, meter: if d.total > 0 { used as f32 / d.total as f32 } else { 0.0 } });
        y += 46.0;
    }
    ui.set_aliases_open(st.cfg.opts.aliases_open);
    ui.set_side_hi_on(hi_on);
    ui.set_side_hi_y(hi_y);
    ui.set_side_hi_h(30.0);
    ui.set_side_height(y + 12.0);
    ui.set_side(ModelRc::new(VecModel::from(rows)));
    push_status(ui, st);
}

pub fn toggle_pin(ui: &App, st: &mut State, path: &str) {
    if let Some(i) = st.cfg.pins.iter().position(|p| eq(p, path)) { st.cfg.pins.remove(i); toast(ui, st, &format!("Unpinned {}", leaf(path))); }
    else { st.cfg.pins.push(path.to_string()); toast(ui, st, &format!("Pinned {}", leaf(path))); }
    st.cfg.save(); push_chrome(ui, st);
}

fn add_alias(ui: &App, st: &mut State) {
    let path = st.cwd.clone();
    if let Some(a) = st.cfg.aliases.iter().find(|a| eq(&a.path, &path)) { let n = a.name.clone(); toast(ui, st, &format!("Already an alias: {n}")); return; }
    let base: String = leaf(&path).to_lowercase().chars().filter(|c| c.is_alphanumeric()).collect();
    let base = if base.is_empty() { "here".to_string() } else { base };
    let mut name = base.clone(); let mut n = 2;
    while st.cfg.aliases.iter().any(|a| a.name.eq_ignore_ascii_case(&name)) { name = format!("{base}{n}"); n += 1; }
    st.cfg.aliases.push(config::Alias { name: name.clone(), path });
    st.cfg.save(); push_chrome(ui, st);
    toast(ui, st, &format!("Alias \"{name}\" added. Type it in the path bar."));
}

// ---------------------------------------------------------------- navigation

pub enum Hist { Push, Back, Fwd, None }

pub fn go(ui: &App, st: &mut State, path: &str, hist: Hist, select: Option<&str>) -> bool {
    let list = match fsops::list_dir(path.to_string(), &st.index) {
        Ok(l) => l,
        Err(e) => { toast(ui, st, &format!("Can't open {}: {}", leaf(path), e)); return false; }
    };
    if st.tabs[st.tab].ed.is_some() { st.tabs.insert(st.tab + 1, Tab { path: path.to_string(), back: vec![], fwd: vec![], ed: None }); st.tab += 1; }
    ed::leave(ui, st);
    let t = &mut st.tabs[st.tab];
    match hist {
        Hist::Push => { if !eq(&t.path, path) { t.back.push(t.path.clone()); t.fwd.clear(); } }
        Hist::Back => { t.fwd.push(t.path.clone()); }
        Hist::Fwd => { t.back.push(t.path.clone()); }
        Hist::None => {}
    }
    t.path = path.to_string();
    st.cwd = path.to_string();
    st.cfg.bump(path);
    st.cfg.save();
    st.entries = list.into_iter().map(|f| { let (cat, kind) = kinds::cat_of(&f.name, f.dir); Entry { k: f.name.to_lowercase(), name: f.name, dir: f.dir, size: f.size, mtime: f.mtime, hidden: f.hidden, cat, kind } }).collect();
    st.filter.clear();
    st.sel.clear();
    st.cur = None;
    st.anchor = None;
    build_view(st);
    if let Some(name) = select {
        if let Some(&p) = st.pos.get(&name.to_lowercase()) { st.sel.insert(name.to_lowercase()); st.cur = Some(p); st.anchor = Some(p); }
    }
    if st.cfg.opts.motion { ui.set_list_opacity(0.0); let w = ui.as_weak(); slint::Timer::single_shot(std::time::Duration::from_millis(24), move || { if let Some(u) = w.upgrade() { u.set_list_opacity(1.0); } }); }
    push_rows(ui, st);
    ui.set_list_y(0.0);
    if let Some(p) = st.cur { reveal(ui, st, p); }
    push_chrome(ui, st);
    view::schedule_pane(st);
    let _ = st.watcher.watch(path, || post(|ui, st| refresh(ui, st)));
    true
}

pub fn refresh(ui: &App, st: &mut State) {
    let path = st.cwd.clone();
    match fsops::list_dir(path.clone(), &st.index) {
        Ok(list) => {
            st.entries = list.into_iter().map(|f| { let (cat, kind) = kinds::cat_of(&f.name, f.dir); Entry { k: f.name.to_lowercase(), name: f.name, dir: f.dir, size: f.size, mtime: f.mtime, hidden: f.hidden, cat, kind } }).collect();
            let keep: HashSet<String> = st.entries.iter().map(|e| e.k.clone()).collect();
            st.sel.retain(|k| keep.contains(k));
            build_view(st);
            st.cur = st.cur.filter(|&p| p < st.view.len());
            st.media.sizes.clear();
            push_rows(ui, st);
            push_status(ui, st);
        }
        Err(_) => { if let Some(p) = parent(&path) { go(ui, st, &p, Hist::None, None); } }
    }
}

fn reveal(ui: &App, st: &State, p: usize) {
    let rh = view::row_height(st);
    let top = (p / st.media.cols.max(1)) as f32 * rh;
    let (y, h) = (-ui.get_list_y(), ui.get_list_h());
    if top < y { ui.set_list_y(-top); } else if top + rh > y + h && h > 0.0 { ui.set_list_y(-(top + rh - h)); }
}

pub fn toast(ui: &App, st: &mut State, msg: &str) {
    st.toast = msg.to_string();
    push_status(ui, st);
}

fn nav_back(ui: &App, st: &mut State) { if let Some(p) = st.tabs[st.tab].back.last().cloned() { let t = &mut st.tabs[st.tab]; t.back.pop(); if !go(ui, st, &p, Hist::Back, None) { st.tabs[st.tab].fwd.pop(); st.tabs[st.tab].back.push(p); } } }
fn nav_fwd(ui: &App, st: &mut State) { if let Some(p) = st.tabs[st.tab].fwd.last().cloned() { let t = &mut st.tabs[st.tab]; t.fwd.pop(); if !go(ui, st, &p, Hist::Fwd, None) { st.tabs[st.tab].back.pop(); st.tabs[st.tab].fwd.push(p); } } }
fn nav_up(ui: &App, st: &mut State) { if let Some(p) = parent(&st.cwd) { let me = leaf(&st.cwd).to_string(); go(ui, st, &p, Hist::Push, Some(&me)); } }

// ---------------------------------------------------------------- selection

pub fn set_selection(ui: &App, st: &mut State, sel: HashSet<String>, cur: Option<usize>, anchor: Option<usize>) {
    let old = std::mem::replace(&mut st.sel, sel);
    st.cur = cur;
    st.anchor = anchor;
    sync_selection(ui, st, &old);
    if let Some(p) = cur { reveal(ui, st, p); }
    push_status(ui, st);
    view::schedule_pane(st);
}

fn key_at(st: &State, p: usize) -> String { st.entries[st.view[p]].k.clone() }

fn select_row(ui: &App, st: &mut State, p: usize, ctrl: bool, shift: bool) {
    if p >= st.view.len() { return; }
    if shift {
        let a = st.anchor.unwrap_or(p);
        let (lo, hi) = if a < p { (a, p) } else { (p, a) };
        let s: HashSet<String> = (lo..=hi).map(|i| key_at(st, i)).collect();
        set_selection(ui, st, s, Some(p), Some(a));
    } else if ctrl {
        let mut s = st.sel.clone();
        let k = key_at(st, p);
        if !s.remove(&k) { s.insert(k); }
        set_selection(ui, st, s, Some(p), Some(p));
    } else {
        let s: HashSet<String> = std::iter::once(key_at(st, p)).collect();
        set_selection(ui, st, s, Some(p), Some(p));
    }
}

fn move_cur(ui: &App, st: &mut State, d: i32, extend: bool) {
    let n = st.view.len() as i32;
    if n == 0 { return; }
    let p = match st.cur { None => if d > 0 { 0 } else { n - 1 }, Some(c) => (c as i32 + d).clamp(0, n - 1) };
    select_row(ui, st, p as usize, false, extend);
}

pub fn open_row(ui: &App, st: &mut State, p: usize) {
    if p >= st.view.len() { return; }
    let e = &st.entries[st.view[p]];
    let path = join(&st.cwd, &e.name);
    if e.dir { go(ui, st, &path, Hist::Push, None); return; }
    let (name, size) = (e.name.clone(), e.size);
    if kinds::can_edit(&name, size) {
        match ed::open(ui, st, &path) {
            Ok(()) => return,
            Err(err) => { if !(err.contains("binary") || err.contains("too large")) { toast(ui, st, &format!("Can't edit {}: {}", name, err)); } }
        }
    }
    if let Err(err) = fsops::open_path(path) { toast(ui, st, &format!("Couldn't open {}: {}", name, err)); }
}

// ---------------------------------------------------------------- the go bar

fn runs_for(res: &resolve::Res) -> Vec<Run> {
    let full = res.lone.last().map(|l| l.1.clone()).unwrap_or_else(|| res.final_path.clone());
    let sg = segs(&full);
    #[derive(Clone)]
    enum Info { None, Matched(Vec<usize>), Via }
    let mut info = vec![Info::None; sg.len()];
    for st in &res.steps {
        let d = segs(&st.path).len().saturating_sub(1);
        if d < info.len() { info[d] = Info::Matched(st.idx.clone()); }
        for (j, _) in st.via.iter().enumerate() {
            let at = d as i64 - st.via.len() as i64 + j as i64;
            if at >= 0 && (at as usize) < info.len() { info[at as usize] = Info::Via; }
        }
    }
    let mut out: Vec<Run> = Vec::new();
    let mut push = |out: &mut Vec<Run>, t: String, k: i32| { if let Some(l) = out.last_mut() { if l.kind == k { l.text = format!("{}{}", l.text, t).into(); return; } } out.push(Run { text: t.into(), kind: k }); };
    for (i, s) in sg.iter().enumerate() {
        if i > 0 { push(&mut out, SEP.to_string(), 0); }
        match &info[i] {
            Info::None => push(&mut out, s.to_string(), 0),
            Info::Via => push(&mut out, s.to_string(), 3),
            Info::Matched(ix) if ix.is_empty() => push(&mut out, s.to_string(), 1),
            Info::Matched(ix) => {
                let set: HashSet<usize> = ix.iter().copied().collect();
                for (ci, ch) in s.chars().enumerate() { push(&mut out, ch.to_string(), if set.contains(&ci) { 2 } else { 1 }); }
            }
        }
    }
    out
}

fn note_for(s: &resolve::Step) -> String {
    let mut n = s.kind.clone();
    if s.jump { n += " · jumped from index"; } else if !s.note.is_empty() { n += &format!(" · {}", s.note); }
    if !s.via.is_empty() { n += &format!(" · via {}", s.via.join(&SEP.to_string())); }
    if s.total > 0 && s.alts == 1 { n += &format!(" · only match of {}", s.total); } else if s.alts > 1 { n += &format!(" · best of {} matches", s.alts); }
    n
}

fn draw_drop(ui: &App, st: &State) {
    let idle = st.cands.is_empty() && !st.idle_paths.is_empty();
    let home = &st.known.home;
    let rows: Vec<CandRow> = if idle {
        st.idle_paths.iter().enumerate().map(|(i, p)| CandRow { runs: ModelRc::new(VecModel::from(vec![Run { text: short_path(p, home).into(), kind: 1 }])), note: format!("{}×", st.cfg.frec.get(p).map(|f| f.n).unwrap_or(0)).into(), selected: i == st.cand_sel, bad: false }).collect()
    } else {
        st.cands.iter().enumerate().map(|(i, r)| {
            let last = r.steps.last();
            let note = if r.partial { format!("stops: no match for “{}”", r.unresolved) } else if r.file.is_some() { "file".into() } else if !r.lone.is_empty() { format!("+{} lone", r.lone.len()) } else if last.map(|s| s.jump).unwrap_or(false) { "jump".into() } else { String::new() };
            CandRow { runs: ModelRc::new(VecModel::from(runs_for(r))), note: note.into(), selected: i == st.cand_sel, bad: r.partial }
        }).collect()
    };
    ui.set_cands(ModelRc::new(VecModel::from(rows)));
    ui.set_drop_title(if idle { "Frequent".into() } else { SharedString::new() });
    // trace of the highlighted result
    let mut tr: Vec<TraceRow> = Vec::new();
    let mut foot = String::new();
    if let Some(r) = st.cands.get(st.cand_sel) {
        for s in &r.steps { tr.push(TraceRow { tok: s.tok.as_str().into(), name: s.name.as_str().into(), note: note_for(s).into(), warn: false }); }
        for (n, _) in &r.lone { tr.push(TraceRow { tok: SharedString::new(), name: n.as_str().into(), note: "only folder, no files → opened".into(), warn: false }); }
        if r.partial { tr.push(TraceRow { tok: r.unresolved.as_str().into(), name: "no match".into(), note: format!("staying in {}", leaf(&r.final_path)).into(), warn: true }); }
        if let Some(f) = &r.file { tr.push(TraceRow { tok: SharedString::new(), name: f.as_str().into(), note: "will be selected".into(), warn: false }); }
        else if let Some((d, f)) = r.counts { tr.push(TraceRow { tok: SharedString::new(), name: format!("{} folder{}, {} file{}", d, if d == 1 { "" } else { "s" }, f, if f == 1 { "" } else { "s" }).into(), note: SharedString::new(), warn: false }); }
        foot = st.resolve_note.clone();
    } else if idle { foot = "Type fragments in order, e.g. appdata local ath ver".into(); }
    ui.set_trace(ModelRc::new(VecModel::from(tr)));
    ui.set_trace_foot(foot.into());
}

fn bar_open(ui: &App, st: &mut State) {
    st.bar_base = st.cwd.clone();
    st.cands.clear();
    st.cand_sel = 0;
    let mut f: Vec<(&String, f64)> = st.cfg.frec.keys().filter(|p| std::path::Path::new(p.as_str()).is_dir()).map(|p| (p, st.cfg.frec_score(p))).collect();
    f.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
    st.idle_paths = f.into_iter().take(5).map(|(p, _)| p.clone()).collect();
    ui.set_bar_text(st.bar_base.as_str().into());
    ui.set_bar_editing(true);
    draw_drop(ui, st);
}

fn bar_close(ui: &App, st: &mut State) {
    ui.set_bar_editing(false);
    st.cands.clear();
    st.idle_paths.clear();
    ui.set_cands(ModelRc::new(VecModel::from(Vec::<CandRow>::new())));
}

fn bar_edited(ui: &App, st: &mut State, text: &str) {
    st.cand_sel = 0;
    if text.trim().is_empty() || text == st.bar_base {
        st.cands.clear();
        draw_drop(ui, st);
        return;
    }
    st.idle_paths.clear();
    let aliases: Vec<(String, String)> = st.cfg.aliases.iter().map(|a| (a.name.clone(), a.path.clone())).collect();
    let drives: Vec<String> = st.drives.iter().map(|d| d.path.clone()).collect();
    let vars = st.known.env.clone();
    let cfg = st.cfg.clone();
    let env = resolve::Env { cwd: &st.cwd, home: &st.known.home, aliases: &aliases, drives: &drives, vars: &vars, lone: st.cfg.opts.lone, boost: &|p| cfg.boost(p), index: &st.index };
    let r = resolve::resolve(text, &env);
    st.resolve_note = format!("resolved in {:.1} ms · {} dirs read", r.ms, r.scanned);
    st.cands = r.results;
    draw_drop(ui, st);
}

fn bar_accept(ui: &App, st: &mut State) {
    let target = if let Some(r) = st.cands.get(st.cand_sel) {
        Some((r.dir.clone(), r.file.clone(), r.partial.then(|| r.unresolved.clone())))
    } else { st.idle_paths.get(st.cand_sel).map(|p| (p.clone(), None, None)) };
    bar_close(ui, st);
    if let Some((dir, file, partial)) = target {
        if go(ui, st, &dir, Hist::Push, file.as_deref()) {
            if let Some(u) = partial { toast(ui, st, &format!("Stopped at {}: nothing matches “{}”", leaf(&dir), u)); }
        }
    }
}

fn bar_key(ui: &App, st: &mut State, text: &str) -> bool {
    if text == SharedString::from(Key::Escape).as_str() { bar_close(ui, st); return true; }
    let n = if st.cands.is_empty() { st.idle_paths.len() } else { st.cands.len() };
    if n > 0 && (text == SharedString::from(Key::DownArrow).as_str() || text == SharedString::from(Key::UpArrow).as_str()) {
        st.cand_sel = if text == SharedString::from(Key::DownArrow).as_str() { (st.cand_sel + 1) % n } else { (st.cand_sel + n - 1) % n };
        draw_drop(ui, st);
        return true;
    }
    false
}

// ---------------------------------------------------------------- keys

fn key_event(ui: &App, st: &mut State, text: &str, ctrl: bool, shift: bool, alt: bool) -> bool {
    let is = |k: Key| text == SharedString::from(k).as_str();
    if ctrl && (text == "l" || text == "k") { bar_open(ui, st); return true; }
    if ctrl && text == "d" { let p = st.cwd.clone(); toggle_pin(ui, st, &p); return true; }
    if ctrl && text == "t" { let p = st.cwd.clone(); st.tabs.push(Tab { path: p, back: vec![], fwd: vec![], ed: None }); st.tab = st.tabs.len() - 1; activate_tab(ui, st); return true; }
    if ctrl && text == "w" { close_tab(ui, st, st.tab); return true; }
    if ctrl && is(Key::Tab) { let n = st.tabs.len(); st.tab = if shift { (st.tab + n - 1) % n } else { (st.tab + 1) % n }; activate_tab(ui, st); return true; }
    if ed::is_editing(st) && !(alt && !ctrl) { return ed::key(ui, st, text, ctrl, shift, alt); }
    if text == "/" && !ctrl { bar_open(ui, st); return true; }
    if alt && !ctrl {
        if is(Key::LeftArrow) { nav_back(ui, st); return true; }
        if is(Key::RightArrow) { nav_fwd(ui, st); return true; }
        if is(Key::UpArrow) { nav_up(ui, st); return true; }
        return false;
    }
    if ctrl {
        match text {
            "a" => { let s: HashSet<String> = (0..st.view.len()).map(|i| key_at(st, i)).collect(); set_selection(ui, st, s, st.cur, st.anchor); return true; }
            "h" => { st.cfg.opts.hidden = !st.cfg.opts.hidden; st.cfg.save(); build_view(st); push_rows(ui, st); push_status(ui, st); return true; }
            "r" => { refresh(ui, st); return true; }
            "f" => { ui.invoke_focus_filter(); return true; }
            "c" => { ops::copy(ui, st, false); return true; }
            "x" => { ops::copy(ui, st, true); return true; }
            "v" => { ops::paste(ui, st); return true; }
            "z" => { ops::undo(ui, st); return true; }
            "N" | "n" if shift => { ops::new_item(ui, st, true); return true; }
            "t" => { let p = st.cwd.clone(); st.tabs.push(Tab { path: p, back: vec![], fwd: vec![], ed: None }); st.tab = st.tabs.len() - 1; push_chrome(ui, st); return true; }
            "w" => { close_tab(ui, st, st.tab); return true; }
            _ => return false,
        }
    }
    let cols = st.media.cols.max(1) as i32;
    let page = ((ui.get_list_h() / view::row_height(st)) as i32 - 1).max(1) * cols;
    if is(Key::DownArrow) { move_cur(ui, st, cols, shift); return true; }
    if is(Key::UpArrow) { move_cur(ui, st, -cols, shift); return true; }
    if is(Key::RightArrow) && st.media.tiles { move_cur(ui, st, 1, shift); return true; }
    if is(Key::LeftArrow) && st.media.tiles { move_cur(ui, st, -1, shift); return true; }
    if is(Key::PageDown) { move_cur(ui, st, page, shift); return true; }
    if is(Key::PageUp) { move_cur(ui, st, -page, shift); return true; }
    if is(Key::Home) { if !st.view.is_empty() { select_row(ui, st, 0, false, shift); } return true; }
    if is(Key::End) { if !st.view.is_empty() { let l = st.view.len() - 1; select_row(ui, st, l, false, shift); } return true; }
    if is(Key::Return) { if let Some(c) = st.cur { open_row(ui, st, c); } return true; }
    if is(Key::Backspace) { nav_up(ui, st); return true; }
    if is(Key::F5) { refresh(ui, st); return true; }
    if is(Key::F2) { ops::start_rename(ui, st); return true; }
    if is(Key::Delete) { ops::delete(ui, st); return true; }
    if is(Key::Escape) { if !st.sel.is_empty() { set_selection(ui, st, HashSet::new(), st.cur, st.anchor); } return true; }
    // type a few letters to jump to a name
    if text.chars().count() == 1 && !text.chars().next().unwrap().is_control() && !ctrl && !alt {
        let now = now_ms();
        let (mut buf, t) = std::mem::take(&mut st.typeahead);
        if now - t > 700 { buf.clear(); }
        buf.push_str(&text.to_lowercase());
        let start = st.cur.map(|c| if buf.chars().count() > 1 { c } else { c + 1 }).unwrap_or(0);
        let n = st.view.len();
        let hit = (0..n).map(|i| (start + i) % n.max(1)).find(|&p| st.entries[st.view[p]].k.starts_with(&buf));
        st.typeahead = (buf, now);
        if let Some(p) = hit { select_row(ui, st, p, false, false); }
        return true;
    }
    false
}

fn activate_tab(ui: &App, st: &mut State) {
    if st.tabs[st.tab].ed.is_some() { ed::activate(ui, st); } else { let p = st.tabs[st.tab].path.clone(); go(ui, st, &p, Hist::None, None); }
}

fn close_tab(ui: &App, st: &mut State, i: usize) {
    if st.tabs.len() < 2 || i >= st.tabs.len() { return; }
    if st.tabs[i].ed.as_ref().map(|e| e.dirty()).unwrap_or(false) && st.armed != Some(i) {
        st.armed = Some(i);
        let n = st.tabs[i].ed.as_ref().map(|e| e.name.clone()).unwrap_or_default();
        ops::notify(ui, st, &format!("{n} has unsaved changes. Close it again to discard them."));
        return;
    }
    st.armed = None;
    st.tabs.remove(i);
    st.tab = if st.tab > i { st.tab - 1 } else { st.tab.min(st.tabs.len() - 1) };
    activate_tab(ui, st);
}

// ---------------------------------------------------------------- startup

fn already_running() -> bool {
    #[cfg(windows)]
    {
    use windows_sys::Win32::{Foundation::{GetLastError, ERROR_ALREADY_EXISTS}, System::Threading::CreateMutexW, UI::WindowsAndMessaging::*};
    let name: Vec<u16> = r"Local\hop-single-instance".encode_utf16().chain(Some(0)).collect();
    let title: Vec<u16> = "hop".encode_utf16().chain(Some(0)).collect();
    unsafe {
        let _h = CreateMutexW(std::ptr::null(), 0, name.as_ptr()); // held for the life of the process
        if GetLastError() != ERROR_ALREADY_EXISTS { std::mem::forget(_h); return false; }
        let w = FindWindowW(std::ptr::null(), title.as_ptr());
        if !w.is_null() { if IsIconic(w) != 0 { ShowWindow(w, SW_RESTORE); } SetForegroundWindow(w); }
        true
    }
    }
    #[cfg(not(windows))]
    { false }
}

fn main() {
    // a crash leaves its reason in %APPDATA%\hop\crash.txt (release builds abort, which would otherwise be silent)
    std::panic::set_hook(Box::new(|info| {
        #[cfg(windows)]
        let dir = std::env::var("APPDATA").map(std::path::PathBuf::from).unwrap_or_else(|_| std::env::temp_dir()).join("hop");
        #[cfg(not(windows))]
        let dir = dirs::config_dir().unwrap_or_else(std::env::temp_dir).join("hop");
        let _ = std::fs::create_dir_all(&dir);
        let _ = std::fs::write(dir.join("crash.txt"), format!("{info}
{}", std::backtrace::Backtrace::force_capture()));
    }));
    // one window: a second launch just brings the first to the front
    if !std::env::args().any(|a| a == "--script") && already_running() { return; }
    // the software renderer: no GPU driver loaded, a fraction of the memory. HOP_RENDERER=gpu to compare.
    let renderer = if std::env::var("HOP_RENDERER").map(|v| v == "gpu").unwrap_or(false) { "femtovg" } else { "software" };
    let _ = slint::BackendSelector::new().backend_name("winit".into()).renderer_name(renderer.into()).select();

    let args: Vec<String> = std::env::args().collect();
    let arg = |name: &str| args.iter().skip_while(|a| a.as_str() != name).nth(1).cloned();

    let known = sys::known();
    let cfg = Config::load(&known);
    let start = arg("--cwd").or_else(|| args.get(1).filter(|a| !a.starts_with("--")).cloned()).filter(|p| std::path::Path::new(p).is_dir()).unwrap_or_else(|| known.home.clone());
    let index = Index::default();
    {
        let idx = index.clone();
        core::index::spawn_scan(idx, |n| post(move |ui, st| { st.idx_status = (n, true); push_status(ui, st); }));
    }

    let ui = App::new().unwrap();
    ui.global::<Theme>().set_light(cfg.opts.theme == "light");
    ui.set_has_pane(cfg.opts.pane);
    apply_opts(&ui, &cfg);
    let state = State {
        known, drives: sys::drives(), index, watcher: Watcher::default(), idx_status: (0, false),
        tabs: vec![Tab { path: start.clone(), back: vec![], fwd: vec![], ed: None }], tab: 0, cwd: start.clone(),
        entries: vec![], view: vec![], pos: HashMap::new(), sel: HashSet::new(), cur: None, anchor: None, sort_key: 0, sort_dir: 1, filter: String::new(),
        rows: Rc::new(VecModel::default()), bar_base: String::new(), cands: vec![], idle_paths: vec![], cand_sel: 0, resolve_note: String::new(), typeahead: (String::new(), 0), toast: String::new(), renaming: None, clip: None, undo: vec![], armed: None, media: view::Media::new(), cfg,
    };
    CTX.with(|c| *c.borrow_mut() = Some((ui.as_weak(), state)));

    ui.on_key_event(|t, c, s, a| with(|ui, st| key_event(ui, st, t.as_str(), c, s, a)).unwrap_or(false));
    ui.on_tab_clicked(|i| { with(|ui, st| { if (i as usize) < st.tabs.len() { st.tab = i as usize; activate_tab(ui, st); } }); });
    ui.on_tab_closed(|i| { with(|ui, st| close_tab(ui, st, i as usize)); });
    ui.on_new_tab(|| { with(|ui, st| { let p = st.cwd.clone(); st.tabs.push(Tab { path: p, back: vec![], fwd: vec![], ed: None }); st.tab = st.tabs.len() - 1; activate_tab(ui, st); }); });
    ui.on_nav_back(|| { with(|ui, st| nav_back(ui, st)); });
    ui.on_nav_fwd(|| { with(|ui, st| nav_fwd(ui, st)); });
    ui.on_nav_up(|| { with(|ui, st| nav_up(ui, st)); });
    ui.on_crumb_clicked(|i| { with(|ui, st| { let t = &st.tabs[st.tab]; let base = if t.ed.is_some() { t.path.clone() } else { st.cwd.clone() }; if t.ed.is_some() && (i as usize) + 1 == segs(&base).len() { return; } let sg = segs(&base); let mut p = String::new(); for (n, s) in sg.iter().enumerate().take(i as usize + 1) { p = if n == 0 { format!("{}\\", s) } else { join(&p, s) }; } let me = leaf(&st.cwd).to_string(); go(ui, st, &p, Hist::Push, if (i as usize) + 2 == sg.len() { Some(&me) } else { None }); }); });
    ui.on_bar_open(|| { with(|ui, st| bar_open(ui, st)); });
    ui.on_bar_edited(|t| { with(|ui, st| bar_edited(ui, st, t.as_str())); });
    ui.on_bar_accept(|| { with(|ui, st| bar_accept(ui, st)); });
    ui.on_bar_key(|t| with(|ui, st| bar_key(ui, st, t.as_str())).unwrap_or(false));
    ui.on_cand_clicked(|i| { with(|ui, st| { st.cand_sel = i as usize; bar_accept(ui, st); }); });
    ui.on_side_clicked(|i| { with(|ui, st| { if let Some(r) = ui.get_side().row_data(i as usize) { if r.kind != 0 { go(ui, st, r.path.as_str(), Hist::Push, None); } } }); });
    ui.on_row_clicked(|i, c, s| { with(|ui, st| select_row(ui, st, i as usize, c, s)); });
    ui.on_row_open(|i| { with(|ui, st| open_row(ui, st, i as usize)); });
    ui.on_sort_clicked(|i| { with(|ui, st| { if st.sort_key == i { st.sort_dir = -st.sort_dir; } else { st.sort_key = i; st.sort_dir = if i == 1 || i == 3 { -1 } else { 1 }; } build_view(st); push_rows(ui, st); push_chrome(ui, st); }); });
    ui.on_list_blank_clicked(|| { with(|ui, st| set_selection(ui, st, HashSet::new(), st.cur, st.anchor)); });

    ui.on_tile_clicked(|r, c, ctrl, shift| { with(|ui, st| { let p = r as usize * st.media.cols.max(1) + c as usize; select_row(ui, st, p, ctrl, shift); }); });
    ui.on_tile_open(|r, c| { with(|ui, st| { let p = r as usize * st.media.cols.max(1) + c as usize; open_row(ui, st, p); }); });
    ui.on_tile_context(|r, c| { with(|ui, st| { let p = r as usize * st.media.cols.max(1) + c as usize; if p < st.view.len() && !st.sel.contains(&key_at(st, p)) { select_row(ui, st, p, false, false); } }); });
    ui.on_scrolled(|| { with(|ui, st| view::request_visible(ui, st)); });
    ui.on_layout_changed(|| { with(|ui, st| view::on_layout(ui, st)); });
    ui.on_win_drag({ let w = ui.as_weak(); move || { if let Some(ui) = w.upgrade() { use slint::winit_030::WinitWindowAccessor; ui.window().with_winit_window(|win| { let _ = win.drag_window(); }); } } });
    ui.on_win_resize({ let w = ui.as_weak(); move |d| { if let Some(ui) = w.upgrade() {
        use slint::winit_030::{WinitWindowAccessor, winit::window::ResizeDirection as R};
        let dir = [R::East, R::North, R::NorthEast, R::NorthWest, R::West, R::South, R::SouthEast, R::SouthWest][(d as usize).min(7)];
        ui.window().with_winit_window(|win| { let _ = win.drag_resize_window(dir); });
    } } });
    ui.on_win_min({ let w = ui.as_weak(); move || { if let Some(ui) = w.upgrade() { ui.window().set_minimized(true); } } });
    ui.on_win_max({ let w = ui.as_weak(); move || { if let Some(ui) = w.upgrade() { let m = !ui.window().is_maximized(); ui.window().set_maximized(m); ui.set_maximized(m); } } });
    ui.on_win_close(|| { let _ = slint::quit_event_loop(); });
    ui.on_setting_toggled(|k| { with(|ui, st| {
        let o = &mut st.cfg.opts;
        match k.as_str() { "hidden" => o.hidden = !o.hidden, "sizes" => o.folder_sizes = !o.folder_sizes, "lone" => o.lone = !o.lone, "pane" => o.pane = !o.pane, "motion" => o.motion = !o.motion, _ => {} }
        st.cfg.save();
        apply_opts(ui, &st.cfg);
        ui.set_has_pane(st.cfg.opts.pane);
        build_view(st); push_rows(ui, st); push_status(ui, st); view::update_pane(ui, st); view::on_layout(ui, st);
    }); });
    ui.on_theme_pick(|i| { with(|ui, st| { st.cfg.opts.theme = if i == 1 { "light" } else { "dark" }.into(); st.cfg.save(); apply_opts(ui, &st.cfg); }); });
    ui.on_pane_toggle(|| { with(|ui, st| { st.cfg.opts.pane = !st.cfg.opts.pane; st.cfg.save(); ui.set_has_pane(st.cfg.opts.pane); view::update_pane(ui, st); view::on_layout(ui, st); }); });
    ui.on_pane_action(|a| { with(|ui, st| view::pane_action(ui, st, a.as_str())); });
    ui.on_view_pick(|i| { with(|ui, st| { let k = st.cwd.to_lowercase(); match i { 1 => { st.cfg.views.insert(k, "rows".into()); } 2 => { st.cfg.views.insert(k, "tiles".into()); } _ => { st.cfg.views.remove(&k); } } st.cfg.save(); push_rows(ui, st); ui.set_list_y(0.0); }); });
    ui.on_size_pick(|i| { with(|ui, st| { st.cfg.opts.tile = ["s", "m", "l"][(i as usize).min(2)].into(); st.cfg.save(); push_rows(ui, st); }); });
    ui.on_sort_pick(|i| { with(|ui, st| { st.sort_key = i; st.sort_dir = if i == 1 || i == 3 { -1 } else { 1 }; build_view(st); push_rows(ui, st); push_chrome(ui, st); }); });
    ui.on_ed_mouse(|k, x, y, sh| { with(|ui, st| ed::mouse(ui, st, k, x, y, sh)); });
    ui.on_ed_scroll(|dy, dx| { with(|ui, st| ed::scroll(ui, st, dy, dx)); });
    ui.on_ed_resized(|| { with(|ui, st| { if ed::is_editing(st) { ed::changed(ui, st); } }); });
    ui.on_side_remove(|i| { with(|ui, st| { if let Some(r) = ui.get_side().row_data(i as usize) {
        if r.kind == 1 { st.cfg.pins.retain(|p| !eq(p, r.path.as_str())); } else if r.kind == 2 { st.cfg.aliases.retain(|a| !(eq(&a.path, r.path.as_str()) && a.name == r.label.as_str())); }
        st.cfg.save(); push_chrome(ui, st);
    } }); });
    ui.on_side_toggle(|| { with(|ui, st| { st.cfg.opts.aliases_open = !st.cfg.opts.aliases_open; st.cfg.save(); push_chrome(ui, st); }); });
    ui.on_side_add(|k| { with(|ui, st| { if k == "pin" { let p = st.cwd.clone(); toggle_pin(ui, st, &p); } else { add_alias(ui, st); } }); });
    ui.on_row_drag(|i, dy| { with(|ui, st| ops::row_drag(ui, st, i as usize, dy)); });
    ui.on_row_drop(|i, dy| { with(|ui, st| ops::row_drop(ui, st, i as usize, dy)); });
    ui.on_ed_suggest_accept(|| { with(|ui, st| ed::accept_suggest(ui, st)); });
    ui.on_ed_lang_pick(|i| { with(|ui, st| ed::set_lang(ui, st, i as usize)); });
    ui.set_ed_langs(ModelRc::new(VecModel::from(syntax::all().iter().map(|l| SharedString::from(l.name)).collect::<Vec<_>>())));
    ui.on_ed_save(|| { with(|ui, st| ed::save(ui, st)); });
    ui.on_ed_eol_toggle(|| { with(|ui, st| ed::toggle_eol(ui, st)); });
    ui.on_ed_find_edited(|t| { with(|ui, st| ed::find_edited(ui, st, t.as_str())); });
    ui.on_ed_find_key(|t, sh| with(|ui, st| ed::find_key(ui, st, t.as_str(), sh)).unwrap_or(false));
    ui.on_row_context(|i| { with(|ui, st| { if (i as usize) < st.view.len() && !st.sel.contains(&key_at(st, i as usize)) { select_row(ui, st, i as usize, false, false); } }); });
    ui.on_ctx_action(|a| { with(|ui, st| ops::ctx_action(ui, st, a.as_str())); });
    ui.on_rename_accept(|| { with(|ui, st| ops::rename_accept(ui, st)); });
    ui.on_rename_cancel(|| { with(|ui, st| ops::rename_cancel(ui, st)); });
    ui.on_filter_edited(|t| { with(|ui, st| { st.filter = t.to_string(); build_view(st); let keep: HashSet<String> = st.view.iter().map(|&i| st.entries[i].k.clone()).collect(); st.sel.retain(|k| keep.contains(k)); st.cur = None; push_rows(ui, st); ui.set_list_y(0.0); push_status(ui, st); }); });
    ui.on_filter_done(|| { with(|ui, _| ui.invoke_focus_list()); });

    with(|ui, st| { go(ui, st, &start, Hist::None, None); });

    if let Some(script) = arg("--script") { testing::run(&ui, &script); }
    {
        use slint::winit_030::WinitWindowAccessor;
        ui.show().unwrap();
        #[cfg(windows)]
        ui.window().with_winit_window(|w| { use slint::winit_030::winit::platform::windows::WindowExtWindows; w.set_undecorated_shadow(true); });
    }
    ui.run().unwrap();
}
