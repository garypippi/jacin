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
            vim.rpcnotify(0, 'ime_snapshot', collect_snapshot())
        end
        -- Push snapshot when entering insert mode (e.g. i, a, A after <Esc>)
        -- so the mode indicator updates immediately
        if new_mode and new_mode:match('^i') and old_mode and not old_mode:match('^i') then
            vim.rpcnotify(0, 'ime_snapshot', collect_snapshot())
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
                    vim.rpcnotify(0, 'ime_snapshot', collect_snapshot())
                end)
                snapshot_pending = false
                if not ok then
                    vim.notify('[jacin] snapshot error: ' .. tostring(err), vim.log.levels.ERROR)
                end
            end)
        end
    end,
})

-- Command-line display updates (`:` normal commands, `@` input() prompts)
vim.api.nvim_create_autocmd('CmdlineChanged', {
    callback = function()
        local cmdtype = vim.fn.getcmdtype()
        if cmdtype == ':' then
            vim.rpcnotify(0, 'ime_cmdline', {
                type = 'update',
                text = ':' .. vim.fn.getcmdline()
            })
        elseif cmdtype == '@' then
            vim.rpcnotify(0, 'ime_cmdline', {
                type = 'update',
                text = vim.fn.getcmdline()
            })
        end
    end,
})

-- Detect entry into input() prompts (e.g., skkeleton dictionary registration).
-- Sends an initial update so the Rust side sets PendingState::CommandLine
-- before any key arrives, preventing c-mode recovery from escaping the prompt.
vim.api.nvim_create_autocmd('CmdlineEnter', {
    callback = function()
        if vim.fn.getcmdtype() == '@' then
            vim.rpcnotify(0, 'ime_cmdline', {
                type = 'update',
                text = vim.fn.getcmdline()
            })
        end
    end,
})

-- Post-command handling
vim.api.nvim_create_autocmd('CmdlineLeave', {
    callback = function()
        local cmdtype = vim.fn.getcmdtype()
        if cmdtype == '@' then
            -- input() prompt ended (confirmed or cancelled)
            vim.rpcnotify(0, 'ime_cmdline', { type = 'cancelled' })
            return
        end
        if cmdtype ~= ':' then return end
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
