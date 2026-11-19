//! The conversation the popup shows, as plain data: a flat list of blocks
//! (a prompt, a tool line, a permission card, the reply, a note), each in a
//! group. A group is one top-level item: an exchange, a divider, or a card
//! with no exchange to sit in. The view mirrors the list through the `Op`s
//! taken from it, so it never has to diff anything.

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Prompt,
    Reply,
    Tool,
    Note,
    Error,
    Ask,
}

/// A permission card's own parts.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Card {
    /// The question it asks (`PermissionAsk::id`).
    #[serde(rename = "askId")]
    pub ask_id: String,
    pub action: String,
    pub detail: String,
    /// The answers offered, as "ALLOW,ALWAYS,DENY"; empty once answered.
    pub buttons: String,
    pub answered: bool,
    /// What became of it, once answered.
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Block {
    /// (`key` and `shown` in JSON: QML has its own `id` and `visible`.)
    #[serde(rename = "key")]
    pub id: u64,
    pub group: u64,
    pub kind: Kind,
    pub text: String,
    #[serde(rename = "shown")]
    pub visible: bool,
    /// Space above it: none for the first shown, a little within a group,
    /// more between groups. Hidden blocks don't count.
    pub gap: u32,
    #[serde(flatten)]
    pub card: Card,
}

/// A change to the list, for the view to make to its copy.
#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    Insert(usize, Block),
    Update(usize, Block),
    Remove(usize, usize),
    Clear,
}

/// Spacing within a group and between groups, in pixels.
pub const GAP_IN: u32 = 6;
pub const GAP_BETWEEN: u32 = 16;

#[derive(Default)]
pub struct History {
    blocks: Vec<Block>,
    next: u64,
    ops: Vec<Op>,
}

impl History {
    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    pub fn new_group(&mut self) -> u64 {
        self.next += 1;
        self.next
    }

    fn block(&mut self, group: u64, kind: Kind, text: &str) -> Block {
        self.next += 1;
        Block { id: self.next, group, kind, text: text.to_string(), visible: true, gap: 0, card: Card::default() }
    }

    fn index(&self, id: u64) -> Option<usize> {
        self.blocks.iter().position(|b| b.id == id)
    }

    #[cfg(test)]
    pub fn get(&self, id: u64) -> Option<&Block> {
        self.blocks.iter().find(|b| b.id == id)
    }

    fn insert_at(&mut self, i: usize, b: Block) -> u64 {
        let id = b.id;
        self.blocks.insert(i, b.clone());
        self.ops.push(Op::Insert(i, b));
        self.regap();
        id
    }

    /// A new top-level item at the end: a group of its own.
    pub fn push_item(&mut self, kind: Kind, text: &str) -> u64 {
        let group = self.new_group();
        self.push(group, kind, text)
    }

    /// Add to the end of the list, in `group`.
    pub fn push(&mut self, group: u64, kind: Kind, text: &str) -> u64 {
        let b = self.block(group, kind, text);
        self.insert_at(self.blocks.len(), b)
    }

    /// Add as the last block of `group` (which may not be the last group:
    /// a divider can have come after it). Nothing if the group is gone
    /// (cleared, or trimmed away).
    pub fn append_to_group(&mut self, group: u64, kind: Kind, text: &str) -> Option<u64> {
        let at = self.blocks.iter().rposition(|x| x.group == group)? + 1;
        let b = self.block(group, kind, text);
        Some(self.insert_at(at, b))
    }

    /// Add just before block `before`, in its group. Nothing if it's gone.
    pub fn insert_before(&mut self, before: u64, kind: Kind, text: &str) -> Option<u64> {
        let i = self.index(before)?;
        let b = self.block(self.blocks[i].group, kind, text);
        Some(self.insert_at(i, b))
    }

    /// A permission card before block `before` (its exchange's reply), or as
    /// an item of its own when there's no such block: a question is never
    /// left out of sight.
    pub fn insert_card(&mut self, before: Option<u64>, title: &str, card: Card) -> u64 {
        let id = match before.and_then(|b| self.insert_before(b, Kind::Ask, title)) {
            Some(id) => id,
            None => self.push_item(Kind::Ask, title),
        };
        self.update(id, |b| b.card = card);
        id
    }

