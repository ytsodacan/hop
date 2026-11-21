//! The built-in text editor: a plain Rust text buffer with its own caret, selection and undo, drawn line by line
//! by the UI as coloured runs. The disk side (reading, saving with line endings and BOM kept) is core::editor.
use crate::core::editor as eio;
use crate::syntax;
use crate::util::*;
use crate::{post, App, EdLine, Run, State};
use slint::{ComponentHandle, ModelRc, SharedString, VecModel};

pub const LH: f32 = 20.0;
const MAX_HL_LINES: usize = 20_000;
type Pos = (usize, usize); // (line, column in characters)

struct Rec { line: usize, old: Vec<String>, new_len: usize, caret: Pos, anchor: Option<Pos>, group: u32 }

pub struct EditBuf {
    pub path: String,
    pub name: String,
    pub lines: Vec<String>,
    pub crlf: bool,
    pub bom: bool,
    pub mtime: i64,
    pub lang: usize,
    pub ext_lang: usize,           // what the file name says
    pub manual_lang: bool,         // the user picked a language, stop guessing
    pub suggest: Option<usize>,    // what the text looks like, when that differs from the name
    pub detected_version: u64,
    pub unit: String,
    pub caret: Pos,
    pub anchor: Option<Pos>,
    pub want_col: usize,
    pub top: usize,
    pub left: usize,                 // horizontal scroll, in columns
    undo: Vec<Rec>,
    redo: Vec<Rec>,
    last_edit: i64,
    group: u32,
    pub version: u64,
    pub saved_version: u64,
    states: Vec<&'static str>,
    toks: Vec<Vec<(String, u8)>>,
    pub find: String,
    pub find_hit: Option<(usize, usize, usize)>, // line, col, len
    pub find_total: usize,
    pub dragging: bool,
    pub force_save: bool,
}

fn byte_of(s: &str, col: usize) -> usize { s.char_indices().nth(col).map(|(i, _)| i).unwrap_or(s.len()) }
fn clen(s: &str) -> usize { s.chars().count() }
/// columns taken by the first `col` characters of a line, tabs expanding to the next multiple of 4
fn disp_col(s: &str, col: usize) -> usize { let mut x = 0; for c in s.chars().take(col) { x += if c == '\t' { 4 - x % 4 } else { 1 }; } x }
fn col_at_disp(s: &str, d: usize) -> usize {
    let mut x: usize = 0;
    for (i, c) in s.chars().enumerate() { let w = if c == '\t' { 4 - x % 4 } else { 1 }; if d < x + w.div_ceil(2) { return i; } x += w; }
    clen(s)
}
fn word_class(c: char) -> u8 { if c.is_alphanumeric() || c == '_' { 1 } else if c.is_whitespace() { 0 } else { 2 } }

impl EditBuf {
    pub fn open(path: &str) -> Result<EditBuf, String> {
        let r = eio::read_text(path.to_string())?;
        let lines: Vec<String> = r.text.split('\n').map(String::from).collect();
        let mut tabs = 0; let mut min = 99usize;
        for l in lines.iter().take(400) { if l.starts_with('\t') { tabs += 1; } else { let n = l.len() - l.trim_start_matches(' ').len(); if n > 0 && !l.trim().is_empty() { min = min.min(n); } } }
        let unit = if tabs > 3 { "\t".to_string() } else { " ".repeat(if min == 4 || min == 8 { 4 } else { 2 }) };
        let name = leaf(path).to_string();
        let ext_lang = syntax::for_file(&name);
        Ok(EditBuf { lang: ext_lang, ext_lang, manual_lang: false, suggest: None, detected_version: u64::MAX, path: path.to_string(), name, lines, crlf: r.crlf, bom: r.bom, mtime: r.mtime, unit, caret: (0, 0), anchor: None, want_col: 0, top: 0, left: 0,
            undo: vec![], redo: vec![], last_edit: 0, group: 0, version: 0, saved_version: 0, states: vec!["main"], toks: vec![], find: String::new(), find_hit: None, find_total: 0, dragging: false, force_save: false })
    }
    /// look at the text: highlight it as what it is, and remember the guess so the header can offer a rename
    pub fn run_detect(&mut self) {
        self.detected_version = self.version;
        if self.manual_lang { return; }
        let (lang, suggest) = match crate::detect::detect(&self.lines, self.ext_lang) { Some(d) => (d, Some(d)), None => (self.ext_lang, None) };
        if lang != self.lang { self.lang = lang; self.invalidate(0); }
        self.suggest = suggest;
    }
    pub fn dirty(&self) -> bool { self.version != self.saved_version }
    fn invalidate(&mut self, from: usize) { self.toks.truncate(from); self.states.truncate(from + 1); }
    fn ensure(&mut self, upto: usize) {
        let lang = &syntax::all()[self.lang];
        let hl = self.lang != 0 && self.lines.len() <= MAX_HL_LINES;
        let upto = upto.min(self.lines.len());
        while self.toks.len() < upto {
            let i = self.toks.len();
            let st = self.states[i];
            if hl { let (t, next) = syntax::line(lang, &self.lines[i], st); self.toks.push(t); self.states.push(syntax::intern(lang, next)); }
            else { self.toks.push(vec![(self.lines[i].clone(), 0)]); self.states.push("main"); }
        }
    }
    pub fn sel(&self) -> Option<(Pos, Pos)> { self.anchor.filter(|a| *a != self.caret).map(|a| if a < self.caret { (a, self.caret) } else { (self.caret, a) }) }
    pub fn selected_text(&self) -> String { match self.sel() { Some((a, b)) => self.text_in(a, b), None => String::new() } }
    fn text_in(&self, a: Pos, b: Pos) -> String {
        if a.0 == b.0 { return self.lines[a.0].chars().skip(a.1).take(b.1 - a.1).collect(); }
        let mut s: String = self.lines[a.0].chars().skip(a.1).collect();
        for l in a.0 + 1..b.0 { s.push('\n'); s.push_str(&self.lines[l]); }
        s.push('\n'); s.extend(self.lines[b.0].chars().take(b.1)); s
    }

