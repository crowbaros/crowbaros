-- Restore only saved CrowbarOS preferences in this user's session.
hl.on("hyprland.start", function ()
    hl.exec_cmd("/usr/bin/systemd-cat --identifier=crowbaros-settings-session /usr/bin/crowbaros-studio-settings --apply-session")
end)
