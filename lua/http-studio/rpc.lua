--- A JSON-RPC 2.0 client over stdio against `hts serve`.
---
--- It speaks the same protocol `docs/engine-protocol.md` documents, with
--- LSP-style framing. It is deliberately dumb: it knows nothing about HTTP
--- requests nor about workspaces, only about messages.
---
--- The process is started lazily on the first call and then reused, because
--- raising `hts serve` per request would lose the workspace cache and the
--- engine's connection pool.

local M = {}

--- @class HtsClient
--- @field cmd string[] The command that starts the server.
--- @field proc table|nil The running process.
--- @field pending table<integer, fun(err: table|nil, result: table|nil)>
--- @field handlers table<string, fun(params: table)>
--- @field next_id integer
--- @field buffer string Bytes received and not yet unframed.
local Client = {}
Client.__index = Client

--- Creates a client without starting it yet.
--- @param cmd string[] The full command, e.g. `{ "hts", "serve" }`.
--- @return HtsClient
function M.new(cmd)
  return setmetatable({
    cmd = cmd,
    proc = nil,
    pending = {},
    handlers = {},
    next_id = 1,
    buffer = "",
  }, Client)
end

--- Registers the handler for one of the server's notifications.
--- @param method string
--- @param handler fun(params: table)
function Client:on(method, handler)
  self.handlers[method] = handler
end

--- Starts the process when it is not already running.
--- @return boolean ok, string|nil err
function Client:start()
  if self.proc then
    return true
  end

  local ok, proc = pcall(vim.system, self.cmd, {
    stdin = true,
    stdout = function(err, data)
      if not err and data then
        self:feed(data)
      end
    end,
    -- The server reserves stdout for the protocol, so anything on stderr is a
    -- real failure and deserves to reach the user.
    stderr = function(err, data)
      if not err and data and data ~= "" then
        vim.schedule(function()
          vim.notify("http-studio: " .. data, vim.log.levels.ERROR)
        end)
      end
    end,
  }, function()
    -- The process died: everything is cleared so the next call relaunches it.
    self.proc = nil
    self.buffer = ""
    for _, callback in pairs(self.pending) do
      callback({ message = "the server closed" }, nil)
    end
    self.pending = {}
  end)

  if not ok then
    return false, tostring(proc)
  end

  self.proc = proc
  return true
end

--- Stops the server, when it was running.
function Client:stop()
  if self.proc then
    self.proc:kill(15)
    self.proc = nil
    self.buffer = ""
    self.pending = {}
  end
end

--- Says whether the server is running.
--- @return boolean
function Client:is_running()
  return self.proc ~= nil
end

--- Sends a request and calls `callback` with the response.
--- @param method string
--- @param params table|nil
--- @param callback fun(err: table|nil, result: table|nil)|nil
function Client:request(method, params, callback)
  local ok, err = self:start()
  if not ok then
    if callback then
      callback({ message = "could not start `hts serve`: " .. tostring(err) }, nil)
    end
    return
  end

  local id = self.next_id
  self.next_id = id + 1
  if callback then
    self.pending[id] = callback
  end

  self:write({
    jsonrpc = "2.0",
    id = id,
    method = method,
    -- `vim.empty_dict()` keeps an empty object from serializing as `[]`,
    -- which the server would reject for not being a parameters object.
    params = params or vim.empty_dict(),
  })
end

--- Serializes and writes a framed message.
--- @param message table
function Client:write(message)
  local body = vim.json.encode(message)
  self.proc:write(string.format("Content-Length: %d\r\n\r\n%s", #body, body))
end

--- Accumulates bytes and extracts every complete message there is.
---
--- A stdout `chunk` does not match a message: it can carry half of one, or two
--- and a half. Hence the accumulation and consumption by announced length.
--- @param chunk string
function Client:feed(chunk)
  self.buffer = self.buffer .. chunk

  while true do
    local header_end = self.buffer:find("\r\n\r\n", 1, true)
    if not header_end then
      return
    end

    local header = self.buffer:sub(1, header_end - 1)
    local length = tonumber(header:match("[Cc]ontent%-[Ll]ength:%s*(%d+)"))
    if not length then
      -- An unreadable header: there is no way to know where the message ends,
      -- so what was read is discarded rather than silently desynchronising.
      self.buffer = ""
      vim.schedule(function()
        vim.notify("http-studio: response with no Content-Length", vim.log.levels.ERROR)
      end)
      return
    end

    local body_start = header_end + 4
    if #self.buffer < body_start + length - 1 then
      return
    end

    local body = self.buffer:sub(body_start, body_start + length - 1)
    self.buffer = self.buffer:sub(body_start + length)
    self:dispatch(body)
  end
end

--- Routes an already unframed message.
--- @param body string
function Client:dispatch(body)
  local ok, message = pcall(vim.json.decode, body)
  if not ok then
    return
  end

  if message.id ~= nil then
    local callback = self.pending[message.id]
    self.pending[message.id] = nil
    if callback then
      -- The callbacks touch buffers, so they leave `vim.system`'s fast
      -- context before running.
      vim.schedule(function()
        callback(message.error, message.result)
      end)
    end
    return
  end

  local handler = self.handlers[message.method]
  if handler then
    vim.schedule(function()
      handler(message.params or {})
    end)
  end
end

return M
