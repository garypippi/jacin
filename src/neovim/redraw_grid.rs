//! Parsing of ext_linegrid redraw events into `GridEvent`s
//!
//! Pure functions over msgpack values; only the global grid (1) is kept
//! since ext_multigrid is not used.

use nvim_rs::Value;

use super::protocol::{GridCell, GridEvent, HlAttr};

/// The global grid; the only one without ext_multigrid
const GLOBAL_GRID: u64 = 1;

/// Parse one parameter tuple of a grid redraw event.
/// Returns None for non-grid events, other grids, or malformed params.
pub fn parse_grid_event(name: &str, params: &Value) -> Option<GridEvent> {
    let args = params.as_array()?;
    let uint = |i: usize| args.get(i).and_then(Value::as_u64);
    let on_global_grid = || uint(0) == Some(GLOBAL_GRID);

    match name {
        "grid_resize" if on_global_grid() => Some(GridEvent::Resize {
            width: uint(1)? as usize,
            height: uint(2)? as usize,
        }),
        "grid_clear" if on_global_grid() => Some(GridEvent::Clear),
        "grid_cursor_goto" if on_global_grid() => Some(GridEvent::CursorGoto {
            row: uint(1)? as usize,
            col: uint(2)? as usize,
        }),
        "grid_line" if on_global_grid() => Some(GridEvent::Line {
            row: uint(1)? as usize,
            col_start: uint(2)? as usize,
            cells: parse_cells(args.get(3)?.as_array()?)?,
        }),
        "grid_scroll" if on_global_grid() => Some(GridEvent::Scroll {
            top: uint(1)? as usize,
            bot: uint(2)? as usize,
            left: uint(3)? as usize,
            right: uint(4)? as usize,
            rows: args.get(5)?.as_i64()?,
        }),
        "hl_attr_define" => Some(GridEvent::HlAttrDefine {
            id: uint(0)?,
            attr: parse_hl_attr(args.get(1)?),
        }),
        "default_colors_set" => Some(GridEvent::DefaultColors {
            fg: rgb(args.first()?)?,
            bg: rgb(args.get(1)?)?,
            sp: rgb(args.get(2)?)?,
        }),
        _ => None,
    }
}

/// Parse `[text(, hl_id, repeat)]` cells. A missing hl_id means the most
/// recently seen hl_id in the same event.
fn parse_cells(raw: &[Value]) -> Option<Vec<GridCell>> {
    let mut hl = 0;
    raw.iter()
        .map(|cell| {
            let cell = cell.as_array()?;
            let text = cell.first()?.as_str()?.to_string();
            if let Some(id) = cell.get(1).and_then(Value::as_u64) {
                hl = id;
            }
            let repeat = cell.get(2).and_then(Value::as_u64).unwrap_or(1) as usize;
            Some(GridCell { text, hl, repeat })
        })
        .collect()
}

fn parse_hl_attr(value: &Value) -> HlAttr {
    let mut attr = HlAttr::default();
    let Some(map) = value.as_map() else {
        return attr;
    };
    for (k, v) in map {
        let flag = v.as_bool().unwrap_or(false);
        match k.as_str() {
            Some("foreground") => attr.foreground = rgb(v),
            Some("background") => attr.background = rgb(v),
            Some("special") => attr.special = rgb(v),
            Some("reverse") => attr.reverse = flag,
            Some("bold") => attr.bold = flag,
            Some("italic") => attr.italic = flag,
            Some("underline") => attr.underline = flag,
            Some("undercurl") => attr.undercurl = flag,
            Some("strikethrough") => attr.strikethrough = flag,
            _ => {}
        }
    }
    attr
}

/// RGB color as 0xRRGGBB (negative = unset with ext_termcolors)
fn rgb(value: &Value) -> Option<u32> {
    value.as_i64().and_then(|n| u32::try_from(n).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arr(items: Vec<Value>) -> Value {
        Value::Array(items)
    }

    #[test]
    fn grid_line_resolves_omitted_hl_and_repeat() {
        let params = arr(vec![
            Value::from(1),
            Value::from(2),
            Value::from(0),
            arr(vec![
                arr(vec![Value::from("a"), Value::from(5)]),
                arr(vec![Value::from("b")]),
                arr(vec![Value::from(" "), Value::from(0), Value::from(3)]),
            ]),
            Value::from(false),
        ]);
        assert_eq!(
            parse_grid_event("grid_line", &params),
            Some(GridEvent::Line {
                row: 2,
                col_start: 0,
                cells: vec![
                    GridCell {
                        text: "a".into(),
                        hl: 5,
                        repeat: 1
                    },
                    GridCell {
                        text: "b".into(),
                        hl: 5,
                        repeat: 1
                    },
                    GridCell {
                        text: " ".into(),
                        hl: 0,
                        repeat: 3
                    },
                ],
            })
        );
    }

    #[test]
    fn other_grids_are_ignored() {
        let params = arr(vec![Value::from(2), Value::from(80), Value::from(24)]);
        assert_eq!(parse_grid_event("grid_resize", &params), None);
    }

    #[test]
    fn grid_scroll_keeps_negative_rows() {
        let params = arr(vec![
            Value::from(1),
            Value::from(0),
            Value::from(10),
            Value::from(0),
            Value::from(80),
            Value::from(-2),
            Value::from(0),
        ]);
        assert_eq!(
            parse_grid_event("grid_scroll", &params),
            Some(GridEvent::Scroll {
                top: 0,
                bot: 10,
                left: 0,
                right: 80,
                rows: -2
            })
        );
    }

    #[test]
    fn hl_attr_define_parses_rgb_attrs() {
        let params = arr(vec![
            Value::from(7),
            Value::Map(vec![
                (Value::from("foreground"), Value::from(0xff0000)),
                (Value::from("bold"), Value::from(true)),
            ]),
            Value::Map(vec![]),
            arr(vec![]),
        ]);
        assert_eq!(
            parse_grid_event("hl_attr_define", &params),
            Some(GridEvent::HlAttrDefine {
                id: 7,
                attr: HlAttr {
                    foreground: Some(0xff0000),
                    bold: true,
                    ..HlAttr::default()
                },
            })
        );
    }

    #[test]
    fn non_grid_events_return_none() {
        assert_eq!(parse_grid_event("mode_change", &arr(vec![])), None);
    }
}
