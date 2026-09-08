--- The Neovim plugin for HTTP Studio.
---
--- It runs the `.http` request under the cursor against the engine, speaking
--- JSON-RPC with `hts serve`. The plugin resolves no variables, decides no
--- precedence and formats no responses: the engine does all of that, and here
--- nothing happens but drawing what arrives.
---
--- Minimal use:
--- ```lua
--- require("http-studio").setup()
--- ```

local block = require("http-studio.block")
local rpc = require("http-studio.rpc")
local ui = require("http-studio.ui")

local M = {}

--- @class HtsConfig
--- @field cmd string The binary's path.
--- @field workspace string|nil The workspace root; when nil, `hts` discovers it.
--- @field environment string|nil The active environment.
--- @field keymaps boolean Define the default mappings under `<leader>h`.
M.config = {
  cmd = "hts",
  workspace = nil,
  environment = nil,
  keymaps = true,
}

--- The JSON-RPC client, created in `setup`.
--- @type HtsClient|nil
local client = nil

--- The cached collections, so identifiers resolve without a round trip.
local collections = nil

--- The execution in flight, so it can be cancelled.
local execution = nil

--- Shows an error consistently.
--- @param message string
local function fail(message)
  vim.notify("http-studio: " .. message, vim.log.levels.ERROR)
end

