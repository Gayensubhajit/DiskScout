#!/usr/bin/env python3
import subprocess
import json
import shutil

def check_btrfs():
    info = {"is_btrfs": False, "usage": {}, "snapshots": [], "timers": []}
    
    res = subprocess.run("findmnt -n -o FSTYPE /", shell=True, capture_output=True, text=True)
    fstype = res.stdout.strip()
    if "btrfs" in fstype:
        info["is_btrfs"] = True
        
    if info["is_btrfs"]:
        if shutil.which("btrfs"):
            u_res = subprocess.run("btrfs filesystem usage / 2>/dev/null", shell=True, capture_output=True, text=True)
            info["usage"]["raw"] = u_res.stdout
            
        if shutil.which("snapper"):
            s_res = subprocess.run("snapper list 2>/dev/null", shell=True, capture_output=True, text=True)
            info["snapshots"] = [line for line in s_res.stdout.splitlines() if line.strip()]
            
        t_res = subprocess.run("systemctl list-timers --all 2>/dev/null | grep -Ei 'snapper|dusky|snapshot'", shell=True, capture_output=True, text=True)
        info["timers"] = [line.strip() for line in t_res.stdout.splitlines() if line.strip()]

    print(json.dumps(info, indent=2))

if __name__ == "__main__":
    check_btrfs()
