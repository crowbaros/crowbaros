#!/usr/bin/env python3
"""Interactive regression check for the disposable CrowbarOS test VM.

Keep its viewer open and pause manual interaction. Uses workspace 2, launches
and closes Terminal/Settings, temporarily changes saved appearance, and returns
to Dark with reduced motion. Does not install or build software.
"""
import json
import time
import check_shell as c


def wait_for(predicate, label, seconds=8):
    deadline=time.monotonic()+seconds
    while time.monotonic()<deadline:
        result=predicate()
        if result:return result
        time.sleep(.2)
    raise AssertionError(label)


def layers():
    return [s for level in json.loads(c.hypr("layers","-j"))["Virtual-1"]["levels"].values() for s in level]


def clients():return json.loads(c.hypr("clients","-j"))
def app(name):return next((w for w in clients() if w["class"]==name),None)
def launcher():return any(s["namespace"]=="crowbaros-launcher" for s in layers())
def pid():return c.desktop("systemctl","--user","show","crowbaros-shell","-p","MainPID","--value").strip()
def prefs():return json.loads(c.desktop("crowbaros-studio-settings","--print-settings"))


def paste(text):
    c.lab.ssh("test",c.DESKTOP+["env","WAYLAND_DISPLAY=wayland-1","sh","-c","exec wl-copy --type text/plain >/dev/null 2>&1"],input=text,capture=True)
    c.key(29,30);c.key(29,47);time.sleep(.3)


def toggle():
    c.desktop("crowbaros-shell","--launcher");time.sleep(.5)


def write_prefs(value):
    # A controlled saved-preference fixture; Studio's Save path is tested below.
    code=r"""import json,os,sys
from pathlib import Path
p=Path.home()/'.config/crowbaros/appearance.json'
t=p.with_suffix('.shell-validation')
t.write_text(json.dumps(json.loads(sys.stdin.read()))+'\n');t.chmod(0o600);os.replace(t,p)
"""
    c.lab.ssh("test",c.DESKTOP+["python3","-c",code],input=json.dumps(value))
    time.sleep(1.3)


