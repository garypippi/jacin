//! Parsing of ext_linegrid / ext_multigrid redraw events into `GridEvent`s
//!
//! Pure functions over msgpack values.

use nvim_rs::Value;

use super::protocol::{GridCell, GridEvent, HlAttr};

/// Parse one parameter tuple of a grid/window redraw event.
/// Returns None for unrelated events or malformed params.
pub fn parse_grid_event(name: &str, params: &Value) -> Option<GridEvent> {
    let args = params.as_array()?;
    let uint = |i: usize| args.get(i).and_then(Value::as_u64);
    let size = |i: usize| uint(i).map(|n| n as usize);
    // Float anchor positions may be sent as floats
    let pos = |i: usize| {
        let v = args.get(i)?;
        v.as_u64()
            .map(|n| n as usize)
            .or_else(|| v.as_f64().map(|f| f.max(0.0).round() as usize))
    };
    let grid = uint(0);

    match name {
        "grid_resize" => Some(GridEvent::Resize {
            grid: grid?,
            width: size(1)?,
            height: size(2)?,
        }),
        "grid_clear" => Some(GridEvent::Clear { grid: grid? }),
        "grid_destroy" => Some(GridEvent::Destroy { grid: grid? }),
        "grid_cursor_goto" => Some(GridEvent::CursorGoto {
            grid: grid?,
            row: size(1)?,
            col: size(2)?,
        }),
        "grid_line" => Some(GridEvent::Line {
            grid: grid?,
            row: size(1)?,
            col_start: size(2)?,
            cells: parse_cells(args.get(3)?.as_array()?)?,
            wrap: args.get(4).and_then(Value::as_bool).unwrap_or(false),
        }),
        "grid_scroll" => Some(GridEvent::Scroll {
            grid: grid?,
            top: size(1)?,
            bot: size(2)?,
            left: size(3)?,
            right: size(4)?,
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
        // [grid, win, start_row, start_col, width, height]
        "win_pos" => Some(GridEvent::WinPos {
            grid: grid?,
            row: size(2)?,
            col: size(3)?,
            width: size(4)?,
            height: size(5)?,
        }),
        // [grid, win, anchor, anchor_grid, anchor_row, anchor_col,
        //  mouse_enabled, zindex, compindex, screen_row, screen_col]
        // screen_row/col (Neovim-computed placement) fall back to the anchor
        "win_float_pos" => Some(GridEvent::WinFloatPos {
            grid: grid?,
            anchor_grid: uint(3)?,
            screen_row: pos(9).or_else(|| pos(4))?,
            screen_col: pos(10).or_else(|| pos(5))?,
            zindex: uint(7).unwrap_or(0),
        }),
        "win_hide" => Some(GridEvent::WinHide { grid: grid? }),
        "win_close" => Some(GridEvent::WinClose { grid: grid? }),
        // [grid, win, topline, botline, curline, curcol, line_count, scroll_delta]
        "win_viewport" => Some(GridEvent::WinViewport {
            grid: grid?,
            topline: size(2)?,
            botline: size(3)?,
            curline: size(4)?,
            curcol: size(5)?,
            line_count: size(6)?,
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
                grid: 1,
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
                wrap: false,
            })
        );
    }

    #[test]
    fn window_grids_keep_their_id() {
        let params = arr(vec![Value::from(2), Value::from(80), Value::from(23)]);
        assert_eq!(
            parse_grid_event("grid_resize", &params),
            Some(GridEvent::Resize {
                grid: 2,
                width: 80,
                height: 23
            })
        );
    }

    #[test]
    fn win_float_pos_uses_screen_position() {
        // A float like an nvim-cmp menu, placed by Neovim at screen (2, 0)
        let params = arr(vec![
            Value::from(5),
            Value::from(1000),
            Value::from("NW"),
            Value::from(1),
            Value::from(2.0),
            Value::from(0.0),
            Value::from(true),
            Value::from(1001),
            Value::from(1),
            Value::from(2),
            Value::from(0),
        ]);
        assert_eq!(
            parse_grid_event("win_float_pos", &params),
            Some(GridEvent::WinFloatPos {
                grid: 5,
                anchor_grid: 1,
                screen_row: 2,
                screen_col: 0,
                zindex: 1001
            })
        );
    }

    #[test]
    fn win_float_pos_falls_back_to_float_anchor() {
        let params = arr(vec![
            Value::from(5),
            Value::from(1000),
            Value::from("NW"),
            Value::from(2),
            Value::from(1.6),
            Value::from(3.0),
            Value::from(true),
            Value::from(50),
        ]);
        assert_eq!(
            parse_grid_event("win_float_pos", &params),
            Some(GridEvent::WinFloatPos {
                grid: 5,
                anchor_grid: 2,
                screen_row: 2,
                screen_col: 3,
                zindex: 50
            })
        );
    }

    #[test]
    fn win_viewport_parses_line_count() {
        let params = arr(vec![
            Value::from(2),
            Value::from(1000),
            Value::from(0),
            Value::from(3),
            Value::from(1),
            Value::from(0),
            Value::from(2),
            Value::from(0),
        ]);
        assert_eq!(
            parse_grid_event("win_viewport", &params),
            Some(GridEvent::WinViewport {
                grid: 2,
                topline: 0,
                botline: 3,
                curline: 1,
                curcol: 0,
                line_count: 2
            })
        );
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
                grid: 1,
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
