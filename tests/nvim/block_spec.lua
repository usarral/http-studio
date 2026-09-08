-- Pure tests of `block.lua`: locating the block under the cursor, and naming.
--
-- They need neither the engine nor the network, so they run in milliseconds,
-- and they are what guards the only logic the plugin duplicates from the Rust
-- parser.
--
--     nvim --headless -l tests/nvim/block_spec.lua

package.path = "lua/?.lua;lua/?/init.lua;" .. package.path
local block = require("http-studio.block")

local failures = 0
local total = 0

--- Checks an equality and reports the result.
local function check(name, actual, expected)
  total = total + 1
  if vim.deep_equal(actual, expected) then
    io.write("  ok    ", name, "\n")
  else
    failures = failures + 1
    io.write(
      "  FAIL  ",
      name,
      "\n          expected: ",
      vim.inspect(expected),
      "\n          got:      ",
      vim.inspect(actual),
      "\n"
    )
  end
end

--- The name of the block containing line `cursor`.
local function name_at(lines, cursor)
  local found = block.locate(lines, cursor)
  return found and found.name or nil
end

io.write("slug\n")
check("normalises spaces", block.slug("Iniciar sesion"), "iniciar-sesion")
check("strips accents", block.slug("Iniciar sesión"), "iniciar-sesion")
check("collapses punctuation", block.slug("Añadir  ítem!"), "anadir-item")
check("leaves an existing slug alone", block.slug("post-json"), "post-json")
check("falls back to a default name", block.slug("¿?"), "request")
check("lowercases", block.slug("Login"), "login")

io.write("locate\n")

local simple = {
  "@base = https://a.test",
  "",
  "### Iniciar sesión",
  "# @name login",
  "POST {{base}}/login",
  "",
  "### Refresh",
  "POST {{base}}/refresh",
}

check("uses the @name directive", name_at(simple, 5), "login")
check("finds the block from its first line", name_at(simple, 3), "login")
check("uses the title when there is no @name", name_at(simple, 8), "Refresh")
check("the cursor on the separator already counts", name_at(simple, 7), "Refresh")

local untitled = {
  "GET https://a.test/one",
  "",
  "###",
  "GET https://a.test/two",
}

check("falls back to the block position", name_at(untitled, 1), "request-1")
check("counts the blocks in order", name_at(untitled, 4), "request-2")

local only_variables = {
  "@base = https://a.test",
  "@token = abc",
}

check("a preamble with no request is not a block", block.locate(only_variables, 1), nil)
check("an empty file has no block", block.locate({}, 1), nil)

check("a cursor out of range is clamped", name_at(simple, 999), "Refresh")
-- Clamping to line 1 leaves the cursor in the variable preamble, which is not
-- a request: returning nil there is the right answer.
check("a cursor at zero lands in the preamble", name_at(simple, 0), nil)
check("a cursor in the preamble does not resolve", name_at(simple, 1), nil)

io.write("resolve\n")

local collections = {
  { name = "auth", requests = { { id = "auth/login" }, { id = "auth/refresh" } } },
  { name = "admin/users", requests = { { id = "admin/users/login" } } },
}

check(
  "resolves a unique name",
  select(1, block.resolve(collections, "refresh", "/w/collections/auth.http")),
  "auth/refresh"
)

check(
  "breaks the tie by the file path",
  select(1, block.resolve(collections, "login", "/w/collections/admin/users.http")),
  "admin/users/login"
)

check(
  "breaks the tie for the other file",
  select(1, block.resolve(collections, "login", "/w/collections/auth.http")),
  "auth/login"
)

check(
  "a name that does not exist does not resolve",
  select(1, block.resolve(collections, "does-not-exist", "/w/collections/auth.http")),
  nil
)

io.write(string.format("\n%d checks, %d failures\n", total, failures))
os.exit(failures == 0 and 0 or 1)
