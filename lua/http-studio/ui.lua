--- The response buffer.
---
--- One reusable buffer in a vertical split, rather than one per execution:
--- running twenty requests in a row must not leave twenty buffers to close by
--- hand.

local M = {}

--- The response buffer, once it has been created.
--- @type integer|nil
local bufnr = nil

--- The plugin's own highlight groups, defined once.
local function define_highlights()
  vim.api.nvim_set_hl(0, "HtsStatusOk", { link = "DiagnosticOk", default = true })
  vim.api.nvim_set_hl(0, "HtsStatusError", { link = "DiagnosticError", default = true })
  vim.api.nvim_set_hl(0, "HtsHeader", { link = "Comment", default = true })
  vim.api.nvim_set_hl(0, "HtsRequest", { link = "Title", default = true })
end

--- Returns the response buffer, creating it when needed.
--- @return integer
local function ensure_buffer()
  if bufnr and vim.api.nvim_buf_is_valid(bufnr) then
    return bufnr
  end

  bufnr = vim.api.nvim_create_buf(false, true)
  vim.api.nvim_buf_set_name(bufnr, "http-studio://response")
  vim.bo[bufnr].buftype = "nofile"
  vim.bo[bufnr].bufhidden = "hide"
  vim.bo[bufnr].swapfile = false
  vim.bo[bufnr].filetype = "http-studio-response"

  -- `q` closes the window, as in any read-only vim buffer.
  vim.keymap.set("n", "q", "<cmd>close<cr>", { buffer = bufnr, nowait = true })

  define_highlights()
  return bufnr
end

--- Opens the response window when it is not visible and returns the buffer.
--- @param focus boolean|nil Leave the cursor inside.
--- @return integer
function M.open(focus)
  local buf = ensure_buffer()

  local visible = vim.iter(vim.api.nvim_list_wins()):find(function(win)
    return vim.api.nvim_win_get_buf(win) == buf
  end)

  if not visible then
    local current = vim.api.nvim_get_current_win()
    vim.cmd("vsplit")
    vim.api.nvim_win_set_buf(vim.api.nvim_get_current_win(), buf)
    if not focus then
      vim.api.nvim_set_current_win(current)
    end
  elseif focus then
    vim.api.nvim_set_current_win(visible)
  end

  return buf
end

--- Replaces the buffer's contents.
--- @param lines string[]
function M.set(lines)
  local buf = M.open(false)
  vim.bo[buf].modifiable = true
  vim.api.nvim_buf_set_lines(buf, 0, -1, false, lines)
  vim.bo[buf].modifiable = false
  M.highlight(buf)
end

--- Appends lines at the end and leaves the view on them.
--- @param lines string[]
function M.append(lines)
  if #lines == 0 then
    return
  end

  local buf = M.open(false)
  vim.bo[buf].modifiable = true
  vim.api.nvim_buf_set_lines(buf, -1, -1, false, lines)
  vim.bo[buf].modifiable = false
  M.highlight(buf)

  -- Follow the end only when the cursor is not inside: if the user has gone
  -- off to read a particular header, they are not dragged along.
  for _, win in ipairs(vim.api.nvim_list_wins()) do
    if vim.api.nvim_win_get_buf(win) == buf and win ~= vim.api.nvim_get_current_win() then
      vim.api.nvim_win_set_cursor(win, { vim.api.nvim_buf_line_count(buf), 0 })
    end
  end
end

--- The namespace of the highlight marks.
local namespace = vim.api.nvim_create_namespace("http-studio")

--- Colours the status, the headers and the request line.
--- @param buf integer
function M.highlight(buf)
  vim.api.nvim_buf_clear_namespace(buf, namespace, 0, -1)

  for index, line in ipairs(vim.api.nvim_buf_get_lines(buf, 0, -1, false)) do
    local group

    local status = line:match("^(%d%d%d)%s")
    if status then
      group = tonumber(status) < 300 and "HtsStatusOk" or "HtsStatusError"
    elseif line:match("^%u+%s+https?://") then
      group = "HtsRequest"
    elseif line:match("^[%w%-]+:%s") then
      group = "HtsHeader"
    end

    if group then
      vim.api.nvim_buf_set_extmark(buf, namespace, index - 1, 0, {
        end_col = #line,
        hl_group = group,
      })
    end
  end
end

--- The current contents, for the tests.
--- @return string[]
function M.lines()
  if not bufnr or not vim.api.nvim_buf_is_valid(bufnr) then
    return {}
  end
  return vim.api.nvim_buf_get_lines(bufnr, 0, -1, false)
end

return M