    /// the one way text changes: replace [a, b) with `text`. Undo records the lines it touched.
    fn replace(&mut self, a: Pos, b: Pos, text: &str) {
        let now = now_ms();
        if now - self.last_edit > 600 { self.group += 1; }
        self.last_edit = now;
        let old: Vec<String> = self.lines[a.0..=b.0].to_vec();
        let prefix: String = self.lines[a.0].chars().take(a.1).collect();
        let suffix: String = self.lines[b.0].chars().skip(b.1).collect();
        let joined = format!("{prefix}{text}{suffix}");
        let new_lines: Vec<String> = joined.split('\n').map(String::from).collect();
        self.undo.push(Rec { line: a.0, old, new_len: new_lines.len(), caret: self.caret, anchor: self.anchor, group: self.group });
        if self.undo.len() > 2000 { self.undo.remove(0); }
        self.redo.clear();
        let nl = new_lines.len();
        self.lines.splice(a.0..=b.0, new_lines);
        let added = text.matches('\n').count();
        self.caret = if added == 0 { (a.0, a.1 + clen(text)) } else { (a.0 + added, clen(text.rsplit('\n').next().unwrap_or(""))) };
        let _ = nl;
        self.anchor = None;
        self.want_col = disp_col(&self.lines[self.caret.0], self.caret.1);
        self.version += 1;
        self.invalidate(a.0);
    }

    fn apply(&mut self, rec: Rec, redo: bool) {
        let cur: Vec<String> = self.lines[rec.line..rec.line + rec.new_len].to_vec();
        let inverse = Rec { line: rec.line, old: cur, new_len: rec.old.len(), caret: self.caret, anchor: self.anchor, group: rec.group };
        let n = rec.old.len();
        self.lines.splice(rec.line..rec.line + rec.new_len, rec.old);
        let _ = n;
        self.caret = rec.caret; self.anchor = rec.anchor;
        self.want_col = disp_col(&self.lines[self.caret.0.min(self.lines.len() - 1)], self.caret.1);
        self.version += 1;
        self.invalidate(rec.line);
        if redo { self.undo.push(inverse); } else { self.redo.push(inverse); }
    }
    pub fn undo(&mut self) {
        let Some(first) = self.undo.pop() else { return };
        let g = first.group;
        self.apply(first, false);
        while self.undo.last().map(|r| r.group == g).unwrap_or(false) { let r = self.undo.pop().unwrap(); self.apply(r, false); }
        self.fix_caret();
    }
    pub fn redo(&mut self) {
        let Some(first) = self.redo.pop() else { return };
        let g = first.group;
        self.apply(first, true);
        while self.redo.last().map(|r| r.group == g).unwrap_or(false) { let r = self.redo.pop().unwrap(); self.apply(r, true); }
        self.fix_caret();
    }
    fn fix_caret(&mut self) {
        self.caret.0 = self.caret.0.min(self.lines.len() - 1);
        self.caret.1 = self.caret.1.min(clen(&self.lines[self.caret.0]));
        if let Some(a) = self.anchor { self.anchor = Some((a.0.min(self.lines.len() - 1), a.1.min(clen(&self.lines[a.0.min(self.lines.len() - 1)])))); }
    }

