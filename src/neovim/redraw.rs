//! `redraw` notifications (nvim_ui_attach) → `FromNeovim` messages

use rmpv::Value;

use super::mode::Mode;
use super::protocol::{CandidateInfo, FromNeovim, GridEvent};
use super::redraw_grid::parse_grid_event;

/// msg_show kinds that are not shown
const MSG_KIND_BLOCKLIST: &[&str] = &[
    "return_prompt", // Press ENTER prompt
    "search_count",  // [N/M] search count
    "quickfix",      // quickfix navigation
    "more_prompt",   // -- More -- prompt
];

/// State carried between redraw notifications
#[derive(Default)]
pub struct Redraw {
    /// Items of the shown popupmenu, for popupmenu_select
    popupmenu_items: Vec<String>,
    /// The shown popupmenu completes the command line
    popupmenu_cmdline: bool,
    /// Grid events of the current batch, sent as one message at `flush`
    grid_batch: Vec<GridEvent>,
}

impl Redraw {
    /// Handle the args of one redraw notification:
    /// `[["event_name", params_call1, params_call2, ...], ...]`
    pub fn handle(&mut self, args: &[Value], out: &mut Vec<FromNeovim>) {
        for event_group in args {
            let Some(arr) = event_group.as_array() else {
                continue;
            };
            let Some(event_name) = arr.first().and_then(|v| v.as_str()) else {
                continue;
            };
            for params in &arr[1..] {
                let msg = match event_name {
                    "cmdline_show" => cmdline_show(params),
                    "cmdline_pos" => cmdline_pos(params),
                    "cmdline_hide" => cmdline_hide(params),
                    "popupmenu_show" => Some(self.popupmenu_show(params)),
                    "popupmenu_select" => Some(self.popupmenu_select(params)),
                    "popupmenu_hide" => {
                        log::debug!("[NVIM] popupmenu_hide");
                        self.popupmenu_items.clear();
                        Some(FromNeovim::Candidates(CandidateInfo::empty()))
                    }
                    "msg_show" => msg_show(params),
                    "msg_showcmd" => msg_showcmd(params),
                    "msg_clear" => {
                        log::debug!("[NVIM] msg_clear");
                        Some(FromNeovim::CmdlineMessage {
                            text: String::new(),
                            cmdtype: String::new(),
                        })
                    }
                    "mode_change" => mode_change(params),
                    "flush" => {
                        let events = std::mem::take(&mut self.grid_batch);
                        (!events.is_empty()).then_some(FromNeovim::GridFlush(events))
                    }
                    _ => {
                        if let Some(event) = parse_grid_event(event_name, params) {
                            self.grid_batch.push(event);
                        } else {
                            log::trace!("[NVIM] Ignoring redraw event: {}", event_name);
                        }
                        None
                    }
                };
                out.extend(msg);
            }
        }
    }

    /// popupmenu_show: [items, selected, row, col, grid]
    /// items: [[word, kind, menu, info], ...]; grid = -1 for the command line
    fn popupmenu_show(&mut self, params: &Value) -> FromNeovim {
        let arr = params.as_array().map(Vec::as_slice).unwrap_or_default();
        let words: Vec<String> = arr
            .first()
            .and_then(Value::as_array)
            .map(|items| items.iter().map(item_text).collect())
            .unwrap_or_default();
        let selected = arr.get(1).and_then(Value::as_i64).unwrap_or(-1);
        let cmdline = arr.get(4).and_then(Value::as_i64) == Some(-1);
        log::debug!(
            "[NVIM] popupmenu_show: {} items, selected={}, cmdline={}",
            words.len(),
            selected,
            cmdline
        );
        self.popupmenu_items = words;
        self.popupmenu_cmdline = cmdline;
        self.candidates(selected)
    }

    /// popupmenu_select: [selected]
    fn popupmenu_select(&self, params: &Value) -> FromNeovim {
        let selected = params
            .as_array()
            .and_then(|a| a.first())
            .and_then(Value::as_i64)
            .unwrap_or(-1);
        log::trace!("[NVIM] popupmenu_select: selected={}", selected);
        self.candidates(selected)
    }

    /// Candidates from the cached items; selected = -1 means none
    fn candidates(&self, selected: i64) -> FromNeovim {
        let items = &self.popupmenu_items;
        if items.is_empty() {
            return FromNeovim::Candidates(CandidateInfo::empty());
        }
        let selected = usize::try_from(selected).ok().filter(|&i| i < items.len());
        FromNeovim::Candidates(CandidateInfo::new(
            items.clone(),
            selected,
            self.popupmenu_cmdline,
        ))
    }
}

/// Text of a popupmenu item: word, else menu, else kind
fn item_text(item: &Value) -> String {
    let Some(fields) = item.as_array() else {
        return String::new();
    };
    let field = |i: usize| fields.get(i).and_then(Value::as_str).unwrap_or("");
    [field(0), field(2), field(1)]
        .into_iter()
        .find(|s| !s.is_empty())
        .unwrap_or("")
        .to_string()
}