if __name__=="__main__":
    c.OUT.mkdir(parents=True,exist_ok=True)
    original=prefs()
    assert original=={"schema_version":1,"theme":"dark","studio":False,"reduced_motion":True}, "Start from the recorded Dark/reduced-motion lab baseline"
    fixture=".local/share/applications/crowbaros-shell-validation-error.desktop"
    results={}
    fixture_created=False
    assert not app("foot") and not app("com.crowbaros.StudioSettings"), "Close test Terminal/Settings windows before validation"
    try:
        assert not c.hypr("configerrors").strip()
        wait_for(lambda:len(layers())==3,"Shell layers are not ready",30)
        assert not any(w["class"]=="com.crowbaros.Shell" for w in clients())
        assert json.loads(c.hypr("monitors","-j"))[0]["reserved"]==[0,44,0,96]
        results["native_layers_and_reserved_space"]=True
        c.hypr("eval",'hl.dispatch(hl.dsp.focus({workspace="1"}))')
        c.click(640,744)
        terminal=wait_for(lambda:app("foot"),"Terminal did not open")
        scope=c.guest("sudo","cat",f'/proc/{terminal["pid"]}/cgroup')
        assert ".scope" in scope and "crowbaros-shell.service" not in scope,scope
        c.click(207,22)
        wait_for(lambda:json.loads(c.hypr("activeworkspace","-j"))["id"]==2,"Workspace button failed")
        c.capture("desktop")
        c.key(56,57);wait_for(launcher,"Alt+Space did not open launcher")
        c.capture("launcher")
        paste("studio");c.key(28)
        settings=wait_for(lambda:app("com.crowbaros.StudioSettings"),"Settings did not launch")
        assert settings["floating"] and settings["size"]==[980,640] and not settings["xwayland"]
        wait_for(lambda:not launcher(),"Launcher did not close after launch")
        results["shortcut_search_launch_and_floating"]=True
        c.key(15);c.key(57);c.key(29,31) # Dark -> Light -> Save
        wait_for(lambda:prefs()["theme"]=="light","Settings Save failed")
        time.sleep(1.3);c.capture("light")
        c.key(42,15);c.key(57);c.key(29,31)
        wait_for(lambda:prefs()["theme"]=="dark","Restore Dark failed")
        c.key(29,16);wait_for(lambda:not app("com.crowbaros.StudioSettings"),"Saved Settings did not close")
        toggle();paste("no-such-application");c.capture("empty")
        paste("long-query-"*35);c.capture("long-query")
        paste("foot");c.capture("short-query")
        paste("Éditeur 🐦");c.capture("unicode")
        c.key(1);wait_for(lambda:not launcher(),"Escape did not release launcher")
        results["text_input_and_dismissal"]=True
        code=r"""from pathlib import Path
p=Path.home()/'.local/share/applications/crowbaros-shell-validation-error.desktop'
assert not p.exists()
p.parent.mkdir(parents=True,exist_ok=True)
p.write_text('[Desktop Entry]\nType=Application\nName=Validation Failure\nExec=/crowbaros-validation-missing-executable\n')
"""
        c.desktop("python3","-c",code)
        fixture_created=True
        toggle();paste("validation failure");c.key(28);time.sleep(1.2)
        assert launcher();c.capture("launch-error");c.key(1)
        c.desktop("python3","-c",f"from pathlib import Path;(Path.home()/{fixture!r}).unlink()")
        results["launch_error_recovery"]=True
        for name,theme,studio in [("paper","warm-paper",False),("studio","dark",True)]:
            write_prefs({**original,"theme":theme,"studio":studio});toggle();c.capture(name);c.key(1)
        write_prefs(original)
        toggle()
        c.hypr("eval",'hl.monitor({output="Virtual-1",mode="1024x768@60",position="auto",scale=1})')
        time.sleep(.8)
        bar=next(s for s in layers() if s["namespace"]=="crowbaros-bar")
        assert (bar["x"],bar["w"])==(0,1024),bar
        c.capture("compact-launcher");c.key(1);c.capture("compact-desktop")
        c.hypr("eval",'hl.monitor({output="Virtual-1",mode="1280x800@60",position="auto",scale=1})')
        time.sleep(.8)
        assert next(s for s in layers() if s["namespace"]=="crowbaros-bar")["w"]==1280
        results["live_resolution_change"]=True
        c.click(640,744);time.sleep(.5)
        assert json.loads(c.hypr("activewindow","-j"))["pid"]==terminal["pid"]
        before=pid();c.desktop("systemctl","--user","restart","crowbaros-shell")
        wait_for(lambda:len(layers())==3,"Shell did not restart",20)
        assert pid()!=before and app("foot")["pid"]==terminal["pid"]
        results["dock_focus_and_app_survives_shell_restart"]=True
        mainpid=pid();maps=c.guest("sudo","cat",f"/proc/{mainpid}/maps")
        assert "/opt/crowbaros-lab/mesa-intel-virtio/lib64/libvulkan_intel.so" in maps and "libvulkan_lvp" not in maps
        assert c.guest("getenforce").strip()=="Enforcing"
        assert c.guest("rpm","-V","crowbaros-shell")==""
        assert prefs()==original
        results["hardware_vulkan_rpm_integrity_selinux_and_preferences"]=True
        c.hypr("eval",f'hl.dispatch(hl.dsp.window.close({{window="address:{terminal["address"]}"}}))')
        c.hypr("eval",'hl.dispatch(hl.dsp.focus({workspace="2"}))')
        c.evidence()
        (c.OUT/"checks.json").write_text(json.dumps(results,indent=2)+"\n")
        print(json.dumps(results,indent=2))
    finally:
        write_prefs(original)
        if fixture_created:
            c.desktop("python3","-c",f"from pathlib import Path;(Path.home()/{fixture!r}).unlink(missing_ok=True)")
        c.hypr("eval",'hl.monitor({output="Virtual-1",mode="1280x800@60",position="auto",scale=1})')