    /// Change block `id` in place (nothing if it's gone).
    pub fn update(&mut self, id: u64, f: impl FnOnce(&mut Block)) {
        let Some(i) = self.index(id) else { return };
        let before = self.blocks[i].clone();
        f(&mut self.blocks[i]);
        if self.blocks[i] != before {
            let shown_changed = self.blocks[i].visible != before.visible;
            self.ops.push(Op::Update(i, self.blocks[i].clone()));
            if shown_changed {
                self.regap();
            }
        }
    }

    pub fn set_text(&mut self, id: u64, text: &str) {
        self.update(id, |b| {
            if b.text != text {
                b.text = text.to_string();
            }
        });
    }

    pub fn set_visible(&mut self, id: u64, on: bool) {
        self.update(id, |b| b.visible = on);
    }

    pub fn remove(&mut self, id: u64) {
        if let Some(i) = self.index(id) {
            self.blocks.remove(i);
            self.ops.push(Op::Remove(i, 1));
            self.regap();
        }
    }

    pub fn clear(&mut self) {
        if !self.blocks.is_empty() {
            self.blocks.clear();
            self.ops.push(Op::Clear);
        }
    }

    /// Drop the oldest items until at most `max` are left.
    pub fn trim(&mut self, max: usize) {
        let mut groups: Vec<u64> = Vec::new();
        for b in &self.blocks {
            if groups.last() != Some(&b.group) && !groups.contains(&b.group) {
                groups.push(b.group);
            }
        }
        if groups.len() <= max {
            return;
        }
        let drop = &groups[..groups.len() - max];
        let n = self.blocks.iter().take_while(|b| drop.contains(&b.group)).count();
        if n > 0 {
            self.blocks.drain(..n);
            self.ops.push(Op::Remove(0, n));
            self.regap();
        }
    }

    /// Recompute each block's gap, reporting those that changed.
    fn regap(&mut self) {
        let mut prev: Option<u64> = None;
        for i in 0..self.blocks.len() {
            let b = &self.blocks[i];
            let gap = match prev {
                None => 0,
                Some(g) if g == b.group => GAP_IN,
                Some(_) => GAP_BETWEEN,
            };
            if b.visible {
                prev = Some(b.group);
            }
            if self.blocks[i].gap != gap {
                self.blocks[i].gap = gap;
                self.ops.push(Op::Update(i, self.blocks[i].clone()));
            }
        }
    }

    pub fn take_ops(&mut self) -> Vec<Op> {
        std::mem::take(&mut self.ops)
    }

