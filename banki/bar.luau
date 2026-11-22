--!nonstrict
--!nolint FunctionUnused

local ICON_SOURCE = "assets/anki.svg"
local ENTRY = "dontblameme/banki:banki"
local ICON_FILL = "#80C2EE"
local FIXED = {
  number = ICON_FILL,
  success = "#4CAF50",
}

local data
local pollSeconds = 600
local iconPath = ICON_SOURCE
local lastIconColor

local function shellQuote(value)
  return "'" .. value:gsub("'", "'\\''") .. "'"
end

local function colors()
  if noctalia.getConfig("follow_accent") == false then
    return FIXED
  end
  return {
    number = "primary",
    success = FIXED.success,
  }
end


local function tintIcon(color)
  if color == lastIconColor then
    return iconPath
  end
  if color == ICON_FILL then
    iconPath = ICON_SOURCE
    lastIconColor = color
    return iconPath
  end

  local dataDir = noctalia.pluginDataDir()
  local source = noctalia.readFile(ICON_SOURCE)
  if not dataDir or not source then
    iconPath = ICON_SOURCE
    lastIconColor = nil
    return iconPath
  end

  local path = dataDir .. "/anki-" .. color:gsub("#", "") .. ".svg"
  if noctalia.writeFile(path, source:gsub(ICON_FILL, color)) then
    iconPath = path
    lastIconColor = color
  else
    iconPath = ICON_SOURCE
    lastIconColor = nil
  end
  return iconPath
end

local function prepareSyncKey()
  if noctalia.getConfig("sync_enabled") ~= true then
    return ""
  end
  local keyFile = noctalia.getConfig("sync_key_file")
  if type(keyFile) == "string" and keyFile ~= "" then
    return noctalia.expandPath(keyFile)
  end

  local dir = noctalia.pluginDataDir()
  if not dir then
    return ""
  end
  local key = noctalia.getConfig("sync_key")
  if type(key) ~= "string" then
    key = ""
  end
  local path = dir .. "/sync-key"
  noctalia.writeFile(path, key)
  return path
end

local function content(children)
  local container = barWidget.isVertical() and ui.column or ui.row
  return container({ gap = 6, align = "center", justify = "center" }, children)
end

local function render()
  local palette = colors()
  local accent = noctalia.getConfig("follow_accent") ~= false
  local icon = tintIcon(accent and noctalia.getColor("primary") or ICON_FILL)

  if not data then
    barWidget.setVisible(true)
    barWidget.render(content({
      ui.image({ path = icon, width = 16, height = 16, fit = "contain" }),
      ui.label({ text = "..." }),
    }))
    barWidget.setTooltip(noctalia.tr("tooltip.waiting"))
    return
  end

  local complete = data.remaining == 0
  if complete and noctalia.getConfig("hide_on_complete") == true then
    barWidget.setVisible(false)
    barWidget.clearTooltip()
    return
  end

  barWidget.setVisible(true)
  if complete then
    barWidget.render(content({
      ui.glyph({ name = "check", size = 14, color = palette.success }),
    }))
  else
    local children = {}
    if noctalia.getConfig("hide_icon") ~= true then
      table.insert(children, ui.image({ path = icon, width = 16, height = 16, fit = "contain" }))
    end
    if noctalia.getConfig("hide_text") ~= true then
      table.insert(children, ui.label({ text = tostring(data.remaining), color = palette.number }))
    end
    barWidget.render(content(children))
  end

  local rows = {
    { key = noctalia.tr("tooltip.reviews"), value = tostring(data.reviews) },
    { key = noctalia.tr("tooltip.learning"), value = tostring(data.learning) },
  }
  if noctalia.getConfig("include_new") == true then
    table.insert(rows, { key = noctalia.tr("tooltip.new"), value = tostring(data.new) })
  end
  table.insert(rows, { key = noctalia.tr("tooltip.completed"), value = tostring(data.completed) })
  table.insert(rows, { key = noctalia.tr("tooltip.total"), value = tostring(data.total) })
  barWidget.setTooltip(rows)
end

local function onLine(line)
  local decoded = noctalia.json.decode(line)
  if not decoded or decoded.ok ~= true then
    return
  end

  data = decoded
  render()
end

local function start()
  local pluginDir = noctalia.pluginDir()
  local configured = noctalia.getConfig("collection_path")
  local collection = type(configured) == "string" and configured ~= ""
      and noctalia.expandPath(configured) or ""
  pollSeconds = tonumber(noctalia.getConfig("refresh_seconds")) or 600
  local includeNew = noctalia.getConfig("include_new") == true and "1" or "0"
  local keyPath = prepareSyncKey()
  local lockPath = noctalia.pluginDataDir() or ""
  if lockPath ~= "" then
    lockPath = lockPath .. "/sync.lock"
  end

  if not pluginDir then
    render()
    return
  end

  local command = table.concat({
    shellQuote("python3"),
    shellQuote(pluginDir .. "/due.py"),
    shellQuote(collection),
    tostring(pollSeconds),
    includeNew,
    shellQuote(keyPath),
    shellQuote(lockPath),
  }, " ")

  noctalia.runStream(command, onLine)
  render()
end

local function refreshNow()
  local pluginDir = noctalia.pluginDir()
  if not pluginDir then
    return
  end
  local configured = noctalia.getConfig("collection_path")
  local collection = type(configured) == "string" and configured ~= ""
      and noctalia.expandPath(configured) or ""
  local includeNew = noctalia.getConfig("include_new") == true and "1" or "0"
  local keyPath = prepareSyncKey()
  local lockPath = noctalia.pluginDataDir() or ""
  if lockPath ~= "" then
    lockPath = lockPath .. "/sync.lock"
  end
  noctalia.runAsync({
    "python3",
    pluginDir .. "/due.py",
    collection,
    "once",
    includeNew,
    keyPath,
    lockPath,
  }, function(result)
    if result.exitCode == 0 then
      local stdout = noctalia.string.trim(result.stdout)
      if stdout ~= "" then
        onLine(stdout)
      end
    end
  end, 60000)
end

function onRightClick()
  noctalia.runAsync({ "noctalia", "msg", "plugin", ENTRY, "all", "refresh" })
end

function onIpc(event)
  if event == "refresh" then
    refreshNow()
  end
end

function onConfigChanged()
  prepareSyncKey()
  render()
end

start()





