-- CrowbarOS shell integration for the Hyprland 0.57 Lua API.
hl.bind("ALT + Space", hl.dsp.exec_cmd("/usr/bin/crowbaros-shell --launcher"))
hl.on("hyprland.start", function () hl.exec_cmd("/usr/bin/crowbaros-shell --start-session") end)
-- Familiar floating windows; tiling remains available through Alt+V.
hl.window_rule({match={class=".*"}, float=true})
hl.window_rule({match={class="com.crowbaros.StudioSettings"}, size={980,640}, center=true})
hl.window_rule({match={class="foot"}, size={800,500}, center=true})