/// Concatenated text of `[[attr_id, text], ...]` chunks
fn chunks_text(chunks: &[Value]) -> String {
    chunks
        .iter()
        .filter_map(|chunk| chunk.as_array()?.get(1)?.as_str())
        .collect()
}

/// cmdline_show: [content, pos, firstc, prompt, indent, level]
fn cmdline_show(params: &Value) -> Option<FromNeovim> {
    let arr = params.as_array()?;
    if arr.len() < 6 {
        log::debug!("[NVIM] cmdline_show: expected 6 params, got {}", arr.len());
        return None;
    }
    let content = arr[0]
        .as_array()
        .map(|c| chunks_text(c))
        .unwrap_or_default();
    let pos = arr[1].as_u64().unwrap_or(0) as usize;
    let firstc = arr[2].as_str().unwrap_or("").to_string();
    let prompt = arr[3].as_str().unwrap_or("").to_string();
    let level = arr[5].as_u64().unwrap_or(1);
    log::debug!(
        "[NVIM] cmdline_show: firstc={:?}, prompt={:?}, content={:?}, pos={}, level={}",
        firstc,
        prompt,
        content,
        pos,
        level
    );
    Some(FromNeovim::CmdlineShow {
        content,
        pos,
        firstc,
        prompt,
        level,
    })
}

/// cmdline_pos: [pos, level]
fn cmdline_pos(params: &Value) -> Option<FromNeovim> {
    let arr = params.as_array()?;
    let pos = arr.first()?.as_u64()? as usize;
    let level = arr.get(1)?.as_u64()?;
    log::trace!("[NVIM] cmdline_pos: pos={}, level={}", pos, level);
    Some(FromNeovim::CmdlinePos { pos, level })
}

/// cmdline_hide: [level]
fn cmdline_hide(params: &Value) -> Option<FromNeovim> {
    let level = params
        .as_array()
        .and_then(|a| a.first())
        .and_then(Value::as_u64)
        .unwrap_or(1);
    log::debug!("[NVIM] cmdline_hide: level={}", level);
    Some(FromNeovim::CmdlineHide { level })
}

/// mode_change: [mode, mode_idx]
fn mode_change(params: &Value) -> Option<FromNeovim> {
    let name = params.as_array()?.first()?.as_str()?;
    log::debug!("[NVIM] mode_change: {:?}", name);
    Mode::from_ui(name).map(FromNeovim::ModeChange)
}

/// msg_showcmd: [content]
fn msg_showcmd(params: &Value) -> Option<FromNeovim> {
    let content = params.as_array()?.first()?.as_array()?;
    let text = chunks_text(content);
    log::trace!("[NVIM] msg_showcmd: {:?}", text);
    Some(FromNeovim::ShowCmd(text))
}