    // ---- editing
    pub fn insert(&mut self, t: &str) { let (a, b) = self.sel().unwrap_or((self.caret, self.caret)); self.replace(a, b, t); }
    pub fn backspace(&mut self) {
        if let Some((a, b)) = self.sel() { return self.replace(a, b, ""); }
        let (l, c) = self.caret;
        if c > 0 { self.replace((l, c - 1), (l, c), ""); } else if l > 0 { let pl = clen(&self.lines[l - 1]); self.replace((l - 1, pl), (l, 0), ""); }
    }
    pub fn delete(&mut self) {
        if let Some((a, b)) = self.sel() { return self.replace(a, b, ""); }
        let (l, c) = self.caret;
        if c < clen(&self.lines[l]) { self.replace((l, c), (l, c + 1), ""); } else if l + 1 < self.lines.len() { self.replace((l, c), (l + 1, 0), ""); }
    }
    pub fn newline(&mut self) {
        let (l, c) = self.sel().map(|s| s.0).unwrap_or(self.caret);
        let line = self.lines[l].clone();
        let before: String = line.chars().take(c).collect();
        let after: String = line.chars().skip(c).collect();
        let ind: String = before.chars().take_while(|ch| *ch == ' ' || *ch == '\t').collect();
        let last = before.trim_end().chars().last();
        let opens = matches!(last, Some('{') | Some('[') | Some('(')) || (last == Some(':') && matches!(syntax::all()[self.lang].name, "Python" | "YAML"));
        let close = match last { Some('{') => '}', Some('[') => ']', Some('(') => ')', _ => '\0' };
        let (a, b) = self.sel().unwrap_or(((l, c), (l, c)));
        if opens && after.trim_start().starts_with(close) && close != '\0' {
            self.replace(a, b, &format!("\n{ind}{}\n{ind}", self.unit));
            self.caret = (l + 1, clen(&ind) + clen(&self.unit)); self.want_col = disp_col(&self.lines[l + 1], self.caret.1);
        } else { self.replace(a, b, &format!("\n{ind}{}", if opens { self.unit.clone() } else { String::new() })); }
    }
    fn line_span(&self) -> (usize, usize) { match self.sel() { Some((a, b)) => (a.0, if b.1 == 0 && b.0 > a.0 { b.0 - 1 } else { b.0 }), None => (self.caret.0, self.caret.0) } }
    pub fn indent(&mut self, out: bool) {
        let (a, b) = self.line_span();
        if !out && self.sel().is_none() { let u = self.unit.clone(); return self.insert(&u); }
        let block: Vec<String> = self.lines[a..=b].iter().map(|l| if out { let n = self.unit.chars().count(); let strip = l.chars().take(n).take_while(|c| *c == ' ' || *c == '\t').count(); l.chars().skip(strip).collect() } else if l.is_empty() { l.clone() } else { format!("{}{}", self.unit, l) }).collect();
        let (caret, anchor) = (self.caret, self.anchor);
        let end = (b, clen(&self.lines[b]));
        self.replace((a, 0), end, &block.join("\n"));
        let delta = |p: Pos| -> Pos { if out { (p.0, p.1.saturating_sub(self.unit.chars().count())) } else { (p.0, p.1 + clen(&self.unit)) } };
        self.caret = delta(caret); self.anchor = anchor.map(delta);
    }
    pub fn toggle_comment(&mut self) {
        let Some(lc) = syntax::all()[self.lang].comment else { return };
        let (a, b) = self.line_span();
        let t = lc.trim();
        let all = self.lines[a..=b].iter().filter(|l| !l.trim().is_empty()).all(|l| l.trim_start().starts_with(t));
        let block: Vec<String> = self.lines[a..=b].iter().map(|l| if l.trim().is_empty() { l.clone() } else if all { l.replacen(&format!("{t} "), "", 1).replacen(t, "", 1) } else { let ind: String = l.chars().take_while(|c| c.is_whitespace()).collect(); format!("{ind}{lc}{}{}", if lc.ends_with(' ') { "" } else { " " }, &l[ind.len()..]) }).collect();
        let (caret, anchor) = (self.caret, self.anchor);
        self.replace((a, 0), (b, clen(&self.lines[b])), &block.join("\n"));
        self.caret = (caret.0, caret.1.min(clen(&self.lines[caret.0]))); self.anchor = anchor;
    }

