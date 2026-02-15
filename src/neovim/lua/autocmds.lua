-- Detect line addition on insert entry, and push snapshot on insert→non-insert
-- transition so the mode indicator updates immediately on <Esc>.
vim.api.nvim_create_autocmd('ModeChanged', {
    callback = function(args)
        if ime_context.clearing then return end
        local old_mode = args.match:match('^(.+):')
        local new_mode = args.match:match(':(.+)$')
        if new_mode and new_mode:match('^i') then
            check_line_added()
        end
        -- Push snapshot when leaving insert mode (e.g. <Esc> in insert mode)
        if old_mode and old_mode:match('^i') and new_mode and not new_mode:match('^i') then
            vim.rpcnotify(vim.g.ime_channel, 'ime_snapshot', collect_snapshot())
        end
        -- Push snapshot when entering insert mode (e.g. i, a, A after <Esc>)
        -- so the mode indicator updates immediately
        if new_mode and new_mode:match('^i') and old_mode and not old_mode:match('^i') then
            vim.rpcnotify(vim.g.ime_channel, 'ime_snapshot', collect_snapshot())
        end
    end,
})

-- Insert mode changes (text edits, cursor movement)
-- Deduplicate: TextChangedI and CursorMovedI both fire per keystroke;
-- coalesce into a single snapshot via vim.schedule().
local snapshot_pending = false
vim.api.nvim_create_autocmd({'TextChangedI', 'CursorMovedI'}, {
    callback = function()
        if ime_context.clearing then return end
        check_line_added()
        if not snapshot_pending then
            snapshot_pending = true
            vim.schedule(function()
                local ok, err = pcall(function()
                    vim.rpcnotify(vim.g.ime_channel, 'ime_snapshot', collect_snapshot())
                end)
                snapshot_pending = false
                if not ok then
                    vim.notify('[jacin] snapshot error: ' .. tostring(err), vim.log.levels.ERROR)
                end
            end)
        end
    end,
})

-- CmdlineChanged and CmdlineEnter are replaced by ext_cmdline (nvim_ui_attach).
-- cmdline_show handles both display updates and entry detection, including
-- the prompt text for @-mode (input() prompts).

-- Post-command handling
vim.api.nvim_create_autocmd('CmdlineLeave', {
    callback = function()
        local cmdtype = vim.fn.getcmdtype()
        if cmdtype == '@' then
            -- input() prompt ended (confirmed or cancelled)
            vim.rpcnotify(vim.g.ime_channel, 'ime_cmdline', { type = 'cancelled', cmdtype = '@' })
            return
        end
        if cmdtype == '/' or cmdtype == '?' then
            -- Search command-line ended (ext_cmdline sets PENDING=CommandLine,
            -- this clears it regardless of execute/cancel)
            local event = vim.v.event.abort and 'cancelled' or 'executed'
            vim.rpcnotify(vim.g.ime_channel, 'ime_cmdline', { type = event, cmdtype = cmdtype })
            return
        end
        if cmdtype ~= ':' then return end
        if vim.v.event.abort then
            vim.rpcnotify(vim.g.ime_channel, 'ime_cmdline', { type = 'cancelled', cmdtype = ':' })
        else
            -- Command output messages are captured via ext_messages (msg_show redraw event)
            vim.rpcnotify(vim.g.ime_channel, 'ime_cmdline', { type = 'executed', cmdtype = ':' })
        end
    end,
})
