-- Whole buffer is empty (a single empty line). Passthrough is decided on the
-- buffer, not the current line, so an empty 2nd line doesn't leak keys.
local function buffer_is_empty()
    return vim.fn.line('$') == 1 and vim.fn.getline(1) == ''
end

-- Backspace: detect empty buffer for DeleteSurrounding
function _G.ime_handle_bs()
    if buffer_is_empty() then
        return { type = 'passthrough' }
    end
    vim.api.nvim_input('<BS>')
    return { type = 'processing' }
end

-- Enter: detect empty buffer for passthrough
function _G.ime_handle_enter()
    if buffer_is_empty() then
        return { type = 'passthrough' }
    end
    vim.api.nvim_input('<CR>')
    return { type = 'processing' }
end

-- Commit: join all lines with "\n", clear buffer, return text for commit
function _G.ime_handle_commit()
    if buffer_is_empty() then
        return { type = 'empty' }
    end
    local text = table.concat(vim.api.nvim_buf_get_lines(0, 0, -1, false), '\n')
    vim.cmd('%delete _')
    vim.cmd('startinsert')
    return { type = 'commit', text = text }
end