--- The full command that starts the server.
--- @return string[]
local function server_command()
  local cmd = { M.config.cmd }
  if M.config.workspace then
    vim.list_extend(cmd, { "--workspace", M.config.workspace })
  end
  cmd[#cmd + 1] = "serve"
  return cmd
end

--- The parameters shared by `request/send` and `request/preview`.
--- @param id string
--- @return table
local function request_params(id)
  return {
    requestId = id,
    environment = M.config.environment,
    variables = vim.empty_dict(),
  }
end

--- Draws an execution event in the response buffer.
--- @param params table
local function on_execution_event(params)
  local event = params.event

  if event == "resolved" then
    local request = params.request or {}
    ui.set({ string.format("%s %s", request.method or "?", request.url or "?"), "" })
  elseif event == "head" then
    local head = params.head or {}
    local lines = { string.format("%s %s", head.status, head.version) }
    for _, header in ipairs(head.headers or {}) do
      lines[#lines + 1] = string.format("%s: %s", header.name, header.value)
    end
    lines[#lines + 1] = ""
    ui.append(lines)
  elseif event == "completed" then
    execution = nil
    local exchange = params.exchange or {}
    local body = exchange.body or {}
    local text = {}

    -- The body travels as bytes so as not to presume it is text; here it is
    -- reconstructed, which is what an editor wants to show.
    if body.bytes then
      local chars = {}
      for index, byte in ipairs(body.bytes) do
        chars[index] = string.char(byte)
      end
      text = vim.split(table.concat(chars), "\n", { plain = true })
    end

    local timings = exchange.timings or {}
    local millis = math.floor(((timings.total or {}).secs or 0) * 1000
      + ((timings.total or {}).nanos or 0) / 1e6)

    vim.list_extend(text, { "", string.format("%d ms · %d bytes", millis, #(body.bytes or {})) })
    ui.append(text)
  elseif event == "failed" then
    execution = nil
    ui.append({ "", "error: " .. (params.message or "unknown") })
    fail(params.message or "the execution failed")
  end
end

--- Asks for the collections and caches them.
--- @param callback fun(collections: table[]|nil)
local function with_collections(callback)
  if collections then
    callback(collections)
    return
  end

  client:request("workspace/collections", nil, function(err, result)
    if err then
      fail(err.message or "the workspace could not be read")
      callback(nil)
      return
    end
    collections = result.collections or {}
    callback(collections)
  end)
end

--- Resolves the identifier of the request under the cursor.
--- @param callback fun(id: string)
local function with_request_under_cursor(callback)
  local found = block.under_cursor()
  if not found then
    fail("the cursor is not inside any request")
    return
  end

  local slug = block.slug(found.name)
  local path = vim.api.nvim_buf_get_name(0)

  with_collections(function(list)
    if not list then
      return
    end

    local id, err = block.resolve(list, slug, path)
    if not id then
      -- Nearly always the file is unsaved: the engine reads from disk, so a
      -- new block does not exist for it yet.
      fail((err or "the request was not found") .. " (have you saved the file?)")
      return
    end

    callback(id)
  end)
end

--- Runs the request under the cursor.
function M.run()
  with_request_under_cursor(function(id)
    ui.set({ "running " .. id .. "…" })
    client:request("request/send", request_params(id), function(err, result)
      if err then
        ui.set({ "error: " .. (err.message or "unknown") })
        fail(err.message or "the request could not be run")
        return
      end
      execution = result.executionId
    end)
  end)
end

--- Resolves the request under the cursor without sending it.
function M.preview()
  with_request_under_cursor(function(id)
    client:request("request/preview", request_params(id), function(err, result)
      if err then
        fail(err.message or "the request could not be previewed")
        return
      end

      local request = result.request or {}
      local lines = {
        "preview · nothing has been sent",
        "",
        string.format("%s %s", request.method, request.url),
      }
      for _, header in ipairs(request.headers or {}) do
        lines[#lines + 1] = string.format("%s: %s", header.name, header.value)
      end

      lines[#lines + 1] = ""
      lines[#lines + 1] = "variables · the last scope wins"
      for _, variable in ipairs(result.variables or {}) do
        lines[#lines + 1] =
          string.format("  %-16s %-30s %s", variable.name, variable.value, variable.origin)
      end

      ui.set(lines)
    end)
  end)
end

--- Cancels the execution in flight.
function M.cancel()
  if not execution then
    vim.notify("http-studio: there is no execution in flight", vim.log.levels.WARN)
    return
  end

  client:request("request/cancel", { executionId = execution }, function()
    execution = nil
  end)
end

--- Selects the environment, or asks for it when no name is given.
--- @param name string|nil
function M.environment(name)
  client:request("workspace/environments", nil, function(err, result)
    if err then
      fail(err.message or "the environments could not be read")
      return
    end

    local names = vim.tbl_map(function(env)
      return env.name
    end, result.environments or {})

    if #names == 0 then
      fail("the workspace defines no environments")
      return
    end

    if name then
      if vim.tbl_contains(names, name) then
        M.config.environment = name
        vim.notify("http-studio: environment → " .. name)
      else
        fail(string.format("there is no environment `%s` (there are: %s)", name, table.concat(names, ", ")))
      end
      return
    end

    vim.ui.select(names, { prompt = "HTTP Studio environment" }, function(choice)
      if choice then
        M.config.environment = choice
        vim.notify("http-studio: environment → " .. choice)
      end
    end)
  end)
end

--- Opens a picker with every request in the workspace.
function M.pick()
  with_collections(function(list)
    if not list then
      return
    end

    local items = {}
    for _, collection in ipairs(list) do
      for _, request in ipairs(collection.requests or {}) do
        items[#items + 1] = { id = request.id, method = request.method }
      end
    end

    vim.ui.select(items, {
      prompt = "Requests",
      format_item = function(item)
        return string.format("%-6s %s", item.method, item.id)
      end,
    }, function(choice)
      if not choice then
        return
      end
      ui.set({ "running " .. choice.id .. "…" })
      client:request("request/send", request_params(choice.id), function(err, result)
        if err then
          fail(err.message or "the request could not be run")
          return
        end
        execution = result.executionId
      end)
    end)
  end)
end

--- Shows the history of executions.
--- @param limit integer|nil
function M.history(limit)
  client:request("history/query", { limit = limit or 20 }, function(err, result)
    if err then
      fail(err.message or "the history could not be read")
      return
    end

    local lines = { "execution history", "" }
    for _, entry in ipairs(result.entries or {}) do
      lines[#lines + 1] = string.format(
        "%3d  %s  %-24s %5d ms  %s",
        entry.status,
        entry.executed_at,
        entry.request_id,
        entry.duration_ms,
        entry.url
      )
    end
    ui.set(lines)
  end)
end

--- Discards the workspace cache and restarts the server.
function M.reload()
  collections = nil
  if client then
    client:stop()
  end
  vim.notify("http-studio: workspace reloaded")
end

--- Stops the server.
function M.stop()
  if client then
    client:stop()
  end
end

--- Defines the commands and, when asked for, the default mappings.
local function define_commands()
  local command = vim.api.nvim_create_user_command

  command("HtsRun", M.run, { desc = "Run the request under the cursor" })
  command("HtsPreview", M.preview, { desc = "Resolve the request without sending it" })
  command("HtsCancel", M.cancel, { desc = "Cancel the execution in flight" })
  command("HtsPick", M.pick, { desc = "Pick a request from the workspace" })
  command("HtsReload", M.reload, { desc = "Reload the workspace" })
  command("HtsStop", M.stop, { desc = "Stop the HTTP Studio server" })

  command("HtsEnv", function(opts)
    M.environment(opts.args ~= "" and opts.args or nil)
  end, { nargs = "?", desc = "Select the environment" })

  command("HtsHistory", function(opts)
    M.history(tonumber(opts.args))
  end, { nargs = "?", desc = "Show the history" })

  -- The `<Plug>`s always exist, for whoever prefers mapping their own way.
  vim.keymap.set("n", "<Plug>(hts-run)", M.run, { desc = "HTTP Studio: run" })
  vim.keymap.set("n", "<Plug>(hts-preview)", M.preview, { desc = "HTTP Studio: preview" })
  vim.keymap.set("n", "<Plug>(hts-env)", function()
    M.environment(nil)
  end, { desc = "HTTP Studio: environment" })
  vim.keymap.set("n", "<Plug>(hts-pick)", M.pick, { desc = "HTTP Studio: pick a request" })
end

--- The default mappings, only in `.http` buffers.
local function define_keymaps()
  vim.api.nvim_create_autocmd("FileType", {
    group = vim.api.nvim_create_augroup("HttpStudioKeymaps", { clear = true }),
    pattern = { "http", "rest" },
    callback = function(event)
      local function map(lhs, plug, desc)
        vim.keymap.set("n", lhs, plug, { buffer = event.buf, remap = true, desc = desc })
      end
      map("<leader>hr", "<Plug>(hts-run)", "HTTP Studio: run")
      map("<leader>hp", "<Plug>(hts-preview)", "HTTP Studio: preview")
      map("<leader>he", "<Plug>(hts-env)", "HTTP Studio: environment")
      map("<leader>hf", "<Plug>(hts-pick)", "HTTP Studio: pick a request")
    end,
  })
end

--- Configures the plugin.
--- @param opts HtsConfig|nil
function M.setup(opts)
  M.config = vim.tbl_extend("force", M.config, opts or {})

  client = rpc.new(server_command())
  client:on("execution/event", on_execution_event)
  collections = nil

  define_commands()
  if M.config.keymaps then
    define_keymaps()
  end

  -- Killing the server on exit avoids leaving an orphan `hts serve` behind
  -- when Neovim closes without going through `:HtsStop`.
  vim.api.nvim_create_autocmd("VimLeavePre", {
    group = vim.api.nvim_create_augroup("HttpStudioShutdown", { clear = true }),
    callback = M.stop,
  })
end

--- The client in use, for the tests.
--- @return HtsClient|nil
function M._client()
  return client
end

return M