    // ---- movement
    fn set_caret(&mut self, p: Pos, extend: bool, keep_want: bool) {
        if extend { if self.anchor.is_none() { self.anchor = Some(self.caret); } } else { self.anchor = None; }
        self.caret = p;
        if !keep_want { self.want_col = disp_col(&self.lines[p.0], p.1); }
    }
    pub fn move_h(&mut self, d: i32, extend: bool, word: bool) {
        if !extend { if let Some((a, b)) = self.sel() { return self.set_caret(if d < 0 { a } else { b }, false, false); } }
        let (mut l, mut c) = self.caret;
        if word {
            loop {
                if d < 0 {
                    if c == 0 { if l == 0 { break; } l -= 1; c = clen(&self.lines[l]); break; }
                    let ch: Vec<char> = self.lines[l].chars().collect();
                    while c > 0 && word_class(ch[c - 1]) == 0 { c -= 1; }
                    if c > 0 { let k = word_class(ch[c - 1]); while c > 0 && word_class(ch[c - 1]) == k { c -= 1; } }
                    break;
                } else {
                    let ch: Vec<char> = self.lines[l].chars().collect();
                    if c >= ch.len() { if l + 1 >= self.lines.len() { break; } l += 1; c = 0; break; }
                    let k = word_class(ch[c]);
                    while c < ch.len() && word_class(ch[c]) == k && k != 0 { c += 1; }
                    while c < ch.len() && word_class(ch[c]) == 0 { c += 1; }
                    break;
                }
            }
        } else if d < 0 { if c > 0 { c -= 1; } else if l > 0 { l -= 1; c = clen(&self.lines[l]); } }
        else if c < clen(&self.lines[l]) { c += 1; } else if l + 1 < self.lines.len() { l += 1; c = 0; }
        self.set_caret((l, c), extend, false);
    }
    pub fn move_v(&mut self, d: i32, extend: bool) {
        let l = (self.caret.0 as i64 + d as i64).clamp(0, self.lines.len() as i64 - 1) as usize;
        if l == self.caret.0 && d > 0 { let e = (l, clen(&self.lines[l])); return self.set_caret(e, extend, false); }
        if l == self.caret.0 && d < 0 { return self.set_caret((0, 0), extend, false); }
        let c = col_at_disp(&self.lines[l], self.want_col);
        self.set_caret((l, c), extend, true);
    }
    pub fn home(&mut self, extend: bool, doc: bool) {
        if doc { return self.set_caret((0, 0), extend, false); }
        let l = self.caret.0;
        let first = self.lines[l].chars().take_while(|c| *c == ' ' || *c == '\t').count();
        self.set_caret((l, if self.caret.1 == first { 0 } else { first }), extend, false);
    }
    pub fn end(&mut self, extend: bool, doc: bool) {
        if doc { let l = self.lines.len() - 1; return self.set_caret((l, clen(&self.lines[l])), extend, false); }
        let l = self.caret.0; self.set_caret((l, clen(&self.lines[l])), extend, false);
    }
    pub fn select_all(&mut self) { self.anchor = Some((0, 0)); let l = self.lines.len() - 1; self.caret = (l, clen(&self.lines[l])); }
    pub fn pos_at(&self, line: usize, disp: usize) -> Pos { let l = line.min(self.lines.len() - 1); (l, col_at_disp(&self.lines[l], disp)) }

