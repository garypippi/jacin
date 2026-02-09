-- Native popup menu: use CompleteChanged/CompleteDone autocmds
vim.api.nvim_create_autocmd('CompleteChanged', {
    callback = function()
        local info = vim.fn.complete_info({'items', 'selected'})
        local words = {}
        for _, item in ipairs(info.items or {}) do
            local w = item.word or item.abbr or ''
            if w ~= '' then words[#words + 1] = w end
        end
        vim.rpcnotify(0, 'ime_candidates', {
            candidates = words,
            selected = info.selected,
        })
    end,
})

vim.api.nvim_create_autocmd('CompleteDone', {
    callback = function()
        vim.rpcnotify(0, 'ime_candidates', {
            candidates = {},
            selected = -1,
        })
    end,
})
