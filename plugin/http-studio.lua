-- Filetype detection for the `.http` format.
--
-- It goes in `plugin/` and not in `setup()` on purpose: that way opening a
-- `.http` gets the right filetype even when the user has not called `setup`
-- yet, and FileType mappings can hook in later without reloading the buffer.
--
-- Recent Neovim versions already ship `ft=http` for these files; this only
-- covers `.rest`, the alternative extension VS Code REST Client accepts.

if vim.g.loaded_http_studio then
  return
end
vim.g.loaded_http_studio = true

vim.filetype.add({
  extension = {
    rest = "http",
  },
})
