function _G.collect_snapshot()
    local mode = vim.api.nvim_get_mode()
    local line = vim.fn.getline('.')
    local col = vim.fn.col('.')

    local snapshot = {
        preedit = line,
        cursor_byte = col,
        mode = mode.mode,
        blocking = mode.blocking,
        char_width = 0,
        recording = vim.fn.reg_recording(),
    }

    -- Normal/visual mode: character width under cursor
    if mode.mode == 'n' or mode.mode:find('^no') or mode.mode:find('^v') or mode.mode == 'V' or mode.mode == '\22' then
        local char = vim.fn.matchstr(line, '\\%' .. col .. 'c.')
        snapshot.char_width = vim.fn.strlen(char)
    end

    -- Visual mode: selection range
    if mode.mode == 'V' or mode.mode == '\22' then
        -- Line-wise / block-wise: highlight entire line
        snapshot.visual_begin = 1
        snapshot.visual_end = vim.fn.strlen(line) + 1
    elseif mode.mode:find('^v') then
        local v_col = vim.fn.getpos('v')[3]
        local sel_start = math.min(v_col, col)
        local sel_end_col = math.max(v_col, col)
        local end_char = vim.fn.matchstr(line, '\\%' .. sel_end_col .. 'c.')
        snapshot.visual_begin = sel_start
        snapshot.visual_end = sel_end_col + vim.fn.strlen(end_char)
    end

    return snapshot
end
