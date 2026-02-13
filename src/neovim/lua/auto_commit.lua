_G.ime_context = { last_line_count = 1, clearing = false }

function _G.check_line_added()
    if ime_context.clearing then return end
    local line_count = vim.fn.line('$')
    if line_count > ime_context.last_line_count then
        -- Line added: commit the adjacent non-cursor line
        local cursor_line = vim.fn.line('.')
        local commit_line = cursor_line > 1 and (cursor_line - 1) or (cursor_line + 1)
        local text = vim.fn.getline(commit_line)
        if text ~= '' then
            vim.rpcnotify(vim.g.ime_channel, 'ime_auto_commit', text)
        end
        -- Delete the committed line
        ime_context.clearing = true
        vim.o.eventignore = 'all'
        vim.cmd(commit_line .. 'delete _')
        vim.o.eventignore = ''
        ime_context.clearing = false
    end
    ime_context.last_line_count = vim.fn.line('$')
end