    /// The questions and replies shown, oldest first, one paragraph each.
    pub fn text(&self) -> String {
        let mut parts = Vec::new();
        for b in self.blocks.iter().filter(|b| b.visible) {
            let text = b.text.trim();
            match b.kind {
                // The mic/keyboard mark is an icon-font glyph; plain text gets "›".
                Kind::Prompt => parts.push(format!("› {}", text.split_once(' ').map_or(text, |(_, q)| q))),
                Kind::Reply if !text.is_empty() => parts.push(text.to_string()),
                _ => {}
            }
        }
        parts.join("\n\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Apply `ops` to a copy, as the view does.
    fn apply(view: &mut Vec<Block>, ops: Vec<Op>) {
        for op in ops {
            match op {
                Op::Insert(i, b) => view.insert(i, b),
                Op::Update(i, b) => view[i] = b,
                Op::Remove(i, n) => {
                    view.drain(i..i + n);
                }
                Op::Clear => view.clear(),
            }
        }
    }

    #[test]
    fn the_view_stays_in_step() {
        let mut h = History::default();
        let mut view = Vec::new();
        let g = h.new_group();
        let prompt = h.push(g, Kind::Prompt, "› hello");
        let reply = h.push(g, Kind::Reply, "");
        h.set_visible(reply, false);
        let tool = h.insert_before(reply, Kind::Tool, "SEARCH").unwrap();
        h.push_item(Kind::Note, "— NEW SESSION —");
        // A note for the exchange goes into it, not after the divider.
        h.append_to_group(g, Kind::Note, "— STOPPED");
        h.set_text(reply, "Hi.");
        h.set_visible(reply, true);
        h.remove(tool);
        apply(&mut view, h.take_ops());
        assert_eq!(view, h.blocks);
        let kinds: Vec<Kind> = view.iter().map(|b| b.kind).collect();
        assert_eq!(kinds, [Kind::Prompt, Kind::Reply, Kind::Note, Kind::Note]);
        let gaps: Vec<u32> = view.iter().map(|b| b.gap).collect();
        assert_eq!(gaps, [0, GAP_IN, GAP_IN, GAP_BETWEEN]);
        assert_eq!(view[0].id, prompt);
    }

    #[test]
    fn lines_for_a_cleared_exchange_go_nowhere() {
        let mut h = History::default();
        let g = h.new_group();
        let reply = h.push(g, Kind::Reply, "");
        h.clear();
        assert_eq!(h.insert_before(reply, Kind::Tool, "SEARCH"), None);
        assert_eq!(h.append_to_group(g, Kind::Note, "— STOPPED"), None);
        assert!(h.is_empty());
        // A question still shows, on its own.
        h.insert_card(Some(reply), "BASH · ALLOW?", Card::default());
        assert_eq!(h.blocks.len(), 1);
    }

    #[test]
    fn hidden_blocks_take_no_gap() {
        let mut h = History::default();
        let g = h.new_group();
        let reply = h.push(g, Kind::Reply, "");
        h.set_visible(reply, false);
        let note = h.push(g, Kind::Note, "— NO REPLY");
        assert_eq!(h.get(note).unwrap().gap, 0);
        h.set_visible(reply, true);
        assert_eq!(h.get(note).unwrap().gap, GAP_IN);
    }

    #[test]
    fn trim_drops_whole_old_items() {
        let mut h = History::default();
        let mut view = Vec::new();
        for i in 0..5 {
            let g = h.new_group();
            h.push(g, Kind::Prompt, &format!("› q{i}"));
            h.push(g, Kind::Reply, &format!("a{i}"));
        }
        h.trim(3);
        apply(&mut view, h.take_ops());
        assert_eq!(view, h.blocks);
        assert_eq!(view.len(), 6);
        assert_eq!(view[0].text, "› q2");
        assert_eq!(view[0].gap, 0);
    }

    #[test]
    fn copy_text_has_questions_and_replies() {
        let mut h = History::default();
        let g = h.new_group();
        h.push(g, Kind::Prompt, "󰍬 what time is it");
        h.push(g, Kind::Tool, "CLOCK");
        h.push(g, Kind::Reply, "Noon.");
        let hidden = h.push(g, Kind::Reply, "draft");
        h.set_visible(hidden, false);
        h.push_item(Kind::Note, "— NEW SESSION —");
        assert_eq!(h.text(), "› what time is it\n\nNoon.");
    }

    #[test]
    fn cards_sit_before_the_reply_or_alone() {
        let mut h = History::default();
        let g = h.new_group();
        let reply = h.push(g, Kind::Reply, "");
        let card = Card { ask_id: "1".into(), buttons: "ALLOW,DENY".into(), ..Default::default() };
        let c = h.insert_card(Some(reply), "BASH · ALLOW?", card.clone());
        assert_eq!(h.blocks[0].id, c);
        assert_eq!(h.blocks[0].group, g);
        let lone = h.insert_card(None, "BASH · ALLOW?", card);
        assert_ne!(h.get(lone).unwrap().group, g);
        let json = serde_json::to_value(h.get(c).unwrap()).unwrap();
        assert_eq!(json["askId"], "1");
        assert_eq!(json["shown"], true);
        assert!(json["key"].is_u64());
        assert_eq!(json["kind"], "ask");
    }
}