    // ---- find
    pub fn find_update(&mut self, text: &str) {
        self.find = text.to_string();
        let q = text.to_lowercase();
        self.find_total = if q.is_empty() { 0 } else { self.lines.iter().map(|l| l.to_lowercase().matches(&q).count()).sum() };
        self.find_step(0);
    }
    pub fn find_step(&mut self, dir: i32) {
        let q = self.find.to_lowercase();
        if q.is_empty() { self.find_hit = None; return; }
        let n = self.lines.len();
        let (sl, sc) = match (dir, self.find_hit) { (1, Some((l, c, len))) => (l, c + len), (-1, Some((l, c, _))) => (l, c), (_, Some((l, c, _))) => (l, c), _ => self.sel().map(|s| s.0).unwrap_or(self.caret) };
        let order: Vec<usize> = if dir >= 0 { (0..=n).map(|i| (sl + i) % n).collect() } else { (0..=n).map(|i| (sl + n - i % n) % n).collect() };
        for (k, &l) in order.iter().enumerate() {
            let low = self.lines[l].to_lowercase();
            let chars: Vec<(usize, char)> = low.char_indices().collect();
            let cols: Vec<usize> = { let mut v = Vec::new(); let mut from = 0; while let Some(i) = low[from..].find(&q) { let b = from + i; v.push(chars.iter().position(|(bi, _)| *bi == b).unwrap_or(0)); from = b + q.len().max(1); } v };
            let pick = if dir >= 0 { cols.iter().copied().find(|&c| k > 0 || c >= sc) } else { cols.iter().rev().copied().find(|&c| k > 0 || c < sc) };
            if let Some(c) = pick { let len = clen(&q); self.find_hit = Some((l, c, len)); self.anchor = Some((l, c)); self.caret = (l, c + len); return; }
        }
        self.find_hit = None;
    }

    pub fn save(&mut self) -> Result<(), String> {
        let text = self.lines.join("\n");
        let exp = if self.force_save { None } else { Some(self.mtime) };
        match eio::write_text(self.path.clone(), text, self.crlf, self.bom, exp) {
            Ok(s) => { self.mtime = s.mtime; self.saved_version = self.version; self.force_save = false; Ok(()) }
            Err(e) => Err(e),
        }
    }

    fn lines_for_ui(&mut self, first: usize, count: usize, cw: f32) -> Vec<EdLine> {
        let _ = cw;
        let last = (first + count).min(self.lines.len());
        self.ensure(last);
        let sel = self.sel();
        (first..last).map(|i| {
            // tokens -> runs with tabs expanded to spaces so monospace columns line up
            let mut x = 0usize;
            let runs: Vec<Run> = self.toks[i].iter().map(|(t, c)| {
                let mut s = String::with_capacity(t.len());
                for ch in t.chars() { if ch == '\t' { let w = 4 - x % 4; s.push_str(&" ".repeat(w)); x += w; } else { s.push(ch); x += 1; } }
                Run { text: s.into(), kind: *c as i32 }
            }).collect();
            let (mut sa, mut sn) = (0i32, 0i32);
            if let Some((a, b)) = sel {
                if i >= a.0 && i <= b.0 {
                    let s0 = if i == a.0 { disp_col(&self.lines[i], a.1) } else { 0 };
                    let s1 = if i == b.0 { disp_col(&self.lines[i], b.1) } else { disp_col(&self.lines[i], clen(&self.lines[i])) + 1 };
                    sa = s0 as i32; sn = (s1.saturating_sub(s0)) as i32;
                }
            }
            let (mut ma, mut mn) = (0i32, 0i32);
            if let Some((l, c, len)) = self.find_hit { if l == i { let s0 = disp_col(&self.lines[i], c); ma = s0 as i32; mn = (disp_col(&self.lines[i], c + len) - s0) as i32; } }
            EdLine { tint: match self.toks[i].first().map(|t| t.1) { Some(11) => 1, Some(12) => 2, _ => 0 }, number: (i + 1).to_string().into(), runs: ModelRc::new(VecModel::from(runs)), current: i == self.caret.0, sel_a: sa, sel_n: sn, match_a: ma, match_n: mn }
        }).collect()
    }
}

// ------------------------------------------------------------------- hooking the buffer up to the window

pub fn is_editing(st: &State) -> bool { st.tabs[st.tab].ed.is_some() }

fn cur<'a>(st: &'a mut State) -> Option<&'a mut EditBuf> { let t = st.tab; st.tabs[t].ed.as_deref_mut() }

pub fn open(ui: &App, st: &mut State, path: &str) -> Result<(), String> {
    if let Some(i) = st.tabs.iter().position(|t| t.ed.as_ref().map(|e| eq(&e.path, path)).unwrap_or(false)) { st.tab = i; activate(ui, st); return Ok(()); }
    let mut ed = EditBuf::open(path)?;
    ed.run_detect();
    let at = st.tab + 1;
    st.tabs.insert(at, crate::Tab { path: path.to_string(), back: vec![], fwd: vec![], ed: Some(Box::new(ed)) });
    st.tab = at;
    st.cfg.bump(&parent(path).unwrap_or_default());
    activate(ui, st);
    Ok(())
}

