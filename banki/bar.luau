--!nonstrict
--!nolint FunctionUnused

local bridge = require("./lib/bridge.luau")
bridge.setup("banki", { "sync_enabled", "sync_key", "sync_key_file", "collection_path", "refresh_seconds", "include_new", "hide_icon", "hide_text", "hide_on_complete", "follow_accent" })
local function refreshInterval()
  local seconds = tonumber(noctalia.getConfig("refresh_seconds")) or 600
  noctalia.setUpdateInterval(math.max(seconds, 5) * 1000)
end
refreshInterval()

function onClick() bridge.call("click") end
function onRightClick()
  noctalia.runAsync({ "noctalia", "msg", "plugin", "dontblameme/banki:banki", "all", "refresh" })
end
function onMiddleClick() bridge.call("middle_click") end
function onDoubleClick() bridge.call("double_click") end
function onScroll(axis, steps, startsGesture) bridge.call("scroll", { axis = axis, steps = steps, starts_gesture = startsGesture }) end
function onHover(hovered) bridge.call("hover", { hovered = hovered }) end
function onKey(chord, pressed, repeated) bridge.call("key", { chord = chord, pressed = pressed, repeated = repeated }) end
function onIpc(event, payload) bridge.call("ipc", { event = event, payload = payload }) end
function onConfigChanged()
  refreshInterval()
  bridge.call("config_changed")
end
function onOutputsChanged() bridge.call("outputs_changed") end
function onEnable() bridge.call("enable") end
function onDisable() bridge.call("disable") end
function onExit(signal, reason) bridge.call("exit", { signal = signal, reason = reason }) end
function onAudioSpectrum(valuesCsv, stateCsv) bridge.call("audio_spectrum", { values_csv = valuesCsv, state_csv = stateCsv }) end
function onSuspend() bridge.call("suspend") end
function onResume() bridge.call("resume") end
function onRefresh() bridge.call("refresh") end
function onOpen(context) bridge.call("open", { context = context }) end
function onClose() bridge.call("close") end
function onCloseClicked() bridge.call("close_clicked") end
function onRefreshClicked() bridge.call("refresh_clicked") end
function onSettingsClicked() bridge.call("settings_clicked") end
function onOpenSettings() bridge.call("open_settings") end
function onClosePanel() bridge.call("close_panel") end
function onFrameTick(delta) bridge.call("frame", { delta = delta }) end
function onQuery(text) bridge.query(text) end
function onActivate(id) bridge.call("activate", { id = id }) end
function onSearch(text) bridge.call("search", { text = text }) end
function onSearchChange(text) bridge.call("search_change", { text = text }) end
function update() bridge.call("render", { source = "update" }) end
bridge.watch_state(function() bridge.call("render", { source = "state" }) end)
local waiting = barWidget.isVertical() and ui.column or ui.row
barWidget.setVisible(true)
barWidget.render(waiting({ gap = 6, align = "center", justify = "center" }, {
  ui.image({ path = "assets/anki.svg", width = 16, height = 16, fit = "contain" }),
  ui.label({ text = "..." }),
}))
barWidget.setTooltip(noctalia.tr("tooltip.waiting"))
bridge.call("render", { source = "load" })
