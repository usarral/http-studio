--- Locates the `.http` request under the cursor.
---
--- It reproduces the naming rules of the engine's parser
--- (`crates/infrastructure/src/storage/http_file.rs`): the name comes from
--- `# @name`, else from the title after `###`, else from the block's position.
---
--- It is the plugin's only duplicated logic, and it is deliberate: asking the
--- server "which request is on line 42?" would mean sending it the unsaved
--- buffer on every keystroke. In exchange, the resulting name is **not** used
--- directly: it is checked against the real identifiers `workspace/collections`
--- returns, so a divergence shows up as "I cannot find the request" and never
--- as a silent call to the wrong one.

local M = {}

--- Accented vowels and eñes in UTF-8, mapped to their ASCII equivalent.
local ACCENTS = {
  ["á"] = "a", ["à"] = "a", ["ä"] = "a", ["Á"] = "a", ["À"] = "a", ["Ä"] = "a",
  ["é"] = "e", ["è"] = "e", ["ë"] = "e", ["É"] = "e", ["È"] = "e", ["Ë"] = "e",
  ["í"] = "i", ["ì"] = "i", ["ï"] = "i", ["Í"] = "i", ["Ì"] = "i", ["Ï"] = "i",
  ["ó"] = "o", ["ò"] = "o", ["ö"] = "o", ["Ó"] = "o", ["Ò"] = "o", ["Ö"] = "o",
  ["ú"] = "u", ["ù"] = "u", ["ü"] = "u", ["Ú"] = "u", ["Ù"] = "u", ["Ü"] = "u",
  ["ñ"] = "n", ["Ñ"] = "n", ["ç"] = "c", ["Ç"] = "c",
}

--- Turns a readable name into the identifier fragment the engine uses.
--- @param name string
--- @return string
function M.slug(name)
  local out = {}
  local previous_dash = false

  -- Neovim runs LuaJIT, which is Lua 5.1 and ships no `utf8` library. This
  -- pattern walks whole UTF-8 sequences: an ASCII byte, or a leading byte
  -- (194-244) followed by its continuations (128-191).
  for char in name:gmatch("[%z\1-\127\194-\244][\128-\191]*") do
    local plain = ACCENTS[char]

    if plain then
      out[#out + 1] = plain
      previous_dash = false
    elseif #char == 1 and char:match("^[%w]$") then
      out[#out + 1] = char:lower()
      previous_dash = false
    elseif not previous_dash and #out > 0 then
      out[#out + 1] = "-"
      previous_dash = true
    end
  end

  local slug = table.concat(out):gsub("%-+$", "")
  if slug == "" then
    return "request"
  end
  return slug
end

--- `true` when the line contributes nothing to the request.
--- @param line string
--- @return boolean
local function ignorable(line)
  local trimmed = line:match("^%s*(.-)%s*$")
  return trimmed == ""
    or trimmed:sub(1, 1) == "#"
    or trimmed:sub(1, 2) == "//"
    or trimmed:sub(1, 1) == "@"
end

--- Returns the request of the block containing `cursor`.
---
--- @param lines string[] The buffer's lines.
--- @param cursor integer The cursor's line, starting at 1.
--- @return table|nil `{ name = string, first = integer, last = integer }`
function M.locate(lines, cursor)
  if #lines == 0 then
    return nil
  end

  cursor = math.max(1, math.min(cursor, #lines))

  -- The start of each block: line 1, plus every `###`.
  local starts = { 1 }
  for index, line in ipairs(lines) do
    if line:match("^%s*###") then
      starts[#starts + 1] = index
    end
  end

  -- The cursor's block is the last one starting on its line or before it.
  local block = 1
  for position, start in ipairs(starts) do
    if start <= cursor then
      block = position
    end
  end

  local first = starts[block]
  local last = (starts[block + 1] or (#lines + 1)) - 1

  -- The ordinal among the blocks that do hold a request: it is the number the
  -- engine uses when there is neither an `@name` nor a title.
  local ordinal = 0
  for position = 1, block do
    local from = starts[position]
    local to = (starts[position + 1] or (#lines + 1)) - 1
    for index = from, to do
      if not ignorable(lines[index]) then
        ordinal = ordinal + 1
        break
      end
    end
  end

  if ordinal == 0 then
    return nil
  end

  -- 1) `# @name`, the most explicit of the three.
  for index = first, last do
    local name = lines[index]:match("^%s*[#/]+%s*@name%s+(%S+)")
    if name then
      return { name = name, first = first, last = last }
    end
  end

  -- 2) The title following the `###`.
  local title = lines[first]:match("^%s*###+%s*(.-)%s*$")
  if title and title ~= "" then
    return { name = title, first = first, last = last }
  end

  -- 3) The position within the file.
  return { name = "request-" .. ordinal, first = first, last = last }
end

--- Like [`M.locate`], but reading from the current buffer and window.
--- @param bufnr integer|nil
--- @return table|nil
function M.under_cursor(bufnr)
  bufnr = bufnr or vim.api.nvim_get_current_buf()
  local lines = vim.api.nvim_buf_get_lines(bufnr, 0, -1, false)
  local cursor = vim.api.nvim_win_get_cursor(0)[1]
  return M.locate(lines, cursor)
end

--- Picks, out of the workspace's identifiers, the one matching the block that
--- was found.
---
--- It filters by the identifier's last segment and, when several candidates
--- remain, breaks the tie with the file's path: a collection's name is the
--- relative path without its extension, so it must be a suffix of the buffer's.
---
--- @param collections table[] What `workspace/collections` returns.
--- @param slug string The block's name, already normalised.
--- @param path string The buffer's path.
--- @return string|nil id, string|nil err
function M.resolve(collections, slug, path)
  local without_extension = path:gsub("%.%w+$", "")
  local matches = {}

  for _, collection in ipairs(collections or {}) do
    for _, request in ipairs(collection.requests or {}) do
      local id = request.id
      if id:match("([^/]+)$") == slug then
        matches[#matches + 1] = { id = id, collection = collection.name }
      end
    end
  end

  if #matches == 0 then
    return nil, string.format("there is no request named `%s` in the workspace", slug)
  end

  if #matches == 1 then
    return matches[1].id, nil
  end

  for _, candidate in ipairs(matches) do
    if without_extension:sub(-#candidate.collection) == candidate.collection then
      return candidate.id, nil
    end
  end

  return nil,
    string.format("`%s` is ambiguous: there are %d requests with that name", slug, #matches)
end

return M