/// the active tab is an editor: show it
pub fn activate(ui: &App, st: &mut State) {
    if let Some(p) = parent(&st.tabs[st.tab].path) { st.cwd = p; }
    ui.set_editing(true);
    ui.set_has_pane(false);
    crate::push_chrome(ui, st);
    push(ui, st);
    ui.invoke_focus_list();
}

pub fn leave(ui: &App, st: &mut State) { ui.set_editing(false); ui.set_has_pane(st.cfg.opts.pane); }

fn reveal(ui: &App, ed: &mut EditBuf) {
    let vis = ((ui.get_ed_body_h() / LH) as usize).max(1);
    if ed.caret.0 < ed.top { ed.top = ed.caret.0; } else if ed.caret.0 >= ed.top + vis { ed.top = ed.caret.0 + 1 - vis; }
    let cw = ui.get_ed_char_w().max(1.0);
    let cols = ((ui.get_ed_body_w() - 56.0 - 24.0) / cw) as usize;
    let d = disp_col(&ed.lines[ed.caret.0], ed.caret.1);
    if d < ed.left { ed.left = d.saturating_sub(4); } else if cols > 0 && d >= ed.left + cols { ed.left = d + 4 - cols; }
}

/// redraw the visible lines, the caret and the header
pub fn push(ui: &App, st: &mut State) {
    let t = st.tab;
    let Some(ed) = st.tabs[t].ed.as_deref_mut() else { return };
    let cw = ui.get_ed_char_w().max(1.0);
    let vis = ((ui.get_ed_body_h() / LH) as usize).max(1) + 1;
    ed.top = ed.top.min(ed.lines.len().saturating_sub(1));
    let lines = ed.lines_for_ui(ed.top, vis, cw);
    ui.set_ed_lines(ModelRc::new(VecModel::from(lines)));
    ui.set_ed_name(ed.name.as_str().into());
    ui.set_ed_dirty(ed.dirty());
    ui.set_ed_lang(syntax::all()[ed.lang].name.into());
    let sg = ed.suggest.and_then(|d| { let l = &syntax::all()[d]; let ext = ed.name.rsplit_once('.').map(|(_, e)| e.to_lowercase()).unwrap_or_default(); l.exts.first().filter(|_| !l.exts.contains(&ext.as_str())).map(|e| format!("Looks like {} · rename to .{}", l.name, e)) });
    ui.set_ed_suggest(sg.unwrap_or_default().into());
    ui.set_ed_eol((if ed.crlf { "CRLF" } else { "LF" }).into());
    let dc = disp_col(&ed.lines[ed.caret.0], ed.caret.1);
    ui.set_ed_caret_on(ed.caret.0 >= ed.top && ed.caret.0 < ed.top + vis && ed.sel().is_none());
    ui.set_ed_caret_x((dc as f32 - ed.left as f32) * cw);
    ui.set_ed_caret_y((ed.caret.0 as f32 - ed.top as f32) * LH);
    ui.set_ed_left_px(ed.left as f32 * cw);
    ui.set_ed_find_count(if ed.find.is_empty() { SharedString::new() } else if ed.find_total == 0 { "No results".into() } else { format!("{} found", ed.find_total).into() });
    let total = ed.lines.len().max(1) as f32;
    let h = ui.get_ed_body_h().max(1.0);
    ui.set_ed_sb_h((((vis - 1) as f32 / total).min(1.0) * h).max(24.0));
    ui.set_ed_sb_y(ed.top as f32 / total * h);
    ui.set_ed_sb_on(total > (vis - 1) as f32);
    ui.set_ed_pos(format!("Ln {}, Col {}", ed.caret.0 + 1, ed.caret.1 + 1).into());
    ui.set_status_left(format!("Ln {}, Col {}{}", ed.caret.0 + 1, ed.caret.1 + 1, ed.sel().map(|_| format!("  ·  {} selected", ed.selected_text().chars().count())).unwrap_or_default()).into());
    ui.set_status_right(format!("{} lines · UTF-8 · {} · {}{}", ed.lines.len(), if ed.crlf { "CRLF" } else { "LF" }, syntax::all()[ed.lang].name, if ed.lines.len() > MAX_HL_LINES { " · highlighting off (large file)" } else { "" }).into());
    ui.invoke_reset_blink();
}

thread_local! { static DETECT: slint::Timer = slint::Timer::default(); }