/// msg_show: [kind, content, replace_last]
fn msg_show(params: &Value) -> Option<FromNeovim> {
    let arr = params.as_array()?;
    let kind = arr.first()?.as_str().unwrap_or("");
    if MSG_KIND_BLOCKLIST.contains(&kind) {
        log::trace!("[NVIM] msg_show: blocked kind={:?}", kind);
        return None;
    }
    let text = chunks_text(arr.get(1)?.as_array()?);
    let text = text.trim();
    log::debug!("[NVIM] msg_show: kind={:?}, text={:?}", kind, text);
    if text.is_empty() {
        return None;
    }
    Some(FromNeovim::CmdlineMessage {
        text: text.to_string(),
        cmdtype: String::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arr(items: Vec<Value>) -> Value {
        Value::Array(items)
    }

    /// One redraw notification with a single event call
    fn redraw(redraw: &mut Redraw, name: &str, params: Vec<Value>) -> Vec<FromNeovim> {
        let mut out = Vec::new();
        redraw.handle(&[arr(vec![Value::from(name), arr(params)])], &mut out);
        out
    }

    fn item(word: &str) -> Value {
        arr(vec![
            Value::from(word),
            Value::from(""),
            Value::from(""),
            Value::from(""),
        ])
    }

    #[test]
    fn mode_change_is_classified() {
        let mut r = Redraw::default();
        assert_eq!(
            redraw(
                &mut r,
                "mode_change",
                vec![Value::from("cmdline_normal"), Value::from(4)]
            ),
            vec![FromNeovim::ModeChange(Mode::CmdLine)]
        );
        assert_eq!(
            redraw(
                &mut r,
                "mode_change",
                vec![Value::from("operator"), Value::from(7)]
            ),
            vec![FromNeovim::ModeChange(Mode::OperatorPending)]
        );
        // Transient states don't change the mode
        assert!(
            redraw(
                &mut r,
                "mode_change",
                vec![Value::from("showmatch"), Value::from(16)]
            )
            .is_empty()
        );
    }

    #[test]
    fn cmdline_show_and_hide() {
        let mut r = Redraw::default();
        let content = arr(vec![arr(vec![Value::from(0), Value::from("set")])]);
        assert_eq!(
            redraw(
                &mut r,
                "cmdline_show",
                vec![
                    content,
                    Value::from(3),
                    Value::from(":"),
                    Value::from(""),
                    Value::from(0),
                    Value::from(1),
                ]
            ),
            vec![FromNeovim::CmdlineShow {
                content: "set".into(),
                pos: 3,
                firstc: ":".into(),
                prompt: String::new(),
                level: 1,
            }]
        );
        assert_eq!(
            redraw(&mut r, "cmdline_hide", vec![Value::from(2)]),
            vec![FromNeovim::CmdlineHide { level: 2 }]
        );
    }

    #[test]
    fn popupmenu_select_uses_cached_items() {
        let mut r = Redraw::default();
        let shown = redraw(
            &mut r,
            "popupmenu_show",
            vec![
                arr(vec![item("漢字"), item("感じ")]),
                Value::from(0),
                Value::from(0),
                Value::from(0),
                Value::from(0),
            ],
        );
        let both = vec!["漢字".to_string(), "感じ".to_string()];
        assert_eq!(
            shown,
            vec![FromNeovim::Candidates(CandidateInfo::new(
                both.clone(),
                Some(0),
                false
            ))]
        );
        assert_eq!(
            redraw(&mut r, "popupmenu_select", vec![Value::from(1)]),
            vec![FromNeovim::Candidates(CandidateInfo::new(
                both.clone(),
                Some(1),
                false
            ))]
        );
        // -1: nothing selected (noselect, or back to the typed text)
        assert_eq!(
            redraw(&mut r, "popupmenu_select", vec![Value::from(-1)]),
            vec![FromNeovim::Candidates(CandidateInfo::new(
                both, None, false
            ))]
        );
        assert_eq!(
            redraw(&mut r, "popupmenu_hide", vec![]),
            vec![FromNeovim::Candidates(CandidateInfo::empty())]
        );
    }

    #[test]
    fn popupmenu_of_the_cmdline_is_marked() {
        let mut r = Redraw::default();
        let shown = redraw(
            &mut r,
            "popupmenu_show",
            vec![
                arr(vec![item("edit"), item("enew")]),
                Value::from(-1),
                Value::from(0),
                Value::from(1),
                Value::from(-1),
            ],
        );
        let items = vec!["edit".to_string(), "enew".to_string()];
        assert_eq!(
            shown,
            vec![FromNeovim::Candidates(CandidateInfo::new(
                items.clone(),
                None,
                true
            ))]
        );
        assert_eq!(
            redraw(&mut r, "popupmenu_select", vec![Value::from(0)]),
            vec![FromNeovim::Candidates(CandidateInfo::new(
                items,
                Some(0),
                true
            ))]
        );
    }

    #[test]
    fn msg_showcmd_text() {
        let mut r = Redraw::default();
        let content = arr(vec![arr(vec![Value::from(0), Value::from("2d")])]);
        assert_eq!(
            redraw(&mut r, "msg_showcmd", vec![content]),
            vec![FromNeovim::ShowCmd("2d".into())]
        );
        assert_eq!(
            redraw(&mut r, "msg_showcmd", vec![arr(vec![])]),
            vec![FromNeovim::ShowCmd(String::new())]
        );
    }

    #[test]
    fn msg_show_filters_blocked_kinds() {
        let mut r = Redraw::default();
        let content = |text: &str| arr(vec![arr(vec![Value::from(0), Value::from(text)])]);
        assert!(
            redraw(
                &mut r,
                "msg_show",
                vec![
                    Value::from("search_count"),
                    content("[1/2]"),
                    Value::from(false)
                ]
            )
            .is_empty()
        );
        assert_eq!(
            redraw(
                &mut r,
                "msg_show",
                vec![
                    Value::from("echo"),
                    content(" applied "),
                    Value::from(false)
                ]
            ),
            vec![FromNeovim::CmdlineMessage {
                text: "applied".into(),
                cmdtype: String::new(),
            }]
        );
    }

    #[test]
    fn grid_events_are_batched_until_flush() {
        let mut r = Redraw::default();
        let mut out = Vec::new();
        r.handle(
            &[
                arr(vec![
                    Value::from("grid_resize"),
                    arr(vec![Value::from(2), Value::from(10), Value::from(1)]),
                ]),
                arr(vec![Value::from("grid_clear"), arr(vec![Value::from(2)])]),
            ],
            &mut out,
        );
        assert!(out.is_empty());
        assert_eq!(
            redraw(&mut r, "flush", vec![]),
            vec![FromNeovim::GridFlush(vec![
                GridEvent::Resize {
                    grid: 2,
                    width: 10,
                    height: 1,
                },
                GridEvent::Clear { grid: 2 },
            ])]
        );
        // Nothing to flush
        assert!(redraw(&mut r, "flush", vec![]).is_empty());
    }
}
