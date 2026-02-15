-- BufWriteCmd: hook :w / :wq / :x to commit preedit text
vim.api.nvim_create_autocmd('BufWriteCmd', {
    buffer = 0,
    callback = function()
        local result = ime_handle_commit()
        if result.type == 'commit' then
            vim.rpcnotify(vim.g.ime_channel, 'ime_auto_commit', result.text)
        end
        vim.bo.modified = false
    end,
})