pub fn changed(ui: &App, st: &mut State) {
    let t = st.tab;
    if let Some(ed) = st.tabs[t].ed.as_deref_mut() {
        reveal(ui, ed);
        if ed.version != ed.detected_version && !ed.manual_lang {
            DETECT.with(|tm| tm.start(slint::TimerMode::SingleShot, std::time::Duration::from_millis(700), || post(|ui, st| detect_now(ui, st))));
        }
    }
    crate::push_chrome_tabs(ui, st);
    push(ui, st);
}

#[cfg(windows)]
fn clip_get() -> String { clipboard_win::get_clipboard_string().unwrap_or_default() }
#[cfg(windows)]
fn clip_set(s: &str) { let _ = clipboard_win::set_clipboard_string(s); }
#[cfg(not(windows))]
fn clip_get() -> String { arboard::Clipboard::new().ok().and_then(|mut c| c.get_text().ok()).unwrap_or_default() }
#[cfg(not(windows))]
fn clip_set(s: &str) { if let Ok(mut c) = arboard::Clipboard::new() { let _ = c.set_text(s); } }

pub fn save(ui: &App, st: &mut State) {
    let t = st.tab;
    let Some(ed) = st.tabs[t].ed.as_deref_mut() else { return };
    if !ed.dirty() { return; }
    match ed.save() {
        Ok(()) => { let n = ed.name.clone(); crate::ops::notify(ui, st, &format!("Saved {n}")); }
        Err(e) if e == "CHANGED" => { ed.force_save = true; crate::ops::notify(ui, st, "Changed on disk since you opened it. Press Ctrl+S again to overwrite."); }
        Err(e) => crate::ops::notify(ui, st, &format!("Couldn't save: {e}")),
    }
    changed(ui, st);
}

pub fn key(ui: &App, st: &mut State, text: &str, ctrl: bool, shift: bool, alt: bool) -> bool {
    use slint::platform::Key;
    let is = |k: Key| text == SharedString::from(k).as_str();
    let Some(ed) = cur(st) else { return false };
    let mut edited = true;
    if ctrl && !alt {
        match text {
            "s" => { save(ui, st); return true; }
            "f" => { ui.set_find_open(true); return true; }
            "a" => ed.select_all(),
            "c" => { let s = ed.selected_text(); if !s.is_empty() { clip_set(&s); } edited = false; }
            "x" => { let s = ed.selected_text(); if !s.is_empty() { clip_set(&s); ed.backspace(); } }
            "v" => { let s = clip_get().replace("\r\n", "\n"); if !s.is_empty() { ed.insert(&s); } }
            "z" => if shift { ed.redo() } else { ed.undo() },
            "y" => ed.redo(),
            "/" => ed.toggle_comment(),
            _ => if is(Key::LeftArrow) { ed.move_h(-1, shift, true) } else if is(Key::RightArrow) { ed.move_h(1, shift, true) } else if is(Key::Home) { ed.home(shift, true) } else if is(Key::End) { ed.end(shift, true) } else if is(Key::Backspace) { let c = ed.caret; ed.move_h(-1, true, true); if ed.sel().is_some() { ed.backspace(); } else { ed.caret = c; } } else { return false },
        }
    } else if alt { return false }
    else if is(Key::LeftArrow) { ed.move_h(-1, shift, false) } else if is(Key::RightArrow) { ed.move_h(1, shift, false) }
    else if is(Key::UpArrow) { ed.move_v(-1, shift) } else if is(Key::DownArrow) { ed.move_v(1, shift) }
    else if is(Key::PageUp) { let n = ((ui.get_ed_body_h() / LH) as i32 - 1).max(1); ed.move_v(-n, shift); ed.top = ed.top.saturating_sub(n as usize) }
    else if is(Key::PageDown) { let n = ((ui.get_ed_body_h() / LH) as i32 - 1).max(1); ed.move_v(n, shift); ed.top += n as usize }
    else if is(Key::Home) { ed.home(shift, false) } else if is(Key::End) { ed.end(shift, false) }
    else if is(Key::Return) { ed.newline() } else if is(Key::Backspace) { ed.backspace() } else if is(Key::Delete) { ed.delete() }
    else if is(Key::Tab) { ed.indent(shift) } else if is(Key::Backtab) { ed.indent(true) }
    else if is(Key::Escape) { if ui.get_find_open() { ui.set_find_open(false); } else { ed.anchor = None; } edited = false; }
    else if text.chars().count() == 1 && !text.chars().next().unwrap().is_control() && !is(Key::Delete) { ed.insert(text) }
    else { return false; }
    let _ = edited;
    changed(ui, st);
    true
}

