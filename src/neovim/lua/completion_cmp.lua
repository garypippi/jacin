local function ime_setup_cmp()
    local ok, cmp = pcall(require, 'cmp')
    if not ok then return false end
    local visible = false
    local last_sel = -1
    local last_count = 0
    local pending = false
    local function send()
        if not cmp.visible() then
            if visible then
                visible = false
                last_sel = -1
                last_count = 0
                vim.rpcnotify(0, 'ime_candidates', { candidates = {}, selected = -1 })
            end
            return
        end
        visible = true
        local entries = cmp.get_entries() or {}
        -- Find selected index via active entry
        local active = cmp.get_active_entry()
        local sel = -1
        if active then
            for i, e in ipairs(entries) do
                if e == active then
                    sel = i - 1
                    break
                end
            end
        end
        -- Deduplicate: skip if selection and entry count unchanged
        if sel == last_sel and #entries == last_count then
            return
        end
        last_sel = sel
        last_count = #entries
        local words = {}
        for _, e in ipairs(entries) do
            local w = e:get_word()
            if w and w ~= '' then words[#words + 1] = w end
        end
        vim.rpcnotify(0, 'ime_candidates', {
            candidates = words,
            selected = sel,
        })
    end
    local function schedule_send()
        if pending then return end
        pending = true
        vim.schedule(function()
            pending = false
            send()
        end)
    end
    cmp.event:on('menu_opened', function()
        last_sel = -1
        last_count = 0
        schedule_send()
    end)
    cmp.event:on('menu_closed', function()
        visible = false
        last_sel = -1
        last_count = 0
        vim.rpcnotify(0, 'ime_candidates', { candidates = {}, selected = -1 })
    end)
    -- Poll after every key to catch selection changes (Ctrl+N/P)
    vim.on_key(function()
        if visible then schedule_send() end
    end)
    return true
end
-- Handle lazy-loaded cmp: try now, retry on InsertEnter
if not ime_setup_cmp() then
    vim.api.nvim_create_autocmd('InsertEnter', {
        once = true,
        callback = function() vim.schedule(ime_setup_cmp) end,
    })
end
