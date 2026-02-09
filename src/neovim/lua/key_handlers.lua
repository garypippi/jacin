-- Backspace: detect empty buffer for DeleteSurrounding
function _G.ime_handle_bs()
    local line = vim.fn.getline('.')
    if line == '' then
        return { type = 'delete_surrounding' }
    end
    vim.api.nvim_input('<BS>')
    return { type = 'processing' }
end

-- Commit: get preedit text, clear buffer, return text for commit
function _G.ime_handle_commit()
    local line = vim.fn.getline('.')
    if line == '' then
        return { type = 'empty' }
    end
    vim.cmd('normal! 0D')
    vim.cmd('startinsert')
    return { type = 'commit', text = line }
end
