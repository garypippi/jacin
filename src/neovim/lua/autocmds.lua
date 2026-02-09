-- Detect line addition on insert entry (for o/O from normal mode)
vim.api.nvim_create_autocmd('ModeChanged', {
    callback = function(args)
        if ime_context.clearing then return end
        local new_mode = args.match:match(':(.+)$')
        if new_mode and new_mode:match('^i') then
            check_line_added()
        end
    end,
})

-- Insert mode changes (text edits, cursor movement)
vim.api.nvim_create_autocmd({'TextChangedI', 'CursorMovedI'}, {
    callback = function()
        if ime_context.clearing then return end
        check_line_added()
        vim.rpcnotify(0, 'ime_snapshot', collect_snapshot())
    end,
})

-- Command-line display updates
vim.api.nvim_create_autocmd('CmdlineChanged', {
    callback = function()
        if vim.fn.getcmdtype() == ':' then
            vim.rpcnotify(0, 'ime_cmdline', {
                type = 'update',
                text = ':' .. vim.fn.getcmdline()
            })
        end
    end,
})

-- Post-command handling
vim.api.nvim_create_autocmd('CmdlineLeave', {
    callback = function()
        if vim.fn.getcmdtype() ~= ':' then return end
        if vim.v.event.abort then
            vim.rpcnotify(0, 'ime_cmdline', { type = 'cancelled' })
        else
            -- Snapshot last message before command executes
            local old_msg = vim.fn.execute('1messages')
            vim.rpcnotify(0, 'ime_cmdline', { type = 'executed' })
            vim.schedule(function()
                -- Check if command produced a new message
                local new_msg = vim.fn.execute('1messages')
                if new_msg ~= old_msg and new_msg ~= '' then
                    local text = vim.trim(new_msg)
                    if text ~= '' then
                        vim.rpcnotify(0, 'ime_cmdline', { type = 'message', text = text })
                    end
                end
                if vim.g.ime_auto_startinsert then
                    vim.cmd('startinsert')
                    vim.rpcnotify(0, 'ime_snapshot', collect_snapshot())
                end
            end)
        end
    end,
})