/// kind: 0 press, 1 drag, 2 release. x/y are relative to the text area.
pub fn mouse(ui: &App, st: &mut State, kind: i32, x: f32, y: f32, shift: bool) {
    let cw = ui.get_ed_char_w().max(1.0);
    let Some(ed) = cur(st) else { return };
    let line = ed.top + (y.max(0.0) / LH) as usize;
    let disp = (((x + ed.left as f32 * cw) / cw) + 0.0).max(0.0).round() as usize;
    let p = ed.pos_at(line, disp);
    match kind {
        0 => { ed.dragging = true; if shift { if ed.anchor.is_none() { ed.anchor = Some(ed.caret); } } else { ed.anchor = Some(p); } ed.caret = p; ed.want_col = disp_col(&ed.lines[p.0], p.1); }
        1 if ed.dragging => { if ed.anchor.is_none() { ed.anchor = Some(ed.caret); } ed.caret = p; ed.want_col = disp_col(&ed.lines[p.0], p.1); }
        2 => { ed.dragging = false; if ed.anchor == Some(ed.caret) { ed.anchor = None; } }
        _ => return,
    }
    push(ui, st);
}

pub fn scroll(ui: &App, st: &mut State, dy: f32, dx: f32) {
    let Some(ed) = cur(st) else { return };
    let n = 3usize;
    if dy > 0.0 { ed.top = ed.top.saturating_sub(n); } else if dy < 0.0 { ed.top = (ed.top + n).min(ed.lines.len().saturating_sub(1)); }
    if dx > 0.0 { ed.left = ed.left.saturating_sub(4); } else if dx < 0.0 { ed.left += 4; }
    push(ui, st);
}

pub fn find_edited(ui: &App, st: &mut State, text: &str) { if let Some(ed) = cur(st) { ed.find_update(text); } changed(ui, st); }
pub fn find_key(ui: &App, st: &mut State, text: &str, shift: bool) -> bool {
    use slint::platform::Key;
    if text == SharedString::from(Key::Return).as_str() { if let Some(ed) = cur(st) { ed.find_step(if shift { -1 } else { 1 }); } changed(ui, st); return true; }
    if text == SharedString::from(Key::Escape).as_str() { ui.set_find_open(false); ui.invoke_focus_list(); return true; }
    false
}
fn detect_now(ui: &App, st: &mut State) {
    let t = st.tab;
    let Some(ed) = st.tabs[t].ed.as_deref_mut() else { return };
    let before = (ed.lang, ed.suggest);
    ed.run_detect();
    if before != (ed.lang, ed.suggest) { changed(ui, st); }
}

pub fn set_lang(ui: &App, st: &mut State, i: usize) { if let Some(ed) = cur(st) { if i < syntax::all().len() { ed.lang = i; ed.manual_lang = true; ed.suggest = None; ed.invalidate(0); } } changed(ui, st); }

/// the header offers to rename the file to match what is in it
pub fn accept_suggest(ui: &App, st: &mut State) {
    let t = st.tab;
    let Some(ed) = st.tabs[t].ed.as_deref_mut() else { return };
    let Some(d) = ed.suggest else { return };
    let Some(ext) = syntax::all()[d].exts.first() else { return };
    let stem = ed.name.rsplit_once('.').map(|(a, _)| a.to_string()).unwrap_or_else(|| ed.name.clone());
    let new_name = format!("{stem}.{ext}");
    let new_path = join(&parent(&ed.path).unwrap_or_default(), &new_name);
    if std::path::Path::new(&new_path).exists() { crate::ops::notify(ui, st, &format!("{new_name} already exists here")); return; }
    match std::fs::rename(&ed.path, &new_path) {
        Ok(()) => {
            ed.path = new_path.clone(); ed.name = new_name.clone(); ed.ext_lang = d; ed.lang = d; ed.suggest = None;
            st.tabs[t].path = new_path;
            crate::ops::notify(ui, st, &format!("Renamed to {new_name}"));
            crate::push_chrome(ui, st);
        }
        Err(e) => crate::ops::notify(ui, st, &format!("Couldn't rename: {e}")),
    }
    changed(ui, st);
}

pub fn toggle_eol(ui: &App, st: &mut State) { if let Some(ed) = cur(st) { ed.crlf = !ed.crlf; ed.version += 1; } changed(ui, st); }

pub fn _post_unused() { post(|_, _| {}); }
