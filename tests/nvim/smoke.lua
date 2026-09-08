-- Integration test: a real Neovim speaking JSON-RPC with `hts serve`.
--
-- It covers the whole path — opening a `.http`, placing the cursor, resolving
-- the identifier against the workspace and drawing the response — which is
-- exactly what the pure tests in `block_spec.lua` cannot touch.
--
--     nvim --headless -l tests/nvim/smoke.lua <path-to-hts>

local binary = _G.arg and _G.arg[1] or "target/debug/hts"
local workspace = "examples/demo"

vim.opt.runtimepath:prepend(".")
-- `nvim -l` starts with no filetype detection; it is switched on by hand so
-- the test also covers the `ft=http` the mappings hang off.
vim.cmd("filetype plugin on")

local hts = require("http-studio")
local ui = require("http-studio.ui")

local failures = 0
local total = 0

--- Checks a condition and reports the result.
local function check(name, ok, detail)
  total = total + 1
  if ok then
    io.write("  ok    ", name, "\n")
  else
    failures = failures + 1
    io.write("  FAIL  ", name, "\n")
    if detail then
      io.write("          ", tostring(detail), "\n")
    end
  end
end

--- Waits until `condition` holds, or until the time runs out.
--- @param timeout integer Milliseconds.
--- @param condition fun(): boolean
--- @return boolean
local function wait(timeout, condition)
  return vim.wait(timeout, condition, 50)
end

--- The whole text of the response buffer.
local function response()
  return table.concat(ui.lines(), "\n")
end

hts.setup({
  cmd = binary,
  workspace = workspace,
  environment = "dev",
  keymaps = false,
})

io.write("startup\n")
check("the client is created in setup", hts._client() ~= nil)
check("the commands end up defined", vim.fn.exists(":HtsRun") == 2)
check("HtsPreview ends up defined", vim.fn.exists(":HtsPreview") == 2)
check("HtsEnv ends up defined", vim.fn.exists(":HtsEnv") == 2)

io.write("preview of the request under the cursor\n")

vim.cmd.edit(workspace .. "/collections/echo.http")
check("the filetype is http", vim.bo.filetype == "http", vim.bo.filetype)

-- Line 9 is `GET {{base_url}}/get`, inside the `get` block.
vim.api.nvim_win_set_cursor(0, { 9, 0 })

local located = require("http-studio.block").under_cursor()
check("locates the block under the cursor", located ~= nil and located.name == "get",
  located and located.name)

hts.preview()
check("the preview answers", wait(15000, function()
  return response():find("postman%-echo%.com") ~= nil
end), response())

local text = response()
check("resolves the environment variables", text:find("https://postman%-echo%.com/get") ~= nil, text)
check("sends nothing", text:find("nothing has been sent") ~= nil)
check("explains each variable's origin", text:find("environment:dev") ~= nil, text)
check("shows the collection variable", text:find("collection") ~= nil)

io.write("a real execution\n")

hts.run()

-- It waits for the metrics line and not for the status: the body arrives in
-- events later than the headers, and waiting for the first would read half of
-- it.
check("the whole response arrives", wait(20000, function()
  return response():find("%d+ ms · %d+ bytes") ~= nil
end), response())

text = response()
check("draws the status", text:find("200 HTTP") ~= nil, text)
check("draws the response body", text:find('"page"') ~= nil, text)

io.write("errors\n")

-- A buffer with no request under the cursor must break nothing.
vim.cmd.enew()
vim.bo.filetype = "http"
vim.api.nvim_buf_set_lines(0, 0, -1, false, { "@only = variables" })
vim.api.nvim_win_set_cursor(0, { 1, 0 })
local ok = pcall(hts.run)
check("a buffer with no request raises no exception", ok)

hts.stop()
check("the server stops", not hts._client():is_running())

io.write(string.format("\n%d checks, %d failures\n", total, failures))
os.exit(failures == 0 and 0 or 1)
